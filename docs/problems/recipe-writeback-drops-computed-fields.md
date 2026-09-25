# Problem: recipe write-back drops computed fields on create (inventory tracking_number)

**Status:** RESOLVED 2026-08-18 via two fixes:
1. `write_node` updated-branch (crates/server/src/db/helix.rs): one set_property
   per prop (data + mirrors), all keyed on `var: existing`, existing FIRST in the
   batch (Helix: `variable 'existing' is not bound` otherwise).
2. merge write-back (crates/engine/src/crud.rs `record_set_raw`): deep-merge the
   recipe's working payload into the current payload instead of replacing it, so
   concurrent create recipes accumulate instead of clobbering.
Verified: issuance → ISS-…, restock → REC-…, damage → TRX-… all persist.

The stock quantity derivation is STILL being moved to the graph (below), which
removes the denormalized counter entirely.

**Date:** 2026-08-18

---

## Symptom

The `generate_tracking_number` trigger (SQL) sets `tracking_number` on every
`inventory_transactions` insert. Ported to an engine **recipe**:

```
recipes.add gen_tracking_number
  when: {event: record.created, table: inventory_transactions}
  actions: [{$compute: {$.tracking_number: "concat(if($.type == 'issuance','ISS-', if($.type=='restock','REC-','TRX-')), randhex(8))"}}]
```

Observed: `tracking_number` is missing on some creates.

| type | result |
|---|---|
| damage | ✅ `TRX-92BA754CC5E81391` |
| issuance | ❌ None |
| restock | ❌ None |

`damage` does NOT trigger the stock recipes (stock_sub_qty matches `issuance`,
stock_add_qty matches `restock`) — issuance/restock do, and they are the failing
ones.

## Root causes found so far

### 1. `write_node` updated-branch dropped the data write (FIXED)

`crates/server/src/db/helix.rs` `write_node` built the existing-node update as a
sequence of `set_property` calls, each REPLACING the previous:

```rust
let mut updated = set_property(input, DATA_PROP, row.data);  // data
for (k, v) in numeric_mirror_props(...) { updated = set_property(input, &k, &v); } // clobbers
```

Only the LAST property was set — `data` (which carries the computed fields) was
lost whenever any numeric mirror existed. `inventory_transactions` has numeric
mirrors (`quantity`), so the recipe write-back silently dropped `tracking_number`.

Fix applied: emit ONE `set_property` per prop (data + mirrors), all conditionally
keyed on the same `var: existing`, as separate queries in the write batch, with
`existing` FIRST (Helix errors `variable 'existing' is not bound` if the var query
comes after its users).

### 2. Multiple create recipes write back independently and clobber each other (UNRESOLVED)

Every recipe's `working` payload starts from the SAME original payload, and each
recipe that changes the payload writes back the WHOLE payload via `record_set_raw`.
The LAST write-back wins. So if `gen_tracking_number` fires before
`stock_sub_qty`/`stock_add_qty`, the stock recipe's write-back restores the payload
WITHOUT `tracking_number`.

Evidence: with the 3 debug recipes (`test_marker`, `test_compute_only`,
`test_both`) enabled, they clobbered gen_tracking_number's write-back even after
the write_node fix. Disabling them made `damage` work (no competing recipe);
`issuance`/`restock` still fail because stock_add_qty/stock_sub_qty are the
competitors.

This is a recipe-engine design limitation: **concurrent create recipes that all
write back need sequential composition (each recipe sees the previous recipe's
output) or a merge-on-write-back** (`record_set_raw` should deep-merge into the
current stored payload, not replace it).

## Proposed fixes (pick one, solve later)

1. **Merge write-back:** change `record_set_raw` (and the recipe dispatch write-
   back) to deep-merge the working payload into the current record's payload
   instead of replacing it. Then every recipe's change accumulates. This is the
   robust fix for ALL multi-recipe create flows (approval cascades, derivations).
2. **Sequential dispatch:** in `automation::dispatch`, re-read the current record
   after each recipe's write-back and pass THAT as the next recipe's payload
   (recipes compose in order).
3. **Ordering:** make `recipe_list` order by name and document that later recipes
   must not clobber (fragile; rejected).

## Recommendation

Option 1 (merge write-back) is the correct fix. It is deferred: the stock
quantity derivation is being re-architected to use the GRAPH instead of a
denormalized counter, which removes `stock_add_qty`/`stock_sub_qty` write-backs
entirely — leaving `gen_tracking_number` as the only create recipe on the table,
so the clobber disappears without an engine change.

## Graph-based alternative (being adopted)

The `update_inventory_stock` trigger maintains a denormalized `inventory_stock`
counter that races with every transaction. Instead:

- `inventory_transactions.item_id` → `RELATED_ITEM` edge (via `graph.sync`).
- Stock = read-time traversal: `graph.traverse` from the item node over
  `RELATED_ITEM` edges, then `sum(restock + return) - sum(issuance + damage + adjustment)`.
- No counter table, no trigger, no write-back races. The graph is the source of truth.

The adapter fix (#1 above) is still worth keeping — it fixes recipe write-backs
for ALL tables with numeric mirrors, not just inventory.
