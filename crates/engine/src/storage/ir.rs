use serde_json::Value as JVal;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Eq,
    Ne,
    Gt,
    Gte,
    Lt,
    Lte,
    Contains,
    NotContains,
    In,
    Search,
    IsNull,
}

impl Op {
    pub fn parse(s: &str) -> anyhow::Result<Op> {
        match s {
            "eq" => Ok(Op::Eq),
            "neq" => Ok(Op::Ne),
            "gt" => Ok(Op::Gt),
            "gte" => Ok(Op::Gte),
            "lt" => Ok(Op::Lt),
            "lte" => Ok(Op::Lte),
            "contains" => Ok(Op::Contains),
            "not_contains" => Ok(Op::NotContains),
            "in" => Ok(Op::In),
            "search" => Ok(Op::Search),
            "is" => Ok(Op::IsNull),
            other => anyhow::bail!(
                "unknown filter operator '{other}' (eq, neq, gt, gte, lt, lte, contains, not_contains, in, search, is)"
            ),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Op::Eq => "eq",
            Op::Ne => "neq",
            Op::Gt => "gt",
            Op::Gte => "gte",
            Op::Lt => "lt",
            Op::Lte => "lte",
            Op::Contains => "contains",
            Op::NotContains => "not_contains",
            Op::In => "in",
            Op::Search => "search",
            Op::IsNull => "is",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Agg {
    Count,
    Sum,
    Avg,
    Min,
    Max,
}

impl Agg {
    pub fn parse(s: &str) -> anyhow::Result<Agg> {
        match s {
            "count" => Ok(Agg::Count),
            "sum" => Ok(Agg::Sum),
            "avg" => Ok(Agg::Avg),
            "min" => Ok(Agg::Min),
            "max" => Ok(Agg::Max),
            other => anyhow::bail!("unknown aggregate '{other}' (count, sum, avg, min, max)"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct FilterCond {
    pub field: String,
    pub op: Op,
    pub value: JVal,
}

#[derive(Debug, Clone, Default)]
pub struct SrvFilter {
    pub conds: Vec<FilterCond>,
}

impl SrvFilter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn matches(&self, payload: &JVal) -> bool {
        self.conds.iter().all(|c| cond_matches(c, payload))
    }
}

pub fn normalize_path(p: &str) -> String {
    if p.starts_with('$') {
        p.to_string()
    } else {
        format!("$.{}", p)
    }
}

#[derive(Debug, Clone)]
pub enum PathSeg {
    Key(String),
    Idx(usize),
}

pub fn path_items(p: &str) -> Option<Vec<PathSeg>> {
    let bytes = p.as_bytes();
    let mut segs: Vec<PathSeg> = Vec::new();
    let mut i = 0;
    let mut key = String::new();
    let push_key = |key: &mut String, segs: &mut Vec<PathSeg>| {
        if !key.is_empty() {
            segs.push(PathSeg::Key(std::mem::take(key)));
        }
    };
    while i < bytes.len() {
        let c = bytes[i] as char;
        match c {
            '.' => {
                push_key(&mut key, &mut segs);
                i += 1;
            }
            '[' => {
                push_key(&mut key, &mut segs);
                let close = p[i + 1..].find(']')? + i + 1;
                let idx: usize = p[i + 1..close].trim().parse().ok()?;
                segs.push(PathSeg::Idx(idx));
                i = close + 1;
                if p[i..].starts_with('.') {
                    i += 1;
                }
            }
            _ => {
                key.push(c);
                i += 1;
            }
        }
    }
    push_key(&mut key, &mut segs);
    if matches!(segs.first(), Some(PathSeg::Key(k)) if k == "$") {
        segs.remove(0);
    }
    if segs.is_empty() {
        return None;
    }
    Some(segs)
}

fn parse_cond_obj(v: &JVal) -> anyhow::Result<Vec<FilterCond>> {
    let obj = v.as_object().ok_or_else(|| anyhow::anyhow!("a condition must be an object"))?;
    let field = normalize_path(
        obj.get("field")
            .and_then(|f| f.as_str())
            .ok_or_else(|| anyhow::anyhow!("a condition needs \"field\" (json path)"))?,
    );
    let mut out = Vec::new();
    if let Some(opname) = obj.get("op").and_then(|o| o.as_str()) {
        let value = obj
            .get("value")
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("an operator on field '{field}' needs a \"value\""))?;
        let op = Op::parse(opname)?;
        check_op_value(&op, &value, &field)?;
        out.push(FilterCond { field, op, value });
        return Ok(out);
    }
    for (k, value) in obj {
        if k == "field" {
            continue;
        }
        let op = Op::parse(k)?;
        check_op_value(&op, value, &field)?;
        out.push(FilterCond { field: field.clone(), op, value: value.clone() });
    }
    if out.is_empty() {
        anyhow::bail!("condition for '{field}' has no operators");
    }
    Ok(out)
}

fn check_op_value(op: &Op, value: &JVal, field: &str) -> anyhow::Result<()> {
    if *op == Op::In && !value.is_array() {
        anyhow::bail!("operator \"in\" on field '{field}' needs an array value");
    }
    if *op == Op::Search && !value.is_string() {
        anyhow::bail!("operator \"search\" needs a string query");
    }
    Ok(())
}

fn push_ops(conds: &mut Vec<FilterCond>, field: &str, ops: &JVal) -> anyhow::Result<()> {
    let field = normalize_path(field);
    match ops {
        JVal::Object(map) => {
            for (opname, value) in map {
                let op = Op::parse(opname)?;
                check_op_value(&op, value, &field)?;
                conds.push(FilterCond { field: field.clone(), op, value: value.clone() });
            }
        }
        other => {
            if other.is_null() {
                // {"field": null} means "is null" (supabase-js .is("f", null)).
                conds.push(FilterCond { field, op: Op::IsNull, value: JVal::Null });
            } else {
                conds.push(FilterCond { field, op: Op::Eq, value: other.clone() });
            }
        }
    }
    Ok(())
}

pub fn parse_filter(v: &JVal) -> anyhow::Result<SrvFilter> {
    let mut conds = Vec::new();
    match v {
        JVal::Array(arr) => {
            for el in arr {
                conds.extend(parse_cond_obj(el)?);
            }
        }
        JVal::Object(obj) if obj.contains_key("field") => {
            conds.extend(parse_cond_obj(v)?);
        }
        JVal::Object(obj) if obj.contains_key("search") => {
            let q = obj
                .get("search")
                .and_then(|s| s.as_str())
                .ok_or_else(|| anyhow::anyhow!("\"search\" needs a string query"))?;
            conds.push(FilterCond {
                field: "_".to_string(),
                op: Op::Search,
                value: JVal::String(q.to_string()),
            });
            for (field, ops) in obj {
                if field == "search" {
                    continue;
                }
                push_ops(&mut conds, field, ops)?;
            }
        }
        JVal::Object(obj) => {
            for (field, ops) in obj {
                if field == "$and" {
                    let arr = ops.as_array().ok_or_else(|| {
                        anyhow::anyhow!("\"$and\" needs an array of condition objects")
                    })?;
                    for el in arr {
                        conds.extend(parse_filter(el)?.conds);
                    }
                    continue;
                }
                if field == "$or" {
                    anyhow::bail!(
                        "operator \"$or\" is not supported yet (use multiple top-level keys for implicit AND, or request $or support)"
                    );
                }
                push_ops(&mut conds, field, ops)?;
            }
        }
        _ => anyhow::bail!(
            "filter must be a condition object, a {{field: op}} map, an array of conditions, or {{\"$and\":[...]}}"
        ),
    }
    Ok(SrvFilter { conds })
}

pub fn scalar_text(v: &JVal) -> String {
    match v {
        JVal::String(s) => s.clone(),
        JVal::Number(n) => n.to_string(),
        JVal::Bool(b) => b.to_string(),
        JVal::Null => String::new(),
        _ => v.to_string(),
    }
}

fn loose_eq(a: &JVal, b: &JVal) -> bool {
    match (a.as_f64(), b.as_f64()) {
        (Some(x), Some(y)) => x == y,
        _ => {
            if a.is_number() && b.is_number() {
                false
            } else {
                a == b
            }
        }
    }
}

fn cmp_num_or_str(a: &JVal, b: &JVal, num: impl Fn(f64, f64) -> bool, s: impl Fn(&str, &str) -> bool) -> bool {
    match (a.as_f64(), b.as_f64()) {
        (Some(x), Some(y)) => num(x, y),
        _ => s(&scalar_text(a), &scalar_text(b)),
    }
}

fn contains(a: &JVal, b: &JVal) -> bool {
    let hay = scalar_text(a).to_lowercase();
    hay.contains(&scalar_text(b).to_lowercase())
}

fn resolve_ir_path(root: &JVal, segs: &[PathSeg]) -> JVal {
    let mut cur = root;
    for seg in segs {
        match seg {
            PathSeg::Key(k) => {
                cur = match cur {
                    JVal::Object(m) => m.get(k).unwrap_or(&JVal::Null),
                    _ => &JVal::Null,
                };
            }
            PathSeg::Idx(i) => {
                cur = match cur {
                    JVal::Array(a) => a.get(*i).unwrap_or(&JVal::Null),
                    _ => &JVal::Null,
                };
            }
        }
    }
    cur.clone()
}

fn cond_matches(c: &FilterCond, payload: &JVal) -> bool {
    let actual = if c.field == "_" {
        payload.clone()
    } else {
        match path_items(&c.field) {
            Some(segs) => resolve_ir_path(payload, &segs),
            None => JVal::Null,
        }
    };
    match c.op {
        Op::Eq => loose_eq(&actual, &c.value),
        Op::Ne => !loose_eq(&actual, &c.value),
        Op::Gt => cmp_num_or_str(&actual, &c.value, |a, b| a > b, |a, b| a > b),
        Op::Gte => cmp_num_or_str(&actual, &c.value, |a, b| a >= b, |a, b| a >= b),
        Op::Lt => cmp_num_or_str(&actual, &c.value, |a, b| a < b, |a, b| a < b),
        Op::Lte => cmp_num_or_str(&actual, &c.value, |a, b| a <= b, |a, b| a <= b),
        Op::Contains => contains(&actual, &c.value),
        Op::NotContains => !contains(&actual, &c.value),
        Op::In => c
            .value
            .as_array()
            .map(|arr| arr.iter().any(|x| loose_eq(&actual, x)))
            .unwrap_or(false),
        Op::Search => crate::expr::search_match(payload, &scalar_text(&c.value)),
        Op::IsNull => {
            let want_null = match &c.value {
                JVal::Null => true,
                JVal::Object(o) => o.get("not").map(|v| v.is_null()).unwrap_or(false),
                _ => false,
            };
            let is_null = actual.is_null();
            is_null == want_null
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_array_of_conditions() {
        let f = parse_filter(&json!([
            {"field": "a", "op": "eq", "value": 1},
            {"field": "b", "op": "gte", "value": 5}
        ]))
        .unwrap();
        assert_eq!(f.conds.len(), 2);
        assert_eq!(f.conds[0].op, Op::Eq);
        assert_eq!(f.conds[1].op, Op::Gte);
    }

    #[test]
    fn parses_single_object_with_field() {
        let f = parse_filter(&json!({"field": "$.name", "op": "contains", "value": "acme"})).unwrap();
        assert_eq!(f.conds.len(), 1);
        assert_eq!(f.conds[0].field, "$.name");
    }

    #[test]
    fn parses_search_object() {
        let f = parse_filter(&json!({"search": "laptop", "brand": {"eq": "X"}})).unwrap();
        assert_eq!(f.conds.len(), 2);
        assert_eq!(f.conds[0].op, Op::Search);
        assert_eq!(f.conds[1].op, Op::Eq);
    }

    #[test]
    fn parses_plain_object_map_with_and() {
        let f = parse_filter(&json!({
            "price": {"gte": 500},
            "$and": [{"color": "red"}, {"size": {"in": ["s", "m"]}}]
        }))
        .unwrap();
        assert_eq!(f.conds.len(), 3);
    }

    #[test]
    fn rejects_or() {
        assert!(parse_filter(&json!({"$or": [{"a": 1}]})).is_err());
    }

    #[test]
    fn matches_payload() {
        let f = parse_filter(&json!({"price": {"gte": 10}, "brand": "acme"})).unwrap();
        assert!(f.matches(&json!({"price": 20, "brand": "acme"})));
        assert!(!f.matches(&json!({"price": 5, "brand": "acme"})));
        assert!(!f.matches(&json!({"price": 20, "brand": "other"})));
    }
}
