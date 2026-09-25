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
    let links = engine.list_links(board).map_err(|e| e.to_string())?;
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

pub fn show(
    engine: &ServerlessEngine,
    _principal: &Principal,
    arguments: &Json,
) -> Result<Json, String> {
    let board = arg_str(arguments, "board")?;
    let link = engine
        .get_link(board)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("no link configured for board {board}"))?;
    ok(json!({
        "child_board": link.child_board,
        "child_table": link.child_table,
        "parent_board": link.parent_board,
        "parent_table": link.parent_table,
        "from_key": link.from_key,
        "parent_key": link.parent_key,
    }))
}

/// `graph.schema` — the graph wiring of a board as seen by the engine.
///
/// Reports the declarative wiring the engine tracks: the parent/child link
/// (from `wb_links`) plus per-concern counts. Edge-label tracking (a
/// `wb_graph_schema` table maintained on link/unlink/sync) is proposed in
/// docs/graph-schema-tracking.md.
pub fn schema(
    engine: &ServerlessEngine,
    _principal: &Principal,
    arguments: &Json,
) -> Result<Json, String> {
    let board = arg_str(arguments, "board")?;
    let links = engine.list_links(board).map_err(|e| e.to_string())?;
    let tables = engine.list_tables(board).map_err(|e| e.to_string())?;
    let recipes = engine.list_recipes(board).map_err(|e| e.to_string())?;
    let jobs = engine.list_jobs(board).map_err(|e| e.to_string())?;
    ok(json!({
        "board": board,
        "links": links,
        "tables": tables.iter().map(|t| t.table.clone()).collect::<Vec<_>>(),
        "recipe_count": recipes.len(),
        "job_count": jobs.len(),
    }))
}
