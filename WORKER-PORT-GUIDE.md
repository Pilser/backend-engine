# serverless-worker — Cloudflare Workers Port Guide (single-tenant)

Target: one Worker binary = one tenant (one app). No multi-app, no `board_id` routing.
Source copy: `crates/*`, `docs/*`, `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml` from `../serverless-engine/`.
Excluded on purpose: `target/`, `turso/`, `zerowrapper/`, `topcoat/`, `deploy/`, `backups/`, `Production-folder*`, `waos-data/`, `.git/`.

## 0. Decisions already made (do not re-debate)

1. `crates/engine` is the keeper. It is sync, has no tokio/hyper/reqwest (verify: `grep -r tokio crates/engine/src` must be empty). It compiles to `wasm32-unknown-unknown` after the dep fixes in §3.
2. Single tenant: hardcode one `TENANT` (env var). Delete all `board_id` params, `*_list_apps`, `app_create/delete`, per-board rate/keys tables scoping by board. Keep table/record/job/recipe/secret/file logic unchanged.
3. `crates/server`, `crates/helixdb`, `crates/cli`, `crates/mcp` are donors, not shippable. Harvest logic, delete transports.

## 1. DELETE / REMOVE (in this folder, other agent does this)

| Path | Action | Why |
|---|---|---|
| `crates/server/src/lib.rs: serve(), spawn_background(), db_permits (tokio Semaphore)` | delete fns | No `TcpListener`, no threads on Workers |
| `crates/server/src/jobs.rs: spawn_ttl_sweeper, spawn_scheduler, spawn_webhook_worker, spawn_blocking` | delete spawns, KEEP `run_job_blocking` logic + `job_due/next` math | Replaced by Cron Trigger + `waitUntil` (§2) |
| `crates/server/src/broker/in_proc.rs` (`tokio::sync::broadcast`) | delete file | Replaced by DO WebSocket Hibernation |
| `crates/server/src/transport/` (`rest.rs` hyper handlers, `ai_proxy.rs copy_bidirectional`, `rest_oauth.rs`) | keep route table + auth checks as pure fns, delete hyper framing | Re-implemented on `worker::Router` |
| `crates/server/src/http_caller.rs`, `crates/helixdb/src/client.rs` (`reqwest::blocking`, `thread::sleep`) | delete, KEEP retry/backoff constants | Replaced by `worker::Fetch` (§2) |
| `crates/server/src/db/helix.rs` | delete entirely | HelixDB can't run in Worker; optional external call via fetch later |
| `crates/server/src/db/*turso*, store/fs.rs, s3.rs, gcs.rs, minio.rs` | delete | Replaced by D1/R2 adapters (§2) |
| `crates/server/src/observability/` (cpu_percent, mem_kb) | delete CPU/RAM gauges, KEEP request counters | No process stats on Workers; use Analytics Engine |
| `crates/server/src/resources.rs` counting via full scan + in-proc limiter | rewrite per §4 | Single-tenant counters in DO storage |
| `crates/cli`, `crates/mcp/src/transport.rs`, `crates/mcp` serve paths | delete binaries/serve; KEEP `registry.rs` CommandSpec data if CLI parity wanted later | Worker exposes HTTP only |
| `engine/src/oauth.rs` (`jsonwebtoken` RS256) | delete file + `oauth_config`, `login_user_by_email` SSO path | Microsoft SSO needs external JWKS fetch; re-add later via fetch if needed |
| `engine/src/auth.rs: resolve_jwt_user` dependency on `jsonwebtoken` | already hand-rolled HS256 (`sign_jwt/verify_jwt` use only `sha2+base64`) — keep that, drop crate | See §3 |
| All `board_id` columns/params, `TABLE_APPS`, `app_*` fns, `scoped_key(board,…)` | collapse to constant tenant | Single-tenant simplification (biggest diff, mechanical) |

## 2. ADD (new code, other agent writes this)

1. `crates/worker/` (`cdylib`, `worker = "0.8"`, `wasm-bindgen-futures`): `lib.rs` with `#[event(fetch)]` Router mirroring `/api/srv/*` paths minus `{board_id}` segment, `#[event(scheduled)]` calling `job_due()` + `ttl_sweep()` + webhook flush, Queue consumer for webhook delivery with `Fetch`.
2. `crates/worker/src/d1_db.rs`: `impl engine::Database for D1Db` (worker `D1Database` binding). `query/insert/update/delete/aggregate` → parameterized D1 SQL. `allocate_seqs` → D1 counter row + `RETURNING`. **Correction (2026-09-26, see PORT-TRACK.md Phase 5a): the storage seam is `async_trait(?Send)` — a sync engine cannot await D1/R2 on wasm (no blocking executor exists), so `engine` + `mcp` are async throughout. Correctness-first adapter: tenant/table SQL prefilter, everything else filtered/ordered/paginated in Rust via the shared `apply_query` helper (zero drift vs the memory adapter).
3. `crates/worker/src/r2_store.rs`: `impl engine::ObjectStore for R2Store` (`get/put/delete/list/head` on R2 binding; assets + files prefixes).
4. `wrangler.toml`: `main = "build/serverless-worker/shim.mjs"` (worker-build names output by *package* name; ours is `serverless-worker` because the SDK owns the `worker` name), `compatibility_date`, bindings `[[d1_databases]]`, `[[r2_buckets]]`, `[[kv_namespaces]]` (optional cache), `[[durable_objects.bindings]]` (1 class `TenantDO` incl. SQLite + alarm + websockets), `[[analytics_engine_datasets]]` (usage), `[[queues.producers]]`/`[[queues.consumers]]` (`webhook-deliveries`, created via `wrangler queues create`), `[triggers] crons = ["*/5 * * * *"]`, `[build] command = "worker-build --release"`.
5. `TenantDO` (durable object): holds per-tenant counters + rate state + alarm for TTL/job sweep fallback + hibernatable websockets replacing `broadcast`. Only one instance (id from fixed name, e.g. `"singleton"`).
6. Auth: single `WORKER_KEY` secret env + optional 1 user row. `issue_key/list_keys/revoke` collapse to env check. Password users: see §3 bcrypt verdict.

## 3. DEPS REVIEW (remove / gate — this is the WASM gate)

| Crate | Used in | Verdict for single-tenant Worker |
|---|---|---|
| `tokio, hyper, hyper-util, http-body*, tokio-tungstenite, futures-util, reqwest` | server/cli/mcp/helixdb | REMOVE from worker graph entirely. `cargo tree -e no-dev` must not contain them. `tokio::sync` types alone are OK but simplest to remove all. |
| `jsonschema 0.49` | `engine/src/schema.rs:71 validate_schema` (compiled **per write** — also a perf bug) | REMOVE. Biggest binary-size + CPU risk (Worker 64MiB / 10ms–30s CPU). Single tenant means YOU control all writers, so: replace with (a) your own allowlist validator (required fields + types, ~40 lines), or (b) `valico`/`jsonschema` feature-gated `default-features=false` later. Do not ship full draft-2020-12 validation on edge hot path. Cache compiled schema if ever re-added. |
| `bcrypt 0.19` (+ `jsonwebtoken` for OAuth only) | `engine/src/auth.rs:321 verify/hash`, `oauth.rs:123 decode RS256` | REMOVE both for v1 single-tenant. `bcrypt::DEFAULT_COST` (12) is ~200ms CPU per login — fatal on 10ms free tier, wasteful on paid. Single tenant = no user table needed: auth = `WORKER_KEY` bearer compare (`sha2`, constant-time eq) + optional `aes-gcm` sealed session cookie. Keep `password_hash` column ignored. Re-add `argon2`/`scrypt` with low params or Cloudflare Access later if multi-user ever returns. `jsonwebtoken` crate: remove; internal HS256 already hand-rolled on `sha2` — keep that only if you need self-issued tokens. |
| `chrono` | timestamps everywhere | KEEP but set `default-features=false, features=["serde","alloc"]` and use `time` crate with `wasm-bindgen` feature for `now()`. `Date::now()` is frozen per-request on Workers — audit `now_str()` call sites. |
| `aes-gcm, rand, uuid, sha2, base64` | secrets, keys, salts | KEEP with `getrandom/js` (rand/uuid need `js` feature for `wasm32-unknown-unknown`; else build fails with `getrandom` error). Verify: `uuid = { features=["v4","js"] }`, `rand` pulls `getrandom` with `wasm-bindgen`. |
| `jsonwebtoken` | oauth RS256 only | REMOVE with oauth.rs (§1). |
| `tracing, tracing-subscriber` | server obs | REMOVE or swap to `tracing-web`; spans have identical start/end unless they wrap I/O on Workers. |
| `chrono-tz, axum, tokio-postgres` | not in graph today | DO NOT ADD. `worker` has `axum` feature but prefer its `Router`. |

WASM build gate (run from repo root, not `-p`):
`rustup target add wasm32-unknown-unknown` once; then
`cargo tree --target wasm32-unknown-unknown -e no-dev | grep -Ei 'tokio|hyper|reqwest|bcrypt|jsonschema|jsonwebtoken'` must print nothing (except `worker`'s own dep set — check its tokio is wasm-shimmed; `worker` re-exports a patched tokio that compiles to wasm, real `tokio::runtime` must not appear).

Size gate: `[profile.release] lto=true, strip=true, codegen-units=1, opt-level="z"` + `wasm-opt` via worker-build. Fail if `*.wasm > 30MiB` (warn) / `> 60MiB` (hard, under 64MiB limit).

## 4. USAGE TRACKING (single tenant — simplified, other agent implements)

No per-app attribution needed (1 tenant). Track: `requests_total, req_by_route, rows_read, rows_written (from D1 meta), storage_bytes (R2 list sum, cached 60s), errors, cpu_ms (DO `Date.now` deltas are useless — use `worker::signals` near-limit API + Analytics Engine instead)`.
Implementation: `TenantDO` SQLite `usage_daily(day, req, reads, writes, storage)` + `ctx.waitUntil(analytics.write(...))` per request (fire-and-forget, not on hot path). Keep `/resources` route shape from old `resources.rs` but source numbers from DO + D1 meta. Rate limit = fixed constants in `wrangler.toml`/env, enforced in fetch handler (no `RateStore` trait needed).

## 5. SUGGESTED ORDER FOR THE OTHER AGENT

1. §3 dep removals + `board_id` collapse in `engine` (must still `cargo check` native).
2. `d1_db.rs + r2_store.rs` adapters against in-memory `Database/ObjectStore` contract tests.
3. `crates/worker` fetch router (5 routes first: `records submit/get/query`, `files upload/download`) → `worker-build` → `wrangler dev`.
4. Scheduled + Queue + TenantDO + `/resources`.
5. Size/CPU gates + `wrangler deploy`.

## 6. ENV VARS — WORKERS-FRIENDLY (mandatory rules)

Old daemon vars (`SRV_HOST`, `SRV_PORT`, `SRV_DB`, `SRV_HELIX_URL`, `SRV_DATA_DIR`) are
BANNED here. Mapping:

| Old (daemon) | New (worker) | Where |
|---|---|---|
| `SRV_HOST` + `SRV_PORT` | `WORKER_URL` (public https URL, no port) | `wrangler.toml [vars]` + `.env.example` |
| `SRV_DB`, `SRV_HELIX_URL` | D1 binding `DB` | `wrangler.toml [[d1_databases]]` — never a URL |
| `SRV_DATA_DIR` | R2 binding `STORE` | `wrangler.toml [[r2_buckets]]` — never a path |
| `SRV_SECRET_KEY` | `SECRET_KEY` | wrangler secret ONLY (never in files) |
| (new) admin bearer | `WORKER_KEY` | wrangler secret ONLY |
| `SRV_HTTP_TIMEOUT_MS` | `HTTP_TIMEOUT_MS` | `wrangler.toml [vars]` (plain, non-secret) |
| per-board scoping | `TENANT=singleton` | `wrangler.toml [vars]` (constant) |

Rules: secrets via `wrangler secret put` / `scripts/sync-worker.sh` (shell env only) /
GitHub Secrets (CI). Local dev secrets go in `.dev.vars` (git-ignored, see `.env.example`).
`wrangler.toml [vars]` holds plain values only. CI needs exactly two repo secrets to
deploy: `CLOUDFLARE_API_TOKEN`, `CLOUDFLARE_ACCOUNT_ID`.

## 7. GIT / CI / DEPLOY (already wired — keep this shape)

- `main` branch. CI (`.github/workflows/ci.yml`): native `cargo check --workspace`,
  `wasm-gate` (`cargo check -p engine --target wasm32-unknown-unknown` + forbidden-crate
  grep), and `worker-build --release` → `worker-dist/` artifact once `crates/worker` lands.
- Deploy (`.github/workflows/deploy.yml`, manual dispatch): syncs `SECRET_KEY`/`WORKER_KEY`
  from GitHub Secrets, rebuilds, `wrangler deploy`. The binary Cloudflare runs ALWAYS
  comes from CI — never hand-upload a laptop build.
- Manual/urgent path: `scripts/sync-worker.sh [--env production] [--artifact DIR from
  CI] [--skip-build] [--secrets-only]`. Needs `CLOUDFLARE_API_TOKEN` in env or a
  `wrangler login` session. First-time setup: `wrangler d1 create serverless-worker`,
  paste id into `wrangler.toml`, `wrangler r2 bucket create serverless-worker-store`,
  then deploy and save the public URL back into `WORKER_URL`.
