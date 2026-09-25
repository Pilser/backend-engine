//! Typed builders mirroring the Helix JSON envelope. Each helper returns the
//! `snake_case` AST object consumed by `POST /v2/query`; chained operations
//! nest via the `input` key.

use crate::predicate::Predicate;
use crate::tenant::TENANT_PROP;
use serde_json::{json, Value as Json};

fn prop_value(v: &Json) -> Json {
    match v {
        Json::Null => json!({ "null": null }),
        Json::Bool(b) => json!({ "bool": b }),
        Json::Number(n) if n.is_i64() || n.is_u64() => json!({ "i64": n }),
        Json::Number(n) => json!({ "f64": n }),
        Json::String(s) => json!({ "string": s }),
        other => json!({ "string": other.to_string() }),
    }
}

fn prop_input(v: &Json) -> Json {
    json!({ "value": prop_value(v) })
}

fn literal(n: usize) -> Json {
    json!({ "literal": n })
}

/// Node source by label AND tenantId == tenant.
pub fn nodes_in_tenant(label: &str, tenant: &str) -> Json {
    json!({
        "nodes_where": {
            "predicate": Predicate::new()
                .eq("$label", json!(label))
                .eq(TENANT_PROP, json!(tenant))
                .into_json()
        }
    })
}

/// Node source by `$id` reference.
pub fn node_by_id(id: i64) -> Json {
    json!({ "nodes": { "reference": { "ids": [id] } } })
}

/// Wrap `input` in a `where` predicate.
pub fn where_(input: Json, predicate: Json) -> Json {
    json!({ "where": { "input": input, "predicate": predicate } })
}

/// Wrap `input` in `value_map` projecting the listed properties.
pub fn value_map(input: Json, properties: Vec<&str>) -> Json {
    json!({ "value_map": { "input": input, "properties": properties } })
}

/// Wrap `input` in ordering + skip/limit.
pub fn order_and_page(input: Json, orders: &[(String, bool)], limit: usize, offset: usize) -> Json {
    let mut out = input;
    for (field, desc) in orders.iter().rev() {
        let dir = if *desc { "desc" } else { "asc" };
        out = json!({
            "order_by": { "input": out, "property": field, "order": dir }
        });
    }
    if offset > 0 {
        out = json!({ "skip": { "input": out, "count": literal(offset) } });
    }
    if limit != usize::MAX {
        out = json!({ "limit": { "input": out, "count": literal(limit) } });
    }
    out
}

/// A single batch entry: `{ query: { name, root } }`.
pub fn entry(name: &str, root: Json) -> Json {
    json!({ "query": { "name": name, "root": root } })
}

/// Read batch envelope body: `{ read: { entries, returns } }`.
pub fn read_batch(entries: Vec<Json>, returns: Vec<&str>) -> Json {
    json!({ "read": { "entries": entries, "returns": returns } })
}

/// Write batch envelope body: `{ write: { entries, returns } }`.
pub fn write_batch(entries: Vec<Json>, returns: Vec<&str>) -> Json {
    json!({ "write": { "entries": entries, "returns": returns } })
}

/// `add_n` with ordered properties; the tenant property is always included.
pub fn add_n(label: &str, tenant: &str, properties: &[(String, Json)]) -> Json {
    let mut props: Vec<Json> = properties
        .iter()
        .map(|(k, v)| json!([k, prop_input(v)]))
        .collect();
    props.push(json!([TENANT_PROP, prop_input(&json!(tenant))]));
    json!({ "add_n": { "label": label, "properties": props } })
}

/// `set_property` on the current input. Note: the wire field is `name`, not
/// `property` (confirmed against the Helix SDK serializer).
pub fn set_property(input: Json, name: &str, value: &Json) -> Json {
    json!({
        "set_property": { "input": input, "name": name, "value": prop_input(value) }
    })
}

/// `drop` the current elements (returns the dropped rows).
pub fn drop(input: Json) -> Json {
    json!({ "drop": { "input": input } })
}

/// `add_e` — create an edge from the current node(s) to `to` with a label and
/// properties. The tenant property is always included on the edge.
pub fn add_e(input: Json, label: &str, to: &str, tenant: &str, properties: &[(String, Json)]) -> Json {
    let mut props: Vec<Json> = properties
        .iter()
        .map(|(k, v)| json!([k, prop_input(v)]))
        .collect();
    props.push(json!([TENANT_PROP, prop_input(&json!(tenant))]));
    json!({
        "add_e": { "input": input, "label": label, "to": { "var": to }, "properties": props }
    })
}

/// Traverse `out`/`in`/`both` from the current nodes by edge label.
pub fn traverse(input: Json, dir: &str, label: Option<&str>) -> Json {
    let mut obj = serde_json::Map::new();
    obj.insert("input".into(), input);
    if let Some(l) = label {
        obj.insert("label".into(), json!(l));
    }
    json!({ dir: obj })
}

/// Edge source by label + tenant scope (`edges_where`).
pub fn edges_where(label: &str, tenant: &str) -> Json {
    json!({
        "edges_where": {
            "predicate": Predicate::new()
                .eq("$label", json!(label))
                .eq(TENANT_PROP, json!(tenant))
                .into_json()
        }
    })
}

/// BM25 search over edges: `text_search_edges` with tenant scope.
pub fn text_search_edges_with(
    label: &str,
    property: &str,
    query: &str,
    k: usize,
    tenant: &str,
) -> Json {
    json!({
        "text_search_edges": {
            "label": label,
            "property": property,
            "tenant_value": { "value": { "string": tenant } },
            "query_text": { "value": { "string": query } },
            "k": { "literal": k }
        }
    })
}

/// `create_index` for a node or edge text index (idempotent).
pub fn create_text_index(kind: &str, label: &str, property: &str) -> Json {
    let spec = if kind == "edge" {
        json!({ "edge_text": { "label": label, "property": property, "tenant_property": TENANT_PROP } })
    } else {
        json!({ "node_text": { "label": label, "property": property, "tenant_property": TENANT_PROP } })
    };
    json!({ "create_index": { "spec": spec, "if_not_exists": true } })
}

/// Node source scoped to a board's tenant AND its `table` property — the
/// workhorse for `wb_records` rows.
pub fn record_source(tenant: &str, table: &str) -> Json {
    json!({
        "nodes_where": {
            "predicate": Predicate::new()
                .eq("$label", json!(crate::namespaces::engine_label("wb_records")))
                .eq(TENANT_PROP, json!(tenant))
                .eq("table", json!(table))
                .into_json()
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_n_includes_tenant() {
        let props = vec![("name".to_string(), json!("alice"))];
        let node = add_n("Learner", "b_1", &props);
        let arr = node["add_n"]["properties"].as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[1][0], "tenantId");
    }

    #[test]
    fn record_source_scopes_tenant_and_table() {
        let src = record_source("b_1", "learners");
        let pred = &src["nodes_where"]["predicate"];
        // The chain is a nested `and` tree: and(eq label, and(eq tenant, eq table)).
        // Collect all leaf eq property names.
        let mut props = Vec::new();
        fn collect(pred: &Json, out: &mut Vec<String>) {
            if let Some(and) = pred.get("and").and_then(|a| a.get("predicates")).and_then(|p| p.as_array()) {
                for p in and {
                    collect(p, out);
                }
            } else if let Some(eq) = pred.get("eq") {
                if let Some(l) = eq.get("left").and_then(|l| l.get("property")).and_then(|v| v.as_str()) {
                    out.push(l.to_string());
                }
            }
        }
        collect(pred, &mut props);
        assert_eq!(props, vec!["$label", "tenantId", "table"]);
    }
}
