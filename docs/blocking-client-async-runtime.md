# Blocking Client on Async Runtime — Root Cause, Fix, and Audit

## TL;DR

The daemon's HTTP handlers were calling a **blocking reqwest client directly on a
tokio async worker thread**. reqwest's `blocking` client spawns its own internal
single-threaded tokio runtime to do the I/O. When that blocking client is invoked
*from inside another* (multi-threaded) tokio runtime, the two runtimes interact
badly and, after roughly **128 requests**, the blocking client's internal runtime
stops dispatching the request to the network. Every subsequent request then sits
stalled for the full configured timeout (30s) before failing with
`error sending request`.

The bug was **not** in any dependency version. It was a usage error:
running a `reqwest::blocking::Client` on an async executor thread.

---

## 1. The Symptom (observed)

Command being run repeatedly:

```bash
srv graph sync <board> learners
```

Observed behaviour:

1. The daemon processes ~127–135 Helix requests normally (each 1–40ms).
2. Then one request stalls for exactly **30000ms** and fails:
   ```
   [helix] http://127.0.0.1:7979/v2/query FAILED after 30000ms: error sending request for url (http://127.0.0.1:7979/v2/query)
   ```
3. All subsequent requests fail the same way (30s each). The CLI appears to hang.
4. During the stall, Helix itself is perfectly responsive — `curl` to the *exact*
   failing query returns `{"rows":[]}` in milliseconds.
5. The daemon's `/healthz` goes unresponsive because the engine `Mutex` is held
   by the stuck worker thread.

### Debug evidence

- `strace -f` on the daemon showed the `reqwest-internal-sync-runtime` thread
  parked in `epoll_wait` for 26.5s **without ever issuing a `connect()`** — the
  request was submitted to the blocking client but never dispatched to the socket.
- The caller thread (a `tokio-rt-worker`) was parked in a futex inside reqwest's
  `wait::timeout`, waiting the full 30s for a response that never came.

---

## 2. The Root Cause (proved with a minimal repro)

A standalone Rust repro (plain `reqwest` + `serde_json`) posted the same JSON to
Helix 300 times in a row:

| Execution context                         | Result                    |
| ----------------------------------------- | ------------------------- |
| Blocking client on a **plain `std::thread`** | 300 / 300 OK, 0ms each |
| Blocking client **inside `tokio::Runtime`**  | **stalls at request #127**, then 30s failures |
| Blocking client inside `tokio::task::spawn_blocking` | 300 / 300 OK, 0ms each |

Conclusion: **calling a `reqwest::blocking::Client` from inside a tokio async
context is the trigger.** The blocking client's internal runtime and the outer
tokio runtime deadlock/stall after a burst of requests.

### Why it happens

- `reqwest::blocking::Client` creates a dedicated background thread
  (`reqwest-internal-sync-runtime`) running its own `current_thread` tokio runtime.
- Calls are handed to that thread via an mpsc channel; the caller then blocks with
  `thread::park_timeout` waiting for a oneshot reply.
- When the caller is itself a tokio worker, the two runtimes' parking/notify
  mechanisms collide. After enough requests the wake-up is lost, the internal
  runtime never polls the request, and the caller times out.

---

## 3. The Fix

Wrap every blocking call so it runs on tokio's **blocking thread pool** via
`tokio::task::spawn_blocking`, instead of on an async worker thread.

```rust
let out = match tokio::task::spawn_blocking(move || {
    crate::handle_jsonrpc(&mcp, &text)   // blocking: engine lock + reqwest::blocking
}).await {
    Ok(out) => out,
    Err(_) => { /* json-rpc internal error */ }
};
```

### Where it was patched

| File | Change |
| ---- | ------ |
| `crates/mcp/src/transport.rs` | `handle_hyper` now runs `handle_jsonrpc` (which locks the engine and uses the blocking Helix client) inside `tokio::task::spawn_blocking`. |

This fixed the reported `srv graph sync` hang: 0 failures, all requests 1–247ms.

---

## 4. Codebase Audit — other occurrences of the same problem

Because this is a serverless engine (external apps can trigger writes, hooks,
recipes, cron jobs), the blocking call can be reached from many async entry
points. Every path below invokes the **blocking** Helix client
(`crates/helixdb/src/client.rs`) or the **blocking** outbound HTTP caller
(`crates/server/src/http_caller.rs`, installed as `engine::http::HttpCaller`) from
inside a tokio async context.

### 4.1 The blocking clients

Two `reqwest::blocking::Client`s exist:

1. `crates/helixdb/src/client.rs` — the database backend (Helix).
   - `Client::post` / `Client::execute` are **synchronous**, no `.await`.
2. `crates/server/src/http_caller.rs` — `ReqwestCaller`, installed via
   `engine::install_http_caller`, used for outbound webhooks/recipes/cron HTTP.
   - `HttpCaller::call` is **synchronous**, no `.await`.

### 4.2 Chain that must NOT run on an async worker

```
async HTTP handler (tokio worker)
   └─ engine.lock()                       // std::sync::Mutex
       └─ graph_sync / insert_record / update_record / delete_record / dispatch_recipes / dispatch_cron / job run
           └─ helixdb::Client::post()     // reqwest::blocking  (BLOCKING)
           └─ engine::http::http_call()   // ReqwestCaller       (BLOCKING)
```

### 4.3 Confirmed problem locations (needs the same fix)

| # | File | Line(s) | Why it's a problem |
|---|------|---------|--------------------|
| 1 | `crates/mcp/src/transport.rs` | `handle_hyper` → `handle_jsonrpc` | **FIXED.** Was running blocking engine on async worker; now wrapped in `spawn_blocking`. |
| 2 | `crates/server/src/transport/rest.rs` | `handle` (async, ~172) → `route` (async, ~364) → many `engine.lock()` calls; webhook `dispatch_recipes` at ~721 | REST handlers run on tokio workers and call the blocking engine (Helix + recipes) directly. |
| 3 | `crates/server/src/jobs.rs` | `spawn_scheduler` (async, ~52) → `run_job` (async, ~74) → `engine::http::http_call` at ~116 | Cron job HTTP actions use the blocking caller on an async worker. |
| 4 | `crates/engine/src/crud.rs` | `insert_record`/`update_record`/`delete_record` → `automation::dispatch` | These are triggered from async REST/MCP handlers; recipes run blocking outbound HTTP. |
| 5 | `crates/engine/src/automation.rs` | `dispatch` (~148) / `dispatch_cron` (~185) → `run_actions` → `apply_action` → `http_call_body` (~563); `srv_http_call` (~278) | Recipe actions perform blocking outbound HTTP. Safe only if the caller thread is a blocking thread. |
| 6 | `crates/server/src/transport/rest_admin.rs` | `server.engine.lock().unwrap()` in every admin handler | Admin endpoints call the blocking engine on async workers (lower risk — fewer requests — but same hazard). |

### 4.4 Already correct (async client used properly)

- `crates/server/src/jobs.rs` — `spawn_webhook_worker` uses `reqwest::Client`
  (async) with `.send().await`. **Correct.**
- `crates/server/src/transport/sse.rs`, `ws.rs` — only read records
  (`records_after`), no blocking outbound HTTP. **Low risk.**

---

## 5. Recommended Fixes (apply to the remaining locations)

### 5.1 Server REST transport (`rest.rs`)

Move the body of `route` / `handle` into a `spawn_blocking` closure, mirroring the
MCP fix. The `route` function takes `Arc<Server>`; clone it into the closure and
call the blocking handler there. Because `route` currently awaits `req.collect()`
for the body first, collect the body in the async fn, then dispatch the
*already-collected* request to `spawn_blocking`.

```rust
let server = server.clone();
let body_bytes = /* collected above */;
let resp = tokio::task::spawn_blocking(move || {
    route_blocking(&server, &method, &segs, &headers, &body_bytes, &params)
}).await.unwrap_or_else(/* 500 fallback */);
```

### 5.2 Scheduler cron HTTP (`jobs.rs` `run_job`)

Wrap the HTTP action portion (the `engine::http::http_call(...)` call and the
subsequent `insert_record`) in `spawn_blocking`:

```rust
let engine = engine.clone();
let job = job.clone();
let message = tokio::task::spawn_blocking(move || {
    // ... existing job logic (http_call, insert_record) ...
}).await.unwrap_or_else(|_| "job failed".to_string());
```

### 5.3 Recipe/automation HTTP (`crud.rs`, `automation.rs`)

These are reached from async REST/MCP handlers. Once the REST transport (5.1)
is fixed to run the whole engine dispatch in `spawn_blocking`, recipe actions run
on blocking threads and are safe. The MCP path is already fixed. No change needed
inside `automation.rs` itself — the fix belongs at the transport boundary.

---

## 6. Rule of thumb (prevent regression)

- Never call a `reqwest::blocking::Client` (or any blocking I/O) from a tokio
  async context.
- Every async HTTP handler that touches the engine must dispatch the blocking work
  with `tokio::task::spawn_blocking`.
- Prefer the **async** `reqwest::Client` for any new outbound HTTP that originates
  in an async context (e.g. the webhook worker already does this correctly).
- Keep `engine` free of blocking transport: it must compile and pass tests with
  zero external I/O deps (in-memory adapters only). The blocking Helix/HTTP
  clients are server-side adapters and must only ever run on blocking threads.

---

## 7. Verification

After the fix, re-run:

```bash
srv graph sync <board> learners
```

Expected: completes without 30s failures. Check the daemon log:

```bash
grep -c FAILED /tmp/daemon_fixed.log   # expect 0
grep 'took' /tmp/daemon_fixed.log | tail   # expect millisecond values, no 30s
```

A standalone repro of the underlying issue (blocking client on tokio) is
available under `/tmp/repro/` (not part of this repo).

---

## 8. Other resource-bound paths found while making the daemon idle (2026-08-18)

The blocking-client stall is NOT the only way the daemon ends up CPU/memory
bound or wedged. During the IICO migration completion pass, more issues
were found and fixed. They matter because the engine is replacing the source
Supabase DB, so the daemon must stay responsive while serving the app.

### 8.1 `table_ttl_clause` always returned `Some`, disabling pushdown + cache

`crates/engine/src/query.rs`:

```rust
fn table_ttl_clause(cfg: &TableConfig) -> Option<TtlClause> {
    Some(TtlClause { seconds: cfg.ttl_seconds, field: cfg.ttl_field.clone() })
}
```

Even for tables with NO TTL configured (both fields `None`) this returned
`Some(TtlClause)`. Consequences:

- The HelixDB adapter's `aggregate()` only pushes down to Helix when
  `ttl.is_none()` — so **every count/aggregate fell back to a full row scan**
  in Rust (fetch all rows for the table, count locally). For a 12k-row table
  that's ~800ms per count.
- `CachedDatabase::query()` skips the query cache when `ttl.is_some()` — so
  those scans were never cached either.

**Fix:** return `None` when the table has no TTL:

```rust
fn table_ttl_clause(cfg: &TableConfig) -> Option<TtlClause> {
    if cfg.ttl_seconds.is_none() && cfg.ttl_field.is_none() {
        return None; // no TTL: let the backend push down / cache the query
    }
    Some(TtlClause { seconds: cfg.ttl_seconds, field: cfg.ttl_field.clone() })
}
```

Symptom to watch for: a `count` aggregate on a table with no TTL shows up in
`HELIX_DEBUG=1` as `fetch_rows` with `order_by` instead of `aggregate_by`.

### 8.2 `resources` counted per-table (176 sequential Helix aggregates)

`crates/server/src/resources.rs` used to loop over every table and call the
per-table aggregate:

```rust
for t in tables { records += engine.count_records(board, &t.table)?; }
```

With 176 tables, each aggregate taking 100-800ms on Helix, that's 30-60s for
one `/resources` call — and the old code held the engine `Mutex` across the
whole loop, blocking every other request.

**Fix:** a board-wide count in ONE Helix query (label + tenant, no per-table
loop). Added `Database::count_records(board)` to the trait (default bails; the
engine falls back to summing per-table counts), overridden in the Helix adapter
as a single `aggregate_by { count }` over all `wb_records` nodes for the tenant:

```rust
// crates/server/src/db/helix.rs
fn count_records(&self, board: &str) -> anyhow::Result<i64> {
    let pred = helixdb::predicate::Predicate::new()
        .eq("$label", json!(engine_label("wb_records")))
        .eq(helixdb::tenant::TENANT_PROP, json!(self.tenant(board).as_str()));
    let root = json!({ "aggregate_by": {
        "input": { "nodes_where": { "predicate": pred.into_json() } },
        "function": { "count": null }, "property": SEQ_PROP,
    }});
    // ... post read, parse "<prop>_Count" (capital C) ...
}
```

Result: `/resources` returns the full 21683-record count in **~0.6s**.

Note: Helix names the aggregate result `<prop>_Count` (capital `C`), e.g.
`seq_Count`, NOT `seq_count`. Parse liberally (case-insensitive `_count`
suffix, or `count` / `value` keys).

### 8.3 `report_json` self-deadlock (re-entrant engine Mutex)

`crates/server/src/resources.rs` `report_json()` did:

```rust
let engine = server.engine.lock().unwrap();          // 1st lock
...
let rate = server.rate_limits_for(&board_of(server, board)); // board_of locks AGAIN
```

`std::sync::Mutex` is NOT reentrant — the second `lock()` on the same thread
deadlocks forever. Every `/resources` call wedged the daemon: the main thread
held the engine Mutex and every other request (including `/healthz` — no,
healthz is lock-free, but any engine-touching route) waited on it. All threads
park in `futex_wait_queue`.

**Fix:** fetch the board / rate limits BEFORE taking the engine lock:

```rust
let rate: RateLimits = server.rate_limits_for(&board_of(server, board));
let (storage, usage, now) = { let engine = server.engine.lock().unwrap(); ... };
```

**Rule of thumb:** never call anything that takes the engine lock while
already holding it. `board_of()`, `rate_limits_for()`, `get_app()`, etc. all
lock internally.

### 8.4 Background loops were always on (CPU + store scans every few seconds)

`crates/server/src/jobs.rs` spawned three loops unconditionally:

| Loop | Interval | Cost when idle |
|---|---|---|
| TTL sweeper | 60s | full store scan for TTL-dead rows |
| Job scheduler | 15s | `job_due` scan over the whole store |
| Webhook worker | 2s | `hook_deliveries_due` scan over the whole store |

For a deployment with no TTL tables, no cron jobs, and no webhooks (e.g. the
IICO migration board), these burn CPU and hold the engine lock periodically
for nothing.

**Fix:** all three are now opt-in via env flags, off by default:

```
SRV_BG_TTL=1   enable the TTL sweeper (only if you use TTL tables)
SRV_BG_JOBS=1  enable the cron/job scheduler (only if you use cron jobs)
SRV_BG_HOOKS=1 enable the webhook delivery worker (only if you use webhooks)
```

Also spaced out realtime keepalives: WS resource ticker 2s → 60s
(`crates/server/src/transport/ws.rs`), SSE keepalive 30s → 60s
(`crates/server/src/transport/sse.rs`), and the MCP `apps.resources` reporter
now uses the cached count instead of recomputing per call.

### 8.5 Measured result

After all fixes, with the IICO board (176 tables, 21683 records) loaded:

| Metric | Before | After |
|---|---|---|
| Daemon RSS | ~97-111 MB | ~14 MB |
| Daemon idle CPU | several % (background loops + cache misses) | 0.0% |
| `/resources` latency | >60s (deadlock → hang) | ~0.6s |
| Per-table count on 12k-row table | ~800ms (full scan) | ~0.5s (Helix pushdown) |
| Tables list | 0.04s | 0.04s |

