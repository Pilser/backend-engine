use crate::model::{Key, TableConfig};
use crate::storage::database::Database;
use crate::storage::ir::{normalize_path, scalar_text};
use crate::tables::{tenant_key, TABLE_TABLES};
use serde_json::Value as Json;

pub fn prepare_payload(_db: &dyn Database, cfg: &TableConfig, payload: Json) -> anyhow::Result<Json> {
    let mut prepared = payload;
    apply_computed(cfg, &mut prepared)?;
    validate_rules(cfg, &prepared)?;
    validate_schema(cfg, &prepared)?;
    Ok(prepared)
}

pub fn apply_computed(cfg: &TableConfig, payload: &mut Json) -> anyhow::Result<()> {
    let Some(computed) = cfg.computed_json.as_ref() else {
        return Ok(());
    };
    let map = computed
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("computed must be an object of {{field: expr}}"))?;
    for (field, expr) in map {
        let expr = expr
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("computed field '{field}' must be a string expression"))?;
        let e = crate::expr::parse(expr)
            .map_err(|e| anyhow::anyhow!("computed field '{field}': {e}"))?;
        let value = crate::expr::apply(&e, payload)
            .map_err(|e| anyhow::anyhow!("computed field '{field}': {e}"))?;
        let path = if field.starts_with('$') {
            field.clone()
        } else {
            format!("$.{field}")
        };
        crate::expr::set_path(payload, &path, value)
            .map_err(|e| anyhow::anyhow!("computed field '{field}': {e}"))?;
    }
    Ok(())
}

pub fn validate_rules(cfg: &TableConfig, payload: &Json) -> anyhow::Result<()> {
    let Some(rules) = cfg.validate_json.as_ref() else {
        return Ok(());
    };
    let arr = rules
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("validate must be an array of {{when, error}}"))?;
    for rule in arr {
        let when = rule
            .get("when")
            .and_then(|w| w.as_str())
            .ok_or_else(|| anyhow::anyhow!("validate rule needs a \"when\" expression"))?;
        let msg = rule
            .get("error")
            .and_then(|e| e.as_str())
            .unwrap_or("validation failed")
            .to_string();
        let hit = crate::expr::truthy(when, payload)
            .map_err(|e| anyhow::anyhow!("validate 'when': {e}"))?;
        if hit {
            anyhow::bail!("{msg}");
        }
    }
    Ok(())
}

/// Lightweight JSON-Schema validator (allowlist subset, Draft 2020-12 shape).
///
/// The full `jsonschema` crate was removed: it is the biggest wasm binary-size
/// and CPU risk on the edge hot path, and single-tenant means the operator
/// controls all writers. Supported keywords (everything else is ignored):
/// `type` (string or array), `required`, `properties` (recursive),
/// `items` (single schema, applied to every element), `enum`,
/// `minimum`/`maximum` (numbers), `minLength`/`maxLength` (strings).
pub fn validate_schema(cfg: &TableConfig, payload: &Json) -> anyhow::Result<()> {
    let Some(schema) = cfg.schema_json.as_ref() else {
        return Ok(());
    };
    let Some(obj) = schema.as_object() else {
        anyhow::bail!("table '{}' has an invalid schema_json: schema must be an object", cfg.table);
    };
    check_node(obj, payload, "$").map_err(|e| anyhow::anyhow!("payload failed schema: {e}"))
}

fn check_node(schema: &serde_json::Map<String, Json>, value: &Json, path: &str) -> anyhow::Result<()> {
    if let Some(t) = schema.get("type") {
        let types: Vec<&str> = match t {
            Json::String(s) => vec![s.as_str()],
            Json::Array(arr) => arr.iter().filter_map(|v| v.as_str()).collect(),
            _ => vec![],
        };
        if !types.is_empty() && !types.iter().any(|t| type_matches(t, value)) {
            anyhow::bail!("{path}: expected type {} but found {}", types.join("|"), json_type(value));
        }
    }
    if let Some(Json::Array(items)) = schema.get("enum") {
        if !items.iter().any(|allowed| allowed == value) {
            anyhow::bail!("{path}: value is not one of the allowed enum values");
        }
    }
    match value {
        Json::Object(map) => {
            if let Some(Json::Array(required)) = schema.get("required") {
                for field in required.iter().filter_map(|f| f.as_str()) {
                    if !map.contains_key(field) {
                        anyhow::bail!("{path}: missing required field '{field}'");
                    }
                }
            }
            if let Some(Json::Object(props)) = schema.get("properties") {
                for (field, subschema) in props {
                    if let (Some(sub), Some(v)) = (subschema.as_object(), map.get(field)) {
                        check_node(sub, v, &format!("{path}.{field}"))?;
                    }
                }
            }
        }
        Json::Array(arr) => {
            if let Some(sub) = schema.get("items").and_then(|i| i.as_object()) {
                for (i, v) in arr.iter().enumerate() {
                    check_node(sub, v, &format!("{path}[{i}]"))?;
                }
            }
        }
        Json::String(s) => {
            if let Some(min) = schema.get("minLength").and_then(|v| v.as_u64()) {
                if (s.chars().count() as u64) < min {
                    anyhow::bail!("{path}: string shorter than minLength {min}");
                }
            }
            if let Some(max) = schema.get("maxLength").and_then(|v| v.as_u64()) {
                if (s.chars().count() as u64) > max {
                    anyhow::bail!("{path}: string longer than maxLength {max}");
                }
            }
        }
        Json::Number(n) => {
            let f = n.as_f64().unwrap_or(f64::NAN);
            if let Some(min) = schema.get("minimum").and_then(|v| v.as_f64()) {
                if f < min {
                    anyhow::bail!("{path}: {f} is less than minimum {min}");
                }
            }
            if let Some(max) = schema.get("maximum").and_then(|v| v.as_f64()) {
                if f > max {
                    anyhow::bail!("{path}: {f} is greater than maximum {max}");
                }
            }
        }
        _ => {}
    }
    Ok(())
}

fn type_matches(t: &str, value: &Json) -> bool {
    match t {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "number" => value.is_number(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        _ => true,
    }
}

fn json_type(value: &Json) -> &'static str {
    match value {
        Json::Null => "null",
        Json::Bool(_) => "boolean",
        Json::Number(_) => "number",
        Json::String(_) => "string",
        Json::Array(_) => "array",
        Json::Object(_) => "object",
    }
}

pub fn apply_redact(cfg: &TableConfig, payload: &Json) -> Json {
    let Some(redact) = cfg.redact_json.as_ref() else {
        return payload.clone();
    };
    let Some(paths) = redact.as_array() else {
        return payload.clone();
    };
    let mut out = payload.clone();
    for p in paths {
        let Some(path) = p.as_str() else {
            continue;
        };
        let segs = match crate::expr::path::parse_path(path) {
            Ok(s) => s,
            Err(_) => continue,
        };
        if segs.is_empty() {
            out = Json::String("***".to_string());
            continue;
        }
        crate::expr::path::assign(&mut out, &segs, Json::String("***".to_string())).ok();
    }
    out
}

async fn save_table_config(
    db: &mut dyn Database,
    table: &str,
    apply: impl FnOnce(&mut TableConfig),
) -> anyhow::Result<()> {
    let mut cfg = crate::crud::load_table(db, table).await?;
    apply(&mut cfg);
    db.update(TABLE_TABLES, &Key::text(tenant_key(table)), &serde_json::to_value(&cfg)?).await?;
    Ok(())
}

/// An explicit JSON null *clears* the knob (stores `None`); without this,
/// clearing would store `Some(null)` and fail every subsequent write.
fn opt_json(v: &Json) -> Option<Json> {
    if v.is_null() {
        None
    } else {
        Some(v.clone())
    }
}

pub async fn computed_set(
    db: &mut dyn Database,
    table: &str,
    map: &Json,
) -> anyhow::Result<()> {
    let map = opt_json(map);
    save_table_config(db, table, |c| c.computed_json = map).await
}

pub async fn validate_set(
    db: &mut dyn Database,
    table: &str,
    rules: &Json,
) -> anyhow::Result<()> {
    let rules = opt_json(rules);
    save_table_config(db, table, |c| c.validate_json = rules).await
}

/// Per-table access policy (S1: P0 proposals). Each knob: absent leaves the
/// current value, explicit null clears it (public_read→inherit tenant,
/// write_only→false), true/false sets it. Anything else is rejected.
pub async fn policy_set(
    db: &mut dyn Database,
    table: &str,
    public_read: Option<&Json>,
    write_only: Option<&Json>,
) -> anyhow::Result<()> {
    fn opt_bool(name: &str, v: &Json) -> anyhow::Result<Option<bool>> {
        if v.is_null() {
            Ok(None)
        } else if let Some(b) = v.as_bool() {
            Ok(Some(b))
        } else {
            anyhow::bail!("\"{name}\" must be true, false, or null")
        }
    }
    // None = untouched; Some(None) = clear; Some(Some(b)) = set.
    let pr = public_read.map(|v| opt_bool("public_read", v)).transpose()?;
    let wo = write_only.map(|v| opt_bool("write_only", v)).transpose()?;
    save_table_config(db, table, |c| {
        if let Some(v) = pr {
            c.public_read = v;
        }
        if let Some(v) = wo {
            c.write_only = v;
        }
    })
    .await
}

pub async fn redact_set(
    db: &mut dyn Database,
    table: &str,
    paths: &Json,
) -> anyhow::Result<()> {
    let paths = opt_json(paths);
    save_table_config(db, table, |c| c.redact_json = paths).await
}

pub async fn check_unique(
    db: &dyn Database,
    table_cfg: &TableConfig,
    payload: &Json,
    exclude_seq: Option<i64>,
) -> anyhow::Result<()> {
    let Some(key) = table_cfg.unique_key.as_deref() else {
        return Ok(());
    };
    let path = normalize_path(key);
    let value = crate::expr::get_path(payload, &path);
    if value.is_null() {
        return Ok(());
    }
    let text = scalar_text(&value);
    if let Some(existing) = crate::crud::find_unique_holder(
        db,
        &table_cfg.table,
        &path,
        &text,
        exclude_seq,
    ).await? {
        anyhow::bail!(
            "duplicate value '{text}' for unique field {key} (already record seq {existing})"
        );
    }
    Ok(())
}
