use crate::transport::rest::{board_of, err_json, ok_json, require_admin, BoxBodyResp};
use crate::Server;
use engine::policy::RateLimits;
use hyper::{Response, StatusCode};
use serde_json::{json, Value};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Cache for the expensive record-count scan, shared across connections.
struct CountCache {
    board: String,
    records: i64,
    at: Instant,
}

pub struct ResourceCache {
    count: Mutex<Option<CountCache>>,
}

impl ResourceCache {
    pub fn new() -> Self {
        Self { count: Mutex::new(None) }
    }

    /// Record-count scan result, reused for 60s. Returns None when the caller
    /// should recompute (cold or stale) — callers set it via `store_count`.
    fn take(&self, board: &str) -> Option<i64> {
        let g = self.count.lock().unwrap();
        match g.as_ref() {
            Some(c) if c.board == board && c.at.elapsed() < Duration::from_secs(60) => Some(c.records),
            _ => None,
        }
    }

    fn store(&self, board: &str, records: i64) {
        let mut g = self.count.lock().unwrap();
        *g = Some(CountCache { board: board.to_string(), records, at: Instant::now() });
    }
}

pub fn resource_report_cached(server: &Server, board: &str) -> Value {
    let records = match server.obs.resource_cache.take(board) {
        Some(n) => n,
        None => {
            let n = count_records(server, board);
            server.obs.resource_cache.store(board, n);
            n
        }
    };
    report_json(server, board, records)
}

pub fn resource_report(server: &Server, board: &str) -> Value {
    report_json(server, board, count_records(server, board))
}

/// Total record count for a board via the backend's fast path (one Helix
/// aggregate) when available. The count is cached by the caller for 60s.
fn count_records(server: &Server, board: &str) -> i64 {
    let engine = server.engine.lock().unwrap();
    engine.count_records_board(board).unwrap_or(0)
}

fn report_json(server: &Server, board: &str, records: i64) -> Value {
    // NOTE: never call board_of() while holding the engine lock below — the
    // engine Mutex is not reentrant and the second lock() on the same thread
    // deadlocks. Fetch the board first.
    let rate: RateLimits = server.rate_limits_for(&board_of(server, board));
    let (storage, usage, now) = {
        let engine = server.engine.lock().unwrap();
        let mut storage: u64 = 0;
        if let Ok(keys) = engine.object_store().list(&format!("{board}/files/")) {
            storage += keys.iter().map(|k| k.size).sum::<u64>();
        }
        if let Ok(keys) = engine.object_store().list(&format!("{board}/assets/")) {
            storage += keys.iter().map(|k| k.size).sum::<u64>();
        }
        let usage = server.limiter.lock().unwrap().snapshot(board);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        (storage, usage, now)
    };

    json!({
        "ok": true,
        "timestamp": now,
        "board": board,
        "records": {
            "count": records,
        },
        "storage": {
            "bytes": storage,
            "kib": (storage / 1024),
        },
        "rate": {
            "limits": {
                "submit": rate.submit,
                "upload": rate.upload,
                "search": rate.search,
                "read": rate.read,
                "per_day": rate.per_day,
            },
            "usage": usage,
        },
        "system": {
            "in_flight": server.obs.in_flight(),
            "requests_total": server.obs.total(),
            "cpu_percent": (server.obs.cpu_percent() * 100.0).round() / 100.0,
            "mem_kb": server.obs.mem_kb(),
        },
    })
}

pub fn resources(server: &Server, board: &str, principal: &engine::Principal) -> Response<BoxBodyResp> {
    if !require_admin(principal) {
        return err_json(StatusCode::FORBIDDEN, "admin authorization required");
    }
    ok_json(resource_report_cached(server, board))
}
