use super::{arg_str, ok};
use engine::model::Principal;
use engine::ServerlessEngine;
use serde_json::{json, Value as Json};

pub async fn list(
    engine: &ServerlessEngine,
    _principal: &Principal,
    _arguments: &Json,
) -> Result<Json, String> {
    let recipes = engine.list_recipes().await.map_err(|e| e.to_string())?;
    let out: Vec<Json> = recipes
        .into_iter()
        .map(|r| json!({ "name": r.name, "enabled": r.enabled }))
        .collect();
    ok(json!({ "recipes": out }))
}

pub async fn show(
    engine: &ServerlessEngine,
    _principal: &Principal,
    arguments: &Json,
) -> Result<Json, String> {
    let name = arg_str(arguments, "name")?;
    let recipe = engine.get_recipe(name).await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("recipe '{name}' not found"))?;
    ok(json!({
        "name": recipe.name,
        "enabled": recipe.enabled,
        "when": recipe.when_json,
        "match": recipe.match_json,
        "actions": recipe.actions_json,
        "dedup_on": recipe.dedup_on,
        "table": recipe.table,
    }))
}

pub async fn add(
    engine: &mut ServerlessEngine,
    _principal: &Principal,
    arguments: &Json,
) -> Result<Json, String> {
    let name = arg_str(arguments, "name")?;
    let recipe = engine::model::Recipe {
        name: name.to_string(),
        when_json: arguments.get("when").cloned().unwrap_or(Json::Null),
        match_json: arguments.get("match").cloned(),
        enabled: arguments.get("enabled").and_then(|e| e.as_bool()).unwrap_or(true),
        dedup_on: arguments.get("dedup_on").and_then(|d| d.as_str()).map(String::from),
        actions_json: arguments.get("actions").cloned(),
        table: arguments.get("table").and_then(|t| t.as_str()).map(String::from),
    };
    engine.add_recipe(&recipe).await.map_err(|e| e.to_string())?;
    ok(json!({ "name": name }))
}