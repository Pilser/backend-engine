pub mod ast;
pub mod eval;
pub mod extend;
pub mod funcs;
pub mod parser;
pub mod path;

pub use crate::expr::ast::{Expr, ExprError, MAX_ARGS, MAX_DEPTH, MAX_PARSE_STEPS};
use serde_json::Value;

pub fn parse(src: &str) -> Result<Expr, ExprError> {
    parser::Parser::new(src)?.parse()
}

pub fn evaluate(src: &str, data: &Value) -> Result<Value, ExprError> {
    let expr = parse(src)?;
    eval::eval(&expr, data, 0)
}

pub fn apply(expr: &Expr, data: &Value) -> Result<Value, ExprError> {
    eval::eval(expr, data, 0)
}

pub fn truthy(src: &str, data: &Value) -> Result<bool, ExprError> {
    let v = evaluate(src, data)?;
    Ok(funcs::truthy(&v))
}

pub fn get_path(data: &Value, path: &str) -> Value {
    match path::parse_path(path) {
        Ok(segs) => eval::resolve_path(data, &segs),
        Err(_) => Value::Null,
    }
}

pub fn set_path(data: &mut Value, path: &str, value: Value) -> Result<(), ExprError> {
    let segs = path::parse_path(path)?;
    path::assign(data, &segs, value)
}

pub fn search_match(payload: &Value, query: &str) -> bool {
    let q = query.trim();
    if q.is_empty() {
        return false;
    }
    let hay = serde_json::to_string(payload).unwrap_or_default().to_lowercase();
    q.split_whitespace().all(|tok| hay.contains(&tok.to_lowercase()))
}

#[cfg(test)]
mod tests;
