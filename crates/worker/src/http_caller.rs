//! Outbound HTTP for recipes (`$call`/`$notify`), jobs and webhooks.
//!
//! Implements [`engine::http::HttpCaller`] on `worker::Fetch` (replaces the
//! donor's reqwest blocking client). SSRF gating stays in the engine
//! (`valid_url`); response bodies are capped at 2 MiB like before. There is
//! no subrequest timeout knob on the edge — `timeout_ms` is accepted for API
//! compatibility and currently not enforced (Cloudflare aborts runaway
//! subrequests itself).

use async_trait::async_trait;
use engine::http::{HttpBody, HttpCaller};
use serde_json::Value as Json;
use worker::{Fetch, Headers, Method, Request, RequestInit};

pub struct FetchCaller;

impl Default for FetchCaller {
    fn default() -> Self {
        Self
    }
}

#[async_trait(?Send)]
impl HttpCaller for FetchCaller {
    async fn call(
        &self,
        url: &str,
        headers: &[(String, String)],
        body: &HttpBody,
        _timeout_ms: u64,
    ) -> anyhow::Result<(u16, Json)> {
        if !engine::webhooks::valid_url(url) {
            anyhow::bail!("ssrf-blocked url '{url}'");
        }
        let hs = Headers::new();
        for (k, v) in headers {
            let _ = hs.set(k.as_str(), v.as_str());
        }
        let js_body = match body {
            HttpBody::Json(b) => Some(worker::wasm_bindgen::JsValue::from_str(&b.to_string())),
            HttpBody::Raw { content_type, bytes } => {
                let _ = hs.set("content-type", content_type.as_str());
                Some(worker::wasm_bindgen::JsValue::from_str(
                    &String::from_utf8_lossy(bytes),
                ))
            }
        };
        // The engine egress contract is POST-only (no method on HttpBody).
        let mut init = RequestInit::new();
        init.with_method(Method::Post).with_headers(hs).with_body(js_body);
        let req = Request::new_with_init(url, &init)?;
        let mut resp = Fetch::Request(req).send().await?;
        let status = resp.status_code();
        let text = resp.text().await.unwrap_or_default();
        if text.len() > 2_000_000 {
            anyhow::bail!("response too large (>{} bytes)", 2_000_000);
        }
        let body = serde_json::from_str(&text).unwrap_or(Json::String(text));
        Ok((status, body))
    }
}
