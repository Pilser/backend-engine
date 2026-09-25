use crate::model::{Key, TableConfig};
use crate::storage::database::Database;
use crate::storage::ir::{normalize_path, scalar_text};
use crate::tables::{scoped_key, TABLE_TABLES};
use serde_json::Value as Json;

pub fn prepare_payload(_db: &dyn Database, board: &TableConfig, payload: Json) -> anyhow::Result<Json> {
    let mut prepared = payload;
    apply_computed(board, &mut prepared)?;
    validate_rules(board, &prepared)?;
    validate_schema(board, &prepared)?;
    Ok(prepared)
}

pub fn apply_computed(board: &TableConfig, payload: &mut Json) -> anyhow::Result<()> {
    let Some(computed) = board.computed_json.as_ref() else {
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

pub fn validate_rules(board: &TableConfig, payload: &Json) -> anyhow::Result<()> {
    let Some(rules) = board.validate_json.as_ref() else {
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

pub fn validate_schema(board: &TableConfig, payload: &Json) -> anyhow::Result<()> {
    let Some(schema) = board.schema_json.as_ref() else {
        return Ok(());
    };
    let compiled = jsonschema::options_for::<jsonschema::json::SerdeJson>()
        .should_validate_formats(true)
        .build(schema)
        .map_err(|e| anyhow::anyhow!("board {} has an invalid schema_json: {e}", board.board_id))?;
    match compiled.validate(payload) {
        Ok(()) => Ok(()),
        Err(e) => anyhow::bail!("payload failed schema: {e}"),
    }
}

pub fn apply_redact(board: &TableConfig, payload: &Json) -> Json {
    let Some(redact) = board.redact_json.as_ref() else {
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

fn save_table_config(
    db: &mut dyn Database,
    board_id: &str,
    table: &str,
    apply: impl FnOnce(&mut TableConfig),
) -> anyhow::Result<()> {
    let mut cfg = crate::crud::load_table(db, board_id, table)?;
    apply(&mut cfg);
    db.update(TABLE_TABLES, &Key::text(scoped_key(board_id, table)), &serde_json::to_value(&cfg)?)?;
    Ok(())
}

pub fn computed_set(
    db: &mut dyn Database,
    board_id: &str,
    table: &str,
    map: &Json,
) -> anyhow::Result<()> {
    save_table_config(db, board_id, table, |c| c.computed_json = Some(map.clone()))
}

pub fn validate_set(
    db: &mut dyn Database,
    board_id: &str,
    table: &str,
    rules: &Json,
) -> anyhow::Result<()> {
    save_table_config(db, board_id, table, |c| c.validate_json = Some(rules.clone()))
}

pub fn redact_set(
    db: &mut dyn Database,
    board_id: &str,
    table: &str,
    paths: &Json,
) -> anyhow::Result<()> {
    save_table_config(db, board_id, table, |c| c.redact_json = Some(paths.clone()))
}

pub fn check_unique(
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
        &table_cfg.board_id,
        &table_cfg.table,
        &path,
        &text,
        exclude_seq,
    )? {
        anyhow::bail!(
            "duplicate value '{text}' for unique field {key} (already record seq {existing})"
        );
    }
    Ok(())
}
