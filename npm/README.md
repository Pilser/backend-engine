# @pilser/serverless-worker

A complete backend as **one Cloudflare Worker = one app**: database tables,
auth keys + users, file storage, cron jobs, event recipes, webhooks, static
hosting, MCP control plane. You declare behavior in JSON over HTTP — there is
no backend code to write.

## Install

```sh
npm i @pilser/serverless-worker        # bun add @pilser/serverless-worker
```

Copy `wrangler.example.toml` (in this package) to `wrangler.toml` in your
project and fill in your ids:

```toml
name = "my-app"
main = "node_modules/@pilser/serverless-worker/index.js"
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

Full surface: `GET /mcp` on your deployment, or the `endpoints.list` MCP tool.
Source + guides: https://github.com/Pilser/serverless-worker
