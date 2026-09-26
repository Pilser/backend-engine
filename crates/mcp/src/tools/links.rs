use super::ok;
use engine::model::Principal;
use engine::ServerlessEngine;
use serde_json::{json, Value as Json};

pub async fn list(
    engine: &ServerlessEngine,
    _principal: &Principal,
    _arguments: &Json,
) -> Result<Json, String> {
    let links = engine.list_links().await.map_err(|e| e.to_string())?;
    let out: Vec<Json> = links
        .into_iter()
        .map(|l| {
            json!({
                "child_table": l.child_table,
                "parent_table": l.parent_table,
                "from_key": l.from_key,
                "parent_key": l.parent_key,
            })
        })
        .collect();
    ok(json!({ "links": out }))
}

pub async fn show(
    engine: &ServerlessEngine,
    _principal: &Principal,
    _arguments: &Json,
) -> Result<Json, String> {
    let link = engine.get_link().await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "no link configured".to_string())?;
    ok(json!({
        "child_board": link.child_board,
        "child_table": link.child_table,
        "parent_board": link.parent_board,
        "parent_table": link.parent_table,
        "from_key": link.from_key,
        "parent_key": link.parent_key,
    }))
}

/// `graph.schema` — the link wiring of the app as seen by the engine.
///
/// Reports the declarative wiring the engine tracks: the parent/child link
/// (from `wb_links`) plus per-concern counts. Edge-label tracking (a
/// `wb_graph_schema` table maintained on link/unlink/sync) is proposed in
/// docs/graph-schema-tracking.md.
pub async fn schema(
    engine: &ServerlessEngine,
    _principal: &Principal,
    _arguments: &Json,
) -> Result<Json, String> {
    let links = engine.list_links().await.map_err(|e| e.to_string())?;
    let tables = engine.list_tables().await.map_err(|e| e.to_string())?;
    let recipes = engine.list_recipes().await.map_err(|e| e.to_string())?;
    let jobs = engine.list_jobs().await.map_err(|e| e.to_string())?;
    ok(json!({
        "tenant": engine::TENANT,
        "links": links,
        "tables": tables.iter().map(|t| t.table.clone()).collect::<Vec<_>>(),
        "recipe_count": recipes.len(),
        "job_count": jobs.len(),
    }))
}
