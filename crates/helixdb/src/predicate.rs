//! Predicate builder + translation of engine filter conditions into Helix
//! predicate JSON. The crate stays engine-agnostic: the adapter passes the op
//! as a string (`eq`, `ne`, `gt`, ...).

use serde_json::{json, Value as Json};

fn constant(v: &Json) -> Json {
    let val = match v {
        Json::Null => json!({ "null": null }),
        Json::Bool(b) => json!({ "bool": b }),
        Json::Number(n) if n.is_i64() || n.is_u64() => json!({ "i64": n }),
        Json::Number(n) => json!({ "f64": n }),
        Json::String(s) => json!({ "string": s }),
        Json::Array(a) => json!({ "f64_array": a }),
        Json::Object(_) => json!({ "string": v.to_string() }),
    };
    json!({ "constant": val })
}

fn simple(op: &str, left: &str, right: &Json) -> Json {
    json!({ op: { "left": { "property": left }, "right": constant(right) } })
}

fn in_pred(field: &str, values: &[Json]) -> Json {
    json!({
        "or": {
            "predicates": values.iter().map(|v| simple("eq", field, v)).collect::<Vec<_>>()
        }
    })
}

/// Chainable predicate builder producing `{ op: { left, right } }` trees.
#[derive(Debug, Clone, Default)]
pub struct Predicate {
    root: Option<Json>,
}

impl Predicate {
    pub fn new() -> Self {
        Self::default()
    }

    fn and(mut self, p: Json) -> Self {
        self.root = Some(match self.root.take() {
            Some(prev) => json!({ "and": { "predicates": [prev, p] } }),
            None => p,
        });
        self
    }

    pub fn eq(self, field: &str, value: Json) -> Self {
        self.and(simple("eq", field, &value))
    }

    pub fn ne(self, field: &str, value: Json) -> Self {
        self.and(simple("ne", field, &value))
    }

    pub fn gt(self, field: &str, value: Json) -> Self {
        self.and(simple("gt", field, &value))
    }

    pub fn gte(self, field: &str, value: Json) -> Self {
        self.and(simple("gte", field, &value))
    }

    pub fn lt(self, field: &str, value: Json) -> Self {
        self.and(simple("lt", field, &value))
    }

    pub fn lte(self, field: &str, value: Json) -> Self {
        self.and(simple("lte", field, &value))
    }

    pub fn in_values(self, field: &str, values: Vec<Json>) -> Self {
        self.and(in_pred(field, &values))
    }

    /// AND in a pre-built predicate JSON (used by the adapter to combine
    /// routing predicates with translated conditions).
    pub fn and_raw(self, pred: Json) -> Self {
        self.and(pred)
    }

    /// Serialize to the JSON predicate form.
    pub fn into_json(self) -> Json {
        self.root.unwrap_or_else(|| {
            json!({ "eq": { "left": { "property": "$id" }, "right": { "constant": { "i64": -1 } } } })
        })
    }
}

/// Translate one engine filter condition into a Helix predicate.
///
/// `op` is the engine op name (`eq`, `ne`, `gt`, `gte`, `lt`, `lte`, `in`).
/// `contains`/`not_contains`/`search` return `None`: the caller handles them
/// by materializing rows and matching in Rust, or via BM25.
pub fn from_cond(field: &str, op: &str, value: &Json) -> Option<Json> {
    match op {
        "eq" => Some(simple("eq", field, value)),
        "ne" => Some(simple("ne", field, value)),
        "gt" => Some(simple("gt", field, value)),
        "gte" => Some(simple("gte", field, value)),
        "lt" => Some(simple("lt", field, value)),
        "lte" => Some(simple("lte", field, value)),
        "in" => {
            let arr = value.as_array().cloned().unwrap_or_default();
            Some(in_pred(field, &arr))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_eq_predicate() {
        let p = Predicate::new().eq("status", json!("active")).into_json();
        assert_eq!(p["eq"]["left"]["property"], "status");
        assert_eq!(p["eq"]["right"]["constant"]["string"], "active");
    }

    #[test]
    fn chains_with_and() {
        let p = Predicate::new().eq("a", json!(1)).gt("b", json!(2)).into_json();
        assert_eq!(p["and"]["predicates"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn in_becomes_or_of_eqs() {
        let p = from_cond("size", "in", &json!(["s", "m"])).unwrap();
        assert_eq!(p["or"]["predicates"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn unsupported_ops_return_none() {
        assert!(from_cond("a", "contains", &json!("x")).is_none());
        assert!(from_cond("a", "search", &json!("x")).is_none());
    }
}
