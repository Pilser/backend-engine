use engine::http::{HttpBody, HttpCaller};
use serde_json::Value as Json;
use std::collections::HashMap;
use std::net::ToSocketAddrs;
use std::sync::Mutex;

/// reqwest caller that prefers IPv4 for outbound calls. On hosts where
/// getaddrinfo returns IPv6 first but there is no IPv6 internet, HTTPS calls
/// otherwise fail at connection. We build one cached client per unique host,
/// each pinned to the host's resolved IPv4 addresses (SNI is preserved by
/// reqwest because the URL keeps the hostname).
pub struct ReqwestCaller {
    clients: Mutex<HashMap<String, reqwest::blocking::Client>>,
}

fn host_of(url: &str) -> Option<String> {
    url.split('/').nth(2).map(|s| {
        let s = s.split('@').last().unwrap_or(s);
        let s = s.split(':').next().unwrap_or(s);
        s.to_string()
    })
}

fn ipv4_addrs(host: &str) -> Vec<std::net::SocketAddr> {
    (host, 443)
        .to_socket_addrs()
        .map(|it| it.filter(|a| a.is_ipv4()).collect())
        .unwrap_or_default()
}

impl ReqwestCaller {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self { clients: Mutex::new(HashMap::new()) })
    }

    fn client_for(&self, host: &str) -> reqwest::blocking::Client {
        let mut clients = self.clients.lock().unwrap();
        if let Some(c) = clients.get(host) {
            return c.clone();
        }
        let mut builder = reqwest::blocking::Client::builder();
        let ipv4s = ipv4_addrs(host);
        if !ipv4s.is_empty() {
            builder = builder.resolve_to_addrs(host, &ipv4s);
        }
        let client = builder.build().unwrap_or_else(|_| reqwest::blocking::Client::new());
        clients.insert(host.to_string(), client.clone());
        client
    }
}

impl HttpCaller for ReqwestCaller {
    fn call(
        &self,
        url: &str,
        headers: &[(String, String)],
        body: &HttpBody,
        timeout_ms: u64,
    ) -> anyhow::Result<(u16, Json)> {
        let host = host_of(url).unwrap_or_default();
        let client = self.client_for(&host);
        let mut req = client
            .post(url)
            .timeout(std::time::Duration::from_millis(timeout_ms));
        for (k, v) in headers {
            req = req.header(k, v);
        }
        match body {
            HttpBody::Json(j) => {
                if !j.is_null() {
                    req = req.header("content-type", "application/json");
                    req = req.body(j.to_string());
                }
            }
            HttpBody::Raw { content_type, bytes } => {
                req = req.header("content-type", content_type.as_str());
                req = req.body(bytes.clone());
            }
        }
        let resp = req.send()?;
        let status = resp.status().as_u16();
        let text = resp.text().unwrap_or_default();
        let parsed: Json = serde_json::from_str(&text)
            .unwrap_or_else(|_| if text.is_empty() { Json::Null } else { Json::String(text) });
        Ok((status, parsed))
    }
}
