use crate::model::{Board, Key, KeyRecord, Principal, Recipe, Record, Secret};
use crate::storage::database::{Database, DatabaseCaps};
use crate::storage::ir::{scalar_text, Agg, SrvFilter};
use crate::storage::object_store::ObjectStore;
use serde_json::{json, Value as Json};
use std::sync::Arc;

pub type Notify = Arc<dyn Fn(String, String, i64, Json) + Send + Sync>;

/// Naive English pluralization for graph-sync target-table discovery:
/// `class`->`classes`, `category`->`categories`, `box`->`boxes`, else +s.
fn pluralize(s: &str) -> String {
    if s.ends_with("ss") || s.ends_with("sh") || s.ends_with("ch") || s.ends_with("x") {
        format!("{s}es")
    } else if s.ends_with('y') && !s.ends_with("ay") && !s.ends_with("ey") && !s.ends_with("oy") && !s.ends_with("uy") {
        format!("{}ies", &s[..s.len() - 1])
    } else {
        format!("{s}s")
    }
}

/// Facade over the whole serverless engine. Owns a `Database` and an
/// `ObjectStore`; every operation in the system goes through here.
pub struct ServerlessEngine {
    db: Box<dyn Database>,
    store: Arc<dyn ObjectStore>,
    notify: Option<Notify>,
}

impl ServerlessEngine {
    pub fn new(db: Box<dyn Database>, store: Box<dyn ObjectStore>) -> Self {
        Self { db, store: Arc::from(store), notify: None }
    }

    pub fn with_defaults() -> Self {
        Self::new(
            Box::new(crate::storage::memory::InMemoryDatabase::new()),
            Box::new(crate::storage::memory::InMemoryObjectStore::new()),
        )
    }

    pub fn set_notifier(&mut self, notify: Option<Notify>) {
        self.notify = notify;
    }

    fn emit(&self, board: &str, kind: &str, seq: i64, payload: Json) {
        if let Some(f) = &self.notify {
            f(board.to_string(), kind.to_string(), seq, payload);
        }
    }

    pub fn database(&self) -> &dyn Database {
        self.db.as_ref()
    }

    // ---- graph (HelixDB) ------------------------------------------------

    /// Link two nodes in a board's tenant by an edge label. Node ids are the
    /// Helix node ids (`$id`) returned by records tools.
    pub fn graph_link(
        &mut self,
        board: &str,
        from: i64,
        label: &str,
        to: i64,
        props: &Json,
    ) -> anyhow::Result<i64> {
        self.db.link(board, from, label, to, props)
    }

    /// Traverse from a node along an edge label (out/in/both), up to `depth`
    /// hops. Returns reached nodes (id + data).
    pub fn graph_traverse(
        &self,
        board: &str,
        from: i64,
        label: Option<&str>,
        dir: &str,
        depth: usize,
    ) -> anyhow::Result<crate::storage::database::Cursor> {
        self.db.traverse(board, from, label, dir, depth)
    }

    /// BM25 search over edges in a board's tenant.
    pub fn graph_search_edges(
        &self,
        board: &str,
        label: &str,
        property: &str,
        query: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<Json>> {
        let cursor = self.db.search_edges(board, label, property, query, limit)?;
        Ok(cursor.rows.into_iter().map(|r| r.data).collect())
    }

    /// Drop an edge by its Helix edge id.
    pub fn graph_unlink(&mut self, board: &str, edge_id: i64) -> anyhow::Result<bool> {
        self.db.unlink(board, edge_id)
    }

    /// Drop a node and every edge touching it (both directions).
    pub fn graph_delete_node(&mut self, board: &str, node_id: i64) -> anyhow::Result<usize> {
        self.db.delete_node(board, node_id)
    }

    /// Materialize `*_id` payload references in a table as real Helix edges.
    /// For each record, a payload field ending in `_id` (e.g. `class_id`)
    /// names a target table (`class`/`classes`); its value is the target
    /// record's seq, resolved to a node id. An edge is created from the
    /// source record's node to the target node, labeled from the field
    /// (`class_id` -> `RELATED_CLASS`). Idempotent per (from, label, to).
    /// Returns a report of created edges.
    pub fn graph_sync(
        &mut self,
        board: &str,
        table: &str,
    ) -> anyhow::Result<serde_json::Value> {
        let records = self.list_records(board, table, 1000, None, 0, "desc")?;
        let mut created: Vec<serde_json::Value> = Vec::new();
        let mut skipped = 0usize;
        // Memoize (target_table, ref_value) -> target node id so repeated refs
        // (e.g. many learners -> same class) resolve once, not per record.
        let mut target_cache: std::collections::HashMap<(String, String), Option<i64>> =
            std::collections::HashMap::new();
        // Collect edges, then flush via link_batch in chunks (one write batch
        // per chunk instead of one HTTP round-trip per edge).
        let mut pending: Vec<(i64, String, i64, String, String)> = Vec::new(); // from, label, to, field, target_table
        for rec in &records {
            let Some(from) = rec.payload.get("_node_id").and_then(|v| v.as_i64()) else {
                continue;
            };
            let Some(obj) = rec.payload.as_object() else { continue };
            for (field, value) in obj {
                let Some(target_table) = field.strip_suffix("_id") else {
                    continue;
                };
                if field == "_node_id" || target_table.is_empty() {
                    continue;
                }
                let ref_value = value.clone();
                let cache_key = (target_table.to_string(), scalar_text(&ref_value));
                let target_node = if let Some(cached) = target_cache.get(&cache_key) {
                    *cached
                } else {
                    // A resolution error (e.g. transient) counts as unresolved;
                    // re-runs retry it.
                    let resolved = self.graph_resolve_target(board, table, target_table, &ref_value).unwrap_or(None);
                    target_cache.insert(cache_key, resolved);
                    resolved
                };
                let Some(to) = target_node else {
                    skipped += 1;
                    continue;
                };
                let label = format!("RELATED_{}", target_table.to_uppercase());
                pending.push((from, label, to, field.to_string(), target_table.to_string()));
            }
        }
        // Flush in chunks of 20 edges per write batch.
        const CHUNK: usize = 20;
        for chunk in pending.chunks(CHUNK) {
            let batch: Vec<(i64, String, i64)> = chunk.iter().map(|(f, l, t, _, _)| (*f, l.clone(), *t)).collect();
            match self.db.link_batch(board, &batch) {
                Ok(ids) => {
                    for ((from, label, to, field, target_table), edge) in chunk.iter().zip(ids) {
                        created.push(serde_json::json!({
                            "from": from, "label": label, "to": to, "edge": edge,
                            "field": field, "target_table": target_table,
                        }));
                    }
                }
                Err(_) => {
                    // Transient batch failure: fall back to per-edge links so
                    // a bad edge doesn't lose the whole chunk.
                    for (from, label, to, field, target_table) in chunk {
                        match self.db.link(board, *from, label, *to, &serde_json::json!({})) {
                            Ok(edge) => created.push(serde_json::json!({
                                "from": from, "label": label, "to": to, "edge": edge,
                                "field": field, "target_table": target_table,
                            })),
                            Err(_) => skipped += 1,
                        }
                    }
                }
            }
        }
        Ok(serde_json::json!({
            "table": table,
            "created": created.len(),
            "skipped": skipped,
            "edges": created,
        }))
    }

    /// Resolve a `*_id` ref value to a target table's node id. `*_id` refs are
    /// source UUIDs matching the target's payload `id`; numeric refs match
    /// `$.seq`. Tries exact + plural table names. Returns `Ok(None)` when the
    /// target doesn't exist.
    fn graph_resolve_target(
        &self,
        board: &str,
        source_table: &str,
        target_table: &str,
        ref_value: &Json,
    ) -> anyhow::Result<Option<i64>> {
        if std::env::var("HELIX_DEBUG").as_deref() == Ok("1") {
            eprintln!("[sync] resolve {source_table}.{target_table} = {ref_value}");
        }
        // Candidate table names for the ref: the field-derived name, its
        // plural (handles class->classes, category->categories), and the
        // singular/known aliases the migration uses (e.g. user->profiles).
        let mut candidates = vec![target_table.to_string()];
        let plural = pluralize(target_table);
        if plural != target_table {
            candidates.push(plural);
        }
        if target_table == "user" {
            candidates.push("profiles".to_string());
        }
        if target_table == "teacher" {
            candidates.push("profiles".to_string());
        }
        for t in &candidates {
            if t == source_table {
                continue; // skip self-table refs (would be self-loops)
            }
            let mut lookups = Vec::new();
            if ref_value.is_string() {
                lookups.push(serde_json::json!({
                    "field": "$.id", "op": "eq", "value": ref_value.clone()
                }));
            }
            if let Some(n) = ref_value.as_i64() {
                lookups.push(serde_json::json!({
                    "field": "$.seq", "op": "eq", "value": serde_json::Value::from(n)
                }));
            }
            for lu in lookups {
                let field = lu["field"].as_str().unwrap_or("$.id").to_string();
                let val = lu["value"].clone();
                let q = crate::storage::database::Query {
                    filter: crate::storage::ir::SrvFilter {
                        conds: vec![
                            crate::crud::board_cond(board),
                            crate::crud::table_cond(t),
                            crate::storage::ir::FilterCond { field, op: crate::storage::ir::Op::Eq, value: val },
                        ],
                    },
                    orders: vec![],
                    limit: 1,
                    offset: 0,
                    ttl: None,
                };
                let rows = self.db.query("wb_records", &q)?.rows;
                if let Some(row) = rows.first() {
                    let id = row
                        .data
                        .get("payload")
                        .and_then(|p| p.get("_node_id"))
                        .or_else(|| row.data.get("_node_id"))
                        .and_then(|v| v.as_i64());
                    if let Some(id) = id {
                        return Ok(Some(id));
                    }
                }
            }
        }
        Ok(None)
    }

    pub fn object_store(&self) -> &dyn ObjectStore {
        self.store.as_ref()
    }

    /// Cloneable shared handle to the object store, so transports can read
    /// assets/files WITHOUT taking the engine Mutex.
    pub fn shared_store(&self) -> Arc<dyn ObjectStore> {
        Arc::clone(&self.store)
    }

    pub fn caps(&self) -> DatabaseCaps {
        self.db.capabilities()
    }

    // ---- app lifecycle -------------------------------------------------

    pub fn create_app(
        &mut self,
        owner: &str,
        title: &str,
        schema: Option<Json>,
        public_reads: bool,
        unique_key: Option<&str>,
    ) -> anyhow::Result<Board> {
        crate::crud::app_create(self.db.as_mut(), owner, title, schema, public_reads, unique_key)
    }

    pub fn get_app(&self, board_id: &str) -> anyhow::Result<Option<Board>> {
        crate::crud::app_by_id(self.db.as_ref(), board_id)
    }

    pub fn list_apps(&self, owner: &str) -> anyhow::Result<Vec<Board>> {
        crate::crud::app_list(self.db.as_ref(), owner)
    }

    pub fn update_app(&mut self, board_id: &str, owner: &str, patch: &Json) -> anyhow::Result<()> {
        crate::crud::app_update(self.db.as_mut(), board_id, owner, patch)
    }

    pub fn delete_app(&mut self, board_id: &str, owner: &str) -> anyhow::Result<()> {
        crate::crud::app_delete(self.db.as_mut(), board_id, owner)
    }

    // ---- records -------------------------------------------------------

    pub fn insert_record(
        &mut self,
        board_id: &str,
        table: &str,
        payload: Json,
        writer: Option<&str>,
        upsert: bool,
        principal: &Principal,
    ) -> anyhow::Result<i64> {
        let seq = crate::crud::record_insert(self.db.as_mut(), board_id, table, payload.clone(), writer, upsert, principal)?;
        self.emit(board_id, "created", seq, payload);
        Ok(seq)
    }

    pub fn bulk_insert(
        &mut self,
        board_id: &str,
        table: &str,
        records: Vec<Json>,
        writer: Option<&str>,
        upsert: bool,
        principal: &Principal,
    ) -> anyhow::Result<Vec<i64>> {
        let seqs = crate::crud::record_bulk_insert(
            self.db.as_mut(),
            board_id,
            table,
            records.clone(),
            writer,
            upsert,
            principal,
        )?;
        for (seq, record) in seqs.iter().zip(records.into_iter()) {
            self.emit(board_id, "created", *seq, record);
        }
        Ok(seqs)
    }

    /// Fast bulk import for migration (see `crud::record_bulk_import`). Skips
    /// per-row recipes/webhooks/audit; run `graph_sync` after loading.
    pub fn bulk_import(
        &mut self,
        board_id: &str,
        table: &str,
        records: Vec<Json>,
        writer: Option<&str>,
        principal: &Principal,
    ) -> anyhow::Result<Vec<i64>> {
        let seqs = crate::crud::record_bulk_import(
            self.db.as_mut(),
            board_id,
            table,
            records.clone(),
            writer,
            principal,
        )?;
        for (seq, record) in seqs.iter().zip(records.into_iter()) {
            self.emit(board_id, "created", *seq, record);
        }
        Ok(seqs)
    }

    /// Import records from a JSON (array/JSONL) or CSV dump. Each row is
    /// validated against the table schema; invalid rows are skipped and
    /// reported rather than aborting the whole import.
    pub fn import_records(
        &mut self,
        board_id: &str,
        table: &str,
        format: &str,
        data: &str,
        separator: char,
        upsert: bool,
        principal: &Principal,
    ) -> anyhow::Result<Json> {
        let records = crate::import::parse(format, data, separator)?;
        let mut seqs = Vec::new();
        let mut errors = Vec::new();
        for record in records {
            match self.insert_record(
                board_id,
                table,
                record.clone(),
                Some(&principal.id),
                upsert,
                principal,
            ) {
                Ok(seq) => seqs.push(seq),
                Err(e) => errors.push(json!({ "record": record, "error": e.to_string() })),
            }
        }
        Ok(json!({
            "total": seqs.len() + errors.len(),
            "inserted": seqs.len(),
            "seqs": seqs,
            "errors": errors,
        }))
    }

    pub fn get_record(&self, board_id: &str, table: &str, seq: i64) -> anyhow::Result<Option<Record>> {
        crate::crud::record_get(self.db.as_ref(), board_id, table, seq)
    }

    pub fn set_record(
        &mut self,
        board_id: &str,
        table: &str,
        seq: i64,
        payload: Json,
        writer: Option<&str>,
    ) -> anyhow::Result<()> {
        crate::crud::record_set(self.db.as_mut(), board_id, table, seq, payload.clone(), writer)?;
        self.emit(board_id, "updated", seq, payload);
        Ok(())
    }

    pub fn patch_record(
        &mut self,
        board_id: &str,
        table: &str,
        seq: i64,
        ops: &Json,
        writer: Option<&str>,
    ) -> anyhow::Result<Json> {
        let merged = crate::crud::record_patch(self.db.as_mut(), board_id, table, seq, ops, writer)?;
        self.emit(board_id, "updated", seq, merged.clone());
        Ok(merged)
    }

    pub fn patch_first(
        &mut self,
        board_id: &str,
        table: &str,
        conds: &SrvFilter,
        ops: &Json,
    ) -> anyhow::Result<Option<Json>> {
        crate::crud::record_patch_first(self.db.as_mut(), board_id, table, conds, ops)
    }

    pub fn delete_record(&mut self, board_id: &str, table: &str, seq: i64) -> anyhow::Result<bool> {
        let deleted = crate::crud::record_delete_one(self.db.as_mut(), board_id, table, seq)?;
        if deleted {
            self.emit(board_id, "deleted", seq, Json::Null);
        }
        Ok(deleted)
    }

    pub fn delete_records(&mut self, board_id: &str, table: &str, conds: &SrvFilter) -> anyhow::Result<usize> {
        crate::crud::record_delete_filter(self.db.as_mut(), board_id, table, conds)
    }

    pub fn list_records(
        &self,
        board_id: &str,
        table: &str,
        limit: usize,
        before: Option<i64>,
        offset: usize,
        dir: &str,
    ) -> anyhow::Result<Vec<Record>> {
        crate::crud::record_list(self.db.as_ref(), board_id, table, limit, before, offset, dir)
    }

    pub fn count_records(&self, board_id: &str, table: &str) -> anyhow::Result<i64> {
        crate::crud::record_count(self.db.as_ref(), board_id, table)
    }

    /// Fast board-wide record count (one backend query where supported).
    pub fn count_records_board(&self, board_id: &str) -> anyhow::Result<i64> {
        match self.db.count_records(board_id) {
            Ok(n) => Ok(n),
            Err(_) => {
                // Backend without a fast path: sum per-table counts.
                let mut total = 0i64;
                for cfg in crate::crud::table_list(self.db.as_ref(), board_id)? {
                    total += crate::crud::record_count(self.db.as_ref(), board_id, &cfg.table)?;
                }
                Ok(total)
            }
        }
    }

    pub fn records_after(&self, board_id: &str, after: i64) -> anyhow::Result<Vec<Record>> {
        crate::crud::records_after(self.db.as_ref(), board_id, after)
    }

    // ---- tables ---------------------------------------------------------

    pub fn create_table(
        &mut self,
        board_id: &str,
        table: &str,
        schema: Option<Json>,
        unique_key: Option<&str>,
    ) -> anyhow::Result<crate::model::TableConfig> {
        crate::crud::table_create(self.db.as_mut(), board_id, table, schema, unique_key)
    }

    pub fn list_tables(&self, board_id: &str) -> anyhow::Result<Vec<crate::model::TableConfig>> {
        crate::crud::table_list(self.db.as_ref(), board_id)
    }

    pub fn get_table(&self, board_id: &str, table: &str) -> anyhow::Result<Option<crate::model::TableConfig>> {
        crate::crud::table_get(self.db.as_ref(), board_id, table)
    }

    pub fn drop_table(&mut self, board_id: &str, table: &str) -> anyhow::Result<bool> {
        crate::crud::table_delete(self.db.as_mut(), board_id, table)
    }

    // ---- query / search / aggregate / join ----------------------------

    pub fn query_records(
        &self,
        board_id: &str,
        table: &str,
        filter: &SrvFilter,
        orders: &[(String, bool)],
        limit: usize,
        offset: usize,
    ) -> anyhow::Result<Vec<Record>> {
        crate::query::query_records(
            self.db.as_ref(),
            board_id,
            table,
            filter,
            orders,
            limit,
            offset,
        )
    }

    pub fn search_records(
        &self,
        board_id: &str,
        table: &str,
        query: &str,
        conds: &SrvFilter,
        limit: usize,
        offset: usize,
        snippet: bool,
    ) -> anyhow::Result<Vec<Record>> {
        crate::query::search_records(
            self.db.as_ref(),
            board_id,
            table,
            query,
            conds,
            limit,
            offset,
            snippet,
        )
    }

    pub fn aggregate_records(
        &self,
        board_id: &str,
        table: &str,
        conds: &SrvFilter,
        agg: Agg,
        field: Option<&str>,
        group_by: Option<&str>,
    ) -> anyhow::Result<Vec<Json>> {
        crate::query::aggregate_records(
            self.db.as_ref(),
            board_id,
            table,
            conds,
            agg,
            field,
            group_by,
        )
    }

    pub fn join_list(
        &self,
        child_board: &str,
        child_table: &str,
        conds: &SrvFilter,
        limit: usize,
        offset: usize,
    ) -> anyhow::Result<Vec<Record>> {
        crate::policy::join_list(self.db.as_ref(), child_board, child_table, conds, limit, offset)
    }

    // ---- keys ----------------------------------------------------------

    pub fn issue_key(
        &mut self,
        board_id: &str,
        role: &str,
        writer: Option<&str>,
        scope: Option<&str>,
    ) -> anyhow::Result<(KeyRecord, String)> {
        crate::auth::issue_key(self.db.as_mut(), board_id, role, writer, scope)
    }

    pub fn list_keys(&self, board_id: &str) -> anyhow::Result<Vec<KeyRecord>> {
        crate::auth::list_keys(self.db.as_ref(), board_id)
    }

    pub fn get_key(&self, bucket: &str) -> anyhow::Result<Option<KeyRecord>> {
        crate::auth::get_key(self.db.as_ref(), bucket)
    }

    pub fn revoke_key(&mut self, bucket: &str) -> anyhow::Result<()> {
        crate::auth::revoke_key(self.db.as_mut(), bucket)
    }

    pub fn signup_user(
        &mut self,
        board_id: &str,
        email: &str,
        password: &str,
        password_hash: Option<&str>,
        role: &str,
        caller: &Principal,
    ) -> anyhow::Result<crate::auth::User> {
        crate::auth::user_signup(
            self.db.as_mut(),
            board_id,
            email,
            password,
            password_hash,
            role,
            caller,
        )
    }

    pub fn login_user(
        &mut self,
        board_id: &str,
        email: &str,
        password: &str,
    ) -> anyhow::Result<(String, String)> {
        crate::auth::user_login(self.db.as_mut(), board_id, email, password)
    }

    /// Issue a session for an already-verified user (SSO). No password check —
    /// the caller (OAuth) has verified the identity token.
    pub fn login_user_by_email(&mut self, board_id: &str, email: &str) -> anyhow::Result<(String, String)> {
        let key = crate::tables::scoped_key(board_id, &email.to_lowercase());
        let row = self
            .db
            .get(crate::tables::TABLE_USERS, &Key::text(key))?
            .ok_or_else(|| anyhow::anyhow!("user {email} not found"))?;
        let user: crate::auth::User = serde_json::from_value(row.data)?;
        let (token, jwt) = crate::auth::issue_session(self.db.as_mut(), board_id, &user.email, &user.role)?;
        Ok((token, jwt))
    }

    pub fn list_users(&self, board_id: &str) -> anyhow::Result<Vec<crate::auth::User>> {
        crate::auth::user_list(self.db.as_ref(), board_id)
    }

    pub fn set_user_role(
        &mut self,
        board_id: &str,
        email: &str,
        role: &str,
        caller: &Principal,
    ) -> anyhow::Result<crate::auth::User> {
        crate::auth::user_set_role(self.db.as_mut(), board_id, email, role, caller)
    }

    pub fn logout_user(&mut self, token: &str) -> anyhow::Result<bool> {
        crate::auth::user_logout(self.db.as_mut(), token)
    }

    pub fn user_by_token(
        &self,
        board_id: &str,
        token: &str,
    ) -> anyhow::Result<Option<crate::auth::User>> {
        crate::auth::resolve_session(self.db.as_ref(), board_id, token)
    }

    /// Look up a user by email (for SSO login).
    pub fn user_by_email(&self, board_id: &str, email: &str) -> anyhow::Result<Option<crate::auth::User>> {
        let key = crate::tables::scoped_key(board_id, &email.to_lowercase());
        Ok(self
            .db
            .get(crate::tables::TABLE_USERS, &Key::text(key))?
            .map(|r| serde_json::from_value(r.data))
            .transpose()?)
    }

    /// OAuth state store (per board, in-memory, short-lived).
    fn oauth_states(&self) -> &std::sync::Mutex<std::collections::HashMap<String, String>> {
        static STATES: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, String>>> =
            std::sync::OnceLock::new();
        STATES.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
    }

    pub fn set_oauth_state(&self, board_id: &str, state: &str) -> anyhow::Result<()> {
        self.oauth_states().lock().unwrap().insert(state.to_string(), board_id.to_string());
        Ok(())
    }

    pub fn check_oauth_state(&self, board_id: &str, state: &str) -> bool {
        self.oauth_states()
            .lock()
            .unwrap()
            .get(state)
            .map(|b| b == board_id)
            .unwrap_or(false)
    }

    /// Microsoft SSO config for a board (from its encrypted secrets).
    pub fn oauth_config(&self, board_id: &str) -> anyhow::Result<Option<crate::oauth::OAuthConfig>> {
        crate::oauth::config_from_secrets(self.db.as_ref(), board_id)
    }

    pub fn resolve_principal(
        &self,
        board_id: &str,
        token: Option<&str>,
        scope: Option<&str>,
    ) -> anyhow::Result<Principal> {
        crate::auth::resolve_principal(self.db.as_ref(), board_id, token, scope)
    }

    // ---- recipes -------------------------------------------------------

    pub fn add_recipe(&mut self, board_id: &str, recipe: &Recipe) -> anyhow::Result<()> {
        crate::automation::recipe_add(self.db.as_mut(), board_id, recipe)
    }

    pub fn list_recipes(&self, board_id: &str) -> anyhow::Result<Vec<Recipe>> {
        crate::automation::recipe_list(self.db.as_ref(), board_id)
    }

    pub fn get_recipe(&self, board_id: &str, name: &str) -> anyhow::Result<Option<Recipe>> {
        crate::automation::recipe_get(self.db.as_ref(), board_id, name)
    }

    pub fn remove_recipe(&mut self, board_id: &str, name: &str) -> anyhow::Result<()> {
        crate::automation::recipe_remove(self.db.as_mut(), board_id, name)
    }

    pub fn set_recipe_enabled(&mut self, board_id: &str, name: &str, enabled: bool) -> anyhow::Result<()> {
        crate::automation::recipe_enabled(self.db.as_mut(), board_id, name, enabled)
    }

    // ---- secrets -------------------------------------------------------

    pub fn set_secret(&mut self, board_id: &str, name: &str, value: &str) -> anyhow::Result<()> {
        crate::secrets::secret_set(self.db.as_mut(), board_id, name, value)
    }

    pub fn list_secrets(&self, board_id: &str) -> anyhow::Result<Vec<Secret>> {
        crate::secrets::secret_list(self.db.as_ref(), board_id)
    }

    pub fn get_secret(&self, board_id: &str, name: &str) -> anyhow::Result<Option<Secret>> {
        crate::secrets::secret_get(self.db.as_ref(), board_id, name)
    }

    pub fn secret_value(&self, board_id: &str, name: &str) -> anyhow::Result<Option<String>> {
        crate::secrets::secret_value(self.db.as_ref(), board_id, name)
    }

    pub fn secrets_map(&self, board_id: &str) -> anyhow::Result<std::collections::HashMap<String, String>> {
        crate::secrets::secrets_map(self.db.as_ref(), board_id)
    }

    pub fn remove_secret(&mut self, board_id: &str, name: &str) -> anyhow::Result<()> {
        crate::secrets::secret_remove(self.db.as_mut(), board_id, name)
    }

    // ---- control plane (config) ---------------------------------------

    pub fn set_rate(&mut self, board_id: &str, rate: &Json) -> anyhow::Result<()> {
        crate::policy::rate_set(self.db.as_mut(), board_id, rate)
    }

    pub fn set_ttl(
        &mut self,
        board_id: &str,
        table: &str,
        seconds: Option<i64>,
        field: Option<&str>,
    ) -> anyhow::Result<()> {
        crate::policy::ttl_set(self.db.as_mut(), board_id, table, seconds, field)
    }

    pub fn clear_ttl(&mut self, board_id: &str, table: &str) -> anyhow::Result<()> {
        crate::policy::ttl_clear(self.db.as_mut(), board_id, table)
    }

    pub fn set_link(&mut self, board_id: &str, child_table: &str, parent: &str, parent_table: &str, from_key: &str, parent_key: &str) -> anyhow::Result<()> {
        crate::policy::link_set(self.db.as_mut(), board_id, child_table, parent, parent_table, from_key, parent_key)
    }

    pub fn list_links(&self, board_id: &str) -> anyhow::Result<Vec<crate::model::Link>> {
        crate::policy::link_list(self.db.as_ref(), board_id)
    }

    pub fn get_link(&self, board_id: &str) -> anyhow::Result<Option<crate::model::Link>> {
        crate::policy::get_link(self.db.as_ref(), board_id)
    }

    // ---- sub-apps --------------------------------------------------------

    pub fn put_subapp(
        &mut self,
        board_id: &str,
        slug: &str,
        title: Option<&str>,
        index: Option<&str>,
    ) -> anyhow::Result<()> {
        crate::files::subapp_put(self.db.as_mut(), board_id, slug, title, index)
    }

    pub fn list_subapps(&self, board_id: &str) -> anyhow::Result<Vec<crate::model::SubApp>> {
        crate::files::subapp_list(self.db.as_ref(), board_id)
    }

    pub fn get_subapp(&self, board_id: &str, slug: &str) -> anyhow::Result<Option<crate::model::SubApp>> {
        crate::files::subapp_get(self.db.as_ref(), board_id, slug)
    }

    pub fn remove_subapp(&mut self, board_id: &str, slug: &str) -> anyhow::Result<bool> {
        crate::files::subapp_remove(self.db.as_mut(), board_id, slug)
    }

    pub fn clear_link(&mut self, board_id: &str) -> anyhow::Result<()> {
        crate::policy::link_clear(self.db.as_mut(), board_id)
    }

    pub fn set_computed(&mut self, board_id: &str, table: &str, map: &Json) -> anyhow::Result<()> {
        crate::schema::computed_set(self.db.as_mut(), board_id, table, map)
    }

    pub fn set_validate(&mut self, board_id: &str, table: &str, rules: &Json) -> anyhow::Result<()> {
        crate::schema::validate_set(self.db.as_mut(), board_id, table, rules)
    }

    pub fn set_redact(&mut self, board_id: &str, table: &str, paths: &Json) -> anyhow::Result<()> {
        crate::schema::redact_set(self.db.as_mut(), board_id, table, paths)
    }

    pub fn set_webhook_secret(&mut self, board_id: &str, secret: Option<&str>) -> anyhow::Result<()> {
        crate::webhooks::webhook_secret_set(self.db.as_mut(), board_id, secret)
    }

    // ---- webhooks ------------------------------------------------------

    pub fn register_hook(&mut self, board_id: &str, url: &str, secret: Option<&str>) -> anyhow::Result<()> {
        crate::webhooks::hook_register(self.db.as_mut(), board_id, url, secret)
    }

    pub fn list_hooks(&self, board_id: &str) -> anyhow::Result<Vec<crate::model::Hook>> {
        crate::webhooks::hook_list(self.db.as_ref(), board_id)
    }

    pub fn remove_hook(&mut self, board_id: &str, url: &str) -> anyhow::Result<()> {
        crate::webhooks::hook_remove(self.db.as_mut(), board_id, url)
    }

    // ---- audit ---------------------------------------------------------

    pub fn set_audit(&mut self, board_id: &str, enabled: bool) -> anyhow::Result<()> {
        crate::audit::audit_toggle(self.db.as_mut(), board_id, enabled)
    }

    pub fn audit_list(
        &self,
        board_id: &str,
        since: Option<&str>,
        limit: usize,
    ) -> anyhow::Result<Vec<Json>> {
        crate::audit::audit_list(self.db.as_ref(), board_id, since, limit)
    }

    // ---- files ---------------------------------------------------------

    pub fn upload(
        &mut self,
        board_id: &str,
        table: &str,
        filename: &str,
        content_type: &str,
        bytes: &[u8],
        meta: &Json,
        folder: Option<&str>,
    ) -> anyhow::Result<i64> {
        let seq = crate::files::file_upload(self.db.as_mut(), self.store.as_ref(), board_id, table, filename, content_type, bytes, meta, folder)?;
        self.emit(board_id, "created", seq, meta.clone());
        Ok(seq)
    }

    pub fn download(&self, board_id: &str, table: &str, file: &str) -> anyhow::Result<Option<(Vec<u8>, String)>> {
        crate::files::file_download(self.db.as_ref(), self.store.as_ref(), board_id, table, file)
    }

    pub fn list_files(&self, board_id: &str) -> anyhow::Result<Vec<crate::storage::object_store::KeyInfo>> {
        crate::files::file_list(self.store.as_ref(), board_id)
    }

    // ---- assets (per-board static hosting) ----------------------------

    pub fn put_asset(&self, board_id: &str, rel: &str, bytes: &[u8]) -> anyhow::Result<()> {
        crate::files::asset_put(self.store.as_ref(), board_id, rel, bytes)
    }

    pub fn get_asset(&self, board_id: &str, rel: &str) -> anyhow::Result<Option<(Vec<u8>, String)>> {
        crate::files::asset_get(self.store.as_ref(), board_id, rel)
    }

    pub fn head_asset(&self, board_id: &str, rel: &str) -> anyhow::Result<Option<crate::storage::object_store::BlobMeta>> {
        crate::files::asset_head(self.store.as_ref(), board_id, rel)
    }

    pub fn delete_asset(&self, board_id: &str, rel: &str) -> anyhow::Result<bool> {
        crate::files::asset_delete(self.store.as_ref(), board_id, rel)
    }

    pub fn delete_asset_prefix(&self, board_id: &str, prefix: &str) -> anyhow::Result<usize> {
        crate::files::asset_delete_prefix(self.store.as_ref(), board_id, prefix)
    }

    pub fn list_assets(&self, board_id: &str) -> anyhow::Result<Vec<crate::storage::object_store::KeyInfo>> {
        crate::files::asset_list(self.store.as_ref(), board_id)
    }

    // ---- automation trigger -------------------------------------------

    pub fn dispatch_recipes(
        &mut self,
        board_id: &str,
        table: &str,
        event: crate::events::EventKind,
        seq: Option<i64>,
        payload: Option<Json>,
    ) -> anyhow::Result<()> {
        crate::automation::dispatch(self.db.as_mut(), board_id, table, event, seq, payload)
    }

    pub fn ttl_sweep(&mut self) -> anyhow::Result<usize> {
        crate::policy::ttl_sweep(self.db.as_mut())
    }

    /// Phase A only: recipe matching + DB actions + deferred-call collection.
    /// No network I/O — safe to call while holding the engine Mutex. Execute
    /// `outcome.pending` via `automation::execute_pending` AFTER releasing the
    /// lock; write results back with `apply_call_results` if seq is Some.
    pub fn dispatch_recipes_phased_a(
        &mut self,
        board_id: &str,
        table: &str,
        event: crate::events::EventKind,
        seq: Option<i64>,
        payload: Option<Json>,
        out: &mut crate::automation::DispatchOutcome,
    ) -> anyhow::Result<()> {
        crate::automation::dispatch_phased(self.db.as_mut(), board_id, table, event, seq, payload, out)
    }

    /// Phase C: write deferred-call results back into records. Requires the
    /// engine lock (re-acquire after the HTTP phase).
    pub fn apply_call_results(
        &mut self,
        board_id: &str,
        table: &str,
        writebacks: &[(i64, Json)],
    ) {
        crate::automation::apply_call_results(self.db.as_mut(), board_id, table, writebacks);
    }

    // ---- outbound http --------------------------------------------------

    pub fn install_http_caller(&self, caller: Box<dyn crate::http::HttpCaller>) {
        crate::http::install(caller);
    }

    // ---- cron jobs ------------------------------------------------------

    pub fn add_job(
        &mut self,
        board_id: &str,
        name: &str,
        schedule: &str,
        action: &Json,
    ) -> anyhow::Result<String> {
        crate::jobs::job_add(self.db.as_mut(), board_id, name, schedule, action)
    }

    pub fn list_jobs(&self, board_id: &str) -> anyhow::Result<Vec<crate::model::Job>> {
        crate::jobs::job_list(self.db.as_ref(), board_id)
    }

    pub fn get_job(&self, board_id: &str, name: &str) -> anyhow::Result<Option<crate::model::Job>> {
        crate::jobs::job_get(self.db.as_ref(), board_id, name)
    }

    pub fn remove_job(&mut self, board_id: &str, name: &str) -> anyhow::Result<bool> {
        crate::jobs::job_remove(self.db.as_mut(), board_id, name)
    }

    pub fn job_due(&self, now_iso: &str, limit: usize) -> anyhow::Result<Vec<crate::model::Job>> {
        crate::jobs::job_due(self.db.as_ref(), now_iso, limit)
    }

    pub fn job_reschedule(
        &mut self,
        board_id: &str,
        name: &str,
        next_iso: Option<&str>,
    ) -> anyhow::Result<()> {
        crate::jobs::job_reschedule(self.db.as_mut(), board_id, name, next_iso)
    }

    pub fn job_mark(
        &mut self,
        board_id: &str,
        name: &str,
        last_run_at: &str,
        status: &str,
        message: &str,
    ) -> anyhow::Result<()> {
        crate::jobs::job_mark(self.db.as_mut(), board_id, name, last_run_at, status, message)
    }

    pub fn job_run_insert(
        &mut self,
        board_id: &str,
        job_name: &str,
        triggered_at: &str,
        duration_ms: i64,
        status: &str,
        message: &str,
        result: &str,
    ) -> anyhow::Result<()> {
        crate::jobs::job_run_insert(
            self.db.as_mut(),
            board_id,
            job_name,
            triggered_at,
            duration_ms,
            status,
            message,
            result,
        )
    }

    pub fn job_runs(
        &self,
        board_id: &str,
        job_name: Option<&str>,
        limit: usize,
    ) -> anyhow::Result<Vec<crate::model::JobRun>> {
        crate::jobs::job_runs(self.db.as_ref(), board_id, job_name, limit)
    }

    pub fn dispatch_cron_job(&mut self, board_id: &str, job_name: &str) -> anyhow::Result<()> {
        crate::automation::dispatch_cron(self.db.as_mut(), board_id, job_name)
    }

    /// Phase A of cron dispatch: DB-side recipe actions only; `$call` HTTP is
    /// collected into `out.pending` for lock-free execution by the caller.
    pub fn dispatch_cron_job_phased_a(
        &mut self,
        board_id: &str,
        job_name: &str,
        out: &mut crate::automation::DispatchOutcome,
    ) -> anyhow::Result<()> {
        crate::automation::dispatch_cron_phased(self.db.as_mut(), board_id, job_name, out)
    }
    // ---- webhook delivery queue ----------------------------------------

    pub fn hook_deliveries_due(&self, now_iso: &str, limit: usize) -> anyhow::Result<Vec<Json>> {
        crate::webhooks::hook_deliveries_due(self.db.as_ref(), now_iso, limit)
    }

    pub fn mark_hook_delivery(
        &mut self,
        id: &str,
        attempts: i64,
        last_status: Option<&str>,
        next_attempt: Option<&str>,
        delivered_at: Option<&str>,
    ) -> anyhow::Result<()> {
        crate::webhooks::hook_mark_delivery(
            self.db.as_mut(),
            id,
            attempts,
            last_status,
            next_attempt,
            delivered_at,
        )
    }
}
