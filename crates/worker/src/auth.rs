//! Per-request context: engine (D1 + R2) + principal.
//!
//! Auth model (single tenant):
//! - `WORKER_KEY` bearer (secret, fallback `[vars]`) → admin. This is the
//!   bootstrap credential; it never touches the database.
//! - Otherwise the engine's own keys/users/sessions resolve the principal,
//!   with `public_reads` allowing anonymous reads. Scope (`?scope=` or key
//!   scope) enables customer-scoped access exactly like the donor daemon.
//! - Rate limiting is NOT enforced here (no per-isolate state survives);
//!   `TenantDO` owns rate state in Phase 7.

use engine::{model::Principal, ServerlessEngine};
use worker::{Env, Request, Response, Result};

use crate::{cors, d1_db::D1Db, http_caller::FetchCaller, r2_store::R2Store};

pub struct Ctx {
    pub engine: ServerlessEngine,
    pub principal: Principal,
}

fn cteq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut d = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        d |= x ^ y;
    }
    d == 0
}

fn bearer(req: &Request) -> Option<String> {
    // Authorization: Bearer <token>
    if let Ok(h) = req.headers().get("authorization") {
        if let Some(v) = h {
            if let Some(tok) = v.strip_prefix("Bearer ").or_else(|| v.strip_prefix("bearer ")) {
                let tok = tok.trim().to_string();
                if !tok.is_empty() {
                    return Some(tok);
                }
            }
        }
    }
    // Compatibility headers (donor daemon accepted these too).
    for name in ["x-srv-key", "x-api-key"] {
        if let Ok(Some(v)) = req.headers().get(name) {
            let v = v.trim().to_string();
            if !v.is_empty() {
                return Some(v);
            }
        }
    }
    None
}

fn worker_key(env: &Env) -> Option<String> {
    // Secret binding first, plain var fallback (local dev via .dev.vars).
    if let Ok(s) = env.secret("WORKER_KEY") {
        let s = s.to_string();
        if !s.is_empty() {
            return Some(s);
        }
    }
    env.var("WORKER_KEY").ok().map(|v| v.to_string()).filter(|s| !s.is_empty())
}

/// Build the per-request engine + principal. Returns a ready-to-send error
/// `Response` when the host bindings are missing (never for auth failure —
/// that yields an anonymous principal and the route guards decide).
pub async fn context(env: &Env, req: &Request, scope: Option<String>) -> std::result::Result<Ctx, Response> {
    if let Ok(secret) = env.secret("SECRET_KEY") {
        // Idempotent per isolate (first call wins; rotation takes effect as
        // isolates recycle).
        let _ = engine::secrets::set_master_key(&secret.to_string());
    }
    let db = env.d1("DB").map_err(|_| cors::err(500, "missing D1 binding DB"))?;
    let bucket = env.bucket("STORE").map_err(|_| cors::err(500, "missing R2 binding STORE"))?;
    let engine = ServerlessEngine::new(Box::new(D1Db::new(db)), Box::new(R2Store::new(bucket)));
    // Outbound HTTP for `$call` / jobs / webhooks (first install wins).
    engine.install_http_caller(Box::new(FetchCaller));

    // Token: explicit `?key=` wins (WS/SSE-style clients), else headers.
    let token: Option<String> = req
        .url()
        .ok()
        .and_then(|u| {
            u.query_pairs()
                .find_map(|(k, v)| if k == "key" && !v.is_empty() { Some(v.into_owned()) } else { None })
        })
        .or_else(|| bearer(req));

    // Bootstrap admin credential — constant-time compare, never in the DB.
    if let (Some(tok), Some(key)) = (token.clone(), worker_key(env)) {
        if cteq(tok.as_bytes(), key.as_bytes()) {
            return Ok(Ctx {
                engine,
                principal: Principal {
                    id: "worker".to_string(),
                    role: "admin".to_string(),
                    scope,
                    writer: None,
                },
            });
        }
    }

    let principal = engine
        .resolve_principal(token.as_deref(), scope.as_deref())
        .await
        .unwrap_or(Principal { id: "anon".to_string(), role: "none".to_string(), scope: None, writer: None });
    Ok(Ctx { engine, principal })
}

/// Request entry: query `?scope=` plus host bindings, in one call.
pub async fn ctx_for(
    req: &Request,
    rctx: &worker::RouteContext<()>,
) -> std::result::Result<Ctx, Response> {
    let scope = crate::query::params(req).get("scope").cloned();
    context(&rctx.env, req, scope).await
}

/// Host-only engine (scheduled/queue/DO): D1 + R2 + master key + Fetch
/// egress, no request principal involved.
pub async fn engine_for(env: &Env) -> std::result::Result<ServerlessEngine, String> {
    if let Ok(secret) = env.secret("SECRET_KEY") {
        let _ = engine::secrets::set_master_key(&secret.to_string());
    }
    let db = env.d1("DB").map_err(|e| format!("missing D1 binding DB: {e}"))?;
    let bucket = env.bucket("STORE").map_err(|e| format!("missing R2 binding STORE: {e}"))?;
    let engine = ServerlessEngine::new(Box::new(D1Db::new(db)), Box::new(R2Store::new(bucket)));
    engine.install_http_caller(Box::new(FetchCaller));
    Ok(engine)
}

pub fn require_read(p: &Principal) -> bool {
    matches!(p.role.as_str(), "reader" | "list" | "writer" | "admin" | "owner")
        || (p.role == "customer" && p.scope.is_some())
}

pub fn require_write(p: &Principal) -> bool {
    matches!(p.role.as_str(), "writer" | "admin" | "owner")
        || (p.role == "customer" && p.scope.is_some())
}

pub fn require_admin(p: &Principal) -> bool {
    matches!(p.role.as_str(), "admin" | "owner")
}

/// True when no agent bearer is configured: the management door (`/mcp`)
/// stays open without a token (dev convenience). Set `WORKER_KEY` to lock
/// it down — then a valid bearer (or writer key) is required.
pub fn management_open(env: &Env) -> bool {
    worker_key(env).is_none()
}

/// Read gate with the `public_reads` bypass. Resolves the tenant row.
pub async fn can_read(ctx: &mut Ctx) -> bool {
    if require_read(&ctx.principal) {
        return true;
    }
    ctx.engine.tenant().await.map(|t| t.public_reads).unwrap_or(false)
}

/// Per-table read gate (S1: P0 proposals). Precedence: admin/owner always;
/// `write_only` tables deny everyone else; table `public_read` overrides
/// the tenant flag; otherwise the tenant rule. Unknown tables fall back to
/// the tenant rule (callers 404 on missing tables first).
pub async fn can_table_read(ctx: &mut Ctx, table: &str) -> bool {
    let admin = matches!(ctx.principal.role.as_str(), "admin" | "owner");
    if admin {
        return true;
    }
    let reader = require_read(&ctx.principal);
    let cfg = ctx.engine.get_table(table).await.unwrap_or(None);
    match cfg {
        Some(c) => {
            if reader {
                // Keyed readers bypass write_only (admin-only reads still
                // apply below); writers/admins pass through require_*.
                if !c.write_only.unwrap_or(false) {
                    return true;
                }
                return false;
            }
            c.anon_read_open(ctx.engine.tenant().await.map(|t| t.public_reads).unwrap_or(false))
        }
        None => {
            if reader {
                true
            } else {
                ctx.engine.tenant().await.map(|t| t.public_reads).unwrap_or(false)
            }
        }
    }
}

/// Submit gate (S1): writers/admins always; `write_only` tables additionally
/// allow anonymous submit (the inbox shape: insert without list).
pub async fn can_table_submit(ctx: &Ctx, table: &str) -> bool {
    if require_write(&ctx.principal) {
        return true;
    }
    ctx.engine
        .get_table(table)
        .await
        .unwrap_or(None)
        .map(|c| c.anon_submit_open())
        .unwrap_or(false)
}

/// Customer-scoped ownership check against a record payload: scoped callers
/// only see their own rows (documented intent; the donor's helper for this
/// was dead code).
pub fn scope_ok(principal: &Principal, payload: &serde_json::Value) -> bool {
    let Some(scope) = principal.scope.as_deref() else {
        return true;
    };
    payload.get("customer_id").and_then(|v| v.as_str()) == Some(scope)
}

/// Customer scope as an extra query condition (donor parity).
pub fn scope_cond(principal: &Principal) -> Option<engine::storage::ir::FilterCond> {
    principal.scope.as_ref().map(|s| engine::storage::ir::FilterCond {
        field: "$.customer_id".to_string(),
        op: engine::storage::ir::Op::Eq,
        value: serde_json::Value::String(s.clone()),
    })
}

pub fn ok_or_500(r: Result<Response>) -> Response {
    r.unwrap_or_else(|_| cors::err(500, "response build failed"))
}
