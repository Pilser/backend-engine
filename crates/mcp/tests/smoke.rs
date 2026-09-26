//! MCP agent-surface smoke tests (Phase 5b).
//!
//! Exercises the JSON-RPC surface an AI agent drives: tool discovery, table
//! setup, record round-trip, and tenant config. Runs natively in CI.

use futures_executor::block_on;
use mcp::McpServer;
use serde_json::{json, Value as Json};

fn call(mcp: &McpServer, tool: &str, arguments: Json) -> Json {
    let body = json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": {"name": tool, "arguments": arguments},
    })
    .to_string();
    let resp: Json =
        serde_json::from_str(&block_on(mcp::handle_jsonrpc(mcp, &body))).unwrap();
    assert!(
        resp.get("error").is_none(),
        "tool {tool} errored: {}",
        resp["error"]
    );
    let text = resp["result"]["content"][0]["text"].as_str().unwrap().to_string();
    serde_json::from_str::<Json>(&text).unwrap()["result"].clone()
}

#[test]
fn agent_drives_backend() {
    let mcp = McpServer::with_defaults();

    // Discovery: the tool index parses and mentions records.submit.
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {}}).to_string();
    let resp: Json =
        serde_json::from_str(&block_on(mcp::handle_jsonrpc(&mcp, &body))).unwrap();
    let tools = resp["result"]["tools"].as_array().unwrap();
    assert!(!tools.is_empty());
    assert!(tools.iter().any(|t| t["name"] == json!("records.submit")));

    // Tenant config + table setup (single tenant, no board argument).
    let shown = call(&mcp, "tenant.show", json!({}));
    assert_eq!(shown["title"], json!(engine::TENANT));
    call(&mcp, "tenant.update", json!({"title": "smoke"}));
    call(&mcp, "tables.create", json!({"table": "notes"}));

    // Record round-trip.
    let sub = call(&mcp, "records.submit", json!({"table": "notes", "payload": {"body": "hi"}}));
    let seq = sub["seq"].as_i64().unwrap();
    assert!(seq > 0);
    let got = call(&mcp, "records.get", json!({"table": "notes", "seq": seq}));
    assert_eq!(got["payload"]["body"], json!("hi"));

    // Keys + secrets.
    let listed = call(&mcp, "keys.list", json!({}));
    assert_eq!(listed["keys"].as_array().unwrap().len(), 0);
    let issued = call(&mcp, "keys.issue", json!({"role": "reader"}));
    assert_eq!(issued["role"], json!("reader"));
    assert!(!issued["key"].as_str().unwrap().is_empty());
    call(&mcp, "secrets.set", json!({"name": "K", "value": "v"}));
    let secrets = call(&mcp, "secrets.list", json!({}));
    assert_eq!(secrets["secrets"].as_array().unwrap().len(), 1);

    // Update + patch + table knobs.
    call(&mcp, "records.update", json!({"table": "notes", "seq": seq, "payload": {"body": "edited"}}));
    let got2 = call(&mcp, "records.get", json!({"table": "notes", "seq": seq}));
    assert_eq!(got2["payload"]["body"], json!("edited"));
    let patched = call(&mcp, "records.patch", json!({"table": "notes", "seq": seq, "patch": {"$set": {"body": "patched"}}}));
    assert_eq!(patched["payload"]["body"], json!("patched"));
    let cfg = call(&mcp, "tables.config", json!({"table": "notes", "show": true}));
    assert!(cfg["ttl_seconds"].is_null());
    call(&mcp, "tables.config", json!({"table": "notes", "computed": {"$.upper": "upper($.body)"}}));
    let cfg2 = call(&mcp, "tables.config", json!({"table": "notes", "show": true}));
    assert!(cfg2["computed"].is_object());

    // Auth session lifecycle.
    call(&mcp, "auth.signup", json!({"email": "u@x.y", "password": "secret123"}));
    let login = call(&mcp, "auth.login", json!({"email": "u@x.y", "password": "secret123"}));
    let token = login["token"].as_str().unwrap().to_string();
    let me = call(&mcp, "auth.me", json!({"token": token}));
    assert_eq!(me["user"]["email"], json!("u@x.y"));
    call(&mcp, "auth.logout", json!({"token": token}));
}

fn call_one(mcp: &McpServer, command: &str) -> Json {
    let body = json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": {"name": "manage_serverless_engine", "arguments": {"command": command}},
    })
    .to_string();
    let resp: Json =
        serde_json::from_str(&block_on(mcp::handle_jsonrpc(mcp, &body))).unwrap();
    assert!(
        resp.get("error").is_none(),
        "single tool errored on {command:?}: {}",
        resp["error"]
    );
    let text = resp["result"]["content"][0]["text"].as_str().unwrap().to_string();
    serde_json::from_str::<Json>(&text).unwrap()
}

#[test]
fn single_tool_cli() {
    let mcp = McpServer::with_defaults();

    // Discovery advertises exactly one tool.
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {}}).to_string();
    let resp: Json =
        serde_json::from_str(&block_on(mcp::handle_jsonrpc(&mcp, &body))).unwrap();
    let tools = resp["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["name"], json!("manage_serverless_engine"));

    // --help at every level comes back as help text.
    let top = call_one(&mcp, "--help");
    assert!(top["text"].as_str().unwrap().contains("groups:"));
    let grp = call_one(&mcp, "records --help");
    assert!(grp["text"].as_str().unwrap().contains("submit"));
    let verb = call_one(&mcp, "records submit --help");
    assert!(verb["text"].as_str().unwrap().contains("--help")
        || verb["text"].as_str().unwrap().contains("payload"));

    // Full flow through the single door, incl. leading `serverless` + flags.
    let shown = call_one(&mcp, "serverless tenant show");
    assert_eq!(shown["result"]["title"], json!("singleton"));
    call_one(&mcp, "tables create cli_notes");
    let sub = call_one(&mcp, "records submit cli_notes '{\"body\":\"via-cli\"}'");
    let seq = sub["result"]["seq"].as_i64().unwrap();
    assert!(seq > 0);
    let got = call_one(&mcp, &format!("records get cli_notes {seq}"));
    assert_eq!(got["result"]["payload"]["body"], json!("via-cli"));

    // Unknown verbs explain themselves.
    let body = json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": {"name": "manage_serverless_engine", "arguments": {"command": "nope nope"}},
    })
    .to_string();
    let resp: Json =
        serde_json::from_str(&block_on(mcp::handle_jsonrpc(&mcp, &body))).unwrap();
    assert!(resp.get("error").is_some());
}
// touch
