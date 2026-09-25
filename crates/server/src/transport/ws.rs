use crate::transport::rest::{require_admin, require_read, BoxBodyResp};
use crate::Server;
use engine::model::Record;
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use http::header::{CONNECTION, SEC_WEBSOCKET_ACCEPT, SEC_WEBSOCKET_PROTOCOL, UPGRADE};
use http::{HeaderValue, Request, Response, StatusCode};
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper_util::rt::TokioIo;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;
use tokio::time::interval;
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
use tokio_tungstenite::tungstenite::protocol::Role;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;

pub fn is_ws(req: &Request<Incoming>) -> bool {
    if req.method() != http::Method::GET {
        return false;
    }
    let segs: Vec<&str> = req
        .uri()
        .path()
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    if segs.len() != 4 || segs[0] != "api" || segs[1] != "srv" || segs[3] != "ws" {
        return false;
    }
    req.headers()
        .get(UPGRADE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.eq_ignore_ascii_case("websocket"))
        .unwrap_or(false)
}

fn token_of(req: &Request<Incoming>) -> Option<String> {
    if let Some(auth) = req.headers().get("authorization") {
        if let Ok(s) = auth.to_str() {
            for prefix in ["Bearer ", "bearer "] {
                if let Some(t) = s.strip_prefix(prefix) {
                    return Some(t.to_string());
                }
            }
        }
    }
    if let Some(proto) = req.headers().get(SEC_WEBSOCKET_PROTOCOL) {
        if let Ok(s) = proto.to_str() {
            let mut parts = s.split(',');
            if let Some(first) = parts.next() {
                if first.trim() == "bearer" {
                    if let Some(t) = parts.next() {
                        return Some(t.trim().to_string());
                    }
                } else if !first.trim().is_empty() {
                    return Some(first.trim().to_string());
                }
            }
        }
    }
    if let Some(q) = req.uri().query() {
        for pair in q.split('&') {
            if let Some((k, v)) = pair.split_once('=') {
                if k == "key" && !v.is_empty() {
                    return Some(crate::transport::rest::url_decode(v));
                }
            }
        }
    }
    None
}

pub fn ws_upgrade(
    server: Arc<Server>,
    req: Request<Incoming>,
) -> Response<BoxBodyResp> {
    let board = {
        let segs: Vec<&str> = req.uri().path().split('/').filter(|s| !s.is_empty()).collect();
        segs[2].to_string()
    };
    let params = crate::transport::rest::query_params(req.uri());
    let after: i64 = params.get("after").and_then(|v| v.parse().ok()).unwrap_or(0);
    let filter = params.get("filter").map(|f| crate::transport::rest::url_decode(f)).filter(|f| !f.is_empty());
    let want_resources = params.get("resources").map(|v| v == "1" || v == "true").unwrap_or(false);

    let token = token_of(&req);
    let principal = server
        .engine
        .lock()
        .unwrap()
        .resolve_principal(&board, token.as_deref(), None)
        .unwrap_or_else(|_| engine::model::Principal {
            id: "anon".to_string(),
            role: "none".to_string(),
            scope: None,
            writer: None,
        });
    let allowed = server
        .engine
        .lock()
        .unwrap()
        .get_app(&board)
        .ok()
        .flatten()
        .map(|app| app.public_reads || require_read(&principal))
        .unwrap_or(false);

    let sec_key = req
        .headers()
        .get("sec-websocket-key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let accept = derive_accept_key(sec_key.as_bytes());
    let offered = req
        .headers()
        .get("sec-websocket-protocol")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.split(',').next())
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty());
    let on_upgrade = hyper::upgrade::on(req);

    let empty = Full::new(Bytes::new())
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e));
    let mut resp = Response::builder()
        .status(StatusCode::SWITCHING_PROTOCOLS)
        .header(CONNECTION, HeaderValue::from_static("upgrade"))
        .header(UPGRADE, HeaderValue::from_static("websocket"))
        .header(SEC_WEBSOCKET_ACCEPT, HeaderValue::from_str(&accept).unwrap())
        .body(BoxBody::new(empty))
        .unwrap();
    if let Some(proto) = offered {
        resp.headers_mut().insert(SEC_WEBSOCKET_PROTOCOL, HeaderValue::from_str(&proto).unwrap());
    }

    tokio::spawn(async move {
        match on_upgrade.await {
            Ok(upgraded) => {
                let socket = WebSocketStream::from_raw_socket(TokioIo::new(upgraded), Role::Server, None).await;
                let resources = want_resources && require_admin(&principal);
                run_ws(socket, server, board, after, filter, allowed, resources).await;
            }
            Err(err) => {
                tracing::warn!("ws upgrade failed: {err}");
            }
        }
    });

    resp
}

fn event_text(board: &str, kind: &str, seq: i64, record: Option<&Record>) -> String {
    let record_json = match record {
        Some(r) => serde_json::to_value(r).unwrap_or(serde_json::Value::Null),
        None => serde_json::Value::Null,
    };
    serde_json::json!({
        "board": board,
        "type": format!("record.{kind}"),
        "seq": seq,
        "record": record_json,
    })
    .to_string()
}

fn filter_matches(filter: &Option<serde_json::Value>, payload: &serde_json::Value) -> bool {
    let Some(filter) = filter else {
        return true;
    };
    let Some(map) = filter.as_object() else {
        return true;
    };
    for (key, want) in map {
        let Some(actual) = value_at(payload, key) else {
            return false;
        };
        if !eq_value(actual, want) {
            return false;
        }
    }
    true
}

fn value_at<'a>(payload: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    let mut cur = payload;
    for seg in path.split('.') {
        cur = cur.as_object()?.get(seg)?;
    }
    Some(cur)
}

fn eq_value(a: &serde_json::Value, b: &serde_json::Value) -> bool {
    if let (Some(x), Some(y)) = (a.as_f64(), b.as_f64()) {
        return x == y;
    }
    a == b
}

async fn run_ws(
    socket: WebSocketStream<TokioIo<hyper::upgrade::Upgraded>>,
    server: Arc<Server>,
    board: String,
    after: i64,
    filter: Option<String>,
    allowed: bool,
    resources: bool,
) {
    let (mut sink, mut stream) = socket.split();
    if !allowed {
        let _ = sink.send(Message::Text("{\"error\":\"unauthorized\"}".into())).await;
        return;
    }
    let filter_json: Option<serde_json::Value> = filter.and_then(|f| serde_json::from_str(&f).ok());
    let mut rx = server.broker.subscribe();
    let mut ticker = if resources {
        let mut iv = interval(Duration::from_secs(60));
        iv.tick().await;
        Some(iv)
    } else {
        None
    };

    let initial: Vec<String> = {
        // Blocking Helix read (records_after) must run on the blocking thread
        // pool, not the async WS task.
        let engine = server.engine.clone();
        let b = board.clone();
        let a = after;
        let initial_rows = tokio::task::spawn_blocking(move || {
            let e = engine.lock().unwrap();
            e.records_after(&b, a).unwrap_or_default()
        })
        .await
        .unwrap_or_default();
        initial_rows
            .into_iter()
            .filter(|r| filter_matches(&filter_json, &r.payload))
            .map(|record| event_text(&board, "created", record.seq, Some(&record)))
            .collect()
    };
    for ev in initial {
        if sink.send(Message::Text(ev.into())).await.is_err() {
            return;
        }
    }

    loop {
        tokio::select! {
            incoming = stream.next() => match incoming {
                Some(Ok(Message::Close(_))) | None => break,
                Some(Ok(_)) => {}
                Some(Err(_)) => break,
            },
            incoming = rx.recv() => match incoming {
                Ok(event) => {
                    let parsed: serde_json::Value = match serde_json::from_str(&event) {
                        Ok(v) => v,
                        Err(_) => continue,
                    };
                    if parsed.get("board").and_then(|b| b.as_str()) != Some(board.as_str()) {
                        continue;
                    }
                    if let Some(payload) = parsed.get("record").and_then(|r| r.get("payload")) {
                        if !filter_matches(&filter_json, payload) {
                            continue;
                        }
                    }
                    if sink.send(Message::Text(event.into())).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            },
            _ = async {
                if let Some(iv) = ticker.as_mut() {
                    iv.tick().await;
                } else {
                    std::future::pending::<()>().await;
                }
            } => {
                // Throttle the resource report: the count sums over every table
                // (full scans), so only emit when the client asks for it, and
                // no more than once a minute.
                let report = crate::resources::resource_report_cached(&server, &board);
                let msg = serde_json::json!({ "type": "resource", "resources": report })
                    .to_string();
                if sink.send(Message::Text(msg.into())).await.is_err() {
                    break;
                }
            },
        }
    }
}