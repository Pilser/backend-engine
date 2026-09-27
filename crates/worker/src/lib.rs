//! backend-engine — single-tenant Workers engine entry point.
//!
//! One Worker = one app. HTTP serving (`fetch`), background work (`scheduled`
//! Cron Trigger + `queue` webhook pipeline) and per-tenant state (`TenantDO`)
//! all funnel into the pure async `engine` core via thin host adapters.

pub mod ai_proxy;
pub mod auth;
pub mod cors;
pub mod d1_db;
pub mod email_inbound;
pub mod handlers_admin;
pub mod handlers_core;
pub mod handlers_oauth;
pub mod handlers_site;
pub mod handlers_tables;
pub mod http_caller;
pub mod query;
pub mod queue;
pub mod r2_store;
pub mod router;
pub mod scheduled;
pub mod tenant_do;

use worker::{
    event, Context, Env, Request, Response, Result, ScheduleContext, ScheduledEvent,
};

#[event(fetch, respond_with_errors)]
pub async fn main(req: Request, env: Env, ctx: Context) -> Result<Response> {
    // AI assistant mount first (it owns /srv/ai/* and legacy /ws).
    if ai_proxy::is_ai_route(&req) {
        return ai_proxy::serve(req, &env, &ctx).await;
    }
    router::run(req, env).await
}

/// Cron Trigger body: due jobs, TTL sweep, webhook flush (queue or inline).
/// Fires every 5 minutes per `wrangler.toml [triggers]`.
#[event(scheduled)]
pub async fn on_scheduled(_event: ScheduledEvent, env: Env, _ctx: ScheduleContext) {
    scheduled::run(&env).await;
}

/// Webhook delivery consumer. The scheduler claims due rows and enqueues
/// them; delivery (HMAC, backoff bookkeeping) happens here so sub-5-minute
/// retries work. Without the queue binding the scheduler delivers inline.
#[event(queue)]
pub async fn on_queue(
    batch: worker::MessageBatch<queue::DeliveryMsg>,
    env: Env,
    _ctx: Context,
) -> Result<()> {
    queue::consume(&env, batch).await
}

/// Inbound email (Cloudflare Email Routing → this worker, Phase A).
/// Stores into `email_log` and runs `email.received` recipes. Route the
/// domain's mail here in the Cloudflare dashboard; no extra worker.
#[event(email)]
pub async fn on_email(
    message: worker::ForwardableEmailMessage,
    env: Env,
    _ctx: Context,
) -> Result<()> {
    email_inbound::handle(message, &env).await
}
