use crate::model::{Job, JobRun, Key};
use crate::storage::database::{Database, Query, Row};
use crate::storage::ir::{FilterCond, Op, SrvFilter};
use crate::tables::{scoped_key, TABLE_JOBS, TABLE_JOB_RUNS};
use serde_json::{json, Value as Json};

fn board_cond(board_id: &str) -> FilterCond {
    FilterCond {
        field: "$.board_id".to_string(),
        op: Op::Eq,
        value: Json::String(board_id.to_string()),
    }
}

fn name_cond(name: &str) -> FilterCond {
    FilterCond {
        field: "$.name".to_string(),
        op: Op::Eq,
        value: Json::String(name.to_string()),
    }
}

fn job_key(board_id: &str, name: &str) -> Key {
    Key::text(scoped_key(board_id, name))
}

pub fn job_add(
    db: &mut dyn Database,
    board_id: &str,
    name: &str,
    schedule: &str,
    action: &Json,
) -> anyhow::Result<String> {
    let next = crate::cron::next_run(schedule, chrono::Utc::now().naive_utc())
        .ok_or_else(|| anyhow::anyhow!("invalid schedule '{schedule}'"))?;
    let next_iso = crate::cron::fmt_iso(next);
    let now = crate::crud::now_str();
    let data = json!({
        "board_id": board_id,
        "name": name,
        "schedule": schedule,
        "action": action,
        "next_run_at": next_iso,
        "created_at": now,
    });
    db.delete(
        TABLE_JOBS,
        &SrvFilter { conds: vec![board_cond(board_id), name_cond(name)] },
    )?;
    db.insert(TABLE_JOBS, Row::new(job_key(board_id, name), data))?;
    Ok(next_iso)
}

pub fn job_list(db: &dyn Database, board_id: &str) -> anyhow::Result<Vec<Job>> {
    let q = Query {
        filter: SrvFilter { conds: vec![board_cond(board_id)] },
        orders: vec![("$.created_at".to_string(), false)],
        limit: usize::MAX,
        offset: 0,
    
        ttl: None,
    };
    let mut out = Vec::new();
    for row in db.query(TABLE_JOBS, &q)?.rows {
        out.push(serde_json::from_value(row.data)?);
    }
    Ok(out)
}

pub fn job_get(db: &dyn Database, board_id: &str, name: &str) -> anyhow::Result<Option<Job>> {
    let key = job_key(board_id, name);
    let Some(row) = db.get(TABLE_JOBS, &key)? else {
        return Ok(None);
    };
    Ok(serde_json::from_value(row.data)?)
}

pub fn job_remove(db: &mut dyn Database, board_id: &str, name: &str) -> anyhow::Result<bool> {
    let n = db.delete(
        TABLE_JOBS,
        &SrvFilter { conds: vec![board_cond(board_id), name_cond(name)] },
    )?;
    Ok(n > 0)
}

pub fn job_due(db: &dyn Database, now_iso: &str, limit: usize) -> anyhow::Result<Vec<Job>> {
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
    for row in db.query(TABLE_JOBS, &q)?.rows {
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

pub fn job_reschedule(
    db: &mut dyn Database,
    board_id: &str,
    name: &str,
    next_iso: Option<&str>,
) -> anyhow::Result<()> {
    let key = job_key(board_id, name);
    let Some(row) = db.get(TABLE_JOBS, &key)? else {
        return Ok(());
    };
    let mut data = row.data;
    match next_iso {
        Some(n) => data["next_run_at"] = Json::String(n.to_string()),
        None => {
            data.as_object_mut().map(|m| m.remove("next_run_at"));
        }
    }
    db.update(TABLE_JOBS, &key, &data)?;
    Ok(())
}

pub fn job_mark(
    db: &mut dyn Database,
    board_id: &str,
    name: &str,
    last_run_at: &str,
    status: &str,
    message: &str,
) -> anyhow::Result<()> {
    let key = job_key(board_id, name);
    let Some(row) = db.get(TABLE_JOBS, &key)? else {
        return Ok(());
    };
    let mut data = row.data;
    data["last_run_at"] = Json::String(last_run_at.to_string());
    data["last_status"] = Json::String(status.to_string());
    data["last_message"] = Json::String(message.to_string());
    db.update(TABLE_JOBS, &key, &data)?;
    Ok(())
}

pub fn job_run_insert(
    db: &mut dyn Database,
    board_id: &str,
    job_name: &str,
    triggered_at: &str,
    duration_ms: i64,
    status: &str,
    message: &str,
    result: &str,
) -> anyhow::Result<()> {
    let data = json!({
        "board_id": board_id,
        "job_name": job_name,
        "triggered_at": triggered_at,
        "duration_ms": duration_ms,
        "status": status,
        "message": message,
        "result": result,
    });
    db.insert(TABLE_JOB_RUNS, Row::new(Key::Int(0), data))?;
    Ok(())
}

pub fn job_runs(
    db: &dyn Database,
    board_id: &str,
    job_name: Option<&str>,
    limit: usize,
) -> anyhow::Result<Vec<JobRun>> {
    let mut conds = vec![board_cond(board_id)];
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
    for row in db.query(TABLE_JOB_RUNS, &q)?.rows {
        out.push(serde_json::from_value(row.data)?);
    }
    Ok(out)
}