use super::{arg_i64, arg_str, ok};
use engine::model::Principal;
use engine::ServerlessEngine;
use serde_json::{json, Value as Json};

pub async fn add(
    engine: &mut ServerlessEngine,
    _principal: &Principal,
    arguments: &Json,
) -> Result<Json, String> {
    let name = arg_str(arguments, "name")?;
    let schedule = arg_str(arguments, "schedule")?;
    let action = arguments.get("action").cloned().unwrap_or(Json::Null);
    engine.add_job(name, schedule, &action).await
        .map_err(|e| e.to_string())?;
    ok(json!({ "name": name, "schedule": schedule }))
}

pub async fn list(
    engine: &ServerlessEngine,
    _principal: &Principal,
    _arguments: &Json,
) -> Result<Json, String> {
    let jobs = engine.list_jobs().await.map_err(|e| e.to_string())?;
    let out: Vec<Json> = jobs
        .into_iter()
        .map(|j| {
            json!({
                "name": j.name,
                "schedule": j.schedule,
                "next_run_at": j.next_run_at,
                "last_run_at": j.last_run_at,
                "last_status": j.last_status,
                "last_message": j.last_message,
            })
        })
        .collect();
    ok(json!({ "jobs": out }))
}

pub async fn show(
    engine: &ServerlessEngine,
    _principal: &Principal,
    arguments: &Json,
) -> Result<Json, String> {
    let name = arg_str(arguments, "name")?;
    let job = engine.get_job(name).await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("job '{name}' not found"))?;
    ok(json!({
        "name": job.name,
        "schedule": job.schedule,
        "action": job.action,
        "next_run_at": job.next_run_at,
        "last_run_at": job.last_run_at,
        "last_status": job.last_status,
        "last_message": job.last_message,
        "created_at": job.created_at,
    }))
}

pub async fn remove(
    engine: &mut ServerlessEngine,
    _principal: &Principal,
    arguments: &Json,
) -> Result<Json, String> {
    let name = arg_str(arguments, "name")?;
    let removed = engine.remove_job(name).await.map_err(|e| e.to_string())?;
    ok(json!({ "removed": removed }))
}

pub async fn runs(
    engine: &ServerlessEngine,
    _principal: &Principal,
    arguments: &Json,
) -> Result<Json, String> {
    let job = arguments.get("job").and_then(|j| j.as_str());
    let limit = arg_i64(arguments, "limit", 20).clamp(1, 500) as usize;
    let runs = engine.job_runs(job, limit).await
        .map_err(|e| e.to_string())?;
    let out: Vec<Json> = runs
        .into_iter()
        .map(|r| {
            json!({
                "job": r.job_name,
                "triggered_at": r.triggered_at,
                "duration_ms": r.duration_ms,
                "status": r.status,
                "message": r.message,
            })
        })
        .collect();
    ok(json!({ "runs": out }))
}