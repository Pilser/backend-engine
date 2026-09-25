use super::rest::{
    err_json, json_response, ok_json, orders_from, parse_filter_param, rate_action,
    records_json, require_admin, require_read, require_write, url_decode, BoxBodyResp,
};
use crate::Server;
use bytes::Bytes;
use engine::model::Principal;
use engine::storage::ir::{FilterCond, Op, SrvFilter};
use hyper::{Response, StatusCode};
use serde_json::{json, Value as Json};
use std::collections::HashMap;

fn board_exists(server: &Server, board: &str) -> bool {
    server.engine.lock().unwrap().get_app(board).ok().flatten().is_some()
}

fn public_reads(server: &Server, board: &str) -> bool {
    server
        .engine
        .lock()
        .unwrap()
        .get_app(board)
        .ok()
        .flatten()
        .map(|b| b.public_reads)
        .unwrap_or(false)
}

// ---- tables lifecycle -----------------------------------------------------

pub fn tables_create(
    server: &Server,
    board: &str,
    principal: &Principal,
    body: &Bytes,
) -> Response<BoxBodyResp> {
    if !require_admin(principal) {
        return err_json(StatusCode::FORBIDDEN, "admin authorization required");
    }
    if !board_exists(server, board) {
        return err_json(StatusCode::NOT_FOUND, "not found");
    }
    let req: Json = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return err_json(StatusCode::BAD_REQUEST, "invalid json"),
    };
    let table = req
        .get("name")
        .or_else(|| req.get("table"))
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .to_string();
    let schema = req.get("schema").cloned();
    let unique_key = req.get("unique_key").and_then(|u| u.as_str());
    match server.engine.lock().unwrap().create_table(board, &table, schema, unique_key) {
        Ok(cfg) => json_response(StatusCode::CREATED, json!({ "ok": true, "table": cfg })),
        Err(e) => err_json(StatusCode::BAD_REQUEST, &e.to_string()),
    }
}

pub fn tables_list(server: &Server, board: &str, principal: &Principal) -> Response<BoxBodyResp> {
    if !require_read(principal) {
        return err_json(StatusCode::FORBIDDEN, "reader authorization required");
    }
    if !board_exists(server, board) {
        return err_json(StatusCode::NOT_FOUND, "not found");
    }
    match server.engine.lock().unwrap().list_tables(board) {
        Ok(tables) => ok_json(json!({ "ok": true, "tables": tables })),
        Err(e) => err_json(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

pub fn tables_show(server: &Server, board: &str, table: &str, principal: &Principal) -> Response<BoxBodyResp> {
    if !require_read(principal) {
        return err_json(StatusCode::FORBIDDEN, "reader authorization required");
    }
    if !board_exists(server, board) {
        return err_json(StatusCode::NOT_FOUND, "not found");
    }
    match server.engine.lock().unwrap().get_table(board, table) {
        Ok(Some(cfg)) => ok_json(json!({ "ok": true, "table": cfg })),
        Ok(None) => err_json(StatusCode::NOT_FOUND, "table not found"),
        Err(e) => err_json(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

pub fn tables_delete(server: &Server, board: &str, table: &str, principal: &Principal) -> Response<BoxBodyResp> {
    if !require_admin(principal) {
        return err_json(StatusCode::FORBIDDEN, "admin authorization required");
    }
    if !board_exists(server, board) {
        return err_json(StatusCode::NOT_FOUND, "not found");
    }
    match server.engine.lock().unwrap().drop_table(board, table) {
        Ok(true) => ok_json(json!({ "ok": true, "deleted": true })),
        Ok(false) => err_json(StatusCode::NOT_FOUND, "table not found"),
        Err(e) => err_json(StatusCode::BAD_REQUEST, &e.to_string()),
    }
}

// ---- records --------------------------------------------------------------

pub fn submit(
    server: &Server,
    board: &str,
    table: &str,
    principal: &Principal,
    body: &Bytes,
    params: &HashMap<String, String>,
) -> Response<BoxBodyResp> {
    if let Err(e) = rate_action(server, board, principal, "submit") {
        return err_json(StatusCode::TOO_MANY_REQUESTS, &e.to_string());
    }
    if !board_exists(server, board) {
        return err_json(StatusCode::NOT_FOUND, "not found");
    }
    if !require_write(principal) {
        return err_json(StatusCode::FORBIDDEN, "writer authorization required");
    }
    let payload: Json = match serde_json::from_slice(body) {
        Ok(p) => p,
        Err(_) => return err_json(StatusCode::BAD_REQUEST, "invalid json"),
    };
    let upsert = params
        .get("upsert")
        .map(|v| matches!(v.as_str(), "1" | "true"))
        .unwrap_or(false);
    let writer = principal.writer.clone();
    match server
        .engine
        .lock()
        .unwrap()
        .insert_record(board, table, payload.clone(), writer.as_deref(), upsert, principal)
    {
        Ok(seq) => ok_json(json!({ "ok": true, "seq": seq })),
        Err(e) => err_json(StatusCode::BAD_REQUEST, &e.to_string()),
    }
}

pub fn bulk(
    server: &Server,
    board: &str,
    table: &str,
    principal: &Principal,
    body: &Bytes,
    params: &HashMap<String, String>,
) -> Response<BoxBodyResp> {
    if !require_write(principal) {
        return err_json(StatusCode::FORBIDDEN, "writer authorization required");
    }
    let req: Json = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return err_json(StatusCode::BAD_REQUEST, "invalid json"),
    };
    let records = match req.get("records").and_then(|r| r.as_array()).cloned() {
        Some(r) => r,
        None => return err_json(StatusCode::BAD_REQUEST, "expected \"records\" array"),
    };
    let upsert = params.get("upsert").map(|v| v == "1").unwrap_or(false);
    let writer = principal.writer.clone();
    let migrate = params.get("migrate").map(|v| v == "1").unwrap_or(false);
    let result = if migrate {
        server
            .engine
            .lock()
            .unwrap()
            .bulk_import(board, table, records.clone(), writer.as_deref(), principal)
    } else {
        server
            .engine
            .lock()
            .unwrap()
            .bulk_insert(board, table, records.clone(), writer.as_deref(), upsert, principal)
    };
    match result {
        Ok(seqs) => ok_json(json!({ "ok": true, "seqs": seqs })),
        Err(e) => err_json(StatusCode::BAD_REQUEST, &e.to_string()),
    }
}

pub fn import_records(
    server: &Server,
    board: &str,
    table: &str,
    principal: &Principal,
    body: &Bytes,
) -> Response<BoxBodyResp> {
    if !require_write(principal) {
        return err_json(StatusCode::FORBIDDEN, "writer authorization required");
    }
    let req: Json = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return err_json(StatusCode::BAD_REQUEST, "invalid json"),
    };
    let format = req.get("format").and_then(|f| f.as_str()).unwrap_or("json").to_string();
    let data = req.get("data").and_then(|d| d.as_str()).unwrap_or("").to_string();
    let separator = req
        .get("separator")
        .and_then(|s| s.as_str())
        .and_then(|s| s.chars().next())
        .unwrap_or(',');
    let upsert = req.get("upsert").and_then(|u| u.as_bool()).unwrap_or(false);
    match server
        .engine
        .lock()
        .unwrap()
        .import_records(board, table, &format, &data, separator, upsert, principal)
    {
        Ok(report) => ok_json(json!({ "ok": true, "result": report })),
        Err(e) => err_json(StatusCode::BAD_REQUEST, &e.to_string()),
    }
}

pub fn list_records(
    server: &Server,
    board: &str,
    table: &str,
    principal: &Principal,
    params: &HashMap<String, String>,
) -> Response<BoxBodyResp> {
    if let Err(e) = rate_action(server, board, principal, "read") {
        return err_json(StatusCode::TOO_MANY_REQUESTS, &e.to_string());
    }
    if !public_reads(server, board) && !require_read(principal) {
        return err_json(StatusCode::FORBIDDEN, "private board");
    }
    let limit: usize = params.get("limit").and_then(|l| l.parse().ok()).unwrap_or(50).clamp(1, 200);
    let offset: usize = params.get("offset").and_then(|v| v.parse().ok()).unwrap_or(0);
    let before: Option<i64> = params.get("before").and_then(|b| b.parse().ok());
    let dir = params.get("dir").map(|d| d.as_str()).unwrap_or("desc").to_string();
    match server.engine.lock().unwrap().list_records(board, table, limit, before, offset, &dir) {
        Ok(rs) => ok_json(json!({ "ok": true, "records": records_json(&rs) })),
        Err(e) => table_err(table, e),
    }
}

pub fn query_records(
    server: &Server,
    board: &str,
    table: &str,
    principal: &Principal,
    params: &HashMap<String, String>,
) -> Response<BoxBodyResp> {
    if let Err(e) = rate_action(server, board, principal, if params.contains_key("q") { "search" } else { "read" }) {
        return err_json(StatusCode::TOO_MANY_REQUESTS, &e.to_string());
    }
    if !public_reads(server, board) && !require_read(principal) {
        return err_json(StatusCode::FORBIDDEN, "private board");
    }
    let limit: usize = params.get("limit").and_then(|l| l.parse().ok()).unwrap_or(50).clamp(1, 500);
    let offset: usize = params.get("offset").and_then(|v| v.parse().ok()).unwrap_or(0);
    let orders = orders_from(params);
    let mut extra_conds: Vec<FilterCond> = Vec::new();
    if let Some(s) = &principal.scope {
        extra_conds.push(FilterCond {
            field: "$.customer_id".to_string(),
            op: Op::Eq,
            value: Json::String(s.clone()),
        });
    }
    let engine = server.engine.lock().unwrap();
    if let Some(q) = params.get("q").map(|q| url_decode(q)).filter(|q| !q.is_empty()) {
        let mut conds = match params.get("filter").map(|f| url_decode(f)) {
            Some(f) if !f.is_empty() => match parse_filter_param(&f) {
                Ok(sf) => sf.conds,
                Err(e) => return err_json(StatusCode::BAD_REQUEST, &e.to_string()),
            },
            _ => Vec::new(),
        };
        conds.extend(extra_conds);
        let snippet = params.get("hl").map(|v| matches!(v.as_str(), "1" | "on" | "true")).unwrap_or(false);
        let sf = SrvFilter { conds };
        match engine.search_records(board, table, q.as_str(), &sf, limit, offset, snippet) {
            Ok(rs) => ok_json(json!({ "ok": true, "records": records_json(&rs) })),
            Err(e) => err_json(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
        }
    } else {
        let filter = match params.get("filter").map(|f| url_decode(f)) {
            Some(f) if !f.is_empty() => match parse_filter_param(&f) {
                Ok(sf) => sf,
                Err(e) => return err_json(StatusCode::BAD_REQUEST, &e.to_string()),
            },
            _ => SrvFilter::new(),
        };
        let mut conds = filter.conds;
        conds.extend(extra_conds);
        let sf = SrvFilter { conds };
        match engine.query_records(board, table, &sf, &orders, limit, offset) {
            Ok(rs) => ok_json(json!({ "ok": true, "records": records_json(&rs) })),
            Err(e) => table_err(table, e),
        }
    }
}

/// Map engine errors: missing table -> 404 (so clients can treat it as an
/// empty result); everything else -> 500.
fn table_err(table: &str, e: anyhow::Error) -> Response<BoxBodyResp> {
    if e.to_string().contains(&format!("table '{table}' does not exist")) {
        err_json(StatusCode::NOT_FOUND, &format!("table '{table}' does not exist"))
    } else {
        err_json(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string())
    }
}

pub fn aggregate_records(
    server: &Server,
    board: &str,
    table: &str,
    principal: &Principal,
    params: &HashMap<String, String>,
) -> Response<BoxBodyResp> {
    if !require_read(principal) {
        return err_json(StatusCode::FORBIDDEN, "reader authorization required");
    }
    let agg = match engine::storage::ir::Agg::parse(params.get("op").map(|s| s.as_str()).unwrap_or("count")) {
        Ok(a) => a,
        Err(e) => return err_json(StatusCode::BAD_REQUEST, &e.to_string()),
    };
    let field = params.get("field").map(|s| s.as_str());
    let group = params.get("group").map(|s| s.as_str());
    let filter = match params.get("filter").map(|f| url_decode(f)) {
        Some(f) if !f.is_empty() => match parse_filter_param(&f) {
            Ok(sf) => sf,
            Err(e) => return err_json(StatusCode::BAD_REQUEST, &e.to_string()),
        },
        _ => SrvFilter::new(),
    };
    match server.engine.lock().unwrap().aggregate_records(board, table, &filter, agg, field, group) {
        Ok(res) => ok_json(json!({ "ok": true, "aggregate": res })),
        Err(e) => table_err(table, e),
    }
}

pub fn get_record(
    server: &Server,
    board: &str,
    table: &str,
    principal: &Principal,
    params: &HashMap<String, String>,
) -> Response<BoxBodyResp> {
    if let Err(e) = rate_action(server, board, principal, "read") {
        return err_json(StatusCode::TOO_MANY_REQUESTS, &e.to_string());
    }
    let seq: i64 = match params.get("seq").and_then(|s| s.parse().ok()) {
        Some(s) => s,
        None => return err_json(StatusCode::BAD_REQUEST, "missing or invalid seq"),
    };
    if !public_reads(server, board) && !require_read(principal) {
        return err_json(StatusCode::FORBIDDEN, "private board");
    }
    match server.engine.lock().unwrap().get_record(board, table, seq) {
        Ok(Some(record)) => {
            let rec = records_json(&[record]).into_iter().next().unwrap();
            ok_json(json!({ "ok": true, "record": rec }))
        }
        Ok(None) => err_json(StatusCode::NOT_FOUND, "not found"),
        Err(e) => err_json(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

pub fn put_record(
    server: &Server,
    board: &str,
    table: &str,
    principal: &Principal,
    seq: &str,
    body: &Bytes,
) -> Response<BoxBodyResp> {
    if !require_write(principal) {
        return err_json(StatusCode::FORBIDDEN, "writer authorization required");
    }
    let seq: i64 = match seq.parse() {
        Ok(s) => s,
        Err(_) => return err_json(StatusCode::BAD_REQUEST, "invalid seq"),
    };
    let payload: Json = match serde_json::from_slice(body) {
        Ok(p) => p,
        Err(_) => return err_json(StatusCode::BAD_REQUEST, "invalid json"),
    };
    let writer = principal.writer.clone();
    match server.engine.lock().unwrap().set_record(board, table, seq, payload, writer.as_deref()) {
        Ok(()) => ok_json(json!({ "ok": true, "seq": seq })),
        Err(e) => err_json(StatusCode::BAD_REQUEST, &e.to_string()),
    }
}

pub fn patch_record(
    server: &Server,
    board: &str,
    table: &str,
    principal: &Principal,
    seq: &str,
    body: &Bytes,
) -> Response<BoxBodyResp> {
    if !require_write(principal) {
        return err_json(StatusCode::FORBIDDEN, "writer authorization required");
    }
    let seq: i64 = match seq.parse() {
        Ok(s) => s,
        Err(_) => return err_json(StatusCode::BAD_REQUEST, "invalid seq"),
    };
    let ops: Json = match serde_json::from_slice(body) {
        Ok(p) => p,
        Err(_) => return err_json(StatusCode::BAD_REQUEST, "invalid json"),
    };
    let writer = principal.writer.clone();
    match server.engine.lock().unwrap().patch_record(board, table, seq, &ops, writer.as_deref()) {
        Ok(merged) => ok_json(json!({ "ok": true, "seq": seq, "payload": merged })),
        Err(e) => err_json(StatusCode::BAD_REQUEST, &e.to_string()),
    }
}

pub fn delete_record(
    server: &Server,
    board: &str,
    table: &str,
    principal: &Principal,
    seq: &str,
) -> Response<BoxBodyResp> {
    if !require_write(principal) {
        return err_json(StatusCode::FORBIDDEN, "writer authorization required");
    }
    let seq: i64 = match seq.parse() {
        Ok(s) => s,
        Err(_) => return err_json(StatusCode::BAD_REQUEST, "invalid seq"),
    };
    match server.engine.lock().unwrap().delete_record(board, table, seq) {
        Ok(true) => ok_json(json!({ "ok": true, "seq": seq, "deleted": true })),
        Ok(false) => err_json(StatusCode::NOT_FOUND, "not found"),
        Err(e) => err_json(StatusCode::BAD_REQUEST, &e.to_string()),
    }
}

pub fn delete_records(
    server: &Server,
    board: &str,
    table: &str,
    principal: &Principal,
    params: &HashMap<String, String>,
) -> Response<BoxBodyResp> {
    if !require_write(principal) {
        return err_json(StatusCode::FORBIDDEN, "writer authorization required");
    }
    let filter = match params.get("filter").map(|f| url_decode(f)) {
        Some(f) if !f.is_empty() => match parse_filter_param(&f) {
            Ok(sf) => sf,
            Err(e) => return err_json(StatusCode::BAD_REQUEST, &e.to_string()),
        },
        _ => return err_json(StatusCode::BAD_REQUEST, "expected filter query param"),
    };
    match server.engine.lock().unwrap().delete_records(board, table, &filter) {
        Ok(deleted) => ok_json(json!({ "ok": true, "deleted": deleted })),
        Err(e) => err_json(StatusCode::BAD_REQUEST, &e.to_string()),
    }
}

/// Dispatch `/api/srv/{board}/tables/<tail>` where tail starts after "tables".
pub fn handle_tables(
    server: &Server,
    board: &str,
    method: &hyper::Method,
    t: &[&str],
    body: &Bytes,
    params: &HashMap<String, String>,
    principal: &Principal,
) -> Response<BoxBodyResp> {
    use hyper::Method;
    match (method, t) {
        (&Method::GET, []) => tables_list(server, board, principal),
        (&Method::POST, []) => tables_create(server, board, principal, body),
        (&Method::GET, [table]) => tables_show(server, board, table, principal),
        (&Method::DELETE, [table]) => tables_delete(server, board, table, principal),
        (&Method::POST, [table, "submit"]) => submit(server, board, table, principal, body, params),
        (&Method::POST, [table, "bulk"]) => bulk(server, board, table, principal, body, params),
        (&Method::POST, [table, "import"]) => import_records(server, board, table, principal, body),
        (&Method::GET, [table, "records"]) => list_records(server, board, table, principal, params),
        (&Method::DELETE, [table, "records"]) => delete_records(server, board, table, principal, params),
        (&Method::GET, [table, "query"]) => query_records(server, board, table, principal, params),
        (&Method::GET, [table, "aggregate"]) => aggregate_records(server, board, table, principal, params),
        (&Method::GET, [table, "record"]) => get_record(server, board, table, principal, params),
        (&Method::PUT, [table, "records", seq]) => put_record(server, board, table, principal, seq, body),
        (&Method::PATCH, [table, "records", seq]) => patch_record(server, board, table, principal, seq, body),
        (&Method::DELETE, [table, "records", seq]) => delete_record(server, board, table, principal, seq),
        _ => err_json(StatusCode::NOT_FOUND, "not found"),
    }
}
