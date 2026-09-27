# Demo — backend as a dependency (+ React frontend)

This folder is a complete, working example: the **entire backend** is the
`@pilser/backend-engine` npm package. There is no server code here —
just config. `node_modules/` is git-ignored; everything else is tracked so
you can copy this pattern.

Part of the repo-root **bun workspace** (`backend-engine-demo`):
`demo-example` (this backend) + `web` (React frontend). One install,
one build, frontend included:

```sh
bun install          # workspace root: backend + frontend deps, one lockfile
```

**Two servers in dev, one in production.** `bun run dev:engine` starts the
engine (`wrangler dev :8788` — the API plus the uploaded SPA at `/srv/`).
`bun run dev:web` starts Vite (`:5173` — the React source, proxying
`/api /mcp /srv` to the engine). Code the UI at `:5173` against live data;
what you see there is what the engine serves after deploy.

**Build is not deploy.** `bun run build` only compiles the frontend
(`web/dist/`, git-ignored). `bun run deploy` builds AND copies the dist
into the engine (`scripts/publish-frontend.sh` PUTs every file to
`/api/assets/*`). Deploy reads config, it never guesses:

```sh
bun run deploy                                            # local defaults (dev key)
ENGINE_URL=https://<app>.workers.dev ENGINE_KEY="$WORKER_KEY" bun run deploy
```

The demo app tours almost the whole engine: Status (app/resources/health),
Tables (CRUD + query + patch), Files (R2 upload/download), Automate
(recipes + cron jobs + runs), Access (API keys + users + sessions),
Agent (raw `/mcp` command console — the same door MCP clients use).

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

Public pages need **no key**. The engine is private by default, but one
switch opens anonymous reads (writes always stay gated):

```sh
curl -X PATCH "localhost:8788/api/app?key=local-demo-key" -d '{"public_reads": true}'
```

Then just open the app — `/` redirects to the hosted SPA, no tokens:

- http://localhost:8788/ — the React demo (served from `/srv/`)
- http://localhost:8788/mcp — setup sheet: every route, live

Reads are public now (`/api/tables/notes/records` with no key works);
writes still need a key (`?key=` in the URL for browsers/terminal,
`Authorization: Bearer` header for code):

```sh
curl "localhost:8788/api/tables/notes/records"                          # open
curl -X POST "localhost:8788/api/tables/notes/submit?key=local-demo-key" -d '{"body":"hi"}'
```

The demo page bakes the local key (`web/.env.development`) so its form
works out of the box. Production: `wrangler secret put SECRET_KEY` +
`wrangler secret put WORKER_KEY`, flip `public_reads` the same way, and
give real users scoped keys — never bake the admin key into a public
bundle.

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
