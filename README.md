# ⚡ backend-engine — your backend that you never code

> **Backend as a dependency.** Only focus on creativity, your features, and your
> frontend — the backend is already there. You configure it **while it runs**
> (JSON over HTTP, or one MCP tool your agent talks to) and go. Zero backend
> code. One Cloudflare Worker = one app.

`backend-engine` turns one Cloudflare Worker into a complete backend: database tables,
auth keys, file storage, automations, cron jobs, webhooks. You don't write routes,
controllers, or migrations — you declare **tables + rules + recipes as JSON**, and the
engine serves your frontend (or your AI agent) over HTTP.

```json
// 1. Declare a table (this JSON *is* your backend code)
POST /tables
{ "table": "orders",
  "schema": { "type": "object", "required": ["total"],
               "properties": { "total": { "type": "number" }, "status": { "type": "string" } } },
  "unique_key": "$.id" }

// 2. Declare automation: on every new order, notify the warehouse
POST /recipes
{ "name": "order_created",
  "when": { "event": "record.created", "table": "orders" },
  "actions": [{ "$call": { "url": "https://warehouse.example.com/in", "method": "POST" } },
              { "$set": { "$.status": "sent" } }] }

// 3. Your frontend just does CRUD — validation, automation, schedules all happen inside
POST /tables/orders/records
{ "id": "o-1", "total": 42 }
```

## Why agents first?

Every capability is a plain HTTP verb with a predictable JSON shape — no SDK, no codegen,
no tribal knowledge. An AI agent can read the route list and operate the entire backend:
provision tables, set validation, wire cross-table sync, schedule jobs, rotate keys.
JavaScript frontends get the same API with `fetch`. JS second, but first-class.

Agents get a third door: **one MCP tool** (`manage_serverless_engine`) that speaks CLI —
`records submit notes '{"body":"hi"}'`, with `--help` at every level. No MCP client?
The same commands run over plain HTTP: `GET /mcp?command=records+list+notes`
or `POST /mcp {"command":"..."}`. One grammar, three doors, zero setup for the URL one.

Your agent configures the backend **while it runs** — point it at `/mcp` and go:

- **Terminal**: `curl "$BASE/mcp?command=--help"` (add `?key=$WORKER_KEY` or the
  `Authorization` header on locked deployments)
- **Claude**: `claude mcp add --transport http backend-engine https://<app>/mcp`
- **Codex** (`~/.codex/config.toml`): `[mcp_servers.backend-engine]` + `url = "https://<app>/mcp"`
- **OpenCode** (`opencode.json`): `"mcp": {"backend-engine": {"type": "remote", "url": "https://<app>/mcp"}}`
- **Anything standard**: `POST /mcp` JSON-RPC `tools/call`
  (`manage_serverless_engine`, `arguments: {"command": "..."}`)

Tip: `GET /mcp` returns the exact `mcpServers` snippet for your deployment —
copy, paste, done. Full client matrix ships in the npm package README.

## Where this came from

This engine was converted from a **multi-tenant standalone serverless engine** — a Rust
daemon with its own database drivers — into a **Workers serverless engine**: one Worker
binary = one tenant = one app. The core (`crates/engine`) is pure, sync Rust: no tokio,
no sockets, no threads, no V8-isolate-specific code. It compiles to
`wasm32-unknown-unknown` and runs anywhere WebAssembly runs; only thin adapters (D1, R2,
Durable Objects) touch host APIs.

Because the core is runtime-agnostic, Workers is the first host, not the last. **We want
to collaborate with Cloudflare** to push this further: a JSON-configured, agent-operated
backend as a native edge primitive — backends that agents can provision the way they
provision infrastructure today.

## What's inside

| You declare (JSON) | Engine does |
|---|---|
| Tables + JSON Schema | Validation on every write |
| Computed fields, validate rules, redact paths | Derived data, rejected bad writes, masked reads |
| Recipes (`when`/`match`/`actions`) | Event automation: `$compute`, `$set`, `$copy`, `$upsert_other`, `$call`, … |
| Cron jobs | Scheduled work via Workers Cron Triggers |
| Keys, users, sessions | Scoped API auth without a login server |
| Files + assets | Blob storage on R2 |
| Webhooks in/out | HMAC-signed delivery with retries |
| TTL, links, audit | Expiry, relations, immutable log |

Full inventory: [`docs/FEATURES.md`](docs/FEATURES.md). Big picture: [`docs/OVERVIEW.md`](docs/OVERVIEW.md).

## Run it

```sh
# one-time infra (ids go into wrangler.toml)
wrangler d1 create backend-engine
wrangler r2 bucket create backend-engine-store
wrangler queues create webhook-deliveries

# local edge dev
cp .env.example .dev.vars   # fill WORKER_KEY, SECRET_KEY — never commit
wrangler dev                # then exercise the Phase 8 matrix in PORT-TRACK.md

# ship it — binary always comes from CI, secrets from env, never files
export CLOUDFLARE_API_TOKEN=... SECRET_KEY=... WORKER_KEY=...
scripts/sync-worker.sh
```

No ports. No database URLs. Storage is bindings (`DB`, `STORE`, `TENANT_DO` in
`wrangler.toml`), not connection strings — see `WORKER-PORT-GUIDE.md` §6.

## Status & roadmap

- ✅ Engine core: single-tenant, async, WASM-clean (no tokio/bcrypt/jsonschema)
- ✅ Contract tests: 14 engine flows + MCP agent-surface smoke (`cargo test` in CI)
- ✅ `crates/worker`: D1/R2 adapters, full fetch router (~60 routes), Cron +
  Queue webhook pipeline, `TenantDO` (usage/rate/sweep)
- ✅ CI builds the deploy binary (`worker-dist/` artifact) + forbidden-crate
  gates + wasm size gate
- 🔨 Next: live verification — `wrangler dev`, D1/R2 provisioning, deploy
  (see `PORT-TRACK.md` Phase 8 test matrix)
- 🔜 Realtime WS/SSE fan-out behind `TenantDO` (`GET /api/events` is 501),
  Analytics-Engine usage pipeline, multipart upload

Port map for contributors and agents: [`WORKER-PORT-GUIDE.md`](WORKER-PORT-GUIDE.md).
Execution tracker: [`PORT-TRACK.md`](PORT-TRACK.md).
