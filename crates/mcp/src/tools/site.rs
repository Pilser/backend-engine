use super::{arg_str, ok};
use engine::model::Principal;
use engine::ServerlessEngine;
use serde_json::{json, Value as Json};

pub async fn routes(
    engine: &mut ServerlessEngine,
    _principal: &Principal,
    _arguments: &Json,
) -> Result<Json, String> {
    let routes = engine::site::route_list(engine).await.map_err(|e| e.to_string())?;
    ok(json!({ "routes": routes }))
}

pub async fn add(
    engine: &mut ServerlessEngine,
    _principal: &Principal,
    arguments: &Json,
) -> Result<Json, String> {
    let route = arguments.get("route").ok_or_else(|| "missing route (trailing JSON)".to_string())?;
    let owner = arguments.get("owner").and_then(|v| v.as_str()).unwrap_or("tenant");
    engine::site::route_put(engine, owner, route).await.map_err(|e| e.to_string())?;
    ok(json!({ "ok": true, "path": route.get("path") }))
}

pub async fn remove(
    engine: &mut ServerlessEngine,
    _principal: &Principal,
    arguments: &Json,
) -> Result<Json, String> {
    let path = arg_str(arguments, "path")?;
    let removed = engine::site::route_remove(engine, path).await.map_err(|e| e.to_string())?;
    ok(json!({ "removed": removed }))
}
