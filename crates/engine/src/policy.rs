use crate::model::{Key, Link, Record, TableConfig};
use crate::storage::database::{Database, Query, Row};
use crate::storage::ir::{FilterCond, Op, SrvFilter};
use crate::tables::{tenant_key, TABLE_LINKS, TABLE_RECORDS, TABLE_TABLES};
use serde_json::{json, Value as Json};
use std::collections::HashMap;
use std::sync::Mutex;

const ACTION_WINDOW_SECS: u64 = 60;
const DAY_WINDOW_SECS: u64 = 86400;
const MAX_ENTRIES: usize = 10_000;

async fn save_table(db: &mut dyn Database, cfg: &TableConfig) -> anyhow::Result<()> {
    db.update(TABLE_TABLES, &Key::text(tenant_key(&cfg.table)), &serde_json::to_value(cfg)?).await?;
    Ok(())
}

pub async fn rate_set(db: &mut dyn Database, rate: &Json) -> anyhow::Result<()> {
    let mut board = crate::crud::tenant_config(db).await?;
    board.rate_json = Some(rate.clone());
    crate::crud::save_tenant(db, &board).await
}

pub async fn ttl_set(db: &mut dyn Database, table: &str, seconds: Option<i64>, field: Option<&str>) -> anyhow::Result<()> {
    let mut cfg = crate::crud::load_table(db, table).await?;
    cfg.ttl_seconds = seconds;
    cfg.ttl_field = field.map(String::from).filter(|f| !f.is_empty());
    save_table(db, &cfg).await
}

pub async fn ttl_clear(db: &mut dyn Database, table: &str) -> anyhow::Result<()> {
    let mut cfg = crate::crud::load_table(db, table).await?;
    cfg.ttl_seconds = None;
    cfg.ttl_field = None;
    save_table(db, &cfg).await
}

pub async fn ttl_sweep(db: &mut dyn Database) -> anyhow::Result<usize> {
    let now = crate::crud::now_str();
    let mut total = 0usize;
    for cfg in crate::crud::table_list(db).await? {
        if cfg.ttl_seconds.is_none() && cfg.ttl_field.is_none() {
            continue;
        }
        let q = Query {
            filter: SrvFilter {
                conds: vec![crate::crud::table_cond(&cfg.table)],
            },
            orders: vec![],
            limit: usize::MAX,
            offset: 0,
            ttl: None,
        };
        let mut dead: Vec<i64> = Vec::new();
        for row in db.query(TABLE_RECORDS, &q).await?.rows {
            let rec: Record = serde_json::from_value(row.data)?;
            let created = rec.created_at.as_deref().unwrap_or("");
            if crate::crud::is_ttl_dead(&cfg, created, &rec.payload, &now) {
                dead.push(rec.seq);
            }
        }
        if dead.is_empty() {
            continue;
        }
        let filter = SrvFilter {
            conds: vec![
                crate::crud::table_cond(&cfg.table),
                FilterCond { field: "$.seq".to_string(), op: Op::In, value: Json::Array(dead.into_iter().map(Json::from).collect()) },
            ],
        };
        total += db.delete(TABLE_RECORDS, &filter).await?;
    }
    Ok(total)
}

pub async fn link_set(db: &mut dyn Database, child_table: &str, parent_table: &str, from_key: &str, parent_key: &str) -> anyhow::Result<()> {
    let link = Link {
        child_board: crate::TENANT.to_string(),
        child_table: child_table.to_string(),
        parent_board: crate::TENANT.to_string(),
        parent_table: parent_table.to_string(),
        from_key: crate::storage::ir::normalize_path(from_key),
        parent_key: crate::storage::ir::normalize_path(parent_key),
    };
    db.upsert(TABLE_LINKS, "$.child_board", Row::new(Key::text(crate::TENANT), serde_json::to_value(&link)?)).await?;
    Ok(())
}

/// Link rows carry `child_board` (always TENANT), not a `$.board_id` payload
/// field — scope by it. (The old `$.board_id` filter never matched, which
/// silently disabled joins; fixed in the single-tenant collapse.)
fn link_tenant_cond() -> FilterCond {
    FilterCond {
        field: "$.child_board".to_string(),
        op: Op::Eq,
        value: Json::String(crate::TENANT.to_string()),
    }
}

pub async fn get_link(db: &dyn Database) -> anyhow::Result<Option<Link>> {
    let q = Query { filter: SrvFilter { conds: vec![link_tenant_cond()] }, orders: vec![], limit: 1, offset: 0, ttl: None };
    match db.query(TABLE_LINKS, &q).await?.rows.into_iter().next() {
        Some(row) => Ok(Some(serde_json::from_value(row.data)?)),
        None => Ok(None),
    }
}

pub async fn link_list(db: &dyn Database) -> anyhow::Result<Vec<Link>> {
    let q = Query { filter: SrvFilter { conds: vec![link_tenant_cond()] }, orders: vec![], limit: usize::MAX, offset: 0, ttl: None };
    let mut out = Vec::new();
    for row in db.query(TABLE_LINKS, &q).await?.rows {
        out.push(serde_json::from_value(row.data)?);
    }
    Ok(out)
}

pub async fn link_clear(db: &mut dyn Database) -> anyhow::Result<()> {
    let removed = db.delete(TABLE_LINKS, &SrvFilter { conds: vec![link_tenant_cond()] }).await?;
    if removed == 0 {
        anyhow::bail!("no link configured");
    }
    Ok(())
}

pub async fn join_list(
    db: &dyn Database,
    child_table: &str,
    conds: &SrvFilter,
    limit: usize,
    offset: usize,
) -> anyhow::Result<Vec<Record>> {
    let mut records = crate::query::query_records(db, child_table, conds, &[], limit, offset).await?;
    let Some(link) = get_link(db).await? else {
        return Ok(records);
    };
    let mut from_values: Vec<Json> = Vec::new();
    for rec in &records {
        let v = crate::expr::get_path(&rec.payload, &link.from_key);
        if !v.is_null() && !v.is_array() && !v.is_object() && !from_values.contains(&v) {
            from_values.push(v);
        }
    }
    let mut parents: HashMap<String, Json> = HashMap::new();
    if !from_values.is_empty() {
        let filter = SrvFilter {
            conds: vec![
                FilterCond { field: link.parent_key.clone(), op: Op::In, value: Json::Array(from_values) },
            ],
        };
        for rec in crate::query::query_records(db, &link.parent_table, &filter, &[], usize::MAX, 0).await? {
            let v = crate::expr::get_path(&rec.payload, &link.parent_key);
            if !v.is_null() {
                parents.insert(crate::storage::ir::scalar_text(&v), rec.payload);
            }
        }
    }
    for rec in &mut records {
        let key = crate::expr::get_path(&rec.payload, &link.from_key);
        let joined = if key.is_null() { None } else { parents.get(&crate::storage::ir::scalar_text(&key)).cloned() };
        let mut joined_payload = serde_json::Map::new();
        joined_payload.insert(link.from_key.clone(), joined.unwrap_or(Json::Null));
        rec.payload = Json::Object(joined_payload);
    }
    Ok(records)
}

#[derive(Debug, Clone, PartialEq)]
pub struct RateLimits {
    pub submit: u64,
    pub upload: u64,
    pub search: u64,
    pub read: u64,
    pub per_day: u64,
}

impl Default for RateLimits {
    fn default() -> Self {
        Self { submit: 120, upload: 20, search: 240, read: 600, per_day: 100_000 }
    }
}

impl RateLimits {
    pub fn from_json(json: &Json) -> Self {
        let mut c = RateLimits::default();
        if let Some(v) = json.get("submit").and_then(|v| v.as_u64()) {
            c.submit = v;
        }
        if let Some(v) = json.get("upload").and_then(|v| v.as_u64()) {
            c.upload = v;
        }
        if let Some(v) = json.get("search").and_then(|v| v.as_u64()) {
            c.search = v;
        }
        if let Some(v) = json.get("read").and_then(|v| v.as_u64()) {
            c.read = v;
        }
        if let Some(v) = json.get("per_day").and_then(|v| v.as_u64()) {
            c.per_day = v;
        }
        c
    }
}

pub struct RateLimiter {
    inner: Mutex<HashMap<String, (u64, u64, u64)>>,
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

impl RateLimiter {
    pub fn new() -> Self {
        Self { inner: Mutex::new(HashMap::new()) }
    }

    pub fn check(&mut self, key: &str, action: &str, limits: &RateLimits) -> anyhow::Result<()> {
        let action_limit = match action {
            "submit" => limits.submit,
            "upload" => limits.upload,
            "search" => limits.search,
            _ => limits.read,
        };
        let now = now_secs();
        let mut inner = self.inner.lock().unwrap();
        if inner.len() > MAX_ENTRIES {
            inner.retain(|_, e| now - e.2 < DAY_WINDOW_SECS);
        }
        let e = inner.entry(key.to_string()).or_insert((0, 0, now));
        if now - e.2 >= DAY_WINDOW_SECS {
            *e = (0, 0, now);
        } else if now - e.2 >= ACTION_WINDOW_SECS {
            e.0 = 0;
        }
        e.0 += 1;
        e.1 += 1;
        if e.0 > action_limit || e.1 > limits.per_day {
            anyhow::bail!("rate limited");
        }
        Ok(())
    }

    pub fn snapshot(&self, prefix: &str) -> serde_json::Value {
        let now = now_secs();
        let inner = self.inner.lock().unwrap();
        let mut actions = serde_json::Map::new();
        let mut day = 0u64;
        let p = format!("{prefix}:");
        for (k, e) in inner.iter() {
            if let Some(action) = k.strip_prefix(&p) {
                if now - e.2 < ACTION_WINDOW_SECS {
                    actions.insert(action.to_string(), json!(e.0));
                }
            }
        }
        for (k, e) in inner.iter() {
            if let Some(action) = k.strip_prefix(&p) {
                if action != "day" && now - e.2 < DAY_WINDOW_SECS {
                    day = day.saturating_add(e.1);
                }
            }
        }
        serde_json::json!({
            "window": ACTION_WINDOW_SECS,
            "per_action": actions,
            "day_count": day,
        })
    }

    pub fn prune(&mut self, max: usize) {
        let now = now_secs();
        let mut inner = self.inner.lock().unwrap();
        inner.retain(|_, e| now - e.2 < DAY_WINDOW_SECS);
        if inner.len() > max {
            let mut keys: Vec<String> = inner.keys().cloned().collect();
            keys.sort_by_key(|k| inner[k].2);
            for k in keys.into_iter().take(inner.len() - max) {
                inner.remove(&k);
            }
        }
    }
}

fn now_secs() -> u64 {
    // chrono reads the host clock on wasm (js_sys::Date); SystemTime panics.
    chrono::Utc::now().timestamp().max(0) as u64
}