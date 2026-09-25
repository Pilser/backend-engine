use crate::expr::ast::ExprError;
use crate::expr::{eval, funcs, parse};
use serde_json::{json, Map, Value};

pub fn invoke(name: &str, args: &[Value], data: &Value) -> Result<Value, ExprError> {
    let n = name.to_lowercase();
    match n.as_str() {
        "map" => map(args),
        "filter" => filter(args),
        "reduce" => reduce(args),
        "sum" => sum(args),
        "avg" => avg(args),
        "first" => first(args),
        "last" => last(args),
        "sort" => sort(args),
        "unique" => unique(args),
        "flatten" => flatten(args),
        "count" => count(args),
        "object" => object(args),
        "array" => Ok(Value::Array(args.to_vec())),
        "keys" => keys(args),
        "values" => values(args),
        "get" => get(args, data),
        _ => Err(ExprError::new(0, format!("unknown function '{n}'"))),
    }
}

fn field_of(el: &Value, field: &Option<String>) -> Value {
    match field {
        Some(p) => crate::expr::get_path(el, p),
        None => el.clone(),
    }
}

fn first_arr(args: &[Value]) -> Option<&Vec<Value>> {
    match args.first() {
        Some(Value::Array(a)) => Some(a),
        _ => None,
    }
}

fn field_arg(args: &[Value]) -> Option<String> {
    args.get(1).and_then(|v| v.as_str()).map(str::to_string)
}

fn map(args: &[Value]) -> Result<Value, ExprError> {
    let arr = first_arr(args).ok_or_else(|| ExprError::new(0, "map expects an array"))?;
    let src = args
        .get(1)
        .and_then(|v| v.as_str())
        .ok_or_else(|| ExprError::new(0, "map expects an expression string"))?;
    let expr = parse(src)?;
    let mut out = Vec::new();
    for el in arr {
        out.push(eval::eval(&expr, el, 0)?);
    }
    Ok(Value::Array(out))
}

fn filter(args: &[Value]) -> Result<Value, ExprError> {
    let arr = first_arr(args).ok_or_else(|| ExprError::new(0, "filter expects an array"))?;
    let src = args
        .get(1)
        .and_then(|v| v.as_str())
        .ok_or_else(|| ExprError::new(0, "filter expects a condition expression"))?;
    let expr = parse(src)?;
    let mut out = Vec::new();
    for el in arr {
        let v = eval::eval(&expr, el, 0)?;
        if funcs::truthy(&v) {
            out.push(el.clone());
        }
    }
    Ok(Value::Array(out))
}

fn reduce(args: &[Value]) -> Result<Value, ExprError> {
    let arr = first_arr(args).ok_or_else(|| ExprError::new(0, "reduce expects an array"))?;
    let src = args
        .get(1)
        .and_then(|v| v.as_str())
        .ok_or_else(|| ExprError::new(0, "reduce expects an expression string"))?;
    let expr = parse(src)?;
    let mut acc = args.get(2).cloned().unwrap_or(Value::Null);
    for el in arr {
        let ctx = json!({ "value": el, "acc": acc });
        acc = eval::eval(&expr, &ctx, 0)?;
    }
    Ok(acc)
}

fn sum(args: &[Value]) -> Result<Value, ExprError> {
    let arr = first_arr(args).ok_or_else(|| ExprError::new(0, "sum expects an array"))?;
    let field = field_arg(args);
    let mut acc = 0.0;
    for el in arr {
        if let Some(x) = field_of(el, &field).as_f64() {
            acc += x;
        }
    }
    Ok(Value::from(acc))
}

fn avg(args: &[Value]) -> Result<Value, ExprError> {
    let arr = first_arr(args).ok_or_else(|| ExprError::new(0, "avg expects an array"))?;
    let field = field_arg(args);
    let mut acc = 0.0;
    let mut n = 0;
    for el in arr {
        if let Some(x) = field_of(el, &field).as_f64() {
            acc += x;
            n += 1;
        }
    }
    if n == 0 {
        Ok(Value::Null)
    } else {
        Ok(Value::from(acc / n as f64))
    }
}

fn first(args: &[Value]) -> Result<Value, ExprError> {
    let arr = first_arr(args).ok_or_else(|| ExprError::new(0, "first expects an array"))?;
    Ok(arr.first().cloned().unwrap_or(Value::Null))
}

fn last(args: &[Value]) -> Result<Value, ExprError> {
    let arr = first_arr(args).ok_or_else(|| ExprError::new(0, "last expects an array"))?;
    Ok(arr.last().cloned().unwrap_or(Value::Null))
}

fn count(args: &[Value]) -> Result<Value, ExprError> {
    let arr = first_arr(args).ok_or_else(|| ExprError::new(0, "count expects an array"))?;
    Ok(Value::from(arr.len() as i64))
}

fn sort(args: &[Value]) -> Result<Value, ExprError> {
    let arr = first_arr(args).ok_or_else(|| ExprError::new(0, "sort expects an array"))?;
    let field = field_arg(args);
    let mut items: Vec<Value> = arr.iter().map(|el| field_of(el, &field)).collect();
    if items.iter().all(|v| v.is_number()) {
        items.sort_by(|a, b| {
            a.as_f64()
                .partial_cmp(&b.as_f64())
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    } else {
        items.sort_by(|a, b| {
            let sa = match a {
                Value::String(s) => s.clone(),
                _ => a.to_string(),
            };
            let sb = match b {
                Value::String(s) => s.clone(),
                _ => b.to_string(),
            };
            sa.cmp(&sb)
        });
    }
    Ok(Value::Array(items))
}

fn unique(args: &[Value]) -> Result<Value, ExprError> {
    let arr = first_arr(args).ok_or_else(|| ExprError::new(0, "unique expects an array"))?;
    let field = field_arg(args);
    let mut out: Vec<Value> = Vec::new();
    for el in arr {
        let k = field_of(el, &field);
        if !out.contains(&k) {
            out.push(k);
        }
    }
    Ok(Value::Array(out))
}

fn flatten(args: &[Value]) -> Result<Value, ExprError> {
    let arr = first_arr(args).ok_or_else(|| ExprError::new(0, "flatten expects an array"))?;
    let mut out = Vec::new();
    for el in arr {
        if let Value::Array(inner) = el {
            out.extend(inner.iter().cloned());
        } else {
            out.push(el.clone());
        }
    }
    Ok(Value::Array(out))
}

fn object(args: &[Value]) -> Result<Value, ExprError> {
    let mut map = Map::new();
    if args.len() == 1 {
        if let Value::Array(pairs) = &args[0] {
            for pair in pairs {
                if let Value::Array(p) = pair {
                    if let Some(k) = p.first().and_then(|v| v.as_str()) {
                        map.insert(k.to_string(), p.get(1).cloned().unwrap_or(Value::Null));
                    }
                }
            }
            return Ok(Value::Object(map));
        }
    }
    let mut it = args.iter();
    while let Some(k) = it.next() {
        let Some(v) = it.next() else { break };
        if let Value::String(ks) = k {
            map.insert(ks.clone(), v.clone());
        }
    }
    Ok(Value::Object(map))
}

fn keys(args: &[Value]) -> Result<Value, ExprError> {
    let obj = args
        .first()
        .and_then(|v| v.as_object())
        .ok_or_else(|| ExprError::new(0, "keys expects an object"))?;
    let ks: Vec<Value> = obj.keys().map(|k| Value::String(k.clone())).collect();
    Ok(Value::Array(ks))
}

fn values(args: &[Value]) -> Result<Value, ExprError> {
    let obj = args
        .first()
        .and_then(|v| v.as_object())
        .ok_or_else(|| ExprError::new(0, "values expects an object"))?;
    Ok(Value::Array(obj.values().cloned().collect()))
}

fn get(args: &[Value], data: &Value) -> Result<Value, ExprError> {
    let path = args
        .first()
        .and_then(|v| v.as_str())
        .ok_or_else(|| ExprError::new(0, "get expects a path string"))?;
    let v = crate::expr::get_path(data, path);
    if v.is_null() {
        Ok(args.get(1).cloned().unwrap_or(Value::Null))
    } else {
        Ok(v)
    }
}
