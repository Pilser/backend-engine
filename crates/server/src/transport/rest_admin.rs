use super::rest::{json_response, ok_json, err_json, BoxBodyResp};
use crate::Server;
use bytes::Bytes;
use engine::model::{Principal, Recipe};
use hyper::{Method, Response, StatusCode};
use serde_json::{json, Value as Json};
use std::collections::HashMap;

fn require_admin_ok(_server: &Server, principal: &Principal) -> Option<Response<BoxBodyResp>> {
    if !super::rest::require_admin(principal) {
        Some(err_json(StatusCode::FORBIDDEN, "admin authorization required"))
    } else {
        None
    }
}

pub fn handle_admin(
    server: &Server,
    method: &Method,
    board: &str,
    tail: &[&str],
    _headers: &http::HeaderMap,
    body: &Bytes,
    params: &HashMap<String, String>,
    principal: &Principal,
) -> Option<Response<BoxBodyResp>> {
    match (method.as_str(), tail) {
        ("POST", ["keys"]) => Some(issue_key(server, board, principal, body)),
        ("GET", ["keys"]) => Some(list_keys(server, board, principal)),
        ("DELETE", ["keys"]) => Some(revoke_key(server, board, principal, params)),
        ("GET", ["hooks"]) => Some(list_hooks(server, board, principal)),
        ("POST", ["hooks"]) => Some(register_hook(server, board, principal, body)),
        ("DELETE", ["hooks"]) => Some(remove_hook(server, board, principal, params)),
        ("GET", ["hooks", "deliveries"]) => Some(hook_deliveries(server, board, principal, params)),
        ("GET", ["jobs"]) => Some(list_jobs(server, board, principal)),
        ("POST", ["jobs"]) => Some(add_job(server, board, principal, body)),
        ("DELETE", ["jobs"]) => Some(remove_job(server, board, principal, params)),
        ("GET", ["jobs", "runs"]) => Some(job_runs(server, board, principal, params)),
        ("GET", ["recipes"]) => Some(list_recipes(server, board, principal)),
        ("POST", ["recipes"]) => Some(add_recipe(server, board, principal, body)),
        ("PATCH", ["recipes", name]) => Some(set_recipe_enabled(server, board, principal, name, body)),
        ("DELETE", ["recipes", name]) => Some(remove_recipe(server, board, principal, name)),
        ("GET", ["secrets"]) => Some(list_secrets(server, board, principal)),
        ("POST", ["secrets"]) => Some(set_secret(server, board, principal, body)),
        ("DELETE", ["secrets", name]) => Some(remove_secret(server, board, principal, name)),
        ("POST", ["rate"]) => Some(rate_config(server, board, principal, body)),
        ("PUT", ["ttl"]) => Some(ttl_set(server, board, principal, body)),
        ("DELETE", ["ttl"]) => Some(ttl_clear(server, board, principal, params)),
        ("PUT", ["link"]) => Some(link_set(server, board, principal, body)),
        ("DELETE", ["link"]) => Some(link_clear(server, board, principal)),
        ("PUT", ["audit"]) => Some(audit_set(server, board, principal, body)),
        ("GET", ["audit"]) => Some(audit_list(server, board, principal, params)),
        ("PUT", ["computed"]) => Some(set_config_kind(server, board, principal, body, "computed", false)),
        ("DELETE", ["computed"]) => Some(set_config_kind(server, board, principal, body, "computed", true)),
        ("PUT", ["validate"]) => Some(set_config_kind(server, board, principal, body, "validate", false)),
        ("DELETE", ["validate"]) => Some(set_config_kind(server, board, principal, body, "validate", true)),
        ("PUT", ["redact"]) => Some(set_config_kind(server, board, principal, body, "redact", false)),
        ("DELETE", ["redact"]) => Some(set_config_kind(server, board, principal, body, "redact", true)),
        ("PUT", ["webhook_secret"]) => Some(webhook_secret_set(server, board, principal, body)),
        ("DELETE", ["webhook_secret"]) => Some(webhook_secret_clear(server, board, principal)),
        ("GET", ["config"]) => Some(config(server, board, principal)),
        _ => None,
    }
}

fn parse_body(body: &Bytes) -> Result<Json, Response<BoxBodyResp>> {
    serde_json::from_slice(body).map_err(|_| err_json(StatusCode::BAD_REQUEST, "invalid json"))
}

fn issue_key(server: &Server, board: &str, principal: &Principal, body: &Bytes) -> Response<BoxBodyResp> {
    if let Some(resp) = require_admin_ok(server, principal) {
        return resp;
    }
    let req = match parse_body(body) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let role = req.get("role").and_then(|r| r.as_str()).unwrap_or("writer").to_string();
    let writer = req.get("writer").and_then(|w| w.as_str()).map(String::from);
    let scope = req.get("scope").and_then(|s| s.as_str()).map(String::from);
    match server.engine.lock().unwrap().issue_key(board, &role, writer.as_deref(), scope.as_deref()) {
        Ok((kr, secret)) => json_response(
            StatusCode::CREATED,
            json!({ "ok": true, "bucket": kr.bucket, "key": secret, "role": kr.role, "writer": kr.writer, "scope": kr.scope }),
        ),
        Err(e) => err_json(StatusCode::BAD_REQUEST, &e.to_string()),
    }
}

fn list_keys(server: &Server, board: &str, principal: &Principal) -> Response<BoxBodyResp> {
    if let Some(resp) = require_admin_ok(server, principal) {
        return resp;
    }
    // Never reveal key_hash/salt over REST — only identity + role + state.
    match server.engine.lock().unwrap().list_keys(board) {
        Ok(keys) => {
            let safe: Vec<Json> = keys
                .into_iter()
                .map(|k| {
                    json!({
                        "name": k.bucket,
                        "role": k.role,
                        "scope": k.scope,
                        "writer": k.writer,
                        "revoked": k.revoked_at.is_some(),
                        "revoked_at": k.revoked_at,
                    })
                })
                .collect();
            ok_json(json!({ "ok": true, "keys": safe }))
        }
        Err(e) => err_json(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

fn revoke_key(server: &Server, _board: &str, principal: &Principal, params: &HashMap<String, String>) -> Response<BoxBodyResp> {
    if let Some(resp) = require_admin_ok(server, principal) {
        return resp;
    }
    let bucket = params.get("bucket").map(|b| b.to_string()).filter(|b| !b.is_empty());
    let Some(bucket) = bucket else {
        return err_json(StatusCode::BAD_REQUEST, "missing bucket query param");
    };
    match server.engine.lock().unwrap().revoke_key(&bucket) {
        Ok(()) => ok_json(json!({ "ok": true })),
        Err(e) => err_json(StatusCode::BAD_REQUEST, &e.to_string()),
    }
}

fn list_hooks(server: &Server, board: &str, principal: &Principal) -> Response<BoxBodyResp> {
    if let Some(resp) = require_admin_ok(server, principal) {
        return resp;
    }
    match server.engine.lock().unwrap().list_hooks(board) {
        Ok(hooks) => ok_json(json!({ "ok": true, "hooks": hooks })),
        Err(e) => err_json(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

fn register_hook(server: &Server, board: &str, principal: &Principal, body: &Bytes) -> Response<BoxBodyResp> {
    if let Some(resp) = require_admin_ok(server, principal) {
        return resp;
    }
    let req = match parse_body(body) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let url = match req.get("url").and_then(|u| u.as_str()) {
        Some(u) if u.starts_with("http://") || u.starts_with("https://") => u.to_string(),
        _ => return err_json(StatusCode::BAD_REQUEST, "url must start with http:// or https://"),
    };
    let secret = req.get("secret").and_then(|s| s.as_str()).filter(|s| !s.is_empty()).map(String::from);
    match server.engine.lock().unwrap().register_hook(board, &url, secret.as_deref()) {
        Ok(()) => json_response(StatusCode::CREATED, json!({ "ok": true })),
        Err(e) => err_json(StatusCode::BAD_REQUEST, &e.to_string()),
    }
}

fn remove_hook(server: &Server, board: &str, principal: &Principal, params: &HashMap<String, String>) -> Response<BoxBodyResp> {
    if let Some(resp) = require_admin_ok(server, principal) {
        return resp;
    }
    let url = params.get("url").map(|u| u.to_string()).filter(|u| !u.is_empty());
    let Some(url) = url else {
        return err_json(StatusCode::BAD_REQUEST, "missing url query param");
    };
    match server.engine.lock().unwrap().remove_hook(board, &url) {
        Ok(()) => ok_json(json!({ "ok": true })),
        Err(e) => err_json(StatusCode::BAD_REQUEST, &e.to_string()),
    }
}

fn hook_deliveries(server: &Server, _board: &str, principal: &Principal, _params: &HashMap<String, String>) -> Response<BoxBodyResp> {
    if let Some(resp) = require_admin_ok(server, principal) {
        return resp;
    }
    ok_json(json!({ "ok": true, "deliveries": [] }))
}

fn list_jobs(server: &Server, board: &str, principal: &Principal) -> Response<BoxBodyResp> {
    if let Some(resp) = require_admin_ok(server, principal) {
        return resp;
    }
    match server.engine.lock().unwrap().list_jobs(board) {
        Ok(jobs) => ok_json(json!({ "ok": true, "jobs": jobs })),
        Err(e) => err_json(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

fn add_job(server: &Server, board: &str, principal: &Principal, body: &Bytes) -> Response<BoxBodyResp> {
    if let Some(resp) = require_admin_ok(server, principal) {
        return resp;
    }
    let req = match parse_body(body) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let name = match req.get("name").and_then(|n| n.as_str()) {
        Some(n) if !n.is_empty() => n.to_string(),
        _ => return err_json(StatusCode::BAD_REQUEST, "missing name"),
    };
    let schedule = match req.get("schedule").and_then(|s| s.as_str()) {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => return err_json(StatusCode::BAD_REQUEST, "missing schedule"),
    };
    let action = req.get("action").cloned().unwrap_or(Json::Null);
    match server.engine.lock().unwrap().add_job(board, &name, &schedule, &action) {
        Ok(key) => json_response(StatusCode::CREATED, json!({ "ok": true, "name": name, "schedule": schedule, "key": key })),
        Err(e) => err_json(StatusCode::BAD_REQUEST, &e.to_string()),
    }
}

fn remove_job(server: &Server, board: &str, principal: &Principal, params: &HashMap<String, String>) -> Response<BoxBodyResp> {
    if let Some(resp) = require_admin_ok(server, principal) {
        return resp;
    }
    let name = params.get("name").map(|n| n.to_string()).filter(|n| !n.is_empty());
    let Some(name) = name else {
        return err_json(StatusCode::BAD_REQUEST, "missing name query param");
    };
    match server.engine.lock().unwrap().remove_job(board, &name) {
        Ok(removed) => ok_json(json!({ "ok": true, "removed": removed })),
        Err(e) => err_json(StatusCode::BAD_REQUEST, &e.to_string()),
    }
}

fn job_runs(server: &Server, board: &str, principal: &Principal, params: &HashMap<String, String>) -> Response<BoxBodyResp> {
    if let Some(resp) = require_admin_ok(server, principal) {
        return resp;
    }
    let job = params.get("job").map(|j| j.to_string()).filter(|j| !j.is_empty());
    let limit: usize = params
        .get("limit")
        .and_then(|l| l.parse().ok())
        .unwrap_or(20)
        .clamp(1, 500);
    match server.engine.lock().unwrap().job_runs(board, job.as_deref(), limit) {
        Ok(runs) => ok_json(json!({ "ok": true, "runs": runs })),
        Err(e) => err_json(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

fn list_recipes(server: &Server, board: &str, principal: &Principal) -> Response<BoxBodyResp> {
    if let Some(resp) = require_admin_ok(server, principal) {
        return resp;
    }
    match server.engine.lock().unwrap().list_recipes(board) {
        Ok(recipes) => ok_json(json!({ "ok": true, "recipes": recipes })),
        Err(e) => err_json(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

fn add_recipe(server: &Server, board: &str, principal: &Principal, body: &Bytes) -> Response<BoxBodyResp> {
    if let Some(resp) = require_admin_ok(server, principal) {
        return resp;
    }
    let req = match parse_body(body) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let name = match req.get("name").and_then(|n| n.as_str()) {
        Some(n) if !n.is_empty() => n.to_string(),
        _ => return err_json(StatusCode::BAD_REQUEST, "missing name"),
    };
    let when = match req.get("when").and_then(|w| w.as_str()) {
        Some(w) if !w.is_empty() => w.to_string(),
        _ => return err_json(StatusCode::BAD_REQUEST, "missing when"),
    };
    let match_json = req.get("match").cloned();
    let dedup_on = req.get("dedup_on").and_then(|d| d.as_str()).map(String::from);
    let enabled = req.get("enabled").and_then(|e| e.as_bool()).unwrap_or(true);
    let actions = match req.get("actions") {
        Some(a) if a.is_array() => a.clone(),
        _ => return err_json(StatusCode::BAD_REQUEST, "actions must be a json array"),
    };
    let recipe = Recipe {
        name,
        when_json: json!(when),
        match_json,
        enabled,
        dedup_on,
        actions_json: Some(actions),
        table: req.get("table").and_then(|t| t.as_str()).map(String::from),
    };
    match server.engine.lock().unwrap().add_recipe(board, &recipe) {
        Ok(()) => json_response(StatusCode::CREATED, json!({ "ok": true, "recipe_id": recipe.name })),
        Err(e) => err_json(StatusCode::BAD_REQUEST, &e.to_string()),
    }
}

fn set_recipe_enabled(server: &Server, board: &str, principal: &Principal, name: &str, body: &Bytes) -> Response<BoxBodyResp> {
    if let Some(resp) = require_admin_ok(server, principal) {
        return resp;
    }
    let req = match parse_body(body) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let enabled = match req.get("enabled").and_then(|e| e.as_bool()) {
        Some(e) => e,
        None => return err_json(StatusCode::BAD_REQUEST, "expected boolean enabled field"),
    };
    match server.engine.lock().unwrap().set_recipe_enabled(board, name, enabled) {
        Ok(()) => ok_json(json!({ "ok": true, "enabled": enabled })),
        Err(e) => err_json(StatusCode::BAD_REQUEST, &e.to_string()),
    }
}

fn remove_recipe(server: &Server, board: &str, principal: &Principal, name: &str) -> Response<BoxBodyResp> {
    if let Some(resp) = require_admin_ok(server, principal) {
        return resp;
    }
    match server.engine.lock().unwrap().remove_recipe(board, name) {
        Ok(()) => ok_json(json!({ "ok": true })),
        Err(e) => err_json(StatusCode::BAD_REQUEST, &e.to_string()),
    }
}

fn list_secrets(server: &Server, board: &str, principal: &Principal) -> Response<BoxBodyResp> {
    if let Some(resp) = require_admin_ok(server, principal) {
        return resp;
    }
    // Never reveal the encrypted blob or plaintext over REST — only metadata.
    match server.engine.lock().unwrap().list_secrets(board) {
        Ok(secrets) => {
            let safe: Vec<Json> = secrets
                .into_iter()
                .map(|s| json!({ "name": s.name, "fingerprint": s.fingerprint }))
                .collect();
            ok_json(json!({ "ok": true, "secrets": safe }))
        }
        Err(e) => err_json(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

fn set_secret(server: &Server, board: &str, principal: &Principal, body: &Bytes) -> Response<BoxBodyResp> {
    if let Some(resp) = require_admin_ok(server, principal) {
        return resp;
    }
    let req = match parse_body(body) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let name = match req.get("name").and_then(|n| n.as_str()) {
        Some(n) if !n.is_empty() => n.to_string(),
        _ => return err_json(StatusCode::BAD_REQUEST, "missing name"),
    };
    let value = match req.get("value").and_then(|v| v.as_str()) {
        Some(v) if !v.is_empty() => v.to_string(),
        _ => return err_json(StatusCode::BAD_REQUEST, "missing value"),
    };
    match server.engine.lock().unwrap().set_secret(board, &name, &value) {
        Ok(()) => json_response(StatusCode::CREATED, json!({ "ok": true, "set": name.to_uppercase() })),
        Err(e) => err_json(StatusCode::BAD_REQUEST, &e.to_string()),
    }
}

fn remove_secret(server: &Server, board: &str, principal: &Principal, name: &str) -> Response<BoxBodyResp> {
    if let Some(resp) = require_admin_ok(server, principal) {
        return resp;
    }
    match server.engine.lock().unwrap().remove_secret(board, name) {
        Ok(()) => ok_json(json!({ "ok": true })),
        Err(e) => err_json(StatusCode::BAD_REQUEST, &e.to_string()),
    }
}

fn rate_config(server: &Server, board: &str, principal: &Principal, body: &Bytes) -> Response<BoxBodyResp> {
    if let Some(resp) = require_admin_ok(server, principal) {
        return resp;
    }
    let req = match parse_body(body) {
        Ok(v) => v,
        Err(e) => return e,
    };
    if req.get("clear").and_then(|v| v.as_bool()).unwrap_or(false) {
        match server.engine.lock().unwrap().set_rate(board, &Json::Null) {
            Ok(()) => ok_json(json!({ "ok": true })),
            Err(e) => err_json(StatusCode::BAD_REQUEST, &e.to_string()),
        }
    } else {
        match server.engine.lock().unwrap().set_rate(board, &req) {
            Ok(()) => ok_json(json!({ "ok": true })),
            Err(e) => err_json(StatusCode::BAD_REQUEST, &e.to_string()),
        }
    }
}

fn ttl_set(server: &Server, board: &str, principal: &Principal, body: &Bytes) -> Response<BoxBodyResp> {
    if let Some(resp) = require_admin_ok(server, principal) {
        return resp;
    }
    let req = match parse_body(body) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let table = req.get("table").and_then(|t| t.as_str()).unwrap_or("records").to_string();
    let seconds = req.get("seconds").and_then(|s| s.as_i64());
    let field = req.get("field").and_then(|f| f.as_str()).filter(|f| !f.is_empty());
    match server.engine.lock().unwrap().set_ttl(board, &table, seconds, field) {
        Ok(()) => ok_json(json!({ "ok": true, "table": table, "seconds": seconds, "field": field })),
        Err(e) => err_json(StatusCode::BAD_REQUEST, &e.to_string()),
    }
}

fn ttl_clear(server: &Server, board: &str, principal: &Principal, params: &HashMap<String, String>) -> Response<BoxBodyResp> {
    if let Some(resp) = require_admin_ok(server, principal) {
        return resp;
    }
    let table = params.get("table").map(|t| t.as_str()).unwrap_or("records").to_string();
    match server.engine.lock().unwrap().clear_ttl(board, &table) {
        Ok(()) => ok_json(json!({ "ok": true })),
        Err(e) => err_json(StatusCode::BAD_REQUEST, &e.to_string()),
    }
}

fn link_set(server: &Server, board: &str, principal: &Principal, body: &Bytes) -> Response<BoxBodyResp> {
    if let Some(resp) = require_admin_ok(server, principal) {
        return resp;
    }
    let req = match parse_body(body) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let (Some(on), Some(to), Some(via)) = (
        req.get("on").and_then(|v| v.as_str()),
        req.get("to").and_then(|v| v.as_str()),
        req.get("via").and_then(|v| v.as_str()),
    ) else {
        return err_json(StatusCode::BAD_REQUEST, "expected on, to and via fields");
    };
    let child_table = req.get("table").and_then(|t| t.as_str()).unwrap_or("records");
    let parent_table = req.get("parent_table").and_then(|t| t.as_str()).unwrap_or("records");
    match server.engine.lock().unwrap().set_link(board, child_table, to, parent_table, on, via) {
        Ok(()) => ok_json(json!({ "ok": true, "on": on, "to": to, "via": via })),
        Err(e) => err_json(StatusCode::BAD_REQUEST, &e.to_string()),
    }
}

fn link_clear(server: &Server, board: &str, principal: &Principal) -> Response<BoxBodyResp> {
    if let Some(resp) = require_admin_ok(server, principal) {
        return resp;
    }
    match server.engine.lock().unwrap().clear_link(board) {
        Ok(()) => ok_json(json!({ "ok": true })),
        Err(e) => err_json(StatusCode::BAD_REQUEST, &e.to_string()),
    }
}

fn audit_set(server: &Server, board: &str, principal: &Principal, body: &Bytes) -> Response<BoxBodyResp> {
    if let Some(resp) = require_admin_ok(server, principal) {
        return resp;
    }
    let req = match parse_body(body) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let enabled = match req.get("enabled").and_then(|e| e.as_bool()) {
        Some(e) => e,
        None => return err_json(StatusCode::BAD_REQUEST, "expected boolean enabled field"),
    };
    match server.engine.lock().unwrap().set_audit(board, enabled) {
        Ok(()) => ok_json(json!({ "ok": true, "enabled": enabled })),
        Err(e) => err_json(StatusCode::BAD_REQUEST, &e.to_string()),
    }
}

fn audit_list(server: &Server, board: &str, principal: &Principal, params: &HashMap<String, String>) -> Response<BoxBodyResp> {
    if let Some(resp) = require_admin_ok(server, principal) {
        return resp;
    }
    let since = params.get("since").map(|s| s.to_string()).filter(|s| !s.is_empty());
    let limit: usize = params
        .get("limit")
        .and_then(|l| l.parse().ok())
        .unwrap_or(100)
        .clamp(1, 1000);
    match server.engine.lock().unwrap().audit_list(board, since.as_deref(), limit) {
        Ok(rows) => ok_json(json!({ "ok": true, "audit": rows })),
        Err(e) => err_json(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

fn set_config_kind(
    server: &Server,
    board: &str,
    principal: &Principal,
    body: &Bytes,
    kind: &str,
    clear: bool,
) -> Response<BoxBodyResp> {
    if let Some(resp) = require_admin_ok(server, principal) {
        return resp;
    }
    let value = if clear {
        Json::Null
    } else {
        match parse_body(body) {
            Ok(v) => v,
            Err(e) => return e,
        }
    };
    let mut engine = server.engine.lock().unwrap();
    let res = match kind {
        "computed" => engine.set_computed(board, "records", &value),
        "validate" => engine.set_validate(board, "records", &value),
        "redact" => engine.set_redact(board, "records", &value),
        _ => unreachable!(),
    };
    match res {
        Ok(()) => ok_json(json!({ "ok": true, "kind": kind })),
        Err(e) => err_json(StatusCode::BAD_REQUEST, &e.to_string()),
    }
}

fn webhook_secret_set(server: &Server, board: &str, principal: &Principal, body: &Bytes) -> Response<BoxBodyResp> {
    if let Some(resp) = require_admin_ok(server, principal) {
        return resp;
    }
    let req = match parse_body(body) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let secret = req.get("secret").and_then(|s| s.as_str()).filter(|s| s.len() >= 16).map(String::from);
    if req.get("secret").is_some() && secret.is_none() {
        return err_json(StatusCode::BAD_REQUEST, "secret must be at least 16 chars");
    }
    match server.engine.lock().unwrap().set_webhook_secret(board, secret.as_deref()) {
        Ok(()) => ok_json(json!({ "ok": true, "set": secret.is_some() })),
        Err(e) => err_json(StatusCode::BAD_REQUEST, &e.to_string()),
    }
}

fn webhook_secret_clear(server: &Server, board: &str, principal: &Principal) -> Response<BoxBodyResp> {
    if let Some(resp) = require_admin_ok(server, principal) {
        return resp;
    }
    match server.engine.lock().unwrap().set_webhook_secret(board, None) {
        Ok(()) => ok_json(json!({ "ok": true, "set": false })),
        Err(e) => err_json(StatusCode::BAD_REQUEST, &e.to_string()),
    }
}

fn config(server: &Server, board: &str, principal: &Principal) -> Response<BoxBodyResp> {
    if let Some(resp) = require_admin_ok(server, principal) {
        return resp;
    }
    let engine = server.engine.lock().unwrap();
    let app = engine.get_app(board).ok().flatten();
    let Some(app) = app else {
        return err_json(StatusCode::NOT_FOUND, "not found");
    };
    let rate = app.rate_json.clone().unwrap_or(Json::Null);
    let hooks = engine.list_hooks(board).unwrap_or_default();
    let keys: Vec<Json> = engine
        .list_keys(board)
        .unwrap_or_default()
        .into_iter()
        .map(|k| {
            json!({
                "name": k.bucket,
                "role": k.role,
                "scope": k.scope,
                "writer": k.writer,
                "revoked": k.revoked_at.is_some(),
                "revoked_at": k.revoked_at,
            })
        })
        .collect();
    let recipes = engine.list_recipes(board).unwrap_or_default();
    let secrets: Vec<Json> = engine
        .list_secrets(board)
        .unwrap_or_default()
        .into_iter()
        .map(|s| json!({ "name": s.name, "fingerprint": s.fingerprint }))
        .collect();
    ok_json(json!({
        "ok": true,
        "app": app,
        "rate": rate,
        "ttl": { "seconds": null, "field": null },
        "audit": app.audit,
        "link": null,
        "hooks": hooks,
        "keys": keys,
        "computed": app.computed_json,
        "validate": app.validate_json,
        "redact": app.redact_json,
        "recipes": recipes,
        "secrets": secrets,
        "webhook_secret_set": app.webhook_secret.is_some(),
    }))
}