//! Fetch router: the full single-tenant API surface (no `{board}` segment).
//! Mirrors the donor route tables (`rest.rs` core, `rest_tables.rs`,
//! `rest_admin.rs` — see PORT-TRACK.md harvest map). Precedence: OPTIONS →
//! explicit routes → 404 fallback. Realtime streaming is a 501 until the
//! Phase 7 `TenantDO` fan-out lands.

use worker::{Env, Request, Response, Result, Router};

use crate::{
    cors,
    handlers_admin as admin,
    handlers_core as core,
    handlers_oauth as oauth,
    handlers_site as site,
    handlers_tables as tables,
};

pub async fn run(req: Request, env: Env) -> Result<Response> {
    Router::new()
        .options("/*catchall", |_, _| Ok(cors::preflight()))
        .get("/healthz", |_, _| {
            Ok(cors::ok(serde_json::json!({ "ok": true, "service": "backend-engine" })))
        })
        .get("/api/version", |_, _| {
            Ok(cors::ok(serde_json::json!({
                "ok": true, "version": engine::VERSION, "tenant": engine::TENANT,
            })))
        })
        // app + auth
        .get_async("/api/app", core::app_info)
        .patch_async("/api/app", core::app_patch)
        .get_async("/api/resources", core::resources)
        .post_async("/api/auth/signup", core::signup)
        .post_async("/api/auth/login", core::login)
        .post_async("/api/auth/logout", core::logout)
        .post_async("/api/auth/role", core::set_role)
        .get_async("/api/auth/me", core::me)
        .get_async("/api/auth/oauth/start", oauth::start)
        .get_async("/api/auth/oauth/callback", oauth::callback)
        // tables + records
        .get_async("/api/tables", tables::tables_list)
        .post_async("/api/tables", tables::tables_create)
        .get_async("/api/tables/:table", tables::tables_show)
        .delete_async("/api/tables/:table", tables::tables_delete)
        .post_async("/api/tables/:table/submit", tables::submit)
        .post_async("/api/tables/:table/bulk", tables::bulk)
        .post_async("/api/tables/:table/import", tables::import_records)
        .get_async("/api/tables/:table/records", tables::list_records)
        .delete_async("/api/tables/:table/records", tables::delete_records)
        .get_async("/api/tables/:table/query", tables::query_records)
        .get_async("/api/tables/:table/aggregate", tables::aggregate_records)
        .put_async("/api/tables/:table/records/:seq", tables::put_record)
        .patch_async("/api/tables/:table/records/:seq", tables::patch_record)
        .delete_async("/api/tables/:table/records/:seq", tables::delete_record)
        // files + call + events
        .post_async("/api/upload", core::upload)
        .get_async("/api/file", core::file)
        .post_async("/api/call", core::call)
        .post_async("/api/events", core::events_inbound)
        .get_async("/api/events", core::events_stream)
        // assets
        .put_async("/api/assets/*rel", core::asset_put)
        .get_async("/api/assets", core::asset_list)
        .get_async("/api/assets/*rel", core::asset_get)
        .delete_async("/api/assets/*rel", core::asset_delete)
        // admin: keys
        .post_async("/api/keys", admin::issue_key)
        .get_async("/api/keys", admin::list_keys)
        .delete_async("/api/keys", admin::revoke_key)
        // admin: hooks
        .get_async("/api/hooks", admin::list_hooks)
        .post_async("/api/hooks", admin::register_hook)
        .delete_async("/api/hooks", admin::remove_hook)
        .get_async("/api/hooks/deliveries", admin::hook_deliveries)
        // admin: jobs
        .get_async("/api/jobs", admin::list_jobs)
        .post_async("/api/jobs", admin::add_job)
        .delete_async("/api/jobs", admin::remove_job)
        .get_async("/api/jobs/runs", admin::job_runs)
        // admin: recipes
        .get_async("/api/recipes", admin::list_recipes)
        .post_async("/api/recipes", admin::add_recipe)
        .get_async("/api/recipes/:name", admin::get_recipe)
        .patch_async("/api/recipes/:name", admin::set_recipe_enabled)
        .delete_async("/api/recipes/:name", admin::remove_recipe)
        // admin: secrets
        .get_async("/api/secrets", admin::list_secrets)
        .post_async("/api/secrets", admin::set_secret)
        .delete_async("/api/secrets/:name", admin::remove_secret)
        // admin: config knobs
        .put_async("/api/rate", admin::rate_config)
        .put_async("/api/ttl", admin::ttl_set)
        .delete_async("/api/ttl", admin::ttl_clear)
        .put_async("/api/link", admin::link_set)
        .delete_async("/api/link", admin::link_clear)
        .put_async("/api/audit", admin::audit_set)
        .get_async("/api/audit", admin::audit_list)
        .put_async("/api/computed", |req, ctx| async move {
            admin::set_config_kind(req, ctx, "computed", false).await
        })
        .delete_async("/api/computed", |req, ctx| async move {
            admin::set_config_kind(req, ctx, "computed", true).await
        })
        .put_async("/api/validate", |req, ctx| async move {
            admin::set_config_kind(req, ctx, "validate", false).await
        })
        .delete_async("/api/validate", |req, ctx| async move {
            admin::set_config_kind(req, ctx, "validate", true).await
        })
        .put_async("/api/redact", |req, ctx| async move {
            admin::set_config_kind(req, ctx, "redact", false).await
        })
        .delete_async("/api/redact", |req, ctx| async move {
            admin::set_config_kind(req, ctx, "redact", true).await
        })
        .put_async("/api/webhook_secret", admin::webhook_secret_set)
        .delete_async("/api/webhook_secret", admin::webhook_secret_clear)
        .get_async("/api/config", admin::config)
        // system + agent surface + static site
        .get_async("/api/system/health", core::system_health)
        .post_async("/mcp", core::mcp)
        .get_async("/mcp", core::mcp_get)
        .get_async("/srv", site::srv_root)
        // Bare "/srv/" (empty path): serve() defaults an empty target to the
        // index.html SPA fallback — without this, /*path needs ≥1 segment and
        // the canonical URL falls through to the 404 catchall.
        .get_async("/srv/", site::serve)
        .get_async("/srv/*path", site::serve)
        .or_else_any_method_async("/*catchall", |req, _ctx| async move {
            Ok(cors::err(404, &format!("not found: {}", req.path())))
        })
        .run(req, env)
        .await
}
