use super::{arg_i64, arg_str, ok};
use engine::model::Principal;
use engine::ServerlessEngine;
use serde_json::{json, Value as Json};

pub async fn link(
    engine: &mut ServerlessEngine,
    _principal: &Principal,
    arguments: &Json,
) -> Result<Json, String> {
    let from = arg_i64(arguments, "from", 0);
    let label = arg_str(arguments, "label")?;
    let to = arg_i64(arguments, "to", 0);
    let props = arguments.get("props").cloned().unwrap_or(Json::Object(Default::default()));
    if from <= 0 || to <= 0 {
        return Err("from/to must be positive node ids ($id)".into());
    }
    let edge_id = engine.graph_link(from, label, to, &props).await
        .map_err(|e| e.to_string())?;
    ok(json!({ "edge": edge_id, "from": from, "label": label, "to": to }))
}

pub async fn unlink(
    engine: &mut ServerlessEngine,
    _principal: &Principal,
    arguments: &Json,
) -> Result<Json, String> {
    let edge_id = arg_i64(arguments, "edge", 0);
    if edge_id <= 0 {
        return Err("edge must be a positive edge id ($id from graph link / search_edges)".into());
    }
    let removed = engine.graph_unlink(edge_id).await.map_err(|e| e.to_string())?;
    ok(json!({ "edge": edge_id, "removed": removed }))
}

pub async fn delete(
    engine: &mut ServerlessEngine,
    _principal: &Principal,
    arguments: &Json,
) -> Result<Json, String> {
    let node_id = arg_i64(arguments, "node", 0);
    if node_id <= 0 {
        return Err("node must be a positive node id ($id)".into());
    }
    let removed = engine.graph_delete_node(node_id).await
        .map_err(|e| e.to_string())?;
    ok(json!({ "node": node_id, "removed_edges": removed.saturating_sub(1), "deleted": true }))
}

pub async fn traverse(
    engine: &ServerlessEngine,
    _principal: &Principal,
    arguments: &Json,
) -> Result<Json, String> {
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
    let cursor = engine.graph_traverse(from, label.as_deref(), &dir, depth).await
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

pub async fn sync(
    engine: &mut ServerlessEngine,
    _principal: &Principal,
    arguments: &Json,
) -> Result<Json, String> {
    let table = arg_str(arguments, "table")?;
    let report = engine.graph_sync(table).await.map_err(|e| e.to_string())?;
    ok(report)
}

pub async fn search_edges(
    engine: &ServerlessEngine,
    _principal: &Principal,
    arguments: &Json,
) -> Result<Json, String> {
    let label = arg_str(arguments, "label")?;
    let property = arg_str(arguments, "property")?;
    let q = arg_str(arguments, "q")?;
    let limit = arg_i64(arguments, "limit", 20).clamp(1, 200) as usize;
    let edges = engine.graph_search_edges(label, property, q, limit).await
        .map_err(|e| e.to_string())?;
    ok(json!({ "edges": edges, "count": edges.len() }))
}
