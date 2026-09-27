//! D1 [`engine::Database`] adapter.
//!
//! Storage layout: one SQLite table per engine table,
//! `"name" (key TEXT PRIMARY KEY, data TEXT NOT NULL)` where `data` is the
//! stored JSON document. Keys are type-prefixed (`i42` / `t: singleton/…`)
//! so integer and text keys can never collide.
//!
//! Correctness-first: `query` pushes down only the tenant/table equality
//! predicates (bounding transfer to one table's rows); all other filtering,
//! ordering, TTL and pagination reuse the memory adapter's exact logic via
//! [`engine::storage::memory::apply_query`]. The default `aggregate`
//! implementation (fetch + Rust math) is inherited unchanged.
//!
//! Every batch fuses a `CREATE TABLE IF NOT EXISTS` for the touched table,
//! so a fresh D1 database self-heals on first use (one round trip per op).

use async_trait::async_trait;
use engine::model::Key;
use engine::storage::database::{Cursor, Database, DatabaseCaps, Query, Row};
use engine::storage::ir::{Op, SrvFilter};
use engine::storage::memory::apply_query;
use serde::Deserialize;
use worker::d1::{D1Database, D1Type};

/// Engine tables (fixed set — user tables are `table`-field values inside
/// `wb_records`, never separate SQL tables).
const COUNTERS_TABLE: &str = "wb_counters";

fn qt(table: &str) -> String {
    format!("\"{}\"", table.replace('"', "\"\""))
}

fn ensure_sql(table: &str) -> String {
    if table == COUNTERS_TABLE {
        format!(
            "CREATE TABLE IF NOT EXISTS {t} (name TEXT PRIMARY KEY, next INTEGER NOT NULL)",
            t = qt(table)
        )
    } else {
        format!(
            "CREATE TABLE IF NOT EXISTS {t} (key TEXT PRIMARY KEY, data TEXT NOT NULL)",
            t = qt(table)
        )
    }
}

/// Physical key namespacing (the vanishing-rows hunt, 2026-09-27).
///
/// ALL logical record tables share one physical table, but seqs are
/// per-table — so bare `i1` keys collide across tables and `INSERT OR
/// REPLACE` silently destroys other tables' rows (every table's seq-N row
/// fought over one key; last writer won). Namespacing by logical table
/// makes physical keys unique: `{table}/i{n}`.
///
/// `decode_key` stays backward-tolerant (strips any `{table}/` prefix, still
/// reads legacy bare keys), so pre-fix rows remain readable; they get
/// rewritten namespaced on next write.
fn encode_key(table: &str, key: &Key) -> String {
    match key {
        // Zero-padded so key ordering still equals recency ordering.
        Key::Int(n) => format!("{table}/i{n:020}"),
        // Composite record keys (`{table}/r{seq:020}` from crud) and any
        // other slashed form pass through under the physical prefix.
        Key::Text(s) if s.contains('/') => format!("{table}/{s}"),
        Key::Text(s) => format!("{table}/t:{s}"),
    }
}

fn decode_key(s: &str) -> Key {
    // Legacy bare Text keys predate namespacing entirely (`t:singleton/x`
    // may itself contain slashes) — a leading `t:` always means "the whole
    // string is the key", never a namespace to strip.
    if let Some(rest) = s.strip_prefix("t:") {
        return Key::Text(format!("t:{rest}"));
    }
    let rest = s.split_once('/').map(|(_, r)| r).unwrap_or(s);
    if let Some(n) = rest.strip_prefix('i').and_then(|r| r.parse::<i64>().ok()) {
        Key::Int(n)
    } else if let Some(t) = rest.strip_prefix("t:") {
        Key::Text(t.to_string())
    } else {
        Key::Text(rest.to_string())
    }
}

/// Pre-0.2.3 bare format (no table namespace). Reads fall back to it so
/// rows written before key namespacing stay visible; every write path now
/// stores namespaced keys, so legacy rows migrate on next touch.
fn legacy_encode_key(key: &Key) -> String {
    match key {
        Key::Int(n) => format!("i{n}"),
        Key::Text(s) => format!("t:{s}"),
    }
}

/// Canonical dedupe key: logical identity across all key generations —
/// bare (`i6`), 0.2.3 (`wb_records/i6`), padded (`wb_records/i000...006`)
/// all canonicalize per decoded value; composite record keys stay distinct.
fn logical_key(raw: &str) -> String {
    match decode_key(raw) {
        Key::Int(n) => format!("i{n}"),
        Key::Text(s) => format!("t:{s}"),
    }
}

/// Drop shadow duplicates: same logical key present bare (pre-0.2.3) and
/// namespaced — keep the namespaced (current) version.
fn dedupe_rows(pairs: Vec<(String, Row)>) -> Vec<Row> {
    use std::collections::HashMap;
    let mut best: HashMap<String, (Row, bool)> = HashMap::new();
    for (raw, row) in pairs.into_iter() {
        let k = logical_key(&raw);
        let namespaced = raw.contains('/');
        match best.get(&k) {
            Some((_, true)) if !namespaced => {}
            _ => {
                best.insert(k, (row, namespaced));
            }
        }
    }
    best.into_values().map(|(r, _)| r).collect()
}

#[derive(Deserialize)]
struct DataRow {
    key: String,
    data: String,
}

fn parse_row(r: DataRow) -> anyhow::Result<Row> {
    let data: serde_json::Value = serde_json::from_str(&r.data)?;
    Ok(Row::new(decode_key(&r.key), data))
}

/// Extract the `$.table` string-equality predicate for SQL pushdown (table
/// scoping is the only routing left). Returns the WHERE fragment and the
/// bound values in order. The full filter is still evaluated in Rust
/// afterwards.
fn prefilter(filter: &SrvFilter) -> (String, Vec<String>) {
    let mut values: Vec<String> = Vec::new();
    let mut sql = String::new();
    for c in &filter.conds {
        if c.op != Op::Eq || c.field != "$.table" {
            continue;
        }
        let Some(v) = c.value.as_str() else { continue };
        sql.push_str(if values.is_empty() { " WHERE " } else { " AND " });
        sql.push_str(&format!("json_extract(data, '$.table') = ?{}", values.len() + 1));
        values.push(v.to_string());
    }
    (sql, values)
}

fn bind_values<'a>(values: &'a [String]) -> Vec<D1Type<'a>> {
    values.iter().map(|v| D1Type::Text(v.as_str())).collect()
}

pub struct D1Db {
    db: D1Database,
}

impl D1Db {
    pub fn new(db: D1Database) -> Self {
        Self { db }
    }

    /// Run `stmts` with an `ensure` DDL first, returning all results.
    async fn batch_ensured(
        &self,
        table: &str,
        stmts: Vec<worker::d1::D1PreparedStatement>,
    ) -> anyhow::Result<Vec<worker::d1::D1Result>> {
        let mut all = Vec::with_capacity(stmts.len() + 1);
        all.push(self.db.prepare(ensure_sql(table)));
        all.extend(stmts);
        Ok(self.db.batch(all).await?)
    }
}

#[async_trait(?Send)]
impl Database for D1Db {
    fn adapter(&self) -> &'static str {
        "d1"
    }

    fn capabilities(&self) -> DatabaseCaps {
        DatabaseCaps::none()
    }

    async fn insert(&mut self, table: &str, row: Row) -> anyhow::Result<i64> {
        let seq = match &row.key {
            Key::Text(_) => 0,
            Key::Int(v) if *v > 0 => *v,
            Key::Int(_) => self.allocate_seqs(engine::TENANT, table, 1).await?,
        };
        let key = match &row.key {
            Key::Int(v) if *v > 0 => Key::Int(*v),
            Key::Int(_) => Key::Int(seq),
            Key::Text(_) => row.key.clone(),
        };
        let ks = encode_key(table, &key);
        let data = serde_json::to_string(&row.data)?;
        let t = qt(table);
        let sql = format!("INSERT OR REPLACE INTO {t} (key, data) VALUES (?1, ?2)");
        let params = vec![D1Type::Text(ks.as_str()), D1Type::Text(data.as_str())];
        let stmt = self.db.prepare(sql).bind_refs(&params)?;
        let results = self.batch_ensured(table, vec![stmt]).await?;
        let r = results.into_iter().last().expect("batch returns one result per stmt");
        if !r.success() {
            anyhow::bail!("d1 insert failed: {}", r.error().unwrap_or_default());
        }
        Ok(seq)
    }

    async fn bulk_insert(&mut self, table: &str, rows: Vec<Row>) -> anyhow::Result<Vec<i64>> {
        // Resolve seqs first (mirrors the memory adapter), then write in
        // chunks of 50 statements per batch round trip.
        let mut seqs = Vec::with_capacity(rows.len());
        let mut payloads = Vec::with_capacity(rows.len());
        for row in rows {
            let seq = match &row.key {
                Key::Text(_) => 0,
                Key::Int(v) if *v > 0 => *v,
                Key::Int(_) => self.allocate_seqs(engine::TENANT, table, 1).await?,
            };
            let key = match &row.key {
                Key::Int(v) if *v > 0 => Key::Int(*v),
                Key::Int(_) => Key::Int(seq),
                Key::Text(_) => row.key.clone(),
            };
            seqs.push(seq);
            payloads.push((encode_key(table, &key), serde_json::to_string(&row.data)?));
        }
        let t = qt(table);
        for chunk in payloads.chunks(50) {
            let mut stmts = Vec::with_capacity(chunk.len());
            for (k, d) in chunk {
                let sql = format!("INSERT OR REPLACE INTO {t} (key, data) VALUES (?1, ?2)");
                // bind_refs copies values into JS; `params` may drop after.
                let params = vec![D1Type::Text(k.as_str()), D1Type::Text(d.as_str())];
                stmts.push(self.db.prepare(sql).bind_refs(&params)?);
            }
            let results = self.batch_ensured(table, stmts).await?;
            for r in results.into_iter().skip(1) {
                if !r.success() {
                    anyhow::bail!("d1 bulk insert failed: {}", r.error().unwrap_or_default());
                }
            }
        }
        Ok(seqs)
    }

    async fn get(&self, table: &str, pk: &Key) -> anyhow::Result<Option<Row>> {
        // Namespaced first, legacy bare fallback (pre-0.2.3 rows).
        let t = qt(table);
        for ks in [encode_key(table, pk), legacy_encode_key(pk)] {
            let sql = format!("SELECT key, data FROM {t} WHERE key = ?1");
            let params = vec![D1Type::Text(ks.as_str())];
            let stmt = self.db.prepare(sql).bind_refs(&params)?;
            let results = self.batch_ensured(table, vec![stmt]).await?;
            let r = results.into_iter().last().expect("batch returns one result per stmt");
            let mut rows = r.results::<DataRow>()?;
            if let Some(row) = rows.pop() {
                return parse_row(row).map(Some);
            }
        }
        Ok(None)
    }

    async fn update(&mut self, table: &str, pk: &Key, patch: &serde_json::Value) -> anyhow::Result<()> {
        let data = serde_json::to_string(patch)?;
        let t = qt(table);
        // Write to BOTH key forms: heals diverged pairs (bare legacy +
        // namespaced current) in one shot; exactly one matches normally.
        let k1 = encode_key(table, pk);
        let k2 = legacy_encode_key(pk);
        let sql = format!("UPDATE {t} SET data = ?1 WHERE key = ?2 OR key = ?3");
        let params = vec![D1Type::Text(data.as_str()), D1Type::Text(k1.as_str()), D1Type::Text(k2.as_str())];
        let stmt = self.db.prepare(sql).bind_refs(&params)?;
        let results = self.batch_ensured(table, vec![stmt]).await?;
        let r = results.into_iter().last().expect("batch returns one result per stmt");
        if !r.success() {
            anyhow::bail!("d1 update failed: {}", r.error().unwrap_or_default());
        }
        Ok(())
    }

    async fn delete(&mut self, table: &str, filter: &SrvFilter) -> anyhow::Result<usize> {
        let (where_sql, values) = prefilter(filter);
        let t = qt(table);
        let sql = format!("SELECT key, data FROM {t}{where_sql}");
        let params = bind_values(&values);
        let stmt = self.db.prepare(sql).bind_refs(&params)?;
        let results = self.batch_ensured(table, vec![stmt]).await?;
        let r = results.into_iter().last().expect("batch returns one result per stmt");
        // Delete by RAW stored key (not re-encoded): legacy bare rows must
        // match too, otherwise filtered deletes silently miss pre-0.2.3 rows.
        let doomed: Vec<String> = r
            .results::<DataRow>()?
            .into_iter()
            .filter_map(|dr| {
                let raw = dr.key.clone();
                parse_row(dr)
                    .ok()
                    .filter(|row| filter.matches(&engine::storage::memory::match_view(&row.data)))
                    .map(|_| raw)
            })
            .collect();
        if doomed.is_empty() {
            return Ok(0);
        }
        let mut total = 0usize;
        for chunk in doomed.chunks(500) {
            let placeholders: Vec<String> =
                chunk.iter().enumerate().map(|(i, _)| format!("?{}", i + 1)).collect();
            let sql = format!("DELETE FROM {t} WHERE key IN ({})", placeholders.join(","));
            let params: Vec<D1Type<'_>> = chunk.iter().map(|k| D1Type::Text(k.as_str())).collect();
            let stmt = self.db.prepare(sql).bind_refs(&params)?;
            let results = self.batch_ensured(table, vec![stmt]).await?;
            let r = results.into_iter().last().expect("batch returns one result per stmt");
            total += r.meta()?.and_then(|m| m.changes).unwrap_or(0);
        }
        Ok(total)
    }

    async fn query(&self, table: &str, q: &Query) -> anyhow::Result<Cursor> {
        let (where_sql, values) = prefilter(&q.filter);
        let t = qt(table);
        let sql = format!("SELECT key, data FROM {t}{where_sql}");
        let params = bind_values(&values);
        let stmt = self.db.prepare(sql).bind_refs(&params)?;
        let results = self.batch_ensured(table, vec![stmt]).await?;
        let r = results.into_iter().last().expect("batch returns one result per stmt");
        let pairs: Vec<(String, Row)> = r
            .results::<DataRow>()?
            .into_iter()
            .map(|dr| {
                let raw = dr.key.clone();
                parse_row(dr).map(|row| (raw, row))
            })
            .collect::<anyhow::Result<_>>()?;
        Ok(apply_query(dedupe_rows(pairs), q))
    }

    async fn upsert(&mut self, table: &str, key: &str, mut row: Row) -> anyhow::Result<i64> {
        let unique = engine::expr::get_path(&row.data, key);
        let t = qt(table);
        let sql = format!("SELECT key, data FROM {t}");
        let stmt = self.db.prepare(sql);
        let results = self.batch_ensured(table, vec![stmt]).await?;
        let r = results.into_iter().last().expect("batch returns one result per stmt");
        let rows: Vec<Row> =
            r.results::<DataRow>()?.into_iter().map(parse_row).collect::<anyhow::Result<_>>()?;
        if let Some(existing) = rows.iter().find(|r| engine::expr::get_path(&r.data, key) == unique) {
            let seq = match &existing.key {
                Key::Int(v) => *v,
                Key::Text(_) => 0,
            };
            let eks = encode_key(table, &existing.key);
            let data = serde_json::to_string(&row.data)?;
            row.key = existing.key.clone();
            let sql = format!("UPDATE {t} SET data = ?1 WHERE key = ?2");
            let params = vec![D1Type::Text(data.as_str()), D1Type::Text(eks.as_str())];
            let stmt = self.db.prepare(sql).bind_refs(&params)?;
            let results = self.batch_ensured(table, vec![stmt]).await?;
            let r = results.into_iter().last().expect("batch returns one result per stmt");
            if !r.success() {
                anyhow::bail!("d1 upsert failed: {}", r.error().unwrap_or_default());
            }
            return Ok(seq);
        }
        let seq = self.allocate_seqs(engine::TENANT, table, 1).await?;
        row.key = Key::Int(seq);
        self.insert(table, row).await?;
        Ok(seq)
    }

    async fn allocate_seqs(&mut self, _board: &str, table: &str, n: i64) -> anyhow::Result<i64> {
        // Single atomic statement: insert-or-bump a per-table counter and
        // RETURNING the new head. The range [head-n+1, head] is ours.
        let delta: i32 = i32::try_from(n.max(1)).map_err(|_| anyhow::anyhow!("seq range too large"))?;
        let t = qt(COUNTERS_TABLE);
        let sql = format!(
            "INSERT INTO {t}(name, next) VALUES (?1, ?2) \
             ON CONFLICT(name) DO UPDATE SET next = next + excluded.next \
             RETURNING next"
        );
        let name = table.to_string();
        let params = vec![D1Type::Text(name.as_str()), D1Type::Integer(delta)];
        let stmt = self.db.prepare(sql).bind_refs(&params)?;
        let results = self.batch_ensured(COUNTERS_TABLE, vec![stmt]).await?;
        let r = results.into_iter().last().expect("batch returns one result per stmt");
        let head: Option<i64> = r.results::<serde_json::Value>()?.pop().and_then(|v| {
            v.get("next").and_then(|n| n.as_i64()).or_else(|| v.as_i64())
        });
        let head = head.ok_or_else(|| anyhow::anyhow!("d1 allocate_seqs returned no row"))?;
        Ok(head - n + 1)
    }
}
