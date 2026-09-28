//! Static SPA hosting (`GET /srv/…`). Ports the donor `static_site` handler:
//! sub-app slug routing, `index.html` fallbacks, `<base>` injection so
//! relative asset URLs survive deep-link refreshes, and long caching for
//! hashed assets. No `{board}` — one worker serves one app.
//!
//! Cost control (every hit here is worker CPU + storage I/O):
//! - Validators: blob hash → ETag, `304` on `If-None-Match` (no bytes).
//! - Edge cache (`Cache::default`): public tenants only — shared entries
//!   must never mix principals. Invalidated on asset put/delete.

use worker::{Request, Response, Result, RouteContext};

use crate::{auth, cors};

pub async fn srv_root(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    // A configured site route for "/" wins (manifest-driven entry point);
    // otherwise the historical default: the hosted SPA.
    if let Ok(app) = auth::ctx_for(&req, &ctx).await {
        if let Ok(Some(row)) = engine::site::route_match(&app.engine, "GET", "/").await {
            return dispatch_site(req, ctx, row).await;
        }
    }
    // Trailing-slash canonical form so relative "./…" URLs in index.html
    // resolve against /srv/ instead of /.
    Ok(cors::redirect_to("/srv/"))
}

/// Root entry (replaces the hardcoded redirect): site route or SPA default.
pub async fn root(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    srv_root(req, ctx).await
}

const MAX_PROXY_BODY: usize = 5 * 1024 * 1024;

fn apply_headers(h: &worker::Headers, map: &serde_json::Value) {
    if let Some(obj) = map.as_object() {
        for (k, v) in obj {
            if let Some(s) = v.as_str() {
                let _ = h.set(k, s);
            }
        }
    }
}

/// Catchall site dispatcher (registered before the 404 fallback): exact
/// paths, `/*` prefixes, every method. Unmatched → the historical 404.
pub async fn custom(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let method = req.method().to_string();
    let path = req.path();
    let app = match auth::ctx_for(&req, &ctx).await {
        Ok(c) => c,
        Err(r) => return Ok(r),
    };
    let row = match engine::site::route_match(&app.engine, &method, &path).await {
        Ok(r) => r,
        Err(e) => return Ok(cors::bad(&e)),
    };
    let Some(row) = row else {
        return Ok(cors::err(404, &format!("not found: {path}")));
    };
    dispatch_site(req, ctx, row).await
}

async fn dispatch_site(req: Request, ctx: RouteContext<()>, row: serde_json::Value) -> Result<Response> {
    let method = req.method().to_string();
    let spec = row.get("spec").cloned().unwrap_or(serde_json::Value::Null);
    let kind = spec.get("kind").and_then(|v| v.as_str()).unwrap_or("");
    let route_path = row.get("path").and_then(|v| v.as_str()).unwrap_or("");
    if kind != "proxy" {
        let want = spec.get("method").and_then(|v| v.as_str()).unwrap_or("GET");
        if want != "*" && method != want {
            return Ok(cors::err(405, &format!("route allows {want}")));
        }
    }
    // Site routes are public BY DEFAULT (SEO, exact paths): no allow_roles
    // means anonymous + edge-cacheable. Listed tiers switch to keyed mode:
    // caller must hold a tier, reads follow the caller (not anonymous),
    // and nothing is shared-cached. Scoped customer keys are not admitted.
    let mut app = match auth::ctx_for(&req, &ctx).await {
        Ok(c) => c,
        Err(r) => return Ok(r),
    };
    let allow = match engine::site::parse_allow_roles(&spec) {
        Ok(a) => a,
        Err(_) => return Ok(cors::deny("route misconfigured")),
    };
    let keyed = !allow.is_empty();
    if keyed && !engine::site::role_allowed(&app.principal.role, &allow) {
        return Ok(cors::deny("route requires a key"));
    }
    match kind {
        "redirect" => {
            let to = spec.get("to").and_then(|v| v.as_str()).unwrap_or("/srv/");
            let status = spec.get("status").and_then(|v| v.as_u64()).unwrap_or(302) as u16;
            let to = engine::site::render_target(to, engine::TENANT, "", "");
            Ok(cors::redirect_to_status(&to, status))
        }
        "text" => {
            let body = spec.get("body").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let ct = spec.get("content_type").and_then(|v| v.as_str()).unwrap_or("text/plain").to_string();
            let bytes = body.into_bytes();
            let etag = sha256_hex(&bytes);
            if cors::etag_matches(&req, &etag) {
                return Ok(cors::not_modified(&etag));
            }
            let smax = spec.get("s_maxage").and_then(|v| v.as_u64()).unwrap_or(3600);
            let cache = format!("public, max-age=3600, s-maxage={smax}");
            let mut res = cors::asset_bytes(
                bytes.clone(),
                &ct,
                &if keyed { "private, max-age=0".to_string() } else { cache.clone() },
                &etag,
            );
            apply_headers(res.headers_mut(), spec.get("headers").unwrap_or(&serde_json::Value::Null));
            if !keyed {
                if let Ok(url) = req.url().map(|u| u.to_string()) {
                    let cached = cors::asset_bytes(bytes, &ct, &cache, &etag);
                    let _ = worker::Cache::default().put(url.as_str(), cached).await;
                }
            }
            Ok(res)
        }
        "asset" => {
            let rel = spec.get("path").and_then(|v| v.as_str()).unwrap_or("");
            let Some((data, ct)) = app.engine.get_asset(rel).await.unwrap_or(None) else {
                return Ok(cors::gone("not found"));
            };
            let smax = spec.get("s_maxage").and_then(|v| v.as_u64()).unwrap_or_else(|| {
                if ct.starts_with("text/html") { 60 } else { 86400 }
            });
            let cache = if ct.starts_with("text/html") {
                format!("public, max-age=0, s-maxage={smax}")
            } else {
                format!("public, max-age=3600, s-maxage={smax}")
            };
            respond_bytes(&req, data, &ct, (!keyed).then(|| cache), &spec).await
        }
        "query" => {
            let table = spec.get("table").and_then(|v| v.as_str()).unwrap_or("").to_string();
            // Public routes serve the anonymous view (fail loudly, never
            // silently empty, so misconfigurations show). Keyed routes
            // serve the caller's view through the normal table gates.
            let open = if keyed {
                auth::can_table_read(&mut app, &table).await
            } else {
                let tenant_public = app.engine.tenant().await.map(|t| t.public_reads).unwrap_or(false);
                app.engine
                    .get_table(&table)
                    .await
                    .ok()
                    .flatten()
                    .map(|c| c.anon_read_open(tenant_public))
                    .unwrap_or(false)
            };
            if !open {
                return Ok(cors::deny(if keyed { "private app" } else { "route table not public" }));
            }
            let limit = spec.get("limit").and_then(|v| v.as_u64()).unwrap_or(100).clamp(1, 5000) as usize;
            let filter_json = spec.get("filter").cloned().unwrap_or(serde_json::Value::Null);
            let conds = if filter_json.is_null() {
                Vec::new()
            } else {
                match engine::storage::ir::parse_filter(&filter_json) {
                    Ok(f) => f.conds,
                    Err(e) => return Ok(cors::bad(&e)),
                }
            };
            // Paginate to completeness (engine clamps 500/call).
            let mut payloads = Vec::new();
            let mut offset = 0usize;
            loop {
                let chunk = limit.min(500).min(limit.saturating_sub(payloads.len()));
                if chunk == 0 {
                    break;
                }
                let sf = engine::storage::ir::SrvFilter { conds: conds.clone() };
                let batch = match app.engine.query_records(&table, &sf, &[], chunk, offset).await {
                    Ok(b) => b,
                    Err(e) => return Ok(cors::bad(&e)),
                };
                let n = batch.len();
                payloads.extend(batch.into_iter().map(|r| r.payload));
                if n < chunk || payloads.len() >= limit {
                    break;
                }
                offset += n;
            }
            let format = spec.get("format").and_then(|v| v.as_str()).unwrap_or("json");
            let (body, ct) = if format == "sitemap" {
                let origin = req.url().map(|u| u.origin().ascii_serialization()).unwrap_or_default();
                let base = origin.trim_end_matches('/').to_string();
                let prefix = spec.get("prefix").and_then(|v| v.as_str()).unwrap_or("/p/");
                let field = spec.get("url_field").and_then(|v| v.as_str()).unwrap_or("slug");
                let xml = engine::site::render_sitemap(&base, prefix, field, &payloads);
                (xml.into_bytes(), "application/xml".to_string())
            } else {
                let body = serde_json::json!({ "ok": true, "records": payloads });
                (serde_json::to_vec(&body).unwrap_or_default(), "application/json".to_string())
            };
            let smax = spec.get("s_maxage").and_then(|v| v.as_u64()).unwrap_or(60);
            let cache = format!("public, max-age=0, s-maxage={smax}");
            respond_bytes(&req, body, &ct, (!keyed).then(|| cache), &spec).await
        }
        "proxy" => proxy_route(req, &mut app, &spec, route_path, &method).await,
        _ => Ok(cors::err(500, "unsupported site route kind")),
    }
}

/// Request-time proxy (Phase D): declarative `op: proxy` route bindings.
/// Forwards method + allowlisted headers + body, returns upstream status +
/// bytes. Never forwards `authorization`/`cookie` (upstream auth comes only
/// from admin-configured `inject_headers`); SSRF-gated like `$call`.
async fn proxy_route(
    mut req: Request,
    _app: &mut crate::auth::Ctx,
    spec: &serde_json::Value,
    route_path: &str,
    method: &str,
) -> Result<Response> {
    let allowed: Vec<String> = spec
        .get("methods")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
        .unwrap_or_else(|| vec!["GET".to_string()]);
    if !allowed.iter().any(|m| m == method) {
        return Ok(cors::err(405, &format!("route allows {}", allowed.join(","))));
    }
    let rest = if let Some(pre) = route_path.strip_suffix("/*") {
        req.path().strip_prefix(pre).unwrap_or("").trim_start_matches('/').to_string()
    } else {
        String::new()
    };
    let query = req.url().ok().and_then(|u| u.query().map(str::to_string)).unwrap_or_default();
    let tmpl = spec.get("target").and_then(|v| v.as_str()).unwrap_or("");
    let mut target = engine::site::render_target(tmpl, engine::TENANT, &rest, &query);
    let forward = spec.get("forward_query").and_then(|v| v.as_bool()).unwrap_or(true);
    if forward && !query.is_empty() && !tmpl.contains("{{$.query}}") {
        target.push(if target.contains('?') { '&' } else { '?' });
        target.push_str(&query);
    }
    if !engine::webhooks::valid_url(&target) {
        return Ok(cors::err(502, "ssrf-blocked proxy target"));
    }
    let hs = worker::Headers::new();
    for name in req.headers().keys() {
        let n = name.to_ascii_lowercase();
        if matches!(
            n.as_str(),
            "host" | "connection"
                | "upgrade"
                | "content-length"
                | "transfer-encoding"
                | "accept-encoding"
                | "cookie"
                | "authorization"
        ) {
            continue;
        }
        if let Ok(Some(v)) = req.headers().get(&name) {
            let _ = hs.set(&name, &v);
        }
    }
    if let Some(obj) = spec.get("inject_headers").and_then(|v| v.as_object()) {
        for (k, v) in obj {
            if let Some(s) = v.as_str() {
                let rendered = engine::site::render_target(s, engine::TENANT, &rest, &query);
                let _ = hs.set(k, &rendered);
            }
        }
    }
    let body_bytes = if matches!(method, "POST" | "PUT" | "PATCH" | "DELETE") {
        match req.bytes().await {
            Ok(b) if b.len() <= MAX_PROXY_BODY => b,
            Ok(_) => return Ok(cors::err(413, "proxy body too large (5 MiB max)")),
            Err(_) => return Ok(cors::err(400, "unreadable proxy body")),
        }
    } else {
        Vec::new()
    };
    let js_body = if body_bytes.is_empty() {
        None
    } else {
        Some(worker::js_sys::Uint8Array::from(body_bytes.as_slice()).into())
    };
    let wmethod = match method {
        "POST" => worker::Method::Post,
        "PUT" => worker::Method::Put,
        "PATCH" => worker::Method::Patch,
        "DELETE" => worker::Method::Delete,
        "HEAD" => worker::Method::Head,
        "OPTIONS" => worker::Method::Options,
        _ => worker::Method::Get,
    };
    let mut init = worker::RequestInit::new();
    init.with_method(wmethod).with_headers(hs).with_body(js_body);
    let out_req = match worker::Request::new_with_init(&target, &init) {
        Ok(r) => r,
        Err(_) => return Ok(cors::err(502, "bad proxy target")),
    };
    let upstream = match worker::Fetch::Request(out_req).send().await {
        Ok(r) => r,
        Err(e) => return Ok(cors::err(502, &format!("upstream unreachable: {e}"))),
    };
    let status = upstream.status_code();
    let content_type = upstream
        .headers()
        .get("content-type")
        .ok()
        .flatten()
        .unwrap_or_else(|| "application/octet-stream".to_string());
    let encoding = upstream
        .headers()
        .get("content-encoding")
        .ok()
        .flatten()
        .unwrap_or_default();
    let mut upstream = upstream;
    let data = match upstream.bytes().await {
        Ok(b) if b.len() <= MAX_PROXY_BODY => b,
        Ok(_) => return Ok(cors::err(502, "upstream body too large (5 MiB max)")),
        Err(e) => return Ok(cors::err(502, &format!("upstream read failed: {e}"))),
    };
    let h = worker::Headers::new();
    let _ = h.set("access-control-allow-origin", "*");
    let _ = h.set("content-type", &content_type);
    if !encoding.is_empty() {
        let _ = h.set("content-encoding", &encoding);
    }
    worker::Response::from_bytes(data)
        .map(|r| r.with_headers(h).with_status(status))
        .or_else(|_| Ok(cors::err(502, "response encode failed")))
}

/// Shared responder: ETag over final bytes, 304s, edge put for public
/// routes only (`public_cache = None` → `private, max-age=0`, never
/// shared-cached). Validators are safe either way (no bytes on 304).
async fn respond_bytes(
    req: &Request,
    data: Vec<u8>,
    ct: &str,
    public_cache: Option<String>,
    spec: &serde_json::Value,
) -> Result<Response> {
    let etag = sha256_hex(&data);
    if cors::etag_matches(req, &etag) {
        return Ok(cors::not_modified(&etag));
    }
    let cache = public_cache.unwrap_or_else(|| "private, max-age=0".to_string());
    let mut res = cors::asset_bytes(data.clone(), ct, &cache, &etag);
    apply_headers(res.headers_mut(), spec.get("headers").unwrap_or(&serde_json::Value::Null));
    if cache.starts_with("public") {
        if let Ok(url) = req.url().map(|u| u.to_string()) {
            let cached = cors::asset_bytes(data, ct, &cache, &etag);
            let _ = worker::Cache::default().put(url.as_str(), cached).await;
        }
    }
    Ok(res)
}

/// REST: list site routes (reader) — the exact-path surface.
pub async fn site_routes(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let app = match auth::ctx_for(&req, &ctx).await {
        Ok(c) => c,
        Err(r) => return Ok(r),
    };
    if !auth::require_read(&app.principal) {
        return Ok(cors::deny("reader authorization required"));
    }
    Ok(match engine::site::route_list(&app.engine).await {
        Ok(routes) => cors::ok(serde_json::json!({ "ok": true, "routes": routes })),
        Err(e) => cors::bad(&e),
    })
}

/// REST: add a site route (admin). Body is the route JSON; `owner`
/// defaults to tenant.
pub async fn site_add(mut req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let mut app = match auth::ctx_for(&req, &ctx).await {
        Ok(c) => c,
        Err(r) => return Ok(r),
    };
    if !auth::require_admin(&app.principal) {
        return Ok(cors::deny("admin authorization required"));
    }
    let bytes = match req.bytes().await {
        Ok(b) if b.len() <= 1_000_000 => b,
        _ => return Ok(cors::err(400, "unreadable/too-large body")),
    };
    let route: serde_json::Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(_) => return Ok(cors::err(400, "invalid json")),
    };
    let owner = route.get("owner").and_then(|v| v.as_str()).unwrap_or("tenant").to_string();
    Ok(match engine::site::route_put(&mut app.engine, &owner, &route).await {
        Ok(()) => {
            if let (Some(path), Ok(url)) = (
                route.get("path").and_then(|v| v.as_str()),
                req.url().map(|u| u.origin().ascii_serialization()),
            ) {
                let full = format!("{}{}", base_trim(&url), path);
                let _ = worker::Cache::default().delete(full.as_str(), false).await;
            }
            cors::created(serde_json::json!({ "ok": true, "path": route.get("path") }))
        }
        Err(e) => cors::bad(&e),
    })
}

/// REST: remove a site route by `?path=` (admin).
pub async fn site_remove(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let mut app = match auth::ctx_for(&req, &ctx).await {
        Ok(c) => c,
        Err(r) => return Ok(r),
    };
    if !auth::require_admin(&app.principal) {
        return Ok(cors::deny("admin authorization required"));
    }
    let path = crate::query::params(&req).get("path").cloned().unwrap_or_default();
    Ok(match engine::site::route_remove(&mut app.engine, &path).await {
        Ok(true) => {
            if let Ok(url) = req.url().map(|u| u.origin().ascii_serialization()) {
                let full = format!("{}{}", base_trim(&url), path);
                let _ = worker::Cache::default().delete(full.as_str(), false).await;
            }
            cors::ok(serde_json::json!({ "ok": true, "removed": true }))
        }
        Ok(false) => cors::gone("no such route"),
        Err(e) => cors::bad(&e),
    })
}

fn base_trim(url: &str) -> String {
    url.trim_end_matches('/').to_string()
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(64);
    for b in Sha256::digest(bytes) {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

fn has_file_ext(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    let Some(dot) = name.rfind('.') else {
        return false;
    };
    dot > 0 && dot < name.len() - 1
}

fn inject_base_tag(html: &[u8], base_tag: &str) -> String {
    let src = String::from_utf8_lossy(html);
    if let Some(hi) = src.to_ascii_lowercase().find("<head") {
        if let Some(end) = src[hi..].find('>') {
            let pos = hi + end + 1;
            return format!("{}{}{}", &src[..pos], base_tag, &src[pos..]);
        }
    }
    // No <head>: prepend so the base still applies.
    format!("{base_tag}{src}")
}

pub async fn serve(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let rel = ctx.param("path").cloned().unwrap_or_default();
    let mut app = match auth::ctx_for(&req, &ctx).await {
        Ok(c) => c,
        Err(r) => return Ok(r),
    };
    let tenant = match app.engine.tenant().await {
        Ok(t) => t,
        Err(e) => return Ok(cors::srv(&e)),
    };
    if !tenant.public_reads && !auth::require_read(&app.principal) {
        return Ok(cors::deny("private app"));
    }

    let mut base = "/srv/".to_string();
    let mut target = rel.clone();
    let mut fallback = "index.html".to_string();

    // Sub-app slug routing: serve from the slug prefix with per-slug SPA
    // fallback so deep links inside the sub-app land on ITS index.html.
    let first_seg = rel.split('/').next().unwrap_or("");
    if !first_seg.is_empty() {
        if let Ok(Some(sub)) = app.engine.get_subapp(first_seg).await {
            base = format!("/srv/{}/", sub.slug);
            fallback = sub.index.clone();
            target = match rel.strip_prefix(&sub.slug) {
                Some(rest) if !rest.is_empty() => format!("{}{}", sub.slug, rest),
                _ => sub.index.clone(),
            };
        }
    }
    if target.is_empty() {
        target = fallback.clone();
    }

    // Candidate paths in historical order (pure — no I/O yet).
    let mut candidates = vec![target.clone()];
    if !target.ends_with(".html") {
        candidates.push(format!("{target}/index.html"));
    }
    // SPA fallback: extensionless unknown paths serve the index.
    if !has_file_ext(&target) {
        candidates.push(fallback.clone());
    }
    let public = tenant.public_reads;
    let url = req.url().map(|u| u.to_string()).unwrap_or_default();

    // Phase 2: edge cache first (public tenants only). Cache ops are
    // best-effort — a cache error must never fail the serve.
    if public && !url.is_empty() {
        if let Ok(hit) = worker::Cache::default().get(url.as_str(), false).await {
            if let Some(res) = hit {
                if let Ok(etag) = res.headers().get("etag") {
                    if let Some(e) = etag {
                        if cors::etag_matches(&req, e.trim_matches('"')) {
                            return Ok(cors::not_modified(e.trim_matches('"')));
                        }
                    }
                }
                return Ok(res);
            }
        }
    }

    // Read once; the ETag is always the hash of the FINAL served bytes
    // (post `<base>` injection), so validators are exact on every adapter
    // and every transform — verifiable with any local sha256sum.
    let mut fetched: Option<(Vec<u8>, String)> = None;
    for cand in &candidates {
        if let Ok(Some((data, ct))) = app.engine.get_asset(cand).await {
            fetched = Some((data, ct));
            break;
        }
    }
    let Some((data, content_type)) = fetched else {
        return Ok(cors::gone("not found"));
    };
    let is_html = content_type.starts_with("text/html");
    // NOTE: `no-cache` vetoes edge storage, so HTML uses `max-age=0` —
    // browsers still revalidate every load (deploy-fresh), while the shared
    // cache may serve `s-maxage` seconds (purged on put regardless).
    let (body, cache) = if is_html {
        let tag = format!("<base href=\"{base}\">");
        (inject_base_tag(&data, &tag).into_bytes(), "public, max-age=0")
    } else {
        (data, "public, max-age=3600")
    };
    let etag = sha256_hex(&body);
    if cors::etag_matches(&req, &etag) {
        return Ok(cors::not_modified(&etag));
    }
    // Shared-cache lifetime rides alongside (never instead of) the browser
    // directive; HTML revalidates fast so deploys surface, hashed assets
    // linger (exact-URL invalidation on put keeps them correct).
    // Tenant knob wins (tenants can't set worker env), then env var, then default.
    let s_maxage = |tenant_val: Option<u64>, default: u64, var: &str| {
        tenant_val
            .or_else(|| {
                ctx.env
                    .var(var)
                    .ok()
                    .and_then(|v| v.to_string().parse::<u64>().ok())
            })
            .unwrap_or(default)
    };
    if !public {
        return Ok(cors::asset_bytes(body, &content_type, cache, &etag));
    }
    let smax = if is_html {
        s_maxage(tenant.assets_s_maxage_html, 60, "ASSETS_S_MAXAGE_HTML")
    } else {
        s_maxage(tenant.assets_s_maxage, 86400, "ASSETS_S_MAXAGE")
    };
    let cache = format!("{cache}, s-maxage={smax}");
    let cached = cors::asset_bytes(body.clone(), &content_type, &cache, &etag);
    let _ = worker::Cache::default().put(url.as_str(), cached).await;
    Ok(cors::asset_bytes(body, &content_type, &cache, &etag))
}
