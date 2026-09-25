//! Same-origin reverse proxy for the embedded AI assistant frontend.
//!
//! The AI app is served by its own service; the target base URL is a per-board
//! setting stored through the MCP secrets endpoint as `AI_BASE_URL` (fallback:
//! `SRV_AI_TARGET` env var; no hardcoded default). The IICO frontend embeds it
//! inside a card, and proxying through the engine keeps it same-origin so the
//! app's state/cookies stay shared:
//!   HTTP: /srv/<board>/ai/<rest> -> <target>/<rest>  (any method; HTML+JS rewritten)
//!   WS:   /srv/<board>/ai/ws     -> <target>/ws      (raw tunnel after handshake)
//!   WS:   /ws (legacy root)      -> <target>/ws      (env target only)
//!
//! The AI app builds its socket URL from `location.host + "/ws"`, so the JS
//! bundles it serves are rewritten to the board-scoped mount; the rewritten
//! client then connects back to `/srv/<board>/ai/ws` and we tunnel raw bytes.

use crate::transport::rest::{err_json, with_cors, BoxBodyResp};
use bytes::Bytes;
use engine::ServerlessEngine;
use futures_util::Stream;
use http::header::{CONNECTION, SEC_WEBSOCKET_ACCEPT, SEC_WEBSOCKET_PROTOCOL, UPGRADE};
use http::{HeaderMap, HeaderValue, Method, Request, Response, StatusCode};
use http_body::Frame;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper_util::rt::TokioIo;
use std::pin::Pin;
use std::sync::{Arc, Mutex, OnceLock};
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::io::{copy_bidirectional, AsyncReadExt, AsyncWriteExt};

/// Resolve the AI backend base URL for a board: the board's `AI_BASE_URL`
/// secret (set via the MCP secrets endpoint), falling back to `SRV_AI_TARGET`.
/// The secret lookup takes the engine lock and hits the blocking Helix client,
/// so it runs on the blocking thread pool.
async fn ai_target(
    engine: &Arc<Mutex<ServerlessEngine>>,
    board: Option<&str>,
) -> Option<String> {
    if let Some(board) = board {
        let engine = engine.clone();
        let board = board.to_string();
        let found = tokio::task::spawn_blocking(move || {
            engine
                .lock()
                .ok()
                .and_then(|e| e.secrets_map(&board).ok())
                .and_then(|map| {
                    map.into_iter()
                        .find(|(k, v)| k.eq_ignore_ascii_case("AI_BASE_URL") && !v.is_empty())
                        .map(|(_, v)| v)
                })
        })
        .await
        .unwrap_or(None);
        if found.is_some() {
            return found;
        }
    }
    std::env::var("SRV_AI_TARGET").ok().filter(|s| !s.is_empty())
}

fn client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .read_timeout(Duration::from_secs(300))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new())
    })
}

fn proxy_header_ok(name: &http::HeaderName) -> bool {
    !(name == http::header::HOST
        || name == http::header::CONNECTION
        || name == http::header::UPGRADE
        || name == http::header::CONTENT_LENGTH
        || name == http::header::TRANSFER_ENCODING
        || name == http::header::ACCEPT_ENCODING
        || name == http::header::COOKIE)
}

fn is_javascript(ct: &str) -> bool {
    let ct = ct.to_ascii_lowercase();
    ct.contains("javascript") || ct.contains("ecmascript") || ct == "text/x-js"
}

/// Rewrite the absolute asset roots the AI SPA emits (`/_topcoat/`, `/assets/`,
/// `/favicon.svg`, root `href="/"`) to the engine's board-scoped mount so they
/// resolve through the proxy, and inject a `<base href="/srv/<board>/ai/">` so
/// base-relative URLs (e.g. `?asset=...`, `?api=...`, `?ws=1`) the AI app emits
/// resolve against the board mount instead of the host root. Single-pass
/// scanner: overlapping prefixes are consumed together to avoid double-rewriting.
fn rewrite_html(data: &[u8], board: &str) -> String {
    let prefix = format!("/srv/{board}/ai");
    let base_tag = format!("<base href=\"{prefix}/\">");
    let html = String::from_utf8_lossy(data);
    let mut out = String::with_capacity(html.len() + 128);
    // Strip any <base href="..."> the upstream emitted (e.g. our own layout
    // emits <base href="/">), then inject the board-scoped base.
    let stripped = strip_base_tag(&html);
    let mut rest: &str = stripped.as_str();
    let mut head_done = false;
    while !rest.is_empty() {
        // Inject the base right after the opening <head ...> tag.
        if !head_done {
            if let Some(hi) = rest.to_ascii_lowercase().find("<head") {
                if let Some(hi2) = rest[hi..].find('>') {
                    let end = hi + hi2 + 1;
                    out.push_str(&rest[..end]);
                    out.push_str(&base_tag);
                    rest = &rest[end..];
                    head_done = true;
                    continue;
                }
            }
            head_done = true;
        }
        let mut matched = false;
        for (pat, rel) in [("/_topcoat/", "_topcoat/"), ("/assets/", "assets/"), ("/favicon.svg", "favicon.svg")] {
            if rest.starts_with(pat) {
                out.push_str(&prefix);
                out.push('/');
                out.push_str(rel);
                rest = &rest[pat.len()..];
                matched = true;
                break;
            }
        }
        if matched {
            continue;
        }
        if rest.starts_with("href=\"/\"") {
            out.push_str(&format!("href=\"{prefix}/\"" ));
            rest = &rest["href=\"/\"".len()..];
            continue;
        }
        if rest.starts_with("src=\"/\"") {
            out.push_str(&format!("src=\"{prefix}/\"" ));
            rest = &rest["src=\"/\"".len()..];
            continue;
        }
        let ch = rest.chars().next().unwrap();
        out.push(ch);
        rest = &rest[ch.len_utf8()..];
    }
    out
}

/// Remove an existing `<base ...>` tag so the injected board-scoped base wins.
fn strip_base_tag(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let mut out = String::with_capacity(html.len());
    let mut offset = 0usize;
    while let Some(pos) = lower[offset..].find("<base") {
        let abs = offset + pos;
        if let Some(rel_end) = html[abs..].find('>') {
            let end = abs + rel_end + 1;
            out.push_str(&html[offset..abs]);
            offset = end;
        } else {
            break;
        }
    }
    if offset == 0 {
        html.to_string()
    } else {
        out.push_str(&html[offset..]);
        out
    }
}

/// Rewrite the AI app's backend-call literals to the board-scoped mount so they
/// resolve through the engine instead of the origin root.
///
/// The AI app's WS URL is derived from the document <base href> (already
/// injected board-scoped by rewrite_html), so a bare "/ws" literal is NOT
/// rewritten here — doing so double-prefixes paths our client builds from the
/// base (e.g. base + "/ws").
fn rewrite_js(data: &[u8], board: &str) -> String {
    let prefix = format!("/srv/{board}/ai");
    String::from_utf8_lossy(data)
        .replace("\"/api/", &format!("\"{prefix}/api/"))
        .replace("location.origin + \"/w/\"", &format!("location.origin + \"{prefix}/w/\""))
}

/// Streaming body adapter that pumps a reqwest byte stream (used for SSE and
/// binary responses that must not be buffered).
struct ProxyStream<S>(S);

impl<S> ProxyStream<S> {
    fn new(inner: S) -> Self {
        Self(inner)
    }
}

impl<S> http_body::Body for ProxyStream<S>
where
    S: Stream<Item = Result<Bytes, reqwest::Error>> + Unpin + Send,
{
    type Data = Bytes;
    type Error = std::io::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.get_mut();
        match Pin::new(&mut this.0).poll_next(cx) {
            Poll::Ready(Some(Ok(b))) => Poll::Ready(Some(Ok(Frame::data(b)))),
            Poll::Ready(Some(Err(e))) => Poll::Ready(Some(Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                e,
            )))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

/// HTTP half of the proxy. Forwards any method with its body, query string and
/// selected headers; HTML/JS are buffered and rewritten to the board mount,
/// everything else (SSE, images, binaries) streams through untouched.
pub async fn http_proxy(
    engine: &Arc<Mutex<ServerlessEngine>>,
    board: &str,
    method: &Method,
    rest: &str,
    qs: &str,
    req_headers: &HeaderMap,
    body: &Bytes,
) -> Response<BoxBodyResp> {
    let Some(target) = ai_target(engine, Some(board)).await else {
        return err_json(
            StatusCode::BAD_GATEWAY,
            "no AI_BASE_URL configured for this board (set it via the secrets endpoint)",
        );
    };
    let url = format!("{}/{}{}", target.trim_end_matches('/'), rest, qs);
    let mut rq = client().request(method.clone(), &url).body(body.clone());
    for (name, value) in req_headers {
        if proxy_header_ok(name) {
            if let Ok(v) = value.to_str() {
                rq = rq.header(name, v);
            }
        }
    }
    let upstream = match rq.send().await {
        Ok(r) => r,
        Err(e) => return err_json(StatusCode::BAD_GATEWAY, &format!("upstream {target} unreachable: {e}")),
    };
    let status = upstream.status();
    let content_type = upstream
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string();
    let encoding = upstream
        .headers()
        .get(reqwest::header::CONTENT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let rewritable = encoding.is_empty() || encoding.eq_ignore_ascii_case("identity");
    if rewritable && (content_type.starts_with("text/html") || is_javascript(&content_type)) {
        let data = match upstream.bytes().await {
            Ok(b) => b.to_vec(),
            Err(e) => return err_json(StatusCode::BAD_GATEWAY, &format!("upstream read failed: {e}")),
        };
        let out = if content_type.starts_with("text/html") {
            rewrite_html(&data, board).into_bytes()
        } else {
            rewrite_js(&data, board).into_bytes()
        };
        let full = Full::new(Bytes::from(out)).map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e));
        return with_cors(
            Response::builder()
                .status(status)
                .header("content-type", content_type)
                .header("cache-control", "no-cache")
                .body(BoxBody::new(full))
                .unwrap(),
        );
    }
    let stream = ProxyStream::new(upstream.bytes_stream());
    with_cors(
        Response::builder()
            .status(status)
            .header("content-type", content_type)
            .header("cache-control", "no-cache")
            .header("x-accel-buffering", "no")
            .body(BoxBody::new(stream))
            .unwrap(),
    )
}

fn parse_target(target: &str) -> Option<(String, bool)> {
    let rest = target.strip_prefix("http://").or_else(|| target.strip_prefix("https://"))?;
    let host_port = rest.split('/').next()?;
    if host_port.is_empty() {
        return None;
    }
    Some((host_port.to_string(), true))
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|p| p + 4)
}

fn extract_header(head: &str, name: &str) -> Option<String> {
    head.lines()
        .find_map(|l| {
            let (k, v) = l.split_once(':')?;
            if k.trim().eq_ignore_ascii_case(name) {
                Some(v.trim().to_string())
            } else {
                None
            }
        })
}

/// WebSocket half of the proxy. Relays the client's upgrade handshake to the AI
/// service, forwards its 101 (with the upstream's computed Sec-WebSocket-Accept)
/// back to the client, then tunnels raw bytes in both directions. `board` is
/// `None` for the legacy root `/ws` route.
pub async fn ws_proxy(
    engine: &Arc<Mutex<ServerlessEngine>>,
    board: Option<&str>,
    req: Request<Incoming>,
    rest: &str,
) -> Response<BoxBodyResp> {
    let Some(target) = ai_target(engine, board).await else {
        return err_json(
            StatusCode::BAD_GATEWAY,
            "no AI base URL configured (set AI_BASE_URL via secrets)",
        );
    };
    let Some((host, _)) = parse_target(&target) else {
        return err_json(StatusCode::BAD_GATEWAY, "invalid AI base URL");
    };
    let sec_key = req
        .headers()
        .get("sec-websocket-key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    if sec_key.is_empty() {
        return err_json(StatusCode::BAD_REQUEST, "missing sec-websocket-key");
    }
    let subproto = req
        .headers()
        .get(SEC_WEBSOCKET_PROTOCOL)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let mut handshake = format!(
        "GET /{rest} HTTP/1.1\r\nHost: {host}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Key: {sec_key}\r\nSec-WebSocket-Version: 13\r\n"
    );
    if let Some(p) = &subproto {
        handshake.push_str(&format!("Sec-WebSocket-Protocol: {p}\r\n"));
    }
    handshake.push_str("\r\n");

    let mut upstream = match tokio::net::TcpStream::connect(&host).await {
        Ok(s) => s,
        Err(e) => return err_json(StatusCode::BAD_GATEWAY, &format!("upstream {host} unreachable: {e}")),
    };
    if upstream.write_all(handshake.as_bytes()).await.is_err() {
        return err_json(StatusCode::BAD_GATEWAY, "upstream handshake write failed");
    }

    let mut buf = Vec::with_capacity(1024);
    let mut tmp = [0u8; 2048];
    let header_end = loop {
        match upstream.read(&mut tmp).await {
            Ok(0) => return err_json(StatusCode::BAD_GATEWAY, "upstream closed before handshake"),
            Ok(n) => buf.extend_from_slice(&tmp[..n]),
            Err(e) => return err_json(StatusCode::BAD_GATEWAY, &format!("upstream read failed: {e}")),
        }
        if let Some(pos) = find_header_end(&buf) {
            break pos;
        }
        if buf.len() > 64 * 1024 {
            return err_json(StatusCode::BAD_REQUEST, "upstream handshake headers too large");
        }
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
    if !head.starts_with("HTTP/1.1 101") {
        let first = head.lines().next().unwrap_or("").to_string();
        return err_json(StatusCode::BAD_GATEWAY, &format!("upstream refused upgrade: {first}"));
    }
    let accept = extract_header(&head, "sec-websocket-accept").unwrap_or_default();
    let leftover = buf[header_end..].to_vec();

    let on_upgrade = hyper::upgrade::on(req);
    let empty = Full::new(Bytes::new()).map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e));
    let mut resp = Response::builder()
        .status(StatusCode::SWITCHING_PROTOCOLS)
        .header(CONNECTION, HeaderValue::from_static("upgrade"))
        .header(UPGRADE, HeaderValue::from_static("websocket"))
        .header(
            SEC_WEBSOCKET_ACCEPT,
            HeaderValue::from_str(&accept).unwrap_or_else(|_| HeaderValue::from_static("")),
        )
        .body(BoxBody::new(empty))
        .unwrap();
    if let Some(p) = subproto {
        if let Ok(v) = HeaderValue::from_str(&p) {
            resp.headers_mut().insert(SEC_WEBSOCKET_PROTOCOL, v);
        }
    }

    tokio::spawn(async move {
        match on_upgrade.await {
            Ok(upgraded) => {
                let mut client = TokioIo::new(upgraded);
                let mut upstream = upstream;
                if !leftover.is_empty() && client.write_all(&leftover).await.is_err() {
                    return;
                }
                let _ = copy_bidirectional(&mut client, &mut upstream).await;
            }
            Err(e) => tracing::warn!("ai ws upgrade failed: {e}"),
        }
    });

    resp
}