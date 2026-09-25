use super::ok;
use engine::model::Principal;
use engine::ServerlessEngine;
use serde_json::{json, Value as Json};

/// `endpoints.list` — the platform API surface a board's front-end relies on.
///
/// These routes are compiled into the server binary (same for every board),
/// not stored per-app, so this tool documents the contract rather than reading
/// a config row.
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
        ("GET", "/api/srv/{board}", "App info"),
        ("DELETE", "/api/srv/{board}", "Delete app"),
        ("PATCH", "/api/srv/{board}/app", "Update app config"),
        ("GET", "/api/srv/{board}/resources", "Usage/rates/resources"),
        ("GET", "/api/srv/{board}/tables", "List tables"),
        ("POST", "/api/srv/{board}/tables", "Create table"),
        ("GET", "/api/srv/{board}/tables/{table}", "Show table config"),
        ("DELETE", "/api/srv/{board}/tables/{table}", "Delete table"),
        ("POST", "/api/srv/{board}/tables/{table}/submit", "Insert record"),
        ("POST", "/api/srv/{board}/tables/{table}/bulk", "Bulk insert"),
        ("POST", "/api/srv/{board}/tables/{table}/import", "Import json/csv"),
        ("GET", "/api/srv/{board}/tables/{table}/records", "List records"),
        ("DELETE", "/api/srv/{board}/tables/{table}/records", "Delete records (filter)"),
        ("GET", "/api/srv/{board}/tables/{table}/query", "Filter records"),
        ("GET", "/api/srv/{board}/tables/{table}/aggregate", "Aggregate (sum/avg/...)"),
        ("GET", "/api/srv/{board}/tables/{table}/record", "Get one record by seq/query"),
        ("PUT", "/api/srv/{board}/tables/{table}/records/{seq}", "Put (upsert) record"),
        ("PATCH", "/api/srv/{board}/tables/{table}/records/{seq}", "Patch record"),
        ("DELETE", "/api/srv/{board}/tables/{table}/records/{seq}", "Delete record"),
        ("POST", "/api/srv/{board}/auth/signup", "Create user"),
        ("POST", "/api/srv/{board}/auth/login", "Login (session token + jwt)"),
        ("POST", "/api/srv/{board}/auth/logout", "Logout"),
        ("POST", "/api/srv/{board}/auth/role", "Set role"),
        ("GET", "/api/srv/{board}/auth/me", "Resolve session"),
        ("GET", "/api/srv/{board}/auth/oauth/microsoft/start", "Microsoft SSO start"),
        ("GET", "/api/srv/{board}/auth/oauth/microsoft/callback", "Microsoft SSO callback"),
        ("POST", "/api/srv/{board}/upload", "Upload file"),
        ("GET", "/api/srv/{board}/file", "Download file"),
        ("POST", "/api/srv/{board}/call", "Outbound HTTP call"),
        ("POST", "/api/srv/{board}/events", "Inbound webhook/event"),
        ("GET", "/api/srv/{board}/events", "SSE event stream (realtime)"),
        ("GET", "/api/srv/{board}/keys", "List keys"),
        ("POST", "/api/srv/{board}/keys", "Issue key"),
        ("DELETE", "/api/srv/{board}/keys", "Revoke key"),
        ("GET", "/api/srv/{board}/jobs", "List jobs"),
        ("POST", "/api/srv/{board}/jobs", "Add job"),
        ("DELETE", "/api/srv/{board}/jobs", "Remove job"),
        ("GET", "/api/srv/{board}/jobs/runs", "Job run history"),
        ("GET", "/api/srv/{board}/recipes", "List recipes"),
        ("POST", "/api/srv/{board}/recipes", "Add recipe"),
        ("PATCH", "/api/srv/{board}/recipes/{name}", "Enable/disable recipe"),
        ("DELETE", "/api/srv/{board}/recipes/{name}", "Remove recipe"),
        ("GET", "/api/srv/{board}/secrets", "List secrets"),
        ("POST", "/api/srv/{board}/secrets", "Set secret"),
        ("DELETE", "/api/srv/{board}/secrets/{name}", "Delete secret"),
        ("GET", "/api/srv/{board}/assets", "List assets"),
        ("PUT", "/api/srv/{board}/assets/*", "Put asset"),
        ("GET", "/api/srv/{board}/assets/*", "Get asset"),
        ("DELETE", "/api/srv/{board}/assets/*", "Delete asset"),
    ]
    .iter()
    .map(|(m, p, d)| json!({ "method": m, "path": p, "desc": d }))
    .collect();

    let mut non_api: Vec<Json> = [
        ("GET", "/srv/{board}/", "SPA hosting: front-end dist (index.html + assets/*)"),
        ("GET", "/srv/{board}/ai/*", "AI assistant proxy → AI_BASE_URL target"),
        ("GET", "/srv/{board}/ai/ws", "AI assistant WebSocket tunnel → target /ws"),
        ("GET", "/ws", "Legacy root WS → env SRV_AI_TARGET /ws"),
        ("GET", "/api/system/health", "Host health ('School VPS Node' widget)"),
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
        "ai_proxy": {
            "mount": "/srv/{board}/ai/",
            "target_source": "board secret AI_BASE_URL (fallback: env SRV_AI_TARGET)",
            "ws": { "board_mount": "/srv/{board}/ai/ws", "legacy": "/ws" }
        },
        "sso": {
            "provider": "microsoft",
            "secrets_used": ["MICROSOFT_CLIENT_ID", "MICROSOFT_CLIENT_SECRET", "MICROSOFT_TENANT_ID"],
            "routes": ["/api/srv/{board}/auth/oauth/microsoft/start", "/api/srv/{board}/auth/oauth/microsoft/callback"]
        },
        "realtime": { "sse": "/api/srv/{board}/events?after={seq}" }
    }))
}
