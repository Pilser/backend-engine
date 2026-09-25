# Graph Schema Tracking — Proposal

**Status:** Proposed (not yet implemented)
**Goal:** make a board's **graph wiring** (edge labels + which tables sync into the graph) visible and versionable through MCP, without depending on HelixDB schema introspection.

## Problem

The graph tools (`graph.link`, `graph.unlink`, `graph.traverse`, `graph.sync`,
`graph.search_edges`) are operational: you can *create* and *walk* edges, but
there is **no way to enumerate the graph's structure**:

- What **edge labels** exist in a board's tenant?
- Which **tables** have been synced into the graph (`graph.sync`)?
- Which **node labels** / properties are in use?

### Why HelixDB introspection is not available

We evaluated the HelixDB SDKs and the `tools/helixdb-skills/` agent skills:

- The hosted `helix-mcp` server (`helix_list_database_indexes`, etc.) is
  **Helix Cloud-only** — it requires Cloud auth and cannot talk to our
  self-hosted `ghcr.io/helixdb/helixdb:v0.0.4` runtime.
- Our engine talks raw `POST /v2/query`. Every node/edge source
  (`nodes_where`, `edges_where`) requires a **concrete label** in its
  predicate. There is **no "list distinct labels" primitive** in the v3 SDK,
  the query DSL, or the dynamic-query route.
- `DatabaseCaps` has no schema-introspection field.

So the graph's labels cannot be *discovered* from the backend today — they must
be **tracked by the engine** as they are created/updated/removed.

## Proposal: a `wb_graph_schema` table

Add a per-board table that records the graph's declared wiring, written
transactionally by the graph mutation functions.

### Schema

Table name: `wb_graph_schema` (constant `TABLE_GRAPH_SCHEMA` in
`crates/engine/src/tables.rs`).

One row per (board, kind, name):

```json
{
  "board_id": "b_7z5lr8zaurkd0000",
  "kind": "edge_label",              // "edge_label" | "synced_table" | "node_label"
  "name": "ENROLLED_IN",             // the edge label / table / node label
  "from_table": "learners",          // edge_label: source table (optional)
  "to_table": "classes",             // edge_label: target table (optional)
  "props": ["since", "note"],        // properties seen on the label (optional)
  "first_seen_at": "2026-08-20T...",
  "last_seen_at": "2026-08-20T..."
}
```

Key: `scoped_key(board_id, format!("{kind}/{name}"))`.

### Write points (kept in sync automatically)

| Mutation | Effect |
|----------|--------|
| `graph.link` (add edge) | upsert `edge_label` row for the label, record `props` |
| `graph.sync` (table → graph) | upsert `synced_table` row for the table |
| `graph.unlink` / `graph.delete` | do **not** drop the label row (a label may still be used); a `graph.schema` `--prune` flag can remove labels with no remaining edges — requires a count query |
| `tables.delete` | remove matching `synced_table` / `node_label` rows for the table |

### Read surface

- `graph.schema` returns the full contents of `wb_graph_schema` for the board:
  - `edge_labels`: the distinct edge labels + their props
  - `synced_tables`: tables wired into the graph by `graph.sync`
  - `node_labels`: node labels in use
- No HelixDB introspection needed — this is engine-declared state, so it also
  works on the in-memory backend.

### Benefits for the app-folder format

The `wb_graph_schema` rows are exactly what an `export` of a board writes into
the `links.json` / `graph.json` files of the app-folder layout (see
`docs/serverless-engine-plan.md` and the app-folder convention). A graph
becomes diffable, reviewable, and re-appliable like any other concern.

## Alternatives considered

1. **Scan HelixDB for labels** — rejected: no distinct-label query exists on
   the self-hosted runtime.
2. **Derive from `wb_records`** — partially useful (node labels = table
   names), but cannot capture edge labels or edge props.
3. **REST/CLI-only, no table** — rejected: without persistence the schema
   disappears on restart and can't be exported.

## Open questions

- Should `graph.schema --prune` issue a per-label edge count against HelixDB
  (N queries) or trust the tracked rows? (Trust-then-prune is cheaper.)
- Should `node_label` rows be written on `graph_sync` only, or on every
  `records.submit` (expensive)? — recommend `graph_sync` only.
