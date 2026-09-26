use super::{arg_str, ok};
use engine::model::Principal;
use engine::ServerlessEngine;
use serde_json::{json, Value as Json};

pub async fn set(
    engine: &mut ServerlessEngine,
    _principal: &Principal,
    arguments: &Json,
) -> Result<Json, String> {
    let name = arg_str(arguments, "name")?;
    let value = arg_str(arguments, "value")?;
    engine.set_secret(&name, &value).await.map_err(|e| e.to_string())?;
    ok(json!({ "set": name.to_uppercase() }))
}

pub async fn list(
    engine: &ServerlessEngine,
    _principal: &Principal,
    _arguments: &Json,
) -> Result<Json, String> {
    let secrets = engine.list_secrets().await.map_err(|e| e.to_string())?;
    let out: Vec<Json> = secrets
        .into_iter()
        .map(|s| json!({ "name": s.name, "fingerprint": s.fingerprint }))
        .collect();
    ok(json!({ "secrets": out }))
}

pub async fn show(
    engine: &ServerlessEngine,
    _principal: &Principal,
    arguments: &Json,
) -> Result<Json, String> {
    let name = arg_str(arguments, "name")?;
    let secret = engine.get_secret(name).await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("secret '{name}' not found"))?;
    // MCP-only: reveal the decrypted value so the app folder / export can
    // capture the real content. The REST API never returns it.
    let value = engine.secret_value(name).await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("secret '{name}' not found"))?;
    ok(json!({
        "name": secret.name,
        "fingerprint": secret.fingerprint,
        "value": value,
    }))
}

pub async fn delete(
    engine: &mut ServerlessEngine,
    _principal: &Principal,
    arguments: &Json,
) -> Result<Json, String> {
    let name = arg_str(arguments, "name")?;
    engine.remove_secret(&name).await.map_err(|e| e.to_string())?;
    ok(json!({ "deleted": true }))
}
