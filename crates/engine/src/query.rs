use crate::model::{Record, TableConfig};
use crate::storage::database::{Database, Query, Row, TtlClause};
use crate::storage::ir::{Agg, FilterCond, Op, SrvFilter};
use crate::tables::TABLE_RECORDS;
use serde_json::Value as Json;

fn table_ttl_clause(cfg: &TableConfig) -> Option<TtlClause> {
    if cfg.ttl_seconds.is_none() && cfg.ttl_field.is_none() {
        return None; // no TTL: let the backend push down / cache the query
    }
    Some(TtlClause {
        seconds: cfg.ttl_seconds,
        field: cfg.ttl_field.clone(),
    })
}

fn to_record(row: Row) -> anyhow::Result<Record> {
    serde_json::from_value(row.data).map_err(Into::into)
}

pub async fn query_records(
    db: &dyn Database,
    table: &str,
    filter: &SrvFilter,
    orders: &[(String, bool)],
    limit: usize,
    offset: usize,
) -> anyhow::Result<Vec<Record>> {
    let cfg = crate::crud::load_table(db, table).await?;
    let limit = limit.clamp(1, 500);
    let mut conds = vec![crate::crud::table_cond(table)];
    conds.extend(filter.conds.clone());
    let q = Query {
        filter: SrvFilter { conds },
        orders: orders.to_vec(),
        limit,
        offset,
        ttl: table_ttl_clause(&cfg),
    };
    let mut recs = Vec::new();
    for row in db.query(TABLE_RECORDS, &q).await?.rows {
        let mut rec = to_record(row)?;
        rec.payload = crate::schema::apply_redact(&cfg, &rec.payload);
        recs.push(rec);
    }
    Ok(recs)
}

fn ci_replace_once(src: &str, needle: &str) -> String {
    let low_needle = needle.to_lowercase();
    let low_src = src.to_lowercase();
    let mut out = String::new();
    let mut from = 0;
    while let Some(rel) = low_src.get(from..).and_then(|s| s.find(&low_needle)) {
        let start = from + rel;
        let end = start + needle.len();
        out.push_str(&src[from..start]);
        out.push_str("<mark>");
        out.push_str(&src[start..end]);
        out.push_str("</mark>");
        from = end;
    }
    out.push_str(&src[from..]);
    out
}

fn highlight(text: &str, query: &str) -> String {
    let mut out = text.to_string();
    for token in query.split_whitespace() {
        if !token.is_empty() {
            out = ci_replace_once(&out, token);
        }
    }
    out
}

pub async fn search_records(
    db: &dyn Database,
    table: &str,
    query: &str,
    conds: &SrvFilter,
    limit: usize,
    offset: usize,
    snippet: bool,
) -> anyhow::Result<Vec<Record>> {
    let cfg = crate::crud::load_table(db, table).await?;
    let limit = limit.clamp(1, 500);
    let mut all = vec![crate::crud::table_cond(table)];
    all.push(FilterCond {
        field: "_".to_string(),
        op: Op::Search,
        value: Json::String(query.to_string()),
    });
    all.extend(conds.conds.clone());
    let q = Query {
        filter: SrvFilter { conds: all },
        orders: vec![],
        limit,
        offset,
        ttl: table_ttl_clause(&cfg),
    };
    let mut recs = Vec::new();
    for row in db.query(TABLE_RECORDS, &q).await?.rows {
        let mut rec = to_record(row)?;
        rec.payload = crate::schema::apply_redact(&cfg, &rec.payload);
        if snippet && !query.trim().is_empty() {
            let text = serde_json::to_string(&rec.payload).unwrap_or_default();
            rec.snippet = Some(highlight(&text, query));
        }
        recs.push(rec);
    }
    Ok(recs)
}

pub async fn aggregate_records(
    db: &dyn Database,
    table: &str,
    conds_orig: &SrvFilter,
    agg: Agg,
    field: Option<&str>,
    group_by: Option<&str>,
) -> anyhow::Result<Vec<Json>> {
    let cfg = crate::crud::load_table(db, table).await?;
    if agg != Agg::Count && field.is_none() {
        anyhow::bail!("field is required for {agg:?}");
    }
    db.aggregate(
        TABLE_RECORDS,
        crate::TENANT,
        table,
        conds_orig,
        agg,
        field,
        group_by,
        table_ttl_clause(&cfg),
    ).await
}