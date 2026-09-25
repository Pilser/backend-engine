use super::{arg_str, ok};
use engine::model::Principal;
use engine::ServerlessEngine;
use serde_json::Value as Json;

pub fn create(
    engine: &mut ServerlessEngine,
    _principal: &Principal,
    arguments: &Json,
) -> Result<Json, String> {
    let board = arg_str(arguments, "board")?;
    let table = arg_str(arguments, "table")?;
    let schema = arguments.get("schema").cloned();
    let unique_key = arguments.get("unique_key").and_then(|u| u.as_str());
    let cfg = engine
        .create_table(&board, &table, schema, unique_key)
        .map_err(|e| e.to_string())?;
    ok(serde_json::to_value(&cfg).map_err(|e| e.to_string())?)
}

pub fn list(
    engine: &ServerlessEngine,
    _principal: &Principal,
    arguments: &Json,
) -> Result<Json, String> {
    let board = arg_str(arguments, "board")?;
    let tables = engine.list_tables(&board).map_err(|e| e.to_string())?;
    let out: Vec<Json> = tables
        .into_iter()
        .map(|t| serde_json::to_value(&t).unwrap_or(Json::Null))
        .collect();
    ok(serde_json::json!({ "tables": out }))
}

pub fn show(
    engine: &ServerlessEngine,
    _principal: &Principal,
    arguments: &Json,
) -> Result<Json, String> {
    let board = arg_str(arguments, "board")?;
    let table = arg_str(arguments, "table")?;
    match engine.get_table(&board, &table).map_err(|e| e.to_string())? {
        Some(cfg) => ok(serde_json::to_value(&cfg).map_err(|e| e.to_string())?),
        None => Err(format!("table '{table}' not found on board '{board}'")),
    }
}

pub fn delete(
    engine: &mut ServerlessEngine,
    _principal: &Principal,
    arguments: &Json,
) -> Result<Json, String> {
    let board = arg_str(arguments, "board")?;
    let table = arg_str(arguments, "table")?;
    let deleted = engine.drop_table(&board, &table).map_err(|e| e.to_string())?;
    ok(serde_json::json!({ "table": table, "deleted": deleted }))
}
