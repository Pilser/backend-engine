use super::{arg_i64, arg_str, ok};
use engine::model::Principal;
use engine::ServerlessEngine;
use serde_json::{json, Value as Json};

pub fn link(
    engine: &mut ServerlessEngine,
    _principal: &Principal,
    arguments: &Json,
) -> Result<Json, String> {
    let board = arg_str(arguments, "board")?;
    let from = arg_i64(arguments, "from", 0);
    let label = arg_str(arguments, "label")?;
    let to = arg_i64(arguments, "to", 0);
    let props = arguments.get("props").cloned().unwrap_or(Json::Object(Default::default()));
    if from <= 0 || to <= 0 {
        return Err("from/to must be positive node ids ($id)".into());
    }
    let edge_id = engine
        .graph_link(board, from, label, to, &props)
        .map_err(|e| e.to_string())?;
    ok(json!({ "edge": edge_id, "from": from, "label": label, "to": to }))
}

pub fn unlink(
    engine: &mut ServerlessEngine,
    _principal: &Principal,
    arguments: &Json,
) -> Result<Json, String> {
    let board = arg_str(arguments, "board")?;
    let edge_id = arg_i64(arguments, "edge", 0);
    if edge_id <= 0 {
        return Err("edge must be a positive edge id ($id from graph link / search_edges)".into());
    }
    let removed = engine.graph_unlink(board, edge_id).map_err(|e| e.to_string())?;
    ok(json!({ "edge": edge_id, "removed": removed }))
}

pub fn delete(
    engine: &mut ServerlessEngine,
    _principal: &Principal,
    arguments: &Json,
) -> Result<Json, String> {
    let board = arg_str(arguments, "board")?;
    let node_id = arg_i64(arguments, "node", 0);
    if node_id <= 0 {
        return Err("node must be a positive node id ($id)".into());
    }
    let removed = engine
        .graph_delete_node(board, node_id)
        .map_err(|e| e.to_string())?;
    ok(json!({ "node": node_id, "removed_edges": removed.saturating_sub(1), "deleted": true }))
}

pub fn traverse(
    engine: &ServerlessEngine,
    _principal: &Principal,
    arguments: &Json,
) -> Result<Json, String> {
    let board = arg_str(arguments, "board")?;
    let from = arg_i64(arguments, "from", 0);
    let dir = arguments.get("dir").and_then(|v| v.as_str()).unwrap_or("out").to_string();
    let label = arguments.get("label").and_then(|v| v.as_str()).map(String::from);
    let depth = arg_i64(arguments, "depth", 1).clamp(1, 10) as usize;
    if from <= 0 {
        return Err("from must be a positive node id ($id)".into());
    }
    if !matches!(dir.as_str(), "out" | "in" | "both") {
        return Err("dir must be one of: out | in | both".into());
    }
    let cursor = engine
        .graph_traverse(board, from, label.as_deref(), &dir, depth)
        .map_err(|e| e.to_string())?;
    let nodes: Vec<Json> = cursor
        .rows
        .into_iter()
        .map(|r| {
            let id = match &r.key {
                engine::model::Key::Int(v) => *v,
                engine::model::Key::Text(s) => s.parse().unwrap_or(0),
            };
            json!({ "id": id, "data": r.data })
        })
        .collect();
    ok(json!({ "nodes": nodes, "count": nodes.len() }))
}

pub fn sync(
    engine: &mut ServerlessEngine,
    _principal: &Principal,
    arguments: &Json,
) -> Result<Json, String> {
    let board = arg_str(arguments, "board")?;
    let table = arg_str(arguments, "table")?;
    let report = engine.graph_sync(board, table).map_err(|e| e.to_string())?;
    ok(report)
}

pub fn search_edges(
    engine: &ServerlessEngine,
    _principal: &Principal,
    arguments: &Json,
) -> Result<Json, String> {
    let board = arg_str(arguments, "board")?;
    let label = arg_str(arguments, "label")?;
    let property = arg_str(arguments, "property")?;
    let q = arg_str(arguments, "q")?;
    let limit = arg_i64(arguments, "limit", 20).clamp(1, 200) as usize;
    let edges = engine
        .graph_search_edges(board, label, property, q, limit)
        .map_err(|e| e.to_string())?;
    ok(json!({ "edges": edges, "count": edges.len() }))
}
