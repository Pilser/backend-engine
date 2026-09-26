# serverlessEngine — Full Feature Reference

> **Workers-port deltas (2026-09-26, see `PORT-TRACK.md`):** single tenant —
> no `{board}` routing, no app create/list/delete (one `TENANT` row); schema
> validation is an allowlist subset (required/types/properties/items/enum/
> ranges), NOT the `jsonschema` crate; passwords are salted SHA-256 (`v1$…`,
> no bcrypt); Microsoft SSO removed; engine + storage seam are `async`
> (`async_trait(?Send)`); links filter by `child_board` (the old `$.board_id`
> filter never matched — fixed); `GET /api/events` is 501 until the
> `TenantDO` fan-out lands. The `§0` status table and `/api/srv/{board}`
> paths below describe the reference daemon, not this worker.

> **Scope:** This document covers ONLY the serverless engine (the `zw-db` data layer, the `zw-expr` expression language, the `zw-recipe` automation engine, and the `ch-web-ui` HTTP/WebSocket/SSE transport that exposes it). It does **not** cover the chat, whiteboard, materials, channel, or ZeroClaw plumbing of zerowrapper, except where they intersect the engine.
>
> Source of truth: `zerowrapper/zw-db/`, `zerowrapper/zw-expr/`, `zerowrapper/zw-recipe/`, `zerowrapper/crates/ch-web-ui/src/routes/serverless{,_ws,_sse}.rs`, `zerowrapper/zw-mcp/src/srv.rs`.

---

## 0. serverlessEngine-rs Implementation Status

> Live tracking of what the new engine (`serverless-engine/`) has shipped vs the reference below.
> **Legend:** ✅ done (live-tested) · ⚠️ partial/stub · 🚧 planned (not started) · 🔴 deferred by design · ❌ not in reference scope.

| § | Feature | Status | Notes |
|---|---|---|---|
| §3–4 | Board model + lifecycle (create/get/list/update/delete) | ✅ | `crud.rs`; export/import 🚧 |
| §5 | Record CRUD (insert/upsert/set/patch/delete/bulk/after/count) | ✅ | incl. Mongo `$` patch ops |
| §6 | Filter DSL, sort, search, aggregate | ✅ | search = in-memory token match, **not** BM25 🚧 |
| §7 | Schema / computed / validate / redact | ✅ | computed: sequential, no DAG (matches ref) |
| §8 | Unique keys + upsert | ✅ | |
| §9 | TTL (read-time + sweeper) | ✅ | sweeper 60s in `server/jobs.rs` |
| §10 | Realtime WS + SSE | ⚠️ | SSE ✅; **WS 🚧** |
| §11 | Auth/keys/scopes | ✅ | salted-hash improvement 🚧 |
| §12 | Rate limiting | ✅ | in-memory |
| §13 | Secrets (AES-256-GCM) | ✅ | |
| §14 | Webhooks (out+in) | ⚠️ | registration + HMAC ✅; **delivery worker + real HTTP egress 🚧** |
| §15 | Recipes + actions | ⚠️ | core actions ✅; `dedup_on`, `$format/$merge/$push/$pull/$sort/$slice/$set_state`, `$schedule` 🚧 |
| §16 | Cron + scheduler | 🚧 | only TTL sweeper exists |
| §17 | Links/joins | ✅ | |
| §18 | Audit | ✅ | |
| §19 | Files (upload/download/list) | ✅ | + **asset hosting** (§19a, new) |
| §20 | Outbound HTTP `$call` | ⚠️ | `srv_http_call` is a stub; **real reqwest egress 🚧** |
| §21 | Expression language | ✅ | extended stdlib beyond ref (map/filter/reduce/…) |
| §22 | Two-tier DB layout | 🔴 | deferred; DB adapter is LAST (§0.1) |
| §23 | Templates/materials | ❌ | out of engine scope |
| §24 | HTTP API surface | ✅ | both data + admin planes wired; path-shape deltas noted in §24 |
| §25 | MCP/CLI surface | ✅ | 17 verbs; granular subset of ref grammar |
| §26 | Cross-cutting limits | — | see §26 notes |
| §27 | **Cache layer (RAM + DB)** | 🚧 | design in §27 (new, optional) |

### 0.1 Implementation order (decision, session 2026-08-16)

1. Everything EXCEPT the DB adapter is implemented and tested against the in-memory `Database`/`ObjectStore` adapters first.
2. The **DB adapter is deliberately LAST**. Then it is validated by running the same conformance suite against **SQLite, Postgres, and Turso** to prove backends can be swapped without degradation or loss of functionality (the `Database` trait + capability matrix is the swap seam).
3. Cache (§27) is an optional acceleration layer on top — volatile in RAM, durable truth in DB — so it never masks a backend correctness problem.

---

## 1. Executive Summary

serverlessEngine is an embeddable, schema-flexible document/database platform. It gives every board (a.k.a. *serverless app*) a JSON-document store with:

- MongoDB-style patch operations, JSON-path filtering, BM25 full-text search, aggregation
- Optional JSON Schema (Draft 2020-12) validation, expression-driven computed fields, validation rules, and read-time redaction
- Multi-key auth (admin / writer / reader / list / customer-scoped) with per-action rate limiting
- Realtime via WebSocket and Server-Sent Events
- Event-driven automation ("recipes") with dedup, cron scheduling, and webhooks (in/out, HMAC-signed)
- Encrypted at-rest secrets, audit trail, TTL, cross-board links, file uploads
- A full MCP tool (`srv …`) that mirrors the entire surface as a CLI-like grammar

The storage engine is backed by **HelixDB** (`helix` backend, default): a graph +
vector + full-text engine reached over raw `POST /v2/query` JSON through the
`crates/helixdb` client. One Helix tenant per board; engine metadata lives in the
reserved `__srv__` label namespace; BM25 full-text search runs on a lazily-created
text index over each table's `_search` property. The `Database` trait keeps the
engine adapter-agnostic (an in-memory adapter remains for tests/embedding).

---

## 2. Architecture

```
┌──────────────────────────────────────────────────────────────────┐
│                      ch-web-ui (transport)                       │
│  ┌──────────────┐   ┌───────────────┐   ┌──────────────────────┐ │
│  │ REST /api/srv│   │ WS /api/srv/  │   │ SSE /api/srv/        │ │
│  │ {board_id}/… │   │ {board_id}/ws │   │ {board_id}/events    │ │
│  └──────┬───────┘   └──────┬────────┘   └──────────┬───────────┘ │
│         │   Broker (in-memory broadcast, cap 1024) │             │
└─────────┼──────────────────┼───────────────────────┼─────────────┘
          ▼                  ▼                       ▼
┌──────────────────────────────────────────────────────────────────┐
│                          zw-db (engine)                          │
│  serverless.rs   filters.rs    logic.rs     keys.rs    rate.rs   │
│  ttl.rs          links.rs      audit.rs     secrets.rs netguard │
│  recipes.rs      cron.rs       jobs.rs      hooks.rs   calls.rs  │
│  hook_deliveries templates.rs  users.rs     sessions.rs          │
│  session_store.rs migrations.rs                                  │
│                    │                                             │
│                    ▼                                             │
│               helix (HelixDB via POST /v2/query)                 │
│               - one tenant per board                             │
│               - __srv__ labels for engine metadata               │
│               - records = nodes, BM25 on _search                 │
└──────────────────────────────────────────────────────────────────┘
          │                    │                     │
          ▼                    ▼                     ▼
┌──────────────────────────────────────────────────────────────────┐
│  zw-expr (expression engine)     zw-recipe (automation engine)   │
│  parse / apply / truthy /        Engine · dispatch · actions ·   │
│  get_path / set_path             secrets · context · rule        │
└──────────────────────────────────────────────────────────────────┘
```

Two independent "worker" loops (in-process tokio tasks, spawned by `ch-web-ui`):
- **Webhook worker** — polls due webhook deliveries every 2 s, batch of 64, exponential backoff.
- **Scheduler** — polls due `app_jobs` every 15 s, batch of 32, runs HTTP/poll actions and `cron:` recipes.
- **TTL sweeper** — runs every 30 s, deletes expired records across all session DBs.

---

## 3. Core Data Model

### 3.1 Board (SrvApp)

A board is the top-level container — the "serverless app". Stored as a row in `wb_apps` (session DB).

| Field | Type | Notes |
|---|---|---|
| `board_id` | `TEXT` | PK. Generated as `b_` + 16 base-36 chars of nanoseconds (monotonic-ish). Doubles as the **legacy master admin token**. |
| `owner_key` | `TEXT` | The owning session id. This is the owner's admin key. |
| `title` | `TEXT` | Human label. |
| `schema_json` | `TEXT` | Optional JSON Schema (Draft 2020-12). |
| `public_reads` | `INTEGER` | 1 = anonymous reads allowed. |
| `unique_key` | `TEXT` | Optional JSON path for upsert conflict detection. |
| `computed_json` | `TEXT` | Map of `{ "<path>": "<expr>" }`. |
| `validate_json` | `TEXT` | Array of `{ "when": "<expr>", "error": "msg" }`. |
| `redact_json` | `TEXT` | Array of JSON paths masked on read. |
| `rate_json` | `TEXT` | Rate-limit config. |
| `ttl_seconds` / `ttl_field` | `INTEGER` / `TEXT` | Expiry policy. |
| `audit` | `INTEGER` | 0/1 toggle for the audit trail. |
| `webhook_secret` | `TEXT` | Secret for verifying inbound events. |
| `record_count` | computed | `COUNT(*)` via subquery in reads. |
| `created_at` | `TEXT` | ISO timestamp. |

### 3.2 Record (SrvRecord)

| Field | Type | Notes |
|---|---|---|
| `seq` | `INTEGER` | Per-board monotonically increasing primary key (computed as `MAX(seq)+1`, not SQLite rowid). |
| `payload` | `TEXT` (JSON) | The free-form document. |
| `created_at` | `TEXT` | ISO timestamp. |
| `writer` | `TEXT` | Optional identity of the writer (from API key `writer` field, `X-Writer` header, or `?writer=` param). |
| `score` | `REAL` | Optional — only populated by FTS search (BM25). |
| `snippet` | `TEXT` | Optional — `<mark>`-highlighted snippet from search (`hl=1`). |

### 3.3 Table inventory

**System DB** (`migrations.rs` SYSTEM_SCHEMA):

| Table | Purpose |
|---|---|
| `users` | Accounts (id, external_id, name, role, password_hash). |
| `sessions` | Session/agent rows (id, user_id, agent_alias, …). |
| `wb_keys` | API keys (bucket, board_id, key_hash=sha256, role, writer, scope, revoked_at). |
| `wb_hook_deliveries` | Outbound webhook delivery queue (attempts, last_status, next_attempt_at). |
| `srv_board_index` | `board_id → session_id → owner_key` mapping. |
| `app_jobs` | Cron jobs (schedule, action JSON, next_run_at, last_status). |
| `wb_job_runs` | Job execution history. |
| `wb_public_index` | Public whiteboard page slugs. |
| `templates` | Material template registry (template_id, current_version). |

**Session DB** (`migrations.rs` SESSION_SCHEMA) — one file per session:

| Table | Purpose |
|---|---|
| `wb_apps` | Boards (see 3.1). |
| `wb_records` | Records (board_id, seq, payload, writer, created_at) + **FTS index** `USING fts (payload) WITH (tokenizer='ngram')`. |
| `wb_hooks` | Outbound webhook registrations (url, secret in plaintext). |
| `wb_links` | Cross-board link config. |
| `wb_audit` | Append-only audit trail. |
| `wb_recipes` | Automation recipes (when_json, match_json, dedup_on, actions_json). |
| `wb_recipe_runs` | Dedup ledger (PK `board_id, recipe, dedup_key`). |
| `wb_recipe_runs_log` | Run history (status, message, duration_ms). |
| `wb_app_secrets` | AES-256-GCM encrypted secrets. |
| `whiteboard_*` | Whiteboard pages/meta (engine-adjacent, not part of the API). |

---

## 4. Board Lifecycle

- **Create** — `srv_app_create(owner, title, schema_json?, public_reads?, unique_key?)` → new `board_id`. Also `srv_app_create_with_id` (used for material templates `template@version`).
- **Get** — `srv_app_by_id(board_id)` / `srv_app_owned(board_id, owner)` (owner-guarded).
- **List** — `srv_app_list(owner)` ordered by `created_at`.
- **Update** — `srv_app_update(board_id, owner, title?, schema?, public_reads?, unique_key?)`; each field optional; `None` clears.
- **Delete** — `srv_app_delete(board_id, owner)` — removes board + all its records in a single `BEGIN…COMMIT`.
- **Export / Import** — `srv apps export <board>` produces `{format:"srv-app", app, rate, ttl, audit, link, hooks, records}` (records paged 500/batch); `srv apps import` recreates board, bulk-inserts records, restores rate/ttl/audit/link/hooks. Hook secrets are re-registered without the secret.

---

## 5. Record CRUD

All writes funnel through `prepare_payload` which runs, in order:
1. **computed fields** (`apply_computed`)
2. **custom validation rules** (`validate_rules`)
3. **JSON Schema validation** (allowlist subset in the worker port; was the `jsonschema` crate with formats enabled)

### 5.1 Insert & Upsert — `srv_record_insert(board_id, payload, writer, upsert)`
- Computes next `seq = MAX(seq)+1`, inserts in a transaction, then appends an audit `created` row.
- If the board has a `unique_key` (a JSON path): the scalar value at that path is checked against existing records. On conflict: without `upsert` → error `duplicate value … for unique field …`; with `upsert` → the existing record is **replaced** via `srv_record_set` (identity/seq preserved) and its seq returned.

### 5.2 Bulk insert — `srv_record_bulk_insert(board_id, records, writer, upsert)`
- Sequential loop over `srv_record_insert`; each record validated; a failure aborts the batch (no partial rollback). Returns the list of seqs.

### 5.3 Get — `srv_record_get(board_id, seq)`
- Raw variant (`_raw`) skips redaction; public variant applies the board's redaction.

### 5.4 Replace — `srv_record_set(board_id, seq, payload, writer)`
- Preserves identity and `created_at`, replaces payload, re-validates, re-checks uniqueness (excluding self), appends audit `updated`.

### 5.5 Patch — `srv_record_patch(board_id, seq, ops)`
Two accepted shapes:
1. **Shallow merge** — a plain object `{ "a": 1 }` → top-level keys merged.
2. **MongoDB-style operators** — an object whose keys start with `$`:
   - `$set` — `{ "$set": { "a.b.c": v, "items.2": v } }` (dot/index paths)
   - `$unset` — array of paths or object of keys
   - `$inc` / `$dec` — path → numeric delta (missing value treated as 0; integer-preserving)
   - `$mul` — path → numeric factor
   - any other key that parses as a path → treated as a direct `set_path` (so bare dotted keys work too)

Patch re-validates against schema, re-checks uniqueness, appends audit `updated`, and **returns the merged payload**.

### 5.6 Patch first match — `srv_record_patch_first(board_id, conds, ops)`
Finds the first record matching the filter (by `seq ASC`), applies `srv_record_patch`. Returns merged payload or `None`. This is a **raw write path** — it never re-dispatches recipes.

### 5.7 Atomic multi-step patch — `srv_record_patch_many(board_id, steps)`
Takes `(filter, ops)` steps, runs all under a single **`BEGIN IMMEDIATE` transaction** (on the board's write lock): all succeed or none. Returns seqs of patched records. Used by the recipe `$transaction` action.

### 5.8 Delete
- `srv_record_delete(board_id, seq)` — single row. `seq=None` deletes **all** records of the board.
- `srv_record_delete_filter(board_id, conds)` — deletes every matching row, returns count.
- `srv_record_delete_one(board_id, seq)` — returns `bool` (whether removed), appends audit `deleted`.

### 5.9 List & pagination
- `srv_record_list(board_id, limit, before, offset)` — newest-first (`seq DESC`), `limit` clamped `1..=200`. Two cursor styles: `before=<seq>` (records with `seq < before`) and `offset`.
- `srv_record_count(board_id)` — `COUNT(*)`.
- `srv_records_after(board_id, after_seq)` — ascending `seq > after_seq`, capped at 1000 (used by realtime backfill).

---

## 6. Querying & Filtering

### 6.1 Filter DSL (`filters.rs`)

A filter is a list of conditions (`SrvFilter { conds: [FilterCond] }`) — **implicit AND only**; `$or` is rejected as "not supported yet".

`FilterCond { field: <JSON path>, op: Op, value: <JSON> }`.

**Operators (`Op`):**

| Op | SQL form |
|---|---|
| `eq` | `CAST(json_extract(payload, $path) AS TEXT) = ?` (or REAL cast when the value is numeric) |
| `ne` | `CAST(… AS TEXT) <> ?` |
| `gt` / `gte` / `lt` / `lte` | comparison against REAL (numeric value) or TEXT cast |
| `contains` | `LOWER(text) LIKE LOWER('%escaped%') ESCAPE '\'` |
| `not_contains` | `LOWER(text) NOT LIKE …` |
| `in` | `text IN (?,?,…)` (one placeholder per array element) |
| `search` | `fts_match(payload, ?)` → routed through the Turso FTS index |
| `writer eq` | special-cased to the real `writer` column |

**JSON filter formats** accepted by `parse_filter`:
1. **Array** of condition objects `[{field, op, value}, …]`
2. **Single object** with a `field` key
3. **Search object** `{ "search": "q", "brand": {"eq": "X"} }` → `search(q) AND brand=X`
4. **Plain object** — each key is a field path, each value an op-map or bare value (bare → `eq`); `$and` accepted (recursive array); `$or` rejected.

Path parsing: `a.b[2].c`, leading `$` optional; `PathSeg::Key` / `PathSeg::Idx`.

### 6.2 Sorting
`orders: &[(&str, bool)]` (field, desc) → `ORDER BY json_extract(payload, ?) ASC|DESC`, always with `seq DESC` appended. With no explicit orders: search → `ORDER BY fts_score(payload, ?) DESC` (BM25), else `seq DESC`. `LIMIT` clamped `1..=500`, optional `OFFSET`.

### 6.3 Full-text search — `srv_record_search(board_id, query, conds, limit, offset, snippet)`
- Index: `CREATE INDEX … USING fts (payload) WITH (tokenizer='ngram')` (Turso index method, Tantivy engine).
- **ngram tokenizer** ⇒ substring/partial-token matching ("lapt" hits "laptop").
- **BM25 scoring** (Tantivy `order_by_score`), surfaced as `score`.
- **Snippet/highlight**: when `snippet=true`, the engine generates a snippet in Rust — case-insensitive `<mark>…</mark>` around each query token (mirroring the ngram "partial token matches" semantics). (`fts_highlight()` exists in the engine but is not used.)

### 6.4 Aggregation — `srv_record_aggregate(board_id, conds, agg, field, group_by)`
- `agg` ∈ `count | sum | avg | min | max`.
- `sum/avg/min/max` require a `field` (JSON path); SQL: `FUNC(CAST(json_extract(payload, ?) AS REAL))`.
- Optional `group_by` (JSON path) → `GROUP BY json_extract(…) ORDER BY 1`.
- Output rows: `{"value": v}` or `{"group": g, "value": v}`; NULLs preserved.

---

## 7. Schema, Computed, Validate, Redact (`logic.rs`)

### 7.1 JSON Schema validation
- `wb_apps.schema_json` validated on **every write** (worker port: allowlist validator; was the `jsonschema` crate with `should_validate_formats(true)`).
- Errors: `payload failed schema: …`.

### 7.2 Computed fields (`computed_json`)
- Format: JSON **object map** `{ "<path>": "<expr string>", … }` e.g. `{ "$.total": "$.price * $.qty", "$.tax": "$.total * 0.2" }`.
- Evaluation order = JSON key order; **later computed fields can reference earlier ones** (results materialize into the payload as evaluation proceeds). No dependency DAG — order is whatever you wrote.
- Applied in `prepare_payload` before validation. Missing inputs resolve to `null`.
- `set_path` auto-creates missing objects/arrays.

### 7.3 Validation rules (`validate_json`)
- Format: array of `{ "when": "<expr>", "error": "<message>" }`.
- A rule **blocks the write** when `when` is truthy; first failing rule wins; message returned to the caller. Missing `error` → `"validation failed"`.

### 7.4 Redaction (`redact_json`)
- Format: array of JSON paths, e.g. `["$.ssn", "$.api_key"]`.
- Applied **on read only** (DB retains plaintext): matching values replaced with `"***"`. Path `$` (root) masks the entire payload. Applies to `get`, `list`, `filter`, `search`, `query`, and linked-parent payloads. Raw variants used by internal consumers skip masking.

---

## 8. Unique Keys & Upsert

- Set once at board creation or via update (`unique_key` = JSON path).
- On insert/set/patch: the scalar at that path is looked up (`json_extract` cast to TEXT) — if a different record holds the same value, write is rejected **unless** `upsert` (insert path) — then the existing record is replaced and its seq returned.
- Excludes the current seq on update/patch so self-replacement is allowed.

---

## 9. TTL (Time To Live) — `ttl.rs`

Two independent expiry mechanisms (combined with OR):
- **Global age**: `ttl_seconds` — records with `created_at < now - N seconds` are dead.
- **Per-record field**: `ttl_field` (JSON path) — dead once `CAST(json_extract(payload, path) AS TEXT) < datetime('now')` (i.e. the field holds an expiry timestamp string) and the field is non-null.

Enforcement is twofold:
- **Read-time filtering**: every list/query/filter/count/aggregate/delete/after wraps the TTL clause as `NOT (ttl_frag)` so expired records are invisible.
- **Background sweep**: `srv_ttl_sweep()` deletes dead rows (per distinct `ttl_seconds`, then per `ttl_field` board); run every 30 s by the TTL sweeper across all session DBs. Returns deleted count.

---

## 10. Realtime — WebSocket & SSE

- **WS** — `GET /api/srv/{board_id}/ws` (subprotocol `bearer`).
- **SSE** — `GET /api/srv/{board_id}/events` (event type `record`); KeepAlive enabled.
- **Query params (both)**: `after=<seq>` (replay `seq > after` as `created` events before going live; capped 1000) and `filter=<url-encoded JSON>` (client-side object filter of `key → value`, AND over dot paths; numbers compare numerically, else serde equality).
- **Event shape** (server → client, text frame):

```json
{ "board": "b_…", "type": "record.created|record.updated|record.deleted",
  "seq": 42, "record": { "seq": 42, "payload": {…}, "created_at": "…", "writer": "…" } }
```

- Payload is redacted server-side per board config before broadcast.
- **Broker**: one in-memory `tokio::sync::broadcast` channel (capacity 1024) shared by all boards. **No topics** — every event goes to every subscriber; consumers filter by the `board` field. Lagging subscribers get `Lagged` and keep going.
- Auth: WS requires a valid read auth (`bearer` key, `?key=`, or public board). **SSE forbids customer-scoped keys**.
- There is **no client→server message protocol** — one connection per board; filtering is a query param, not a subscribe message.

---

## 11. Authentication & Authorization (`keys.rs`, `ch-web-ui` auth layer)

### 11.1 Credential sources (resolution order)
1. `Authorization: Bearer <token>` header
2. `Sec-WebSocket-Protocol: bearer, <token>` subprotocol (WS)
3. `?key=<token>` query param

### 11.2 Identities
- **Anonymous** — no token. Reads allowed only when `public_reads`; writes allowed only when the board has **no** keys at all.
- **LegacyAdmin** — passing the board id itself as the token = full admin.
- **AppKey** — token is SHA-256-hashed and matched against `wb_keys` (non-revoked, `LIMIT 1`). Must belong to the requested board.

### 11.3 Roles & rank
`customer` < `reader`(=rank1) = `list`(=rank1) < `writer`(=rank2) < `admin`(=rank3).

| Role | Reads | Writes | Admin endpoints | Notes |
|---|---|---|---|---|
| `admin` | ✓ | ✓ | ✓ | owner-level |
| `writer` | ✓ | ✓ | ✗ | |
| `reader` / `list` | ✓ | ✗ | ✗ | `list` intended for listing |
| `customer` | ✓ (scoped) | ✓ (scoped) | ✗ | **requires** `scope`; writes forced into `payload.customer_id` |
| (none/unknown) | 403 | 403 (unless no keys exist) | ✗ | |

### 11.4 Customer scoping
- Customer keys carry an immutable `scope` (a customer id).
- **On write** (`force_scope`): `payload["customer_id"]` is overwritten with the scope before insert.
- **On read**: `customer_scope` appends `$.customer_id == scope` to every query, and get/put/patch/delete return 404 for foreign records.
- **Forbidden for customer keys**: aggregate, join, SSE, bulk-delete (403).

### 11.5 Key issuance
- `POST /api/srv/{board_id}/keys` (admin) — body `{role, writer?, scope?}` (role defaults `writer`).
- Returns `{bucket: <uuid>, key: <uuid secret>, role, writer, scope}` — **plaintext secret shown once**; only the SHA-256 hash is stored.
- `customer` requires `scope`; only `customer` may carry a scope.
- Revocation is **soft** (`revoked_at = datetime('now')`).

---

## 12. Rate Limiting (`rate.rs`)

- Per-board config: `submit=120, upload=20, search=240, read=600` per 60 s rolling fixed window + `per_day=100_000`.
- **In-memory** limiter (Mutex<HashMap>), two fixed windows per key: `{key}:{action}` (60 s) and `{key}:day` (24 h). Allowed only if **both** under their limits. Entries pruned when > 10 000.
- Client key: token-based `{board_id}:kw:{sha256(token)[..16]}` if a bearer token is present, else `X-Forwarded-For` first IP, else `{board_id}:anon`.
- Rejection: `429 {"error":"rate limited","retry_after":N}` with `Retry-After` + `X-RateLimit-Remaining`.
- Note: **in-memory only** — not shared across processes; resets on restart.

---

## 13. Secrets (`secrets.rs`)

- Per-board key-value store `wb_app_secrets` (names stored **uppercased**).
- **Encryption at rest**: AES-256-GCM, fresh random 12-byte nonce per value, output `base64(nonce ‖ ciphertext)`. Master key derived from env `SRV_SECRET_KEY` (32 bytes, cycled); dev fallback otherwise.
- Fingerprint = SHA-256 of plaintext (so lists identify secrets without exposing them).
- Secrets are decrypted **only for the recipe engine** (`srv_secrets_map`) — never serialized to clients.

---

## 14. Webhooks (`hooks.rs`, `hook_deliveries.rs`)

### 14.1 Outbound (deliveries)
- Register: `POST /api/srv/{board_id}/hooks` `{url, secret?}` — URL scheme validated (SSRF), ownership checked, secret stored **in plaintext** in `wb_hooks`.
- On every record event (created/updated/deleted), a delivery row is enqueued per registered URL in the **system-DB** queue `wb_hook_deliveries` (event JSON captured).
- Worker (every 2 s, batch 64) POSTs to the URL with:
  - body = the event JSON with `hooks_secret` injected
  - header `X-Srv-Signature: base64(HMAC-SHA256(body, secret))`
  - 10 s timeout, SSRF guard per URL.
- **Retry**: max 5 attempts, backoff `[0, 1, 5, 30, 120]` s. 2xx → delivered; non-2xx → backoff; 5th failure → dormant (undelivered).
- Replay: `srv_hook_reschedule` re-dues all undelivered attempts ≤ 5.

### 14.2 Inbound (events)
- `POST /api/srv/{board_id}/events` (body cap 10 MB, no bearer token required).
- If the board has a `webhook_secret`: requires header `X-Hub-Signature-256: sha256=<hex>` matching `HMAC-SHA256(body, secret)` (hex), else 401.
- On success (202) it runs every board recipe with `EventKind::Inbound`, with board secrets injected.

> Signature asymmetry: outbound uses **base64** `X-Srv-Signature`; inbound uses **hex** `X-Hub-Signature-256` (GitHub-style).

---

## 15. Recipes — Event-Driven Automation (`zw-recipe` + `zw-db/recipes.rs`)

### 15.1 Recipe model (`wb_recipes`)
| Field | Meaning |
|---|---|
| `name` | unique per board |
| `when_json` | trigger: `record.created` / `record.updated` / `record.deleted` / `inbound` / `cron:<job_name>` |
| `match_json` | extra condition filter evaluated against the event payload |
| `enabled` | 0/1 |
| `dedup_on` | JSON path whose value gates dedup |
| `actions_json` | array of action objects |

### 15.2 Action kinds (`actions.rs`)

| Kind | Behavior |
|---|---|
| `$compute` | evaluate `zw_expr` expressions, set each field |
| `$set` | secret/template-substituted deep field set |
| `$copy` / `$move` | copy / move value between paths |
| `$format` | render `{path}` template string into a field |
| `$merge` | deep-merge object into a path |
| `$push` / `$pull` / `$sort` / `$slice` | array ops (pull matches a filter; sort by path; slice start/end) |
| `$set_state` | write `$.state` |
| `$log` | template-rendered message to the run log |
| `$upsert_other` | insert/upsert into **another board** (writer `recipe:<board_id>`) |
| `$patch_other` | filter + patch first match in another board |
| `$transaction` | same-board atomic `(filter, patch)` steps via `srv_record_patch_many` |
| `$notify` / `$call` | outbound HTTP (`srv_http_call`, SSRF-guarded, default 15 s timeout) |
| `$schedule` | create an `app_jobs` row (cron or `@every`) |

Every action may also carry an optional per-action `when` condition (evaluated against the running payload).

### 15.3 Execution flow (`engine.rs`)
1. Skip if disabled or event-kind mismatch (or cron name mismatch).
2. Evaluate `match_json` against the event payload; no match → matched but zero actions.
3. Clone the payload — actions mutate a working copy.
4. **Dedup**: resolve `dedup_on` path; if non-null and already in `wb_recipe_runs` → short-circuit. Otherwise `INSERT OR IGNORE` into the ledger (race-safe). Dedup key = SHA-256 of the serialized value.
5. Run up to `MAX_ACTIONS=50` actions sequentially. External actions (`$upsert_other`, `$patch_other`, `$transaction`, `$notify`/`$call`, `$schedule`) are executed after local actions.
6. If the working copy changed (and kind is created/updated and seq present) → **persist via raw `srv_record_set`** (this second write does **not** re-trigger recipes — one-hop only).
7. Write a run-log row (`ok`/`error`, message, duration).

### 15.4 Dispatch semantics (important caveats)
- `run_recipes_for_record` runs **inline and sequentially in the request path** — a slow `$call` blocks the HTTP/MCP response. No background queue.
- Recipes run on REST and MCP writes via the shared `dispatch_recipes` hook; cross-board writes (`$upsert_other` etc.) use raw write paths so **cascades are one-hop**.
- `cron:` recipes receive `payload = null` (they rely on `{{…}}` event tokens for data).
- Run log is written even for non-mutating matched runs.

---

## 16. Cron & Scheduled Jobs (`cron.rs`, `jobs.rs`, scheduler)

- **Cron syntax**: 5-field `min hour dom month dow` with `*`, `*/n`, `a-b`, `a-b/n`, `a,b,c`; DOW `0`(Sun)–`7`; DOM/DOW both-restricted → OR (Vixie convention); `@every <N><s|m|h|d>` also supported. **No** seconds field, no `@daily` macros, no timezones (UTC).
- `app_jobs` rows: `schedule`, `action` JSON, `next_run_at` (computed at add time via `next_run`).
- **Scheduler worker**: every 15 s, takes top 32 due jobs across all boards, for each: reschedule first (protects against crash-missed runs), then run:
  - `http` action: SSRF-guarded request, per-job `timeout_ms` (default 15 s).
  - `poll` action: extract a JSON path from the response and insert it as a new record.
  - `cron:` recipes: dispatch matching recipes with `EventKind::Cron`.
- Job history in `wb_job_runs` (status, message, result).
- Recipes can create jobs via `$schedule`.

---

## 17. Links / Joins (`links.rs`)

- Define a parent/child relation: child board's `from_key` (JSON path) ↔ parent board's `parent_key` (JSON path). One link per board.
- `srv_join_list(child_board, conds, limit, offset)` returns each child record **joined** with the matched parent payload:
  - Extract child `from_key` scalars, batch-lookup parents via a single `json_extract IN (…)` query, index by `parent_key` value.
  - Joined payload shape: `{ from_key: <parent payload> }` (or `null` when unresolved).
  - Parent payloads are redacted per the **parent board's** config.

---

## 18. Audit Trail (`audit.rs`)

- Toggle per board (`PUT /api/srv/{board_id}/audit {"enabled": bool}`), **disabled by default**.
- `wb_audit` is append-only: `event` ∈ `created|updated|deleted`, `actor`, `writer`, `payload_hash` (SHA-256 of the serialized payload), `ts`.
- Audit rows appended on insert (with hash), set (with hash), patch (with hash), single delete (no hash).
- Query: `GET /api/srv/{board_id}/audit?since=<iso>&limit=N` (owner-only, limit clamped `1..=1000`).

---

## 19. Files (`serverless.rs` file helpers + upload route)

- **Upload** — `POST /api/srv/{board_id}/upload`:
  - Body: `multipart/form-data` (first `filename=` part is the file; other name-only parts become **record payload metadata**) OR raw binary with `X-Filename` / `Content-Type` headers.
  - Size cap 25 MB.
  - Stored at `{whiteboard_dir}/sessions/{owner}/apps/{board_id}/files/{file_id}.bin` (`file_id` = `f_` + 16 base-36 chars).
  - A record is inserted with payload `{file: "files/<id>.bin", name, type, size, <extra fields>, customer_id?}` — so uploads are ordinary records: they validate against schema, fire recipes, publish realtime, enqueue webhooks.
  - Returns `{"ok":true, "seq":…, "file":"files/<id>.bin"}`.
- **Download** — `GET /api/srv/{board_id}/file?file=files/<id>.bin`:
  - Path-traversal guarded (resolved path must stay under the board files dir; id alphanumeric/underscore).
  - Content-Type + `Content-Disposition: inline` from the record's metadata.
  - ⚠️ Auth quirk: download requires the board to be **`public_reads`** — read keys are NOT honored here (differs from the record-read path).
- **List**: `srv_list_files` scans the files dir for `.bin` entries → `(file_id, size)`.

---

## 19a. Per-Board Static Asset Hosting (new, `serverlessEngine-rs` addition)

> Not in the reference; added because each board is a serverless app that serves its own front end.
> Architecture: no embedded UI, no per-app ports — apps are identified by board ID in the URL path on
> one port (like Supabase/Vercel): `GET /srv/{board_id}/{path}`.

- **Namespace:** blobs live in the per-board `ObjectStore` prefix `{board_id}/assets/`, isolated per board.
- **Upload** — `PUT /api/srv/{board_id}/assets/{path}` (raw bytes) or MCP `files put <board> <path> --content <text>` (writer auth).
- **Serve** — `GET /api/srv/{board_id}/assets/{path}` and clean path `GET /srv/{board_id}/{path}`:
  - `Content-Type` inferred from file extension (html/css/js/json/png/jpg/svg/webp/woff/woff2/pdf/wasm/…).
  - `index.html` served at the board root (`/srv/{board_id}/`) and for directory-style paths (`/srv/{board_id}/app/` → `app/index.html`).
  - `cache-control: public, max-age=3600`.
  - Public boards (`public_reads`) serve anonymously; private boards require read auth.
- **Delete** — `DELETE /api/srv/{board_id}/assets/{path}`; **list** — `GET /api/srv/{board_id}/assets`.
- **Safety:** path sanitization rejects `..`, empty segments, and backslashes (traversal-guarded); `FsObjectStore::resolve` also escapes path segments on disk.
- **MCP/CLI:** `files put` / `files get` / `files delete`; `apps.update --public_reads` controls anonymous access.

---

## 20. Outbound HTTP Calls (`calls.rs`)

- `srv_http_call(board_id, owner_key, method, url, headers, body, timeout_ms)`:
  - **SSRF guard** (`netguard::valid_url`): only `http(s)://`; rejects loopback/private/link-local/multicast/unspecified IPs and literal `localhost`. ⚠️ No DNS resolution — internal hostnames are not blocked (documented limitation).
  - Methods: GET/POST/PUT/PATCH/DELETE only.
  - Timeout clamped 500–60 000 ms (default 10 s); response capped at 2 MB.
  - Returns `{status, headers, body, duration_ms}`.
- Exposed as REST `POST /api/srv/{board_id}/call` (admin-only) and as the recipe `$call` / `$notify` actions.

---

## 21. Expression Language (`zw-expr`)

Used for computed fields, validation rules, recipe conditions (`match_json`, action `when`), dedup paths, TTL field paths, and filters.

**API**: `parse(src) -> Expr`, `evaluate(src, data) -> Value`, `apply(expr, data)` (pre-parsed fast path), `truthy(src, data) -> bool`, `get_path(data, path)`, plus `set_path`.

**Syntax**:
- Literals: numbers (i64/f64), single/double-quoted strings (`\n \t \r \" \' \\`), `true/false/null`.
- Paths: `$`, `$.a.b[0]`, `$.items[*]` wildcard (collects immediate children into an array, then stops). Keys restricted to `[A-Za-z0-9_-]`; no quoted keys, no filters/slices/recursive descent/negative indexes.
- Operators: unary `-` `!`; binary `+ - * / %`, `== != < <= > >=`, `&& ||`; ternary `?:` (right-assoc). `&&`/`||` short-circuit and return `bool`.
- Functions (case-insensitive, eager args, max 32): `lower upper trim len num str concat round floor ceil abs min max if coalesce contains startswith endswith join nowunix daysago dateadd year month day hour minute second`.

**Semantics**: missing paths → `null`; type coercion is forgiving (numeric compare if both coerce, else string compare lexically for ordering; `+` doubles as concat); division/modulo by zero → `null`; truthiness: `false`/`null`/`0`/`""` falsy, arrays/objects always truthy. **Total over any JSON input — never panics.**

**Safety limits**: `MAX_PARSE_STEPS=100_000`, `MAX_DEPTH=64`, `MAX_ARGS=32`.

**Known limits**: no aggregation across records, no array map/filter/reduce, no object/array literals, no variables, no user functions, no async/IO.

---

## 22. Two-Tier Database Layout (`session_store.rs`)

- **System DB** (`ZEROWRAPPER__DB_PATH`): accounts, sessions, `wb_keys`, `srv_board_index`, hook delivery queue, jobs, public slugs, templates.
- **Session DBs**: one file per session under `{root}/sessions/{sid}/{sid}.db`; lazily opened and cached (Mutex<HashMap>), migrated on first open. Holds boards, records, hooks config, links, audit, recipes, secrets, whiteboards.
- `SessionStore` is the facade used by the HTTP layer and MCP: `session(sid)`, `board_pool(board_id)` (resolves board → owning session pool via `srv_board_index`), `create_board`, `key_issue/list/revoke`, `hook_enqueue/reschedule/deliveries`, `ttl_sweep_all`.
- `DbPool` wraps a single `Arc<Mutex<Connection>>` — **all access on one pool is serialized by a mutex**; concurrency comes from multiple pools (multiple sessions), not multiple connections on the same pool.

---

## 23. Templates / Materials (`templates.rs` + `zw-materials`)

- Template versions are **isolated boards** whose `board_id` is literally `"{template_id}@{version}"` (`srv_app_create_with_id`).
- `parse_version_sid` splits at the last `@`; version must be `>= 1`.
- At the HTTP layer, `board_pool` first checks whether the path parses as a template version → routes to the version's own DB pool (verified against the version manifest). This is how `embed` (shared live data) works.
- The `mat` MCP tool reuses the same `srv` grammar scoped to `template@version` sessions.

---

## 24. HTTP API Reference (`/api/srv/{board_id}/…`)

Prefix is `/api/srv/{board_id}` (likely rewritten to `/srv/{board_id}` in deployment). All JSON responses include `Access-Control-Allow-Origin: *`; permissive CORS on `/api`; `OPTIONS` preflight handled.

### Data plane
| Method | Path | Notes |
|---|---|---|
| POST | `/submit` | insert; `?upsert=1` |
| GET | `/records` | list; params `limit, before, offset, filter, order, search, hl` |
| GET | `/records/{seq}` | get one |
| GET | `/aggregate` | `op, field, group_by, filter` |
| GET | `/join` | linked records; `filter, limit, offset` |
| PUT | `/records/{seq}` | replace |
| PATCH | `/records/{seq}` | patch ops |
| DELETE | `/records/{seq}` | delete one |
| DELETE | `/records` | delete all (admin, no customer) |
| POST | `/bulk` | bulk insert (10 MB cap) |
| POST | `/upload` | multipart/raw (25 MB cap) |
| GET | `/file` | download (requires `public_reads`) |

### Realtime
| Method | Path | Notes |
|---|---|---|
| GET | `/ws` | WebSocket; `after`, `filter` |
| GET | `/events` | SSE; `after`, `filter` (no customer keys) |

### Control plane (admin)
| Method | Path | Notes |
|---|---|---|
| POST/GET/DELETE | `/keys` | issue / list / revoke (`?bucket=`) |
| POST/DELETE/GET | `/hooks` | register / remove / list |
| GET | `/hooks/deliveries` | delivery history |
| POST | `/call` | outbound HTTP (admin only) |
| POST | `/rate` | set rate config |
| GET/PUT | `/audit` | query / enable-disable |
| GET/PATCH | `/config` | aggregate snapshot / patch app |
| PUT/DELETE | `/ttl` | set / clear |
| PUT/DELETE | `/link` | set / clear link |
| PUT/DELETE | `/computed` | set / clear computed |
| PUT/DELETE | `/validate` | set / clear validation rules |
| PUT/DELETE | `/redact` | set / clear redaction |
| PUT/DELETE | `/webhook_secret` | set (≥16 chars) / clear |
| GET/POST/PATCH/DELETE | `/recipes` (+ `/recipes/{name}`) | recipe CRUD & enable toggle |
| GET/POST/DELETE | `/secrets` (+ `/secrets/{name}`) | secrets CRUD |
| POST | `/events` | inbound webhook event (HMAC-verified) |

### Common conventions
- Errors: `{"error": "<message>"}` (400/401/403/404/429/500/503).
- Writes: `{"ok": true, …}` on success.
- Limit clamps: list 1–200 (data plane) / 1–500 (filter/search), audit 1–1000, backfill 1000, body 1 MB (JSON) / 10 MB (bulk & inbound) / 25 MB (upload).

---

## 25. MCP Surface (`zw-mcp/src/srv.rs`)

The engine is fully exposed to AI agents as a CLI-like grammar inside two MCP tools (`wb --session <sid> srv …`, `mat srv …`). One tool = one `command` string; the shared `execute_srv` parses `group verb args`:

| Group | Verbs |
|---|---|
| `srv apps` | `create`, `list`, `show`, `schema`, `find`, `update`, `delete`, `export`, `import`, `keys` (list/issue/revoke), `hooks` (register/list/remove/deliveries/replay), `rate`, `ttl`, `link`, `audit` (on/off/list), `jobs` (add/list/runs/remove), `computed`, `validate`, `redact` |
| `srv records` | `submit`, `bulk`, `list`, `get`, `update`, `patch`, `query`, `search`, `aggregate`, `count`, `delete`, `audit`, `call` |
| `srv recipes` | `list`, `add`, `enable`, `runs`, `remove` |
| `srv secrets` | `set`, `list`, `remove` |
| `srv files` | `list`, `get` |

Grammar style: positional + `--flag value` (multi-word values re-joined until the next `--`; repeatable flags via `all_flags`; boolean flags via `has_flag`; `--help` at group, verb, and detail levels). Auth is session-scoped: every operation runs as `--session <sid>`; cross-session access only for `public_reads` boards (or an explicit "retry with --session <owner>" hint).

---

## 26. Cross-cutting Limits & Known Quirks

- **Per-pool serialization**: one mutex-guarded connection per session pool — vertical scaling limited to multiple sessions, not connections.
- **Realtime is in-memory broadcast** — no durable subscriptions; single-instance only.
- **Recipes run inline in the request** — no queue/decoupling; slow external actions block the request.
- **Recipe cascades are one-hop** — cross-board writes never re-trigger recipes.
- **FTS is Tantivy-backed, ngram-tokenized** — great for substring, but no stemming/phrase operators exposed by the filter DSL.
- **Filter logic is AND-only** — no nested `$or`.
- **No transactions across boards** — `$transaction` is same-board only.
- **Webhook secrets stored in plaintext** in `wb_hooks` (unlike `wb_app_secrets` which is AES-GCM).
- **Rate limiter and realtime broker are in-process** — lost on restart / not shared across instances.
- **SSRF guard does no DNS resolution** — internal hostnames pass.
- **File download ignores read keys** — requires global `public_reads`.
- **Keys are unsalted SHA-256** — deterministic hash of the secret.
- **`$or`, group conditions, savepoints, and multi-board transactions** are explicit "not supported yet" boundaries.

---

## 27. Cache Layer (RAM + DB) — design proposal (new)

> Asked in session 2026-08-16: can we run a Redis-like cache alongside TTL so hot data lives in RAM
> and the DB stays the durable truth? **Yes — and it fits the existing architecture cleanly.**

### 27.1 Why it works here

- The engine talks to storage only through the `Database` / `ObjectStore` traits. A cache can be a
  **wrapper adapter** (decorator) implementing the same traits, layered between the engine and the real
  backend. Nothing in the engine changes; the swap seam is already the trait.
- The in-memory adapters already prove the pattern (records in a `DashMap`, blobs in memory).
- TTL already exists as first-class semantics (§9): cache entries can carry the *same* expiry policy, so
  the cache and the DB agree on what is dead.

### 27.2 Design sketch

```
engine  →  CachedDatabase (decorator, implements Database)
              ├─ read path:  get/query/search → check RAM (hashmap + TTL) → miss ⇒ backend ⇒ fill
              └─ write path: insert/set/patch/delete/upsert → write backend (durable) → invalidate RAM
engine  →  CachedObjectStore (decorator, implements ObjectStore)
              ├─ get/head/list → RAM blob cache (small, LRU + TTL) → miss ⇒ backend ⇒ fill
              └─ put/delete    → write backend → invalidate/refresh RAM
```

- **RAM layer:** in-process `Mutex<HashMap>` (or LRU cap) keyed by `table+pk` / blob key, each entry with
  a TTL. TTL per entry mirrors record `ttl_seconds`/`ttl_field` where applicable (§9).
- **DB layer:** the real adapter (SQLite/Postgres/Turso later). Always the source of truth.
- **Consistency:** writes go through the engine and invalidate the corresponding cache keys. Single-process
  ⇒ strong consistency for free. Multi-instance (future) ⇒ a pub/sub invalidation bus (e.g. Redis) per §5/Phase 5.
- **TTL sweeper** (§9) still runs against the DB; the cache independently evicts on its own TTL, so the two
  can never disagree about *visible* data (read-time TTL filter also runs at the engine layer regardless).

### 27.3 What it buys

| Concern | With cache |
|---|---|
| Hot read latency | RAM hit avoids DB round-trip / JSON re-parse |
| Sweeper pressure | DB still does the durable sweep; RAM evicts lazily by TTL |
| Backend swap safety | Cache is behind the trait — swap SQLite↔Postgres↔Turso with zero engine changes, cache included |
| Blob (files/assets) reads | `CachedObjectStore` avoids re-reading bytes from disk/S3 for hot assets |
| FTS results | Optional: cache top-N search results keyed by query+filter for a short TTL (stale-tolerant) |

### 27.4 Constraints / decisions to confirm

1. **Which DBs to conformance-test the swap on:** SQLite first (lightweight, same-dir), then Postgres, then Turso. Proving the same suite passes on all three = the "swap without degradation" guarantee.
2. **Cache granularity:** point-gets + query-result caching (TTL-bounded) vs. only point-gets. Start with point-gets + blob cache; add query caching later if benchmarks justify.
3. **Cache invalidate-on-write:** strict (every write invalidates) vs. TTL-only (stale-tolerant). Recommend strict for records, TTL-only for search/aggregate.
4. **Where the decorator lives:** `server/db/cache.rs` / `server/store/cache.rs` (server crate), so `engine` stays pure — consistent with §4 crate rules.
5. **Eviction policy:** pure TTL first (simplest, matches record TTL), LRU cap later for unbounded/hot paths.
