use crate::model::Key;
use crate::storage::ir::SrvFilter;
use async_trait::async_trait;
use serde_json::{json, Value as Json};
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Int(i64),
    Float(f64),
    Text(String),
    Bool(bool),
    Blob(Vec<u8>),
    Json(Json),
}

#[derive(Debug, Clone)]
pub struct Row {
    pub key: Key,
    pub data: Json,
}

impl Row {
    pub fn new(key: Key, data: Json) -> Self {
        Self { key, data }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Query {
    pub filter: SrvFilter,
    pub orders: Vec<(String, bool)>,
    pub limit: usize,
    pub offset: usize,
    pub ttl: Option<TtlClause>,
}

#[derive(Debug, Clone, Default)]
pub struct TtlClause {
    pub seconds: Option<i64>,
    pub field: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Cursor {
    pub rows: Vec<Row>,
    pub has_more: bool,
}

#[derive(Debug, Clone, Default)]
pub struct FtsCaps {
    pub bm25: bool,
    pub ngram: bool,
    pub phrase: bool,
    pub analyzers: bool,
}

#[derive(Debug, Clone, Default)]
pub struct TxCaps {
    pub savepoints: bool,
    pub cross_shard: bool,
}

#[derive(Debug, Clone, Default)]
pub struct ReplicationCaps {
    pub embedded_replica: bool,
    pub multi_writer: bool,
    pub cdc: bool,
}

#[derive(Debug, Clone, Default)]
pub struct JsonCaps {
    pub jsonb: bool,
    pub json_path: bool,
}

#[derive(Debug, Clone, Default)]
pub struct DatabaseCaps {
    pub fts: FtsCaps,
    pub vector: bool,
    pub transactions: TxCaps,
    pub replication: ReplicationCaps,
    pub json: JsonCaps,
}

impl DatabaseCaps {
    pub fn none() -> Self {
        Self::default()
    }

    pub fn full() -> Self {
        Self {
            fts: FtsCaps { bm25: true, ngram: true, phrase: true, analyzers: true },
            vector: true,
            transactions: TxCaps { savepoints: true, cross_shard: true },
            replication: ReplicationCaps { embedded_replica: true, multi_writer: true, cdc: true },
            json: JsonCaps { jsonb: true, json_path: true },
        }
    }
}

/// Storage seam. Async (`?Send`: worker futures such as JsFuture are `!Send`,
/// and nothing on the isolate requires `Send`) so D1/R2 adapters can await
/// host I/O. See PORT-TRACK.md Phase 5a.
#[async_trait(?Send)]
pub trait Database: Send + Sync + 'static {
    fn adapter(&self) -> &'static str;

    fn capabilities(&self) -> DatabaseCaps;

    async fn insert(&mut self, table: &str, row: Row) -> anyhow::Result<i64>;

    /// Insert many rows in one batch. Default falls back to per-row `insert`;
    /// adapters (e.g. HelixDB) override to send a single write batch.
    async fn bulk_insert(&mut self, table: &str, rows: Vec<Row>) -> anyhow::Result<Vec<i64>> {
        let mut seqs = Vec::with_capacity(rows.len());
        for row in rows {
            seqs.push(self.insert(table, row).await?);
        }
        Ok(seqs)
    }

    async fn get(&self, table: &str, pk: &Key) -> anyhow::Result<Option<Row>>;

    async fn update(&mut self, table: &str, pk: &Key, patch: &Json) -> anyhow::Result<()>;

    async fn delete(&mut self, table: &str, filter: &SrvFilter) -> anyhow::Result<usize>;

    async fn query(&self, table: &str, q: &Query) -> anyhow::Result<Cursor>;

    async fn upsert(&mut self, table: &str, key: &str, row: Row) -> anyhow::Result<i64>;

    // ---- graph (HelixDB) -------------------------------------------------
    // Optional graph operations over nodes/edges, scoped to a board tenant.
    // Default implementations report "unsupported" so non-graph adapters
    // (memory) degrade gracefully; the HelixDB adapter overrides them.

    /// Link two nodes by an edge label with optional edge properties.
    async fn link(
        &mut self,
        _board: &str,
        _from: i64,
        _label: &str,
        _to: i64,
        _props: &Json,
    ) -> anyhow::Result<i64> {
        anyhow::bail!("graph links are not supported by the {} backend", self.adapter())
    }

    /// Create MANY edges in one write batch (much cheaper than N `link` calls
    /// for graph sync). Each entry is (from_node_id, label, to_node_id).
    /// Returns the created edge ids.
    async fn link_batch(
        &mut self,
        _board: &str,
        _edges: &[(i64, String, i64)],
    ) -> anyhow::Result<Vec<i64>> {
        anyhow::bail!("graph batch links are not supported by the {} backend", self.adapter())
    }

    /// Traverse from a node along an edge label (out/in/both), optionally
    /// multi-hop. Returns reached node ids with their data.
    async fn traverse(
        &self,
        _board: &str,
        _from: i64,
        _label: Option<&str>,
        _dir: &str,
        _depth: usize,
    ) -> anyhow::Result<Cursor> {
        anyhow::bail!("graph traversal is not supported by the {} backend", self.adapter())
    }

    /// Search edges by BM25 over an edge property within the board tenant.
    async fn search_edges(
        &self,
        _board: &str,
        _label: &str,
        _property: &str,
        _query: &str,
        _limit: usize,
    ) -> anyhow::Result<Cursor> {
        anyhow::bail!("edge search is not supported by the {} backend", self.adapter())
    }

    /// Drop an edge by its Helix edge id (`$id`).
    async fn unlink(&mut self, _board: &str, _edge_id: i64) -> anyhow::Result<bool> {
        anyhow::bail!("graph unlink is not supported by the {} backend", self.adapter())
    }

    /// Drop a node and every edge touching it (both directions). Helix does
    /// NOT cascade edge deletes on node drop, so edges must be removed first.
    async fn delete_node(&mut self, _board: &str, _node_id: i64) -> anyhow::Result<usize> {
        anyhow::bail!("graph node delete is not supported by the {} backend", self.adapter())
    }

    /// Begin a write transaction scoped to a board's shard. Default is a no-op
    /// (memory backend and non-transactional adapters). Used by bulk loads to
    /// amortize commit/sync cost across many record inserts.
    async fn begin(&mut self, _board: &str) -> anyhow::Result<()> {
        Ok(())
    }

    /// Commit an open board transaction started with [`Database::begin`].
    async fn commit(&mut self, _board: &str) -> anyhow::Result<()> {
        Ok(())
    }

    /// Roll back an open board transaction started with [`Database::begin`].
    async fn rollback(&mut self, _board: &str) -> anyhow::Result<()> {
        Ok(())
    }

    /// Run an aggregate over `table`, optionally grouped. Default implementation
    /// loads rows and aggregates in Rust; a SQL-capable adapter overrides it.
    /// `filter` carries only the user's conditions (matched against payload).
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
        let _ = board;
        let q = Query {
            filter: crate::storage::ir::SrvFilter {
                conds: vec![
                    crate::storage::ir::FilterCond {
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
        let rows = self.query(table, &q).await?.rows;
        aggregate_rows(&rows, filter, agg, field, group_by, ttl)
    }

    /// Atomically allocate `n` fresh seq values for a (board, table). Returns
    /// the FIRST allocated seq; the range [first, first+n) is reserved for the
    /// caller. Adapters that can increment a counter server-side override this
    /// (HelixDB); the default falls back to read-max+1 which is ONLY safe under
    /// an external exclusive lock.
    async fn allocate_seqs(&mut self, _board: &str, _table: &str, n: i64) -> anyhow::Result<i64> {
        let _ = n;
        anyhow::bail!("allocate_seqs not supported by the {} backend", self.adapter())
    }

    /// Count every record on a board in one cheap query. Default implementation
    /// sums per-table aggregates; a backend that can count a whole tenant in a
    /// single query (e.g. HelixDB) overrides it.
    async fn count_records(&self, _board: &str) -> anyhow::Result<i64> {
        anyhow::bail!("count_records is not supported by the {} backend", self.adapter())
    }
}

/// Aggregate `rows` (record JSON documents) in Rust. Used by the default
/// `Database::aggregate` and by adapters that do not push aggregates to SQL.
pub fn aggregate_rows(
    rows: &[Row],
    filter: &SrvFilter,
    agg: crate::storage::ir::Agg,
    field: Option<&str>,
    group_by: Option<&str>,
    ttl: Option<TtlClause>,
) -> anyhow::Result<Vec<Json>> {
    use crate::storage::ir::{normalize_path, scalar_text, Agg};
    use std::collections::BTreeMap;

    #[derive(Default)]
    struct Acc {
        rows: u64,
        n: u64,
        sum: f64,
        min: f64,
        max: f64,
        has: bool,
    }
    impl Acc {
        fn add(&mut self, v: Option<f64>) {
            self.rows += 1;
            if let Some(v) = v {
                self.n += 1;
                self.sum += v;
                if !self.has {
                    self.min = v;
                    self.max = v;
                    self.has = true;
                } else {
                    self.min = self.min.min(v);
                    self.max = self.max.max(v);
                }
            }
        }
    }
    fn agg_value(agg: Agg, acc: &Acc) -> Json {
        match agg {
            Agg::Count => Json::from(acc.rows),
            Agg::Sum => {
                if acc.has { Json::from(acc.sum) } else { Json::Null }
            }
            Agg::Avg => {
                if acc.has { Json::from(acc.sum / acc.n as f64) } else { Json::Null }
            }
            Agg::Min => {
                if acc.has { Json::from(acc.min) } else { Json::Null }
            }
            Agg::Max => {
                if acc.has { Json::from(acc.max) } else { Json::Null }
            }
        }
    }
    fn field_num(payload: &Json, path: &str) -> Option<f64> {
        crate::expr::get_path(payload, path).as_f64()
    }
    fn aggregate_ttl_alive(ttl: Option<&TtlClause>, rec: &Json) -> bool {
        let Some(ttl) = ttl else { return true };
        let created = rec.get("created_at").and_then(|v| v.as_str()).unwrap_or("");
        let payload = rec.get("payload").cloned().unwrap_or(Json::Null);
        let age_dead = match ttl.seconds {
            Some(n) if n > 0 && !created.is_empty() => {
                created < crate::crud::subtract_seconds(&crate::crud::now_str(), n).as_str()
            }
            _ => false,
        };
        let field_dead = match &ttl.field {
            Some(f) => {
                let value = crate::expr::get_path(&payload, &crate::storage::ir::normalize_path(f));
                !value.is_null() && crate::storage::ir::scalar_text(&value) < crate::crud::now_str()
            }
            None => false,
        };
        !(age_dead || field_dead)
    }
    let field_path = field.map(normalize_path);
    let group_path = group_by.map(normalize_path);

    if group_path.is_none() {
        let mut acc = Acc::default();
        for row in rows {
            let rec: Json = row.data.clone();
            let payload = rec.get("payload").cloned().unwrap_or(Json::Null);
            if !aggregate_ttl_alive(ttl.as_ref(), &rec) || !filter.matches(&payload) {
                continue;
            }
            let v = field_path.as_deref().and_then(|p| field_num(&payload, p));
            acc.add(v);
        }
        return Ok(vec![json!({ "value": agg_value(agg, &acc) })]);
    }

    let mut groups: BTreeMap<String, (Json, Acc)> = BTreeMap::new();
    for row in rows {
        let rec: Json = row.data.clone();
        let payload = rec.get("payload").cloned().unwrap_or(Json::Null);
        if !aggregate_ttl_alive(ttl.as_ref(), &rec) || !filter.matches(&payload) {
            continue;
        }
        let gv = group_path
            .as_deref()
            .map(|p| crate::expr::get_path(&payload, p))
            .unwrap_or(Json::Null);
        let key = scalar_text(&gv);
        let entry = groups.entry(key).or_insert((gv, Acc::default()));
        let v = field_path.as_deref().and_then(|p| field_num(&payload, p));
        entry.1.add(v);
    }
    let mut out = Vec::new();
    for (_, (g, acc)) in groups {
        out.push(json!({ "group": g, "value": agg_value(agg, &acc) }));
    }
    Ok(out)
}
