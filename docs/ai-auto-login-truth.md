# AI Auto-Login — Confirmed Truth (increment as we verify)

> Working doc. Every entry here is **verified via the `srv` CLI / MCP endpoint or
> by reading actual data** — not guessed. Update this file as we confirm more.

## How to check things (the tools)

- CLI (MCP client over HTTP): `./target/release/srv <group> <verb> [args]`
  from `/home/vox/.AAAPROJECTS/Whatsapp4Agents/serverless-engine`
- `srv --help` → all groups/verbs; `srv help <group> <verb>` → full spec
- `srv about` → orientation; `srv reference` → full command reference
- MCP endpoint: `POST http://127.0.0.1:7070/mcp` (JSON-RPC `tools/list` / `tools/call`)
- Daemon: `./target/release/srv daemon start`, env `SRV_DB=helix SRV_HELIX_URL=http://127.0.0.1:7979 SRV_PORT=7070 SRV_DATA_DIR=.../deploy/helix-iico-data`

## Confirmed facts

### The main SPA (deploy/iico/) — how it calls the API + where AI loads
- API client: `src/lib/engineClient.ts` — calls `/api/srv/<board>/auth/*`
  - `SRV_BASE` = `/api/srv/<board>` (same origin, derived from `/srv/<board>` path)
  - Session cached in `localStorage["alheib_engine_session"]` = `{token, jwt, user:{email, role, board_id, created_at}}`
  - `engineAuth.signIn(email, password)` → POST `/auth/login` → token → GET `/auth/me` → saves session
  - `engineAuth.me(token)` → GET `/auth/me`
  - `basePath()` → `/srv/<board>` (the mount prefix)
  - `loadSession()` / `saveSession()` in the same file
- **AI button**: `src/components/common/AIAssistantButton.tsx`
  - Opens a `Dialog` containing an **iframe** with `src = `${basePath()}/ai/``
  - iframe sandbox: `allow-scripts allow-same-origin allow-forms allow-popups allow-downloads`
- There's also `src/components/ai/AIAssistant.tsx` — a **fake/local** chat card (simulated responses, not the real one). The real AI is the iframe → `/srv/<board>/ai/` → daemon proxy → Jerboa chat.
- So the SPA already has: session (email+role) in localStorage, and the AI iframe URL. We add the profile lookup + pass params into the iframe URL.

### Auth model (verified via `srv auth *` help)
- Board: `b_7z5lr8zaurkd0000` — "Alheb Islamic Primary School - Management System" (Lovable SPA)
- Frontend source: `deploy/iico/` (served by the daemon at `/srv/b_7z5lr8zaurkd0000/`)
- AI chat (Jerboa, our zw build) is embedded **in a card** via `/srv/b_7z5lr8zaurkd0000/ai/` (daemon proxy → `AI_BASE_URL`)
- `AI_BASE_URL` secret = `http://127.0.0.1:9033` (the Jerboa zw container)
- The chat login is **serverless auth session** (the daemon's auth), not frontend-only

### Auth model (verified via `srv auth *` help)
- `auth signup <board> <email> <password> [--role r]` → `{ok, user:{email, role}}`
- `auth login <board> <email> <password>` → `{ok, token, jwt}`
- `auth me <board> <token>` → `{ok, user:{board_id, email, role, created_at}}`
- `users list <board>` → `{ok, users:[{board_id, email, role, created_at}]}`
- `auth set_role <board> <email> <role>` → changes one role; expires sessions
- **One role per user** in the auth API (not a list)

### Names + roles live in the `profiles` TABLE (verified by querying)
- `srv tables list b_7z5lr8zaurkd0000` → tables include `profiles`
- `srv records query b_7z5lr8zaurkd0000 profiles --filter '{"email":"student.382@sised.sc.ug"}'` returns:
  ```
  { "email": "student.382@sised.sc.ug", "full_name": "NAKIYUKA LATIFAH",
    "role": "student", "scope": "school", "account_status": "active", ... }
  ```
- So the **profiles** table holds `full_name` + `role` per email. Auth API has email+role only;
  names come from profiles.

### Records query (the read endpoint we'll use)
- `srv records query <board> <table> --filter <json>` — filter shapes:
  1. equality `{"class_id":"abc"}`
  2. op map `{"price":{"gte":500}}`
  3. array `[{"field":"$.x","op":"eq","value":1}]`
  4. `{"$and":[cond,...]}`
  5. `{"search":"term", ...}`
- Operators: `eq ne gt gte lt lte in contains search`
- REST: `GET /api/srv/<board>/tables/<table>/query?filter=<urlencoded-json>`
- Unfiltered query over big table is blocked → always filter

### Recipes = the trigger/action mechanism
- `srv recipes add <board> <name> --when '...' --actions '[...]'`
- Triggers: `record.created`, `record.updated`, `record.deleted`, `record.inbound`, `record.cron`
- Actions include `$call` (HTTP, with `{secret:name}` resolution), `$create_user`,
  `$resolve_other`, `$upsert_other`, `$compute`, `$set`, `$log`, etc.
- REST: `POST /api/srv/<board>/recipes`

### The AI chat side (zerowrapper zw / Jerboa) — how it loads + receives identity
- `auth.js`: `state = {email, sessionId}`, `buildSessionId(email)` = `web.{agent}.{email}`,
  storage keys `zerowrapper_email` / `zerowrapper_session`, `loadFromStorage()` reads them.
- `main.js init()`: on load, `loadFromStorage()`; if `auth.state.email` → `showBadge()` + `connection.connect()`,
  else → `showForm()` (email input). `handleLogin()` sets email + sessionId, saves, connects.
- `connection.js connect()`: WS URL derived from `<base href>` → `/ws` standalone, `/srv/<board>/ai/ws` proxied.
- So to auto-login from the SPA: pass `?email=...&name=...&role=...` on the iframe URL;
  `auth.js`/`main.js` reads them on load, sets `state.email`, builds sessionId, connects, stores name/role.

## What we're building (locked direction, not yet implemented)

1. Read `deploy/iico/` to find the AI button + login flow + where the session token is cached.
2. On login / AI-button click, SPA calls an endpoint that:
   - takes the session token → `auth me` → email
   - queries `profiles` by email → `full_name`, `role`
   - returns `{email, full_name, role}`
3. Pass those into the AI card URL (`/srv/{board}/ai/?email=...&name=...&role=...`).
4. zw chat (`connection.js`/`auth.js`) reads those params on load → auto-login/register
   (`web.jerboa.{email}`), caches name/role, uses name in the message envelope.

## Open questions (to verify, not guess)
- Where exactly is the AI button in `deploy/iico/` source? (NOT yet read)
- How is the session token cached in the SPA (localStorage key / context)? (NOT yet read)
- Do we need a new REST endpoint, or can the SPA call `records query` directly
  (it has public_reads=true)? Check what `public_reads` allows.

---

## Session 2026-08-19 — implemented + verified end to end

### Stack bring-up (what was down / how it was fixed)
- Daemon was NOT running. Started in tmux session `srv`:
  `SRV_DB=helix SRV_HELIX_URL=http://127.0.0.1:7979 SRV_PORT=7070 SRV_DATA_DIR=deploy/helix-iico-data ./target/release/srv daemon start`
- HelixDB container was up but its MinIO sidecar (`helix-serverless-engine-dev-minio`)
  had exited → every `/v2/query` hung and closed ("Empty reply"). Fix: `docker start helix-serverless-engine-dev-minio`.
- After MinIO restart HelixDB recovered (compaction worker started) and queries returned.
- MCP endpoint verified: `POST http://127.0.0.1:7070/mcp` → 42 tools.
- `.mcp.json` project scope already registered: `cmd mcp list` shows `serverless-engine` http → `http://127.0.0.1:7070/mcp`.

### SPA side — verified live (was written last session, now deployed)
- `AIAssistantButton.tsx` builds `${basePath()}/ai/?email=&role=&name=` from
  `loadSession()` (`alheib_engine_session` in localStorage) + `records query` on `profiles`.
- **`public_reads=true` confirmed** via `srv apps show`; anonymous
  `GET /api/srv/<board>/tables/profiles/query?filter={...}` returns 200 with
  `data.records[0].payload = {email, full_name, role, ...}` (the earlier "private board"
  403 was HelixDB being down, not a real access control issue).
- Rebuilt + redeployed SPA via `deploy/iico/scripts/build-static.sh`. AI button lives in
  the `DashboardLayout-*.js` chunk (lazy-loaded), not the index bundle.

### zw chat side — NEW code (receiving end of the handshake)
- `zerowrapper/crates/ch-web-ui/assets/js/chat/auth.js`:
  - state now carries `name` + `role` (persisted as `zerowrapper_name` / `zerowrapper_role`).
  - new `loadFromQuery()`: reads `?email=&name=&role=` from the iframe URL, builds
    `sessionId = web.jerboa.<email>`, saves to storage. Only applies when no stored session
    yet (a returning user keeps their own session).
  - `showBadge()` shows `name || email`, plus `role · email` in a new `badge-sub` span.
- `main.js`: `init()` calls `auth.loadFromQuery()` before `loadFromStorage()`.
- `connection.js`: message envelope now includes `user_name` (from name) and `role`.
- `dom.js` + `chat_section.rs`: added `#badge-sub` element.
- Build/deploy followed `zerowrapper/deploy/DEPLOY-ALHEIB-AI/build-deploy.md`:
  `bun build app-entry.js ... --outfile app.js` → `cargo build --release` (9m28s, clean) →
  `topcoat asset bundle --bin zw --release --out deploy/DEPLOY-ALHEIB-AI/assets` →
  copy `target/release/zw` → `docker compose up -d --force-recreate`.
- New bundle `app-7dd91fc6fe6d0bfd.js` confirmed served direct (9033) and via daemon proxy.

### Verified end-to-end (through the daemon proxy)
- `GET /srv/<board>/ai/?email=student.382@sised.sc.ug&name=NAKIYUKA%20LATIFAH&role=student` → 200, base href injected board-scoped.
- Rewritten `app-*.js` served through proxy contains `loadFromQuery`/`user_name`/`zerowrapper_name`.
- WS upgrade `GET /srv/<board>/ai/ws` → 101 (tunnel works).
- `?api=history&user_id=student.382@sised.sc.ug` → `{"session_id":"web.jerboa.student.382@sised.sc.ug", ...}`
  — exactly the sessionId the chat builds from the `?email=` param, so the chat auto-connects
  and history loads.

### Remaining notes
- Iframe sandbox includes `allow-same-origin` so localStorage in the iframe works.
- To re-verify live in a browser: open the SPA at `/srv/<board>/`, click AI Assistant,
  the iframe should show the badge with the student's name and connect without the email form.

---

## Session 2026-08-19 (2nd) — session id email-only, roles array, tour removed, sidebar AI

### Session id: email only (no role suffix)
- `zerowrapper/zw-channels/src/router.rs` `build_session_id` → always `web.{agent}.{email}`.
- `crates/ch-web-ui/src/api/history.rs` + `routes/root_dispatch.rs` history lookups → same shape.
- Verified: `?api=history&user_id=boss@alheib.test` → `"session_id":"web.jerboa.boss@alheib.test"`.
- Old `.admin`-suffixed sessions remain on disk (orphaned history; new messages use email-only id).

### Message envelope → agent: no `owner`, `roles` as array
- `zerowrapper/zw-channels/src/zeroclaw_bridge.rs` `format_prompt` now emits:
  `{"message":..., "channel":"web", "roles":["admin","teacher","accountant"]}` — dropped the
  `owner`/`customer` key entirely.
- `crates/ch-web-ui/src/ws/handler.rs`: accepts `roles` array from the client (falls back to
  legacy `role` string), joins with commas into `ChannelMessage.role`; `format_prompt` splits
  back into the array.
- Chat JS (`auth.js`/`connection.js`): `state.roles` array, persisted as `zerowrapper_roles`;
  WS payload sends `roles: [...]` (array), `user_name`, no `role` string.

### Roles source: `user_roles` table
- Profiles table has ONE `role` per person. Multiple roles live in `user_roles`
  (`user_id` → `role`, one row per role).
- SPA `AIAssistantButton.tsx` now: profiles query by email → get profile `id` → query
  `user_roles` by `user_id` → join roles comma-separated into `?roles=` on the iframe URL.
- Test data: `boss@alheib.test` → profile `boss-profile-001` (role admin) + user_roles
  admin/teacher/accountant. REST chain verified: `user_roles/query?filter={"user_id":"boss-profile-001"}`
  → `["accountant","teacher","admin"]`.

### SPA: Page Tour removed, AI buttons in its place + sidebar
- Deleted `src/components/common/PageGuide.tsx` and `src/lib/tour/*` (tourFlow/tourState/tourTypes).
  Removed imports in `App.tsx`, `DashboardLayout.tsx`, `pages/Index.tsx`.
- `AIAssistantButton` got a `floating` prop (round icon button). Floating AI button sits at the
  old Page Tour spot (`fixed bottom-6 right-6 z-[100]`) in `DashboardLayout.tsx`.
- Sidebar: AI button above Logout in the bottom nav block (`Sidebar.tsx`).
- Verified in deployed bundle: `user_roles/query`, `floating`, no "Page Tour" text.

### Stack state at end
- daemon tmux `srv` (7070) up; zw-jerboa recreated with bundle `app-e2d0b9281ac1117e.js`;
  MinIO + HelixDB up; MCP 42 tools.
- **Known blocker for live chat test:** DeepSeek provider account is out of balance
  (`402 Insufficient Balance`) — the agent turn fails at the LLM provider, but the WS handler
  parsed the `roles` array and the envelope reached zeroclaw fine.
