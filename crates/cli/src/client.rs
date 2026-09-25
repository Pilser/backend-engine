use serde_json::{json, Value as Json};

pub struct ToolCall {
    pub name: String,
    pub arguments: Json,
}

pub async fn call(daemon: &str, call: &ToolCall) -> anyhow::Result<Json> {
    let url = format!("{}/mcp", daemon.trim_end_matches('/'));
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": { "name": call.name, "arguments": call.arguments },
    });
    let client = reqwest::Client::new();
    let resp = client
        .post(&url)
        .header("content-type", "application/json")
        .body(body.to_string())
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("cannot reach daemon at {url}: {e}"))?;
    let text = resp
        .text()
        .await
        .map_err(|e| anyhow::anyhow!("cannot read daemon response: {e}"))?;
    let parsed: Json = serde_json::from_str(&text)
        .map_err(|e| anyhow::anyhow!("invalid jsonrpc response from {url}: {e}"))?;
    if let Some(err) = parsed.get("error") {
        let msg = err
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("unknown error");
        return Err(anyhow::anyhow!("{msg}"));
    }
    let result = parsed.get("result").cloned().unwrap_or(Json::Null);
    let text_content = result
        .get("content")
        .and_then(|c| c.as_array())
        .and_then(|arr| arr.first())
        .and_then(|c| c.get("text"))
        .and_then(|t| t.as_str())
        .map(|s| s.to_string());
    match text_content {
        Some(s) => serde_json::from_str(&s).map_err(|_| anyhow::anyhow!(s)),
        None => Ok(result),
    }
}