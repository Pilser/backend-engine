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
        Ok(Some(plugin)) => cors::ok(json!({ "ok": true, "plugin": plugin })),
        Ok(None) => cors::gone(&format!("plugin '{slug}' is not installed")),
        Err(e) => cors::bad(&e),
    })
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
