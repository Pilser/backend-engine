use super::{arg_str, ok};
use engine::model::Principal;
use engine::ServerlessEngine;
use serde_json::{json, Value as Json};

pub async fn install(
    engine: &mut ServerlessEngine,
    _principal: &Principal,
    arguments: &Json,
) -> Result<Json, String> {
    let manifest = arguments.get("manifest").ok_or_else(|| "missing manifest (trailing JSON)".to_string())?;
    let out = engine::plugins::install(engine, manifest).await.map_err(|e| e.to_string())?;
    ok(out)
}

pub async fn list(
    engine: &mut ServerlessEngine,
    _principal: &Principal,
    _arguments: &Json,
) -> Result<Json, String> {
    let plugins = engine::plugins::list(engine).await.map_err(|e| e.to_string())?;
    ok(json!({ "plugins": plugins }))
}

pub async fn show(
    engine: &mut ServerlessEngine,
    _principal: &Principal,
    arguments: &Json,
) -> Result<Json, String> {
    let slug = arg_str(arguments, "slug")?;
    let plugin = engine::plugins::get(engine, slug)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("plugin '{slug}' is not installed"))?;
    ok(json!({ "plugin": plugin }))
}

pub async fn remove(
    engine: &mut ServerlessEngine,
    _principal: &Principal,
    arguments: &Json,
) -> Result<Json, String> {
    let slug = arg_str(arguments, "slug")?;
    let prune = arguments.get("prune").and_then(|v| v.as_bool()).unwrap_or(false);
    let out = engine::plugins::remove(engine, slug, prune).await.map_err(|e| e.to_string())?;
    ok(out)
}
