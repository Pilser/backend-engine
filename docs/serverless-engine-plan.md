# serverlessEngine-rs — Implementation Plan

> **Status:** Planning · **Owner:** serverlessEngine-rs
>
> **Companion docs** (in this folder):
> - `serverless-engine-features.md` — full feature inventory of the existing reference engine (the behavioral reference)
> - `serverless-engine-gap-analysis.md` — gap matrix + storage abstraction design (the "why")
> - `cli-mcp-interface-design.md` — the CLI ↔ MCP control surface spec (help layers, error contract, registry; referenced by Phase 4)
>
> This document is the actionable plan: folder layout, crate boundaries, compute strategy, phases, milestones, and test gates.

---

## 1. Mission

Build a standalone, embeddable **`serverlessEngine-rs`** crate plus an optional **serverless server** on top of it, such that:

1. The **crate** is a pure library: no HTTP, no transport, no DB driver, no object-store SDK. It owns the engine logic, the query IR, the event model, and two pluggable storage traits (`Database`, `ObjectStore`).
2. The **server** provides every adapter and transport: DB backends (Turso/local, Postgres, …), object stores (FS, S3/R2/GCS, …), REST/WebSocket/SSE, identity, job/pubsub brokers, observability.
3. **Compute needs are met declaratively** — expressions, computed fields, validation rules, recipes, scheduled jobs, and outbound calls — with **no user-provided code (no WASM) in this phase**. WASM is deferred to a future phase (see §5).
4. The existing reference engine in zerowrapper/ is the reference implementation: **its tests are the behavioral contract** the new crate must satisfy.

---

## 2. Non-goals (for this phase)

- ❌ No WASM / user-provided server-side functions. Compute is configuration-driven only (see §5).
- ❌ No multi-region replication engine work (we design for it via traits, we do not build it yet).
- ❌ No distributed job brokers yet (a `JobBroker` trait exists; the in-process adapter ships first).
- ❌ No OAuth/OIDC implementation (the crate accepts a resolved `Principal`; the server ships the key/token flows).
- ❌ No billing/quotas metering (usage events are emitted; meters come later).

---

## 3. Repository & Folder Structure

New git repo at `serverless-engine/` (independent from the parent workspace repo; parent's allowlist `.gitignore` already excludes it).

```
serverless-engine/                          ← git repo root (crate workspace)
├── Cargo.toml                              workspace manifest (members below)
├── rust-toolchain.toml                     pinned toolchain
├── .gitignore                              target/, *.db, .env, etc.
├── README.md                               quick start
├── LICENSE                                 (TBD)
│
├── docs/
│   ├── serverless-engine-plan.md           ← this document
│   ├── serverless-engine-features.md       current engine reference
│   └── serverless-engine-gap-analysis.md   gaps + trait sketches
│
├── crates/
│   ├── engine/                             ★ the core crate (serverlessEngine-rs)
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs                      facade + EngineBuilder
│   │       ├── model/                      Board, Record, Key, Recipe, Secret,
│   │       │                               Job, Link, Principal, Capability
│   │       ├── storage/
│   │       │   ├── mod.rs
│   │       │   ├── database.rs             Database trait + capability matrix
│   │       │   ├── object_store.rs         ObjectStore trait + capability matrix
│   │       │   ├── ir.rs                   FilterCond/SrvFilter/Agg IR (shared)
│   │       │   └── memory.rs               in-memory adapters (tests, embedded)
│   │       ├── crud.rs                     insert/get/set/patch/delete/upsert/bulk
│   │       ├── query.rs                    filter compile, search, aggregate, sort, cursor
│   │       ├── schema.rs                   json-schema, computed, validate, redact
│   │       ├── auth.rs                     keys, scopes, RBAC policy IR
│   │       ├── realtime.rs                 event model + EventStream (publish) trait
│   │       ├── automation.rs               recipes, actions, cron, jobs (JobBroker trait)
│   │       ├── webhooks.rs                 registry + signing
│   │       ├── secrets.rs                  encrypted store interface
│   │       ├── files.rs                    blob refs + ObjectStore orchestration
│   │       ├── audit.rs                    immutable log
│   │       ├── policy.rs                   ttl, links, rate (RateStore trait)
│   │       ├── expr.rs                     port of zw-expr (extended, see §5)
│   │       ├── migrations.rs               backend-agnostic schema versioning
│   │       └── events.rs                   outbox + usage/metric events
│   │
│   ├── server/                             ★ the embeddable server
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs                      ServerBuilder (mount engine + adapters)
│   │       ├── db/                         turso.rs, postgres.rs, memory.rs
│   │       ├── store/                      fs.rs, s3.rs, r2.rs, gcs.rs, minio.rs
│   │       ├── broker/                     in_proc.rs, redis.rs (job + pubsub)
│   │       ├── transport/                  rest.rs, ws.rs, sse.rs
│   │       ├── identity/                   keys.rs, jwt.rs (flows later)
│   │       └── observability/              otel.rs, prometheus.rs
│   │
│   ├── mcp/                                ★ MCP tool server (granular tools)
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs                      tool registry over the engine (from CommandSpec)
│   │       ├── tools/                      one tool per feature (boards, records,
│   │       │                               query, search, aggregate, recipes, …)
│   │       └── transport.rs                streamable-HTTP (hosted by the daemon)
│   │
│   └── cli/                                ★ srv CLI (MCP client over HTTP) + studio
│       ├── Cargo.toml
│       └── src/
│           ├── main.rs                     `srv` entrypoint (help layers, argv→tools/call)
│           ├── client.rs                   MCP client over HTTP (Bearer auth, auto-spawn daemon)
│           ├── registry.rs                 CommandSpec model + help/schema generator
│           ├── render.rs                   human/JSON output
│           └── studio.rs                   REPL stub (partial; enhanced later)
│
└── target/                                 (ignored)
```

**Dependency direction:** `server → engine` and `mcp/cli → server`. `engine` depends on nothing workspace-local. `server` is the only place DB/object-store/broker drivers live.

---

## 4. Crate Boundaries (rules of the road)

| Concern | Lives in |
|---|---|
| Filter IR, semantics, query compilation | `engine` |
| `Database` / `ObjectStore` traits + capability matrices | `engine` |
| In-memory adapters (tests) | `engine` |
| Turso / Postgres / MySQL adapters | `server/db` |
| FS / S3 / R2 / GCS / MinIO adapters | `server/store` |
| REST / WS / SSE framing, CORS, body limits, TLS | `server/transport` |
| OAuth / OIDC / JWT / session flows | `server/identity` |
| Redis / NATS / Kafka pub-sub & job brokers | `server/broker` |
| Key / token auth (issue, hash, scope) | `engine` (logic) + `server` (crypto backend choice) |
| Granular MCP tools (generated from `CommandSpec`) | `mcp` |
| `CommandSpec` registry model + help/schema generator | `cli` (shared with `mcp`) |
| `srv` CLI — MCP client over HTTP (MAIN) | `cli` |
| `srv studio` REPL (partial) | `cli` |
| Daemon (hosts engine + MCP over HTTP) | `server` + `mcp` |
| OTel / Prometheus exporters | `server/observability` |

**Rule:** `engine` must compile and pass tests with **zero** external I/O dependencies (only in-memory adapters). This keeps it embeddable (a single process, like the current reference engine) or distributable (behind `server`).

---

## 5. Compute Strategy — Declarative, No WASM (this phase)

> The requirement: **cover all needed compute without user-written code (no WASM) for now.** WASM is a future phase, not a blocker.

### 5.1 What "compute" must cover

Every place today's engine computes something, and how we cover it **declaratively**:

| Compute need | Declarative mechanism (phase 1) |
|---|---|
| Derived fields on write | **Computed fields** — expression map `{path → expr}` (port `zw-db/logic.rs`), with a dependency DAG + cycle detection added |
| Reject bad writes | **Validation rules** — `{when: expr, error}` + JSON Schema (port `zw-db/logic.rs`) |
| Read-time masking | **Redaction** — path list (port `zw-db/logic.rs`) |
| Event-driven automation | **Recipes** — `when`/`match`/`dedup`/`actions` (port `zw-recipe`) with the **outbox** decoupling added |
| Data transformation | Recipe actions: `$compute`, `$set`, `$copy`, `$move`, `$format`, `$merge`, `$push`/`$pull`/`$sort`/`$slice`, `$set_state` (port `zw-recipe/actions.rs`) |
| Cross-board sync | Recipe actions `$upsert_other`, `$patch_other`, `$transaction` |
| Periodic / scheduled | **Cron jobs** — 5-field cron + `@every` (port `zw-db/cron.rs`, `jobs.rs`) with leases |
| Notify / call external systems | **Webhooks** (out + in, HMAC) + **`$call`/`$notify`** outbound HTTP (port `zw-db/calls.rs`) |
| Trigger external code | Webhook delivery to the user's own HTTP endpoint (the "compute" the user runs themselves) |
| Condition/guard logic anywhere | Expression language `zw-expr` (extended stdlib — see 5.2) |
| Template/layout logic | `$format` templates + `{path}`/`{{token}}` substitution (port `zw-recipe/secrets.rs`) |

**Design principle:** the expression engine is the single "programming surface." Anything an agent/user wants to compute must be expressible as a composition of: expressions, condition filters, recipe action chains, cron schedules, and outbound calls. No embedded code, no language runtime, no sandbox — therefore no RCE surface and no sandbox to maintain.

### 5.2 Expression language extension (to close compute gaps without code)

Port `zw-expr` and extend it so the missing compute needs are expressible:

- **Array functions**: `map`, `filter`, `reduce`, `sum`, `avg`, `min`, `max`, `first`, `last`, `sort`, `unique`, `flatten`, `count`
- **Aggregation over nested data**: reduce over wildcard collections (`$.orders[*]`)
- **Object/array construction**: `object(...)`, `array(...)`, `keys`, `values`, `get(path, default)`
- **More string/date/number funcs**: `substring`, `replace`, `split`, `formatdate`, `nowiso`, `diffdays`, `hash`, `random`
- **Determinism guarantees**: functions are pure except explicit `now*`/`random` (important for computed-field stability, testing, and reproducibility of recipes)
- **Safety limits retained**: parse-step budget, depth limit, arg cap (already in `zw-expr`)

This list is the **definition of done for "compute without code"**: if a compute need cannot be expressed after the extension, it is either (a) pushed to a webhook/external call, or (b) explicitly listed as a "WASM-phase" candidate and tracked in `docs/compute-gaps.md` (created when the first such gap appears).

### 5.3 Deferred — WASM phase (future)

Reserved design seams so WASM can be added later without rework:
- `$wasm_call` recipe action placeholder (documented, not implemented)
- A `FunctionRuntime` trait in `engine/automation.rs` (one method per compute entry point), with the phase-1 implementation being the **expression engine** and a future implementation being a WASM host
- No user code paths exist yet, so there is no sandbox, no memory limits, no host ABI to design now — the trait isolates the seam only

---

## 6. Storage Abstraction (recap — details in gap-analysis doc §4)

Two traits in `engine/storage/`, implemented in `server`:

- **`Database`** — pooled connections, transactions + savepoints, insert/get/update/delete/query/aggregate/search/upsert, plus a **capability matrix** (`fts {ngram, bm25, phrase}`, `vector`, `savepoints`, `replication`, `jsonb`). The engine compiles its query IR into each backend's dialect.
- **`ObjectStore`** — put/get/head/delete/list/presign/copy + capabilities (`presign`, `multipart`, `versioning`).

Both traits default to in-memory implementations in `engine` for tests/embedded use.

---

## 7. Implementation Phases

### Phase 0 — Foundation (engine skeleton + traits) ✅ gate: `engine` tests green, no external deps
1. Scaffold workspace, `engine` crate, folder structure above.
2. Port the **query IR** (`FilterCond`, `Op`, `Agg`, `SrvFilter`, `parse_filter`) and its semantics.
3. Define `Database` + `ObjectStore` traits + capability matrices.
4. In-memory adapters (records in a `DashMap`, FTS as a simple token matcher, blobs in memory).
5. Port the **expression engine** (`zw-expr`) with the §5.2 extensions.
6. Port `model` types (`Board`, `Record`, …).
7. **Test gate:** port `zw-db/tests/` filter/CRUD/expression tests to run against the in-memory adapter.

### Phase 1 — Core data plane (crud + query + schema)
1. `crud.rs`: insert/get/set/patch/delete/upsert/bulk via `Database` (patch ops incl. `$set/$inc/$dec/$mul/$unset`).
2. `query.rs`: filter→backend SQL, search (capability-gated), aggregate, sort, cursors.
3. `schema.rs`: JSON Schema validation, computed (with DAG), validate rules, redact.
4. Unique keys + upsert; TTL (read-time filter + sweep); links/join (single-level).
5. **Test gate:** port `zw-db/tests/serverless.rs`, `filters.rs`, `logic.rs`, `fts.rs`.

### Phase 2 — Server: Turso adapter + transports + keys
1. `server/db/turso.rs` (use the existing `../turso/bindings/rust` dependency).
2. `server/store/fs.rs` (files on disk, replacing today's `…/files/*.bin`).
3. `server/transport/rest.rs` — mirror the current `/api/srv/{board_id}/*` surface exactly.
4. `auth.rs` + `server/identity/keys.rs` — issue/hash/scope (fix: salted hashes, key prefix, expiry).
5. Rate limiting behind a `RateStore` trait with an in-memory impl.
6. **Test gate:** port `ch-web-ui/tests/cors_preflight.rs` + HTTP-level tests; full parity with the reference REST surface.

### Phase 3 — Automation & realtime (async, outbox)
1. Port `zw-recipe` engine + actions + `zw-db/recipes.rs`.
2. **Outbox:** every write appends to an outbox in the same transaction; a durable dispatcher (not the request handler) runs recipes/hooks/realtime.
3. `JobBroker` trait + in-process adapter; lease-based scheduler (multi-instance safe).
4. Webhooks (out: HMAC + backoff/DLQ; in: HMAC verify) + `calls.rs` (SSRF with DNS resolution).
5. Secrets via an encrypted-store interface (AES-GCM in-memory impl; KMS later).
6. Realtime: `EventStream` publish trait + per-board topics; in-proc pubsub adapter.
7. Audit (immutable log) + files (blob-ref + ObjectStore orchestration).
8. **Test gate:** port `zw-recipe/tests/recipes.rs`; add outbox/dispatcher tests.

### Phase 4 — MCP + CLI (the tooling layer)

> **Full interface spec:** see `cli-mcp-interface-design.md` in this folder. It defines the `CommandSpec` registry (single source of truth), the four help layers, the structured error contract, the "never-guess" checklist, the daemon/CLI/MCP-tools/studio shape, and the golden tests. **Phase 4 below implements that spec.**

**Architecture (from the spec):**
- A **daemon** hosts the engine + MCP server over **HTTP** (streamable HTTP, JSON-RPC 2.0). HTTP is the only production wire protocol.
- **`srv` CLI (★ MAIN, full implementation)** — a thin **MCP client over HTTP** that turns argv into `tools/call` JSON-RPC requests. This is the flagship surface, built to zerowrapper's proven layered-help pattern and formalized so an agent can never guess: `about`, `--help`/`help` index, `help <group>`, `help <group> <verb>` (flags/JSON/response/example), `help json <topic>`, `help search`, plus `--dry-run`/`--yes` on destructive verbs and `did_you_mean` on any unknown token.
- **Granular MCP agent tools (PARTIAL)** — a curated subset (`apps.create/list/show/delete`, `records.submit/get/list/query/search/aggregate/patch/delete`, `recipes.list/add`, `files.list`) generated from the same registry, one tool per feature, to prove the pattern. The full ~50-tool set is generated later with no design change.
- **`srv studio` REPL (PARTIAL)** — a readline stub (prompt, history, help, run commands) to show the interactive pattern; enhancement deferred.
- All three surfaces are **generated from one `CommandSpec` registry**, so help ↔ CLI parser ↔ MCP schemas can never drift (golden tests enforce it).

**Tasks:**
1. Registry module + `CommandSpec` model in `engine` (declarative data; generator inputs).
2. Help generator: Layers 0–3 + `help json …` topics + `help search`.
3. **CLI client (MAIN):** HTTP MCP client, argv→tool mapping, rendering (human/JSON), auth (`--key`/env/`~/.config/serverless`), optional auto-spawn of a local daemon.
4. Daemon HTTP MCP transport + auth (Bearer → `Principal`).
5. Granular MCP tools — curated subset (partial; full set later).
6. `srv studio` — REPL stub (partial; enhanced later).
7. Golden tests: `srv <verb> --help` == generated spec; parser accepts exactly the documented grammar; MCP `tools/list` schema == same spec; every documented example round-trips.
8. `reference` (OpenAPI-style) generation + `help search`.

**Test gate:** golden tests green; a cold-start agent (zero prior knowledge) can discover and run every curated verb using help alone; `srv <anything> --help` never errors.

### Phase 5 — Ops hardening (post-MVP)
1. Redis broker/pubsub + distributed rate limiter.
2. Postgres adapter (validates the `Database` trait's generality).
3. S3/R2/GCS object-store adapters + signed URLs.
4. Observability: OTel metrics/traces, correlation IDs.
5. Quotas/billing meters consuming usage events.
6. Vector search (Turso `vector*` funcs) + hybrid ranking.
7. WASM runtime phase (deferred, behind `FunctionRuntime` trait).

---

## 8. Testing Strategy

- **Contract tests** (in `engine`): all behavior tested against the in-memory adapters only. These become the acceptance tests for every backend.
- **Backend conformance suite** (in `server`): the same contract suite run against Turso, then Postgres, asserting the capability matrix (skip unsupported features).
- **Parity suite:** port the existing `zw-db/tests/*` and `zw-recipe/tests/*` unchanged; they must pass as-is against the new engine via the Turso adapter.
- **HTTP parity:** the current REST surface is the spec; a diff-tool compares responses (modulo auth hashing changes).
- **Property/fuzz:** query IR → executor round-trips; expression fuzzer (never panics).
- **Bench gates:** CRUD/search/recipe benchmarks with regression thresholds.

---

## 9. Milestones & Definition of Done

| Milestone | Deliverable | Done when |
|---|---|---|
| **M0** | Repo scaffold + traits + in-memory engine | `cargo test` green on `engine` with zero external deps; IR + expr tests ported |
| **M1** | Full data plane on in-memory | All ported `zw-db` behavior tests pass on memory adapter |
| **M2** | Turso adapter + REST server | Current `/api/srv/*` surface fully replicated; parity diff clean |
| **M3** | Automation + outbox + realtime | Recipes/tests green; recipe latency decoupled from requests |
| **M4** | Tooling layer shipped | `srv` CLI (MCP client over HTTP) works against the daemon; MCP granular tools + `srv studio` partials generated from the shared `CommandSpec` registry; golden help/parser/schema tests green; cold-start-agent discovery pass |
| **M5** | Multi-backend + ops | Postgres + S3 adapters pass conformance; observability wired |

---

## 10. Risks & Mitigations

| Risk | Mitigation |
|---|---|
| SQL-string ports diverge across backends | Capability matrix + conformance suite; backend dialect kept in `server/db/*`, never in `engine` |
| Recipe parity drift | Port tests verbatim first, then extend; outbox is additive |
| Compute gaps "leak" into WASM early | §5.2 expression extension list is the gate; external-call escape hatch; `docs/compute-gaps.md` tracker |
| Realtime durability scope creep | Phase 3 ships in-proc topics; durable subs tracked separately |
| Dependency on vendored `turso` | `Database` trait isolates it; Postgres adapter in M5 proves independence |

---

## 11. Immediate Next Steps (this session’s output, done)

- [x] Created `serverless-engine/` folder + `serverless-engine/docs/`
- [x] Copied feature & gap-analysis docs into the repo
- [x] Initialized an independent git repo inside `serverless-engine/` (parent repo unaffected)
- [x] Authored `cli-mcp-interface-design.md` (help layers, error contract, registry) and wired Phase 4 to it
- [ ] Write `README.md` (short mission + layout pointer)
- [ ] Create `Cargo.toml` workspace skeleton + `.gitignore` (when implementation begins)
