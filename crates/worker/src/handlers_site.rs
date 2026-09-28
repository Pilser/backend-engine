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
    let _ = (&req, &ctx);
    // Trailing-slash canonical form so relative "./…" URLs in index.html
    // resolve against /srv/ instead of /.
    Ok(cors::redirect_to("/srv/"))
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
    let s_maxage = |default: u64, var: &str| {
        ctx.env
            .var(var)
            .ok()
            .and_then(|v| v.to_string().parse::<u64>().ok())
            .unwrap_or(default)
    };
    if !public {
        return Ok(cors::asset_bytes(body, &content_type, cache, &etag));
    }
    let smax = if is_html {
        s_maxage(60, "ASSETS_S_MAXAGE_HTML")
    } else {
        s_maxage(86400, "ASSETS_S_MAXAGE")
    };
    let cache = format!("{cache}, s-maxage={smax}");
    let cached = cors::asset_bytes(body.clone(), &content_type, &cache, &etag);
    let _ = worker::Cache::default().put(url.as_str(), cached).await;
    Ok(cors::asset_bytes(body, &content_type, &cache, &etag))
}
