# ⚡ serverless-worker — the backend you never code

> **Ship a full backend with JSON config. Zero backend code. Built for AI agents, friendly to JavaScript, running on Cloudflare Workers.**

`serverless-worker` turns one Cloudflare Worker into a complete backend: database tables,
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
# local edge dev (once crates/worker lands)
cp .env.example .dev.vars   # fill WORKER_KEY, SECRET_KEY — never commit
wrangler dev

# ship it — binary always comes from CI, secrets from env, never files
export CLOUDFLARE_API_TOKEN=... SECRET_KEY=... WORKER_KEY=...
scripts/sync-worker.sh
```

No ports. No database URLs. Storage is bindings (`DB`, `STORE`, `TENANT_DO` in
`wrangler.toml`), not connection strings — see `WORKER-PORT-GUIDE.md` §6.

## Status & roadmap

- ✅ Engine core ported, tested, WASM-clean (`cargo check -p engine --target wasm32-unknown-unknown`)
- ✅ CI builds the deploy binary (`worker-dist/` artifact) + forbidden-crate gate
- 🔨 Next: `crates/worker` — D1/R2 adapters, fetch router, scheduled triggers, TenantDO
- 🔜 Agent discovery pack (machine-readable route index), usage metering per tenant

Port map for contributors and agents: [`WORKER-PORT-GUIDE.md`](WORKER-PORT-GUIDE.md).
