//! JSON + CORS response helpers. All helpers are total (infallible) so route
//! handlers stay readable: fallible worker APIs on constant values cannot
//! realistically fail, and anything else is a programming error anyway.

use serde_json::{json, Value as Json};
use worker::{Headers, Response};

pub const ALLOW_HEADERS: &str =
    "Authorization, Content-Type, X-Filename, X-Hub-Signature-256, X-Srv-Key, X-Api-Key, X-Writer";
pub const ALLOW_METHODS: &str = "GET, POST, PUT, PATCH, DELETE, OPTIONS";

fn cors_headers() -> Headers {
    let h = Headers::new();
    let _ = h.set("access-control-allow-origin", "*");
    let _ = h.set("access-control-allow-headers", ALLOW_HEADERS);
    let _ = h.set("access-control-allow-methods", ALLOW_METHODS);
    h
}

/// Total JSON responder. `from_json` on a `serde_json::Value` cannot fail;
/// the unwraps below only fire on impossible platform failures.
pub fn json(status: u16, value: Json) -> Response {
    Response::from_json(&value)
        .map(|r| r.with_headers(cors_headers()).with_status(status))
        .unwrap_or_else(|_| Response::empty().unwrap().with_status(500))
}

pub fn ok(value: Json) -> Response {
    json(200, value)
}

pub fn created(value: Json) -> Response {
    json(201, value)
}

pub fn err(status: u16, msg: &str) -> Response {
    json(status, json!({ "error": msg }))
}

pub fn bad(e: &anyhow::Error) -> Response {
    err(400, &e.to_string())
}

pub fn srv(e: &anyhow::Error) -> Response {
    err(500, &e.to_string())
}

pub fn gone(msg: &str) -> Response {
    err(404, msg)
}

pub fn deny(msg: &str) -> Response {
    err(403, msg)
}

pub fn unauth(msg: &str) -> Response {
    err(401, msg)
}

pub fn preflight() -> Response {
    Response::empty().unwrap().with_headers(cors_headers()).with_status(204)
}

pub fn bytes(data: Vec<u8>, content_type: &str, cache: &str) -> Response {
    let h = cors_headers();
    let _ = h.set("content-type", content_type);
    let _ = h.set("cache-control", cache);
    Response::from_bytes(data)
        .map(|r| r.with_headers(h).with_status(200))
        .unwrap_or_else(|_| err(500, "body encode failed"))
}

/// Static-asset bytes with a validator. `etag` is the opaque blob hash
/// (memory sha256 / R2 http_etag); empty means "no validator available"
/// (assets uploaded before validators existed — re-upload to gain one).
pub fn asset_bytes(data: Vec<u8>, content_type: &str, cache: &str, etag: &str) -> Response {
    let h = cors_headers();
    let _ = h.set("content-type", content_type);
    let _ = h.set("cache-control", cache);
    // R2 http_etags arrive pre-quoted; memory shas are bare — normalize.
    let etag = etag.trim_matches('"');
    if !etag.is_empty() {
        let _ = h.set("etag", &format!("\"{etag}\""));
    }
    Response::from_bytes(data)
        .map(|r| r.with_headers(h).with_status(200))
        .unwrap_or_else(|_| err(500, "body encode failed"))
}

/// 304 for a validator match — carries no bytes (safe on private apps too).
pub fn not_modified(etag: &str) -> Response {
    let h = cors_headers();
    let etag = etag.trim_matches('"');
    if !etag.is_empty() {
        let _ = h.set("etag", &format!("\"{etag}\""));
    }
    Response::empty().unwrap().with_headers(h).with_status(304)
}

/// True when the request's If-None-Match allows a 304 for this validator.
/// Handles `*`, single and list forms, weak (`W/`) prefixes, quoting.
pub fn etag_matches(req: &worker::Request, etag: &str) -> bool {
    let etag = etag.trim_matches('"');
    if etag.is_empty() {
        return false;
    }
    let inm = req
        .headers()
        .get("if-none-match")
        .ok()
        .flatten()
        .unwrap_or_default();
    let inm = inm.trim();
    if inm.is_empty() {
        return false;
    }
    if inm == "*" {
        return true;
    }
    let quoted = format!("\"{etag}\"");
    inm.split(',').any(|t| {
        let t = t.trim();
        t == quoted || t == etag || t.strip_prefix("W/").map(|w| w == quoted || w == etag).unwrap_or(false)
    })
}

pub fn redirect_to(location: &str) -> Response {
    redirect_to_status(location, 308)
}

/// Redirect with a configured status (site routes). Unknown codes fall
/// back to 302; only 301/302/303/307/308 are honored.
pub fn redirect_to_status(location: &str, status: u16) -> Response {
    let status = match status {
        301 | 302 | 303 | 307 | 308 => status,
        _ => 302,
    };
    let h = cors_headers();
    let _ = h.set("location", location);
    Response::empty().unwrap().with_headers(h).with_status(status)
}
