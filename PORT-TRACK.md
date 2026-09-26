# serverless-worker port tracker — multi-tenant daemon → single-tenant Workers engine

> Goal: a deployable Cloudflare Worker (one Worker = one tenant/app) that serves the
> full engine API over HTTP, backed by D1 + R2 + Durable Objects.
> Source of truth for WHAT/WHY: `WORKER-PORT-GUIDE.md`. This file tracks execution status.
> Rule: local verification is `cargo check` only; builds/tests/wasm gates run in CI (`AGENTS.md`).

_Last updated: 2026-09-26 — PUSHED to `main` (`5ee6fc3`), CI running. All local
gates were green pre-push (native + `--tests` + wasm, both forbidden-crate
greps). Remaining: CI verdict + live matrix (needs your Cloudflare account)._

## Phase status

| Phase | Scope | Status | Verify |
|---|---|---|---|
| 0 | Tracking setup (this file, todos, AGENTS.md rule) | ✅ done | file exists |
| 1 | Engine wasm-compat fixes (uuid/js, getrandom/wasm_js, cache cfg-gate, SystemTime→chrono, SECRET_KEY) | ✅ done | `cargo check --workspace` green 2026-09-26 |
| 1b | Remove `bcrypt`/`jsonschema`/`jsonwebtoken` + replacements (allowlist validator, sha2 auth, delete oauth) | ✅ done | `cargo check --workspace` green; engine graph free of the 3 crates |
| 2 | Single-tenant collapse (`board_id` → `TENANT`) in engine + mcp | ✅ done | `cargo check --workspace` green 2026-09-26 |
| 3 | Donor trim: workspace = engine + mcp; `server`/`cli`/`helixdb` excluded from build (files kept for harvest); mcp transport + tokio/hyper deleted | ✅ done | `cargo check --workspace` green 2026-09-26 (2m41s) |
| 4 | `crates/worker` skeleton (cdylib, fetch + scheduled stubs, `TenantDO` stub, CI `worker-dist/` artifact) | ✅ done (skeleton; CI build runs in CI) | wasm `cargo check` green 2026-09-26 |
| 5a | ASYNC PIVOT: `Database`/`ObjectStore`/`HttpCaller` → `async_trait(?Send)`; engine + mcp async | ✅ done | native + `--tests` + wasm `cargo check` green 2026-09-26 |
| 5b | D1 `Database` + R2 `ObjectStore` adapters + contract tests | ✅ done (code; live verify Phase 8) | wasm `cargo check` green; CI `cargo test` |
| 6 | Full fetch router (data+admin+assets, CORS, `WORKER_KEY` auth) | ✅ done (code; live verify Phase 8) | wasm `cargo check` green, warning-free |
| 7 | Scheduled + queue + TenantDO (usage/rate/sweep) | ✅ done (code; live verify Phase 8) | wasm `cargo check` green, warning-free |
| 8 | Gates, wrangler final, docs, donor deletion, **ready-to-test** | ✅ done (code; CI + live need you — see below) | all local gates green 2026-09-26 |
| 9 | FOLLOW-UP (not blocking testing): realtime WS/SSE fan-out, Analytics usage pipe, multipart upload | ⬜ pending | live workerd iteration |
| 10 | De-boardification: stored `board_id` deleted, `Board`→`Tenant`, `wb_apps`→`wb_tenant`, `tenant_cond` deleted, TENANT env dropped, `ArgType::Board` dropped, `apps.*`→`tenant.*`, prose swept | ✅ done | native + `--tests` + wasm green 2026-09-26 |
| 11 | Endpoint review deltas: HTTP `GET /recipes/:name`, dropped singular `/record`, `PUT /api/rate`; MCP `records.update/patch`, `tables.config`, `keys.issue`, `auth.logout` + specs; `Null`-clears fix; `keys.show` hash leak fixed | ✅ done | native + `--tests` + wasm green, warning-free 2026-09-26 |
| 12 | CLI-over-MCP: single `manage_serverless_engine` tool (grammar, `--help` everywhere, trailing-JSON bodies); terminal doors `GET /mcp?command=` + `POST {"command"}` | ✅ done | native + `--tests` + wasm green 2026-09-26 |
| 13 | `/mcp` setup sheet (no command → client config + terminal usage, secrets never echoed); open-if-no-`WORKER_KEY` rule on the management door | ✅ done | wasm green 2026-09-26 |
| 14 | AI proxy restored (HTTP full-fidelity + framed WS bridge, `/srv/ai/*` + legacy `/ws`); SSE record stream (auth-gated, D1-polled) | ✅ done (code; needs live AI backend to verify) | wasm green, warning-free 2026-09-26 |
| 15 | Generic OIDC login (any provider: discovery, PKCE/secret, RS256 via pure-Rust `rsa`, sealed state cookie, in-app redirects, provision-on-login) + crypto tests | ✅ done (code; discovery/exchange need a real provider) | native + `--tests` + wasm green 2026-09-26 |

Legend: ⬜ pending · 🔨 in progress · ✅ done · ⚠️ blocked · ❌ dropped

## Phase 1 checklist — DONE 2026-09-26

- [x] `uuid` → `features = ["v4", "js"]` (workspace `Cargo.toml`)
- [x] target-gated `getrandom = { version = "0.4", features = ["wasm_js"] }` for wasm32 (`engine/Cargo.toml`)
- [x] `storage/cache.rs` → `#[cfg(not(target_arch = "wasm32"))]`
- [x] `SystemTime::now()` → chrono in `crud.rs:gen_board_id`, `files.rs:file_id`, `policy.rs:now_secs`, `expr/funcs.rs:rand_byte`
- [x] `secrets.rs` master key: `SRV_SECRET_KEY` → `SECRET_KEY` + `set_master_key()` OnceLock injection for the Worker
- [x] `cargo check --workspace` green (only 2 pre-existing `server` warnings)

## Phase 1b checklist — DONE 2026-09-26

- [x] `schema.rs`: allowlist validator (`type`/`required`/`properties`/`items`/`enum`/`minimum`/`maximum`/`minLength`/`maxLength`; lenient on unknown keywords)
- [x] `auth.rs`: salted sha2 passwords (`v1$<salt>$<hex>`, constant-time verify); `$2` legacy rejected with re-register error
- [x] deleted `engine/src/oauth.rs`, `ServerlessEngine::{login_user_by_email,user_by_email,oauth_states,set/check_oauth_state,oauth_config}`, server `transport/rest_oauth.rs` + route arms; pruned stale `endpoints.rs` SSO docs + `registry.rs` bcrypt copy
- [x] removed `bcrypt`/`jsonschema`/`jsonwebtoken` from workspace + engine manifests (lock pruned by `cargo check`)
- [x] hand-rolled HS256 (`sign_jwt`/`verify_jwt`) kept — sha2+base64 only

## Phase 5a checklist — DONE 2026-09-26

- [x] `async-trait` dep; `Database`/`ObjectStore`/`HttpCaller` → `#[async_trait(?Send)]`
- [x] recursion breaker: 4 `dispatch*` fns return boxed futures (`*_inner` async bodies)
- [x] engine + mcp fully async (scripted transform, compiler-verified)
- [x] `await`-in-sync-closure fix (bulk import → `async` block); private helpers
  (`save_table`, `find_user`) converted; `srv_http_call` async; `cache.rs` ported
- [x] audit: zero unused-future warnings; multi-line chains joined; tail calls awaited
- [x] `cargo check` green: native, `--tests`, and wasm32 for engine + mcp

## Phase 5b checklist (adapters written, compiling)

- [x] `d1_db.rs`: key-prefixed tables, `INSERT OR REPLACE`, tenant/table SQL
  prefilter + Rust-side full filter via shared `apply_query` (zero drift vs
  memory), chunked `IN`-deletes, `upsert` parity, atomic `allocate_seqs`
  (`INSERT … ON CONFLICT … RETURNING`), every batch fuses its `CREATE TABLE
  IF NOT EXISTS` (self-healing fresh DB, 1 round trip)
- [x] `r2_store.rs`: 1:1 keys, local sha256, prefix list with cursor loop,
  get+put `copy`; content-types stay in records/paths (not blob metadata)
- [x] `memory::apply_query` extracted + shared (memory impl now delegates)
- [x] `crates/engine/tests/tenant_flow.rs`: 14 end-to-end contract tests
  (tenant/tables/records/schema-computed-unique-redact/recipes/search-aggregate/
  keys/users/secrets/jobs/hooks/files-assets/audit-ttl/links-rate)
- [x] `crates/mcp/tests/smoke.rs`: agent-surface test (discovery + submit/get)
- [x] links bug fixed (`$.board_id` filter never matched `Link` rows → joins dead)
- [x] test targets compile (`--tests` check green; execution in CI)
- [ ] live D1/R2 verification against real bindings (Phase 8, wrangler)

## Phase 6 checklist — DONE 2026-09-26 (code-complete, compiles)

- [x] `cors`/`auth`/`query` helpers; per-request `Ctx` (D1+R2 engine, principal)
- [x] auth: `WORKER_KEY` (secret→var fallback, constant-time) → admin, else
  engine keys/users/sessions + `public_reads`; `?key=`/Bearer/X-Srv-Key tokens
- [x] 15 tables/records routes + 30 admin routes + app/auth/upload/file/call/
  inbound/assets/system/mcp/site routes (donor params/shapes/statuses kept)
- [x] customer scope: query conds + point ownership 404s (donor intent, was dead)
- [x] `FetchCaller` egress installed per request (`Fetch::Request().send()`)
- [x] SSE/WS → 501 placeholder (streaming behind `TenantDO` in Phase 7)
- [x] deviations: asset-list now read-gated; deliveries route returns due queue
  (was `[]` stub); link `to` field ignored (was mislotted); recipe `when`
  accepts object form; config TTL/link/computed reflect per-table model
- [x] wasm `cargo check` green, zero warnings; shell graph clean
- [ ] live behavior verification (Phase 8, deployed worker)

## Wasm verification — DONE 2026-09-26

- [x] `cargo check -p engine --target wasm32-unknown-unknown` green (local)
- [x] forbidden-crate grep clean for engine + mcp wasm graphs (local `cargo tree`)
- [x] `worker` SDK 0.8.7 API verified via docs.rs (Router/Env/scheduled/DO)

## Phase 4 checklist

- [x] `crates/worker` (`serverless-worker` pkg, cdylib, `worker = "0.8"` + `d1`/`queue` features)
- [x] `lib.rs`: `#[event(fetch)]` → router; `#[event(scheduled)]` stub
- [x] `router.rs`: `/healthz`, `/api/version`, 404 JSON (CORS + full surface in Phase 6)
- [x] `tenant_do.rs`: `TenantDO` stub (`DurableObject::new` + `fetch`)
- [x] `wrangler.toml` main → `build/serverless-worker/shim.mjs` (worker-build names by package)
- [x] CI: native check/test exclude the wasm-only shell; wasm-gate checks all three
      crates with split forbidden-crate greps (tokio allowed only in shell graph)
- [x] `cargo check -p serverless-worker --target wasm32-unknown-unknown` green
  (needed direct `wasm-bindgen` dep for the `durable_object` expansion)
- [ ] CI `worker-build --release` produces `worker-dist/` (verified in CI, not locally)

## Phase 2 checklist — DONE 2026-09-26

- [x] `pub const TENANT: &str = "singleton"` (`engine::lib`); matches `wrangler.toml`
- [x] dropped `board_id`/`board` params from all engine + mcp APIs (facade → leaves);
  stored/compared values use `TENANT` (`tenant_cond()`, `tenant_key()`)
- [x] deleted multi-app machinery: `app_create*/app_by_id/app_list/app_update/app_delete`,
  `load_board`, `gen_board_id`, `token == board_id` owner shortcut, oauth SSO path,
  `apps.create/list/delete` MCP verbs + registry specs
- [x] single `TENANT` config row in `wb_apps` via `tenant_config()` (lazy create) /
  `save_tenant()` / `tenant_update()`; facade exposes `tenant()` / `update_tenant()`
- [x] model struct `board_id` FIELDS kept (stored-row shape, always `TENANT`); `Notify`
  dropped to `(kind, seq, payload)` (no in-workspace consumers)
- [x] cross-board recipe actions (`$upsert_other/$patch_other/$transaction`) collapsed
  to tenant (legacy `board` field ignored)
- [x] registry: board positionals removed from all specs; `args:` examples + `v1$`
  hash example fixed (`response:` prose refresh deferred to Phase 8)
- [x] `cargo check --workspace` green (1 pre-existing `cache.rs` warning)

## Harvest map (donor files — DELETED from disk in Phase 8, live in git history)

Harvested into the worker in Phases 5b–7. Recover donor sources with
`git show <pre-deletion-commit>:<path>` (deletion commit: see `git log`).

| Harvest | Location (line refs at time of trim) |
|---|---|
| Route table core (`match (method, tail)`, no `{board}` in worker) | `crates/server/src/transport/rest.rs:635-662` |
| Tables/records dispatcher | `crates/server/src/transport/rest_tables.rs:459-487` |
| Admin dispatcher (all admin-gated) | `crates/server/src/transport/rest_admin.rs:17-64` |
| Pure auth guards (`require_read/write/admin`) | `rest.rs:214-232`, `rest_admin.rs:9-15` |
| CORS constants + preflight | `rest.rs:26-56,246-248` |
| WS/SSE filter helpers (`filter_matches/value_at/eq_value`) | `crates/server/src/transport/ws.rs:171-202` |
| Event envelope shapes (broker→DO WS) | `lib.rs:182-192`, `sse.rs:42-55`, `ws.rs:157-169` |
| Job runner (`run_job_blocking` phases A/B, `extract_path`, backoff `[0,1,5,30,120]`, `next_attempt_iso`, `fmt_now`) | `crates/server/src/jobs.rs:8-14,35-54,97-223,263-325` |
| Webhook deliver (HMAC base64 `X-Srv-Signature`, SSRF gate) | `jobs.rs:263-325` |
| `/resources` route shape + response JSON | `crates/server/src/resources.rs:86-121` (source numbers from DO + D1 meta per guide §4) |
| `HttpCaller` trait contract (re-implement on `worker::Fetch`) | `crates/engine/src/http.rs:9-19`, impl was `server/src/http_caller.rs` |
| `handle_jsonrpc` (mount as `/mcp` route — already pure) | `crates/mcp/src/lib.rs:135` (mcp crate KEPT) |
| CommandSpec/registry data (CLI parity later) | `crates/engine/src/registry.rs` (KEPT in engine) |
| Env mapping SRV_* → worker vars | guide §6 + `server/src/config.rs:33-47` |

## Phase 8 checklist

- [x] release profile → `opt-z` + thin LTO + strip (`[profile.release]`)
- [x] CI wasm size gate (fail > 60 MiB, warn > 30 MiB)
- [x] `sync-worker.sh`: `build/serverless-worker/` paths + idempotent queue creation
- [x] guide staleness fixed (async deviation §2.2, shim path, queue config)
- [x] `endpoints.list` route index rewritten to the tenant-less surface
- [x] donor crates (`server`/`cli`/`helixdb`) deleted (in git history)
- [x] README / OVERVIEW / FEATURES status + deltas
- [ ] CI green on push (native check + tests, wasm gates, `worker-build` + size)
- [ ] live matrix below passes against `wrangler dev` / deployed worker

## Live test matrix (run with `wrangler dev` or the deployed URL)

Prereqs: D1/R2 ids in `wrangler.toml`, `.dev.vars` (or secrets) with
`WORKER_KEY` + `SECRET_KEY`, queue created (or inline-delivery fallback).
`export K="Authorization: Bearer $WORKER_KEY"`.

```sh
BASE=http://localhost:8787   # or https://serverless-worker.<acct>.workers.dev
curl $BASE/healthz                                        # {ok:true}
curl $BASE/mcp                                            # setup sheet: MCP client config + terminal usage
curl $BASE/api/version                                    # version + singleton
# tables + records
curl -H "$K" -X POST $BASE/api/tables -d '{"table":"notes"}'
curl -H "$K" -X POST $BASE/api/tables/notes/submit -d '{"body":"hi"}'  # seq 1
curl -H "$K" "$BASE/api/tables/notes/records"             # 1 record
curl -H "$K" "$BASE/api/tables/notes/query?filter=%7B%22body%22%3A%7B%22contains%22%3A%22hi%22%7D%7D"
# keys (reader) + public reads off by default → 403 anon, 200 with key
curl -H "$K" -X POST $BASE/api/keys -d '{"role":"reader"}'  # save .key
curl "$BASE/api/tables/notes/records"                     # 403 (private app)
# assets + SPA
echo '<h1>hi</h1>' | curl -H "$K" -X PUT --data-binary @- $BASE/api/assets/app/index.html
curl -H "$K" $BASE/api/assets                             # lists app/index.html
# recipes + jobs + webhooks + secrets + audit
curl -H "$K" -X POST $BASE/api/recipes -d '{"name":"s","when":{"event":"record.created"},"actions":[{"$set":{"$.s":1}}]}'
curl -H "$K" -X POST $BASE/api/jobs -d '{"name":"j","schedule":"*/5 * * * *","action":{"type":"http","url":"https://example.com"}}'
curl -H "$K" -X POST $BASE/api/hooks -d '{"url":"https://example.com/hook"}'
curl -H "$K" -X POST $BASE/api/secrets -d '{"name":"K","value":"v"}'
curl -H "$K" -X PUT $BASE/api/audit -d '{"enabled":true}'
curl -H "$K" $BASE/api/config                             # aggregate snapshot
# MCP agent surface
curl -H "$K" -X POST $BASE/mcp -d '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"records.submit","arguments":{"table":"notes","payload":{"body":"via-mcp"}}}}'
# Single-tool CLI door + terminal door
curl -H "$K" -X POST $BASE/mcp -d '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"manage_serverless_engine","arguments":{"command":"records list notes"}}}'
curl -H "$K" "$BASE/mcp?command=tenant+show"
# Generic OIDC (manual: needs a real provider; configure then open start in a browser)
curl -H "$K" -X POST $BASE/api/secrets -d '{"name":"OAUTH_ISSUER","value":"https://<provider>/"}'
curl -H "$K" -X POST $BASE/api/secrets -d '{"name":"OAUTH_CLIENT_ID","value":"<id>"}'
# open: $BASE/api/auth/oauth/start?redirect=/srv/  → provider → back with ?srv_token=
# cron fast-path (dev only): curl -v "http://localhost:8787/__scheduled?cron=*/5+*+*+*+*"  (needs wrangler dev --test-scheduled)
```

## Phase 7 checklist — DONE 2026-09-26 (code-complete, compiles)

- [x] `scheduled.rs`: due jobs (reschedule-first, phased recipes, Fetch actions,
  http/poll incl. table insert, mark + run history) + TTL sweep + webhook flush
- [x] `queue.rs`: claim-and-enqueue from scheduler; consumer delivers with
  per-hook HMAC + `[0,1,5,30,120]`s backoff (max 5); inline fallback without binding
- [x] `tenant_do.rs`: KV-backed usage days + fixed-window rate checks + `/sweep`
  (TTL + due census); wired in `wrangler.toml` with migration tag
- [x] `wrangler.toml` queue producer/consumer config; script creates the queue
- [x] wasm `cargo check` green, warning-free
- [ ] live behavior verification (Phase 8 matrix + cron/queue firing for real)

## Phase 10 checklist — DONE 2026-09-26

- [x] stored `board_id` removed from all rows/structs/filters (incl. `tenant_cond()`)
- [x] `Board` → trimmed `Tenant`; `wb_apps` → `wb_tenant`; `scoped_key` folded away
- [x] links bugfix kept (`child_board` scoping); default `aggregate` fixed to match
- [x] TENANT env var deleted (was unread decoration); `ArgType::Board` deleted
- [x] MCP `apps.show/update/resources` → `tenant.*`; outputs say `tenant`
- [x] registry prose swept (REST paths, Helix→D1, CLI→tool, graph-backend notes)
- [x] caught + restored a dropped `job_add` insert via write-path audit
- [x] fixed adjacent leak: MCP `keys.show` exposed `key_hash`/`salt`
- [x] tests updated; native + `--tests` + wasm green, graphs clean

## CI incident log (2026-09-26)

- First green-ish signal: `wasm-gate` ✅ passed on the big port push.
- `native-check` ❌ on the first-ever `cargo test` run: pre-existing
  `expr::tests::ternary_is_right_associative` expected `1.0`, evaluator
  correctly returns `1` (suite convention: ints stay ints). Fixed the test
  (not the evaluator) + added a real associativity assertion.
- Next run: all 24 lib tests pass, then 36 s of silence and an EXTERNAL
  cancel (no `running N tests`, no FAILED — cargo never spawned the next
  binary, so no test hung or failed). Re-ran failed jobs; if it recurs,
  suspect runner preemption, not code.

## Decisions log

- 2026-09-26: keep chrono defaults (`wasmbind` gives `js_sys::Date` on wasm); do NOT follow guide §3's `time`-crate advice — it would break wasm (decision from wasm-dep survey).
- 2026-09-26: local rule = `cargo check` only; added `cargo test --workspace` to CI `native-check` so tests actually run somewhere (`AGENTS.md`).
- 2026-09-26: `storage/cache.rs` has zero in-engine callers → cfg-gate it out on wasm instead of porting `Instant`.
- 2026-09-26: order swap — Phase 3 (donor trim) before Phase 2 (collapse), so the
  collapse only touches engine + mcp and checks stay fast. Donor files stay on
  disk until harvested (Phase 8 deletes them).
- 2026-09-26: Phase 2 scope — drop `board_id`/`board` PARAMS from engine + mcp
  APIs (facade → leaves), replace stored/compared values with `TENANT`. Model
  STRUCT fields named `board_id` stay (stored-row shape + data compat) but are
  always `TENANT`. `Board`/`TABLE_APPS` multi-app machinery is deleted; `Notify`
  dropped to `(kind, seq, payload)` (no in-workspace consumers).
- 2026-09-26: worker package named `serverless-worker`, NOT `worker` (SDK crate
  owns that name; `use worker::…` would self-resolve and `worker-macros`
  hardcodes `::worker::` paths, so the dep cannot be renamed instead).
- 2026-09-26: worker shell is wasm-only (`cargo check` natively fails inside
  wasm-bindgen macros — expected). Local rule: `--exclude serverless-worker`
  natively, `-p serverless-worker --target wasm32-unknown-unknown` for the shell.
- 2026-09-26: split wasm-gate greps — engine/mcp forbid
  tokio|hyper|reqwest|bcrypt|jsonschema|jsonwebtoken; shell forbids all but
  tokio (worker's own wasm-shimmed copy).
- 2026-09-26: GUIDE DEVIATION (§2.2 "engine stays sync") — impossible: D1/R2
  bindings are async-only and wasm has no blocking executor, so the storage
  seam MUST be async. `Database`/`ObjectStore` become `#[async_trait(?Send)]`
  (`?Send` because worker futures like JsFuture are `!Send`; nothing on the
  isolate requires `Send`). Engine + mcp go async method-by-method
  (compiler-guided); native tests use `futures-executor::block_on` (NOT tokio —
  still banned). Memory adapters go async too (same trait).

## Blockers / risks

- `endpoints.rs` route index still documents `{board}` paths (static strings) — rewrite
  in Phase 6/8 when the worker router lands (agent discovery pack).
- `Link.{child_board,parent_board}` struct fields kept (always `TENANT`); `get_link`
  family filters on `$.board_id` which `Link` rows don't carry (pre-existing quirk,
  semantics preserved — joins return unjoined rows as before).
- `per-table` MCP verbs for computed/validate/redact/ttl don't exist (old
  `apps.update` applied them to a hardcoded `records` table / silently dropped
  schema) — decide in Phase 6 whether to add `tables.*` verbs.
- local `wasm32` target now installed for `cargo check --target` signal; release
  builds/tests/`worker-build`/`wrangler` still CI-only per AGENTS.md.
