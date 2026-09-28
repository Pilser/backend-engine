# Plugins — the native standard

A plugin is **not a worker**. It is a manifest the engine installs: dist slug
+ namespaced tables + recipes + jobs + route bindings + email intent, in one
admin call. No `worker.js`, no Cloudflare API calls, no side-workers.

## Manifest

```json
{
  "slug": "shop",
  "title": "Shop",
  "version": "0.1.0",
  "tables": [
    { "table": "plugin_shop_orders", "schema": { "type": "object" }, "unique_key": "$.id" }
  ],
  "recipes": [
    { "name": "notify", "when": { "event": "record.created", "table": "plugin_shop_orders" },
      "actions": [{ "$send_email": { "to": "{{$.email}}", "subject": "Order", "text": "thanks" } }] }
  ],
  "jobs": [
    { "name": "sync", "schedule": "@every 1h",
      "action": { "type": "http", "url": "https://supplier.example.com/ping" } }
  ],
  "routes": [
    { "name": "orders", "method": "GET", "op": "query",
      "table": "plugin_shop_orders", "filter": { "status": "open" }, "limit": 50 }
  ],
  "email": { "inbound": true },
  "subapp": { "title": "Shop", "index": "index.html" }
}
```

### Rules (enforced by the installer)

- `slug`: `^[a-z0-9][a-z0-9-]{0,39}$`.
- Tables MUST be named `plugin_<slug-with-_-for-->_*` — collision-proof and
  prunable. Anything else is rejected.
- Recipe/job names are auto-prefixed `<slug>__<name>` (pre-prefixed names
  pass through, so reinstalls are idempotent).
- Install is idempotent per slug: reinstall replaces recipes/jobs/routes and
  the plugin record; existing tables are kept (never dropped on install).
- `remove` drops recipes, jobs, routes, and the sub-app registration.
  Tables drop ONLY with `prune=true` — data loss stays explicit.

## Email

Outbound and inbound are engine-native; no mail worker exists.

**Send** — secrets (never in recipes):
`MAIL_PROVIDER` (`resend` default, or `mailchannels`), `MAIL_API_KEY`
(required for resend; optional `X-Api-Key` for mailchannels),
`MAIL_FROM` (default sender).

- Recipe: `{"$send_email": {"to": "{{$.email}}", "subject": "…",
  "text": "…" /* or "html" */}}` — fields template from the payload,
  result lands on `$.email_result`.
- Ad-hoc: `email send to@x.io 'Subject' --text '…' [--html …] [--from …]`
  or `POST /api/email/send` (admin).

**Receive** — Cloudflare dashboard: Email Routing → send to this worker
(configuration, not code). Each message is stored in `email_log`
(`direction: "in"`, body truncated to 4000 chars) and `email.received`
recipes run against `{from, to, subject, body}`:

```json
{ "name": "support_triage",
  "when": { "event": "email.received" },
  "match": { "to": "support@example.com" },
  "actions": [{ "$upsert_other": { "table": "tickets", "record": {
    "from": "{{$.from}}", "subject": "{{$.subject}}", "body": "{{$.body}}" } } }] }
```

Anti-abuse: gate with `match: {"from": …}` allowlists (anyone can email
the address). Recipe dedup guards repeats. Never auto-reply to
`{{$.from}}` unconditionally — bounce loops.

## Route bindings (configured functions)

No code: a route declares an operation on a plugin table. Served at
`ANY /api/plugin/{slug}/{route}` — the binding's method is enforced
(405 otherwise), the binding's base `filter` ANDs with any caller
`?filter=`, caller `?limit=` is capped by the binding.

- `query`: table + optional filter/order/limit. Reads honor `public_reads`.
- `get`: `?seq=`. `submit`: JSON body (validated by the table schema,
  writer-gated). `aggregate`: `?op=&field=&group=` like `/api` aggregate.

## Control surfaces (all three, same grammar family)

- MCP/terminal: `plugins install '{…manifest…}'`, `plugins list`,
  `plugins show <slug>`, `plugins remove <slug> [--prune]`,
  `email send <to> <subject> [--text/--html/--from]`.
- REST: `POST|GET /api/plugins`, `GET|DELETE /api/plugins/{slug}`,
  `ANY /api/plugin/{slug}/{route}`, `POST /api/email/send`.
- Dist bytes travel separately: `files put --slug <slug>` (front-end
  hosting), then the manifest's `subapp` registers it at `/srv/<slug>/`.

## Migrating pilserlabs-style plugins

| Old (side-worker) | Native |
|---|---|
| `worker.js` upload + CF Scripts/Routes API | manifest `routes` + `recipes` |
| `p:{slug}:{key}` KV | namespaced table (`plugin_<slug>_kv`) |
| `plugin_*` raw SQL passthrough | route bindings (`query`/`aggregate`) |
| dist zip + SSR rewrite | `files put --slug` + `/srv/<slug>/` |
| send-mail binding / SMTP / Email worker | `$send_email` + `email.received` |

What stays outside the engine on purpose: arbitrary JS execution
(workerd forbids dynamic code — platform rule), raw SMTP/IMAP sockets
(HTTPS mail APIs cover it).

## Site routes (exact paths) + request proxy

One worker = one app = one site: no shim worker for sitemap/robots/root.
Manifest keys (validated up front, conflicts fail the install):

```json
"site_routes": [
  { "path": "/sitemap.xml", "op": "query", "table": "plugins",
    "format": "sitemap", "url_field": "slug", "prefix": "/p/", "limit": 5000 },
  { "path": "/robots.txt", "text": "User-agent: *\nAllow: /\n" },
  { "path": "/", "redirect": "/srv/", "status": 302 },
  { "path": "/manual.pdf", "asset": "docs/manual.pdf" }
],
"request_routes": [
  { "path": "/docs/*", "op": "proxy", "target": "https://cdn.example.com/{{$.path}}",
    "inject_headers": { "x-tenant": "{{$.tenant}}" } }
]
```

Rules: paths start with `/`; exact beats longest-`/*`-prefix;
`/api/*`, `/mcp`, `/srv/*`, `/ws`, `/healthz` are reserved (rejected).
Same path + same owner replaces (upsert, so re-PUT updates); same path +
different owner is a conflict error (no silent shadowing). Site routes are public
BY DEFAULT (anonymous + edge-cacheable). Per route, `allow_roles` tunes
it: absent/empty stays public; `["reader"]` (or writer/admin) switches to
keyed mode — caller must hold the tier, reads follow the caller (not
anonymous), nothing is shared-cached (`private, max-age=0`). Unknown role
names are rejected at write time. Scoped customer keys are not admitted
(use unscoped reader+ keys for keyed routes). Validators (ETag/304) apply
uniformly; dynamic query routes default to short `s-maxage` (overridable
per route).

Proxy notes: method allowlist per binding (default GET), query forwarded
unless the template embeds `{{$.query}}`, bodies to 5 MiB, upstream
`Authorization`/`Cookie` never forwarded (use `inject_headers` for
upstream auth), targets SSRF-gated like `$call`, upstream status + bytes
passed through. Proxy responses are never edge-cached (dynamic upstream).

Manage live: `site routes|add|remove` (MCP/terminal — same grammar family),
`GET|POST|DELETE /api/site/routes` (REST). Tenant-owned vs
`plugin:<slug>`-owned shown everywhere; plugin removal prunes its rows.
