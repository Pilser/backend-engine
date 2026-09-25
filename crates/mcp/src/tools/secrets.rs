use super::{arg_str, ok};
use engine::model::Principal;
use engine::ServerlessEngine;
use serde_json::{json, Value as Json};

pub fn set(
    engine: &mut ServerlessEngine,
    _principal: &Principal,
    arguments: &Json,
) -> Result<Json, String> {
    let board = arg_str(arguments, "board")?;
    let name = arg_str(arguments, "name")?;
    let value = arg_str(arguments, "value")?;
    engine.set_secret(&board, &name, &value).map_err(|e| e.to_string())?;
    ok(json!({ "set": name.to_uppercase() }))
}

pub fn list(
    engine: &ServerlessEngine,
    _principal: &Principal,
    arguments: &Json,
) -> Result<Json, String> {
    let board = arg_str(arguments, "board")?;
    let secrets = engine.list_secrets(&board).map_err(|e| e.to_string())?;
    let out: Vec<Json> = secrets
        .into_iter()
        .map(|s| json!({ "name": s.name, "fingerprint": s.fingerprint }))
        .collect();
    ok(json!({ "secrets": out }))
}

pub fn show(
    engine: &ServerlessEngine,
    _principal: &Principal,
    arguments: &Json,
) -> Result<Json, String> {
    let board = arg_str(arguments, "board")?;
    let name = arg_str(arguments, "name")?;
    let secret = engine
        .get_secret(board, name)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("secret '{name}' not found on board {board}"))?;
    // MCP-only: reveal the decrypted value so the app folder / export can
    // capture the real content. The REST API never returns it.
    let value = engine
        .secret_value(board, name)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("secret '{name}' not found on board {board}"))?;
    ok(json!({
        "name": secret.name,
        "fingerprint": secret.fingerprint,
        "value": value,
    }))
}

pub fn delete(
    engine: &mut ServerlessEngine,
    _principal: &Principal,
    arguments: &Json,
) -> Result<Json, String> {
    let board = arg_str(arguments, "board")?;
    let name = arg_str(arguments, "name")?;
    engine.remove_secret(&board, &name).map_err(|e| e.to_string())?;
    ok(json!({ "deleted": true }))
}
