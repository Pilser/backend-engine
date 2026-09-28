# @pilser/backend-engine

**Your backend that you never code (Backend as a dependency).** One install
gives your app a complete serverless backend engine: database tables, auth
keys + users, file storage, cron jobs, event automations (recipes), webhooks,
static hosting, AI proxy, MCP control plane. One Cloudflare Worker = one app.

**You don't write backend code.** No server files, no routes, no ORM, no
framework to learn. You declare behavior in JSON over HTTP — create a
table, submit a record, schedule a cron, wire an automation — all API
calls from your frontend or scripts. If you can `curl`, you can backend.

**Only focus on creativity, your features, and your frontend.** The backend
is already there — you configure it **while it runs** and go. Tables,
validation, automations, schedules are declared at runtime as JSON and take
effect immediately. No redeploy to change behavior.

**It's free.** MIT-licensed and runs on Cloudflare's free tier (Workers
+ D1 + R2). No per-seat pricing, no hosted-backend bill, no vendor
lock-in — your Cloudflare account, your data, your limits. Fork it,
self-host it, it's yours.

## Install

```sh
npm i @pilser/backend-engine        # bun add @pilser/backend-engine
```

Copy `wrangler.example.toml` (in this package) to `wrangler.toml` in your
project and fill in your ids:

```toml
name = "my-app"
main = "node_modules/@pilser/backend-engine/index.js"
compatibility_date = "2026-09-01"

[vars]
WORKER_URL = "https://my-app.<account>.workers.dev"

[[d1_databases]]
binding = "DB"
database_name = "my-app"
database_id = "<run: wrangler d1 create my-app>"

[[r2_buckets]]
binding = "STORE"
bucket_name = "my-app-store"            # wrangler r2 bucket create my-app-store

[[durable_objects.bindings]]
name = "TENANT_DO"
class_name = "TenantDO"

[[migrations]]
tag = "v1"
new_sqlite_classes = ["TenantDO"]

[[queues.producers]]
queue = "my-app-deliveries"            # wrangler queues create my-app-deliveries
binding = "WEBHOOKS"

[[queues.consumers]]
queue = "my-app-deliveries"
max_batch_size = 10
max_batch_timeout = 30

[triggers]
crons = ["*/5 * * * *"]
```

Secrets (never in files):

```sh
wrangler secret put SECRET_KEY   # 32+ random bytes: at-rest encryption
wrangler secret put WORKER_KEY   # admin bearer for /api + /mcp
wrangler deploy
```

First calls (`Authorization: Bearer $WORKER_KEY`):

```sh
curl $BASE/mcp                                    # setup sheet
curl -H "$K" -X POST $BASE/api/tables -d '{"table":"notes"}'
curl -H "$K" -X POST $BASE/api/tables/notes/submit -d '{"body":"hi"}'
```

## Configure at runtime — terminal + MCP clients

The backend is already running — configure it live. One tool
(`manage_serverless_engine`) speaks CLI; three doors lead to it.

**Terminal (zero setup)** — same grammar over plain HTTP:

```sh
curl "$BASE/mcp?command=--help"
curl "$BASE/mcp?command=records+list+notes"
curl -X POST $BASE/mcp -d '{"command":"records list notes"}'
```

Locked deployments add auth (`Authorization: Bearer $WORKER_KEY` header,
or `?key=$WORKER_KEY` in the URL — browsers can only do the latter).

**MCP clients** — point at `https://<your-app>/mcp` (it serves MCP
JSON-RPC). Easiest start: open `GET /mcp` in a browser — it returns the
exact `mcpServers` snippet for your deployment, auth included:

- **Claude**: `claude mcp add --transport http backend-engine https://<app>/mcp`
  (+ `--header "Authorization: Bearer $WORKER_KEY"` when locked), or
  `.mcp.json`: `{"mcpServers": {"backend-engine": {"url": "...", "headers": {...}}}}}`
- **Codex** (`~/.codex/config.toml`):
  ```toml
  [mcp_servers.backend-engine]
  url = "https://<app>/mcp"
  http_headers = { "Authorization" = "Bearer $WORKER_KEY" }  # when locked
  ```
- **OpenCode** (`opencode.json`):
  ```json
  {"mcp": {"backend-engine": {"type": "remote", "url": "https://<app>/mcp",
    "headers": {"Authorization": "Bearer $WORKER_KEY"}}}}
  ```
  (v2 config nests servers under `"mcp": {"servers": {...}}`)
- **Any standard client**: `POST /mcp` JSON-RPC `tools/call` with
  `{"name": "manage_serverless_engine", "arguments": {"command": "..."}}`.
  Start agents with command `--help`, then `<group> --help`.

Full route surface: `GET /mcp` on your deployment, or the `endpoints.list` MCP tool.
Source + guides: https://github.com/Pilser/backend-engine

## Files vs assets (two stores — don't mix them)

- **Files** (`POST /api/upload`, `GET /api/file`): record-backed blobs —
  user uploads, attachments, form files. Listed per table, validated,
  permission-checked like records.
- **Assets** (`PUT /api/assets/*`, `GET /api/assets/*`, served at `/srv/*`):
  front-end hosting — HTML/JS/CSS. Served with ETags + `304`s and edge
  cache (public apps); purged from cache on every put/delete.

They are different namespaces: `files delete x` never touches `/srv/x`.
Deploy front-ends with `PUT /api/assets/*` (or `scripts/publish-frontend.sh`
in the repo); manage uploads with the files verbs. The edge cache is the
Worker Cache API (`caches.default`, isolate-adjacent) driven by `s-maxage`
— there is no `cf-cache-status` header to watch; correctness comes from
exact-URL invalidation on write, not from edge observability.
