# Build And Deploy

How to build and deploy each repo in this workspace when changes are made.
This is the reference for future agents — follow it exactly, in order.

Workspace root: `/home/vox/.AAAPROJECTS/Whatsapp4Agents/serverless-engine`

Repos / components:

1. **Engine + daemon** (`crates/engine`, `crates/server`, `crates/cli`, `crates/mcp`) — the
   serverless engine, daemon binary `srv`, MCP endpoint.
2. **Zerowrapper (zw / Jerboa chat)** (`zerowrapper/`) — the AI chat frontend+backend
   embedded in the SPA via the daemon's AI proxy. Deployed as the `zw-jerboa` Docker container.
3. **IICO SPA** (`deploy/iico/`) — the main school management frontend. Served by the daemon
   from the board tenant's object store (NOT from disk).
4. **HelixDB + MinIO** (`ghcr.io/helixdb/helixdb` + `minio/minio` Docker containers) — the
   `helix` storage backend the daemon talks to.

---

## 0. The running stack (what should be up)

| Component | How to check | Port |
|-----------|--------------|------|
| HelixDB container | `docker ps` → `helix-serverless-engine-dev` | 7979 |
| MinIO sidecar | `docker ps` → `helix-serverless-engine-dev-minio` | — |
| zw-jerboa container | `docker ps` → `zw-jerboa` | 9033 |
| zeroclaw daemon | `ps aux \| grep zeroclaw` | socket |
| Engine daemon (srv) | `curl http://127.0.0.1:7070/healthz` → `{"ok":true}` | 7070 |
| MCP endpoint | `POST http://127.0.0.1:7070/mcp` (tools/list) | 7070/mcp |

**Known failure mode:** if HelixDB queries hang/close with "Empty reply", the MinIO sidecar
exited. Fix: `docker start helix-serverless-engine-dev-minio`. HelixDB recovers on its own
(compaction worker restarts).

---

## 1. Engine / daemon (`crates/*`, binary `srv`)

### Build

```bash
cd /home/vox/.AAAPROJECTS/Whatsapp4Agents/serverless-engine
cargo build --release        # from workspace root, NO -p flags, NO cargo test, NO clean
```

### Start / restart the daemon (tmux)

```bash
tmux new-session -d -s srv -n daemon \
  'SRV_DB=helix SRV_HELIX_URL=http://127.0.0.1:7979 SRV_PORT=7070 SRV_DATA_DIR=deploy/helix-iico-data ./target/release/srv daemon start 2>&1 | tee /tmp/srv-daemon.log'
```

- Kill + restart if the binary changed: `tmux kill-session -t srv`, then the above.
- Verify: `curl http://127.0.0.1:7070/healthz` and `curl -X POST http://127.0.0.1:7070/mcp -d '{"jsonrpc":"2.0","id":1,"method":"tools/list"}'`
- The CLI `./target/release/srv <group> <verb>` is an MCP client over HTTP — it talks to the
  daemon, so the daemon must be running.

### Config env (`.env.example` at repo root)

`SRV_HOST SRV_PORT SRV_DATA_DIR SRV_DB SRV_HELIX_URL SRV_HTTP_TIMEOUT_MS SRV_SECRET_KEY`
(`SRV_BG_TTL/SRV_BG_JOBS/SRV_BG_HOOKS` turn on background loops, default off).

---

## 2. Zerowrapper / Jerboa chat (`zerowrapper/`)

The chat frontend is Rust (`zw` binary, `crates/ch-web-ui`) + JS bundled by bun into
`assets/js/chat/app.js`, then shipped as a topcoat asset bundle. The container
`zw-jerboa` bind-mounts `zerowrapper/deploy/DEPLOY-ALHEIB-AI/{zw,assets}` read-only.

Full guide: `zerowrapper/deploy/DEPLOY-ALHEIB-AI/build-deploy.md`. The steps below are the
canonical flow. **If you changed only Rust, skip step 1.**

### 1. Bundle frontend JS (bun) — only if you changed JS/CSS

```bash
cd /home/vox/.AAAPROJECTS/Whatsapp4Agents/serverless-engine/zerowrapper/crates/ch-web-ui/assets/js/chat
bun build app-entry.js \
  --format esm \
  --external marked \
  --external highlight.js \
  --external katex \
  --external mermaid \
  --outfile app.js
```

CDN libs stay external via the importmap. `app.js` is what the server serves.

### 2. Build the release binary (cargo) — from the zerowrapper workspace

```bash
cd /home/vox/.AAAPROJECTS/Whatsapp4Agents/serverless-engine/zerowrapper
cargo build --release
```

(First build ~9-10 min; incremental ~3-4 min. `zw` lands in `zerowrapper/target/release/zw`.)

### 3. Bundle assets into the Jerboa deploy folder (topcoat)

```bash
cd /home/vox/.AAAPROJECTS/Whatsapp4Agents/serverless-engine/zerowrapper
topcoat asset bundle --bin zw --release --out deploy/DEPLOY-ALHEIB-AI/assets
```

Writes hashed files (`app-<hash>.js`, `manifest.toml`, ...) into
`DEPLOY-ALHEIB-AI/assets/`, mounted as `/app/bundle` in the container.

**CRITICAL:** profile must match the deployed binary (`--release`). A debug-profile bundle
has different AssetIds and the container panics with `failed to resolve asset`.

### 4. Copy the binary (atomic, safe while container runs)

```bash
cd /home/vox/.AAAPROJECTS/Whatsapp4Agents/serverless-engine/zerowrapper
cp target/release/zw deploy/DEPLOY-ALHEIB-AI/zw.new
mv -f deploy/DEPLOY-ALHEIB-AI/zw.new deploy/DEPLOY-ALHEIB-AI/zw
```

### 5. Recreate the container

```bash
cd /home/vox/.AAAPROJECTS/Whatsapp4Agents/serverless-engine/zerowrapper/deploy/DEPLOY-ALHEIB-AI
docker compose -f compose.yml --env-file jerboa.env up -d --force-recreate
```

(Project name comes from `INSTANCE_NAME=jerboa` in the env file — no `-p` needed.)

### 6. Verify

```bash
curl -s -o /dev/null -w "%{http_code}\n" http://127.0.0.1:9033/        # 200
curl -s http://127.0.0.1:9033/ | grep -o 'app-[a-f0-9]*\.js' | head -1  # current hash
# through the daemon proxy (the SPA path):
curl -s "http://127.0.0.1:7070/srv/b_7z5lr8zaurkd0000/ai/" | grep -oE 'app-[a-f0-9]*\.js' | head -1
```

Note: frontend change shows old behavior until hard-refresh (assets are hash-versioned).

---

## 3. IICO SPA (`deploy/iico/`)

The SPA is a Vite + React app. **It is gitignored** (`deploy/iico` in .gitignore) — changes
are not tracked by git. Deployment means building it and pushing the dist files into the
engine board's object store (the daemon serves `/srv/<board>/assets/...` from there).

### Build + push (one command)

```bash
cd /home/vox/.AAAPROJECTS/Whatsapp4Agents/serverless-engine/deploy/iico
bash scripts/build-static.sh
```

What it does (see `scripts/build-static.sh`):

1. `VITE_ENGINE_BOARD=b_7z5lr8zaurkd0000 bunx vite build --outDir dist --emptyOutDir`
2. Clears stale assets from the board's `/assets/` (old chunk hashes).
3. PUTs every dist file to `$ENGINE_URL/api/srv/$BOARD/assets/<rel>` with
   `Authorization: Bearer $BOARD` (board id = owner token).
4. Pushes `index.html` first so a partial upload still serves the app.

Env overrides: `ENGINE_URL` (default `http://127.0.0.1:7070`), `BOARD`
(default `b_7z5lr8zaurkd0000`), `TOKEN` (default = board id).

### Verify

```bash
BOARD=b_7z5lr8zaurkd0000
curl -s "http://127.0.0.1:7070/srv/$BOARD/" | grep -oE 'assets/index-[a-zA-Z0-9_-]*\.js' | head -1
# fetch that chunk + any lazy chunks, grep for your change
curl -s "http://127.0.0.1:7070/srv/$BOARD/assets/DashboardLayout-<hash>.js" | grep 'your-symbol'
```

Lazy-loaded components live in their own chunks (e.g. the AI button is in
`DashboardLayout-*.js`, not the index bundle). The served files physically live in
`deploy/helix-iico-data/objects/<board>/assets/...` — that IS the daemon's tenant store.

### TypeScript sanity check (before deploying)

```bash
cd deploy/iico && npx tsc --noEmit
```

---

## 4. HelixDB + MinIO (storage backend)

Both are Docker containers, started outside this repo's compose (managed manually).

| Container | Image | Port |
|-----------|-------|------|
| `helix-serverless-engine-dev` | `ghcr.io/helixdb/helixdb:v0.0.4` | 7979→8080 |
| `helix-serverless-engine-dev-minio` | `minio/minio:latest` | — (sidecar) |

```bash
docker start helix-serverless-engine-dev-minio   # if HelixDB queries hang
docker start helix-serverless-engine-dev          # the DB itself
docker logs helix-serverless-engine-dev           # SlateDB/object-store retries visible here
```

HelixDB stores data in MinIO (`http://helix-serverless-engine-dev-minio:9000/helix-db`).
If MinIO is down, every `/v2/query` times out and the daemon reports
`http error: error sending request for url (http://127.0.0.1:7979/v2/query)`.

---

## Quick reference — "I changed X, what do I run?"

| I changed... | Run |
|--------------|-----|
| Rust in `crates/*` (engine/daemon) | `cargo build --release` (root) → restart tmux daemon |
| JS in `zerowrapper/crates/ch-web-ui/assets/js/chat/` | steps 1→5 of §2 |
| Rust in `zerowrapper/` | steps 2→5 of §2 |
| Anything in `deploy/iico/src/` | `cd deploy/iico && bash scripts/build-static.sh` (§3) |
| HelixDB/MinIO down | §4 `docker start` |
