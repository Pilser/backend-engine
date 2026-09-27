use crate::model::{Job, JobRun, Key};
use crate::storage::database::{Database, Query, Row};
use crate::storage::ir::{FilterCond, Op, SrvFilter};
use crate::tables::{tenant_key, TABLE_JOBS, TABLE_JOB_RUNS};
use serde_json::{json, Value as Json};

fn name_cond(name: &str) -> FilterCond {
    FilterCond {
        field: "$.name".to_string(),
        op: Op::Eq,
        value: Json::String(name.to_string()),
    }
}

fn job_key(name: &str) -> Key {
    Key::text(tenant_key(name))
}

pub async fn job_add(
    db: &mut dyn Database,
    name: &str,
    schedule: &str,
    action: &Json,
) -> anyhow::Result<String> {
    let next = crate::cron::next_run(schedule, chrono::Utc::now().naive_utc())
        .ok_or_else(|| anyhow::anyhow!("invalid schedule '{schedule}'"))?;
    let next_iso = crate::cron::fmt_iso(next);
    let now = crate::crud::now_str();
    let data = json!({
        "name": name,
        "schedule": schedule,
        "action": action,
        "next_run_at": next_iso,
        "created_at": now,
    });
    db.delete(
        TABLE_JOBS,
        &SrvFilter { conds: vec![name_cond(name)] },
    ).await?;
    db.insert(TABLE_JOBS, Row::new(job_key(name), data)).await?;
    Ok(next_iso)
}

pub async fn job_list(db: &dyn Database) -> anyhow::Result<Vec<Job>> {
    let q = Query {
        filter: SrvFilter { conds: Vec::new() },
        orders: vec![("$.created_at".to_string(), false)],
        limit: usize::MAX,
        offset: 0,
    
        ttl: None,
    };
    let mut out = Vec::new();
    for row in db.query(TABLE_JOBS, &q).await?.rows {
        out.push(serde_json::from_value(row.data)?);
    }
    Ok(out)
}

pub async fn job_get(db: &dyn Database, name: &str) -> anyhow::Result<Option<Job>> {
    let key = job_key(name);
    let Some(row) = db.get(TABLE_JOBS, &key).await? else {
        return Ok(None);
    };
    Ok(serde_json::from_value(row.data)?)
}

pub async fn job_remove(db: &mut dyn Database, name: &str) -> anyhow::Result<bool> {
    let n = db.delete(
        TABLE_JOBS,
        &SrvFilter { conds: vec![name_cond(name)] },
    ).await?;
    Ok(n > 0)
}

pub async fn job_due(db: &dyn Database, now_iso: &str, limit: usize) -> anyhow::Result<Vec<Job>> {
    let q = Query {
        filter: SrvFilter {
            conds: vec![FilterCond {
                field: "$.next_run_at".to_string(),
                op: Op::Lte,
                value: Json::String(now_iso.to_string()),
            }],
        },
        orders: vec![("$.next_run_at".to_string(), true)],
        limit: limit * 4,
        offset: 0,
        ttl: None,
    };
    let mut out = Vec::new();
    for row in db.query(TABLE_JOBS, &q).await?.rows {
        if out.len() >= limit {
            break;
        }
        let job: Job = serde_json::from_value(row.data)?;
        match job.next_run_at.as_deref() {
            Some(next) if next <= now_iso => out.push(job),
            _ => {}
        }
    }
    Ok(out)
}

pub async fn job_reschedule(
    db: &mut dyn Database,
    name: &str,
    next_iso: Option<&str>,
) -> anyhow::Result<()> {
    let key = job_key(name);
    let Some(row) = db.get(TABLE_JOBS, &key).await? else {
        return Ok(());
    };
    let mut data = row.data;
    match next_iso {
        Some(n) => data["next_run_at"] = Json::String(n.to_string()),
        None => {
            data.as_object_mut().map(|m| m.remove("next_run_at"));
        }
    }
    db.update(TABLE_JOBS, &key, &data).await?;
    Ok(())
}

pub async fn job_mark(
    db: &mut dyn Database,
    name: &str,
    last_run_at: &str,
    status: &str,
    message: &str,
) -> anyhow::Result<()> {
    let key = job_key(name);
    let Some(row) = db.get(TABLE_JOBS, &key).await? else {
        return Ok(());
    };
    let mut data = row.data;
    data["last_run_at"] = Json::String(last_run_at.to_string());
    data["last_status"] = Json::String(status.to_string());
    data["last_message"] = Json::String(message.to_string());
    db.update(TABLE_JOBS, &key, &data).await?;
    Ok(())
}

pub async fn job_run_insert(
    db: &mut dyn Database,
    job_name: &str,
    triggered_at: &str,
    duration_ms: i64,
    status: &str,
    message: &str,
    result: &str,
) -> anyhow::Result<()> {
    let data = json!({
        "job_name": job_name,
        "triggered_at": triggered_at,
        "duration_ms": duration_ms,
        "status": status,
        "message": message,
        "result": result,
    });
    // Unique key per run: a constant key here meant every run overwrote the
    // last (same shared-table REPLACE collision as records had).
    let key = format!("{job_name}@{triggered_at}@{}", &uuid::Uuid::new_v4().to_string()[..8]);
    db.insert(TABLE_JOB_RUNS, Row::new(Key::Text(key), data)).await?;
    Ok(())
}

pub async fn job_runs(
    db: &dyn Database,
    job_name: Option<&str>,
    limit: usize,
) -> anyhow::Result<Vec<JobRun>> {
    let mut conds = Vec::new();
    if let Some(n) = job_name {
        conds.push(FilterCond {
            field: "$.job_name".to_string(),
            op: Op::Eq,
            value: Json::String(n.to_string()),
        });
    }
    let q = Query {
        filter: SrvFilter { conds },
        orders: vec![("$.triggered_at".to_string(), true)],
        limit: limit.clamp(1, 200),
        offset: 0,
    
        ttl: None,
    };
    let mut out = Vec::new();
    for row in db.query(TABLE_JOB_RUNS, &q).await?.rows {
        out.push(serde_json::from_value(row.data)?);
    }
    Ok(out)
}