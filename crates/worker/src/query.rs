//! Query-string + body parsing helpers (donor parity with
//! `server::transport::rest::{query_params, parse_filter_param, orders_from,
//! records_json, url_decode}` — minus hyper types).

use engine::model::Record;
use engine::storage::ir::SrvFilter;
use serde_json::{json, Value as Json};
use std::collections::HashMap;
use worker::Request;

/// Parsed query params. Values come from `Url::query_pairs`, which already
/// percent-decodes (`+` → space), matching the donor's `url_decode`.
pub fn params(req: &Request) -> HashMap<String, String> {
    let mut map = HashMap::new();
    if let Ok(url) = req.url() {
        for (k, v) in url.query_pairs() {
            map.insert(k.into_owned(), v.into_owned());
        }
    }
    map
}

pub fn parse_filter_param(raw: &str) -> anyhow::Result<SrvFilter> {
    let parsed: Json =
        serde_json::from_str(raw).map_err(|_| anyhow::anyhow!("invalid filter json"))?;
    engine::parse_filter(&parsed)
}

pub fn filter_from(params: &HashMap<String, String>) -> anyhow::Result<SrvFilter> {
    match params.get("filter") {
        Some(f) if !f.is_empty() => parse_filter_param(f),
        _ => Ok(SrvFilter::new()),
    }
}

pub fn orders_from(params: &HashMap<String, String>) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    if let Some(o) = params.get("order") {
        let dir = match params.get("dir").map(|d| d.as_str()) {
            Some("asc") => false,
            Some("desc") => true,
            _ => false,
        };
        for p in o.split(',') {
            if !p.is_empty() {
                out.push((p.to_string(), dir));
            }
        }
    }
    out
}

pub fn records_json(records: &[Record]) -> Vec<Json> {
    records
        .iter()
        .map(|r| {
            let mut v = json!({ "seq": r.seq, "payload": r.payload, "created_at": r.created_at });
            if let Some(score) = r.score {
                v["score"] = json!(score);
            }
            if let Some(w) = &r.writer {
                v["writer"] = json!(w);
            }
            if let Some(s) = &r.snippet {
                v["snippet"] = json!(s);
            }
            v
        })
        .collect()
}

pub fn limit(params: &HashMap<String, String>, key: &str, default: usize, max: usize) -> usize {
    params.get(key).and_then(|l| l.parse().ok()).unwrap_or(default).clamp(1, max)
}

pub fn offset(params: &HashMap<String, String>) -> usize {
    params.get("offset").and_then(|v| v.parse().ok()).unwrap_or(0)
}

pub fn is_true(params: &HashMap<String, String>, key: &str) -> bool {
    params.get(key).map(|v| matches!(v.as_str(), "1" | "true" | "on")).unwrap_or(false)
}
