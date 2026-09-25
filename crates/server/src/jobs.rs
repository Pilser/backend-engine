use engine::model::{Job, Principal};
use engine::ServerlessEngine;
use serde_json::Value as Json;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn fmt_now() -> String {
    chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

fn parse_now(now: &str) -> Option<chrono::NaiveDateTime> {
    chrono::NaiveDateTime::parse_from_str(now, "%Y-%m-%d %H:%M:%S").ok()
}

pub fn spawn_ttl_sweeper(engine: Arc<Mutex<ServerlessEngine>>) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;
            // ttl_sweep scans the whole store via the blocking Helix client;
            // run it on the blocking pool, never on this async task.
            let e = engine.clone();
            let _ = tokio::task::spawn_blocking(move || {
                if let Ok(mut e) = e.lock() {
                    if let Err(err) = e.ttl_sweep() {
                        tracing::warn!("ttl_sweep error: {err}");
                    }
                }
            })
            .await;
        }
    });
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

const JOB_BATCH: usize = 32;

pub fn spawn_scheduler(engine: Arc<Mutex<ServerlessEngine>>) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(15)).await;
            let now = fmt_now();
            // job_due scans the whole store via the blocking Helix client;
            // run the scan on the blocking pool, never on this async task.
            let due = {
                let e = engine.clone();
                let now2 = now.clone();
                match tokio::task::spawn_blocking(move || {
                    e.lock().ok().and_then(|e| e.job_due(&now2, JOB_BATCH).ok())
                })
                .await
                {
                    Ok(Some(d)) => d,
                    Ok(None) => continue,
                    Err(err) => {
                        tracing::warn!("scheduler: due query failed: {err}");
                        continue;
                    }
                }
            };
            for job in due {
                run_job(engine.clone(), job).await;
            }
        }
    });
}

async fn run_job(engine: Arc<Mutex<ServerlessEngine>>, job: Job) {
    // The job body (engine lock + blocking http_call + insert) uses the
    // blocking reqwest client; run it on the blocking thread pool, never on an
    // async worker (stalls after ~128 requests otherwise).
    let engine2 = engine.clone();
    let job2 = job.clone();
    let _ = tokio::task::spawn_blocking(move || run_job_blocking(engine2, job2)).await;
}

fn run_job_blocking(engine: Arc<Mutex<ServerlessEngine>>, job: Job) {
    let now = fmt_now();
    let started = std::time::Instant::now();
    let next = engine::cron::next_run(&job.schedule, chrono::Utc::now().naive_utc())
        .map(|dt| engine::cron::fmt_iso(dt));
    // Phase A (locked): reschedule + run cron recipes' DB-side actions.
    // $call HTTP is collected, NOT executed — the network I/O happens AFTER
    // this lock scope ends, so a slow webhook target can no longer freeze
    // every other daemon request while a cron recipe runs.
    let mut outcome = engine::automation::DispatchOutcome {
        pending: Vec::new(),
        writebacks: Vec::new(),
    };
    {
        let mut e = engine.lock().unwrap();
        let _ = e.job_reschedule(&job.board_id, &job.name, next.as_deref());
        if let Err(err) = e.dispatch_cron_job_phased_a(&job.board_id, &job.name, &mut outcome) {
            tracing::warn!("cron {}: phased dispatch failed: {err}", job.name);
        }
    }
    if !outcome.pending.is_empty() {
        let mut working = Json::Null;
        let mut logs = Vec::new();
        engine::automation::execute_pending(outcome.pending, &mut working, &mut logs);
        for l in &logs {
            eprintln!("[recipe cron:{}] {l}", job.name);
        }
    }

    let action = job.action.clone();
    let mut status = "ok";
    let message = {
        let url = action.get("url").and_then(|u| u.as_str());
        if let Some(url) = url {
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
                match engine::http::http_call(url, &headers, &body, timeout) {
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
                                };
                                let result = engine.lock().unwrap().insert_record(
                                    &job.board_id,
                                    action.get("table").and_then(|t| t.as_str()).unwrap_or("records"),
                                    record,
                                    Some(&format!("job:{}", job.name)),
                                    false,
                                    &principal,
                                );
                                match result {
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
        } else {
            "cron-only job (no http action)".to_string()
        }
    };

    let duration_ms = started.elapsed().as_millis() as i64;
    let mut e = engine.lock().unwrap();
    let _ = e.job_mark(&job.board_id, &job.name, &now, status, &message);
    let _ = e.job_run_insert(&job.board_id, &job.name, &now, duration_ms, status, &message, "");
}

const BACKOFF: [u64; 5] = [0, 1, 5, 30, 120];
const MAX_ATTEMPTS: i64 = 5;
const DELIVERY_BATCH: usize = 64;

fn next_attempt_iso(attempts: i64, now: &str) -> Option<String> {
    if attempts + 1 > MAX_ATTEMPTS {
        return None;
    }
    let backoff = BACKOFF[(attempts.min(4)) as usize];
    let dt = parse_now(now)?;
    Some(
        (dt + chrono::Duration::seconds(backoff as i64))
            .format("%Y-%m-%d %H:%M:%S")
            .to_string(),
    )
}

pub fn spawn_webhook_worker(engine: Arc<Mutex<ServerlessEngine>>) {
    tokio::spawn(async move {
        let client = match reqwest::Client::builder().build() {
            Ok(c) => c,
            Err(err) => {
                tracing::error!("webhook worker: failed to build http client: {err}");
                return;
            }
        };
        let mut secrets: HashMap<String, HashMap<String, String>> = HashMap::new();
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;
            let now = fmt_now();
            // hook_deliveries_due scans the whole store via the blocking
            // Helix client; run the scan on the blocking pool.
            let due = {
                let e = engine.clone();
                let now2 = now.clone();
                match tokio::task::spawn_blocking(move || {
                    e.lock().ok().and_then(|e| e.hook_deliveries_due(&now2, DELIVERY_BATCH).ok())
                })
                .await
                {
                    Ok(Some(d)) => d,
                    Ok(None) => continue,
                    Err(err) => {
                        tracing::warn!("webhook worker: due query failed: {err}");
                        continue;
                    }
                }
            };
            for d in due {
                deliver(&engine, &client, &mut secrets, &d, &now).await;
            }
        }
    });
}

async fn deliver(
    engine: &Arc<Mutex<ServerlessEngine>>,
    client: &reqwest::Client,
    secrets: &mut HashMap<String, HashMap<String, String>>,
    d: &Json,
    now: &str,
) {
    let id = d.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let board_id = d.get("board_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let url = d.get("url").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let attempts = d.get("attempts").and_then(|v| v.as_i64()).unwrap_or(0);
    if id.is_empty() || url.is_empty() {
        return;
    }
    if !engine::automation::valid_url(&url) {
        let _ = engine.lock().unwrap().mark_hook_delivery(&id, attempts + 1, Some("blocked"), None, None);
        tracing::warn!("webhook worker: blocked SSRF target {url}");
        return;
    }
    if !secrets.contains_key(&board_id) {
        let map: HashMap<String, String> = engine
            .lock()
            .unwrap()
            .list_hooks(&board_id)
            .unwrap_or_default()
            .into_iter()
            .map(|h| (h.url, h.secret.unwrap_or_default()))
            .collect();
        secrets.insert(board_id.clone(), map);
    }
    let secret = secrets
        .get(&board_id)
        .and_then(|m| m.get(&url))
        .cloned()
        .unwrap_or_default();
    let mut body: Json = d.get("payload").cloned().unwrap_or(Json::Null);
    body["hooks_secret"] = Json::String(secret.clone());
    let body_str = body.to_string();
    let sig = engine::webhooks::hmac_base64(&body_str, &secret);
    let result = client
        .post(&url)
        .header("X-Srv-Signature", sig)
        .body(body_str)
        .timeout(Duration::from_secs(10))
        .send()
        .await;
    match result {
        Ok(resp) if resp.status().is_success() => {
            let status = resp.status().as_u16().to_string();
            let _ = engine.lock().unwrap().mark_hook_delivery(&id, attempts + 1, Some(&status), None, Some(now));
        }
        Ok(resp) => {
            let status = resp.status().as_u16().to_string();
            let next = next_attempt_iso(attempts, now);
            let _ = engine.lock().unwrap().mark_hook_delivery(&id, attempts + 1, Some(&status), next.as_deref(), None);
        }
        Err(err) => {
            tracing::warn!("webhook worker: POST {url} failed: {err}");
            let next = next_attempt_iso(attempts, now);
            let _ = engine.lock().unwrap().mark_hook_delivery(&id, attempts + 1, Some("http-error"), next.as_deref(), None);
        }
    }
}