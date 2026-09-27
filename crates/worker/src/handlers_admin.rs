//! Admin routes (`/api/keys|hooks|jobs|recipes|email|secrets|rate|ttl|link|audit|
//! computed|validate|redact|webhook_secret|config`). Ports the donor
//! `rest_admin.rs` arms 1:1 minus `{board}` (single tenant). Every route is
//! admin-gated. Table-scoped knobs (`computed`/`validate`/`redact`/`ttl`)
//! accept `?table=` (default `records`, donor parity).

use serde_json::{json, Value as Json};
use worker::{Request, Response, Result, RouteContext};

use crate::{auth, cors, query};

use super::handlers_tables::body_json_for;

const MAX_JSON: usize = 1_000_000;

fn admin(app: &auth::Ctx) -> std::result::Result<(), Response> {
    if auth::require_admin(&app.principal) {
        Ok(())
    } else {
        Err(cors::deny("admin authorization required"))
    }
}

fn table_of(body: &Json, params: &std::collections::HashMap<String, String>) -> String {
    body.get("table")
        .and_then(|t| t.as_str())
        .or_else(|| params.get("table").map(|s| s.as_str()))
        .unwrap_or("records")
        .to_string()
}

// ---- keys ---------------------------------------------------------------

pub async fn issue_key(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let mut app = match auth::ctx_for(&req, &ctx).await {
        Ok(c) => c,
        Err(r) => return Ok(r),
    };
    if let Err(r) = admin(&app) {
        return Ok(r);
    }
    let body = match body_json_for(req, MAX_JSON).await {
        Ok(b) => b,
        Err(r) => return Ok(r),
    };
    let role = body.get("role").and_then(|r| r.as_str()).unwrap_or("writer").to_string();
    let writer = body.get("writer").and_then(|w| w.as_str()).map(String::from);
    let scope = body.get("scope").and_then(|s| s.as_str()).map(String::from);
    let tables = body.get("tables").and_then(|v| v.as_array()).map(|a| {
        a.iter().filter_map(|x| x.as_str().map(String::from)).collect::<Vec<_>>()
    });
    Ok(match app.engine.issue_key(&role, writer.as_deref(), scope.as_deref(), tables).await {
        Ok((kr, secret)) => cors::created(
            json!({ "ok": true, "bucket": kr.bucket, "key": secret, "role": kr.role, "writer": kr.writer, "scope": kr.scope, "tables": kr.tables }),
        ),
        Err(e) => cors::bad(&e),
    })
}

pub async fn list_keys(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let app = match auth::ctx_for(&req, &ctx).await {
        Ok(c) => c,
        Err(r) => return Ok(r),
    };
    if let Err(r) = admin(&app) {
        return Ok(r);
    }
    Ok(match app.engine.list_keys().await {
        Ok(keys) => {
            let safe: Vec<Json> = keys
                .into_iter()
                .map(|k| {
                    json!({
                        "name": k.bucket,
                        "role": k.role,
                        "scope": k.scope,
                        "tables": k.tables,
                        "writer": k.writer,
                        "revoked": k.revoked_at.is_some(),
                        "revoked_at": k.revoked_at,
                    })
                })
                .collect();
            cors::ok(json!({ "ok": true, "keys": safe }))
        }
        Err(e) => cors::srv(&e),
    })
}

pub async fn revoke_key(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let p = query::params(&req);
    let mut app = match auth::ctx_for(&req, &ctx).await {
        Ok(c) => c,
        Err(r) => return Ok(r),
    };
    if let Err(r) = admin(&app) {
        return Ok(r);
    }
    let bucket = p.get("bucket").map(|b| b.to_string()).filter(|b| !b.is_empty());
    let Some(bucket) = bucket else {
        return Ok(cors::err(400, "missing bucket query param"));
    };
    Ok(match app.engine.revoke_key(&bucket).await {
        Ok(()) => cors::ok(json!({ "ok": true })),
        Err(e) => cors::bad(&e),
    })
}

// ---- hooks --------------------------------------------------------------

pub async fn list_hooks(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let app = match auth::ctx_for(&req, &ctx).await {
        Ok(c) => c,
        Err(r) => return Ok(r),
    };
    if let Err(r) = admin(&app) {
        return Ok(r);
    }
    Ok(match app.engine.list_hooks().await {
        Ok(hooks) => cors::ok(json!({ "ok": true, "hooks": hooks })),
        Err(e) => cors::srv(&e),
    })
}

pub async fn register_hook(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let mut app = match auth::ctx_for(&req, &ctx).await {
        Ok(c) => c,
        Err(r) => return Ok(r),
    };
    if let Err(r) = admin(&app) {
        return Ok(r);
    }
    let body = match body_json_for(req, MAX_JSON).await {
        Ok(b) => b,
        Err(r) => return Ok(r),
    };
    let url = match body.get("url").and_then(|u| u.as_str()) {
        Some(u) if u.starts_with("http://") || u.starts_with("https://") => u.to_string(),
        _ => return Ok(cors::err(400, "url must start with http:// or https://")),
    };
    let secret = body.get("secret").and_then(|s| s.as_str()).filter(|s| !s.is_empty()).map(String::from);
    Ok(match app.engine.register_hook(&url, secret.as_deref()).await {
        Ok(()) => cors::created(json!({ "ok": true })),
        Err(e) => cors::bad(&e),
    })
}

pub async fn remove_hook(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let p = query::params(&req);
    let mut app = match auth::ctx_for(&req, &ctx).await {
        Ok(c) => c,
        Err(r) => return Ok(r),
    };
    if let Err(r) = admin(&app) {
        return Ok(r);
    }
    let url = p.get("url").map(|u| u.to_string()).filter(|u| !u.is_empty());
    let Some(url) = url else {
        return Ok(cors::err(400, "missing url query param"));
    };
    Ok(match app.engine.remove_hook(&url).await {
        Ok(()) => cors::ok(json!({ "ok": true })),
        Err(e) => cors::bad(&e),
    })
}

pub async fn hook_deliveries(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let app = match auth::ctx_for(&req, &ctx).await {
        Ok(c) => c,
        Err(r) => return Ok(r),
    };
    if let Err(r) = admin(&app) {
        return Ok(r);
    }
    // Improvement over the donor stub (which always returned []): report the
    // currently-due delivery queue.
    Ok(match app.engine.hook_deliveries_due(&engine::crud::now_str(), 100).await {
        Ok(due) => cors::ok(json!({ "ok": true, "deliveries": due })),
        Err(e) => cors::srv(&e),
    })
}

// ---- jobs ---------------------------------------------------------------

pub async fn list_jobs(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let app = match auth::ctx_for(&req, &ctx).await {
        Ok(c) => c,
        Err(r) => return Ok(r),
    };
    if let Err(r) = admin(&app) {
        return Ok(r);
    }
    Ok(match app.engine.list_jobs().await {
        Ok(jobs) => cors::ok(json!({ "ok": true, "jobs": jobs })),
        Err(e) => cors::srv(&e),
    })
}

pub async fn add_job(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let mut app = match auth::ctx_for(&req, &ctx).await {
        Ok(c) => c,
        Err(r) => return Ok(r),
    };
    if let Err(r) = admin(&app) {
        return Ok(r);
    }
    let body = match body_json_for(req, MAX_JSON).await {
        Ok(b) => b,
        Err(r) => return Ok(r),
    };
    let name = match body.get("name").and_then(|n| n.as_str()) {
        Some(n) if !n.is_empty() => n.to_string(),
        _ => return Ok(cors::err(400, "missing name")),
    };
    let schedule = match body.get("schedule").and_then(|s| s.as_str()) {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => return Ok(cors::err(400, "missing schedule")),
    };
    let action = body.get("action").cloned().unwrap_or(Json::Null);
    Ok(match app.engine.add_job(&name, &schedule, &action).await {
        Ok(key) => cors::created(json!({ "ok": true, "name": name, "schedule": schedule, "key": key })),
        Err(e) => cors::bad(&e),
    })
}

pub async fn remove_job(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let p = query::params(&req);
    let mut app = match auth::ctx_for(&req, &ctx).await {
        Ok(c) => c,
        Err(r) => return Ok(r),
    };
    if let Err(r) = admin(&app) {
        return Ok(r);
    }
    let name = p.get("name").map(|n| n.to_string()).filter(|n| !n.is_empty());
    let Some(name) = name else {
        return Ok(cors::err(400, "missing name query param"));
    };
    Ok(match app.engine.remove_job(&name).await {
        Ok(removed) => cors::ok(json!({ "ok": true, "removed": removed })),
        Err(e) => cors::bad(&e),
    })
}

pub async fn job_runs(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let p = query::params(&req);
    let app = match auth::ctx_for(&req, &ctx).await {
        Ok(c) => c,
        Err(r) => return Ok(r),
    };
    if let Err(r) = admin(&app) {
        return Ok(r);
    }
    let job = p.get("job").map(|j| j.to_string()).filter(|j| !j.is_empty());
    let limit = query::limit(&p, "limit", 20, 500);
    Ok(match app.engine.job_runs(job.as_deref(), limit).await {
        Ok(runs) => cors::ok(json!({ "ok": true, "runs": runs })),
        Err(e) => cors::srv(&e),
    })
}

// ---- recipes ------------------------------------------------------------

pub async fn list_recipes(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let app = match auth::ctx_for(&req, &ctx).await {
        Ok(c) => c,
        Err(r) => return Ok(r),
    };
    if let Err(r) = admin(&app) {
        return Ok(r);
    }
    Ok(match app.engine.list_recipes().await {
        Ok(recipes) => cors::ok(json!({ "ok": true, "recipes": recipes })),
        Err(e) => cors::srv(&e),
    })
}

pub async fn add_recipe(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let mut app = match auth::ctx_for(&req, &ctx).await {
        Ok(c) => c,
        Err(r) => return Ok(r),
    };
    if let Err(r) = admin(&app) {
        return Ok(r);
    }
    let body = match body_json_for(req, MAX_JSON).await {
        Ok(b) => b,
        Err(r) => return Ok(r),
    };
    let name = match body.get("name").and_then(|n| n.as_str()) {
        Some(n) if !n.is_empty() => n.to_string(),
        _ => return Ok(cors::err(400, "missing name")),
    };
    // Accept the legacy string form and the object form ({event, table, …}).
    let when = match body.get("when") {
        Some(Json::String(w)) if !w.is_empty() => json!(w),
        Some(o @ Json::Object(_)) => o.clone(),
        _ => return Ok(cors::err(400, "missing when")),
    };
    let actions = match body.get("actions") {
        Some(a) if a.is_array() => a.clone(),
        _ => return Ok(cors::err(400, "actions must be a json array")),
    };
    let recipe = engine::model::Recipe {
        name: name.clone(),
        when_json: when,
        match_json: body.get("match").cloned(),
        enabled: body.get("enabled").and_then(|e| e.as_bool()).unwrap_or(true),
        dedup_on: body.get("dedup_on").and_then(|d| d.as_str()).map(String::from),
        actions_json: Some(actions),
        table: body.get("table").and_then(|t| t.as_str()).map(String::from),
    };
    Ok(match app.engine.add_recipe(&recipe).await {
        Ok(()) => cors::created(json!({ "ok": true, "recipe_id": name })),
        Err(e) => cors::bad(&e),
    })
}

pub async fn set_recipe_enabled(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let name = match ctx.param("name").filter(|n| !n.is_empty()).cloned() {
        Some(n) => n,
        None => return Ok(cors::err(400, "missing recipe name")),
    };
    let mut app = match auth::ctx_for(&req, &ctx).await {
        Ok(c) => c,
        Err(r) => return Ok(r),
    };
    if let Err(r) = admin(&app) {
        return Ok(r);
    }
    let body = match body_json_for(req, MAX_JSON).await {
        Ok(b) => b,
        Err(r) => return Ok(r),
    };
    let enabled = match body.get("enabled").and_then(|e| e.as_bool()) {
        Some(e) => e,
        None => return Ok(cors::err(400, "expected boolean enabled field")),
    };
    Ok(match app.engine.set_recipe_enabled(&name, enabled).await {
        Ok(()) => cors::ok(json!({ "ok": true, "enabled": enabled })),
        Err(e) => cors::bad(&e),
    })
}

pub async fn get_recipe(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let name = match ctx.param("name").filter(|n| !n.is_empty()).cloned() {
        Some(n) => n,
        None => return Ok(cors::err(400, "missing recipe name")),
    };
    let app = match auth::ctx_for(&req, &ctx).await {
        Ok(c) => c,
        Err(r) => return Ok(r),
    };
    if let Err(r) = admin(&app) {
        return Ok(r);
    }
    Ok(match app.engine.get_recipe(&name).await {
        Ok(Some(recipe)) => cors::ok(json!({ "ok": true, "recipe": recipe })),
        Ok(None) => cors::gone("recipe not found"),
        Err(e) => cors::srv(&e),
    })
}

pub async fn remove_recipe(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let name = match ctx.param("name").filter(|n| !n.is_empty()).cloned() {
        Some(n) => n,
        None => return Ok(cors::err(400, "missing recipe name")),
    };
    let mut app = match auth::ctx_for(&req, &ctx).await {
        Ok(c) => c,
        Err(r) => return Ok(r),
    };
    if let Err(r) = admin(&app) {
        return Ok(r);
    }
    Ok(match app.engine.remove_recipe(&name).await {
        Ok(()) => cors::ok(json!({ "ok": true })),
        Err(e) => cors::bad(&e),
    })
}

// ---- email --------------------------------------------------------------
// One-off send via the MAIL_* secrets (Phase A: plugins). Recipes use the
// `$send_email` action instead (same provider builders, deferred).

pub async fn email_send(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let mut app = match auth::ctx_for(&req, &ctx).await {
        Ok(c) => c,
        Err(r) => return Ok(r),
    };
    if let Err(r) = admin(&app) {
        return Ok(r);
    }
    let body = match body_json_for(req, MAX_JSON).await {
        Ok(b) => b,
        Err(r) => return Ok(r),
    };
    let to = body.get("to").and_then(|v| v.as_str()).unwrap_or("");
    let subject = body.get("subject").and_then(|v| v.as_str()).unwrap_or("");
    let text = body.get("text").and_then(|v| v.as_str());
    let html = body.get("html").and_then(|v| v.as_str());
    let from = body.get("from").and_then(|v| v.as_str());
    Ok(match app.engine.send_email(to, subject, text, html, from).await {
        Ok(out) => cors::ok(json!({ "ok": true, "provider": out["provider"], "status": out["status"] })),
        Err(e) => cors::bad(&e),
    })
}

// ---- secrets ------------------------------------------------------------

pub async fn list_secrets(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let app = match auth::ctx_for(&req, &ctx).await {
        Ok(c) => c,
        Err(r) => return Ok(r),
    };
    if let Err(r) = admin(&app) {
        return Ok(r);
    }
    Ok(match app.engine.list_secrets().await {
        Ok(secrets) => {
            let safe: Vec<Json> = secrets
                .into_iter()
                .map(|s| json!({ "name": s.name, "fingerprint": s.fingerprint }))
                .collect();
            cors::ok(json!({ "ok": true, "secrets": safe }))
        }
        Err(e) => cors::srv(&e),
    })
}

pub async fn set_secret(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let mut app = match auth::ctx_for(&req, &ctx).await {
        Ok(c) => c,
        Err(r) => return Ok(r),
    };
    if let Err(r) = admin(&app) {
        return Ok(r);
    }
    let body = match body_json_for(req, MAX_JSON).await {
        Ok(b) => b,
        Err(r) => return Ok(r),
    };
    let name = match body.get("name").and_then(|n| n.as_str()) {
        Some(n) if !n.is_empty() => n.to_string(),
        _ => return Ok(cors::err(400, "missing name")),
    };
    let value = match body.get("value").and_then(|v| v.as_str()) {
        Some(v) if !v.is_empty() => v.to_string(),
        _ => return Ok(cors::err(400, "missing value")),
    };
    Ok(match app.engine.set_secret(&name, &value).await {
        Ok(()) => cors::created(json!({ "ok": true, "set": name.to_uppercase() })),
        Err(e) => cors::bad(&e),
    })
}

pub async fn remove_secret(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let name = match ctx.param("name").filter(|n| !n.is_empty()).cloned() {
        Some(n) => n,
        None => return Ok(cors::err(400, "missing secret name")),
    };
    let mut app = match auth::ctx_for(&req, &ctx).await {
        Ok(c) => c,
        Err(r) => return Ok(r),
    };
    if let Err(r) = admin(&app) {
        return Ok(r);
    }
    Ok(match app.engine.remove_secret(&name).await {
        Ok(()) => cors::ok(json!({ "ok": true })),
        Err(e) => cors::bad(&e),
    })
}

// ---- config knobs -------------------------------------------------------

pub async fn rate_config(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let mut app = match auth::ctx_for(&req, &ctx).await {
        Ok(c) => c,
        Err(r) => return Ok(r),
    };
    if let Err(r) = admin(&app) {
        return Ok(r);
    }
    let body = match body_json_for(req, MAX_JSON).await {
        Ok(b) => b,
        Err(r) => return Ok(r),
    };
    let value = if body.get("clear").and_then(|v| v.as_bool()).unwrap_or(false) {
        Json::Null
    } else {
        body
    };
    Ok(match app.engine.set_rate(&value).await {
        Ok(()) => cors::ok(json!({ "ok": true })),
        Err(e) => cors::bad(&e),
    })
}

pub async fn ttl_set(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let p = query::params(&req);
    let mut app = match auth::ctx_for(&req, &ctx).await {
        Ok(c) => c,
        Err(r) => return Ok(r),
    };
    if let Err(r) = admin(&app) {
        return Ok(r);
    }
    let body = match body_json_for(req, MAX_JSON).await {
        Ok(b) => b,
        Err(r) => return Ok(r),
    };
    let table = table_of(&body, &p);
    let seconds = body.get("seconds").and_then(|s| s.as_i64());
    let field = body.get("field").and_then(|f| f.as_str()).filter(|f| !f.is_empty());
    Ok(match app.engine.set_ttl(&table, seconds, field).await {
        Ok(()) => cors::ok(json!({ "ok": true, "table": table, "seconds": seconds, "field": field })),
        Err(e) => cors::bad(&e),
    })
}

pub async fn ttl_clear(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let p = query::params(&req);
    let mut app = match auth::ctx_for(&req, &ctx).await {
        Ok(c) => c,
        Err(r) => return Ok(r),
    };
    if let Err(r) = admin(&app) {
        return Ok(r);
    }
    let table = p.get("table").map(|s| s.as_str()).unwrap_or("records").to_string();
    Ok(match app.engine.clear_ttl(&table).await {
        Ok(()) => cors::ok(json!({ "ok": true })),
        Err(e) => cors::bad(&e),
    })
}

pub async fn link_set(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let mut app = match auth::ctx_for(&req, &ctx).await {
        Ok(c) => c,
        Err(r) => return Ok(r),
    };
    if let Err(r) = admin(&app) {
        return Ok(r);
    }
    let body = match body_json_for(req, MAX_JSON).await {
        Ok(b) => b,
        Err(r) => return Ok(r),
    };
    let (Some(on), Some(to), Some(via)) = (
        body.get("on").and_then(|v| v.as_str()),
        body.get("to").and_then(|v| v.as_str()),
        body.get("via").and_then(|v| v.as_str()),
    ) else {
        return Ok(cors::err(400, "expected on, to and via fields"));
    };
    let child_table = body.get("table").and_then(|t| t.as_str()).unwrap_or("records");
    let parent_table = body.get("parent_table").and_then(|t| t.as_str()).unwrap_or("records");
    Ok(match app.engine.set_link(child_table, parent_table, on, via).await {
        Ok(()) => cors::ok(json!({ "ok": true, "on": on, "to": to, "via": via })),
        Err(e) => cors::bad(&e),
    })
}

pub async fn link_clear(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let mut app = match auth::ctx_for(&req, &ctx).await {
        Ok(c) => c,
        Err(r) => return Ok(r),
    };
    if let Err(r) = admin(&app) {
        return Ok(r);
    }
    Ok(match app.engine.clear_link().await {
        Ok(()) => cors::ok(json!({ "ok": true })),
        Err(e) => cors::bad(&e),
    })
}

pub async fn audit_set(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let mut app = match auth::ctx_for(&req, &ctx).await {
        Ok(c) => c,
        Err(r) => return Ok(r),
    };
    if let Err(r) = admin(&app) {
        return Ok(r);
    }
    let body = match body_json_for(req, MAX_JSON).await {
        Ok(b) => b,
        Err(r) => return Ok(r),
    };
    let enabled = match body.get("enabled").and_then(|e| e.as_bool()) {
        Some(e) => e,
        None => return Ok(cors::err(400, "expected boolean enabled field")),
    };
    Ok(match app.engine.set_audit(enabled).await {
        Ok(()) => cors::ok(json!({ "ok": true, "enabled": enabled })),
        Err(e) => cors::bad(&e),
    })
}

pub async fn audit_list(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let p = query::params(&req);
    let app = match auth::ctx_for(&req, &ctx).await {
        Ok(c) => c,
        Err(r) => return Ok(r),
    };
    if let Err(r) = admin(&app) {
        return Ok(r);
    }
    let since = p.get("since").map(|s| s.to_string()).filter(|s| !s.is_empty());
    let limit = query::limit(&p, "limit", 100, 1000);
    Ok(match app.engine.audit_list(since.as_deref(), limit).await {
        Ok(rows) => cors::ok(json!({ "ok": true, "audit": rows })),
        Err(e) => cors::srv(&e),
    })
}

pub async fn set_config_kind(
    req: Request,
    ctx: RouteContext<()>,
    kind: &str,
    clear: bool,
) -> Result<Response> {
    let p = query::params(&req);
    let mut app = match auth::ctx_for(&req, &ctx).await {
        Ok(c) => c,
        Err(r) => return Ok(r),
    };
    if let Err(r) = admin(&app) {
        return Ok(r);
    }
    let value = if clear {
        Json::Null
    } else {
        match body_json_for(req, MAX_JSON).await {
            Ok(b) => b,
            Err(r) => return Ok(r),
        }
    };
    let table = p.get("table").map(|s| s.as_str()).unwrap_or("records").to_string();
    let res = match kind {
        "computed" => app.engine.set_computed(&table, &value).await,
        "validate" => app.engine.set_validate(&table, &value).await,
        "redact" => app.engine.set_redact(&table, &value).await,
        _ => return Ok(cors::err(400, "unknown config kind")),
    };
    Ok(match res {
        Ok(()) => cors::ok(json!({ "ok": true, "kind": kind })),
        Err(e) => cors::bad(&e),
    })
}

pub async fn webhook_secret_set(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let mut app = match auth::ctx_for(&req, &ctx).await {
        Ok(c) => c,
        Err(r) => return Ok(r),
    };
    if let Err(r) = admin(&app) {
        return Ok(r);
    }
    let body = match body_json_for(req, MAX_JSON).await {
        Ok(b) => b,
        Err(r) => return Ok(r),
    };
    let secret = body.get("secret").and_then(|s| s.as_str()).filter(|s| s.len() >= 16).map(String::from);
    if body.get("secret").is_some() && secret.is_none() {
        return Ok(cors::err(400, "secret must be at least 16 chars"));
    }
    Ok(match app.engine.set_webhook_secret(secret.as_deref()).await {
        Ok(()) => cors::ok(json!({ "ok": true, "set": secret.is_some() })),
        Err(e) => cors::bad(&e),
    })
}

pub async fn webhook_secret_clear(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let mut app = match auth::ctx_for(&req, &ctx).await {
        Ok(c) => c,
        Err(r) => return Ok(r),
    };
    if let Err(r) = admin(&app) {
        return Ok(r);
    }
    Ok(match app.engine.set_webhook_secret(None).await {
        Ok(()) => cors::ok(json!({ "ok": true, "set": false })),
        Err(e) => cors::bad(&e),
    })
}

pub async fn config(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let mut app = match auth::ctx_for(&req, &ctx).await {
        Ok(c) => c,
        Err(r) => return Ok(r),
    };
    if let Err(r) = admin(&app) {
        return Ok(r);
    }
    let tenant = match app.engine.tenant().await {
        Ok(t) => t,
        Err(e) => return Ok(cors::srv(&e)),
    };
    let rate = tenant.rate_json.clone().unwrap_or(Json::Null);
    let hooks = app.engine.list_hooks().await.unwrap_or_default();
    let keys: Vec<Json> = app
        .engine
        .list_keys()
        .await
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
    let recipes = app.engine.list_recipes().await.unwrap_or_default();
    let secrets: Vec<Json> = app
        .engine
        .list_secrets()
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|s| json!({ "name": s.name, "fingerprint": s.fingerprint }))
        .collect();
    Ok(cors::ok(json!({
        "ok": true,
        "app": tenant,
        "rate": rate,
        "audit": tenant.audit,
        "link": app.engine.get_link().await.unwrap_or_default(),
        "hooks": hooks,
        "keys": keys,
        "recipes": recipes,
        "secrets": secrets,
        "webhook_secret_set": tenant.webhook_secret.is_some(),
    })))
}
