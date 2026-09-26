//! Same-origin reverse proxy for the embedded AI assistant frontend.
//!
//! Ported 1:1 from the donor daemon's `transport/ai_proxy.rs` (single tenant).
//! The AI app is served by its own service; the target base URL is the
//! `AI_BASE_URL` tenant secret (fallback: `AI_TARGET` wrangler var — the old
//! `SRV_AI_TARGET` env was renamed per workers env rules, same role).
//!   HTTP: /srv/ai/<rest> -> <target>/<rest>  (any method; HTML+JS rewritten)
//!   WS:   /srv/ai/ws     -> <target>/ws      (framed bridge, see below)
//!   WS:   /ws (legacy)   -> <target>/ws      (the AI bundle builds its socket
//!     URL from location.host + "/ws", so the host root is kept)
//!
//! The rewrite rules, header filter, handshake bytes and status rules below
//! are byte-identical to the donor. Two deliberate improvements: https
//! upstreams actually work now (TLS via SecureTransport — the donor's raw TCP
//! could only speak ws://), and bare hostnames default to :80/:443 (the donor
//! required an explicit port).
//!
//! PLATFORM ADAPTATION (faithful behavior, different mechanism): the donor
//! tunneled raw TCP bytes both ways (hyper upgrade + TcpStream +
//! copy_bidirectional). Workers has no raw inbound sockets, so the bridge is
//! framed: browser messages arrive via the WebSocket API and are encoded as
//! unmasked WS frames onto the upstream TCP socket, while upstream frames are
//! parsed back into messages (auto-pong, fragmentation reassembly, close
//! propagation). Driven via `Context::wait_until`; the 101 to the browser is
//! completed by the runtime from the returned `WebSocketPair` client.
//! Like the donor, these routes carry no engine auth (same-origin frontend
//! traffic; the AI service authenticates itself).

use futures_util::{select, FutureExt, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use worker::{
    Context, Env, Fetch, Headers, Method, Request, RequestInit, Response, Result,
    SecureTransport, Socket, WebsocketEvent, WebSocketPair,
};

use crate::{cors, d1_db::D1Db};

/// Resolve the AI backend base URL: the `AI_BASE_URL` tenant secret
/// (case-insensitive, set via the secrets endpoint), falling back to the
/// `AI_TARGET` wrangler var. No hardcoded default.
async fn ai_target(db: &D1Db, ai_var: Option<String>) -> Option<String> {
    if let Ok(map) = engine::secrets::secrets_map(db).await {
        if let Some((_, v)) = map
            .into_iter()
            .find(|(k, v)| k.eq_ignore_ascii_case("AI_BASE_URL") && !v.is_empty())
        {
            return Some(v);
        }
    }
    ai_var.filter(|s| !s.is_empty())
}

fn proxy_header_ok(name: &str) -> bool {
    !matches!(
        name,
        "host" | "connection"
            | "upgrade"
            | "content-length"
            | "transfer-encoding"
            | "accept-encoding"
            | "cookie"
    )
}

fn is_javascript(ct: &str) -> bool {
    let ct = ct.to_ascii_lowercase();
    ct.contains("javascript") || ct.contains("ecmascript") || ct == "text/x-js"
}

/// Rewrite the absolute asset roots the AI SPA emits (`/_topcoat/`, `/assets/`,
/// `/favicon.svg`, root `href="/"`/`src="/"`) to the engine's mount so they
/// resolve through the proxy, and inject a `<base href="/srv/ai/">` so
/// base-relative URLs (e.g. `?asset=...`, `?api=...`, `?ws=1`) the AI app emits
/// resolve against the mount instead of the host root. Single-pass scanner:
/// overlapping prefixes are consumed together to avoid double-rewriting.
fn rewrite_html(data: &[u8]) -> String {
    const PREFIX: &str = "/srv/ai";
    let base_tag = format!("<base href=\"{PREFIX}/\">");
    let html = String::from_utf8_lossy(data);
    let mut out = String::with_capacity(html.len() + 128);
    // Strip any <base href="..."> the upstream emitted (e.g. our own layout
    // emits <base href="/">), then inject the mount-scoped base.
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
                out.push_str(PREFIX);
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
            out.push_str(&format!("href=\"{PREFIX}/\""));
            rest = &rest["href=\"/\"".len()..];
            continue;
        }
        if rest.starts_with("src=\"/\"") {
            out.push_str(&format!("src=\"{PREFIX}/\""));
            rest = &rest["src=\"/\"".len()..];
            continue;
        }
        let ch = rest.chars().next().unwrap();
        out.push(ch);
        rest = &rest[ch.len_utf8()..];
    }
    out
}

/// Remove an existing `<base ...>` tag so the injected mount-scoped base wins.
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

/// Rewrite the AI app's backend-call literals to the mount so they resolve
/// through the engine instead of the origin root.
///
/// The AI app's WS URL is derived from the document <base href> (already
/// injected mount-scoped by rewrite_html), so a bare "/ws" literal is NOT
/// rewritten here — doing so double-prefixes paths our client builds from the
/// base (e.g. base + "/ws").
fn rewrite_js(data: &[u8]) -> String {
    const PREFIX: &str = "/srv/ai";
    String::from_utf8_lossy(data)
        .replace("\"/api/", &format!("\"{PREFIX}/api/"))
        .replace("location.origin + \"/w/\"", &format!("location.origin + \"{PREFIX}/w/\""))
}

fn method_of(req: &Request) -> Result<Method, Response> {
    match req.method() {
        Method::Get => Ok(Method::Get),
        Method::Post => Ok(Method::Post),
        Method::Put => Ok(Method::Put),
        Method::Patch => Ok(Method::Patch),
        Method::Delete => Ok(Method::Delete),
        Method::Head => Ok(Method::Head),
        Method::Options => Ok(Method::Options),
        _ => Err(cors::err(400, "unsupported method for AI proxy")),
    }
}

/// HTTP half of the proxy. Forwards any method with its body, query string and
/// selected headers; HTML/JS are buffered and rewritten to the mount,
/// everything else (SSE, images, binaries) streams through untouched.
pub async fn http_proxy(
    db: D1Db,
    ai_var: Option<String>,
    method: Method,
    query: &str,
    req_headers: &Headers,
    body_bytes: Vec<u8>,
    rest: &str,
) -> Result<Response> {
    let Some(target) = ai_target(&db, ai_var).await else {
        return Ok(cors::err(
            502,
            "no AI_BASE_URL configured (set it via the secrets endpoint)",
        ));
    };
    let url = format!("{}/{rest}{query}", target.trim_end_matches('/'));
    let hs = Headers::new();
    for name in req_headers.keys() {
        if !proxy_header_ok(&name.to_ascii_lowercase()) {
            continue;
        }
        if let Ok(Some(v)) = req_headers.get(&name) {
            let _ = hs.set(&name, &v);
        }
    }
    let js_body = if body_bytes.is_empty() {
        None
    } else {
        Some(worker::js_sys::Uint8Array::from(body_bytes.as_slice()).into())
    };
    let mut init = RequestInit::new();
    init.with_method(method).with_headers(hs).with_body(js_body);
    let out_req = Request::new_with_init(&url, &init)?;
    let upstream = match Fetch::Request(out_req).send().await {
        Ok(r) => r,
        Err(e) => {
            return Ok(cors::err(502, &format!("upstream {target} unreachable: {e}")));
        }
    };
    let status = upstream.status_code();
    let content_type = upstream
        .headers()
        .get("content-type")
        .ok()
        .flatten()
        .unwrap_or_else(|| "application/octet-stream".to_string());
    let encoding = upstream
        .headers()
        .get("content-encoding")
        .ok()
        .flatten()
        .unwrap_or_default();
    let rewritable = encoding.is_empty() || encoding.eq_ignore_ascii_case("identity");
    if rewritable && (content_type.starts_with("text/html") || is_javascript(&content_type)) {
        let mut upstream = upstream;
        let data = match upstream.bytes().await {
            Ok(b) => b,
            Err(e) => return Ok(cors::err(502, &format!("upstream read failed: {e}"))),
        };
        let out = if content_type.starts_with("text/html") {
            rewrite_html(&data).into_bytes()
        } else {
            rewrite_js(&data).into_bytes()
        };
        let h = cors_headers_for(&content_type, false);
        return Ok(Response::from_bytes(out)?.with_headers(h).with_status(status));
    }
    let (_builder, body) = upstream.into_parts();
    let h = cors_headers_for(&content_type, true);
    Ok(Response::from_body(body)?.with_headers(h).with_status(status))
}

fn cors_headers_for(content_type: &str, streamed: bool) -> Headers {
    let h = Headers::new();
    let _ = h.set("access-control-allow-origin", "*");
    let _ = h.set("access-control-allow-headers", crate::cors::ALLOW_HEADERS);
    let _ = h.set("access-control-allow-methods", crate::cors::ALLOW_METHODS);
    let _ = h.set("content-type", content_type);
    let _ = h.set("cache-control", "no-cache");
    if streamed {
        let _ = h.set("x-accel-buffering", "no");
    }
    h
}

// ---- WebSocket half -------------------------------------------------------

const WS_MAX_MESSAGE: usize = 16 * 1024 * 1024;

enum UpMsg {
    Text(String),
    Binary(Vec<u8>),
    Ping(Vec<u8>),
    Close(u16, String),
}

fn parse_target(target: &str) -> Option<(String, u16, bool)> {
    let (rest, tls) = if let Some(r) = target.strip_prefix("https://") {
        (r, true)
    } else if let Some(r) = target.strip_prefix("http://") {
        (r, false)
    } else {
        return None;
    };
    let host_port = rest.split('/').next()?;
    if host_port.is_empty() {
        return None;
    }
    if let Some((host, port)) = host_port.rsplit_once(':') {
        if host.is_empty() {
            return None;
        }
        Some((host.to_string(), port.parse().ok()?, tls))
    } else {
        Some((host_port.to_string(), if tls { 443 } else { 80 }, tls))
    }
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|p| p + 4)
}

/// Parse complete WS frames from the front of `buf` (servers must not mask,
/// but masked frames are tolerated). Fragmentation state lives in `frag`.
fn drain_frames(
    buf: &mut Vec<u8>,
    frag: &mut Option<(u8, Vec<u8>)>,
) -> Result<Vec<UpMsg>, String> {
    let mut out = Vec::new();
    loop {
        if buf.len() < 2 {
            break;
        }
        let b0 = buf[0];
        let b1 = buf[1];
        let fin = b0 & 0x80 != 0;
        let opcode = b0 & 0x0f;
        let masked = b1 & 0x80 != 0;
        let mut len = (b1 & 0x7f) as u64;
        let mut hdr = 2usize;
        if len == 126 {
            if buf.len() < 4 {
                break;
            }
            len = u16::from_be_bytes([buf[2], buf[3]]) as u64;
            hdr = 4;
        } else if len == 127 {
            if buf.len() < 10 {
                break;
            }
            let mut arr = [0u8; 8];
            arr.copy_from_slice(&buf[2..10]);
            len = u64::from_be_bytes(arr);
            hdr = 10;
        }
        if len > WS_MAX_MESSAGE as u64 {
            return Err("ws frame too large".to_string());
        }
        let mut key = [0u8; 4];
        if masked {
            if buf.len() < hdr + 4 {
                break;
            }
            key.copy_from_slice(&buf[hdr..hdr + 4]);
            hdr += 4;
        }
        if buf.len() - hdr < len as usize {
            break;
        }
        let mut payload = buf[hdr..hdr + len as usize].to_vec();
        buf.drain(..hdr + len as usize);
        if masked {
            for (i, b) in payload.iter_mut().enumerate() {
                *b ^= key[i % 4];
            }
        }
        match opcode {
            0x0 => {
                let Some((_, acc)) = frag else {
                    return Err("stray continuation frame".to_string());
                };
                acc.extend_from_slice(&payload);
                if fin {
                    let (op, data) = frag.take().unwrap();
                    out.push(data_msg(op, data)?);
                }
            }
            0x1 | 0x2 => {
                if frag.is_some() {
                    return Err("interleaved data frame".to_string());
                }
                if fin {
                    out.push(data_msg(opcode, payload)?);
                } else {
                    *frag = Some((opcode, payload));
                }
            }
            0x8 => {
                if !fin || len > 125 {
                    return Err("bad close frame".to_string());
                }
                let (code, reason) = if payload.len() >= 2 {
                    let c = u16::from_be_bytes([payload[0], payload[1]]);
                    let r = String::from_utf8_lossy(&payload[2..]).to_string();
                    (c, r)
                } else {
                    (1005, String::new())
                };
                out.push(UpMsg::Close(code, reason));
            }
            0x9 => {
                if !fin || len > 125 {
                    return Err("bad ping frame".to_string());
                }
                out.push(UpMsg::Ping(payload));
            }
            0xA => {}
            _ => return Err(format!("unknown opcode {opcode:#x}")),
        }
    }
    Ok(out)
}

fn data_msg(opcode: u8, data: Vec<u8>) -> Result<UpMsg, String> {
    match opcode {
        0x1 => Ok(UpMsg::Text(String::from_utf8_lossy(&data).to_string())),
        0x2 => Ok(UpMsg::Binary(data)),
        _ => Err("bad fragmented opcode".to_string()),
    }
}

fn encode_frame(opcode: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(2 + payload.len());
    out.push(0x80 | opcode);
    if payload.len() < 126 {
        out.push(payload.len() as u8);
    } else if payload.len() < 65536 {
        out.push(126);
        out.extend_from_slice(&(payload.len() as u16).to_be_bytes());
    } else {
        out.push(127);
        out.extend_from_slice(&(payload.len() as u64).to_be_bytes());
    }
    out.extend_from_slice(payload);
    out
}

fn encode_close(code: u16, reason: &str) -> Vec<u8> {
    if code == 1005 || code == 1006 {
        return encode_frame(0x8, &[]);
    }
    let mut payload = code.to_be_bytes().to_vec();
    payload.extend_from_slice(reason.as_bytes());
    encode_frame(0x8, &payload)
}

/// WebSocket half of the proxy. Relays the handshake to the AI service and
/// bridges framed messages both ways (see module docs for why it is framed
/// rather than a raw byte tunnel like the donor).
pub async fn ws_proxy(
    db: D1Db,
    ai_var: Option<String>,
    req: &Request,
    rest: &str,
    ctx: &Context,
) -> Result<Response> {
    let Some(target) = ai_target(&db, ai_var).await else {
        return Ok(cors::err(502, "no AI base URL configured (set AI_BASE_URL via secrets)"));
    };
    let Some((host, port, tls)) = parse_target(&target) else {
        return Ok(cors::err(502, "invalid AI base URL"));
    };
    let sec_key = req
        .headers()
        .get("sec-websocket-key")
        .ok()
        .flatten()
        .unwrap_or_default();
    if sec_key.is_empty() {
        return Ok(cors::err(400, "missing sec-websocket-key"));
    }
    let subproto = req.headers().get("sec-websocket-protocol").ok().flatten();

    let mut handshake = format!(
        "GET /{rest} HTTP/1.1\r\nHost: {host}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Key: {sec_key}\r\nSec-WebSocket-Version: 13\r\n"
    );
    if let Some(p) = &subproto {
        handshake.push_str(&format!("Sec-WebSocket-Protocol: {p}\r\n"));
    }
    handshake.push_str("\r\n");

    let transport = if tls { SecureTransport::On } else { SecureTransport::Off };
    let mut upstream = match Socket::builder().secure_transport(transport).connect(host.clone(), port) {
        Ok(s) => s,
        Err(e) => {
            return Ok(cors::err(502, &format!("upstream {host} unreachable: {e}")));
        }
    };
    if upstream.write_all(handshake.as_bytes()).await.is_err() {
        return Ok(cors::err(502, "upstream handshake write failed"));
    }

    let mut buf = Vec::with_capacity(1024);
    let mut tmp = [0u8; 2048];
    let header_end = loop {
        match upstream.read(&mut tmp).await {
            Ok(0) => return Ok(cors::err(502, "upstream closed before handshake")),
            Ok(n) => buf.extend_from_slice(&tmp[..n]),
            Err(e) => return Ok(cors::err(502, &format!("upstream read failed: {e}"))),
        }
        if let Some(pos) = find_header_end(&buf) {
            break pos;
        }
        if buf.len() > 64 * 1024 {
            return Ok(cors::err(400, "upstream handshake headers too large"));
        }
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
    if !head.starts_with("HTTP/1.1 101") {
        let first = head.lines().next().unwrap_or("").to_string();
        return Ok(cors::err(502, &format!("upstream refused upgrade: {first}")));
    }
    let leftover = buf[header_end..].to_vec();

    let pair = WebSocketPair::new()?;
    pair.server.accept()?;
    let server = pair.server.clone();
    // Split so the read pump and the browser-event pump own disjoint halves.
    let (mut rd, mut wr) = tokio::io::split(upstream);
    let bridge = async move {
        let mut tcp_buf = leftover;
        let mut frag: Option<(u8, Vec<u8>)> = None;
        let mut tmp = [0u8; 8192];
        let mut events = match server.events() {
            Ok(e) => e,
            Err(_) => return,
        };
        loop {
            select! {
                r = rd.read(&mut tmp).fuse() => {
                    let n = match r {
                        Ok(0) | Err(_) => {
                            let _ = server.close(None::<u16>, None::<&str>);
                            break;
                        }
                        Ok(n) => n,
                    };
                    tcp_buf.extend_from_slice(&tmp[..n]);
                    let msgs = match drain_frames(&mut tcp_buf, &mut frag) {
                        Ok(m) => m,
                        Err(_) => {
                            let _ = server.close(None::<u16>, None::<&str>);
                            break;
                        }
                    };
                    for msg in msgs {
                        match msg {
                            UpMsg::Text(t) => {
                                if server.send_with_str(&t).is_err() {
                                    return;
                                }
                            }
                            UpMsg::Binary(b) => {
                                if server.send_with_bytes(&b).is_err() {
                                    return;
                                }
                            }
                            UpMsg::Ping(p) => {
                                if wr.write_all(&encode_frame(0xA, &p)).await.is_err() {
                                    return;
                                }
                            }
                            UpMsg::Close(code, reason) => {
                                let _ = server.close(Some(code), Some(reason.as_str()));
                                return;
                            }
                        }
                    }
                },
                ev = events.next().fuse() => {
                    match ev {
                        Some(Ok(WebsocketEvent::Message(m))) => {
                            let frame = if let Some(t) = m.text() {
                                encode_frame(0x1, t.as_bytes())
                            } else if let Some(b) = m.bytes() {
                                encode_frame(0x2, &b)
                            } else {
                                continue;
                            };
                            if wr.write_all(&frame).await.is_err() {
                                return;
                            }
                        }
                        _ => {
                            // Browser closed, errored, or stream ended:
                            // propagate a clean close upstream and stop.
                            let _ = wr.write_all(&encode_close(1000, "")).await;
                            return;
                        }
                    }
                }
            }
        }
    };
    ctx.wait_until(bridge);
    Ok(Response::from_websocket(pair.client)?)
}

/// True for the AI mount (`/srv/ai`, `/srv/ai/…`) and the legacy host-root
/// `/ws`. Checked in the fetch handler before the main router; note the
/// sub-app slug `ai` is therefore reserved.
pub fn is_ai_route(req: &Request) -> bool {
    let path = req.path();
    path == "/ws" || path == "/srv/ai" || path.starts_with("/srv/ai/")
}

pub fn is_upgrade(req: &Request) -> bool {
    req.headers()
        .get("upgrade")
        .ok()
        .flatten()
        .map(|v| v.eq_ignore_ascii_case("websocket"))
        .unwrap_or(false)
}

/// AI entry point (called from the fetch handler before the main router):
/// the `/srv/ai/` mount and the legacy host-root `/ws`.
pub async fn serve(mut req: Request, env: &Env, ctx: &Context) -> Result<Response> {
    let path = req.path();
    if req.method() == Method::Options {
        return Ok(cors::preflight());
    }
    if path == "/srv/ai" {
        return Ok(cors::redirect_to("/srv/ai/"));
    }
    let ai_var = env.var("AI_TARGET").ok().map(|v| v.to_string());
    let get_db = || {
        env.d1("DB")
            .map(D1Db::new)
            .map_err(|_| cors::err(500, "missing D1 binding DB"))
    };
    if path == "/ws" {
        if !is_upgrade(&req) {
            return Ok(cors::err(404, "not found"));
        }
        let db = match get_db() {
            Ok(db) => db,
            Err(r) => return Ok(r),
        };
        return ws_proxy(db, ai_var, &req, "ws", ctx).await;
    }
    let Some(rest) = path.strip_prefix("/srv/ai/") else {
        return Ok(cors::err(404, "not found"));
    };
    if is_upgrade(&req) {
        let db = match get_db() {
            Ok(db) => db,
            Err(r) => return Ok(r),
        };
        return ws_proxy(db, ai_var, &req, rest, ctx).await;
    }
    let method = match method_of(&req) {
        Ok(m) => m,
        Err(r) => return Ok(r),
    };
    let query = req
        .url()
        .ok()
        .and_then(|u| u.query().map(|q| format!("?{q}")))
        .unwrap_or_default();
    let headers = req.headers().clone();
    let body = req.bytes().await.unwrap_or_default();
    let db = match get_db() {
        Ok(db) => db,
        Err(r) => return Ok(r),
    };
    http_proxy(db, ai_var, method, &query, &headers, body, rest).await
}
