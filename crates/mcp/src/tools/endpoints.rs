use super::ok;
use engine::model::Principal;
use engine::ServerlessEngine;
use serde_json::{json, Value as Json};

/// `endpoints.list` — the platform API surface a front-end or agent relies on.
///
/// Single tenant: one Worker serves one app, so there is no `{board}` segment
/// anywhere. These routes are compiled into the worker binary, not stored
/// per-app, so this tool documents the contract rather than reading config.
pub fn list(
    _engine: &ServerlessEngine,
    _principal: &Principal,
    arguments: &Json,
) -> Result<Json, String> {
    let dir = arguments
        .get("dir")
        .and_then(|d| d.as_str())
        .unwrap_or("desc")
        .to_string();
    if !matches!(dir.as_str(), "asc" | "desc") {
        return Err("dir must be one of: asc | desc".into());
    }
    let mut endpoints: Vec<Json> = [
        ("GET", "/api/app", "App info (tenant config)"),
        ("PATCH", "/api/app", "Update tenant config (title, public_reads)"),
        ("GET", "/api/resources", "Usage: records, storage bytes, rate limits"),
        ("POST", "/api/auth/signup", "Create user"),
        ("POST", "/api/auth/login", "Login (session token + jwt)"),
        ("POST", "/api/auth/logout", "Logout"),
        ("POST", "/api/auth/role", "Set role (admin)"),
        ("GET", "/api/auth/me", "Resolve session"),
        ("GET", "/api/auth/oauth/start", "SSO login start (redirects to provider)"),
        ("GET", "/api/auth/oauth/callback", "SSO callback (session + redirect)"),
        ("GET", "/api/tables", "List tables"),
        ("POST", "/api/tables", "Create table"),
        ("GET", "/api/tables/{table}", "Show table config"),
        ("DELETE", "/api/tables/{table}", "Delete table"),
        ("POST", "/api/tables/{table}/submit", "Insert record (?upsert=1)"),
        ("POST", "/api/tables/{table}/bulk", "Bulk insert (?upsert=1, ?migrate=1)"),
        ("POST", "/api/tables/{table}/import", "Import json/csv"),
        ("GET", "/api/tables/{table}/records", "List records"),
        ("DELETE", "/api/tables/{table}/records", "Delete records (?filter=, no scoped keys)"),
        ("GET", "/api/tables/{table}/query", "Filter records (?filter, ?order, ?q, ?hl)"),
        ("GET", "/api/tables/{table}/aggregate", "Aggregate (?op, ?field, ?group)"),
        ("GET", "/api/recipes/{name}", "Show one recipe"),
        ("PUT", "/api/tables/{table}/records/{seq}", "Replace record"),
        ("PATCH", "/api/tables/{table}/records/{seq}", "Patch record"),
        ("DELETE", "/api/tables/{table}/records/{seq}", "Delete record"),
        ("POST", "/api/upload", "Upload file (raw bytes, X-Filename, ?table=, ?folder=)"),
        ("GET", "/api/file", "Download file (?file=, ?table=)"),
        ("POST", "/api/call", "Outbound HTTP call (admin)"),
        ("POST", "/api/events", "Inbound webhook/event (HMAC)"),
        ("GET", "/api/events", "Realtime stream — 501 until TenantDO fan-out"),
        ("GET", "/api/keys", "List keys"),
        ("POST", "/api/keys", "Issue key"),
        ("DELETE", "/api/keys", "Revoke key (?bucket=)"),
        ("GET", "/api/hooks", "List webhooks"),
        ("POST", "/api/hooks", "Register webhook"),
        ("DELETE", "/api/hooks", "Remove webhook (?url=)"),
        ("GET", "/api/hooks/deliveries", "Due delivery queue"),
        ("GET", "/api/jobs", "List jobs"),
        ("POST", "/api/jobs", "Add job"),
        ("DELETE", "/api/jobs", "Remove job (?name=)"),
        ("GET", "/api/jobs/runs", "Job run history"),
        ("GET", "/api/recipes", "List recipes"),
        ("POST", "/api/recipes", "Add recipe"),
        ("PATCH", "/api/recipes/{name}", "Enable/disable recipe"),
        ("DELETE", "/api/recipes/{name}", "Remove recipe"),
        ("GET", "/api/secrets", "List secrets"),
        ("POST", "/api/secrets", "Set secret"),
        ("DELETE", "/api/secrets/{name}", "Delete secret"),
        ("POST", "/api/email/send", "Send email via MAIL_* secrets (admin)"),
        ("PUT", "/api/rate", "Set rate config"),
        ("PUT", "/api/ttl", "Set table TTL"),
        ("DELETE", "/api/ttl", "Clear table TTL (?table=)"),
        ("PUT", "/api/link", "Set table link"),
        ("DELETE", "/api/link", "Clear link"),
        ("PUT", "/api/audit", "Enable/disable audit"),
        ("GET", "/api/audit", "Query audit"),
        ("PUT", "/api/computed", "Set computed (?table=)"),
        ("DELETE", "/api/computed", "Clear computed (?table=)"),
        ("PUT", "/api/validate", "Set validation (?table=)"),
        ("DELETE", "/api/validate", "Clear validation (?table=)"),
        ("PUT", "/api/redact", "Set redaction (?table=)"),
        ("DELETE", "/api/redact", "Clear redaction (?table=)"),
        ("PUT", "/api/webhook_secret", "Set inbound secret (≥16 chars)"),
        ("DELETE", "/api/webhook_secret", "Clear inbound secret"),
        ("GET", "/api/config", "Aggregate tenant snapshot"),
        ("GET", "/api/assets", "List assets"),
        ("PUT", "/api/assets/*", "Put asset"),
        ("GET", "/api/assets/*", "Get asset"),
        ("DELETE", "/api/assets/*", "Delete asset"),
        ("POST", "/mcp", "MCP JSON-RPC (tools/call) or {\"command\"} CLI mode"),
        ("GET", "/mcp", "Setup sheet (no command) or CLI-over-URL (?command=)"),
    ]
    .iter()
    .map(|(m, p, d)| json!({ "method": m, "path": p, "desc": d }))
    .collect();

    let mut non_api: Vec<Json> = [
        ("ANY", "/srv/ai/*", "AI assistant reverse proxy (HTML/JS rewritten to mount)"),
        ("WS", "/srv/ai/ws", "AI assistant socket (framed bridge to upstream /ws)"),
        ("WS", "/ws", "Legacy AI socket (AI bundle dials host-root /ws)"),
        ("GET", "/srv/", "SPA hosting: front-end dist (index.html + assets/*, sub-app slugs)"),
        ("GET", "/api/system/health", "Worker health (version, tenant, D1 probe)"),
        ("GET", "/api/version", "Build version"),
        ("GET", "/healthz", "Liveness"),
    ]
    .iter()
    .map(|(m, p, d)| json!({ "method": m, "path": p, "desc": d }))
    .collect();

    // ORDER BY path [asc|desc], method as tiebreak — stable so the list reads
    // like a DB query instead of the compiled insertion order.
    let key = |e: &Json| {
        let path = e.get("path").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let method = e.get("method").and_then(|v| v.as_str()).unwrap_or("").to_string();
        (path, method)
    };
    endpoints.sort_by(|a, b| key(a).cmp(&key(b)));
    non_api.sort_by(|a, b| key(a).cmp(&key(b)));
    if dir == "desc" {
        endpoints.reverse();
        non_api.reverse();
    }

    ok(json!({
        "endpoints": endpoints,
        "non_api_routes": non_api,
        "auth": {
            "admin": "WORKER_KEY bearer (secret binding) — full access, never in the DB",
            "keys": "POST /api/keys (admin) issues reader/writer/customer keys; ?key= works like a bearer",
            "public_reads": "tenant flag (PATCH /api/app) allows anonymous reads",
        },
        "realtime": { "sse": "GET /api/events is 501 until the TenantDO fan-out lands" }
    }))
}
