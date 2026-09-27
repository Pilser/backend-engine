use crate::model::{KeyRecord, Principal, Recipe, Record, Secret, Tenant};
use crate::storage::database::{Database, DatabaseCaps};
use crate::storage::ir::{scalar_text, Agg, SrvFilter};
use crate::storage::object_store::ObjectStore;
use serde_json::{json, Value as Json};
use std::sync::Arc;

/// Realtime sink: (event kind, seq, payload). Single tenant, so no board is
/// carried — every event belongs to [`crate::TENANT`].
pub type Notify = Arc<dyn Fn(String, i64, Json) + Send + Sync>;

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

    fn emit(&self, kind: &str, seq: i64, payload: Json) {
        if let Some(f) = &self.notify {
            f(kind.to_string(), seq, payload);
        }
    }

    pub fn database(&self) -> &dyn Database {
        self.db.as_ref()
    }

    // ---- graph (HelixDB) ------------------------------------------------

    /// Link two nodes in a board's tenant by an edge label. Node ids are the
    /// Helix node ids (`$id`) returned by records tools.
    pub async fn graph_link(
        &mut self,
        from: i64,
        label: &str,
        to: i64,
        props: &Json,
    ) -> anyhow::Result<i64> {
        self.db.link(crate::TENANT, from, label, to, props).await
    }

    /// Traverse from a node along an edge label (out/in/both), up to `depth`
    /// hops. Returns reached nodes (id + data).
    pub async fn graph_traverse(
        &self,
        from: i64,
        label: Option<&str>,
        dir: &str,
        depth: usize,
    ) -> anyhow::Result<crate::storage::database::Cursor> {
        self.db.traverse(crate::TENANT, from, label, dir, depth).await
    }

    /// BM25 search over edges in a board's tenant.
    pub async fn graph_search_edges(
        &self,
        label: &str,
        property: &str,
        query: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<Json>> {
        let cursor = self.db.search_edges(crate::TENANT, label, property, query, limit).await?;
        Ok(cursor.rows.into_iter().map(|r| r.data).collect())
    }

    /// Drop an edge by its Helix edge id.
    pub async fn graph_unlink(&mut self,  edge_id: i64) -> anyhow::Result<bool> {
        self.db.unlink(crate::TENANT, edge_id).await
    }

    /// Drop a node and every edge touching it (both directions).
    pub async fn graph_delete_node(&mut self,  node_id: i64) -> anyhow::Result<usize> {
        self.db.delete_node(crate::TENANT, node_id).await
    }

    /// Materialize `*_id` payload references in a table as real Helix edges.
    /// For each record, a payload field ending in `_id` (e.g. `class_id`)
    /// names a target table (`class`/`classes`); its value is the target
    /// record's seq, resolved to a node id. An edge is created from the
    /// source record's node to the target node, labeled from the field
    /// (`class_id` -> `RELATED_CLASS`). Idempotent per (from, label, to).
    /// Returns a report of created edges.
    pub async fn graph_sync(
        &mut self,
        table: &str,
    ) -> anyhow::Result<serde_json::Value> {
        let records = self.list_records(table, 1000, None, 0, "desc").await?;
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
                    let resolved = self.graph_resolve_target(table, target_table, &ref_value).await.unwrap_or(None);
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
            match self.db.link_batch(crate::TENANT, &batch).await {
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
                        match self.db.link(crate::TENANT, *from, label, *to, &serde_json::json!({})).await {
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
    async fn graph_resolve_target(
        &self,
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
                            crate::crud::table_cond(t),
                            crate::storage::ir::FilterCond { field, op: crate::storage::ir::Op::Eq, value: val },
                        ],
                    },
                    orders: vec![],
                    limit: 1,
                    offset: 0,
                    ttl: None,
                };
                let rows = self.db.query("wb_records", &q).await?.rows;
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

    // ---- tenant (single app) --------------------------------------------
    // One Worker = one app: no create/list/delete, no owner. The tenant
    // config row is created lazily by `tenant()` on first use.

    pub async fn tenant(&mut self) -> anyhow::Result<Tenant> {
        crate::crud::tenant_config(self.db.as_mut()).await
    }

    pub async fn update_tenant(&mut self, patch: &Json) -> anyhow::Result<()> {
        crate::crud::tenant_update(self.db.as_mut(), patch).await
    }

    // ---- records -------------------------------------------------------

    pub async fn insert_record(
        &mut self,
        table: &str,
        payload: Json,
        writer: Option<&str>,
        upsert: bool,
        principal: &Principal,
    ) -> anyhow::Result<i64> {
        let seq = crate::crud::record_insert(self.db.as_mut(), table, payload.clone(), writer, upsert, principal).await?;
        self.emit("created", seq, payload);
        Ok(seq)
    }

    pub async fn bulk_insert(
        &mut self,
        table: &str,
        records: Vec<Json>,
        writer: Option<&str>,
        upsert: bool,
        principal: &Principal,
    ) -> anyhow::Result<Vec<i64>> {
        let seqs = crate::crud::record_bulk_insert(
            self.db.as_mut(),
                        table,
            records.clone(),
            writer,
            upsert,
            principal,
        ).await?;
        for (seq, record) in seqs.iter().zip(records.into_iter()) {
            self.emit("created", *seq, record);
        }
        Ok(seqs)
    }

    /// Fast bulk import for migration (see `crud::record_bulk_import`). Skips
    /// per-row recipes/webhooks/audit; run `graph_sync` after loading.
    pub async fn bulk_import(
        &mut self,
        table: &str,
        records: Vec<Json>,
        writer: Option<&str>,
        principal: &Principal,
    ) -> anyhow::Result<Vec<i64>> {
        let seqs = crate::crud::record_bulk_import(
            self.db.as_mut(),
                        table,
            records.clone(),
            writer,
            principal,
        ).await?;
        for (seq, record) in seqs.iter().zip(records.into_iter()) {
            self.emit("created", *seq, record);
        }
        Ok(seqs)
    }

    /// Import records from a JSON (array/JSONL) or CSV dump. Each row is
    /// validated against the table schema; invalid rows are skipped and
    /// reported rather than aborting the whole import.
    pub async fn import_records(
        &mut self,
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
                                table,
                record.clone(),
                Some(&principal.id),
                upsert,
                principal,
            ).await {
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

    pub async fn get_record(&self, table: &str, seq: i64) -> anyhow::Result<Option<Record>> {
        crate::crud::record_get(self.db.as_ref(), table, seq).await
    }

    pub async fn set_record(
        &mut self,
        table: &str,
        seq: i64,
        payload: Json,
        writer: Option<&str>,
    ) -> anyhow::Result<()> {
        crate::crud::record_set(self.db.as_mut(), table, seq, payload.clone(), writer).await?;
        self.emit("updated", seq, payload);
        Ok(())
    }

    pub async fn patch_record(
        &mut self,
        table: &str,
        seq: i64,
        ops: &Json,
        writer: Option<&str>,
    ) -> anyhow::Result<Json> {
        let merged = crate::crud::record_patch(self.db.as_mut(), table, seq, ops, writer).await?;
        self.emit("updated", seq, merged.clone());
        Ok(merged)
    }

    pub async fn patch_first(
        &mut self,
        table: &str,
        conds: &SrvFilter,
        ops: &Json,
    ) -> anyhow::Result<Option<Json>> {
        crate::crud::record_patch_first(self.db.as_mut(), table, conds, ops).await
    }

    pub async fn delete_record(&mut self, table: &str, seq: i64) -> anyhow::Result<bool> {
        let deleted = crate::crud::record_delete_one(self.db.as_mut(), table, seq).await?;
        if deleted {
            self.emit("deleted", seq, Json::Null);
        }
        Ok(deleted)
    }

    pub async fn delete_records(&mut self, table: &str, conds: &SrvFilter) -> anyhow::Result<usize> {
        crate::crud::record_delete_filter(self.db.as_mut(), table, conds).await
    }

    pub async fn list_records(
        &self,
        table: &str,
        limit: usize,
        before: Option<i64>,
        offset: usize,
        dir: &str,
    ) -> anyhow::Result<Vec<Record>> {
        crate::crud::record_list(self.db.as_ref(), table, limit, before, offset, dir).await
    }

    pub async fn count_records(&self, table: &str) -> anyhow::Result<i64> {
        crate::crud::record_count(self.db.as_ref(), table).await
    }

    /// Fast board-wide record count (one backend query where supported).
    pub async fn count_records_board(&self) -> anyhow::Result<i64> {
        match self.db.count_records(crate::TENANT).await {
            Ok(n) => Ok(n),
            Err(_) => {
                // Backend without a fast path: sum per-table counts.
                let mut total = 0i64;
                for cfg in crate::crud::table_list(self.db.as_ref()).await? {
                    total += crate::crud::record_count(self.db.as_ref(), &cfg.table).await?;
                }
                Ok(total)
            }
        }
    }

    pub async fn records_after(&self, after: i64) -> anyhow::Result<Vec<Record>> {
        crate::crud::records_after(self.db.as_ref(), after).await
    }

    // ---- tables ---------------------------------------------------------

    pub async fn create_table(
        &mut self,
        table: &str,
        schema: Option<Json>,
        unique_key: Option<&str>,
    ) -> anyhow::Result<crate::model::TableConfig> {
        crate::crud::table_create(self.db.as_mut(), table, schema, unique_key).await
    }

    pub async fn list_tables(&self) -> anyhow::Result<Vec<crate::model::TableConfig>> {
        crate::crud::table_list(self.db.as_ref()).await
    }

    pub async fn get_table(&self, table: &str) -> anyhow::Result<Option<crate::model::TableConfig>> {
        crate::crud::table_get(self.db.as_ref(), table).await
    }

    pub async fn drop_table(&mut self, table: &str) -> anyhow::Result<bool> {
        crate::crud::table_delete(self.db.as_mut(), table).await
    }

    // ---- query / search / aggregate / join ----------------------------

    pub async fn query_records(
        &self,
        table: &str,
        filter: &SrvFilter,
        orders: &[(String, bool)],
        limit: usize,
        offset: usize,
    ) -> anyhow::Result<Vec<Record>> {
        crate::query::query_records(
            self.db.as_ref(),
                        table,
            filter,
            orders,
            limit,
            offset,
        ).await
    }

    pub async fn search_records(
        &self,
        table: &str,
        query: &str,
        conds: &SrvFilter,
        limit: usize,
        offset: usize,
        snippet: bool,
    ) -> anyhow::Result<Vec<Record>> {
        crate::query::search_records(
            self.db.as_ref(),
                        table,
            query,
            conds,
            limit,
            offset,
            snippet,
        ).await
    }

    pub async fn aggregate_records(
        &self,
        table: &str,
        conds: &SrvFilter,
        agg: Agg,
        field: Option<&str>,
        group_by: Option<&str>,
    ) -> anyhow::Result<Vec<Json>> {
        crate::query::aggregate_records(
            self.db.as_ref(),
                        table,
            conds,
            agg,
            field,
            group_by,
        ).await
    }

    pub async fn join_list(
        &self,
        child_table: &str,
        conds: &SrvFilter,
        limit: usize,
        offset: usize,
    ) -> anyhow::Result<Vec<Record>> {
        crate::policy::join_list(self.db.as_ref(), child_table, conds, limit, offset).await
    }

    // ---- keys ----------------------------------------------------------

    pub async fn issue_key(
        &mut self,
        role: &str,
        writer: Option<&str>,
        scope: Option<&str>,
    ) -> anyhow::Result<(KeyRecord, String)> {
        crate::auth::issue_key(self.db.as_mut(), role, writer, scope).await
    }

    pub async fn list_keys(&self) -> anyhow::Result<Vec<KeyRecord>> {
        crate::auth::list_keys(self.db.as_ref()).await
    }

    pub async fn get_key(&self, bucket: &str) -> anyhow::Result<Option<KeyRecord>> {
        crate::auth::get_key(self.db.as_ref(), bucket).await
    }

    pub async fn revoke_key(&mut self, bucket: &str) -> anyhow::Result<()> {
        crate::auth::revoke_key(self.db.as_mut(), bucket).await
    }

    pub async fn signup_user(
        &mut self,
        email: &str,
        password: &str,
        password_hash: Option<&str>,
        role: &str,
        caller: &Principal,
    ) -> anyhow::Result<crate::auth::User> {
        crate::auth::user_signup(
            self.db.as_mut(),
                        email,
            password,
            password_hash,
            role,
            caller,
        ).await
    }

    pub async fn login_user(
        &mut self,
        email: &str,
        password: &str,
    ) -> anyhow::Result<(String, String)> {
        crate::auth::user_login(self.db.as_mut(), email, password).await
    }

    /// SSO login for an already-verified email (OAuth/OIDC callback path).
    /// Finds the user or creates them as reader (random unused password),
    /// then issues a session. No password check — the identity provider
    /// verified the token.
    pub async fn oauth_login(&mut self, email: &str) -> anyhow::Result<(String, String)> {
        let email = email.to_lowercase();
        let exists = crate::auth::find_user(self.db.as_ref(), &email).await?.is_some();
        if !exists {
            let owner =
                Principal { id: crate::TENANT.to_string(), role: "owner".to_string(), scope: None, writer: None };
            let pw = uuid::Uuid::new_v4().to_string();
            crate::auth::user_signup(self.db.as_mut(), &email, &pw, None, "reader", &owner).await?;
        }
        let row = crate::auth::find_user(self.db.as_ref(), &email)
            .await?
            .ok_or_else(|| anyhow::anyhow!("user {email} not found after provision"))?;
        let role = row["role"].as_str().unwrap_or("reader").to_string();
        crate::auth::issue_session(self.db.as_mut(), &email, &role).await
    }

    pub async fn list_users(&self) -> anyhow::Result<Vec<crate::auth::User>> {
        crate::auth::user_list(self.db.as_ref()).await
    }

    pub async fn set_user_role(
        &mut self,
        email: &str,
        role: &str,
        caller: &Principal,
    ) -> anyhow::Result<crate::auth::User> {
        crate::auth::user_set_role(self.db.as_mut(), email, role, caller).await
    }

    pub async fn logout_user(&mut self, token: &str) -> anyhow::Result<bool> {
        crate::auth::user_logout(self.db.as_mut(), token).await
    }

    pub async fn user_by_token(
        &self,
        token: &str,
    ) -> anyhow::Result<Option<crate::auth::User>> {
        crate::auth::resolve_session(self.db.as_ref(), token).await
    }

    pub async fn resolve_principal(
        &self,
        token: Option<&str>,
        scope: Option<&str>,
    ) -> anyhow::Result<Principal> {
        crate::auth::resolve_principal(self.db.as_ref(), token, scope).await
    }

    // ---- recipes -------------------------------------------------------

    pub async fn add_recipe(&mut self, recipe: &Recipe) -> anyhow::Result<()> {
        crate::automation::recipe_add(self.db.as_mut(), recipe).await
    }

    pub async fn list_recipes(&self) -> anyhow::Result<Vec<Recipe>> {
        crate::automation::recipe_list(self.db.as_ref()).await
    }

    pub async fn get_recipe(&self, name: &str) -> anyhow::Result<Option<Recipe>> {
        crate::automation::recipe_get(self.db.as_ref(), name).await
    }

    pub async fn remove_recipe(&mut self, name: &str) -> anyhow::Result<()> {
        crate::automation::recipe_remove(self.db.as_mut(), name).await
    }

    pub async fn set_recipe_enabled(&mut self, name: &str, enabled: bool) -> anyhow::Result<()> {
        crate::automation::recipe_enabled(self.db.as_mut(), name, enabled).await
    }

    // ---- secrets -------------------------------------------------------

    pub async fn set_secret(&mut self, name: &str, value: &str) -> anyhow::Result<()> {
        crate::secrets::secret_set(self.db.as_mut(), name, value).await
    }

    pub async fn list_secrets(&self) -> anyhow::Result<Vec<Secret>> {
        crate::secrets::secret_list(self.db.as_ref()).await
    }

    pub async fn get_secret(&self, name: &str) -> anyhow::Result<Option<Secret>> {
        crate::secrets::secret_get(self.db.as_ref(), name).await
    }

    pub async fn secret_value(&self, name: &str) -> anyhow::Result<Option<String>> {
        crate::secrets::secret_value(self.db.as_ref(), name).await
    }

    pub async fn secrets_map(&self) -> anyhow::Result<std::collections::HashMap<String, String>> {
        crate::secrets::secrets_map(self.db.as_ref()).await
    }

    pub async fn remove_secret(&mut self, name: &str) -> anyhow::Result<()> {
        crate::secrets::secret_remove(self.db.as_mut(), name).await
    }

    // ---- outbound email (Phase A: plugins) ------------------------------
    //
    // One-off send used by the `email send` CLI verb and POST /api/email/send.
    // Recipes use the `$send_email` action instead (same provider builders,
    // deferred through the recipe HTTP pipeline). Provider comes from the
    // MAIL_* secrets; values here are literal (no payload templating — that
    // lives in the recipe action).
    pub async fn send_email(
        &mut self,
        to: &str,
        subject: &str,
        text: Option<&str>,
        html: Option<&str>,
        from: Option<&str>,
    ) -> anyhow::Result<Json> {
        let secrets = self.secrets_map().await?;
        let provider = secrets
            .get(crate::email::SECRET_PROVIDER)
            .cloned()
            .unwrap_or_else(|| "resend".to_string());
        let req = crate::email::EmailRequest {
            to: to
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
            subject: subject.to_string(),
            text: text.map(String::from),
            html: html.map(String::from),
            from: from.map(String::from),
        };
        let call = crate::email::build(
            &provider,
            secrets.get(crate::email::SECRET_API_KEY).map(String::as_str),
            secrets.get(crate::email::SECRET_FROM).map(String::as_str),
            &req,
        )?;
        let (status, body) =
            crate::http::http_call_body(&call.url, &call.headers, &crate::http::HttpBody::Json(call.body), 15_000)
                .await?;
        // Direct sends (CLI/REST) fail loudly on provider rejection; the
        // recipe action instead records the status on `$.email_result`
        // ($call semantics: visible, non-fatal).
        if !(200..300).contains(&status) {
            let msg = body.to_string();
            let short = msg.chars().take(300).collect::<String>();
            anyhow::bail!("{} rejected the send (HTTP {status}): {short}", call.provider);
        }
        Ok(serde_json::json!({ "provider": call.provider, "status": status, "body": body }))
    }

    // ---- control plane (config) ---------------------------------------

    pub async fn set_rate(&mut self, rate: &Json) -> anyhow::Result<()> {
        crate::policy::rate_set(self.db.as_mut(), rate).await
    }

    pub async fn set_ttl(
        &mut self,
        table: &str,
        seconds: Option<i64>,
        field: Option<&str>,
    ) -> anyhow::Result<()> {
        crate::policy::ttl_set(self.db.as_mut(), table, seconds, field).await
    }

    pub async fn clear_ttl(&mut self, table: &str) -> anyhow::Result<()> {
        crate::policy::ttl_clear(self.db.as_mut(), table).await
    }

    pub async fn set_link(&mut self, child_table: &str, parent_table: &str, from_key: &str, parent_key: &str) -> anyhow::Result<()> {
        crate::policy::link_set(self.db.as_mut(), child_table, parent_table, from_key, parent_key).await
    }

    pub async fn list_links(&self) -> anyhow::Result<Vec<crate::model::Link>> {
        crate::policy::link_list(self.db.as_ref()).await
    }

    pub async fn get_link(&self) -> anyhow::Result<Option<crate::model::Link>> {
        crate::policy::get_link(self.db.as_ref()).await
    }

    // ---- sub-apps --------------------------------------------------------

    pub async fn put_subapp(
        &mut self,
        slug: &str,
        title: Option<&str>,
        index: Option<&str>,
    ) -> anyhow::Result<()> {
        crate::files::subapp_put(self.db.as_mut(), slug, title, index).await
    }

    pub async fn list_subapps(&self) -> anyhow::Result<Vec<crate::model::SubApp>> {
        crate::files::subapp_list(self.db.as_ref()).await
    }

    pub async fn get_subapp(&self, slug: &str) -> anyhow::Result<Option<crate::model::SubApp>> {
        crate::files::subapp_get(self.db.as_ref(), slug).await
    }

    pub async fn remove_subapp(&mut self, slug: &str) -> anyhow::Result<bool> {
        crate::files::subapp_remove(self.db.as_mut(), slug).await
    }

    pub async fn clear_link(&mut self) -> anyhow::Result<()> {
        crate::policy::link_clear(self.db.as_mut()).await
    }

    pub async fn set_computed(&mut self, table: &str, map: &Json) -> anyhow::Result<()> {
        crate::schema::computed_set(self.db.as_mut(), table, map).await
    }

    pub async fn set_validate(&mut self, table: &str, rules: &Json) -> anyhow::Result<()> {
        crate::schema::validate_set(self.db.as_mut(), table, rules).await
    }

    pub async fn set_redact(&mut self, table: &str, paths: &Json) -> anyhow::Result<()> {
        crate::schema::redact_set(self.db.as_mut(), table, paths).await
    }

    pub async fn set_webhook_secret(&mut self, secret: Option<&str>) -> anyhow::Result<()> {
        crate::webhooks::webhook_secret_set(self.db.as_mut(), secret).await
    }

    // ---- webhooks ------------------------------------------------------

    pub async fn register_hook(&mut self, url: &str, secret: Option<&str>) -> anyhow::Result<()> {
        crate::webhooks::hook_register(self.db.as_mut(), url, secret).await
    }

    pub async fn list_hooks(&self) -> anyhow::Result<Vec<crate::model::Hook>> {
        crate::webhooks::hook_list(self.db.as_ref()).await
    }

    pub async fn remove_hook(&mut self, url: &str) -> anyhow::Result<()> {
        crate::webhooks::hook_remove(self.db.as_mut(), url).await
    }

    // ---- audit ---------------------------------------------------------

    pub async fn set_audit(&mut self, enabled: bool) -> anyhow::Result<()> {
        crate::audit::audit_toggle(self.db.as_mut(), enabled).await
    }

    pub async fn audit_list(
        &self,
        since: Option<&str>,
        limit: usize,
    ) -> anyhow::Result<Vec<Json>> {
        crate::audit::audit_list(self.db.as_ref(), since, limit).await
    }

    // ---- files ---------------------------------------------------------

    pub async fn upload(
        &mut self,
        table: &str,
        filename: &str,
        content_type: &str,
        bytes: &[u8],
        meta: &Json,
        folder: Option<&str>,
    ) -> anyhow::Result<i64> {
        let seq = crate::files::file_upload(self.db.as_mut(), self.store.as_ref(), table, filename, content_type, bytes, meta, folder).await?;
        self.emit("created", seq, meta.clone());
        Ok(seq)
    }

    pub async fn download(&self, table: &str, file: &str) -> anyhow::Result<Option<(Vec<u8>, String)>> {
        crate::files::file_download(self.db.as_ref(), self.store.as_ref(), table, file).await
    }

    pub async fn list_files(&self) -> anyhow::Result<Vec<crate::storage::object_store::KeyInfo>> {
        crate::files::file_list(self.store.as_ref()).await
    }

    // ---- assets (tenant static hosting) ---------------------------------

    pub async fn put_asset(&self, rel: &str, bytes: &[u8]) -> anyhow::Result<()> {
        crate::files::asset_put(self.store.as_ref(), rel, bytes).await
    }

    pub async fn get_asset(&self, rel: &str) -> anyhow::Result<Option<(Vec<u8>, String)>> {
        crate::files::asset_get(self.store.as_ref(), rel).await
    }

    pub async fn head_asset(&self, rel: &str) -> anyhow::Result<Option<crate::storage::object_store::BlobMeta>> {
        crate::files::asset_head(self.store.as_ref(), rel).await
    }

    pub async fn delete_asset(&self, rel: &str) -> anyhow::Result<bool> {
        crate::files::asset_delete(self.store.as_ref(), rel).await
    }

    pub async fn delete_asset_prefix(&self, prefix: &str) -> anyhow::Result<usize> {
        crate::files::asset_delete_prefix(self.store.as_ref(), prefix).await
    }

    pub async fn list_assets(&self) -> anyhow::Result<Vec<crate::storage::object_store::KeyInfo>> {
        crate::files::asset_list(self.store.as_ref()).await
    }

    // ---- automation trigger -------------------------------------------

    pub async fn dispatch_recipes(
        &mut self,
        table: &str,
        event: crate::events::EventKind,
        seq: Option<i64>,
        payload: Option<Json>,
    ) -> anyhow::Result<()> {
        crate::automation::dispatch(self.db.as_mut(), table, event, seq, payload).await
    }

    pub async fn ttl_sweep(&mut self) -> anyhow::Result<usize> {
        crate::policy::ttl_sweep(self.db.as_mut()).await
    }

    /// Phase A only: recipe matching + DB actions + deferred-call collection.
    /// No network I/O — safe to call while holding the engine Mutex. Execute
    /// `outcome.pending` via `automation::execute_pending` AFTER releasing the
    /// lock; write results back with `apply_call_results` if seq is Some.
    pub async fn dispatch_recipes_phased_a(
        &mut self,
        table: &str,
        event: crate::events::EventKind,
        seq: Option<i64>,
        payload: Option<Json>,
        out: &mut crate::automation::DispatchOutcome,
    ) -> anyhow::Result<()> {
        crate::automation::dispatch_phased(self.db.as_mut(), table, event, seq, payload, out).await
    }

    /// Phase C: write deferred-call results back into records. Requires the
    /// engine lock (re-acquire after the HTTP phase).
    pub async fn apply_call_results(
        &mut self,
        table: &str,
        writebacks: &[(i64, Json)],
    ) {
        crate::automation::apply_call_results(self.db.as_mut(), table, writebacks).await;
    }

    // ---- outbound http --------------------------------------------------

    pub fn install_http_caller(&self, caller: Box<dyn crate::http::HttpCaller>) {
        crate::http::install(caller);
    }

    // ---- cron jobs ------------------------------------------------------

    pub async fn add_job(
        &mut self,
        name: &str,
        schedule: &str,
        action: &Json,
    ) -> anyhow::Result<String> {
        crate::jobs::job_add(self.db.as_mut(), name, schedule, action).await
    }

    pub async fn list_jobs(&self) -> anyhow::Result<Vec<crate::model::Job>> {
        crate::jobs::job_list(self.db.as_ref()).await
    }

    pub async fn get_job(&self, name: &str) -> anyhow::Result<Option<crate::model::Job>> {
        crate::jobs::job_get(self.db.as_ref(), name).await
    }

    pub async fn remove_job(&mut self, name: &str) -> anyhow::Result<bool> {
        crate::jobs::job_remove(self.db.as_mut(), name).await
    }

    pub async fn job_due(&self, now_iso: &str, limit: usize) -> anyhow::Result<Vec<crate::model::Job>> {
        crate::jobs::job_due(self.db.as_ref(), now_iso, limit).await
    }

    pub async fn job_reschedule(
        &mut self,
        name: &str,
        next_iso: Option<&str>,
    ) -> anyhow::Result<()> {
        crate::jobs::job_reschedule(self.db.as_mut(), name, next_iso).await
    }

    pub async fn job_mark(
        &mut self,
        name: &str,
        last_run_at: &str,
        status: &str,
        message: &str,
    ) -> anyhow::Result<()> {
        crate::jobs::job_mark(self.db.as_mut(), name, last_run_at, status, message).await
    }

    pub async fn job_run_insert(
        &mut self,
        job_name: &str,
        triggered_at: &str,
        duration_ms: i64,
        status: &str,
        message: &str,
        result: &str,
    ) -> anyhow::Result<()> {
        crate::jobs::job_run_insert(
            self.db.as_mut(),
                        job_name,
            triggered_at,
            duration_ms,
            status,
            message,
            result,
        ).await
    }

    pub async fn job_runs(
        &self,
        job_name: Option<&str>,
        limit: usize,
    ) -> anyhow::Result<Vec<crate::model::JobRun>> {
        crate::jobs::job_runs(self.db.as_ref(), job_name, limit).await
    }

    pub async fn dispatch_cron_job(&mut self, job_name: &str) -> anyhow::Result<()> {
        crate::automation::dispatch_cron(self.db.as_mut(), job_name).await
    }

    /// Phase A of cron dispatch: DB-side recipe actions only; `$call` HTTP is
    /// collected into `out.pending` for lock-free execution by the caller.
    pub async fn dispatch_cron_job_phased_a(
        &mut self,
        job_name: &str,
        out: &mut crate::automation::DispatchOutcome,
    ) -> anyhow::Result<()> {
        crate::automation::dispatch_cron_phased(self.db.as_mut(), job_name, out).await
    }
    // ---- webhook delivery queue ----------------------------------------

    pub async fn hook_deliveries_due(&self, now_iso: &str, limit: usize) -> anyhow::Result<Vec<Json>> {
        crate::webhooks::hook_deliveries_due(self.db.as_ref(), now_iso, limit).await
    }

    pub async fn mark_hook_delivery(
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
        ).await
    }
}
