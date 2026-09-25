use crate::McpServer;
use serde_json::json;
use std::sync::Arc;

pub async fn handle_hyper(
    mcp: Arc<McpServer>,
    req: hyper::Request<hyper::body::Incoming>,
) -> hyper::Response<http_body_util::combinators::BoxBody<bytes::Bytes, std::io::Error>> {
    if *req.method() != hyper::Method::POST {
        return text_response(
            hyper::StatusCode::METHOD_NOT_ALLOWED,
            "POST required for JSON-RPC",
        );
    }
    let collected = match http_body_util::BodyExt::collect(req.into_body()).await {
        Ok(b) => b.to_bytes(),
        Err(_) => {
            return text_response(hyper::StatusCode::BAD_REQUEST, "could not read body");
        }
    };
    let text = String::from_utf8_lossy(&collected);
    // The engine + helixdb backend use a blocking reqwest client. Running it
    // directly on a tokio async worker stalls the blocking client's internal
    // runtime after ~128 requests, so dispatch to the blocking thread pool.
    let mcp = mcp.clone();
    let text = text.to_string();
    let out = match tokio::task::spawn_blocking(move || crate::handle_jsonrpc(&mcp, &text)).await {
        Ok(out) => out,
        Err(_) => {
            return json_response(
                json!({"jsonrpc":"2.0","id":null,"error":{"code":-32603,"message":"internal error"}})
                    .to_string(),
            );
        }
    };
    json_response(out)
}

pub async fn serve(mcp: Arc<McpServer>, addr: std::net::SocketAddr) -> anyhow::Result<()> {
    use hyper::service::service_fn;
    use hyper_util::rt::{TokioExecutor, TokioIo};

    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("mcp server listening on http://{addr}");
    loop {
        let (stream, _) = listener.accept().await?;
        let mcp = mcp.clone();
        tokio::spawn(async move {
            let io = TokioIo::new(stream);
            let service = service_fn(move |req| {
                let mcp = mcp.clone();
                async move { Ok::<_, std::io::Error>(handle_hyper(mcp, req).await) }
            });
            let builder = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new());
            if let Err(err) = builder.serve_connection(io, service).await {
                tracing::warn!("mcp connection error: {err}");
            }
        });
    }
}

fn json_response(
    text: String,
) -> hyper::Response<http_body_util::combinators::BoxBody<bytes::Bytes, std::io::Error>> {
    let body = http_body_util::Full::new(bytes::Bytes::from(text));
    let body = http_body_util::BodyExt::map_err(body, |_| std::io::Error::other("body error"));
    hyper::Response::builder()
        .status(hyper::StatusCode::OK)
        .header("content-type", "application/json")
        .body(http_body_util::combinators::BoxBody::new(body))
        .unwrap()
}

fn text_response(
    status: hyper::StatusCode,
    text: &str,
) -> hyper::Response<http_body_util::combinators::BoxBody<bytes::Bytes, std::io::Error>> {
    let body = http_body_util::Full::new(bytes::Bytes::from(text.to_string()));
    let body = http_body_util::BodyExt::map_err(body, |_| std::io::Error::other("body error"));
    hyper::Response::builder()
        .status(status)
        .header("content-type", "text/plain")
        .body(http_body_util::combinators::BoxBody::new(body))
        .unwrap()
}