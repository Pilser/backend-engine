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

pub fn redirect_to(location: &str) -> Response {
    let h = cors_headers();
    let _ = h.set("location", location);
    Response::empty().unwrap().with_headers(h).with_status(308)
}
