# Code Audit: Runtime / Concurrency Problems (2026-08-22)

> **STATUS 2026-08-22 (final):** P2, P3, P4, M3 fixed + the full mutex plan landed
> (commit `0a7b2c1` = stages 0.2+1: bg loops off async workers, ai_proxy off async
> worker, assets + board-metadata lock-free via shared object-store handle and 5s
> AppCache; commit `ef4bf9c` = stages 2a/2b/4/5: phased recipe dispatch with lock-free
> $call HTTP, cron lock-scope split, tokio-Semaphore admission control
> (SRV_MAX_DB_CONCURRENCY=8 / SRV_MAX_DB_QUEUE=64), atomic CAS seq allocator).
> M1/M2 health honesty included. Bonus: O(N²) bulk-insert seq allocation fixed.
> Deployed to Production-folder/srv/srv and verified live (§Deployed-results below).
> Stress harness: tools/stress/stress.py — 20/20 pass at 50k rows both before and
> after the refactor stages; parallel inserts improved 36ms → 5ms/req from the allocator
> (tools/stress/BASELINE.md). Remaining open: nothing blocking; optional future work =
> singleflight query coalescing in CachedDatabase if p95 read contention appears.

Scope: `crates/{server,engine,helixdb,mcp,cli}` audited against
`docs/blocking-client-async-runtime.md`, `deploy/iico/docs/ENGINE_FREEZE_INVESTIGATION.md`,
`deploy/iico/docs/DATA_SYNC_HIGH_VOLUME_ISSUE.md`. Every item below is verified in source,
with file:line references. Priority: P1 > P2 > ... ; M = minor.

## Deployed results (live board b_7z5lr8zaurkd0000, 29,083 records, Helix v0.0.4)

| Endpoint | Before incident fixes | After this deploy |
|---|---|---|
| `approval_steps` count (3 rows) | 30–90s | **0.16s** |
| `term_results` count (4,546) | timeouts | **1.0–1.3s** cold, value verified 4546 |
| `in_app_notifications` list limit=2 | died at 90–174s | **0.16s warm** (6.1s first/cold) |
| `learners` filtered query | — | **0.31s** |
| SPA root `/srv/<board>/` | timed out at 60s during freeze | **0.006s** |
| `healthz` | always `{"ok":true}` even when dead | reflects Helix reachability |
| retry log spam under load | `[helix] retry 3/3` storms | 0 retries in verification window |

Caveat: first hit after restart pays SlateDB cold-cache reads from MinIO
(health round-trip ~4.5–5.3s until block cache warms). Warm behavior is the table above.

## Already fixed (do not re-fix)

The blocking-client-on-async-worker stall (~128 requests → 30s failures) is fixed at all
transport boundaries. Verified:

| Fix | Evidence |
|---|---|
| MCP JSON-RPC dispatched via `spawn_blocking` | `crates/mcp/src/transport.rs:27` |
| REST body collected async; whole route on blocking pool | `crates/server/src/transport/rest.rs:218,323` |
| Static assets served on blocking pool | `rest.rs:279-280` |
| Cron job body on blocking pool | `crates/server/src/jobs.rs:80` |
| WS initial rows on blocking pool | `crates/server/src/transport/ws.rs:234` |
| TTL clause no longer always `Some` (aggregate pushdown + query cache restored) | `crates/engine/src/query.rs:7-11` |
| `/resources` single board-wide aggregate (was 176 sequential calls) | `crates/server/src/db/helix.rs:1098-1127` |
| Background loops opt-in (`SRV_BG_TTL/JOBS/HOOKS`) | `crates/server/src/lib.rs:69-82` |
| Helix blocking client keep-alive | `crates/helixdb/src/client.rs:20-23` |
| `report_json` re-entrant mutex deadlock removed; `system_health` bounded `try_lock` | `rest.rs:879-899` |

## Open problems

### P1 — Global engine Mutex held across network I/O serializes the daemon

- `Server.engine: Arc<Mutex<ServerlessEngine>>` (`crates/server/src/lib.rs:20`).
- Every route takes it and holds it across Helix HTTP AND outbound recipe/webhook HTTP.
  Chain: `route_blocking` → `engine.insert_record/update/delete` →
  `engine::automation::dispatch` → `engine::http::http_call` (blocking reqwest, up to 30s
  per call) — all under one lock (`crud.rs`, `automation.rs`, `http_caller.rs`).
- Worst case: `run_job_blocking` holds the lock across `dispatch_cron_job`
  (`jobs.rs:89-92`) — cron actions doing external HTTP stall every request in the daemon.
- Static assets are local-disk reads but `static_site()` locks the engine twice
  (`rest.rs:349,354`) → assets still queue behind slow queries. This is why "static pages
  stopped being served" during the migration and can recur even with `spawn_blocking`.
- Effect: one 30-90s scan or webhook = full-stack freeze symptom, regardless of pool size.

Fix direction: short lock scopes (clone data out, do I/O outside); serve assets/auth from a
lock-free path (cached app metadata); move recipe/webhook HTTP out of the lock.
CAUTION: `next_seq` read-max-then-write (`crud.rs:136-151`) is only correct BECAUSE this
mutex serializes writes. Any lock sharding must first make seq allocation atomic
(e.g. counter table update inside the same critical section).

### P2 — SSE snapshot runs blocking full-board scan on an async worker

- `sse_response()` is called directly from async `handle()` (`rest.rs:308-321`) BEFORE the
  spawn_blocking dispatch, and internally does `engine.lock().unwrap()` +
  `records_after` (`crates/server/src/transport/sse.rs:69-75`).
- `records_after` → `scan_rows_all_tables`: fetches EVERY row of EVERY table
  (`limit: usize::MAX`) then filters/truncates to 1000 in Rust
  (`crates/engine/src/crud.rs:659-681`).
- Two violations in one: (a) blocking reqwest on async worker — the exact pattern that
  stalls after ~128 requests, reintroduced through this path; (b) O(all board records)
  work per SSE connect. On the pre-purge 179k board this wedged a worker + the mutex for
  the scan duration.
Fix direction: wrap snapshot in `spawn_blocking` (mirror ws.rs:234) AND push
`$.seq > after` down to Helix instead of fetching everything.

### P3 — SRV_HTTP_TIMEOUT_MS is parsed but never wired to the Helix client

- Loaded into Config (`crates/server/src/config.rs:43-45`).
- Never passed to `HelixDatabase::new` / `Client::with_timeout`. Grep: only definition
  sites. Client stays hardcoded 30s (`client.rs:24`).
- Answers the freeze doc's open checkbox: ops setting 900000ms had no effect; observed
  90–174s failures = 30s timeout × 3 retries (+ queue time), consistent with logs.
Fix direction: thread `cfg.http_timeout_ms` into `backend_impl()` → `HelixDatabase::new(url)
.with_timeout(cfg.http_timeout_ms)`.

### P4 — Retries amplify heavy scans under congestion

- `Client::execute` retries ANY `Error::Http` 3× with 100/200ms sleeps
  (`crates/helixdb/src/client.rs:45-67`). Includes per-request timeouts and mid-flight
  severances ("error sending request").
- A 60s scan becomes up to ~180s DB pressure exactly when the DB is saturated
  ("[helix] retry 1/3..3/3" seen in incident logs). No jitter, no circuit breaker, no
  distinction between connect-refused (retryable) vs timeout/severed (don't retry big scans).
Fix direction: retry only connect-phase errors; never retry requests that already ran
longer than N seconds; optional circuit breaker when helix latency exceeds threshold.

### P5 — No backpressure / request-class isolation

- No Semaphore, no max in-flight Helix queries, no 429/503 shedding (grep: zero hits).
- `CachedDatabase.query` has no request coalescing (`crates/engine/src/storage/cache.rs:
  177-188`): N concurrent identical cache-misses fire N full scans simultaneously.
- Browser startup burst + bulk ingest + cron interleave FIFO; one 60s scan delays
  everything; nothing sheds low-priority traffic.
Fix direction: Semaphore around helix calls (4-8 permits), 503+Retry-After when saturated;
coalesce identical in-flight queries (singleflight).

## Minor findings

- M1 `system_health` hardcodes IICO board id `b_7z5lr8zaurkd0000` (`rest.rs:890`) — wrong
  table count on any other deployment.
- M2 `/healthz` returns `{"ok":true}` unconditionally (`rest.rs:502`) — lies during
  incidents (freeze doc B5 unresolved). Depth-health exists only in `/api/system/health`,
  and even that checks only engine-lock acquisition + table listing, not a Helix round-trip.
- M3 `find_record` scans the WHOLE table to get one record (`crud.rs:126-134`) although the
  adapter CAN push `$.seq eq` down (`to_predicate`, `helix.rs:143-183`). Point lookups were
  "instant" during the freeze only because of the 30s CachedDatabase TTL mask.
- M4 `next_seq` read-max-then-write (`crud.rs:136-151`) — safe only under the global mutex;
  latent duplicate-seq bug if locking ever changes (ties into P1).
- M5 `$label` predicates work fine through the typed builder (`Predicate::into_json`);
  incident's "$label matches nothing" applied to hand-written purge-script JSON shape, not
  adapter code. Not a bug — record here so nobody "fixes" it.
- M6 `helixdb::Client` default timeout 30s hardcoded (`client.rs:24`) — should come from
  config by default (same fix as P3).

## Suggested fix order (for discussion)

1. P2 SSE snapshot (small, safe, removes an active async-worker violation).
2. P3+M6 wire http_timeout_ms (one-liner chain).
3. P4 retry policy (small, contained in client.rs).
4. P1 lock scope refactor (needs plan; do M4 seq atomicity first as prerequisite).
5. P5 backpressure semaphore + coalescing (after P1 so limits actually help).
6. M1-M3 health/find_record cleanups (trivial, any time).

# Hyper / traffic & threading model (answer to "does our stack use hyper")

What we run: hyper 1.x is the HTTP server library for ALL inbound traffic
(`crates/server/src/lib.rs:141-162`, `crates/cli/src/main.rs:302-340`,
`crates/mcp/src/transport.rs:39-60`). Per connection: `tokio::spawn` + hyper-util
`auto::Builder` (HTTP/1+HTTP/2), each connection's requests handled on tokio's
multi-threaded runtime worker threads. Outbound to HelixDB: reqwest **blocking** client on
tokio's dedicated blocking-thread pool via spawn_blocking. Outbound webhooks:
reqwest **async** client (`jobs.rs:195`). So yes — hyper+tokio manage inbound connections,
and tokio schedules tasks across CPUs; but the engine's own bottleneck was never hyper or
CPU distribution — it is the global Mutex (P1) plus missing backpressure (P5), which no
HTTP library fixes by itself.
