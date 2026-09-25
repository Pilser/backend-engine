use crate::{Error, Result};
use serde_json::{json, Value as Json};

/// POST /v2/query transport. Sends one direct JSON envelope; parses the
/// normalized response (one top-level member per returned batch name).
#[derive(Debug, Clone)]
pub struct Client {
    base: String,
    http: reqwest::blocking::Client,
    timeout_ms: u64,
}

impl Client {
    pub fn new(base_url: impl Into<String>) -> Result<Self> {
        let base = base_url.into().trim_end_matches('/').to_string();
        // Keep-alive enabled: the daemon runs blocking calls on the tokio
        // blocking pool (spawn_blocking), so reusing a connection per host is
        // safe and avoids a TCP handshake + TLS per request (which made
        // dashboard bursts slow). reqwest re-establishes on server close.
        let http = reqwest::blocking::Client::builder()
            .pool_idle_timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(Error::Http)?;
        Ok(Self { base, http, timeout_ms: 30_000 })
    }

    pub fn with_timeout(mut self, ms: u64) -> Self {
        self.timeout_ms = ms;
        self
    }

    /// Execute a raw envelope (used by the typed builders via [`Self::execute`]).
    pub fn post(&self, request_type: &str, query_name: &str, body: Json) -> Result<Response> {
        let envelope = json!({
            "request_type": request_type,
            "query_name": query_name,
            "query": body,
        });
        self.execute(envelope)
    }

    /// Send an envelope built by the request module. Returns the parsed
    /// response object: `{ name: [rows...] }`.
    ///
    /// Retry policy: only CONNECT-phase failures are retried (server briefly
    /// down / restarting). Timeouts and mid-flight severances are NOT retried:
    /// under load a heavy scan would otherwise be re-run up to 3x, tripling
    /// DB pressure exactly during congestion (IICO freeze, 2026-08-22).
    pub fn execute(&self, envelope: Json) -> Result<Response> {
        let url = format!("{}/v2/query", self.base);
        let mut last_err: Option<Error> = None;
        for attempt in 0..3 {
            match self.try_execute(&url, &envelope) {
                Ok(r) => return Ok(r),
                Err(e) => {
                    let retryable = matches!(&e, Error::Http(h) if is_connect_error(h));
                    if retryable {
                        eprintln!(
                            "[helix] retry {}/3 (connect) after error: {e}",
                            attempt + 1
                        );
                        std::thread::sleep(std::time::Duration::from_millis(
                            100 * (attempt + 1),
                        ));
                    }
                    last_err = Some(e);
                    if !retryable {
                        break;
                    }
                }
            }
        }
        Err(last_err.unwrap_or_else(|| Error::Other("request failed".into())))
    }

    fn try_execute(&self, url: &str, envelope: &Json) -> Result<Response> {
        // Debug: when HELIX_DEBUG=1, print every outgoing envelope (truncated)
        // so a hanging/rejected request can be reproduced exactly against Helix.
        if std::env::var("HELIX_DEBUG").as_deref() == Ok("1") {
            let debug_body = serde_json::to_string(envelope).unwrap_or_default();
            let shown = if debug_body.len() > 2000 { &debug_body[..2000] } else { &debug_body };
            eprintln!("[helix] {url} <- {shown}");
        }
        let t0 = std::time::Instant::now();
        let send = self
            .http
            .post(url)
            .timeout(std::time::Duration::from_millis(self.timeout_ms))
            .json(envelope)
            .send();
        if std::env::var("HELIX_DEBUG").as_deref() == Ok("1") {
            match &send {
                Ok(_) => eprintln!("[helix] {url} took {}ms", t0.elapsed().as_millis()),
                Err(e) => eprintln!("[helix] {url} FAILED after {}ms: {e}", t0.elapsed().as_millis()),
            }
        }
        let resp = send.map_err(Error::Http)?;
        let status = resp.status();
        let text = resp.text().map_err(Error::Http)?;
        if !status.is_success() {
            return Err(Error::Status { status: status.as_u16(), body: text });
        }
        let value: Json = serde_json::from_str(&text)
            .map_err(|e| Error::InvalidResponse(format!("bad json: {e}: {text}")))?;
        Ok(Response::new(value))
    }
}

/// True only for failures while ESTABLISHING the connection (refused, DNS,
/// unreachable). Timeouts and mid-flight severances return false so heavy
/// queries are never re-executed.
fn is_connect_error(e: &reqwest::Error) -> bool {
    e.is_connect()
}

/// Parsed normalized response: one member per returned batch name.
#[derive(Debug, Clone)]
pub struct Response {
    pub value: Json,
}

impl Response {
    pub fn new(value: Json) -> Self {
        Self { value }
    }

    /// Rows for a returned batch name.
    pub fn rows(&self, name: &str) -> Vec<Json> {
        self.value
            .get(name)
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default()
    }

    /// Raw value for a returned batch name (e.g. a count scalar).
    pub fn get(&self, name: &str) -> Option<&Json> {
        self.value.get(name)
    }
}
