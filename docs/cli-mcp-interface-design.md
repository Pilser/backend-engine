# CLI ↔ MCP Interface Design (the "never-guess" control surface)

> Referenced from `serverless-engine-plan.md` **Phase 4 — MCP + CLI (the tooling layer)**.
>
> This document specifies the *interface* between the human/agent and the engine: how help, validation, discovery, and errors work together so that **an agent (or user) never has to guess a command, never lacks knowledge, and can always recover from a mistake in one step**.
>
> It is a design spec — deliberate, complete, and deterministic. It extends the pattern proven in zerowrapper (`wb … srv …`), where layered help + structured errors + "did you mean" already let agents drive a complex API reliably.

---

## 1. Goals & Principles

| # | Principle | What it means concretely |
|---|---|---|
| P1 | **Single source of truth** | One declarative `CommandSpec` registry defines every command. From it we *derive*: the CLI parser, the MCP tool schemas, every help string, the fuzzy matcher, and the OpenAPI-style reference. Help can never drift from behavior. |
| P2 | **Never guess** | Every possible input state is handled deterministically. There is no "maybe", no partial knowledge required: if a command is wrong, the error says exactly what is wrong and how to fix it. |
| P3 | **Always recoverable** | No error returns a bare string. Every error is a structured envelope carrying the original command, the usage, and a hint. The agent can fix in one step, every time. |
| P4 | **CLI = MCP client** | The `srv` CLI is a thin client that issues JSON-RPC `tools/call` over HTTP to the daemon's MCP server. The agent-facing MCP tools and the CLI are the *same implementation*. There is no second grammar to maintain. |
| P5 | **Progressive disclosure** | Help is layered (overview → group → verb → flag/JSON detail) so a novice sees an index and an expert sees a complete spec — without ever hitting a dead end. |
| P6 | **Verify, don't trust** | Every response echoes the normalized command it executed (`command`) and returns `status: ok`; every response shape is documented in help. Destructive ops support `--dry-run`. |
| P7 | **Deterministic and testable** | All help text and schemas are generated from the registry; golden tests assert that help ↔ parser ↔ schemas ↔ docs stay in sync. |

---

## 2. Deployment Shape

```
┌──────────────────────────────────────────────────────────────────────────┐
│  daemon (long-running server)                                            │
│                                                                          │
│   serverless-engine (crate)                                              │
│   └── MCP server (crates/mcp)  ──  HTTP transport (streamable HTTP,      │
│                                   JSON-RPC 2.0)  ◄────── all surfaces     │
│                                                                          │
│   HTTP : 127.0.0.1:PORT (or a public URL for remote boards)             │
└─────────────┬────────────────────────────────────────────────────────────┘
              │  JSON-RPC over HTTP (tools/list, tools/call, help/*)
              │  Authorization: Bearer <key>
      ┌───────┴───────────────────────┬──────────────────────┬──────────────┐
      ▼                               ▼                      ▼
┌─────────────┐               ┌──────────────┐       ┌───────────────┐
│  srv CLI    │  ★ MAIN       │ MCP agent    │       │ srv studio    │
│  (cli crate)│  full impl    │ tools        │       │ (cli crate)   │
│  thin MCP   │               │ (crates/mcp) │       │ REPL wrapper  │
│  client     │               │ tool-per-    │       │ (PARTIAL:     │
│  over HTTP  │               │ feature      │       │  stub to show │
│             │               │ (PARTIAL:    │       │  pattern;     │
│             │               │  stub set    │       │  completed    │
│             │               │  to show;    │       │  later if     │
│             │               │  full set    │       │  needed)      │
│             │               │  later)      │       │               │
└─────────────┘               └──────────────┘       └───────────────┘
```

- **Daemon** hosts the engine + MCP server. HTTP is the only wire protocol. There is no stdio path in production.
- **`srv` CLI (MAIN)** — full implementation, talks to the daemon over HTTP MCP, renders the layered help (§4).
- **MCP agent tools** — *partial* in Phase 4: a representative subset of granular tools (`records.submit`, `records.query`, `apps.create`, `recipes.add`, …) generated from the registry, just enough to prove the pattern. The complete set ships in a later phase.
- **`srv studio`** — *partial*: a readline REPL stub (prompt, history, `help`, run a command) to show the interactive pattern. Full studio features (tab-completion, paging, saved sessions) are deferred and tracked.

All three surfaces are generated from the same registry, so the partial tools/stubs are instantly extensible — completing them later is additive, never a rewrite.

---

## 3. The CommandSpec Registry (the foundation)

Every command — CLI or MCP tool — is described once, declaratively, in the registry. The registry is a list of `CommandSpec` entries. Fields:

| Field | Purpose |
|---|---|
| `group` | e.g. `apps`, `records`, `recipes`, `secrets`, `files` |
| `verb` | e.g. `create`, `submit`, `query` |
| `summary` | one line for indexes |
| `description` | prose: what it does, when to use it, side effects |
| `positional` | ordered args: `{name, type (int|string|json|path|url|board|seq|name…), required, help}` |
| `flags` | `{name, type, default, allowed (enum), repeatable, help, conflicts, requires}` |
| `body_json` | JSON argument schema (for `submit`/`patch`/`actions`) as a JSON Schema fragment |
| `response` | the documented JSON response shape (as a JSON Schema / example) |
| `auth` | required role (public / reader / writer / admin / owner) |
| `destructive` | bool — implies `--dry-run` support and a warning banner |
| `dry_run` | whether `--dry-run` is available |
| `examples` | 1–2 worked examples, including the response |
| `see_also` | related verbs |
| `danger_notes` | e.g. "deletes ALL records — use --filter to be surgical" |

From one entry the generator emits:
1. the **CLI grammar** (positional + flag parser, validation, defaults)
2. the **MCP tool** input/output JSON schemas (`tools/list` / `tools/call`)
3. the **help text** for all four layers (§4)
4. the **fuzzy matcher** vocabulary (§5)
5. the **OpenAPI/`reference`** documentation

Because all five are derived, **nothing can drift**. This is the concrete answer to "agents can't guess or lack knowledge": every fact an agent could need is in one spec and reachable through help.

### Registry content rule (for authors)
Every `CommandSpec` must be written with the **"cold-start agent" test**: a brand-new agent with zero prior context must be able to discover and correctly invoke every command by following help alone. If a command cannot be described completely in the registry, it is not finished.

---

## 4. Help Layers (progressive disclosure, no dead ends)

Inspired by zerowrapper (`wb … srv --help`, `srv <group> --help`, `srv <group> <verb> --help`, `about`), formalized and made deterministic:

### Layer 0 — Orientation (`about`)
`about` — a short narrative, "run this first": what the engine is, the mental model (boards = apps, records = documents, recipes = automation, keys = auth), the three surfaces, the first three steps to try, and a pointer to `--help`. Written for an agent that knows nothing.

### Layer 1 — Index (`--help` / `help`)
Prints:
- `usage: srv <group> <verb> [args]`
- every group with a one-line summary
- the "help paths" cheat-sheet:
  ```
  help                       this index
  help <group>               all verbs in a group
  help <group> <verb>        full spec of one verb (flags, json, response)
  about                      orientation / mental model
  help search <text>         find verbs by keyword
  help json <topic>          filter/patch/actions/rate JSON reference
  help examples              worked end-to-end examples
  ```
- a note that `--help` works at any depth (`srv --help`, `srv apps --help`, `srv apps create --help`).

### Layer 2 — Group (`help apps` / `srv apps --help`)
Per group:
- group description
- every verb with its one-line `summary` (the index)
- cross-references (`see_also`)

### Layer 3 — Verb (`help apps create` / `srv apps create --help`)
Full spec, generated verbatim from the `CommandSpec`:
1. canonical usage line(s) — exactly what the parser accepts
2. prose description + side effects + when to use
3. `positional:` table (name, type, required, help)
4. `flags:` table (name, type, default, allowed values, repeatable, help)
5. `json body:` — the JSON Schema fragment for the JSON argument(s)
6. `response:` — the exact JSON shape returned (so the agent can script against it)
7. `example:` — one worked example with its response
8. `see also:` / `danger:` notes

### Auxiliary help topics (`help json …`)
Generated references for shared JSON vocabularies (so agents never guess their syntax):
- `filter` — all filter JSON shapes (`{field,op,value}`, `{$.path:{op:…}}`, `{search,…}`) + every op with an example
- `patch` — `$set/$inc/$dec/$mul/$unset` with path rules
- `actions` — every recipe action key with its fields
- `rate` / `schedule` (cron) / `link` — their JSON
- `ops` — the full operator list with a one-line example each

> Guarantee: **every help layer is reachable from the top in at most two invocations, and no help layer references anything not also in the registry.**

---

## 5. Discovery, Validation & Recovery (the "never fail" contract)

### 5.1 Error contract — every error is a structured envelope
```json
{
  "status": "error",
  "error": "<precise reason>",
  "command": "<normalized command as parsed>",
  "usage": "<the exact usage line for this verb>",
  "hint": "<one concrete next action>",
  "near": "<the offending token/flag if known>"
}
```
Rules:
- No bare strings; every failure returns this shape.
- `command` echoes the normalized invocation (defaults applied) so the agent can see what the parser actually saw.
- `near` pinpoints the offending token/flag when determinable (mirrors zerowrapper's approach but adds precise location).
- `hint` always suggests the fix, e.g. `use --filter to delete selectively`, `missing value for --schedule`.
- Missing/unknown group or verb triggers `did-you-mean` (§5.2) plus the closest group's usage.

### 5.2 Fuzzy matching — "did you mean"
- Unknown verb/flag/group → `did_you_mean: ["verbA", "verbB"]` ranked by edit distance over the registry vocabulary (zerowrapper uses a `common_prefix` scorer; we extend to Levenshtein + prefix + token overlap).
- Suggestions are always accompanied by the correct usage line.
- A typo in a flag (e.g. `--mthod`) → suggestion `--method` with its allowed values.

### 5.3 Validation & dry-run
- Full argument validation happens **client-side in the CLI** before any HTTP call (type, enum, required, conflicts, requires, path sanity), so the agent gets instant feedback without a round-trip.
- **`--dry-run`** on every `destructive` verb: parse + validate + print the exact operation and affected scope without executing. The response includes `dry_run: true` so the agent knows nothing happened.
- `--yes`/`--confirm` required for non-dry-run destructive verbs unless the verb is already surgical (e.g. `delete --seq N`). `delete --all` always requires `--yes`.

### 5.4 Verification responses
- Every successful command returns `{ "status": "ok", "command": "<normalized>", …result }`.
- The documented `response` shape (§3) is exactly what is returned — no undocumented fields, no surprises.
- `--json` flag on any CLI command → machine-friendly JSON (default when piped).

### 5.5 "Never guess" checklist (concrete acceptance)
1. `about` explains the system to a cold-start agent. ✅ required
2. Every help layer reachable in ≤ 2 invocations. ✅ required
3. Every possible failure returns the §5.1 envelope. ✅ required
4. `did_you_mean` present for every unknown token. ✅ required
5. Every destructive verb supports `--dry-run` + `--yes`. ✅ required
6. Every JSON argument has `help json …` documentation. ✅ required
7. Help ↔ parser ↔ MCP schemas stay in sync (golden tests). ✅ required
8. `srv <anything> --help` never errors, at any depth. ✅ required

---

## 6. The `srv` CLI over HTTP MCP (MAIN deliverable)

### 6.1 Connection & auth
- Daemon URL: `SERVERLESS_DAEMON_URL` env, else `--daemon <url>`, else `~/.config/serverless/config.toml`, else default `http://127.0.0.1:PORT`.
- The CLI can also auto-spawn a local daemon (`srv daemon start`) and remember its address.
- Auth: `--key <token>` / `SERVERLESS_KEY` env / `~/.config/serverless/keys.toml`; sent as `Authorization: Bearer <key>` on every JSON-RPC request. The server resolves it to a `Principal` (board owner / admin / writer / reader / customer-scoped).

### 6.2 Argv → tool call mapping
```
srv records query b_abc --filter '{"price":{"gte":500}}' --limit 10 --json
  →  JSON-RPC tools/call { name: "records.query", arguments: {
        board: "b_abc", filter: {"price":{"gte":500}}, limit: 10, json: true } }
  →  HTTP POST <daemon>/mcp   (streamable HTTP transport)
  →  render result
```
- The CLI parser is generated from the `CommandSpec` registry (§3) — same source as the MCP tool schemas. The verb grammar is exactly the MCP tool arguments.
- Output rendering: human table/text for TTY, JSON when `--json` or piped. Structure mirrors the MCP tool result.

### 6.3 Why "CLI = MCP client" is better than zerowrapper's embedding
- zerowrapper embeds the CLI grammar *inside* one MCP tool (`wb … srv …`), so the agent and CLI share it but it's one giant string-parsing blob.
- Here the **MCP tools are granular** (one per feature), each with typed JSON schemas, and the **CLI is the thin client** that turns argv into tool calls. Same single source of truth, but: agents get typed schemas, the CLI stays simple, and the HTTP transport means remote servers and multi-user boards work unchanged.

---

## 7. MCP Agent Tools (tool-per-feature — PARTIAL in Phase 4)

- **Naming:** `{group}.{verb}` (e.g. `apps.create`, `records.submit`, `records.query`, `recipes.add`, `files.get`). Names are stable and unique.
- **Schemas:** input/output derived from the `CommandSpec` registry; every tool documents `summary`, `description`, `destructive`, and `dry_run` in its `tools/list` entry.
- **Help integration:** tools are registered so that `help <group> <verb>` and the tool schema are byte-identical.
- **Phase 4 scope (partial):** a curated subset — enough to run a real workflow end-to-end — e.g. `apps {create,list,show,delete}`, `records {submit,get,list,query,search,aggregate,patch,delete}`, `recipes {list,add}`, `files {list}`. The full set (all ~50 verbs) is generated later from the registry with no design change.

---

## 8. `srv studio` (REPL — PARTIAL in Phase 4)

- `srv studio` — a readline loop wrapping the same HTTP MCP client.
- **Phase 4 stub scope:** prompt, command history, `help`/`about` rendering, running any CLI command, `quit`. Enough to *show* the pattern.
- **Deferred (tracked, not built):** tab-completion from the registry, syntax highlighting, paged output, saved board/daemon sessions, multi-pane layout.
- Because it is a thin wrapper over the CLI client, it inherits every guarantee in §5 for free.

---

## 9. Generation & Testing (keeping it drift-free)

- `engine` ships a `registry` module holding all `CommandSpec`s (declarative data, not hand-written help strings).
- A `gen` tool (in `cli`) renders: CLI parser, MCP schemas, help text, `reference` doc, fuzzy vocabulary — at build time.
- **Golden tests:** for every verb, assert (a) `srv <verb> --help` output == generated spec, (b) parser accepts exactly the documented grammar, (c) MCP `tools/list` schema == the same spec, (d) every documented example round-trips. Any change to a `CommandSpec` forces the goldens to update — nothing can silently diverge.

---

## 10. Phase 4 Work Breakdown (as referenced from the plan)

| # | Task | Scope |
|---|---|---|
| 1 | Registry module + `CommandSpec` model in `engine` | Full |
| 2 | Help generator (Layers 0–3 + `help json …` topics) | Full |
| 3 | CLI client (HTTP MCP) + argv→tool mapping + rendering | **MAIN — Full** |
| 4 | Daemon HTTP MCP transport + auth (Bearer → Principal) | Full |
| 5 | Granular MCP tools — curated subset | Partial (shows pattern; full set later) |
| 6 | `srv studio` REPL stub | Partial (shows pattern; enhanced later) |
| 7 | Golden tests (help↔parser↔schemas↔examples) | Full |
| 8 | `reference` (OpenAPI-style) generation + `help search` | Full |
