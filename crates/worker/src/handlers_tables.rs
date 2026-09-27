//! Tables + records routes (`/api/tables/…`). Ports the donor
//! `rest_tables.rs` handlers: same params, shapes and status codes, minus
//! `{board}` (single tenant) and minus the in-process rate limiter (Phase 7
//! `TenantDO`). Customer-scoped keys additionally get ownership enforcement
//! on point reads/writes (documented donor intent).

use serde_json::{json, Value as Json};
use worker::{Request, Response, Result, RouteContext};

use crate::{auth, cors, query};

const MAX_JSON: usize = 1_000_000;
const MAX_BULK: usize = 10_000_000;

fn table_param(ctx: &RouteContext<()>) -> std::result::Result<String, Response> {
    ctx.param("table").filter(|t| !t.is_empty()).cloned().ok_or_else(|| cors::err(400, "missing table"))
}

fn table_err(table: &str, e: &anyhow::Error) -> Response {
    if e.to_string().contains(&format!("table '{table}' does not exist")) {
        cors::gone(&format!("table '{table}' does not exist"))
    } else {
        cors::srv(e)
    }
}

pub(crate) async fn body_json_for(req: Request, cap: usize) -> std::result::Result<Json, Response> {
    body_json(req, cap).await
}

async fn body_json(mut req: Request, cap: usize) -> std::result::Result<Json, Response> {
    let bytes = req.bytes().await.map_err(|_| cors::err(400, "unreadable body"))?;
    if bytes.len() > cap {
        return Err(cors::err(413, "body too large"));
    }
    serde_json::from_slice(&bytes).map_err(|_| cors::err(400, "invalid json"))
}

async fn ctx_with_scope(
    req: &Request,
    ctx: &RouteContext<()>,
) -> std::result::Result<auth::Ctx, Response> {
    auth::ctx_for(req, ctx).await
}

// ---- tables lifecycle -----------------------------------------------------

pub async fn tables_list(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let app = match ctx_with_scope(&req, &ctx).await {
        Ok(c) => c,
        Err(r) => return Ok(r),
    };
    if !auth::require_read(&app.principal) {
        return Ok(cors::deny("reader authorization required"));
    }
    Ok(match app.engine.list_tables().await {
        Ok(tables) => {
            // Table-scoped keys (S2) only see their tables.
            let tables: Vec<_> = tables
                .into_iter()
                .filter(|t| app.principal.allows_table(&t.table))
                .collect();
            cors::ok(json!({ "ok": true, "tables": tables }))
        }
        Err(e) => cors::srv(&e),
    })
}

pub async fn tables_create(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let mut app = match ctx_with_scope(&req, &ctx).await {
        Ok(c) => c,
        Err(r) => return Ok(r),
    };
    if !auth::require_admin(&app.principal) {
        return Ok(cors::deny("admin authorization required"));
    }
    let body = match body_json(req, MAX_JSON).await {
        Ok(b) => b,
        Err(r) => return Ok(r),
    };
    let table = body
        .get("name")
        .or_else(|| body.get("table"))
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .to_string();
    let schema = body.get("schema").cloned();
    let unique_key = body.get("unique_key").and_then(|u| u.as_str());
    Ok(match app.engine.create_table(&table, schema, unique_key).await {
        Ok(cfg) => {
            if body.get("public_read").is_some()
                || body.get("write_only").is_some()
                || body.get("allow_anon_submit").is_some()
            {
                let mut patch = serde_json::Map::new();
                for k in ["public_read", "write_only", "allow_anon_submit"] {
                    if let Some(v) = body.get(k) {
                        patch.insert(k.into(), v.clone());
                    }
                }
                if let Err(e) = app.engine.set_table_policy(&table, &Json::Object(patch)).await {
                    return Ok(cors::bad(&e));
                }
            }
            cors::created(json!({ "ok": true, "table": cfg }))
        }
        Err(e) => cors::bad(&e),
    })
}

/// PATCH /api/tables/:table — per-table access policy (S1: P0 proposals).
/// Body `{public_read, write_only}`: absent leaves, null clears.
pub async fn tables_policy(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let table = match table_param(&ctx) {
        Ok(t) => t,
        Err(r) => return Ok(r),
    };
    let mut app = match ctx_with_scope(&req, &ctx).await {
        Ok(c) => c,
        Err(r) => return Ok(r),
    };
    if !auth::require_admin(&app.principal) {
        return Ok(cors::deny("admin authorization required"));
    }
    let body = match body_json(req, MAX_JSON).await {
        Ok(b) => b,
        Err(r) => return Ok(r),
    };
    Ok(match app.engine.set_table_policy(&table, &body).await {
        Ok(()) => cors::ok(json!({ "ok": true, "table": table, "updated": true })),
        Err(e) => cors::bad(&e),
    })
}

pub async fn tables_show(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let table = match table_param(&ctx) {
        Ok(t) => t,
        Err(r) => return Ok(r),
    };
    let app = match ctx_with_scope(&req, &ctx).await {
        Ok(c) => c,
        Err(r) => return Ok(r),
    };
    if !auth::require_read(&app.principal) {
        return Ok(cors::deny("reader authorization required"));
    }
    if !app.principal.allows_table(&table) {
        return Ok(cors::gone("table not found"));
    }
    Ok(match app.engine.get_table(&table).await {
        Ok(Some(cfg)) => cors::ok(json!({ "ok": true, "table": cfg })),
        Ok(None) => cors::gone("table not found"),
        Err(e) => cors::srv(&e),
    })
}

pub async fn tables_delete(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let table = match table_param(&ctx) {
        Ok(t) => t,
        Err(r) => return Ok(r),
    };
    let mut app = match ctx_with_scope(&req, &ctx).await {
        Ok(c) => c,
        Err(r) => return Ok(r),
    };
    if !auth::require_admin(&app.principal) {
        return Ok(cors::deny("admin authorization required"));
    }
    Ok(match app.engine.drop_table(&table).await {
        Ok(true) => cors::ok(json!({ "ok": true, "deleted": true })),
        Ok(false) => cors::gone("table not found"),
        Err(e) => cors::bad(&e),
    })
}

// ---- records --------------------------------------------------------------

pub async fn submit(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let table = match table_param(&ctx) {
        Ok(t) => t,
        Err(r) => return Ok(r),
    };
    let p = query::params(&req);
    let upsert = query::is_true(&p, "upsert");
    let mut app = match ctx_with_scope(&req, &ctx).await {
        Ok(c) => c,
        Err(r) => return Ok(r),
    };
    if !auth::can_table_submit(&app, &table).await {
        return Ok(cors::deny("writer authorization required"));
    }
    let payload = match body_json(req, MAX_JSON).await {
        Ok(b) => b,
        Err(r) => return Ok(r),
    };
    let writer = app.principal.writer.clone();
    Ok(match app.engine.insert_record(&table, payload, writer.as_deref(), upsert, &app.principal).await {
        Ok(seq) => cors::ok(json!({ "ok": true, "seq": seq })),
        Err(e) => cors::bad(&e),
    })
}

pub async fn bulk(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let table = match table_param(&ctx) {
        Ok(t) => t,
        Err(r) => return Ok(r),
    };
    let p = query::params(&req);
    let upsert = query::is_true(&p, "upsert");
    let migrate = query::is_true(&p, "migrate");
    let mut app = match ctx_with_scope(&req, &ctx).await {
        Ok(c) => c,
        Err(r) => return Ok(r),
    };
    if !auth::can_table_submit(&app, &table).await {
        return Ok(cors::deny("writer authorization required"));
    }
    let body = match body_json(req, MAX_BULK).await {
        Ok(b) => b,
        Err(r) => return Ok(r),
    };
    let records = match body.get("records").and_then(|r| r.as_array()).cloned() {
        Some(r) => r,
        None => return Ok(cors::err(400, "expected \"records\" array")),
    };
    let writer = app.principal.writer.clone();
    let result = if migrate {
        app.engine.bulk_import(&table, records, writer.as_deref(), &app.principal).await
    } else {
        app.engine.bulk_insert(&table, records, writer.as_deref(), upsert, &app.principal).await
    };
    Ok(match result {
        Ok(seqs) => cors::ok(json!({ "ok": true, "seqs": seqs })),
        Err(e) => cors::bad(&e),
    })
}

pub async fn import_records(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let table = match table_param(&ctx) {
        Ok(t) => t,
        Err(r) => return Ok(r),
    };
    let mut app = match ctx_with_scope(&req, &ctx).await {
        Ok(c) => c,
        Err(r) => return Ok(r),
    };
    if !auth::can_table_submit(&app, &table).await {
        return Ok(cors::deny("writer authorization required"));
    }
    let body = match body_json(req, MAX_BULK).await {
        Ok(b) => b,
        Err(r) => return Ok(r),
    };
    let format = body.get("format").and_then(|f| f.as_str()).unwrap_or("json").to_string();
    let data = body.get("data").and_then(|d| d.as_str()).unwrap_or("").to_string();
    let separator = body.get("separator").and_then(|s| s.as_str()).and_then(|s| s.chars().next()).unwrap_or(',');
    let upsert = body.get("upsert").and_then(|u| u.as_bool()).unwrap_or(false);
    Ok(match app.engine.import_records(&table, &format, &data, separator, upsert, &app.principal).await {
        Ok(report) => cors::ok(json!({ "ok": true, "result": report })),
        Err(e) => cors::bad(&e),
    })
}

pub async fn list_records(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let table = match table_param(&ctx) {
        Ok(t) => t,
        Err(r) => return Ok(r),
    };
    let p = query::params(&req);
    let mut app = match ctx_with_scope(&req, &ctx).await {
        Ok(c) => c,
        Err(r) => return Ok(r),
    };
    if !auth::can_table_read(&mut app, &table).await {
        return Ok(cors::deny("private app"));
    }
    let limit = query::limit(&p, "limit", 50, 200);
    let offset = query::offset(&p);
    let before: Option<i64> = p.get("before").and_then(|b| b.parse().ok());
    let dir = p.get("dir").cloned().unwrap_or_else(|| "desc".to_string());
    Ok(match app.engine.list_records(&table, limit, before, offset, &dir).await {
        Ok(rs) => cors::ok(json!({ "ok": true, "records": query::records_json(&rs) })),
        Err(e) => table_err(&table, &e),
    })
}

pub async fn query_records(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let table = match table_param(&ctx) {
        Ok(t) => t,
        Err(r) => return Ok(r),
    };
    let p = query::params(&req);
    let mut app = match ctx_with_scope(&req, &ctx).await {
        Ok(c) => c,
        Err(r) => return Ok(r),
    };
    if !auth::can_table_read(&mut app, &table).await {
        return Ok(cors::deny("private app"));
    }
    let limit = query::limit(&p, "limit", 50, 500);
    let offset = query::offset(&p);
    let orders = query::orders_from(&p);
    let mut conds: Vec<engine::storage::ir::FilterCond> = Vec::new();
    if let Some(c) = auth::scope_cond(&app.principal) {
        conds.push(c);
    }
    if let Some(q) = p.get("q").filter(|q| !q.is_empty()) {
        let mut fconds = match p.get("filter") {
            Some(f) if !f.is_empty() => match query::parse_filter_param(f) {
                Ok(sf) => sf.conds,
                Err(e) => return Ok(cors::bad(&e)),
            },
            _ => Vec::new(),
        };
        fconds.extend(conds);
        let snippet = query::is_true(&p, "hl");
        let sf = engine::storage::ir::SrvFilter { conds: fconds };
        return Ok(match app.engine.search_records(&table, q, &sf, limit, offset, snippet).await {
            Ok(rs) => cors::ok(json!({ "ok": true, "records": query::records_json(&rs) })),
            Err(e) => cors::srv(&e),
        });
    }
    let mut filter = match query::filter_from(&p) {
        Ok(f) => f,
        Err(e) => return Ok(cors::bad(&e)),
    };
    filter.conds.extend(conds);
    Ok(match app.engine.query_records(&table, &filter, &orders, limit, offset).await {
        Ok(rs) => cors::ok(json!({ "ok": true, "records": query::records_json(&rs) })),
        Err(e) => table_err(&table, &e),
    })
}

pub async fn aggregate_records(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let table = match table_param(&ctx) {
        Ok(t) => t,
        Err(r) => return Ok(r),
    };
    let p = query::params(&req);
    let mut app = match ctx_with_scope(&req, &ctx).await {
        Ok(c) => c,
        Err(r) => return Ok(r),
    };
    if !auth::can_table_read(&mut app, &table).await {
        return Ok(cors::deny("private app"));
    }
    let agg = match engine::storage::ir::Agg::parse(p.get("op").map(|s| s.as_str()).unwrap_or("count")) {
        Ok(a) => a,
        Err(e) => return Ok(cors::bad(&e)),
    };
    let filter = match query::filter_from(&p) {
        Ok(f) => f,
        Err(e) => return Ok(cors::bad(&e)),
    };
    Ok(match app
        .engine
        .aggregate_records(&table, &filter, agg, p.get("field").map(|s| s.as_str()), p.get("group").map(|s| s.as_str()))
        .await
    {
        Ok(res) => cors::ok(json!({ "ok": true, "aggregate": res })),
        Err(e) => table_err(&table, &e),
    })
}

async fn load_for_write(
    app: &mut auth::Ctx,
    table: &str,
    seq: i64,
) -> std::result::Result<engine::model::Record, Response> {
    let rec = app.engine.get_record(table, seq).await.map_err(|e| cors::srv(&e))?;
    match rec {
        Some(r) if auth::scope_ok(&app.principal, &r.payload) => Ok(r),
        _ => Err(cors::gone("not found")),
    }
}

pub async fn put_record(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let table = match table_param(&ctx) {
        Ok(t) => t,
        Err(r) => return Ok(r),
    };
    let seq: i64 = match ctx.param("seq").and_then(|s| s.parse().ok()) {
        Some(s) => s,
        None => return Ok(cors::err(400, "invalid seq")),
    };
    let mut app = match ctx_with_scope(&req, &ctx).await {
        Ok(c) => c,
        Err(r) => return Ok(r),
    };
    if !auth::require_write(&app.principal) {
        return Ok(cors::deny("writer authorization required"));
    }
    let payload = match body_json(req, MAX_JSON).await {
        Ok(b) => b,
        Err(r) => return Ok(r),
    };
    if load_for_write(&mut app, &table, seq).await.is_err() {
        return Ok(cors::gone("not found"));
    }
    let writer = app.principal.writer.clone();
    Ok(match app.engine.set_record(&table, seq, payload, writer.as_deref()).await {
        Ok(()) => cors::ok(json!({ "ok": true, "seq": seq })),
        Err(e) => cors::bad(&e),
    })
}

pub async fn patch_record(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let table = match table_param(&ctx) {
        Ok(t) => t,
        Err(r) => return Ok(r),
    };
    let seq: i64 = match ctx.param("seq").and_then(|s| s.parse().ok()) {
        Some(s) => s,
        None => return Ok(cors::err(400, "invalid seq")),
    };
    let mut app = match ctx_with_scope(&req, &ctx).await {
        Ok(c) => c,
        Err(r) => return Ok(r),
    };
    if !auth::require_write(&app.principal) {
        return Ok(cors::deny("writer authorization required"));
    }
    let ops = match body_json(req, MAX_JSON).await {
        Ok(b) => b,
        Err(r) => return Ok(r),
    };
    if load_for_write(&mut app, &table, seq).await.is_err() {
        return Ok(cors::gone("not found"));
    }
    let writer = app.principal.writer.clone();
    Ok(match app.engine.patch_record(&table, seq, &ops, writer.as_deref()).await {
        Ok(merged) => cors::ok(json!({ "ok": true, "seq": seq, "payload": merged })),
        Err(e) => cors::bad(&e),
    })
}

pub async fn delete_record(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let table = match table_param(&ctx) {
        Ok(t) => t,
        Err(r) => return Ok(r),
    };
    let seq: i64 = match ctx.param("seq").and_then(|s| s.parse().ok()) {
        Some(s) => s,
        None => return Ok(cors::err(400, "invalid seq")),
    };
    let mut app = match ctx_with_scope(&req, &ctx).await {
        Ok(c) => c,
        Err(r) => return Ok(r),
    };
    if !auth::require_write(&app.principal) {
        return Ok(cors::deny("writer authorization required"));
    }
    if load_for_write(&mut app, &table, seq).await.is_err() {
        return Ok(cors::gone("not found"));
    }
    Ok(match app.engine.delete_record(&table, seq).await {
        Ok(true) => cors::ok(json!({ "ok": true, "seq": seq, "deleted": true })),
        Ok(false) => cors::gone("not found"),
        Err(e) => cors::bad(&e),
    })
}

pub async fn delete_records(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let table = match table_param(&ctx) {
        Ok(t) => t,
        Err(r) => return Ok(r),
    };
    let p = query::params(&req);
    let mut app = match ctx_with_scope(&req, &ctx).await {
        Ok(c) => c,
        Err(r) => return Ok(r),
    };
    if !auth::require_write(&app.principal) {
        return Ok(cors::deny("writer authorization required"));
    }
    // Customer keys must not bulk-delete across scopes (donor parity).
    if app.principal.scope.is_some() {
        return Ok(cors::deny("bulk delete forbidden for scoped keys"));
    }
    let filter = match p.get("filter") {
        Some(f) if !f.is_empty() => match query::parse_filter_param(f) {
            Ok(sf) => sf,
            Err(e) => return Ok(cors::bad(&e)),
        },
        _ => return Ok(cors::err(400, "expected filter query param")),
    };
    Ok(match app.engine.delete_records(&table, &filter).await {
        Ok(deleted) => cors::ok(json!({ "ok": true, "deleted": deleted })),
        Err(e) => cors::bad(&e),
    })
}
