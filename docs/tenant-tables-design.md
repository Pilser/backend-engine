# Tenant–Table Data Model (Full Refactor)

> **Status:** Design · **Impact:** breaking (no backward compatibility)
> Supersedes the "1 board = 1 table" model. After this, a **board is a tenant**
> that owns **many tables**; a table is a collection of JSON records with its
> own schema, unique key, and TTL.

## 1. Goal & principles

- **`board_id` = tenant** (the "serverless app" / account / API key scope).
- **`table` = collection** of records within a board.
- Records are addressed by the pair `(board_id, table)`.
- Every per-table concern (schema, unique_key, computed/validate/redact, TTL)
  moves from the board to the table.
- Tenancy-level concerns (auth keys, rate limits, recipes, hooks, jobs, assets,
  realtime) stay on the board.
- **Breaking**: existing single-table boards are not migrated. Fresh model only.

## 2. Naming

| Term | Meaning |
|---|---|
| board / tenant | top-level container, owns keys + tables + recipes + assets |
| table | a named collection of JSON records inside a board |
| record | one JSON document in a `(board, table)` |

## 3. Record model

A record payload is arbitrary JSON. The stored record envelope is:

```
{
  "board_id": "b_...",
  "table":   "learners",
  "seq":     12,
  "payload": { ...document... },
  "created_at": "YYYY-MM-DD HH:MM:SS",
  "writer":  "user@x.com" | null
}
```

- `seq` is monotonic **per (board, table)** (`MAX(seq)+1` within that table).
- `board_id` + `table` always present; every query filters on both.

## 4. Table configuration

Stored as a record in a per-board `wb_tables` collection, keyed
`scoped_key(board, table)` (i.e. `{board}/{table}`), with JSON:

```
{
  "board_id": "b_...",
  "table":    "learners",
  "schema_json":   { ...JSON Schema... } | null,
  "unique_key":    "$.email" | null,
  "computed_json": { ... } | null,
  "validate_json": [ ... ] | null,
  "redact_json":   [ ... ] | null,
  "ttl_seconds":   3600 | null,
  "ttl_field":     "$.expires_at" | null,
  "created_at":    "YYYY-MM-DD HH:MM:SS"
}
```

The old board-level schema/unique_key/ttl/computed/validate/redact fields are
**removed** from the board; each table carries its own.

## 5. Storage (turso / system)

Per-board file (`{data}/boards/{board_id}.db`):

```sql
CREATE TABLE IF NOT EXISTS kv (key TEXT PRIMARY KEY, data TEXT NOT NULL);        -- config + wb_tables + recipes + hooks ...
CREATE TABLE IF NOT EXISTS wb_records (
    seq INTEGER PRIMARY KEY,
    board_id TEXT NOT NULL,
    table_name TEXT NOT NULL,
    payload TEXT NOT NULL,
    writer TEXT,
    created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_wb_records_board_table ON wb_records(board_id, table_name, seq);
CREATE INDEX IF NOT EXISTS idx_wb_records_fts ON wb_records USING fts (payload) WITH (tokenizer='ngram');
```

- `wb_records` gains `table_name`; the SQL pushdown WHERE becomes
  `board_id = ? AND table_name = ? AND <conds> ...`.
- `wb_tables`, `wb_recipes`, `wb_hooks`, `wb_links`, `wb_audit`, `wb_app_secrets`
  live in the per-board `kv` table, keyed with a `table:`-style prefix where
  needed (recipes/links become table-aware; see §10).
- System DB (`system.db`) unchanged in role: keys, jobs, hook deliveries, users,
  sessions, board index.

## 6. Engine API (Rust)

All record operations take `(board_id, table)`:

```rust
// tables (new)
fn create_table(&mut self, board, table, schema, unique_key, ...) -> Result<()>
fn list_tables(&self, board) -> Result<Vec<TableConfig>>
fn get_table(&self, board, table) -> Result<Option<TableConfig>>
fn drop_table(&mut self, board, table) -> Result<()>

// records (add `table`)
fn insert_record(&mut self, board, table, payload, writer, upsert, principal) -> Result<i64>
fn bulk_insert(&mut self, board, table, records, writer, upsert, principal) -> Result<Vec<i64>>
fn import_records(&mut self, board, table, format, data, sep, upsert, principal) -> Result<Json>
fn get_record(&self, board, table, seq) -> Result<Option<Record>>
fn record_list(&self, board, table, limit, before, offset) -> Result<Vec<Record>>
fn delete_record(&mut self, board, table, seq) -> Result<bool>
fn query_records(&self, board, table, filter, orders, limit, offset) -> Result<Vec<Record>>
fn search_records(&self, board, table, q, conds, limit, offset, snippet) -> Result<Vec<Record>>
fn aggregate_records(&self, board, table, conds, agg, field, group_by) -> Result<Vec<Json>>
```

Internal helpers (`crud.rs`, `query.rs`, `schema.rs`) change from `load_board`
to `load_table`, and the query filter becomes `$.board_id == X AND $.table == T`.
`board_cond` is replaced by `board_cond(board) + table_cond(table)`.

## 7. REST API

Board/tenant level (unchanged):

```
GET    /api/srv/{board}                  app/tenant info
GET    /api/srv/{board}/assets[/path]    static hosting (tenant-level)
GET    /api/srv/{board}/ws               realtime
GET    /api/srv/{board}/events/stream    SSE
GET    /api/srv/{board}/resources        usage
POST   /api/srv/{board}/auth/signup|login|logout
POST   /api/srv/{board}/keys
POST   /api/srv/{board}/recipes
POST   /api/srv/{board}/hooks
POST   /api/srv/{board}/jobs
POST   /api/srv/{board}/call             outbound (tenant-level)
POST   /api/srv/{board}/events           inbound webhook
```

Tables (new):

```
POST   /api/srv/{board}/tables                     create table   {name, schema?, unique_key?, ...}
GET    /api/srv/{board}/tables                     list tables
GET    /api/srv/{board}/tables/{table}             table config
PATCH  /api/srv/{board}/tables/{table}             update table config
DELETE /api/srv/{board}/tables/{table}             drop table + records

POST   /api/srv/{board}/tables/{table}/submit      insert record
POST   /api/srv/{board}/tables/{table}/bulk        bulk insert
POST   /api/srv/{board}/tables/{table}/import      import JSON/CSV
GET    /api/srv/{board}/tables/{table}/records     list
GET    /api/srv/{board}/tables/{table}/query       query   (filter, order, dir, limit, offset)
GET    /api/srv/{board}/tables/{table}/search      search  (q)
GET    /api/srv/{board}/tables/{table}/aggregate   aggregate (op, field, group)
GET    /api/srv/{board}/tables/{table}/record?seq=N get
PUT    /api/srv/{board}/tables/{table}/records/{seq}   put (replace)
PATCH  /api/srv/{board}/tables/{table}/records/{seq}   patch
DELETE /api/srv/{board}/tables/{table}/records/{seq}   delete
```

The `{table}` segment is a user table name; the explicit `/tables/` prefix keeps
it unambiguous against reserved segments (records, assets, auth, ws, events).

## 8. CLI / MCP + help

New `tables` group; record verbs gain a `table` positional:

```
srv tables create <board> <table> [--schema ...] [--unique_key ...]
srv tables list <board>
srv tables show <board> <table>
srv tables delete <board> <table>

srv records submit <board> <table> [--payload ...]
srv records bulk <board> <table> --records [...]
srv records import <board> <table> --format json|csv --data ...
srv records list <board> <table> [--limit ...] [--before ...]
srv records query <board> <table> --filter '{...}' [--order ...] [--dir asc|desc]
srv records search <board> <table> <q>
srv records get <board> <table> <seq>
srv records delete <board> <table> <seq>
srv records aggregate <board> <table> --op sum --field price [--group ...]
```

- Every `CommandSpec` record verb adds a `table` positional (`ArgType::Name`).
- Help text (`help index/group/verb`, examples) updated to include `<table>`.
- `records.import` gains the `table` arg.
- Auth, recipes, hooks, jobs, keys, assets verbs keep `board` only.

## 9. Auth & rate limits

- **Auth keys stay board-wide** (tenant). A key grants role/scope on the whole
  board, hence all its tables. (Per-table key scoping is a future option.)
- Users (login/JWT) are board-wide; a session's role applies to all tables.
- Rate limits remain per board.

## 10. Recipes, links, jobs

- **Recipes**: `when` may be `record.created`/`updated`/`deleted` on a specific
  table. Recipe JSON gains `table`; dispatch filters by `(event, table)`.
  `$upsert_other`/`$patch_other`/`$notify` target a `(board, table)`.
- **Links**: `wb_links` row becomes
  `{child_board, child_table, child_key, parent_board, parent_table, parent_key}`;
  `GET /join` hydrates parent records onto child rows within tables.
- **Jobs / webhooks**: stay tenant-level; a job/webhook can name a table it
  reads/writes.

## 11. Files / assets

- Static asset hosting stays **tenant-level** (`{board}/assets/...`) — the
  front-end origin, independent of tables.

## 12. Migration

- **None.** Breaking change. Existing boards/tables are not carried over.
- New `b_` boards created fresh; tables created explicitly.

## 13. Implementation phases

1. **Model + storage**: add `table` to `wb_records`, add `wb_tables` config;
   `crud.rs`/`query.rs`/`schema.rs` take `(board, table)`; turso SQL pushdown
   adds `table_name`. Prove two tables in one board read/write.
2. **Vertical slice**: `tables.create/list/show/drop` + `records.submit/list/
   query/get/delete` end-to-end (REST + one CLI verb) — locks the API shape.
3. **Sweep**: remaining record verbs (bulk/import/search/aggregate/put/patch),
   recipes/links/jobs table-awareness, `auth/me`, resources.
4. **Registry/CLI/help**: add `tables` group, add `table` positional to all
   record specs, regenerate help/examples.
5. **Verify**: conformance on in-memory + turso (schema validation, unique_key,
   TTL, search/aggregate scoped per table; auth roles).

## 14. Locked decisions

- API shape: `…/{board}/tables/{table}/…` (explicit).
- No backward compatibility / no migration.
- Auth keys + rate limits: board-wide.
- Assets: tenant-level.
- Recipes/links: table-aware.
