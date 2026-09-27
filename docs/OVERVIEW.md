# backend-engine — project overview

## What this is

**A backend you never code.** Deploy `backend-engine` to Cloudflare Workers and you get
a complete backend — tables, records, auth keys, file storage, scheduled jobs, event
recipes, webhooks — driven entirely by **JSON config**. No backend code to write, no server
to run, no database to provision. Your frontend (or your AI agent) talks to the Worker's
HTTP API; behavior is declared in JSON.

## Who it is for (in order)

1. **AI agents first.** Every capability is a documented HTTP verb with a machine-readable
   shape. An agent with zero prior knowledge can discover the API and run a whole backend:
   create tables, set validation, wire automations, schedule jobs.
2. **JavaScript second.** The same API serves frontends directly — no BFF layer needed.

## Where it came from

This engine was converted from a **multi-tenant standalone serverless engine** (Rust,
own daemon, own database drivers) into a **Workers serverless engine**: one Worker binary
= one tenant. The engine core (`crates/engine`) is pure, sync Rust with no runtime
dependencies — no tokio, no sockets, no threads — so it compiles to `wasm32-unknown-unknown`
and runs anywhere WebAssembly runs. Only thin adapters (D1, R2, Durable Objects) touch
Cloudflare APIs.

That runtime-agnostic core is the point: the engine does not depend on V8 isolates or any
single host API. Workers is the first host, not the only possible one. We are looking
forward to collaborating with Cloudflare to take this further — a JSON-configured,
agent-operated backend as a native edge primitive.

## How it works

```
frontend / AI agent
      │  JSON over HTTP (no SDK required)
      ▼
Cloudflare Worker (this repo)
      │  pure function calls into crates/engine
      ├─► D1 binding ......... tables, records, jobs, recipes, keys
      ├─► R2 binding ......... files + static assets
      └─► Durable Object ..... counters, rate state, realtime, alarms
```

Behavior-as-config: JSON Schema per table, computed fields, validation rules, redaction,
recipes (`when`/`match`/`actions`: `$compute`, `$set`, `$upsert_other`, `$call`, …),
cron jobs, webhook subscriptions, secrets, TTL. See `docs/FEATURES.md` for the full list
and `WORKER-PORT-GUIDE.md` for the port map.

## Status

Port complete, pending live verification. Engine core: single-tenant, async,
tested. Worker shell (`crates/worker`): D1/R2 adapters, full fetch router,
Cron + Queue pipeline, `TenantDO` — all compiling warning-free to wasm.
CI builds the deploy binary; nothing is hand-uploaded. Live test matrix:
`PORT-TRACK.md` Phase 8. Remaining follow-up: realtime WS/SSE fan-out.
