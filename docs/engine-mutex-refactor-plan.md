# Engine Mutex Refactor — Detailed Plan & Rollback Record (2026-08-22)

> **STATUS 2026-08-22 (final):** Stages 0.2, 1, 2a, 2b, 4, 5 IMPLEMENTED and deployed
> (commit `0a7b2c1` = stages 0.2+1; commit `ef4bf9c` = stages 2a/2b/4/5).
> Stage 3 (true per-board sharding) REJECTED per §3 analysis. Everything below is kept
> as the original pre-refactor record; implementation notes appended at §11.
>
> **Purpose:** the complete "current state" record of how the global engine Mutex
> (`Arc<Mutex<ServerlessEngine>>`) was used, so each stage could be done incrementally
> and rolled back precisely. Every lock site, its held-across-I/O risk, its invariants,
> and its target state are listed. Nothing here is speculative — every line
> was read and verified on 2026-08-22 at commit `22b2487` (plus working fixes P2/P3/P4/M3).

**Golden rule during the whole refactor:** `next_seq` (read-max-then-write,
`crates/engine/src/crud.rs:148-163`) is only correct because ONE mutex serializes all
writes. Any change that allows two writers into the same board/table concurrently MUST
first make seq allocation atomic. See §5.

---

## 1. Current architecture (the thing we are refactoring FROM)

### 1.1 Ownership

```
ServerlessEngine { db: Box<dyn Database>, store: Box<dyn ObjectStore>, notify: Option<Notify> }
        wrapped in  Arc<std::sync::Mutex<...>>   ("the engine Mutex")
        owned by:
          - server::Server.engine            (REST/WS/SSE/admin/assets)
          - mcp::McpServer.engine            (MCP tools; same Arc shared by cli serve_combined)
```

- `crates/server/src/lib.rs:20` — `pub engine: Arc<Mutex<ServerlessEngine>>`
- `crates/mcp/src/lib.rs:10` — same Arc pattern.
- Both share the SAME Arc when started via `srv daemon start` (`cli/main.rs:134-145`).

### 1.2 Threading context today

| Context | Where | Notes |
|---|---|---|
| tokio async workers | hyper accept loop, SSE/WS streaming, body collect | must NOT touch blocking clients |
| tokio blocking pool | ALL engine calls after fix e0a68cb (`route_blocking`, MCP `handle_jsonrpc`, cron `run_job_blocking`, WS/SSE initial rows) | safe for reqwest::blocking |
| background loops (opt-in) | ttl sweeper / scheduler / webhook worker — `tokio::spawn`ed tasks; they take the engine Mutex directly from ASYNC context but only call engine methods that hit the DB via blocking client → **these still violate the rule** unless SRV_BG_* loops wrap their work in spawn_blocking (ttl sweeper does NOT today: jobs.rs:16-27 locks + `ttl_sweep()` inline; scheduler locks inline for `job_due`; webhook worker locks inline for `hook_deliveries_due`) | latent 128-stall risk, low traffic |

> Note: the 2026-08-18 doc said bg loops were fixed to be opt-in; being opt-in reduced
> frequency but did NOT move their engine calls onto the blocking pool. That is part of
> this refactor (§4.6).

### 1.3 What the Mutex actually protects

The engine is a plain struct with NO interior mutability except:
- `oauth_states()` — a static `OnceLock<Mutex<HashMap<String,String>>>`
  (engine.rs:698-702), already independently locked.
Everything else mutates through `&mut self` methods or `&self` reads that hit
`self.db` / `self.store`. The std Mutex is what turns `Box<dyn Database>` (not Sync for
mutation) into something shareable.

So the Mutex protects: **all DB access (Helix over HTTP!) and all object-store access**.
That is the design flaw: it also serializes NETWORK I/O done under the lock.

---

## 2. Complete inventory of engine-lock sites (verified)

Legend: R = read lock scope (&self use), W = needs &mut, IO = performs network I/O while
held, SLOW = potentially slow (>50ms) while held.

### 2.1 crates/server

**transport/rest.rs**
| Line(s) | Call | Class |
|---|---|---|
| 350, 355 | `static_site`: get_app + get_subapp/get_asset loop | R, SLOW (asset reads from store under 2nd lock) |
| 450-455 | `board_of` get_app | R fast |
| 477-481 | `principal_of` resolve_principal (reads sessions+keys tables → Helix) | R, SLOW |
| 575,582 | asset_put exists-check + put | W (put), R |
| 594,601 | asset_get handler get_app + get_asset | R, SLOW |
| 629,633 | asset_delete exists + delete | W/R |
| 648,652 | asset_list exists + list | R |
| 666,680 | app_delete get_app + delete_app | W |
| 694 | app_patch update_app | W |
| 711,735 | file_upload exists + upload (store write under lock) | W, SLOW |
| 754,768 | files_list get_app + list_files/download | R |
| 855 | events_inbound get_app(webhook secret) then dispatch_recipes | W, **IO** (recipe http_call up to 15s) |
| system_health ~889 | try_lock bounded 500ms + list_tables | R (bounded) |

**transport/rest_tables.rs**
| Line(s) | Call | Class |
|---|---|---|
| 14,19-23 | board_exists / public_reads get_app | R fast |
| 55 create_table, 68 list_tables, 81 get_table, 95 drop_table | table admin | W/R |
| 132-134 insert_record | **W, IO** (recipes/webhooks inside) |
| 165-174 bulk_insert/bulk_import | W, SLOW (batched Helix writes; no recipe dispatch in bulk_import path) |
| 205-208 import_records | W, SLOW |
| 232 list_records | R, SLOW (scan/pushdown query) |
| 262 query/search_records | R, SLOW (**worst case: full scan**) |
| 329 aggregate_records | R, SLOW |
| 352 get_record | R (now point lookup after M3 fix) |
| 382 set_record / 408 patch_record / 428 delete_record / 452 delete_records | W, **IO** (dispatch chain) |

**transport/rest_admin.rs** — every admin route: one `engine.lock()` per call
(issue_key 81, list_keys 95, revoke_key 124, list_hooks 134, register_hook 153,
remove_hook 167, list_jobs 184, add_job 207, remove_job 221, job_runs 237,
list_recipes 247, add_recipe 285, set_recipe_enabled 303, remove_recipe 313,
list_secrets 324, set_secret 352, remove_secret 362, set_rate 377/382, set_ttl 400).
All short R/W DB ops. LOW risk individually; they queue behind anything slow.

**transport/rest_auth.rs** — signup 36, login 53, logout 65, me/user_by_token 75,
set_user_role 96. login_user writes session row (W). All short-ish but Helix-bound.

**transport/rest_oauth.rs** — oauth_config 31/70, set/check state 49/78,
login_user_by_email + user provisioning 102+. Short.

**transport/ws.rs** — 92-96 resolve_principal, 103-109 get_app (async ctx!),
234-237 records_after inside spawn_blocking (OK).

**transport/sse.rs** — 73-79 initial snapshot inside spawn_blocking (OK after P2).

**transport/ai_proxy.rs** — 37 secrets_map under lock in async context (blocking client!)
→ latent violation, low traffic. Target: spawn_blocking.

**resources.rs** — count_records 59-62 (board-wide Helix aggregate, cached 60s),
report_json 69-84 object_store list x2 + limiter snapshot. SLOW while held.

**jobs.rs**
| Line | Call | Class |
|---|---|---|
| 20 ttl_sweeper | W (scan whole store!) inline on async task — VIOLATION |
| 57 job_due scan | R scan inline on async task — VIOLATION |
| 89-92 run_job_blocking: job_reschedule + dispatch_cron_job | **W, IO** — recipes with HTTP actions run UNDER LOCK, worst offender |
| 137 insert_record (job action) | W, IO (same as any insert) |
| 171-173 job_mark + job_run_insert | W |
| 206 hook_deliveries_due scan | R inline async — VIOLATION |
| 238/244 mark_hook_delivery/list_hooks | W/R |
| 272-282 mark_hook_delivery after each POST | W (HTTP happens OUTSIDE lock here — correct pattern already) |

**lib.rs:51** set_notifier once at startup (fine).

**db/helix.rs** internal Mutexes (text_indexed/text_probed/_boards) — independent,
short, fine. NOT the engine Mutex.

### 2.2 crates/mcp

- lib.rs:75-79 `call()`: locks engine for the WHOLE tool execution
  `tools::run(...)` — includes records.submit → insert_record → dispatch → **IO**.
  Same class as REST writes. transport.rs wraps it in spawn_blocking (correct pool),
  but the MUTEX is still held across outbound HTTP.

### 2.3 crates/engine internals reached WHILE the lock is held

Chain for every record write (`crud.rs`):
```
record_insert (391)
 ├─ load_table            → db.get(wb_tables)      [Helix]
 ├─ prepare_payload       → computed fields        [CPU]
 ├─ check_unique          → db.query               [Helix]
 ├─ next_seq              → db.query(limit 1)      [Helix]  ← ordering hazard §5
 ├─ db.insert             → Helix write (+existing lookup in write_node)
 ├─ audit::append         → db.insert              [Helix]
 ├─ automation::dispatch  → per matching recipe:
 │    ├─ dedup check/mark → db get/insert          [Helix]
 │    ├─ run_actions      → MAY DO http_call_body  [NETWORK ≤ timeout_ms, default 15s]
 │    └─ record_set_raw   → find_record + deep_merge + write [Helix x2]
 └─ webhooks::fire_hooks  → hook_list + enqueue_delivery rows [Helix] (NO HTTP here — worker delivers later ✓)
```
Same shape for update (record_set 500-523), patch (patch_record 530-549),
delete_one (563-583). `bulk_import` (462-487) deliberately skips audit/recipes/hooks.

---

## 3. Invariants that must survive the refactor

1. **Seq uniqueness per (board,table).** Today guaranteed by: single global Mutex +
   `next_seq` max-read. Any concurrency introduced ⇒ replace with an atomic allocator
   (§5) BEFORE releasing write parallelism.
2. **Unique-key check-then-insert** (`check_unique` then insert) is race-safe ONLY under
   exclusive write access. With per-board write locks it stays safe (one writer per
   board); with fully parallel writes it needs a DB-side constraint (upsert-by-key) or
   acceptance of the tiny race window. Decision needed (§7 Q1).
3. **Transaction semantics:** `db.begin/commit/rollback` are currently NO-OPs in BOTH
   adapters (helix.rs:1051-1063, memory has none) — bulk_insert's "transaction" is
   cosmetic. No invariant depends on them today.
4. **Notifier (broker) emission order:** `emit()` is called AFTER the DB write inside
   engine methods, while the caller holds the lock. SSE/WS rely on broker broadcast
   ordering loosely (they filter by board); reordering across boards is acceptable,
   within a board it stays ordered as long as emits happen while that board's write
   lock is held (§4 plan preserves this).
5. **Rate limiter** is separate (`server.limiter: Arc<Mutex<RateLimiter>>`) — untouched.
6. **Poisoning:** current code uses `.lock().unwrap()` everywhere; a panicking handler
   poisons the engine for EVERYONE (another availability bug). parking_lot removes
   poisoning (repo rule rs-parking-lot prefers parking_lot anyway).
7. **MCP shares the SAME engine Arc as REST** in combined mode — any split must keep
   cross-surface consistency (a record created via MCP is visible to REST immediately).
8. **Static assets & object store do NOT need the DB.** `asset_get/put/delete/list`,
   `file_upload/list_files` only touch `self.store` (+ get_app for authz).
   `get_app` DOES hit the DB (wb_apps node) but is cacheable.

---

## 4. Target architecture (refactor TO) — staged

Stage gates: each stage compiles, passes existing behavior checks (manual curl suite +
stress harness from phase 2 of the main task), and can be reverted independently by
`git checkout <stage-start>` — commits are cut PER STAGE.

### Stage 0 — Prerequisites (no behavior change)
0.1 Switch `Server.engine` type alias: introduce
    `pub type SharedEngine = Arc<parking_lot::Mutex<ServerlessEngine>>;` in server::lib
    and mcp together (mechanical `.lock().unwrap()` → `.lock()`; poisoning disappears).
    Add `parking_lot = "0.12"` to server+mcp Cargo.toml (workspace dep).
    *Rollback: revert commit; zero semantic change.*
0.2 Fix remaining async-context violations (bg loops + ai_proxy): wrap ttl sweep body,
    scheduler due-scan body, webhook due-scan body, ai_proxy secrets_map in
    `spawn_blocking`. Small, isolated.

### Stage 1 — Lock-free static assets + authz cache (biggest win, lowest risk)
1.1 Add `AppCache: RwLock<HashMap<String,(Board,Instant)>>` on Server (TTL 5s).
    `get_app_cached(board)` fills from engine on miss.
1.2 `static_site`, `asset_get/put/delete/list`, `files_*` handlers, `board_of`,
    `principal_of`(sessions still need DB — see 1.3), `board_exists`, `public_reads`
    switch to the cache for the get_app part.
1.3 Asset bytes themselves: bypass the engine entirely — construct
    `FsObjectStore` handle ONCE at startup (`Server.static_store: Option<FsObjectStore>`)
    and read/write assets directly with `asset_key()` helpers from engine::files
    (they're pub). Engine lock NOT taken for asset bytes at all.
    Authz uses AppCache.public_reads + principal (unchanged logic).
1.4 `resolve_principal`: keep under engine lock for now (it reads wb_sessions/wb_keys);
    it becomes fast after Stage 2 because it stops queuing behind scans.
    *Rollback: revert stage commit — Server gains two fields, no API change.*

### Stage 2 — Split the single Mutex into per-board write locks + shared read
Design: engine internally gets `RwLock` per concern? NO — simplest correct model given
invariant 1:

2.1 Keep ONE `parking_lot::Mutex` but introduce **lock scoping discipline**: engine
    methods are split into `*_locked` inner fns that assume the guard, and public fns
    that take `&Self` + acquire internally per OPERATION, never across await/IO beyond
    one Helix round-trip. Concretely:
    - `automation::dispatch` refactored to TWO phases:
      phase A (under lock): load recipes, match, dedup-mark, compute action plans,
      perform DB-only actions (insert/set/patch/write-back);
      phase B (NOT under lock): execute `$call` HTTP actions + webhook enqueue… 
      BUT write-back after $call must RE-acquire the lock → new fn
      `engine.apply_recipe_writeback(board, table, seq, payload)` public API.
    - `run_job_blocking` (jobs.rs:83-174): lock only around reschedule + job_mark +
      job_run_insert; `dispatch_cron_job` runs as: collect due recipes (locked),
      run actions unlocked, write back (locked). Mirrors dispatch split.
2.2 Read parallelism: add `engine.read_view()` returning guard wrapper? Deferred —
    with CachedDatabase(30s TTL) most reads already avoid Helix; the big win was
    stopping WRITE+IO monopolization. Reads still serialize among themselves briefly;
    acceptable for v1 (measure in stress; revisit if p95 read wait > 100ms).
    *Rollback: stages are separate commits; automation/job changes revert cleanly; the
    public API additions (`apply_recipe_writeback`) are additive.*

### Stage 3 — Per-board sharded write locks (ONLY if stress shows contention)
3.1 `Server.engine` becomes `EngineShard: { inner: FairMutex<ServerlessEngine> }`? Not
    possible — one Box<dyn Database>. Realistic shard: keep single engine, add
    `write_permits: Arc<Semaphore>`-style admission (see Stage 4) instead of true
    per-board locks. TRUE sharding requires `Database: Send` split per board — out of
    scope; documented as rejected alternative.

### Stage 4 — Backpressure (pairs with P5)
4.1 `tokio::sync::Semaphore` (permits = SRV_MAX_DB_CONCURRENCY, default 8) acquired in
    `handle()` BEFORE spawn_blocking for routes classified DB-heavy
    (tables/query|aggregate|records writes|import|graph|resources), NOT for
    healthz/assets/auth-me/system-health.
4.2 On acquire failure (try_acquire, queue depth > SRV_MAX_DB_QUEUE default 64):
    503 + Retry-After: 2.
4.3 Metrics: obs counters `db_wait_ms_total`, `db_shed`.

### Stage 5 — Seq allocator (REQUIRED only if/when write parallelism >1)
Adopt NOW as cheap insurance even in single-writer mode:
5.1 New reserved counter row per (board,table) in `wb_counters` label:
    `allocate_seq(db, board, table) -> i64`: single Helix write op that increments and
    returns the new value (add_n with unique key + read-back, or set_property with
    post-read). Fallback: if counter row missing, initialize from next_seq() once.
5.2 Replace `next_seq()` calls in record_insert / record_bulk_import with allocator.
    Keep old fn for migration fallback until verified.
    *This is SAFE under the current single mutex too (idempotent upgrade).*

---

## 5. The next_seq hazard (why Stage 5 exists)

Current: `Query{board,table, limit:1}` (helix orders seq DESC by default in fetch_rows
via adapter's implicit default) → first row = max seq → +1 → insert Key(seq).
Two concurrent inserts between read and write ⇒ same seq ⇒ second insert OVERWRITES
first (write_node upserts by _srv_key+table where key=seq!). Data loss, silent.
Today impossible ONLY because the global Mutex spans next_seq→db.insert.
The moment ANY refactor lets two record-insert operations overlap, this breaks.
=> Rule: no stage may enable overlapping record writes before Stage 5 lands.

## 6. Test/verification matrix (per stage)

- `cargo check` clean (workspace root, no flags).
- Behavior suite (scripts/live_check.sh, created in stress phase):
  healthz, system/health, tables CRUD, record CRUD incl. unique-key dup → 400,
  aggregate count matches inserted N, search q=, resources counts, auth signup/login/me,
  graph link/traverse on test board, SSE receives live event, WS initial rows, MCP
  records.list parity with REST.
- Concurrency probe: 32 parallel record inserts (distinct payloads) → expect 32 distinct
  seqs, zero 500s (validates §5 whenever write parallelism changes).
- Freeze probe: start a 30s+ artificial slow query (big aggregate on loaded board),
  concurrently GET /srv/<board>/index.html → MUST return <100ms after Stage 1
  (this is THE regression test for the original incident).
- Rollback drill: after each stage, `git revert` of the stage commit must restore prior
  behavior with no code references left dangling (checked by cargo check).

## 7. Open decisions (ask user before implementing stage)
Q1 Unique-key race tolerance under parallel writes: (a) keep per-op lock (serialize
writes per board — chosen default), (b) accept last-write-wins window.
Q2 Semaphore defaults: 8 permits / 64 queue OK?
Q3 AppCache TTL 5s acceptable for public_reads flips to propagate?

## 8. File-touch map (expected diff surface)

| Stage | Files |
|---|---|
| 0.1 | crates/server/Cargo.toml, crates/mcp/Cargo.toml, Cargo.toml (workspace deps), lib.rs(server,mcp), every .lock().unwrap() site listed in §2 (~60 sites mechanical) |
| 0.2 | crates/server/src/jobs.rs, transport/ai_proxy.rs |
| 1 | crates/server/src/lib.rs (fields+init), transport/rest.rs (static_site/asset_*/board_of/board_exists/public_reads/files_*), resources.rs optional |
| 2 | crates/engine/src/automation.rs (phase split + apply_action plan/exec split), crud.rs (insert/set/patch/delete call sites pass-through), engine.rs (pub apply_recipe_writeback), server/jobs.rs (run_job_blocking split), mcp unchanged (goes through same engine fns) |
| 4 | crates/server/src/lib.rs (semaphore field), transport/rest.rs (admission in handle), config.rs (env knobs), observability/mod.rs (counters) |
| 5 | crates/engine/src/crud.rs (allocator), storage/database.rs (trait method w/ default), server/db/helix.rs (counter impl), storage/memory.rs (impl) |

## 9. Rollback procedure (per stage)

Every stage = exactly one git commit touching only its file set (§8).
Roll back stage N: `git revert <sha-N>` → cargo check → rerun behavior suite.
Because stages are layered bottom-up and each is independently compilable, reverting a
later stage never requires reverting earlier ones. The ONLY cross-stage coupling:
Stage 3/parallel-writes require Stage 5 first — enforced by review checklist, not code.

## 10. Current-state quick reference (for writing the fix)

Key symbols:
- `Server` struct: crates/server/src/lib.rs:19-24 (engine, broker, limiter, obs)
- `serve_combined`: crates/cli/src/main.rs:302-340 (routes /mcp vs ws vs rest)
- `handle()` async entry: rest.rs:172-333 (SSE branch 308-321; spawn_blocking 323)
- `route_blocking`: rest.rs:494-562
- Engine write chain: crud.rs:391-425 (insert), 500-523 (set), 530-549 (patch), 563-583 (delete)
- Automation dispatch: automation.rs:156-196; cron: 198-230; $call exec: ~630-674
- Cron runner: server/jobs.rs:83-174 (lock span 89-92 = worst IO-under-lock)
- Webhook delivery: server/jobs.rs:193-285 (POST outside lock — good example)
- Assets: engine/files.rs:121-235 (pure store ops, no DB) — enables Stage 1.3
- AppCache candidates: get_app callers listed in §2.1
- CachedDatabase: engine/storage/cache.rs (30s TTL, MAX 10k entries, no coalescing)
- next_seq: crud.rs:148-163 · exact_filter: 87-95 · bulk_import: 462-487

---

## 11. Implementation notes (what actually shipped, commit ef4bf9c)

Deviations from the original plan, all verified by stress + live deploy:

- **Stage 5 shape:** no `wb_counters` table row; the Helix adapter uses a dedicated
  `__srv__counter` LABEL with one node per (tenant, board/table), property `counter`.
  Atomicity via optimistic CAS: read counter → `set_property` whose input
  `nodes_where` predicate INCLUDES `counter == expected` → if zero nodes updated,
  a racer moved it: re-read and retry (32 attempts). Create-if-absent path tolerates
  duplicate-node races because only the get_by_key node is ever CAS'd.
- **Stage 2a shape:** the planned generic `dispatch_recipes_phased<G,R>` was rejected
  (`MutexGuard<'static>` unobtainable; over-abstracted). Shipped instead:
    - `automation::{HttpMode, PendingHttp, DispatchOutcome, dispatch_phased,
      execute_pending, apply_call_results}`
    - `ServerlessEngine::{dispatch_recipes_phased_a, apply_call_results}` — Phase A
      under the caller's guard; caller drops the guard; Phase B = execute_pending;
      Phase C (write-back) re-acquires.
    - Wired at: rest.rs events_inbound. crud.rs record_insert/set/patch/delete keep
      calling plain `dispatch` (Phase A + inline Phase B) — their HTTP still runs
      under the request's lock scope; eliminating THAT fully requires per-request
      lock scoping in route_blocking (future work, low priority: recipes with $call
      are rare and bounded by timeout_ms).
- **Stage 2b:** `dispatch_cron_phased` + `dispatch_cron_job_phased_a`; run_job_blocking
  releases the lock before cron recipe HTTP. The job-action http_call (jobs.rs ~139)
  already ran outside the lock.
- **Stage 4:** `is_db_heavy()` exempts OPTIONS, healthz, /api/system/health, SPA+assets
  (/srv/<board>/...), AI proxy. Shed condition approximates queue depth from
  Obs::in_flight vs db_queue_cap. Permit held for the whole handle() (dropped after
  spawn_blocking join).
- **New env knobs:** SRV_MAX_DB_CONCURRENCY (default 8), SRV_MAX_DB_QUEUE (default 64).
- **Measured effect:** parallel-insert latency 36ms -> 5ms/req (allocator removes the
  per-insert max-scan); freeze-probe unchanged PASS (asset p50=1ms during scans).

Rollback anchors: `git reset --hard 0a7b2c1` (pre-stages-2/4/5) or `22b2487` (pre-day).
