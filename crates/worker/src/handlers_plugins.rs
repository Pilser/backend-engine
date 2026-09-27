//! Native plugin routes (Phase B): install/list/show/remove manifests.
//! Install/remove are admin-gated (they create tables, jobs, registrations);
//! list/show need a reader like the tables inventory.

use serde_json::{json, Value as Json};
use worker::{Request, Response, Result, RouteContext};

use crate::{auth, cors, query};

use super::handlers_tables::body_json_for;

const MAX_JSON: usize = 1_000_000;

fn admin(app: &auth::Ctx) -> std::result::Result<(), Response> {
    if auth::require_admin(&app.principal) {
        Ok(())
    } else {
        Err(cors::deny("admin authorization required"))
    }
}

fn reader(app: &auth::Ctx) -> std::result::Result<(), Response> {
    if auth::require_read(&app.principal) {
        Ok(())
    } else {
        Err(cors::deny("reader authorization required"))
    }
}

pub async fn install(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let mut app = match auth::ctx_for(&req, &ctx).await {
        Ok(c) => c,
        Err(r) => return Ok(r),
    };
    if let Err(r) = admin(&app) {
        return Ok(r);
    }
    let manifest = match body_json_for(req, MAX_JSON).await {
        Ok(b) => b,
        Err(r) => return Ok(r),
    };
    Ok(match engine::plugins::install(&mut app.engine, &manifest).await {
        Ok(out) => cors::created(out),
        Err(e) => cors::bad(&e),
    })
}

pub async fn list(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let app = match auth::ctx_for(&req, &ctx).await {
        Ok(c) => c,
        Err(r) => return Ok(r),
    };
    if let Err(r) = reader(&app) {
        return Ok(r);
    }
    Ok(match engine::plugins::list(&app.engine).await {
        Ok(plugins) => cors::ok(json!({ "ok": true, "plugins": plugins })),
        Err(e) => cors::bad(&e),
    })
}

pub async fn show(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let app = match auth::ctx_for(&req, &ctx).await {
        Ok(c) => c,
        Err(r) => return Ok(r),
    };
    if let Err(r) = reader(&app) {
        return Ok(r);
    }
    let slug = ctx.param("slug").cloned().unwrap_or_default();
    Ok(match engine::plugins::get(&app.engine, &slug).await {
        Ok(Some(plugin)) => match engine::plugins::routes_for(&app.engine, &slug).await {
            Ok(routes) => cors::ok(json!({ "ok": true, "plugin": plugin, "routes": routes })),
            Err(e) => cors::bad(&e),
        },
        Ok(None) => cors::gone(&format!("plugin '{slug}' is not installed")),
        Err(e) => cors::bad(&e),
    })
}

/// Execute a plugin route binding (Phase C: configured functions).
/// `ANY /api/plugin/{slug}/{route}` — the binding's method is enforced
/// here (405 otherwise). Reads honor `public_reads`; writes need a writer;
/// the binding's base filter ANDs with any caller `?filter=`.
pub async fn call(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let mut app = match auth::ctx_for(&req, &ctx).await {
        Ok(c) => c,
        Err(r) => return Ok(r),
    };
    let slug = ctx.param("slug").cloned().unwrap_or_default();
    let name = ctx.param("route").cloned().unwrap_or_default();
    let binding = match engine::plugins::find_route(&app.engine, &slug, &name).await {
        Ok(Some(b)) => b,
        Ok(None) => return Ok(cors::gone(&format!("no route '{name}' on plugin '{slug}'"))),
        Err(e) => return Ok(cors::bad(&e)),
    };
    let method = req.method().to_string();
    let allows = binding.get("method").and_then(|v| v.as_str()).unwrap_or("GET");
    if method != allows {
        return Ok(cors::err(405, &format!("route '{name}' allows {allows}")));
    }
    let table = binding.get("table").and_then(|v| v.as_str()).unwrap_or("").to_string();
    if app.engine.get_table(&table).await.unwrap_or(None).is_none() {
        return Ok(cors::gone(&format!("route table '{table}' is gone")));
    }
    let op = binding.get("op").and_then(|v| v.as_str()).unwrap_or("");
    let p = query::params(&req);
    match op {
        "query" | "aggregate" => {
            if op == "query" {
                if !auth::can_read(&mut app).await {
                    return Ok(cors::deny("private app"));
                }
            } else if !auth::require_read(&app.principal) {
                return Ok(cors::deny("reader authorization required"));
            }
            let client = p.get("filter").filter(|s| !s.is_empty()).map(|s| {
                serde_json::from_str(s).unwrap_or(Json::Null)
            });
            let merged = engine::plugins::merge_filter_json(binding.get("filter"), client.as_ref());
            let mut conds = match engine::storage::ir::parse_filter(&merged) {
                Ok(f) => f.conds,
                Err(e) => return Ok(cors::bad(&e)),
            };
            if let Some(c) = auth::scope_cond(&app.principal) {
                conds.push(c);
            }
            if op == "aggregate" {
                let agg = match engine::storage::ir::Agg::parse(p.get("op").map(|s| s.as_str()).unwrap_or("count")) {
                    Ok(a) => a,
                    Err(e) => return Ok(cors::bad(&e)),
                };
                let sf = engine::storage::ir::SrvFilter { conds };
                return Ok(match app.engine.aggregate_records(
                    &table, &sf, agg,
                    p.get("field").map(|s| s.as_str()),
                    p.get("group").map(|s| s.as_str()),
                ).await {
                    Ok(res) => cors::ok(json!({ "ok": true, "aggregate": res })),
                    Err(e) => cors::bad(&e),
                });
            }
            if let Some(q) = p.get("q").filter(|q| !q.is_empty()) {
                let limit = engine::plugins::binding_limit(&binding, None);
                let sf = engine::storage::ir::SrvFilter { conds };
                let snippet = query::is_true(&p, "hl");
                return Ok(match app.engine.search_records(&table, q, &sf, limit, 0, snippet).await {
                    Ok(rs) => cors::ok(json!({ "ok": true, "records": query::records_json(&rs) })),
                    Err(e) => cors::bad(&e),
                });
            }
            let orders: Vec<(String, bool)> = binding
                .get("order")
                .and_then(|v| v.as_str())
                .map(|o| {
                    let mut it = o.split_whitespace();
                    let f = it.next().unwrap_or("seq").to_string();
                    let d = it.next().map(|w| w.eq_ignore_ascii_case("desc")).unwrap_or(false);
                    vec![(f, d)]
                })
                .unwrap_or_default();
            let client_limit = p.get("limit").and_then(|s| s.parse::<usize>().ok());
            let limit = engine::plugins::binding_limit(&binding, client_limit);
            let sf = engine::storage::ir::SrvFilter { conds };
            Ok(match app.engine.query_records(&table, &sf, &orders, limit, 0).await {
                Ok(rs) => cors::ok(json!({ "ok": true, "records": query::records_json(&rs) })),
                Err(e) => cors::bad(&e),
            })
        }
        "get" => {
            if !auth::require_read(&app.principal) {
                return Ok(cors::deny("reader authorization required"));
            }
            let seq: i64 = match p.get("seq").and_then(|s| s.parse().ok()) {
                Some(n) => n,
                None => return Ok(cors::err(400, "route get needs ?seq=")),
            };
            Ok(match app.engine.get_record(&table, seq).await {
                Ok(Some(rec)) => cors::ok(json!({ "ok": true, "record": query::records_json(&[rec]).into_iter().next() })),
                Ok(None) => cors::gone("not found"),
                Err(e) => cors::bad(&e),
            })
        }
        "submit" => {
            if !auth::require_write(&app.principal) {
                return Ok(cors::deny("writer authorization required"));
            }
            let payload = match body_json_for(req, MAX_JSON).await {
                Ok(b) => b,
                Err(r) => return Ok(r),
            };
            let upsert = query::is_true(&p, "upsert");
            let writer = app.principal.writer.clone();
            Ok(match app.engine.insert_record(&table, payload, writer.as_deref(), upsert, &app.principal).await {
                Ok(seq) => cors::ok(json!({ "ok": true, "seq": seq })),
                Err(e) => cors::bad(&e),
            })
        }
        _ => Ok(cors::err(400, "unknown route op")),
    }
}

pub async fn remove(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let mut app = match auth::ctx_for(&req, &ctx).await {
        Ok(c) => c,
        Err(r) => return Ok(r),
    };
    if let Err(r) = admin(&app) {
        return Ok(r);
    }
    let slug = ctx.param("slug").cloned().unwrap_or_default();
    let prune = query::params(&req)
        .get("prune")
        .map(|v| v == "true" || v == "1")
        .unwrap_or(false);
    Ok(match engine::plugins::remove(&mut app.engine, &slug, prune).await {
        Ok(out) => cors::ok(out),
        Err(e) => cors::bad(&e),
    })
}
