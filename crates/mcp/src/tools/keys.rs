use super::{arg_str, ok};
use engine::model::Principal;
use engine::ServerlessEngine;
use serde_json::{json, Value as Json};

pub async fn list(
    engine: &ServerlessEngine,
    _principal: &Principal,
    _arguments: &Json,
) -> Result<Json, String> {
    let keys = engine.list_keys().await.map_err(|e| e.to_string())?;
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

pub async fn issue(
    engine: &mut ServerlessEngine,
    _principal: &Principal,
    arguments: &Json,
) -> Result<Json, String> {
    let role = arguments.get("role").and_then(|r| r.as_str()).unwrap_or("writer");
    let writer = arguments.get("writer").and_then(|w| w.as_str());
    let scope = arguments.get("scope").and_then(|s| s.as_str());
    let (kr, secret) = engine.issue_key(role, writer, scope).await.map_err(|e| e.to_string())?;
    ok(json!({ "bucket": kr.bucket, "key": secret, "role": kr.role, "writer": kr.writer, "scope": kr.scope }))
}

pub async fn show(
    engine: &ServerlessEngine,
    _principal: &Principal,
    arguments: &Json,
) -> Result<Json, String> {
    let bucket = arg_str(arguments, "bucket")?;
    let key = engine.get_key(bucket).await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("key '{bucket}' not found"))?;
    // Never reveal key_hash/salt — identity + role + state only (REST parity).
    ok(json!({
        "bucket": key.bucket,
        "role": key.role,
        "scope": key.scope,
        "writer": key.writer,
        "revoked": key.revoked_at.is_some(),
        "revoked_at": key.revoked_at,
    }))
}