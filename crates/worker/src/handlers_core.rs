//! Core routes: app info/config, resources, auth, upload/file, outbound call,
//! inbound events, assets, system health, MCP. Ports the donor `rest.rs`
//! handlers minus `{board}` (single tenant). Raw-binary upload only for now
//! (multipart form parsing deferred — send bytes with `X-Filename`).

use serde_json::{json, Value as Json};
use worker::{Request, Response, Result, RouteContext};

use crate::{auth, cors, query};

use super::handlers_tables::body_json_for;

const MAX_JSON: usize = 1_000_000;
const MAX_UPLOAD: usize = 25_000_000;

// ---- app ---------------------------------------------------------------

pub async fn app_info(req: Request, ctx: RouteContext<()>) -> Result<Response> {
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
    Ok(cors::ok(json!({ "ok": true, "app": tenant })))
}

pub async fn app_patch(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let mut app = match auth::ctx_for(&req, &ctx).await {
        Ok(c) => c,
        Err(r) => return Ok(r),
    };
    if !auth::require_admin(&app.principal) {
        return Ok(cors::deny("admin authorization required"));
    }
    let body = match body_json_for(req, MAX_JSON).await {
        Ok(b) => b,
        Err(r) => return Ok(r),
    };
    Ok(match app.engine.update_tenant(&body).await {
        Ok(()) => cors::ok(json!({ "ok": true })),
        Err(e) => cors::bad(&e),
    })
}

pub async fn resources(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let mut app = match auth::ctx_for(&req, &ctx).await {
        Ok(c) => c,
        Err(r) => return Ok(r),
    };
    if !auth::require_admin(&app.principal) {
        return Ok(cors::deny("admin authorization required"));
    }
    let tenant = match app.engine.tenant().await {
        Ok(t) => t,
        Err(e) => return Ok(cors::srv(&e)),
    };
    let mut records: i64 = 0;
    if let Ok(tables) = app.engine.list_tables().await {
        for t in tables {
            records += app.engine.count_records(&t.table).await.unwrap_or(0);
        }
    }
    let mut storage: u64 = 0;
    let store = app.engine.object_store();
    for prefix in [format!("{}/files/", engine::TENANT), format!("{}/assets/", engine::TENANT)] {
        if let Ok(keys) = store.list(&prefix).await {
            storage += keys.iter().map(|k| k.size).sum::<u64>();
        }
    }
    let limits =
        engine::policy::RateLimits::from_json(tenant.rate_json.as_ref().unwrap_or(&Json::Null));
    Ok(cors::ok(json!({
        "tenant": engine::TENANT,
        "records": { "count": records },
        "storage": { "bytes": storage },
        "rate": { "limits": {
            "submit": limits.submit, "upload": limits.upload,
            "search": limits.search, "read": limits.read, "per_day": limits.per_day,
        } },
    })))
}

// ---- auth ---------------------------------------------------------------

pub async fn signup(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let mut app = match auth::ctx_for(&req, &ctx).await {
        Ok(c) => c,
        Err(r) => return Ok(r),
    };
    let body = match body_json_for(req, MAX_JSON).await {
        Ok(b) => b,
        Err(r) => return Ok(r),
    };
    let email = body.get("email").and_then(|e| e.as_str()).unwrap_or("").to_string();
    let password = body.get("password").and_then(|p| p.as_str()).unwrap_or("").to_string();
    let password_hash =
        body.get("password_hash").and_then(|p| p.as_str()).filter(|h| !h.is_empty());
    let role = body.get("role").and_then(|r| r.as_str()).unwrap_or("reader").to_string();
    if !password.is_empty() == password_hash.is_some() {
        return Ok(cors::err(400, "provide exactly one of password or password_hash"));
    }
    let principal = app.principal.clone();
    Ok(match app.engine.signup_user(&email, &password, password_hash, &role, &principal).await {
        Ok(user) => cors::created(json!({ "ok": true, "user": user })),
        Err(e) => cors::bad(&e),
    })
}

pub async fn login(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let mut app = match auth::ctx_for(&req, &ctx).await {
        Ok(c) => c,
        Err(r) => return Ok(r),
    };
    let body = match body_json_for(req, MAX_JSON).await {
        Ok(b) => b,
        Err(r) => return Ok(r),
    };
    let email = body.get("email").and_then(|e| e.as_str()).unwrap_or("").to_string();
    let password = body.get("password").and_then(|p| p.as_str()).unwrap_or("").to_string();
    Ok(match app.engine.login_user(&email, &password).await {
        Ok((token, jwt)) => cors::ok(json!({ "ok": true, "token": token, "jwt": jwt })),
        Err(e) => cors::err(401, &e.to_string()),
    })
}

pub async fn logout(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let mut app = match auth::ctx_for(&req, &ctx).await {
        Ok(c) => c,
        Err(r) => return Ok(r),
    };
    let body = match body_json_for(req, MAX_JSON).await {
        Ok(b) => b,
        Err(r) => return Ok(r),
    };
    let token = body.get("token").and_then(|t| t.as_str()).unwrap_or("").to_string();
    Ok(match app.engine.logout_user(&token).await {
        Ok(_) => cors::ok(json!({ "ok": true })),
        Err(e) => cors::bad(&e),
    })
}

pub async fn set_role(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let mut app = match auth::ctx_for(&req, &ctx).await {
        Ok(c) => c,
        Err(r) => return Ok(r),
    };
    if !auth::require_admin(&app.principal) {
        return Ok(cors::deny("admin authorization required"));
    }
    let body = match body_json_for(req, MAX_JSON).await {
        Ok(b) => b,
        Err(r) => return Ok(r),
    };
    let email = body.get("email").and_then(|e| e.as_str()).unwrap_or("").to_string();
    let role = body.get("role").and_then(|r| r.as_str()).unwrap_or("").to_string();
    if email.is_empty() || role.is_empty() {
        return Ok(cors::err(400, "email and role are required"));
    }
    let principal = app.principal.clone();
    Ok(match app.engine.set_user_role(&email, &role, &principal).await {
        Ok(user) => cors::ok(json!({ "ok": true, "user": user })),
        Err(e) => cors::bad(&e),
    })
}

pub async fn me(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let app = match auth::ctx_for(&req, &ctx).await {
        Ok(c) => c,
        Err(r) => return Ok(r),
    };
    // Token from the same sources as the principal resolution (bearer / ?key=).
    let p = query::params(&req);
    let token = p
        .get("key")
        .cloned()
        .filter(|k| !k.is_empty())
        .or_else(|| {
            req.headers()
                .get("authorization")
                .ok()
                .flatten()
                .and_then(|v| {
                    v.strip_prefix("Bearer ")
                        .or_else(|| v.strip_prefix("bearer "))
                        .map(|s| s.trim().to_string())
                })
                .filter(|s| !s.is_empty())
        });
    let Some(token) = token.filter(|t| !t.is_empty()) else {
        return Ok(cors::err(401, "missing session token"));
    };
    Ok(match app.engine.user_by_token(&token).await {
        Ok(Some(user)) => cors::ok(json!({ "ok": true, "user": user })),
        Ok(None) => cors::err(401, "invalid or expired session"),
        Err(e) => cors::bad(&e),
    })
}

// ---- files --------------------------------------------------------------

pub async fn upload(mut req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let p = query::params(&req);
    let mut app = match auth::ctx_for(&req, &ctx).await {
        Ok(c) => c,
        Err(r) => return Ok(r),
    };
    if !auth::require_write(&app.principal) {
        return Ok(cors::deny("writer authorization required"));
    }
    let bytes = match req.bytes().await {
        Ok(b) => b,
        Err(_) => return Ok(cors::err(400, "unreadable body")),
    };
    if bytes.is_empty() {
        return Ok(cors::err(400, "empty file"));
    }
    if bytes.len() > MAX_UPLOAD {
        return Ok(cors::err(413, "file too large (25 MiB max)"));
    }
    let table = p.get("table").map(|s| s.as_str()).unwrap_or("files").to_string();
    // Uploads land as records in a table — table-scoped keys (S2) stay scoped.
    if !app.principal.allows_table(&table) {
        return Ok(cors::deny("key scope excludes this table"));
    }
    let content_type = req
        .headers()
        .get("content-type")
        .ok()
        .flatten()
        .unwrap_or_else(|| "application/octet-stream".to_string());
    let filename = req
        .headers()
        .get("x-filename")
        .ok()
        .flatten()
        .unwrap_or_else(|| "upload".to_string());
    let meta = json!({ "name": filename, "type": content_type });
    let folder = p.get("folder").map(|s| s.as_str());
    Ok(match app.engine.upload(&table, &filename, &content_type, &bytes, &meta, folder).await {
        Ok(seq) => cors::ok(json!({ "ok": true, "seq": seq, "table": table })),
        Err(e) => cors::srv(&e),
    })
}

pub async fn file(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let p = query::params(&req);
    let mut app = match auth::ctx_for(&req, &ctx).await {
        Ok(c) => c,
        Err(r) => return Ok(r),
    };
    let file = match p.get("file").filter(|f| !f.is_empty()) {
        Some(f) => f.clone(),
        None => return Ok(cors::err(400, "invalid file")),
    };
    let tenant = match app.engine.tenant().await {
        Ok(t) => t,
        Err(e) => return Ok(cors::srv(&e)),
    };
    if !tenant.public_reads && !auth::require_read(&app.principal) {
        return Ok(cors::deny("private app"));
    }
    let table = p.get("table").map(|s| s.as_str()).unwrap_or("files").to_string();
    // Resolve the blob key: `{tenant}/files/…` used as-is; a bare name is
    // looked up in the table's records by `name`.
    let full = if file.starts_with("files/") {
        format!("{}/{file}", engine::TENANT)
    } else {
        let sf = engine::storage::ir::SrvFilter {
            conds: vec![engine::storage::ir::FilterCond {
                field: "$.name".to_string(),
                op: engine::storage::ir::Op::Eq,
                value: Json::String(file.clone()),
            }],
        };
        app.engine
            .query_records(&table, &sf, &[], 1, 0)
            .await
            .ok()
            .and_then(|rs| rs.into_iter().next())
            .and_then(|r| r.payload.get("file").and_then(|v| v.as_str()).map(String::from))
            .unwrap_or(file.clone())
    };
    Ok(match app.engine.download(&table, &full).await {
        Ok(Some((data, content_type))) => cors::bytes(data, &content_type, "public, max-age=3600"),
        Ok(None) => cors::gone("not found"),
        Err(e) => cors::srv(&e),
    })
}

pub async fn call(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let app = match auth::ctx_for(&req, &ctx).await {
        Ok(c) => c,
        Err(r) => return Ok(r),
    };
    if !auth::require_admin(&app.principal) {
        return Ok(cors::deny("admin authorization required"));
    }
    let body = match body_json_for(req, MAX_JSON).await {
        Ok(b) => b,
        Err(r) => return Ok(r),
    };
    let url = match body.get("url").and_then(|u| u.as_str()) {
        Some(u) if !u.is_empty() => u.to_string(),
        _ => return Ok(cors::err(400, "missing url")),
    };
    let headers: Vec<(String, String)> = body
        .get("headers")
        .and_then(|h| h.as_object())
        .map(|map| {
            map.iter().filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string()))).collect()
        })
        .unwrap_or_default();
    let secrets = app.engine.secrets_map().await.unwrap_or_default();
    let url = engine::automation::resolve_secret_placeholders(&secrets, &url);
    let mut headers = headers;
    for (_, v) in headers.iter_mut() {
        *v = engine::automation::resolve_secret_placeholders(&secrets, v);
    }
    let mut call_body = body.get("body").cloned().unwrap_or(Json::Null);
    engine::automation::subst_secret_json(&secrets, &mut call_body);
    Ok(match engine::automation::srv_http_call(&url, &headers, &call_body).await {
        Ok(res) => cors::ok(json!({ "ok": true, "status": 200, "body": res })),
        Err(e) => cors::bad(&e),
    })
}

// ---- inbound events -----------------------------------------------------

pub async fn events_inbound(mut req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let mut app = match auth::ctx_for(&req, &ctx).await {
        Ok(c) => c,
        Err(r) => return Ok(r),
    };
    let bytes = match req.bytes().await {
        Ok(b) => b,
        Err(_) => return Ok(cors::err(400, "unreadable body")),
    };
    if bytes.len() > 10_000_000 {
        return Ok(cors::err(413, "event too large"));
    }
    let payload: Json = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(_) => return Ok(cors::err(400, "invalid json")),
    };
    // Phase A: HMAC check, then recipe matching + DB actions with deferred
    // `$call` collection. The engine is per-request (nothing shared), so no
    // lock discipline is needed around phase B.
    let secret =
        app.engine.tenant().await.ok().and_then(|b| b.webhook_secret.clone());
    if let Some(secret) = &secret {
        let provided = req
            .headers()
            .get("x-hub-signature-256")
            .ok()
            .flatten()
            .map(|s| s.strip_prefix("sha256=").unwrap_or(s.as_str()).to_string())
            .unwrap_or_default();
        let expected = engine::webhooks::hmac_hex(&String::from_utf8_lossy(&bytes), secret);
        if !provided.eq_ignore_ascii_case(&expected) {
            return Ok(cors::err(401, "invalid signature"));
        }
    }
    let mut out =
        engine::automation::DispatchOutcome { pending: Vec::new(), writebacks: Vec::new() };
    if let Err(e) = app
        .engine
        .dispatch_recipes_phased_a(
            "inbound",
            engine::events::EventKind::Inbound,
            None,
            Some(payload),
            &mut out,
        )
        .await
    {
        return Ok(cors::srv(&e));
    }
    // Phase B: execute deferred `$call` HTTP via Fetch.
    let mut payload_out = Json::Null;
    let mut logs = Vec::new();
    engine::automation::execute_pending(out.pending, &mut payload_out, &mut logs).await;
    let _ = logs;
    Ok(cors::ok(json!({ "ok": true })))
}

// ---- assets -------------------------------------------------------------

pub async fn asset_put(mut req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let rel = match ctx.param("rel").filter(|r| !r.is_empty()).cloned() {
        Some(r) => r,
        None => return Ok(cors::err(400, "missing asset path")),
    };
    let app = match auth::ctx_for(&req, &ctx).await {
        Ok(c) => c,
        Err(r) => return Ok(r),
    };
    if !auth::require_write(&app.principal) {
        return Ok(cors::deny("writer authorization required"));
    }
    let bytes = match req.bytes().await {
        Ok(b) => b,
        Err(_) => return Ok(cors::err(400, "unreadable body")),
    };
    if bytes.is_empty() {
        return Ok(cors::err(400, "empty asset"));
    }
    Ok(match app.engine.put_asset(&rel, &bytes).await {
        Ok(()) => {
            invalidate_asset(&req, &rel).await;
            cors::ok(json!({ "ok": true, "asset": rel }))
        }
        Err(e) => cors::bad(&e),
    })
}

pub async fn asset_get(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let rel = match ctx.param("rel").filter(|r| !r.is_empty()).cloned() {
        Some(r) => r,
        None => return Ok(cors::err(400, "missing asset path")),
    };
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
    // Validator = hash of the served bytes (uniform with /srv/): exact on
    // every adapter, verifiable with sha256sum. One read either way.
    // Browser directive matches /srv/ per content type (html revalidates).
    Ok(match app.engine.get_asset(&rel).await {
        Ok(Some((data, ct))) => {
            let etag = crate::handlers_site::sha256_hex(&data);
            if cors::etag_matches(&req, &etag) {
                return Ok(cors::not_modified(&etag));
            }
            let cache = if ct.starts_with("text/html") { "public, max-age=0" } else { "public, max-age=3600" };
            cors::asset_bytes(data, &ct, cache, &etag)
        }
        Ok(None) => cors::gone("not found"),
        Err(e) => cors::bad(&e),
    })
}

/// Drop a stored asset's edge-cache entries (both serving URLs) after a
/// write or delete, so the next GET fetches fresh bytes immediately.
/// Best-effort: cache errors are swallowed by the caller paths.
async fn invalidate_asset(req: &Request, rel: &str) {
    let Ok(url) = req.url() else {
        return;
    };
    let base = url.origin().ascii_serialization();
    let base = base.trim_end_matches('/');
    let cache = worker::Cache::default();
    for path in [format!("{base}/srv/{rel}"), format!("{base}/api/assets/{rel}")] {
        let _ = cache.delete(path.as_str(), false).await;
    }
}

pub async fn asset_list(req: Request, ctx: RouteContext<()>) -> Result<Response> {
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
    Ok(match app.engine.list_assets().await {
        Ok(keys) => {
            let list: Vec<Json> =
                keys.into_iter().map(|k| json!({ "key": k.key, "size": k.size })).collect();
            cors::ok(json!({ "ok": true, "assets": list }))
        }
        Err(e) => cors::srv(&e),
    })
}

pub async fn asset_delete(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let rel = match ctx.param("rel").filter(|r| !r.is_empty()).cloned() {
        Some(r) => r,
        None => return Ok(cors::err(400, "missing asset path")),
    };
    let app = match auth::ctx_for(&req, &ctx).await {
        Ok(c) => c,
        Err(r) => return Ok(r),
    };
    if !auth::require_write(&app.principal) {
        return Ok(cors::deny("writer authorization required"));
    }
    Ok(match app.engine.delete_asset(&rel).await {
        Ok(true) => {
            invalidate_asset(&req, &rel).await;
            cors::ok(json!({ "ok": true, "asset": rel }))
        }
        Ok(false) => cors::gone("not found"),
        Err(e) => cors::bad(&e),
    })
}

// ---- system / mcp / realtime placeholder --------------------------------

pub async fn system_health(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let app = match auth::ctx_for(&req, &ctx).await {
        Ok(c) => c,
        Err(r) => return Ok(r),
    };
    // Depth probe: a failed D1 round-trip means the worker cannot serve real
    // work. No /proc, no threads on the edge.
    let mut engine = app.engine;
    let db_ok = engine.tenant().await.is_ok();
    Ok(cors::ok(json!({
        "ok": db_ok,
        "service": "backend-engine",
        "tenant": engine::TENANT,
        "version": engine::VERSION,
        "db": if db_ok { "connected" } else { "unreachable" },
    })))
}

/// Run one CLI command string with the request's principal and render the
/// terminal-friendly outcome object. Shared by GET/POST command mode.
async fn run_command(app: &mut auth::Ctx, command: &str) -> Response {
    match mcp::cli::execute(&mut app.engine, &app.principal, command).await {
        Ok(mcp::cli::Outcome::Value(value)) => cors::ok(json!({ "ok": true, "result": value })),
        Ok(mcp::cli::Outcome::Help(text)) => cors::ok(json!({ "ok": true, "help": text })),
        Err(error) => cors::ok(json!({ "ok": false, "error": error })),
    }
}

pub async fn mcp(mut req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let open = auth::management_open(&ctx.env);
    let mut app = match auth::ctx_for(&req, &ctx).await {
        Ok(c) => c,
        Err(r) => return Ok(r),
    };
    if !open && !auth::require_write(&app.principal) {
        return Ok(cors::deny("writer authorization required"));
    }
    let body = match req.text().await {
        Ok(b) => b,
        Err(_) => return Ok(cors::err(400, "unreadable body")),
    };
    // Plain `{"command": "..."}` runs CLI mode; anything with a JSON-RPC
    // "method" keeps the standard MCP path.
    if let Ok(v) = serde_json::from_str::<Json>(&body) {
        if let Some(command) = v.get("command").and_then(|c| c.as_str()) {
            return Ok(run_command(&mut app, command).await);
        }
    }
    let server = mcp::McpServer::new(std::sync::Arc::new(std::sync::Mutex::new(app.engine)));
    let out = mcp::handle_jsonrpc(&server, &body).await;
    Ok(cors::bytes(out.into_bytes(), "application/json", "no-cache"))
}

/// Terminal door without any MCP client: `GET /mcp?command=records+list+notes`.
/// Same auth gate as POST /mcp; same command grammar as the single MCP tool.
/// `GET /mcp` with no command returns the setup sheet (open, no secrets).
pub async fn mcp_get(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let p = query::params(&req);
    let open = auth::management_open(&ctx.env);
    let Some(command) = p.get("command").filter(|c| !c.is_empty()).cloned() else {
        return Ok(mcp_setup(&req, open));
    };
    let mut app = match auth::ctx_for(&req, &ctx).await {
        Ok(c) => c,
        Err(r) => return Ok(r),
    };
    if !open && !auth::require_write(&app.principal) {
        return Ok(cors::deny("writer authorization required"));
    }
    Ok(run_command(&mut app, &command).await)
}

/// Setup sheet: how to point an MCP client here, and how to call direct
/// commands — bearer included as a placeholder, never the real key.
fn mcp_setup(req: &Request, open: bool) -> Response {
    let origin = req
        .url()
        .ok()
        .map(|u| {
            let s = u.as_str().to_string();
            match s.find("://").and_then(|i| s[i + 3..].find('/').map(|j| i + 3 + j)) {
                Some(end) => s[..end].to_string(),
                None => s,
            }
        })
        .unwrap_or_default();
    let url = format!("{origin}/mcp");
    if open {
        return cors::ok(serde_json::json!({
            "ok": true,
            "tool": "manage_serverless_engine",
            "url": url,
            "auth": "open — no bearer needed (set WORKER_KEY to lock it down)",
            "mcp_client": { "mcpServers": { "backend-engine": { "url": url } } },
            "terminal": format!("curl '{url}?command=--help'"),
        }));
    }
    cors::ok(serde_json::json!({
        "ok": true,
        "tool": "manage_serverless_engine",
        "url": url,
        "auth": "Authorization: Bearer WORKER_KEY",
        "mcp_client": { "mcpServers": { "backend-engine": {
            "url": url,
            "headers": { "Authorization": "Bearer WORKER_KEY" },
        } } },
        "terminal": format!("curl -H 'Authorization: Bearer WORKER_KEY' '{url}?command=--help'"),
    }))
}

/// Realtime event stream (Server-Sent Events).
///
/// The donor fanned in-process broadcasts here; on the edge there is no
/// shared process, so this polls D1 per table (2 s) and streams new records.
/// Same `after=` replay + `filter=` semantics, same `record` event shape.
/// Two deliberate fixes vs the donor: reads are auth-gated (the old branch
/// had no principal check at all) and customer-scoped keys are refused.
/// Record WS (`/ws` broadcast) stays a Phase 9 item (needs DO hibernation).
pub async fn events_stream(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let p = query::params(&req);
    let mut app = match auth::ctx_for(&req, &ctx).await {
        Ok(c) => c,
        Err(r) => return Ok(r),
    };
    if app.principal.scope.is_some() {
        return Ok(cors::deny("event streams forbidden for scoped keys"));
    }
    if !auth::can_read(&mut app).await {
        return Ok(cors::deny("private app"));
    }
    let base_filter = match query::filter_from(&p) {
        Ok(f) => f,
        Err(e) => return Ok(cors::bad(&e)),
    };
    let mut after: i64 = p.get("after").and_then(|v| v.parse().ok()).unwrap_or(0);
    let engine = app.engine;
    let tables: Vec<String> = engine
        .list_tables()
        .await
        .map(|t| t.into_iter().map(|c| c.table).collect())
        .unwrap_or_default();

    let s = async_stream::stream! {
        // Backfill first (newest cap mirrors the donor), then poll.
        let mut first = true;
        loop {
            let mut batch: Vec<(i64, String)> = Vec::new();
            for table in &tables {
                let mut conds = base_filter.conds.clone();
                conds.push(engine::storage::ir::FilterCond {
                    field: "$.seq".to_string(),
                    op: engine::storage::ir::Op::Gt,
                    value: serde_json::Value::from(after),
                });
                let sf = engine::storage::ir::SrvFilter { conds };
                let limit = if first { 1000 } else { 100 };
                if let Ok(recs) = engine
                    .query_records(table, &sf, &[("$.seq".to_string(), false)], limit, 0)
                    .await
                {
                    for r in recs {
                        let frame = format!(
                            "event: record\ndata: {}\n\n",
                            serde_json::json!({
                                "type": "record",
                                "seq": r.seq,
                                "record": {
                                    "seq": r.seq,
                                    "payload": r.payload,
                                    "created_at": r.created_at,
                                    "writer": r.writer,
                                },
                            })
                        );
                        batch.push((r.seq, frame));
                    }
                }
            }
            batch.sort_by_key(|(seq, _)| *seq);
            for (seq, frame) in batch {
                if seq > after {
                    after = seq;
                }
                yield Ok::<Vec<u8>, worker::Error>(frame.into_bytes());
            }
            first = false;
            worker::Delay::from(std::time::Duration::from_secs(2)).await;
        }
    };
    Ok(Response::from_stream(s)?.with_headers(cors_headers_for_stream()))
}

fn cors_headers_for_stream() -> worker::Headers {
    let h = worker::Headers::new();
    let _ = h.set("access-control-allow-origin", "*");
    let _ = h.set("content-type", "text/event-stream");
    let _ = h.set("cache-control", "no-cache");
    h
}
