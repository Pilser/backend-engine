use super::{arg_str, ok};
use engine::model::Principal;
use engine::ServerlessEngine;
use serde_json::{json, Value as Json};

pub fn list(
    engine: &ServerlessEngine,
    _principal: &Principal,
    arguments: &Json,
) -> Result<Json, String> {
    let board = arg_str(arguments, "board")?;
    let keys = engine.list_keys(&board).map_err(|e| e.to_string())?;
    let out: Vec<Json> = keys
        .into_iter()
        .map(|k| {
            json!({
                "name": k.bucket,
                "role": k.role,
                "scope": k.scope,
                "revoked": k.revoked_at.is_some(),
            })
        })
        .collect();
    ok(json!({ "keys": out }))
}

pub fn show(
    engine: &ServerlessEngine,
    _principal: &Principal,
    arguments: &Json,
) -> Result<Json, String> {
    let bucket = arg_str(arguments, "bucket")?;
    let key = engine
        .get_key(bucket)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("key '{bucket}' not found"))?;
    ok(json!({
        "bucket": key.bucket,
        "board_id": key.board_id,
        "role": key.role,
        "scope": key.scope,
        "writer": key.writer,
        "revoked": key.revoked_at.is_some(),
        "revoked_at": key.revoked_at,
        "key_hash": key.key_hash,
        "salt": key.salt,
    }))
}