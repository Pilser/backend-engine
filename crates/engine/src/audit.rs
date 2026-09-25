use crate::model::Key;
use crate::storage::database::{Database, Query, Row};
use crate::storage::ir::{FilterCond, Op, SrvFilter};
use crate::tables::{TABLE_APPS, TABLE_AUDIT};
use serde_json::Value as Json;

pub fn audit_toggle(db: &mut dyn Database, board_id: &str, enabled: bool) -> anyhow::Result<()> {
    let mut board = crate::crud::load_board(db, board_id)?;
    board.audit = enabled;
    db.update(TABLE_APPS, &Key::text(board_id), &serde_json::to_value(&board)?)?;
    Ok(())
}

pub fn audit_list(
    db: &dyn Database,
    board_id: &str,
    since: Option<&str>,
    limit: usize,
) -> anyhow::Result<Vec<Json>> {
    let mut conds = vec![FilterCond {
        field: "$.board_id".to_string(),
        op: Op::Eq,
        value: Json::String(board_id.to_string()),
    }];
    if let Some(s) = since {
        if !s.is_empty() {
            conds.push(FilterCond { field: "$.ts".to_string(), op: Op::Gte, value: Json::String(s.to_string()) });
        }
    }
    let q = Query {
        filter: SrvFilter { conds },
        orders: vec![("$.ts".to_string(), true)],
        limit: limit.clamp(1, 1000),
        offset: 0,
    
        ttl: None,
    };
    let mut out = Vec::new();
    for row in db.query(TABLE_AUDIT, &q)?.rows {
        out.push(row.data);
    }
    Ok(out)
}

pub fn append(
    db: &mut dyn Database,
    board_id: &str,
    event: &str,
    actor: Option<&str>,
    writer: Option<&str>,
    payload_hash: Option<&str>,
) -> anyhow::Result<()> {
    let enabled = match crate::crud::app_by_id(db, board_id)? {
        Some(b) => b.audit,
        None => return Ok(()),
    };
    if !enabled {
        return Ok(());
    }
    let row = serde_json::json!({
        "board_id": board_id,
        "event": event,
        "actor": actor,
        "writer": writer,
        "payload_hash": payload_hash,
        "ts": crate::crud::now_str(),
    });
    db.insert(TABLE_AUDIT, Row::new(Key::text(board_id), row))?;
    Ok(())
}