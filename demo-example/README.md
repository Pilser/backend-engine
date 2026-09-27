# Demo — backend as a dependency

This folder is a complete, working example: the **entire backend** is the
`@pilser/backend-engine` npm package. There is no server code here —
just config. `node_modules/` is git-ignored; everything else is tracked so
you can copy this pattern.

## Setup (5 minutes)

```sh
cd demo-example
npm install                          # 1. backend arrives as a dependency

wrangler login                       # 2. Cloudflare account (free tier OK)

wrangler d1 create demo-example              # 3. storage — paste ids into wrangler.toml
wrangler r2 bucket create demo-example-store
wrangler queues create demo-example-deliveries

wrangler secret put SECRET_KEY       # 4. secrets (32+ random bytes)
wrangler secret put WORKER_KEY       #    admin bearer for /api + /mcp

npm run deploy                       # 5. live backend
```

<details>
<summary>Bun instead of npm</summary>

```sh
cd demo-example
bun add @pilser/backend-engine    # 1. same package, bun's registry path

bunx wrangler login                  # 2-4. identical from here on:
bunx wrangler d1 create demo-example         #    create storage, paste ids
bunx wrangler r2 bucket create demo-example-store
bunx wrangler queues create demo-example-deliveries
bunx wrangler secret put SECRET_KEY
bunx wrangler secret put WORKER_KEY

bun run deploy                       # 5. bun runs the same deploy script
bunx wrangler dev                    # local dev, if you prefer
```

Only the tooling prefix changes (`bunx` vs `npx`); `wrangler.toml`
points at the same `node_modules/@pilser/backend-engine/index.js`
either way (bun installs npm packages into `node_modules` too).
</details>

## Run locally, test in your browser (no headers needed)

```sh
cd demo-example
npm install
npx wrangler dev --port 8788        # or: bunx wrangler dev --port 8788
```

Browsers can't send `Authorization` headers by clicking, so the worker
also accepts `?key=` in the URL. Local `.dev.vars` (git-ignored) ships
the key `local-demo-key` — click these, no tokens to paste:

- http://localhost:8788/mcp — setup sheet: every route, live
- http://localhost:8788/api/tables?key=local-demo-key — list tables

Write + read from the terminal (same key, still no headers):

```sh
curl -X POST "localhost:8788/api/tables?key=local-demo-key" -d '{"table":"notes"}'
curl -X POST "localhost:8788/api/tables/notes/submit?key=local-demo-key" -d '{"body":"hi"}'
curl "localhost:8788/api/tables/notes/records?key=local-demo-key"
```

Production is locked down the same way: `wrangler secret put SECRET_KEY`
+ `wrangler secret put WORKER_KEY`, then every `/api` call needs
`Authorization: Bearer $WORKER_KEY` (or `?key=`).

## Configure it while it runs — the backend is already there

No code, no redeploy to change behavior. The same CLI grammar works in
your terminal right now (replace the URL with your deployment):

```sh
curl 'http://localhost:8788/mcp?command=--help'
curl 'http://localhost:8788/mcp?command=records+list+notes'
curl -X POST http://localhost:8788/mcp -d '{"command":"records list notes"}'
```

Point any MCP client at `http://localhost:8788/mcp` (or your deployed
`/mcp`) and your agent configures the backend by talking to it — one tool,
`manage_serverless_engine`. Client matrix (Claude, Codex, OpenCode,
standard JSON) is in the package README: `node_modules/@pilser/backend-engine/README.md`.

## First calls (no backend code written)

```sh
BASE=https://demo-example.<account>.workers.dev
K="Authorization: Bearer $WORKER_KEY"

curl $BASE/mcp                                              # setup sheet
curl -H "$K" -X POST $BASE/api/tables -d '{"table":"notes"}'
curl -H "$K" -X POST $BASE/api/tables/notes/submit -d '{"body":"hi"}'
curl -H "$K" $BASE/api/tables/notes/records
```

Tables, auth, files, cron, automations, webhooks — all JSON over HTTP.
Full surface: `GET /mcp` on your deployment.
