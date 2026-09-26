use crate::model::Key;
use crate::storage::database::{Database, Query, Row};
use crate::storage::ir::{FilterCond, Op, SrvFilter};
use crate::tables::TABLE_AUDIT;
use serde_json::Value as Json;

pub async fn audit_toggle(db: &mut dyn Database, enabled: bool) -> anyhow::Result<()> {
    let mut board = crate::crud::tenant_config(db).await?;
    board.audit = enabled;
    crate::crud::save_tenant(db, &board).await
}

pub async fn audit_list(
    db: &dyn Database,
    since: Option<&str>,
    limit: usize,
) -> anyhow::Result<Vec<Json>> {
    let mut conds = Vec::new();
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
    for row in db.query(TABLE_AUDIT, &q).await?.rows {
        out.push(row.data);
    }
    Ok(out)
}

pub async fn append(
    db: &mut dyn Database,
    event: &str,
    actor: Option<&str>,
    writer: Option<&str>,
    payload_hash: Option<&str>,
) -> anyhow::Result<()> {
    if !crate::crud::tenant_config(db).await?.audit {
        return Ok(());
    }
    let row = serde_json::json!({
        "event": event,
        "actor": actor,
        "writer": writer,
        "payload_hash": payload_hash,
        "ts": crate::crud::now_str(),
    });
    db.insert(TABLE_AUDIT, Row::new(Key::text(crate::TENANT), row)).await?;
    Ok(())
}