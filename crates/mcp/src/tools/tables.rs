use super::{arg_str, ok};
use engine::model::Principal;
use engine::ServerlessEngine;
use serde_json::Value as Json;

pub async fn create(
    engine: &mut ServerlessEngine,
    _principal: &Principal,
    arguments: &Json,
) -> Result<Json, String> {
    let table = arg_str(arguments, "table")?;
    let schema = arguments.get("schema").cloned();
    let unique_key = arguments.get("unique_key").and_then(|u| u.as_str());
    let cfg = engine.create_table(&table, schema, unique_key).await
        .map_err(|e| e.to_string())?;
    ok(serde_json::to_value(&cfg).map_err(|e| e.to_string())?)
}

pub async fn list(
    engine: &ServerlessEngine,
    _principal: &Principal,
    _arguments: &Json,
) -> Result<Json, String> {
    let tables = engine.list_tables().await.map_err(|e| e.to_string())?;
    let out: Vec<Json> = tables
        .into_iter()
        .map(|t| serde_json::to_value(&t).unwrap_or(Json::Null))
        .collect();
    ok(serde_json::json!({ "tables": out }))
}

pub async fn show(
    engine: &ServerlessEngine,
    _principal: &Principal,
    arguments: &Json,
) -> Result<Json, String> {
    let table = arg_str(arguments, "table")?;
    match engine.get_table(&table).await.map_err(|e| e.to_string())? {
        Some(cfg) => ok(serde_json::to_value(&cfg).map_err(|e| e.to_string())?),
        None => Err(format!("table '{table}' not found")),
    }
}

pub async fn delete(
    engine: &mut ServerlessEngine,
    _principal: &Principal,
    arguments: &Json,
) -> Result<Json, String> {
    let table = arg_str(arguments, "table")?;
    let deleted = engine.drop_table(&table).await.map_err(|e| e.to_string())?;
    ok(serde_json::json!({ "table": table, "deleted": deleted }))
}

/// Per-table behavior knobs (schema-adjacent config that lives beside the
/// table, not in it). `--show` reads them; any set-flag writes it; an
/// explicit `null` clears that knob; `--clear-ttl` clears the whole TTL.
pub async fn config(
    engine: &mut ServerlessEngine,
    _principal: &Principal,
    arguments: &Json,
) -> Result<Json, String> {
    let table = arg_str(arguments, "table")?;
    if arguments.get("show").and_then(|v| v.as_bool()).unwrap_or(false) {
        let cfg = engine.get_table(&table).await
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("table '{table}' not found"))?;
        return ok(serde_json::json!({
            "table": cfg.table,
            "schema": cfg.schema_json,
            "unique_key": cfg.unique_key,
            "computed": cfg.computed_json,
            "validate": cfg.validate_json,
            "redact": cfg.redact_json,
            "ttl_seconds": cfg.ttl_seconds,
            "ttl_field": cfg.ttl_field,
        }));
    }
    if let Some(v) = arguments.get("computed") {
        engine.set_computed(&table, v).await.map_err(|e| e.to_string())?;
    }
    if let Some(v) = arguments.get("validate") {
        engine.set_validate(&table, v).await.map_err(|e| e.to_string())?;
    }
    if let Some(v) = arguments.get("redact") {
        engine.set_redact(&table, v).await.map_err(|e| e.to_string())?;
    }
    if arguments.get("clear_ttl").and_then(|v| v.as_bool()).unwrap_or(false) {
        engine.clear_ttl(&table).await.map_err(|e| e.to_string())?;
    } else if arguments.get("ttl_seconds").is_some() || arguments.get("ttl_field").is_some() {
        let secs = arguments.get("ttl_seconds").and_then(|v| v.as_i64());
        let field = arguments.get("ttl_field").and_then(|v| v.as_str());
        engine.set_ttl(&table, secs, field).await.map_err(|e| e.to_string())?;
    }
    ok(serde_json::json!({ "table": table, "updated": true }))
}
