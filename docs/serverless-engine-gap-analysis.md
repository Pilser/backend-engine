# Gap Analysis: From the reference serverless engine → a True Full Serverless Engine

> **Purpose:** This document is the design foundation for a dedicated, standalone **`serverlessEngine-rs`** crate + an embeddable serverless server built on it. It (a) defines what "true serverless" means, (b) audits the current reference engine against that bar, (c) specifies the abstraction layers the new crate must provide (pluggable databases, pluggable object storage), and (d) scopes where each responsibility lives — crate vs. server.
>
> Companion doc: `serverless-engine-features.md` (the complete feature inventory of the current engine).

---

## 1. Target Vision

A single Rust crate **`serverlessEngine-rs`** that is a *library* (no I/O opinion, no HTTP, no transport) implementing the complete serverless platform **core**: document store, query/filter/search/aggregate, schema+computed+validate+redact, auth/keys/scopes, recipes/automation, cron/jobs, webhooks, secrets, TTL, audit, links/relations, files, realtime event model, and a **pluggable storage layer**.

Around that crate sits a **server** (also Rust) that provides:

- **Transports:** HTTP (REST), WebSocket, SSE (and later gRPC/GraphQL).
- **Storage adapters:** DB backends (Turso/local, PostgreSQL, MySQL, embedded replica/remote libSQL, etc.) and **object storage** backends (S3, R2, GCS, MinIO, Azure Blob, local FS) — all outside the core crate.
- **Identity & policy:** OAuth/OIDC/JWT, RBAC, tenant isolation, quotas/billing hooks.
- **Tooling:** the full engine exposed as an **MCP server** (many granular tools, not one grammar blob) and a **CLI** that drives the MCP server (so the CLI and the MCP tool share one implementation).

**Guiding rule:** the crate is transport- and storage-agnostic; every persistence concern is behind traits so "all features Turso has" and "all features an object store has" can be plugged in without touching core logic.

---

## 2. What a "True Full Serverless Engine" Includes

| Capability | Description |
|---|---|
| **Document/relational hybrid store** | Schemaless docs + optional strict schemas, secondary indexes, real relations (FKs), views, transactions, full-text + vector search |
| **Compute** | Server-side code that runs on events: triggers, functions/hooks, scheduled jobs, webhooks to user code, (optionally) WASM |
| **Multi-tenancy & isolation** | First-class tenants/workspaces, row-level + column-level security, per-tenant keys/quotas, data locality |
| **Identity & auth** | Issued API keys, OAuth/OIDC/SSO, JWT, short-lived tokens, RBAC, scopes, mTLS |
| **Realtime** | Durable, resumable subscriptions; presence; offline sync/conflict resolution; ordering guarantees |
| **Async work** | Durable job queues, outbox pattern, DLQs, retries, exactly-once (or at-least-once + idempotency), distributed cron |
| **Scaling** | Multi-writer, horizontal read scaling, sharding/partitioning, connection pooling, multi-region replication, read replicas, DR |
| **Files & blobs** | Object storage abstraction, resumable uploads, signed URLs, CDN, thumbnails/transforms, multipart |
| **Search** | FTS with analyzers/ranking, vector similarity + hybrid (keyword+vector) |
| **Observability** | Structured logs, metrics (Prometheus/OTel), traces, per-request correlation, audit of admin actions |
| **Governance** | Rate limits + quotas + burst, per-tenant billing/usage meters, data residency, encryption at rest + in transit, key rotation, secrets management, SSRF hardening |
| **DevEx** | SDKs (Rust/TS/Python), CLI, MCP server, migrations tooling, playground/dashboard, environment parity (local ↔ cloud) |
| **Edge/embedded** | Run as an embedded library (single process), or as a standalone server, or multi-node |

---

## 3. Current Engine vs. Target — Gap Matrix

Legend: ✅ done · 🟡 partial (works but not at serverless scale) · ❌ absent.

| Area | Reference engine today | Gap | Priority |
|---|---|---|---|
| Document store (CRUD, patch, bulk, unique/upsert) | ✅ | — | — |
| Filter DSL / JSON-path query | 🟡 AND-only, no `$or`, no nested logic, JSON-path column casts only | Nested boolean logic, typed secondary indexes, geospatial, projections | High |
| FTS search | 🟡 Turso ngram, BM25, single-payload column | Analyzers/stemming, phrase ops, per-field indexes, highlight tuning | Medium |
| Vector search | ❌ (engine has `vector` funcs, engine never exposes them) | Vector columns, ANN indexes, hybrid search, embeddings pipeline | High |
| JSON Schema validation | ✅ (Draft 2020-12, formats on) | Schema versioning, per-record overrides, draft migration | Low |
| Computed fields / validate / redact | ✅ (expression-driven, sequential) | Dependency DAG, deterministic functions only, async computed | Medium |
| Auth (API keys + scopes + roles) | 🟡 keys only, unsalted SHA-256, no expiry/rotation, no OAuth/JWT | OAuth/OIDC/JWT, short-lived tokens, key expiry/rotation, RBAC policies, MFA | High |
| Customer scoping | ✅ (payload.customer_id forced + filtered) | Row-level security engine, column-level redaction policies | Medium |
| Rate limiting | 🟡 in-memory, fixed window, per-process | Distributed sliding window, token bucket, per-tenant quotas, quota meters | High |
| Realtime | 🟡 in-memory broadcast, no topics, no durability, lag-drop | Durable/resumable subs, per-topic routing, replays, presence, ordering | High |
| Recipes (automation) | 🟡 inline in request, one-hop, no retry, no queue | Durable outbox, async worker, distributed, retries+DLQ, exactly-once, workflow DAGs, timeouts | High |
| Cron / jobs | 🟡 single-process poller, no lease, no overlap protection | Distributed scheduler, leases, cron expressions, daylight/timezone, backfill | High |
| Webhooks | 🟡 in-process worker, plaintext secrets, base64 vs hex signatures | Signed delivery with standard schemes (HMAC hex), retry with idempotency keys, dead-letter, replay from audit | Medium |
| Secrets | 🟡 AES-GCM with single master key, dev fallback | External KMS/vault, key rotation, versioning, per-tenant keys | Medium |
| TTL | ✅ (read-time filter + sweep) | Tiered storage (hot/cold), archive instead of delete, expiration callbacks | Low |
| Links / relations | 🟡 one link per board, join single parent | Real FK constraints, many-to-many, nested join depth, eager loading, denormalization helpers | Medium |
| Audit | 🟡 payload hash only, toggle, no actor identity pipeline | Immutable append log, query by actor/time, retention policies, admin-action audit | Medium |
| Files | 🟡 local FS, plain `.bin`, no signed URLs, no resumable | Object-storage abstraction, signed URLs, resumable/multipart upload, transforms/CDN | High |
| Outbound HTTP (`/call`) | 🟡 SSRF no DNS-resolve, no allowlists | DNS-pinned SSRF protection, allow/deny lists, egress proxies, response streaming | High |
| Expressions | 🟡 capable but no array funcs, no aggregation, no async | Richer stdlib, array/aggregate funcs, user-defined functions, timezone handling | Medium |
| Multi-writer / concurrency | 🟡 Turso MVCC exists; app serializes per pool with a Mutex | True multi-connection pool per tenant, concurrent writers, conflict handling | High |
| Transactions | 🟡 single-board `BEGIN IMMEDIATE`, no savepoints | Cross-board/distributed transactions, savepoints, long-running txn handling | High |
| Multi-tenancy | 🟡 session-per-file isolation, no quotas/billing | Tenant object, per-tenant quotas/billing/resource limits, soft/hard caps | High |
| Multi-region / replication | ❌ | Read replicas, multi-writer replication (CRDT/MVCC), region pinning, failover | High |
| Object storage abstraction | ❌ | Trait + S3/R2/GCS/MinIO/local adapters, signed URLs, part sizes | High |
| DB abstraction | ❌ (hard-bound to `turso` crate + raw SQL strings) | `Database` trait: query builder / adapter per engine (Turso/libSQL, Postgres, MySQL, in-memory) | **Critical** |
| SDKs / CLI / MCP | 🟡 single mega-grammar MCP tool, no SDK | Granular MCP tools, `serverless` CLI driving the MCP server, TS/Python/Rust SDKs | High |
| Observability | ❌ (tracing logs only) | Metrics, traces, per-request IDs, structured audit of admin ops | Medium |
| Testing/quality | 🟡 good unit+integration tests, no fuzz/property, no benchmark gates | Property tests, fuzz, TPC-style benchmarks, failure injection | Medium |

---

## 4. The Storage Abstraction Layer (the heart of the new crate)

The single biggest architectural change: **stop hard-coding `turso` and raw SQL strings inside the engine** and define two traits.

### 4.1 `Database` backend trait (in the crate)

```rust
pub trait Database: Send + Sync + 'static {
    // connections / transactions
    fn connect(&self) -> impl Future<Output = Result<Conn>>;         // pooled
    fn transaction(&self) -> impl Future<Output = Result<Tx>>;
    fn savepoint(&mut self, name: &str) -> impl Future<Output = Result<()>>;

    // DDL (driven by the crate's migrator)
    fn migrate(&self, schema: &[Migration]) -> impl Future<...>;

    // DML — the crate talks to this, not to SQL strings
    fn insert(&self, table: &str, row: &Row) -> impl Future<Output = Result<Seq>>;
    fn get(&self, table: &str, pk: &Key) -> ...
    fn update(&self, table: &str, pk: &Key, patch: &Patch) -> ...
    fn delete(&self, table: &str, conds: &Conds) -> ...
    fn query(&self, q: &Query) -> Result<Cursor>;      // filters/sort/limit/offset
    fn aggregate(&self, q: &AggQuery) -> ...
    fn search(&self, q: &SearchQuery) -> ...           // FTS/vector via backend
    fn upsert(&self, table: &str, key: &UniqueKey, row: &Row) -> ...
    fn run_in_tx(&self, f: impl FnOnce(&mut Tx) -> ...) -> ...   // composition
    fn adapter(&self) -> &'static str;                 // "turso" | "postgres" | ...
}
```

Design constraints:
- **Where clauses are compiled** from the existing `FilterCond`/`SrvFilter` IR into backend-specific SQL. The crate owns the IR and the semantics (string vs numeric comparison, `contains`, `in`, `search`); each backend owns the dialect (parameter style `?` vs `$1`, JSON functions `json_extract` vs `->>`, FTS `fts_match` vs `tsvector`/`to_tsquery`).
- **Feature capability matrix** per backend, e.g.:
  - `fts: Fts { bm25: bool, ngram: bool, phrase: bool, analyzers: bool }`
  - `vector: bool`
  - `transactions: { savepoints, cross_shard }`
  - `replication: { embedded_replica, multi_writer, cdc }`
  - `json: { jsonb, json_path }`
- The **`Row`/`Value`** types are the crate's own (port `zw-db`'s `Value`/`jval` mapping), decoupling payloads from backend value types.
- A **`turso` adapter** ships in the server, not the crate (crate has only the trait + a test in-memory adapter).

Rationale: "flexible for DBs like tursodb if it has all features tursodb has" — Turso's unique advantages (MVCC multi-writer, FTS via index-method, vector functions, embedded-replica sync, encryption, partial sync) must be surfaced through capability flags so the engine can use them when present and degrade gracefully when absent.

### 4.2 Object storage trait (in the crate)

```rust
pub trait ObjectStore: Send + Sync + 'static {
    fn put(&self, key: &str, bytes: &[u8], meta: &BlobMeta) -> Result<PutInfo>;
    fn put_stream(&self, key: &str, stream: …) -> Result<PutInfo>;     // multipart
    fn get(&self, key: &str) -> Result<Stream>;                        // range-aware
    fn head(&self, key: &str) -> Result<BlobMeta>;
    fn delete(&self, key: &str) -> Result<()>;
    fn list(&self, prefix: &str) -> Result<Vec<KeyInfo>>;
    fn presign(&self, key: &str, method: Method, ttl: Duration) -> Result<Url>; // optional
    fn copy(&self, from: &str, to: &str) -> Result<()>;                // optional
    fn capabilities(&self) -> ObjectStoreCaps; // presign, multipart, versioning, copy
}
```

Adapters (server-side): local FS, S3-compatible (S3/R2/MinIO), GCS, Azure Blob. The crate treats every file as a **blob reference in a record** (`file: {store, key, sha256, size, meta}`) and keeps blob metadata + records atomic via the DB adapter (or an outbox for multi-region).

### 4.3 What stays OUT of the crate

- HTTP servers, WS/SSE framing, routers, middleware (CORS, auth extraction).
- DB drivers and object-store SDKs.
- Identity providers (OAuth/OIDC) — the crate accepts a resolved `Principal`, the server does the flows.
- Distributed job brokers/queues — the crate defines a `JobBroker` trait; the server provides Redis/Postgres/Kafka adapters.
- Realtime transport — the crate emits an event stream; the server publishes it.
- Metrics/logging sinks — the crate emits events / uses `tracing`; the server wires exporters.

This keeps `serverlessEngine-rs` a pure library: testable in-memory, embeddable in one process (like today) or behind a distributed server.

---

## 5. Detailed Gap Analysis by Domain

### 5.1 Data model & query
- **AND-only filters** → add `$or`, nested groups, `$not`, existence checks (`exists`), regex. IR already has `SrvFilter`; extend to a tree.
- **Secondary indexes**: today all queries scan `wb_records` with `json_extract`. True serverless needs declarative secondary indexes on JSON paths (unique + non-unique) so `eq`/`in`/`contains` hit indexes. Turso's `CREATE INDEX USING …` and standard indexes both apply; the crate should own index metadata in a `wb_indexes` table and let backends materialize them.
- **Projection / partial read**: fetch only needed payload paths (big win for large docs).
- **Streaming queries / cursors**: long lists currently cap at 200–500; expose keyset cursors with stable ordering.
- **Relations**: generalize one-link-per-board to N links, many-to-many via join tables, nested join depth, and referential integrity (cascade delete/update).
- **Transactions**: savepoints (currently broken/unsupported), cross-board transactions via the DB adapter, multi-record atomicity beyond `patch_many`.

### 5.2 Schema, computed, validation
- Computed fields run sequentially; a dependency DAG + cycle detection prevents silent order-dependence.
- Validation rules and JSON Schema are static; add per-record `$schema` references and draft migration.
- Redaction is read-time masking; production needs column-level policies applied server-side with audit of who saw masked vs unmasked.

### 5.3 Search
- FTS: expose phrase search, boosting, multiple searchable fields, per-board analyzer config, facet/aggregation counts, suggestions/autocomplete.
- Vector: Turso already ships `vector*` functions and `toy_vector_sparse_ivf`; the engine should add vector columns (metadata), ANN index management, `vector_distance_*` query ops, and hybrid (`search` AND/OR vector) scoring.
- Snippet/highlight is Rust-side substring; move to backend highlighters when available.

### 5.4 AuthN/AuthZ
- Key material: hash with a salt (bcrypt/scrypt/argon2) + key prefix for lookup (`wb_keys` today stores unsalted sha256), optional expiry, rotation endpoint, per-key metadata/tags.
- OAuth/OIDC + JWT validation with audience/issuer/tenant claims; short-lived access + refresh.
- RBAC: policy engine (allow/deny overrides, conditions on payload) beyond the 5-role ladder; per-customer scopes become row-level security policies.
- Multi-tenant keys: key → tenant → workspaces; cross-tenant isolation enforced in the query IR, not string manipulation.

### 5.5 Real-time
- Replace the single 1024-capacity broadcast with per-board **topics**, durable subscriber state, resumable cursors (`after` persists server-side), presence (join/leave), and ordering guarantees.
- On multi-node: the crate emits events to a `PubSub` trait; server wires Redis pub/sub / NATS / Kafka so all replicas broadcast.
- Client filtering moves server-side into topics (subscription query), not client-side match on broadcast.

### 5.6 Automation & jobs
- **Outbox pattern**: every record write appends to an outbox table in the same transaction; a durable dispatcher (not the HTTP handler) consumes it → async recipe execution, decoupling request latency from automation.
- Retry + DLQ per action; per-recipe timeout; idempotency via the existing dedup ledger (already SHA-256 keyed) extended with `attempts`/`status`.
- Distributed execution: `JobBroker` trait; scheduler with leases (multi-instance safe), overlap protection, cron with timezone/DST, missed-run backfill.
- Cross-board cascades become multi-hop (with cycle detection and a max-depth guard).

### 5.7 Webhooks
- Standard signing: hex HMAC-SHA256 (`X-Hub-Signature-256`) for outbound too (drop base64 asymmetry).
- Idempotency keys + delivery receipts; dead-letter after N; replay from the audit log; delivery retry with jitter and per-endpoint cooldown.

### 5.8 Files → Object storage
- Blob reference model + `ObjectStore` trait; metadata (sha256, size, mime, custom) stored in a record, bytes in the store.
- Signed URLs for upload/download; resumable multipart; range reads; thumbnails/transforms (server-side pipeline); CDN hooks.
- DB ↔ store consistency: write record + put blob atomically (DB outbox → worker commits blob then marks record ready).

### 5.9 Security hardening
- **SSRF**: resolve DNS and verify the IP (current code skips resolution), allowlists, egress proxy, redirect policy, response streaming caps.
- Secrets: KMS integration + key rotation + versioning; never the dev fallback in production.
- At-rest encryption per tenant; audit admin actions (key issuance, config changes).

### 5.10 Observability
- Structured logs with request/correlation IDs; Prometheus/OTel metrics for each operation family (latency, error rate, rate-limit hits, queue depths); distributed traces across recipe/hook/job boundaries.

### 5.11 Scaling & operations
- **Multi-writer**: today one mutex per session pool. The crate must pool connections and let the backend (Turso MVCC, Postgres) handle concurrency; add retry-on-`Busy` at the adapter.
- **Horizontal reads**: read replicas via backend replication.
- **Sharding**: tenant → shard mapping (the `srv_board_index` becomes a routing table).
- **Multi-region**: replication + conflict resolution strategy (last-write-wins today; CRDT/vector-clock later), region affinity.
- **Quotas & billing**: per-tenant usage meters (records, reads, bandwidth, compute time) with soft/hard caps and webhooks.

---

## 6. Proposed Module Layout for `serverlessEngine-rs`

```
serverless-engine/                    (the crate — no I/O, no transport)
├── src/
│   ├── lib.rs                        facade + version
│   ├── engine/                       orchestration (the "SrvApp" facade)
│   ├── model/                        board, record, key, recipe, secret, job, link …
│   ├── storage/
│   │   ├── database.rs               Database trait + capability matrix
│   │   ├── object_store.rs           ObjectStore trait + capability matrix
│   │   ├── ir/                       filter/query IR shared by all backends
│   │   └── memory.rs                 in-memory adapter (tests, embedded)
│   ├── crud/                         insert/get/set/patch/delete/upsert/bulk
│   ├── query/                        filter compile, search, aggregate, sort, cursor
│   ├── schema/                       json-schema, computed, validate, redact
│   ├── auth/                         keys, scopes, RBAC policy IR
│   ├── realtime/                     event model + EventStream trait (publish)
│   ├── automation/                   recipes, actions, cron, jobs (broker trait)
│   ├── webhooks/                     registry + signing
│   ├── secrets/                      encrypted store interface
│   ├── files/                        blob refs + object-store orchestration
│   ├── audit/                        immutable log
│   ├── ttl/  links/  rate/           policies
│   ├── expr/                         port of zw-expr (richer)
│   └── migrations/                   schema versioning (backend-agnostic)

serverless-server/                    (the embeddable server — adapters + transports)
├── src/
│   ├── db/                           turso, postgres, mysql adapters
│   ├── store/                        fs, s3, r2, gcs, minio, azure adapters
│   ├── broker/                       redis, postgres, in-proc job/pubsub adapters
│   ├── transport/                    rest, ws, sse (+ later grpc)
│   ├── identity/                     oauth, oidc, jwt, cookie sessions
│   ├── mcp/                          granular MCP tools over the engine
│   ├── cli/                          `srv` CLI that drives the MCP server
│   └── observability/                otel, prometheus, logs
```

---

## 7. Server Responsibilities (NOT in the crate)

1. **Storage adapters** — implement `Database` and `ObjectStore` traits (Turso local + remote/sync, Postgres, MySQL; S3/R2/GCS/MinIO/local FS).
2. **Identity provider** — OAuth/OIDC/JWT flows, cookie/session management; hand a resolved `Principal` to the crate.
3. **Transports** — REST/WS/SSE routing, middleware, CORS, body limits, TLS.
4. **Distributed primitives** — Redis/NATS/Kafka pub-sub for realtime; Redis/Postgres job broker; distributed rate limits (Redis) with the crate's `RateStore` trait.
5. **MCP server** — one granular tool per operation (unlike today's single mega-grammar) so agents get typed inputs.
6. **CLI** — a `srv`/`serverless` binary that connects to the MCP server (stdio or HTTP) and wraps the same verbs, so CLI and MCP share the implementation and stay in sync.
7. **Observability** — OTel/Prometheus exporters, correlation-ID middleware.
8. **Quota/billing** — meters reading the crate's usage events.

---

## 8. Migration Path (how we get from today to the target)

1. **Extract IR** — move `FilterCond`/`SrvFilter`/`Agg` and the compile step out of `zw-db` into the crate as the query IR.
2. **Introduce `Database` trait + Turso adapter** — port all `srv_*` functions to call the trait; keep the Turso adapter in the server. Add an in-memory adapter for tests.
3. **Introduce `ObjectStore` trait + local-FS adapter** — replace the direct `…/files/*.bin` writes with the trait; add S3/R2/GCS adapters.
4. **Event stream + outbox** — move recipe/hook/realtime dispatch off the request path onto a durable outbox consumed by a worker (still in-process at first, broker trait next).
5. **Distributed** — Redis pub/sub + broker adapters; lease-based scheduler; multi-instance safe.
6. **Identity** — OAuth/OIDC/JWT layered on the existing key model.
7. **Tooling** — granular MCP tools + `srv` CLI over MCP.
8. **Ops** — quotas, billing meters, observability, replication.

The existing reference engine is a working reference: its tests (`zw-db/tests/`, `zw-recipe/tests/`) are the behavioral contract that the new crate must pass.

---

## 9. Priority Roadmap (high signal first)

| Phase | Scope | Why first |
|---|---|---|
| **P0 — Core crate + storage traits** | Extract query IR, `Database`/`ObjectStore` traits, in-memory + Turso adapters, pass all current tests | Unblocks everything; the crate becomes the contract |
| **P0 — Outbox + async automation** | Durable recipe/hook dispatch off the request path | Removes the biggest scalability blocker (inline blocking) |
| **P1 — Distributed realtime + broker** | PubSub trait + Redis adapter; topic-based durable subs | Multi-instance becomes possible |
| **P1 — Identity** | OAuth/OIDC/JWT + key rotation | Production auth |
| **P1 — Object storage** | S3/R2/GCS adapters + signed URLs | Real file workloads |
| **P2 — Vector + hybrid search** | Vector columns, ANN index, hybrid scoring via Turso | AI-agent differentiation (the project's core use case) |
| **P2 — Scaling** | Connection pooling, read replicas, sharding, leases | Multi-tenant at volume |
| **P2 — Ops & DevEx** | Quotas/billing, observability, SDKs, granular MCP + CLI | Ship-readiness |
