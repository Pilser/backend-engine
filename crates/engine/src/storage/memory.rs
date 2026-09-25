use crate::model::Key;
use crate::storage::database::{Cursor, Database, DatabaseCaps, Query, Row};
use crate::storage::ir::{scalar_text, SrvFilter};
use crate::storage::object_store::{BlobMeta, KeyInfo, ObjectStore, ObjectStoreCaps, PutInfo};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

#[derive(Default)]
struct Table {
    rows: Vec<Row>,
    next_seq: i64,
}

#[derive(Default)]
struct MemData {
    tables: HashMap<String, Table>,
}

pub struct InMemoryDatabase {
    data: Arc<RwLock<MemData>>,
}

impl InMemoryDatabase {
    pub fn new() -> Self {
        Self { data: Arc::new(RwLock::new(MemData::default())) }
    }
}

impl Default for InMemoryDatabase {
    fn default() -> Self {
        Self::new()
    }
}

impl Database for InMemoryDatabase {
    fn adapter(&self) -> &'static str {
        "memory"
    }

    fn capabilities(&self) -> DatabaseCaps {
        DatabaseCaps::none()
    }

    fn insert(&mut self, table: &str, mut row: Row) -> anyhow::Result<i64> {
        let mut data = self.data.write().unwrap();
        let t = data.tables.entry(table.to_string()).or_default();
        let seq = match &row.key {
            Key::Text(_) => {
                t.rows.push(row);
                return Ok(0);
            }
            Key::Int(v) if *v > 0 => {
                if *v > t.next_seq {
                    t.next_seq = *v;
                }
                *v
            }
            Key::Int(_) => {
                t.next_seq += 1;
                row.key = Key::Int(t.next_seq);
                t.next_seq
            }
        };

        t.rows.push(row);
        Ok(seq)
    }

    fn allocate_seqs(&mut self, board: &str, table: &str, n: i64) -> anyhow::Result<i64> {
        // The memory backend's Table.next_seq IS the atomic counter; the
        // write lock here makes the range reservation exclusive. `board`
        // doesn't scope the in-memory tables today, but records keys are
        // per-table so allocation matches insert semantics.
        let _ = board;
        let mut data = self.data.write().unwrap();
        let t = data.tables.entry(table.to_string()).or_default();
        t.next_seq += n;
        Ok(t.next_seq - n + 1)
    }

    fn get(&self, table: &str, pk: &Key) -> anyhow::Result<Option<Row>> {
        let data = self.data.read().unwrap();
        let t = data.tables.get(table);
        let Some(t) = t else { return Ok(None) };
        Ok(t.rows.iter().find(|r| &r.key == pk).cloned())
    }

    fn update(&mut self, table: &str, pk: &Key, patch: &serde_json::Value) -> anyhow::Result<()> {
        let mut data = self.data.write().unwrap();
        let t = data.tables.entry(table.to_string()).or_default();
        if let Some(r) = t.rows.iter_mut().find(|r| &r.key == pk) {
            r.data = patch.clone();
        }
        Ok(())
    }

    fn delete(&mut self, table: &str, filter: &SrvFilter) -> anyhow::Result<usize> {
        let mut data = self.data.write().unwrap();
        let t = data.tables.entry(table.to_string()).or_default();
        let before = t.rows.len();
        t.rows.retain(|r| !filter.matches(&r.data));
        Ok(before - t.rows.len())
    }

    fn query(&self, table: &str, q: &Query) -> anyhow::Result<Cursor> {
        let data = self.data.read().unwrap();
        let t = data.tables.get(table);
        let Some(t) = t else {
            return Ok(Cursor::default());
        };
        let now = crate::crud::now_str();
        let mut rows: Vec<Row> = t
            .rows
            .iter()
            .filter(|r| q.filter.matches(&r.data))
            .filter(|r| memory_ttl_alive(q.ttl.as_ref(), &r.data, &now))
            .cloned()
            .collect();
        if !q.orders.is_empty() {
            rows.sort_by(|a, b| compare_rows(a, b, &q.orders));
        } else if let Some(qq) = search_query(&q.filter) {
            rows.sort_by(|a, b| {
                let sa = search_score(&a.data, &qq);
                let sb = search_score(&b.data, &qq);
                sb.partial_cmp(&sa)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| a.key.cmp(&b.key))
            });
        } else {
            rows.sort_by(|a, b| b.key.cmp(&a.key));
        }
        let start = q.offset.min(rows.len());
        let has_more = start + q.limit < rows.len();
        let end = (start + q.limit).min(rows.len());
        Ok(Cursor { rows: rows[start..end].to_vec(), has_more })
    }

    fn upsert(&mut self, table: &str, key: &str, mut row: Row) -> anyhow::Result<i64> {
        let mut data = self.data.write().unwrap();
        let t = data.tables.entry(table.to_string()).or_default();
        let unique = crate::expr::get_path(&row.data, key);
        let existing_key = t
            .rows
            .iter()
            .find(|r| crate::expr::get_path(&r.data, key) == unique)
            .map(|r| r.key.clone());
        if let Some(ek) = existing_key {
            let seq = match &ek {
                Key::Int(v) => *v,
                Key::Text(_) => 0,
            };
            row.key = ek.clone();
            if let Some(e) = t.rows.iter_mut().find(|r| r.key == ek) {
                *e = row;
            }
            return Ok(seq);
        }
        t.next_seq += 1;
        row.key = Key::Int(t.next_seq);
        t.rows.push(row);
        Ok(t.next_seq)
    }
}

fn search_query(filter: &SrvFilter) -> Option<String> {
    filter
        .conds
        .iter()
        .find(|c| c.op == crate::storage::ir::Op::Search)
        .map(|c| crate::storage::ir::scalar_text(&c.value))
}

fn search_score(data: &serde_json::Value, query: &str) -> f64 {
    let payload = data.get("payload").cloned().unwrap_or(serde_json::Value::Null);
    if !crate::expr::search_match(&payload, query) {
        return 0.0;
    }
    let hay = serde_json::to_string(&payload).unwrap_or_default().to_lowercase();
    let tokens: Vec<String> = query
        .split_whitespace()
        .filter(|t| !t.is_empty())
        .map(|t| t.to_lowercase())
        .collect();
    tokens.iter().filter(|t| hay.contains(t.as_str())).count() as f64
}

fn memory_ttl_alive(ttl: Option<&crate::storage::database::TtlClause>, data: &serde_json::Value, now: &str) -> bool {
    let Some(ttl) = ttl else { return true };
    let created = data.get("created_at").and_then(|v| v.as_str()).unwrap_or("");
    let payload = data.get("payload").cloned().unwrap_or(serde_json::Value::Null);
    let age_dead = match ttl.seconds {
        Some(n) if n > 0 && !created.is_empty() => created < crate::crud::subtract_seconds(now, n).as_str(),
        _ => false,
    };
    let field_dead = match &ttl.field {
        Some(f) => {
            let value = crate::expr::get_path(&payload, &crate::storage::ir::normalize_path(f));
            !value.is_null() && crate::storage::ir::scalar_text(&value) < now.to_string()
        }
        None => false,
    };
    !(age_dead || field_dead)
}

fn compare_rows(a: &Row, b: &Row, orders: &[(String, bool)]) -> std::cmp::Ordering {
    for (field, desc) in orders {
        let va = crate::expr::get_path(&a.data, field);
        let vb = crate::expr::get_path(&b.data, field);
        let ord = compare_scalar(&va, &vb);
        if ord != std::cmp::Ordering::Equal {
            return if *desc { ord.reverse() } else { ord };
        }
    }
    b.key.cmp(&a.key)
}

fn compare_scalar(a: &serde_json::Value, b: &serde_json::Value) -> std::cmp::Ordering {
    match (a.as_f64(), b.as_f64()) {
        (Some(x), Some(y)) => x.partial_cmp(&y).unwrap_or(std::cmp::Ordering::Equal),
        _ => scalar_text(a).cmp(&scalar_text(b)),
    }
}

struct StoredBlob {
    bytes: Vec<u8>,
    meta: BlobMeta,
}

pub struct InMemoryObjectStore {
    data: RwLock<HashMap<String, StoredBlob>>,
}

impl InMemoryObjectStore {
    pub fn new() -> Self {
        Self { data: RwLock::new(HashMap::new()) }
    }
}

impl Default for InMemoryObjectStore {
    fn default() -> Self {
        Self::new()
    }
}

impl ObjectStore for InMemoryObjectStore {
    fn put(&self, key: &str, bytes: &[u8]) -> anyhow::Result<PutInfo> {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        let digest = hasher.finalize();
        let hex = digest.iter().map(|b| format!("{:02x}", b)).collect::<String>();
        let size = bytes.len() as u64;
        let meta = BlobMeta {
            key: key.to_string(),
            size,
            sha256: hex.clone(),
            content_type: None,
            modified: None,
        };
        self.data.write().unwrap().insert(key.to_string(), StoredBlob { bytes: bytes.to_vec(), meta: meta.clone() });
        Ok(PutInfo { key: key.to_string(), size, sha256: hex })
    }

    fn get(&self, key: &str) -> anyhow::Result<Option<Vec<u8>>> {
        Ok(self.data.read().unwrap().get(key).map(|b| b.bytes.clone()))
    }

    fn head(&self, key: &str) -> anyhow::Result<Option<BlobMeta>> {
        Ok(self.data.read().unwrap().get(key).map(|b| b.meta.clone()))
    }

    fn delete(&self, key: &str) -> anyhow::Result<()> {
        self.data.write().unwrap().remove(key);
        Ok(())
    }

    fn list(&self, prefix: &str) -> anyhow::Result<Vec<KeyInfo>> {
        let data = self.data.read().unwrap();
        let mut out: Vec<KeyInfo> = data
            .iter()
            .filter(|(k, _)| k.starts_with(prefix))
            .map(|(k, b)| KeyInfo { key: k.clone(), size: b.meta.size })
            .collect();
        out.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(out)
    }

    fn copy(&self, from: &str, to: &str) -> anyhow::Result<bool> {
        let src = {
            let data = self.data.read().unwrap();
            data.get(from).map(|b| (b.bytes.clone(), b.meta.clone()))
        };
        let Some((bytes, meta)) = src else { return Ok(false) };
        let mut meta = meta;
        meta.key = to.to_string();
        let mut data = self.data.write().unwrap();
        data.insert(to.to_string(), StoredBlob { bytes, meta });
        Ok(true)
    }

    fn capabilities(&self) -> ObjectStoreCaps {
        ObjectStoreCaps { copy: true, ..ObjectStoreCaps::none() }
    }
}
