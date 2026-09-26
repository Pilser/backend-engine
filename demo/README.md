# Demo — backend as a dependency

This folder is a complete, working example: the **entire backend** is the
`@pilser/serverless-worker` npm package. There is no server code here —
just config. `node_modules/` is git-ignored; everything else is tracked so
you can copy this pattern.

## Setup (5 minutes)

```sh
cd demo
npm install                          # 1. backend arrives as a dependency

wrangler login                       # 2. Cloudflare account (free tier OK)

wrangler d1 create demo              # 3. storage — paste ids into wrangler.toml
wrangler r2 bucket create demo-store
wrangler queues create demo-deliveries

wrangler secret put SECRET_KEY       # 4. secrets (32+ random bytes)
wrangler secret put WORKER_KEY       #    admin bearer for /api + /mcp

npm run deploy                       # 5. live backend
```

## First calls (no backend code written)

```sh
BASE=https://demo.<account>.workers.dev
K="Authorization: Bearer $WORKER_KEY"

curl $BASE/mcp                                              # setup sheet
curl -H "$K" -X POST $BASE/api/tables -d '{"table":"notes"}'
curl -H "$K" -X POST $BASE/api/tables/notes/submit -d '{"body":"hi"}'
curl -H "$K" $BASE/api/tables/notes/records
```

Tables, auth, files, cron, automations, webhooks — all JSON over HTTP.
Full surface: `GET /mcp` on your deployment.
