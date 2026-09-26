//! TenantDO — the single-tenant Durable Object.
//!
//! Exactly one instance (id from fixed name `"singleton"`). Holds, in DO
//! storage (durable, transactional per access):
//! - daily usage counters (`usage:{YYYY-MM-DD}`),
//! - fixed-window rate state (`rate:{key}:{action}`),
//! served over a tiny fetch API (`POST /usage`, `GET /usage`, …).
//! `POST /sweep` runs a TTL sweep plus a due-job census as a manual or
//! future-alarmed fallback for the Cron Trigger driver.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value as Json};
use worker::{durable_object, DurableObject, Env, Request, Response, Result, State};

use crate::cors;

#[durable_object]
pub struct TenantDO {
    state: State,
    env: Env,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct UsageDay {
    #[serde(default)]
    req: u64,
    #[serde(default)]
    reads: u64,
    #[serde(default)]
    writes: u64,
    #[serde(default)]
    errors: u64,
    #[serde(default)]
    routes: std::collections::HashMap<String, u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct RateWindow {
    #[serde(default)]
    action_count: u64,
    #[serde(default)]
    day_count: u64,
    #[serde(default)]
    window_start: u64,
}

fn today() -> String {
    chrono::Utc::now().format("%Y-%m-%d").to_string()
}

fn now_secs() -> u64 {
    worker::Date::now().as_millis().saturating_div(1000)
}

impl DurableObject for TenantDO {
    fn new(state: State, env: Env) -> Self {
        Self { state, env }
    }

    async fn fetch(&self, mut req: Request) -> Result<Response> {
        let path = req.path();
        let method = req.method().to_string();
        match (method.as_str(), path.as_str()) {
            ("POST", "/usage") => {
                let body: Json = req.json().await.unwrap_or(Json::Null);
                self.record_usage(&body).await
            }
            ("GET", "/usage") => self.read_usage().await,
            ("POST", "/rate/check") => {
                let body: Json = req.json().await.unwrap_or(Json::Null);
                self.rate_check(&body).await
            }
            ("POST", "/sweep") => self.sweep().await,
            _ => Ok(cors::err(404, "unknown tenant-do route")),
        }
    }
}

impl TenantDO {
    async fn record_usage(&self, body: &Json) -> Result<Response> {
        let storage = self.state.storage();
        let key = format!("usage:{}", today());
        let mut day: UsageDay = storage.get(&key).await?.unwrap_or_default();
        day.req += 1;
        day.reads += body.get("reads").and_then(|v| v.as_u64()).unwrap_or(0);
        day.writes += body.get("writes").and_then(|v| v.as_u64()).unwrap_or(0);
        day.errors += body.get("errors").and_then(|v| v.as_u64()).unwrap_or(0);
        if let Some(route) = body.get("route").and_then(|v| v.as_str()) {
            *day.routes.entry(route.to_string()).or_insert(0) += 1;
        }
        storage.put(&key, &day).await?;
        Ok(cors::ok(json!({ "ok": true })))
    }

    async fn read_usage(&self) -> Result<Response> {
        let storage = self.state.storage();
        let key = format!("usage:{}", today());
        let day: UsageDay = storage.get(&key).await?.unwrap_or_default();
        Ok(cors::ok(json!({ "ok": true, "day": today(), "usage": day })))
    }

    async fn rate_check(&self, body: &Json) -> Result<Response> {
        let key = body.get("key").and_then(|v| v.as_str()).unwrap_or("anon").to_string();
        let action = body.get("action").and_then(|v| v.as_str()).unwrap_or("read").to_string();
        let action_limit = body.get("action_limit").and_then(|v| v.as_u64()).unwrap_or(600);
        let day_limit = body.get("per_day").and_then(|v| v.as_u64()).unwrap_or(100_000);
        let now = now_secs();
        let storage = self.state.storage();
        let skey = format!("rate:{key}:{action}");
        let mut w: RateWindow = storage.get(&skey).await?.unwrap_or_default();
        if now.saturating_sub(w.window_start) >= 86_400 {
            w = RateWindow { action_count: 0, day_count: 0, window_start: now };
        } else if now.saturating_sub(w.window_start) >= 60 {
            w.action_count = 0;
        }
        // First touch initializes the window start.
        if w.window_start == 0 {
            w.window_start = now;
        }
        w.action_count += 1;
        w.day_count += 1;
        storage.put(&skey, &w).await?;
        if w.action_count > action_limit || w.day_count > day_limit {
            let retry_after = 60u64.saturating_sub(now.saturating_sub(w.window_start));
            return Ok(cors::json(429, json!({ "error": "rate limited", "retry_after": retry_after })));
        }
        Ok(cors::ok(json!({ "ok": true, "remaining": action_limit.saturating_sub(w.action_count) })))
    }

    /// Manual/fallback sweep: TTL expiry plus a due-job census (execution
    /// stays with the Cron Trigger runner in `scheduled.rs`).
    async fn sweep(&self) -> Result<Response> {
        let mut engine = match crate::auth::engine_for(&self.env).await {
            Ok(e) => e,
            Err(e) => return Ok(cors::err(500, &e)),
        };
        let swept = engine.ttl_sweep().await.unwrap_or(0);
        let now = engine::crud::now_str();
        let due = engine.job_due(&now, 32).await.map(|jobs| jobs.len()).unwrap_or(0);
        Ok(cors::ok(json!({ "ok": true, "swept": swept, "jobs_due": due })))
    }
}
