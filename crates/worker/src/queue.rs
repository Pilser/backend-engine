//! Webhook delivery pipeline: scheduler claims due rows and enqueues them;
//! the queue consumer delivers with per-hook HMAC signatures and the donor
//! backoff schedule (`[0, 1, 5, 30, 120]`s, max 5 attempts). Without a queue
//! binding the scheduler delivers inline instead.

use engine::http::{HttpBody, HttpCaller};
use engine::ServerlessEngine;
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use worker::{Env, MessageExt, Queue};

use crate::http_caller::FetchCaller;

const BACKOFF: [u64; 5] = [0, 1, 5, 30, 120];
const MAX_ATTEMPTS: i64 = 5;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeliveryMsg {
    pub delivery: Json,
}

pub fn next_attempt_iso(attempts: i64, now: &str) -> Option<String> {
    if attempts + 1 > MAX_ATTEMPTS {
        return None;
    }
    let backoff = BACKOFF[(attempts.min(4)) as usize];
    shift_secs(now, backoff)
}

/// Fixed-offset variant used when claiming rows for the queue.
pub fn next_attempt_iso_clamped(secs: u64, now: &str) -> Option<String> {
    shift_secs(now, secs)
}

fn shift_secs(now: &str, secs: u64) -> Option<String> {
    let dt = chrono::NaiveDateTime::parse_from_str(now, "%Y-%m-%d %H:%M:%S").ok()?;
    Some(
        (dt + chrono::Duration::seconds(secs as i64))
            .format("%Y-%m-%d %H:%M:%S")
            .to_string(),
    )
}

pub async fn send_delivery(q: &Queue, d: &Json) -> worker::Result<()> {
    q.send(DeliveryMsg { delivery: d.clone() }).await
}

pub async fn consume(env: &Env, batch: worker::MessageBatch<DeliveryMsg>) -> worker::Result<()> {
    let mut engine = crate::auth::engine_for(env)
        .await
        .map_err(worker::Error::RustError)?;
    let secrets = engine
        .list_hooks()
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|h| (h.url, h.secret.unwrap_or_default()))
        .collect::<std::collections::HashMap<String, String>>();
    let now = engine::crud::now_str();
    for msg in batch.messages()? {
        let body = msg.body().clone();
        match deliver_one(&mut engine, &secrets, &body.delivery, &now).await {
            Ok(()) => msg.ack(),
            Err(_) => msg.retry(),
        }
    }
    Ok(())
}

/// Deliver one webhook. Never panics; row bookkeeping errors propagate so
/// the queue redelivers (at-least-once).
pub async fn deliver_one(
    engine: &mut ServerlessEngine,
    secrets: &std::collections::HashMap<String, String>,
    d: &Json,
    now: &str,
) -> anyhow::Result<()> {
    let id = d.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let url = d.get("url").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let attempts = d.get("attempts").and_then(|v| v.as_i64()).unwrap_or(0);
    if id.is_empty() || url.is_empty() {
        return Ok(());
    }
    if !engine::automation::valid_url(&url) {
        engine.mark_hook_delivery(&id, attempts + 1, Some("blocked"), None, None).await?;
        return Ok(());
    }
    let secret = secrets.get(&url).cloned().unwrap_or_default();
    let mut body: Json = d.get("payload").cloned().unwrap_or(Json::Null);
    if let Some(obj) = body.as_object_mut() {
        obj.insert("hooks_secret".to_string(), Json::String(secret.clone()));
    }
    let body_str = body.to_string();
    let sig = engine::webhooks::hmac_base64(&body_str, &secret);
    let caller = FetchCaller;
    match caller
        .call(&url, &[("X-Srv-Signature".to_string(), sig)], &HttpBody::Json(body), 10_000)
        .await
    {
        Ok((code, _)) if (200..300).contains(&code) => {
            engine
                .mark_hook_delivery(&id, attempts + 1, Some(&code.to_string()), None, Some(now))
                .await?;
        }
        Ok((code, _)) => {
            let next = next_attempt_iso(attempts, now);
            engine
                .mark_hook_delivery(&id, attempts + 1, Some(&code.to_string()), next.as_deref(), None)
                .await?;
        }
        Err(_) => {
            let next = next_attempt_iso(attempts, now);
            engine
                .mark_hook_delivery(&id, attempts + 1, Some("http-error"), next.as_deref(), None)
                .await?;
        }
    }
    Ok(())
}
