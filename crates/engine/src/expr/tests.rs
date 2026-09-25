use crate::expr::{evaluate, get_path, parse, set_path, truthy};
use serde_json::{json, Value};

fn num(v: Value) -> f64 {
    v.as_f64().expect("expected a number")
}

#[test]
fn parses_and_evaluates_arithmetic() {
    assert_eq!(num(evaluate("1 + 2 * 3", &json!({})).unwrap()), 7.0);
    assert_eq!(num(evaluate("(1 + 2) * 3", &json!({})).unwrap()), 9.0);
    assert_eq!(num(evaluate("10 % 3", &json!({})).unwrap()), 1.0);
    assert_eq!(num(evaluate("-5 + 2", &json!({})).unwrap()), -3.0);
}

#[test]
fn division_by_zero_is_null() {
    assert_eq!(evaluate("1 / 0", &json!({})).unwrap(), Value::Null);
    assert_eq!(evaluate("1 % 0", &json!({})).unwrap(), Value::Null);
}

#[test]
fn comparison_and_logic() {
    assert_eq!(evaluate("2 > 1", &json!({})).unwrap(), json!(true));
    assert_eq!(evaluate("2 <= 1", &json!({})).unwrap(), json!(false));
    assert_eq!(evaluate("true && false", &json!({})).unwrap(), json!(false));
    assert_eq!(evaluate("true || false", &json!({})).unwrap(), json!(true));
    assert_eq!(evaluate("!true", &json!({})).unwrap(), json!(false));
}

#[test]
fn ternary_is_right_associative() {
    assert_eq!(evaluate("true ? 1 : 0", &json!({})).unwrap(), json!(1.0));
    assert_eq!(evaluate("false ? 1 : 0", &json!({})).unwrap(), json!(0.0));
}

#[test]
fn resolves_paths() {
    let data = json!({ "a": { "b": [10, 20, 30] }, "items": [{ "x": 1 }, { "x": 2 }] });
    assert_eq!(get_path(&data, "$.a.b[1]"), json!(20));
    assert_eq!(get_path(&data, "$.a.missing"), Value::Null);
    assert_eq!(get_path(&data, "$"), data);
    let wild = get_path(&data, "$.items[*].x");
    assert_eq!(wild, json!([1, 2]));
}

#[test]
fn sets_paths() {
    let mut data = json!({ "a": {} });
    set_path(&mut data, "$.a.b[1]", json!(42)).unwrap();
    assert_eq!(data, json!({ "a": { "b": [null, 42] } }));
}

#[test]
fn string_functions() {
    assert_eq!(evaluate("lower('AbC')", &json!({})).unwrap(), json!("abc"));
    assert_eq!(evaluate("upper('AbC')", &json!({})).unwrap(), json!("ABC"));
    assert_eq!(evaluate("trim('  x  ')", &json!({})).unwrap(), json!("x"));
    assert_eq!(evaluate("len('hello')", &json!({})).unwrap(), json!(5));
    assert_eq!(evaluate("concat('a', 'b', 1)", &json!({})).unwrap(), json!("ab1"));
    assert_eq!(evaluate("contains('hello', 'ell')", &json!({})).unwrap(), json!(true));
    assert_eq!(evaluate("startswith('hello', 'he')", &json!({})).unwrap(), json!(true));
    assert_eq!(evaluate("endswith('hello', 'lo')", &json!({})).unwrap(), json!(true));
    assert_eq!(evaluate("join([1, 2, 3], '-')", &json!({})).unwrap(), json!("1-2-3"));
}

#[test]
fn numeric_functions() {
    assert_eq!(num(evaluate("num('42')", &json!({})).unwrap()), 42.0);
    assert_eq!(num(evaluate("round(3.14159, 2)", &json!({})).unwrap()), 3.14);
    assert_eq!(num(evaluate("floor(3.9)", &json!({})).unwrap()), 3.0);
    assert_eq!(num(evaluate("ceil(3.1)", &json!({})).unwrap()), 4.0);
    assert_eq!(num(evaluate("abs(-5)", &json!({})).unwrap()), 5.0);
    assert_eq!(num(evaluate("min(2, 3)", &json!({})).unwrap()), 2.0);
    assert_eq!(num(evaluate("max(2, 3)", &json!({})).unwrap()), 3.0);
}

#[test]
fn control_functions() {
    assert_eq!(evaluate("if(true, 'y', 'n')", &json!({})).unwrap(), json!("y"));
    assert_eq!(evaluate("coalesce(null, null, 'v')", &json!({})).unwrap(), json!("v"));
    assert_eq!(evaluate("coalesce(null)", &json!({})).unwrap(), Value::Null);
}

#[test]
fn extended_array_functions() {
    let data = json!({});
    assert_eq!(evaluate("map([1, 2, 3], '$ * 2')", &data).unwrap(), json!([2.0, 4.0, 6.0]));
    assert_eq!(evaluate("filter([1, 2, 3, 4], '$ > 2')", &data).unwrap(), json!([3, 4]));
    assert_eq!(evaluate("sum([1, 2, 3])", &data).unwrap(), json!(6.0));
    assert_eq!(evaluate("avg([1, 2, 3])", &data).unwrap(), json!(2.0));
    assert_eq!(evaluate("min([3, 1, 2])", &data).unwrap(), json!(1.0));
    assert_eq!(evaluate("max([3, 1, 2])", &data).unwrap(), json!(3.0));
    assert_eq!(evaluate("first([7, 8])", &data).unwrap(), json!(7));
    assert_eq!(evaluate("last([7, 8])", &data).unwrap(), json!(8));
    assert_eq!(evaluate("count([1, 2])", &data).unwrap(), json!(2));
    assert_eq!(evaluate("sort([3, 1, 2])", &data).unwrap(), json!([1, 2, 3]));
    assert_eq!(evaluate("unique([1, 1, 2])", &data).unwrap(), json!([1, 2]));
    assert_eq!(evaluate("flatten([1, [2, 3]])", &data).unwrap(), json!([1, 2, 3]));
    assert_eq!(evaluate("array(1, 2)", &data).unwrap(), json!([1, 2]));
    assert_eq!(evaluate("keys({'a': 1, 'b': 2})", &data).unwrap(), json!(["a", "b"]));
    assert_eq!(evaluate("values({'a': 1})", &data).unwrap(), json!([1]));
    assert_eq!(evaluate("get('$.a', 'def')", &json!({ "a": 5 })).unwrap(), json!(5));
    assert_eq!(evaluate("get('$.z', 'def')", &json!({ "a": 5 })).unwrap(), json!("def"));
}

#[test]
fn reduce_threads_accumulator() {
    let out = evaluate("reduce([1, 2, 3], '$.acc + $.value', 0)", &json!({})).unwrap();
    assert_eq!(num(out), 6.0);
}

#[test]
fn object_construction() {
    assert_eq!(evaluate("object('a', 1, 'b', 2)", &json!({})).unwrap(), json!({ "a": 1, "b": 2 }));
    assert_eq!(
        evaluate("object([['a', 1]])", &json!({})).unwrap(),
        json!({ "a": 1 })
    );
}

#[test]
fn truthiness_rules() {
    assert!(!truthy("0", &json!({})).unwrap());
    assert!(!truthy("''", &json!({})).unwrap());
    assert!(!truthy("null", &json!({})).unwrap());
    assert!(!truthy("false", &json!({})).unwrap());
    assert!(truthy("[]", &json!({})).unwrap());
    assert!(truthy("{}", &json!({})).unwrap());
    assert!(truthy("'x'", &json!({})).unwrap());
}

#[test]
fn never_panics_on_garbage() {
    let garbage = [
        "",
        "1 +",
        "(((",
        "$$$",
        "foo(1, 2",
        "unknownfunc(1)",
        "1 / 0",
        "[1,2]",
        "a.b.c",
        "'.'",
        "'unterminated",
        "1 + 2 @ 3",
    ];
    for g in garbage {
        let _ = evaluate(g, &json!({}));
        let _ = truthy(g, &json!({}));
        let _ = parse(g);
    }
}

#[test]
fn wildcard_reduction_over_nested() {
    let data = json!({ "orders": [{ "total": 10 }, { "total": 20 }] });
    let out = evaluate("sum($.orders[*].total)", &data).unwrap();
    assert_eq!(num(out), 30.0);
}
