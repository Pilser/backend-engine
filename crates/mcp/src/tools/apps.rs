use super::{arg_str, ok};
use engine::model::Principal;
use engine::ServerlessEngine;
use serde_json::{json, Value as Json};

pub fn create(
    engine: &mut ServerlessEngine,
    principal: &Principal,
    arguments: &Json,
) -> Result<Json, String> {
    let title = arg_str(arguments, "title")?;
    let schema = arguments.get("schema").cloned();
    let unique_key = arguments.get("unique_key").and_then(|u| u.as_str());
    let public_reads = arguments.get("public_reads").and_then(|v| v.as_bool()).unwrap_or(false);
    let board = engine
        .create_app(&principal.id, title, schema, public_reads, unique_key)
        .map_err(|e| e.to_string())?;
    ok(json!({ "board": board.board_id, "title": board.title }))
}

pub fn list(
    engine: &ServerlessEngine,
    principal: &Principal,
    _arguments: &Json,
) -> Result<Json, String> {
    let apps = engine.list_apps(&principal.id).map_err(|e| e.to_string())?;
    let out: Vec<Json> = apps
        .into_iter()
        .map(|b| json!({ "board": b.board_id, "title": b.title }))
        .collect();
    ok(json!({ "apps": out }))
}

pub fn show(
    engine: &ServerlessEngine,
    principal: &Principal,
    arguments: &Json,
) -> Result<Json, String> {
    let board_id = arg_str(arguments, "board")?;
    let board = engine
        .get_app(board_id)
        .map_err(|e| e.to_string())?
        .filter(|b| b.owner_key == principal.id)
        .ok_or_else(|| "board not found or not owned".to_string())?;
    ok(serde_json::to_value(&board).map_err(|e| e.to_string())?)
}

pub fn delete(
    engine: &mut ServerlessEngine,
    principal: &Principal,
    arguments: &Json,
) -> Result<Json, String> {
    let board_id = arg_str(arguments, "board")?;
    engine
        .delete_app(board_id, &principal.id)
        .map_err(|e| e.to_string())?;
    ok(json!({ "board": board_id, "deleted": true }))
}

pub fn resources(
    engine: &mut ServerlessEngine,
    principal: &Principal,
    arguments: &Json,
) -> Result<Json, String> {
    let board_id = arg_str(arguments, "board")?;
    let board = engine
        .get_app(board_id)
        .map_err(|e| e.to_string())?
        .filter(|b| b.owner_key == principal.id)
        .ok_or_else(|| "board not found or not owned".to_string())?;
    let mut records: i64 = 0;
    if let Ok(tables) = engine.list_tables(board_id) {
        for t in tables {
            records += engine.count_records(board_id, &t.table).map_err(|e| e.to_string())?;
        }
    }
    let mut storage: u64 = 0;
    if let Ok(keys) = engine.object_store().list(&format!("{board_id}/files/")) {
        storage += keys.iter().map(|k| k.size).sum::<u64>();
    }
    if let Ok(keys) = engine.object_store().list(&format!("{board_id}/assets/")) {
        storage += keys.iter().map(|k| k.size).sum::<u64>();
    }
    let limits = engine::policy::RateLimits::from_json(board.rate_json.as_ref().unwrap_or(&Json::Null));
    ok(json!({
        "board": board_id,
        "records": { "count": records },
        "storage": { "bytes": storage },
        "rate": {
            "limits": {
                "submit": limits.submit,
                "upload": limits.upload,
                "search": limits.search,
                "read": limits.read,
                "per_day": limits.per_day,
            }
        }
    }))
}

pub fn update(
    engine: &mut ServerlessEngine,
    principal: &Principal,
    arguments: &Json,
) -> Result<Json, String> {
    let board_id = arg_str(arguments, "board")?;
    let owned = engine
        .get_app(board_id)
        .map_err(|e| e.to_string())?
        .filter(|b| b.owner_key == principal.id)
        .is_some();
    if !owned {
        return Err("board not found or not owned".to_string());
    }
    if let Some(schema) = arguments.get("schema") {
        if schema.is_string() {
            let parsed: Json = serde_json::from_str(schema.as_str().unwrap())
                .map_err(|e| format!("schema is not valid JSON: {e}"))?;
            engine
                .update_app(board_id, &principal.id, &serde_json::json!({ "schema": parsed }))
                .map_err(|e| e.to_string())?;
        } else {
            engine
                .update_app(board_id, &principal.id, &serde_json::json!({ "schema": schema }))
                .map_err(|e| e.to_string())?;
        }
    }
    if let Some(v) = arguments.get("unique_key") {
        let val = if v.is_null() { serde_json::Value::Null } else { serde_json::json!(v.as_str()) };
        engine
            .update_app(board_id, &principal.id, &serde_json::json!({ "unique_key": val }))
            .map_err(|e| e.to_string())?;
    }
    if let Some(v) = arguments.get("computed") {
        engine.set_computed(board_id, "records", v).map_err(|e| e.to_string())?;
    }
    if let Some(v) = arguments.get("validate") {
        engine.set_validate(board_id, "records", v).map_err(|e| e.to_string())?;
    }
    if let Some(v) = arguments.get("redact") {
        engine.set_redact(board_id, "records", v).map_err(|e| e.to_string())?;
    }
    if let Some(v) = arguments.get("ttl_seconds") {
        let secs = v.as_i64();
        let field = arguments.get("ttl_field").and_then(|f| f.as_str()).map(String::from);
        engine.set_ttl(board_id, "records", secs, field.as_deref()).map_err(|e| e.to_string())?;
    }
    if let Some(v) = arguments.get("audit") {
        engine.set_audit(board_id, v.as_bool().unwrap_or(false)).map_err(|e| e.to_string())?;
    }
    if let Some(v) = arguments.get("public_reads") {
        engine
            .update_app(board_id, &principal.id, &serde_json::json!({ "public_reads": v.as_bool().unwrap_or(false) }))
            .map_err(|e| e.to_string())?;
    }
    if let Some(v) = arguments.get("rate") {
        engine.set_rate(board_id, v).map_err(|e| e.to_string())?;
    }
    ok(json!({ "board": board_id, "updated": true }))
}

pub fn _unused(_: i64) {}