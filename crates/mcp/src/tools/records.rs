use super::{arg_i64, arg_str, ok};
use engine::model::Principal;
use engine::ServerlessEngine;
use serde_json::{json, Value as Json};

pub fn submit(
    engine: &mut ServerlessEngine,
    principal: &Principal,
    arguments: &Json,
) -> Result<Json, String> {
    let board = arg_str(arguments, "board")?;
    let table = arg_str(arguments, "table")?;
    let payload = arguments
        .get("payload")
        .cloned()
        .ok_or_else(|| "missing argument 'payload'".to_string())?;
    let upsert = arguments.get("upsert").and_then(|u| u.as_bool()).unwrap_or(false);
    let seq = engine
        .insert_record(&board, &table, payload, Some(&principal.id), upsert, principal)
        .map_err(|e| e.to_string())?;
    ok(json!({ "seq": seq }))
}

pub fn import_records(
    engine: &mut ServerlessEngine,
    principal: &Principal,
    arguments: &Json,
) -> Result<Json, String> {
    let board = arg_str(arguments, "board")?;
    let table = arg_str(arguments, "table")?;
    let format = arguments.get("format").and_then(|f| f.as_str()).unwrap_or("json").to_string();
    let data = arg_str(arguments, "data")?;
    let separator = arguments
        .get("separator")
        .and_then(|s| s.as_str())
        .and_then(|s| s.chars().next())
        .unwrap_or(',');
    let upsert = arguments.get("upsert").and_then(|u| u.as_bool()).unwrap_or(false);
    let report = engine
        .import_records(&board, &table, &format, &data, separator, upsert, principal)
        .map_err(|e| e.to_string())?;
    ok(report)
}

pub fn bulk(
    engine: &mut ServerlessEngine,
    principal: &Principal,
    arguments: &Json,
) -> Result<Json, String> {
    let board = arg_str(arguments, "board")?;
    let table = arg_str(arguments, "table")?;
    let records = arguments
        .get("records")
        .and_then(|r| r.as_array())
        .cloned()
        .ok_or_else(|| "missing 'records' array".to_string())?;
    let upsert = arguments.get("upsert").and_then(|u| u.as_bool()).unwrap_or(false);
    let migrate = arguments.get("migrate").and_then(|m| m.as_bool()).unwrap_or(false);
    let writer = principal.writer.clone();
    let seqs = if migrate {
        engine
            .bulk_import(&board, &table, records.clone(), writer.as_deref(), principal)
            .map_err(|e| e.to_string())?
    } else {
        engine
            .bulk_insert(&board, &table, records.clone(), writer.as_deref(), upsert, principal)
            .map_err(|e| e.to_string())?
    };
    ok(json!({ "seqs": seqs, "migrate": migrate }))
}

pub fn get(
    engine: &ServerlessEngine,
    _principal: &Principal,
    arguments: &Json,
) -> Result<Json, String> {
    let board = arg_str(arguments, "board")?;
    let table = arg_str(arguments, "table")?;
    let seq = arg_i64(arguments, "seq", 0);
    let record = engine
        .get_record(&board, &table, seq)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("record {seq} not found"))?;
    ok(serde_json::to_value(&record).map_err(|e| e.to_string())?)
}

pub fn list(
    engine: &ServerlessEngine,
    _principal: &Principal,
    arguments: &Json,
) -> Result<Json, String> {
    let board = arg_str(arguments, "board")?;
    let table = arg_str(arguments, "table")?;
    let limit = arg_i64(arguments, "limit", 50).min(500).max(1) as usize;
    let offset = arg_i64(arguments, "offset", 0) as usize;
    let dir = arguments.get("dir").and_then(|d| d.as_str()).unwrap_or("desc").to_string();
    if !matches!(dir.as_str(), "asc" | "desc") {
        return Err("dir must be one of: asc | desc".into());
    }
    let records = engine
        .list_records(&board, &table, limit, None, offset, &dir)
        .map_err(|e| e.to_string())?;
    let out: Vec<Json> = records
        .into_iter()
        .map(|r| json!({ "seq": r.seq, "node_id": r.payload.get("_node_id").cloned().unwrap_or(Json::Null), "payload": r.payload }))
        .collect();
    ok(json!({ "records": out }))
}

pub fn query(
    engine: &ServerlessEngine,
    _principal: &Principal,
    arguments: &Json,
) -> Result<Json, String> {
    let board = arg_str(arguments, "board")?;
    let table = arg_str(arguments, "table")?;
    let limit = arg_i64(arguments, "limit", 50).min(500).max(1) as usize;
    let offset = arg_i64(arguments, "offset", 0) as usize;
    let filter_json = arguments.get("filter").cloned().unwrap_or(Json::Null);
    let filter = match filter_json {
        Json::Null => engine::storage::ir::SrvFilter::new(),
        other => engine::storage::ir::parse_filter(&other).map_err(|e| e.to_string())?,
    };
    let orders: Vec<(String, bool)> = arguments
        .get("order")
        .and_then(|o| o.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|o| o.as_str().map(|s| (s.trim_start_matches('-').to_string(), s.starts_with('-'))))
                .collect()
        })
        .unwrap_or_default();

    // Guardrail: an unfiltered query over a large table would dump a huge
    // payload into the agent's context. Block it and point at the right tool.
    let unfiltered = filter.conds.is_empty()
        && !arguments.get("order").and_then(|o| o.as_array()).map(|a| !a.is_empty()).unwrap_or(false);
    if unfiltered && !arguments.get("allow_unfiltered").and_then(|a| a.as_bool()).unwrap_or(false) {
        let count = engine
            .aggregate_records(&board, &table, &engine::storage::ir::SrvFilter::new(), engine::storage::ir::Agg::Count, None, None)
            .ok()
            .and_then(|rows| rows.first().and_then(|r| r.get("value").and_then(|v| v.as_f64())))
            .unwrap_or(0.0) as i64;
        if count > 1000 {
            return Err(format!(
                "records.query without a filter would return ~{count} rows (table '{table}' on board '{board}'). \
                 Use a --filter to narrow the scan, or use one of the specialised tools instead:\n\
                 \x20 • records.aggregate — count/sum/avg/min/max without pulling rows\n\
                 \x20 • records.list — bounded page of the latest records\n\
                 \x20 • records.search — BM25 full-text over the payload\n\
                 If you really need the raw rows, pass --allow_unfiltered true (not recommended for large tables)."
            ));
        }
    }
    let records = engine
        .query_records(&board, &table, &filter, &orders, limit, offset)
        .map_err(|e| e.to_string())?;
    let out: Vec<Json> = records
        .into_iter()
        .map(|r| json!({ "seq": r.seq, "node_id": r.payload.get("_node_id").cloned().unwrap_or(Json::Null), "payload": r.payload }))
        .collect();
    ok(json!({ "records": out }))
}

pub fn search(
    engine: &ServerlessEngine,
    _principal: &Principal,
    arguments: &Json,
) -> Result<Json, String> {
    let board = arg_str(arguments, "board")?;
    let table = arg_str(arguments, "table")?;
    let q = arg_str(arguments, "q")?;
    let limit = arg_i64(arguments, "limit", 50).min(500).max(1) as usize;
    let offset = arg_i64(arguments, "offset", 0) as usize;
    let records = engine
        .search_records(&board, &table, &q, &engine::storage::ir::SrvFilter::new(), limit, offset, false)
        .map_err(|e| e.to_string())?;
    let out: Vec<Json> = records
        .into_iter()
        .map(|r| json!({ "seq": r.seq, "node_id": r.payload.get("_node_id").cloned().unwrap_or(Json::Null), "payload": r.payload }))
        .collect();
    ok(json!({ "records": out }))
}

pub fn aggregate(
    engine: &ServerlessEngine,
    _principal: &Principal,
    arguments: &Json,
) -> Result<Json, String> {
    let board = arg_str(arguments, "board")?;
    let table = arg_str(arguments, "table")?;
    let agg_str = arg_str(arguments, "agg")?;
    let agg = engine::storage::ir::Agg::parse(agg_str).map_err(|e| e.to_string())?;
    let field = arguments.get("field").and_then(|v| v.as_str()).map(String::from);
    let group_by = arguments.get("group_by").and_then(|v| v.as_str()).map(String::from);
    let filter_json = arguments.get("filter").cloned().unwrap_or(Json::Null);
    let filter = match filter_json {
        Json::Null => engine::storage::ir::SrvFilter::new(),
        other => engine::storage::ir::parse_filter(&other).map_err(|e| e.to_string())?,
    };
    let rows = engine
        .aggregate_records(&board, &table, &filter, agg, field.as_deref(), group_by.as_deref())
        .map_err(|e| e.to_string())?;
    ok(json!({ "agg": agg_str, "results": rows }))
}

pub fn delete(
    engine: &mut ServerlessEngine,
    _principal: &Principal,
    arguments: &Json,
) -> Result<Json, String> {
    let board = arg_str(arguments, "board")?;
    let table = arg_str(arguments, "table")?;
    let seq = arg_i64(arguments, "seq", 0);
    engine
        .delete_record(&board, &table, seq)
        .map_err(|e| e.to_string())?;
    ok(json!({ "seq": seq, "deleted": true }))
}

pub fn _unused(_: i64) {}