use async_trait::async_trait;
use serde_json::{json, Value as Json};
use std::sync::OnceLock;

pub enum HttpBody {
    Json(Json),
    Raw { content_type: String, bytes: Vec<u8> },
}

#[async_trait(?Send)]
pub trait HttpCaller: Send + Sync + 'static {
    async fn call(
        &self,
        url: &str,
        headers: &[(String, String)],
        body: &HttpBody,
        timeout_ms: u64,
    ) -> anyhow::Result<(u16, Json)>;
}

pub struct StubCaller;

#[async_trait(?Send)]
impl HttpCaller for StubCaller {
    async fn call(
        &self,
        _url: &str,
        _headers: &[(String, String)],
        _body: &HttpBody,
        _timeout_ms: u64,
    ) -> anyhow::Result<(u16, Json)> {
        Ok((200, json!({"ok": true})))
    }
}

static CALLER: OnceLock<Box<dyn HttpCaller>> = OnceLock::new();

pub fn install(caller: Box<dyn HttpCaller>) {
    let _ = CALLER.set(caller);
}

pub async fn http_call(
    url: &str,
    headers: &[(String, String)],
    body: &Json,
    timeout_ms: u64,
) -> anyhow::Result<(u16, Json)> {
    http_call_body(url, headers, &HttpBody::Json(body.clone()), timeout_ms).await
}

pub async fn http_call_body(
    url: &str,
    headers: &[(String, String)],
    body: &HttpBody,
    timeout_ms: u64,
) -> anyhow::Result<(u16, Json)> {
    match CALLER.get() {
        Some(c) => c.call(url, headers, body, timeout_ms).await,
        None => StubCaller.call(url, headers, body, timeout_ms).await,
    }
}