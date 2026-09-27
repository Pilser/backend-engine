//! Cron Trigger body: due jobs, TTL sweep, webhook flush.
//!
//! Ports the donor `jobs.rs` loops (`spawn_scheduler` 15s batch 32,
//! `ttl_sweep`, `spawn_webhook_worker` 2s batch 64 — cadence now comes from
//! `wrangler.toml [triggers] crons`). Webhook deliveries are claimed and
//! handed to the `WEBHOOKS` queue when bound, otherwise delivered inline.

use engine::{model::Principal, ServerlessEngine};
use serde_json::Value as Json;
use worker::Env;

use crate::queue;

const JOB_BATCH: usize = 32;
const DELIVERY_BATCH: usize = 64;

fn now_str() -> String {
    engine::crud::now_str()
}

pub async fn run(env: &Env) {
    let mut engine = match crate::auth::engine_for(env).await {
        Ok(e) => e,
        Err(_) => return,
    };
    let now = now_str();
    // 1. Due cron jobs (reschedule-first, then phased recipes + action).
    for job in engine.job_due(&now, JOB_BATCH).await.unwrap_or_default() {
        run_job(&mut engine, job).await;
    }
    // 2. TTL sweep.
    let _ = engine.ttl_sweep().await;
    // 3. Webhook flush: claim due rows and hand them to the queue;
    //    without a queue binding, deliver inline (best effort).
    let due = engine.hook_deliveries_due(&now, DELIVERY_BATCH).await.unwrap_or_default();
    if due.is_empty() {
        return;
    }
    if let Ok(q) = env.queue("WEBHOOKS") {
        for d in &due {
            claim(&mut engine, d, &now).await;
            let _ = queue::send_delivery(&q, d).await;
        }
    } else {
        let secrets = load_hook_secrets(&mut engine).await;
        for d in &due {
            // Inline fallback: a failed inline delivery is re-marked with
            // backoff inside deliver_one; a row-level error just skips.
            let _ = queue::deliver_one(&mut engine, &secrets, d, &now).await;
        }
    }
}

/// Mark a delivery as claimed for ~5 minutes so a concurrent scheduler tick
/// does not enqueue it twice. The queue consumer re-marks on completion; if
/// it never runs, the row becomes due again (self-healing).
async fn claim(engine: &mut ServerlessEngine, d: &Json, now: &str) {
    let id = d.get("id").and_then(|v| v.as_str()).unwrap_or("");
    let attempts = d.get("attempts").and_then(|v| v.as_i64()).unwrap_or(0);
    let status = d.get("last_status").and_then(|v| v.as_str()).map(String::from);
    if id.is_empty() {
        return;
    }
    let next = queue::next_attempt_iso_clamped(300, now);
    let _ = engine
        .mark_hook_delivery(id, attempts, status.as_deref(), next.as_deref(), None)
        .await;
}

async fn load_hook_secrets(engine: &mut ServerlessEngine) -> std::collections::HashMap<String, String> {
    engine
        .list_hooks()
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|h| (h.url, h.secret.unwrap_or_default()))
        .collect()
}

fn extract_path<'a>(value: &'a Json, path: &str) -> Option<&'a Json> {
    let p = path.trim().trim_start_matches('$');
    let p = p.strip_prefix('.').unwrap_or(p);
    if p.is_empty() {
        return Some(value);
    }
    let mut cur = value;
    for seg in p.split('.') {
        let key = seg.trim_matches('[').trim_matches(']');
        if key.is_empty() {
            continue;
        }
        if let Some(idx) = key.parse::<usize>().ok() {
            cur = cur.get(idx)?;
        } else {
            cur = cur.get(key)?;
        }
    }
    Some(cur)
}

async fn run_job(engine: &mut ServerlessEngine, job: engine::model::Job) {
    let now = now_str();
    let started_ms = worker::Date::now().as_millis();
    let next = engine::cron::next_run(&job.schedule, chrono::Utc::now().naive_utc())
        .map(|dt| engine::cron::fmt_iso(dt));
    // Reschedule FIRST so a crash cannot miss the run (donor parity).
    let _ = engine.job_reschedule(&job.name, next.as_deref()).await;
    // Phase A: cron recipes' DB-side actions; deferred $calls collected.
    let mut outcome =
        engine::automation::DispatchOutcome { pending: Vec::new(), writebacks: Vec::new() };
    if engine.dispatch_cron_job_phased_a(&job.name, &mut outcome).await.is_err() {
        // Dispatch failure is recorded below via the job action path.
    }
    if !outcome.pending.is_empty() {
        let mut working = Json::Null;
        let mut logs = Vec::new();
        engine::automation::execute_pending(outcome.pending, &mut working, &mut logs).await;
        let _ = logs;
    }

    // Job action: optional outbound HTTP, optionally polling into a table.
    let action = job.action.clone();
    let mut status = "ok";
    let message = match action.get("url").and_then(|u| u.as_str()) {
        Some(url) => {
            if !engine::automation::valid_url(url) {
                status = "error";
                "ssrf-blocked url".to_string()
            } else {
                let kind = action.get("type").and_then(|t| t.as_str()).unwrap_or("http");
                let timeout = action.get("timeout_ms").and_then(|t| t.as_u64()).unwrap_or(15_000);
                let mut headers: Vec<(String, String)> = Vec::new();
                if let Some(h) = action.get("headers") {
                    if let Some(o) = h.as_object() {
                        for (k, v) in o {
                            headers.push((k.clone(), v.as_str().unwrap_or("").to_string()));
                        }
                    } else if let Some(arr) = h.as_array() {
                        for el in arr {
                            if let Some(pair) = el.as_array() {
                                if pair.len() == 2 {
                                    headers.push((
                                        pair[0].as_str().unwrap_or("").to_string(),
                                        pair[1].as_str().unwrap_or("").to_string(),
                                    ));
                                }
                            }
                        }
                    }
                }
                let body = action.get("body").cloned().unwrap_or(Json::Null);
                match engine::http::http_call(url, &headers, &body, timeout).await {
                    Ok((code, res)) => {
                        if kind == "poll" {
                            let path = action.get("path").and_then(|p| p.as_str()).unwrap_or("$");
                            let record = extract_path(&res, path).cloned().unwrap_or(Json::Null);
                            if record.is_object() {
                                let principal = Principal {
                                    id: format!("job:{}", job.name),
                                    role: "writer".to_string(),
                                    scope: None,
                                    writer: Some(format!("job:{}", job.name)),
                                    tables: None,
                                };
                                match engine
                                    .insert_record(
                                        action.get("table").and_then(|t| t.as_str()).unwrap_or("records"),
                                        record,
                                        Some(&format!("job:{}", job.name)),
                                        false,
                                        &principal,
                                    )
                                    .await
                                {
                                    Ok(seq) => format!("http {code}, inserted seq {seq}"),
                                    Err(err) => {
                                        status = "error";
                                        format!("http {code}, insert failed: {err}")
                                    }
                                }
                            } else {
                                format!("http {code}, path '{path}' did not yield an object")
                            }
                        } else {
                            format!("http {code}")
                        }
                    }
                    Err(err) => {
                        status = "error";
                        format!("request failed: {err}")
                    }
                }
            }
        }
        None => "cron-only job (no http action)".to_string(),
    };

    let duration_ms = worker::Date::now().as_millis().saturating_sub(started_ms) as i64;
    let _ = engine.job_mark(&job.name, &now, status, &message).await;
    let _ = engine.job_run_insert(&job.name, &now, duration_ms, status, &message, "").await;
}
