use bytes::Bytes;
use engine::model::Record;
use engine::ServerlessEngine;
use http_body::{Body, Frame};
use hyper::{Response, StatusCode};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tokio::sync::{broadcast, mpsc};

pub struct SseBody {
    rx: mpsc::Receiver<Result<Bytes, std::io::Error>>,
}

impl SseBody {
    pub fn new(rx: mpsc::Receiver<Result<Bytes, std::io::Error>>) -> Self {
        Self { rx }
    }
}

impl Body for SseBody {
    type Data = Bytes;
    type Error = std::io::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        match self.rx.poll_recv(cx) {
            Poll::Ready(Some(Ok(bytes))) => Poll::Ready(Some(Ok(Frame::data(bytes)))),
            Poll::Ready(Some(Err(e))) => Poll::Ready(Some(Err(e))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

fn records_after_json(engine: &ServerlessEngine, board: &str, after: i64) -> Vec<Record> {
    engine.records_after(board, after).unwrap_or_default()
}

fn event_bytes(board: &str, kind: &str, seq: i64, record: Option<&Record>) -> Bytes {
    let record_json = match record {
        Some(r) => serde_json::to_value(r).unwrap_or(serde_json::Value::Null),
        None => serde_json::Value::Null,
    };
    let ev = serde_json::json!({
        "board": board,
        "type": format!("record.{kind}"),
        "seq": seq,
        "record": record_json,
    })
    .to_string();
    Bytes::from(format!("event: record\ndata: {ev}\n\n"))
}

fn keepalive() -> Bytes {
    Bytes::from(": ping\n\n")
}

pub async fn sse_response(
    engine: Arc<std::sync::Mutex<ServerlessEngine>>,
    broker: Arc<crate::broker::InProcBroker>,
    board: String,
    after: i64,
) -> Response<SseBody> {
    let (tx, rx) = mpsc::channel::<Result<Bytes, std::io::Error>>(64);
    let mut live = broker.subscribe();
    // The snapshot is a blocking Helix read (engine lock + reqwest::blocking);
    // it must run on the blocking thread pool, never on the async worker that
    // called us (same constraint as ws.rs initial rows).
    let board2 = board.clone();
    let initial: Vec<Bytes> = tokio::task::spawn_blocking(move || {
        let e = engine.lock().unwrap();
        records_after_json(&e, &board2, after)
            .into_iter()
            .map(|record| event_bytes(&board2, "created", record.seq, Some(&record)))
            .collect::<Vec<Bytes>>()
    })
    .await
    .unwrap_or_default();
    tokio::spawn(async move {
        for b in initial {
            if tx.send(Ok(b)).await.is_err() {
                return;
            }
        }
        loop {
            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_secs(60)) => {
                    if tx.send(Ok(keepalive())).await.is_err() {
                        return;
                    }
                }
                ev = live.recv() => match ev {
                    Ok(event) => {
                        let parsed: serde_json::Value = match serde_json::from_str(&event) {
                            Ok(v) => v,
                            Err(_) => continue,
                        };
                        if parsed.get("board").and_then(|b| b.as_str()) != Some(board.as_str()) {
                            continue;
                        }
                        if tx.send(Ok(Bytes::from(format!("event: record\ndata: {event}\n\n")))).await.is_err() {
                            return;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => return,
                }
            }
        }
    });
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "text/event-stream")
        .header("cache-control", "no-cache")
        .header("connection", "keep-alive")
        .header("access-control-allow-origin", "*")
        .body(SseBody::new(rx))
        .unwrap()
}