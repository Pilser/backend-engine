use crate::model::Key;
use crate::storage::database::{Cursor, Database, DatabaseCaps, Query, Row, TtlClause};
use crate::storage::ir::{FilterCond, SrvFilter};
use crate::storage::object_store::{BlobMeta, KeyInfo, ObjectStore, ObjectStoreCaps, PutInfo};
use async_trait::async_trait;
use serde_json::Value as Json;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

const DEFAULT_TTL_SECS: u64 = 30;
const MAX_ENTRIES: usize = 10_000;

#[derive(Clone)]
enum Cached {
    Row(Row),
    Cursor(Cursor),
}

struct Entry {
    expires: Instant,
    value: Cached,
}

struct CacheData {
    entries: HashMap<String, Entry>,
    order: VecDeque<String>,
}

fn sha256_hex(s: &str) -> String {
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    let d = h.finalize();
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(64);
    for b in d {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

fn row_ttl(data: &Json) -> Duration {
    let secs = data.get("ttl_seconds").and_then(|v| v.as_i64()).unwrap_or(DEFAULT_TTL_SECS as i64);
    Duration::from_secs(secs.max(1) as u64)
}

fn key_repr(k: &Key) -> String {
    match k {
        Key::Int(v) => format!("int:{v}"),
        Key::Text(s) => format!("text:{s}"),
    }
}

fn query_hash(table: &str, q: &Query) -> String {
    let mut s = String::new();
    for c in &q.filter.conds {
        s.push_str(&c.field);
        s.push('|');
        s.push_str(c.op.as_str());
        s.push('|');
        s.push_str(&serde_json::to_string(&c.value).unwrap_or_default());
        s.push(';');
    }
    s.push_str("o:");
    for (f, asc) in &q.orders {
        s.push_str(f);
        s.push_str(if *asc { ":1" } else { ":0" });
        s.push(',');
    }
    s.push_str(&format!("l:{}s:{}", q.limit, q.offset));
    if let Some(t) = &q.ttl {
        s.push_str(&format!("t:{:?}:{:?}", t.seconds, t.field));
    }
    format!("{table}::q::{}", sha256_hex(&s))
}

/// One singleflight slot: the leader stores `Some(result)` when done; errors
/// are carried as `Err(message)` (anyhow::Error is not Clone).
type InflightSlot = Arc<(Mutex<Option<Result<Cursor, String>>>, Condvar)>;

pub struct CachedDatabase {
    inner: Box<dyn Database>,
    cache: Mutex<CacheData>,
    /// Singleflight: key -> slot for identical in-flight queries. The first
    /// caller to hold the slot's cell lock runs ONE backend query; concurrent
    /// duplicates block on that lock and reuse the result instead of
    /// stampeding Helix with N full scans (reproduced on prod: 10 identical
    /// counts = 10 scans, 8.6s tail latency).
    inflight: Arc<Mutex<HashMap<String, InflightSlot>>>,
}

impl CachedDatabase {
    pub fn new(inner: Box<dyn Database>) -> Self {
        Self {
            inner,
            cache: Mutex::new(CacheData { entries: HashMap::new(), order: VecDeque::new() }),
            inflight: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Remove the in-flight slot only if it is still THIS call's slot (a new
    /// leader may have replaced it after a takeover).
    fn inflight_remove(inflight: &Mutex<HashMap<String, InflightSlot>>, ck: &str, slot: &InflightSlot) {
        if let Ok(mut m) = inflight.lock() {
            if m.get(ck).map(|s| Arc::ptr_eq(s, slot)).unwrap_or(false) {
                m.remove(ck);
            }
        }
    }

    fn inflight_contains(&self, inflight: &Mutex<HashMap<String, InflightSlot>>, ck: &str, slot: &InflightSlot) -> bool {
        inflight
            .lock()
            .map(|m| m.get(ck).map(|s| Arc::ptr_eq(s, slot)).unwrap_or(false))
            .unwrap_or(false)
    }


    fn cache_get(&self, key: &str) -> Option<Cached> {
        let mut c = self.cache.lock().unwrap();
        let e = c.entries.get(key)?;
        if e.expires <= Instant::now() {
            c.entries.remove(key);
            c.order.retain(|k| k != key);
            return None;
        }
        Some(e.value.clone())
    }

    fn cache_put(&self, key: &str, value: Cached, ttl: Duration) {
        let now = Instant::now();
        let mut c = self.cache.lock().unwrap();
        if !c.entries.contains_key(key) {
            c.order.push_back(key.to_string());
        }
        c.entries.insert(
            key.to_string(),
            Entry { expires: now + ttl, value },
        );
        while c.entries.len() > MAX_ENTRIES {
            if let Some(k) = c.order.pop_front() {
                if c.entries.contains_key(&k) {
                    c.entries.remove(&k);
                }
            } else {
                break;
            }
        }
    }

    fn invalidate_table(&self, table: &str) {
        let mut c = self.cache.lock().unwrap();
        let prefix = format!("{table}::");
        c.entries.retain(|k, _| !k.starts_with(&prefix));
        let CacheData { entries, order } = &mut *c;
        order.retain(|k| entries.contains_key(k));
    }
}

#[async_trait(?Send)]
impl Database for CachedDatabase {
    fn adapter(&self) -> &'static str {
        "cached"
    }

    fn capabilities(&self) -> DatabaseCaps {
        self.inner.capabilities()
    }

    async fn insert(&mut self, table: &str, row: Row) -> anyhow::Result<i64> {
        let seq = self.inner.insert(table, row).await?;
        self.invalidate_table(table);
        Ok(seq)
    }

    async fn bulk_insert(&mut self, table: &str, rows: Vec<Row>) -> anyhow::Result<Vec<i64>> {
        let seqs = self.inner.bulk_insert(table, rows).await?;
        self.invalidate_table(table);
        Ok(seqs)
    }

    async fn get(&self, table: &str, pk: &Key) -> anyhow::Result<Option<Row>> {
        let ck = format!("{table}::k::{}", key_repr(pk));
        if let Some(Cached::Row(row)) = self.cache_get(&ck) {
            return Ok(Some(row));
        }
        let row = self.inner.get(table, pk).await?;
        if let Some(r) = &row {
            self.cache_put(&ck, Cached::Row(r.clone()), row_ttl(&r.data));
        }
        Ok(row)
    }

    async fn update(&mut self, table: &str, pk: &Key, patch: &Json) -> anyhow::Result<()> {
        self.inner.update(table, pk, patch).await?;
        self.invalidate_table(table);
        Ok(())
    }

    async fn delete(&mut self, table: &str, filter: &SrvFilter) -> anyhow::Result<usize> {
        let n = self.inner.delete(table, filter).await?;
        self.invalidate_table(table);
        Ok(n)
    }

    async fn query(&self, table: &str, q: &Query) -> anyhow::Result<Cursor> {
        if q.ttl.is_some() {
            return self.inner.query(table, q).await;
        }
        let ck = query_hash(table, q);

        // ---- singleflight -------------------------------------------------
        // Register/lookup an in-flight slot for this exact query. The FIRST
        // caller to hold the slot's cell lock is the leader and runs the real
        // backend query while HOLDING that lock; every other identical miss
        // blocks on the same lock, then re-checks the main cache (which the
        // leader populated) — one Helix scan instead of N.
        let slot: InflightSlot = {
            let mut m = match self.inflight.lock() {
                Ok(g) => g,
                Err(_) => return self.inner.query(table, q).await, // poisoned: degrade to no-coalescing
            };
            m.entry(ck.clone())
                .or_insert_with(|| Arc::new((Mutex::new(None), Condvar::new())))
                .clone()
        };
        let (cell, cv) = &*slot;
        let cell_guard = match cell.lock() {
            Ok(g) => g,
            Err(_) => return self.inner.query(table, q).await,
        };
        if cell_guard.is_none() {
            // We are the leader: run the query WITH the cell lock held so
            // followers wait here instead of stampeding the backend.
            let result = self.inner.query(table, q).await;
            {
                let mut st = cell_guard;
                *st = Some(result.as_ref().map_err(|e| e.to_string()).map(|c| c.clone()));
                cv.notify_all();
            }
            Self::inflight_remove(&self.inflight, &ck, &slot);
            // Result also lands in the main cache for later (non-concurrent)
            // misses.
            if let Ok(c) = &result {
                self.cache_put(&ck, Cached::Cursor(c.clone()), Duration::from_secs(DEFAULT_TTL_SECS));
            }
            return result;
        }

        // FOLLOWER path: leader holds the lock; wait for its verdict.
        let mut guard = cell_guard;
        loop {
            guard = match cv.wait_timeout(guard, Duration::from_secs(120)) {
                Ok((g, _)) => g,
                Err(_) => break,
            };
            if let Some(res) = guard.take() {
                drop(guard);
                Self::inflight_remove(&self.inflight, &ck, &slot);
                // Prefer the fresh cache entry when it exists.
                return match self.cache_get(&ck) {
                    Some(Cached::Cursor(c)) => Ok(c),
                    // A Row entry for a query key cannot happen; treat as miss.
                    _ => res.map_err(|msg| anyhow::anyhow!(msg)),
                };
            }
        }

        // Leader failed or timed out: last resort, query directly (no
        // re-registration — avoids thundering-herd recursion).
        self.inner.query(table, q).await
    }

    async fn upsert(&mut self, table: &str, key: &str, row: Row) -> anyhow::Result<i64> {
        let seq = self.inner.upsert(table, key, row).await?;
        self.invalidate_table(table);
        Ok(seq)
    }

    async fn link(
        &mut self,
        board: &str,
        from: i64,
        label: &str,
        to: i64,
        props: &Json,
    ) -> anyhow::Result<i64> {
        self.inner.link(board, from, label, to, props).await
    }

    async fn link_batch(&mut self, board: &str, edges: &[(i64, String, i64)]) -> anyhow::Result<Vec<i64>> {
        self.inner.link_batch(board, edges).await
    }

    async fn traverse(
        &self,
        board: &str,
        from: i64,
        label: Option<&str>,
        dir: &str,
        depth: usize,
    ) -> anyhow::Result<Cursor> {
        self.inner.traverse(board, from, label, dir, depth).await
    }

    async fn search_edges(
        &self,
        board: &str,
        label: &str,
        property: &str,
        query: &str,
        limit: usize,
    ) -> anyhow::Result<Cursor> {
        self.inner.search_edges(board, label, property, query, limit).await
    }

    async fn unlink(&mut self, board: &str, edge_id: i64) -> anyhow::Result<bool> {
        self.inner.unlink(board, edge_id).await
    }

    async fn delete_node(&mut self, board: &str, node_id: i64) -> anyhow::Result<usize> {
        self.inner.delete_node(board, node_id).await
    }

    async fn begin(&mut self, board: &str) -> anyhow::Result<()> {
        self.inner.begin(board).await
    }

    async fn commit(&mut self, board: &str) -> anyhow::Result<()> {
        self.inner.commit(board).await
    }

    async fn rollback(&mut self, board: &str) -> anyhow::Result<()> {
        self.inner.rollback(board).await
    }

    async fn aggregate(
        &self,
        table: &str,
        board: &str,
        table_name: &str,
        filter: &SrvFilter,
        agg: crate::storage::ir::Agg,
        field: Option<&str>,
        group_by: Option<&str>,
        ttl: Option<TtlClause>,
    ) -> anyhow::Result<Vec<Json>> {
        // Route through query() so aggregates get BOTH the result cache AND
        // singleflight coalescing. The table routing cond scopes the scan;
        // (the old board cond is gone with stored board_id fields).
        // Tradeoff: loses the adapter's server-side aggregate_by pushdown;
        // acceptable while admission control bounds concurrency.
        let _ = board;
        let q = Query {
            filter: SrvFilter {
                conds: vec![
                    FilterCond {
                        field: "$.table".to_string(),
                        op: crate::storage::ir::Op::Eq,
                        value: Json::String(table_name.to_string()),
                    },
                ],
            },
            orders: vec![],
            limit: usize::MAX,
            offset: 0,
            ttl: ttl.clone(),
        };
        let cursor = self.query(table, &q).await?;
        crate::storage::database::aggregate_rows(&cursor.rows, filter, agg, field, group_by, ttl)
    }

    /// Seq allocation must reach the real backend (it owns the atomic
    /// counter). A new seq implies new rows are possible; invalidate the
    /// records table so stale count/list caches don't hide them.
    async fn allocate_seqs(&mut self, board: &str, table: &str, n: i64) -> anyhow::Result<i64> {
        let first = self.inner.allocate_seqs(board, table, n).await?;
        self.invalidate_table(crate::tables::TABLE_RECORDS);
        Ok(first)
    }

    async fn count_records(&self, board: &str) -> anyhow::Result<i64> {
        self.inner.count_records(board).await
    }
}

struct BlobEntry {
    expires: Instant,
    bytes: Vec<u8>,
}

struct BlobCache {
    entries: HashMap<String, BlobEntry>,
    order: VecDeque<String>,
}

const DEFAULT_MAX_CACHEABLE_BYTES: u64 = 256 * 1024;

pub struct CachedObjectStore {
    inner: Box<dyn ObjectStore>,
    cache: Mutex<BlobCache>,
    ttl: Duration,
    max: usize,
    max_cacheable_bytes: u64,
}

impl CachedObjectStore {
    pub fn new(inner: Box<dyn ObjectStore>) -> Self {
        Self {
            inner,
            cache: Mutex::new(BlobCache { entries: HashMap::new(), order: VecDeque::new() }),
            ttl: Duration::from_secs(DEFAULT_TTL_SECS),
            max: 512,
            max_cacheable_bytes: DEFAULT_MAX_CACHEABLE_BYTES,
        }
    }

    pub fn with_max_cacheable_bytes(mut self, bytes: u64) -> Self {
        self.max_cacheable_bytes = bytes;
        self
    }

    fn cacheable(&self, size: u64) -> bool {
        size <= self.max_cacheable_bytes
    }

    fn cache_get(&self, key: &str) -> Option<Vec<u8>> {
        let mut c = self.cache.lock().unwrap();
        let e = c.entries.get(key)?;
        if e.expires <= Instant::now() {
            c.entries.remove(key);
            c.order.retain(|k| k != key);
            return None;
        }
        Some(e.bytes.clone())
    }

    fn cache_put(&self, key: &str, bytes: Vec<u8>) {
        let now = Instant::now();
        let mut c = self.cache.lock().unwrap();
        if !c.entries.contains_key(key) {
            c.order.push_back(key.to_string());
        }
        c.entries.insert(key.to_string(), BlobEntry { expires: now + self.ttl, bytes });
        while c.entries.len() > self.max {
            if let Some(k) = c.order.pop_front() {
                if c.entries.contains_key(&k) {
                    c.entries.remove(&k);
                }
            } else {
                break;
            }
        }
    }

    fn invalidate(&self, key: &str) {
        let mut c = self.cache.lock().unwrap();
        if c.entries.remove(key).is_some() {
            c.order.retain(|k| k != key);
        }
    }
}

#[async_trait(?Send)]
impl ObjectStore for CachedObjectStore {
    async fn put(&self, key: &str, bytes: &[u8]) -> anyhow::Result<PutInfo> {
        let info = self.inner.put(key, bytes).await?;
        if self.cacheable(info.size) {
            self.cache_put(key, bytes.to_vec());
        } else {
            self.invalidate(key);
        }
        Ok(info)
    }

    async fn get(&self, key: &str) -> anyhow::Result<Option<Vec<u8>>> {
        if let Some(bytes) = self.cache_get(key) {
            return Ok(Some(bytes));
        }
        let bytes = self.inner.get(key).await?;
        if let Some(b) = &bytes {
            if self.cacheable(b.len() as u64) {
                self.cache_put(key, b.clone());
            }
        }
        Ok(bytes)
    }

    async fn head(&self, key: &str) -> anyhow::Result<Option<BlobMeta>> {
        self.inner.head(key).await
    }

    async fn delete(&self, key: &str) -> anyhow::Result<()> {
        self.inner.delete(key).await?;
        self.invalidate(key);
        Ok(())
    }

    async fn list(&self, prefix: &str) -> anyhow::Result<Vec<KeyInfo>> {
        self.inner.list(prefix).await
    }

    fn capabilities(&self) -> ObjectStoreCaps {
        self.inner.capabilities()
    }
}