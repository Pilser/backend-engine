pub mod tools;
pub mod transport;

use engine::registry::{ArgType, CommandSpec};
use engine::ServerlessEngine;
use serde_json::{json, Value as Json};
use std::sync::{Arc, Mutex};

pub struct McpServer {
    engine: Arc<Mutex<ServerlessEngine>>,
    specs: Vec<CommandSpec>,
    reporter: Option<ResourceReporter>,
}

pub type ResourceReporter = Arc<dyn Fn(&str) -> Json + Send + Sync>;

impl McpServer {
    pub fn new(engine: Arc<Mutex<ServerlessEngine>>) -> Self {
        Self {
            engine,
            specs: engine::registry::registry(),
            reporter: None,
        }
    }

    pub fn with_reporter(mut self, reporter: ResourceReporter) -> Self {
        self.reporter = Some(reporter);
        self
    }

    pub fn with_defaults() -> Self {
        Self::new(Arc::new(Mutex::new(ServerlessEngine::with_defaults())))
    }

    pub fn engine(&self) -> &Arc<Mutex<ServerlessEngine>> {
        &self.engine
    }

    pub fn specs(&self) -> &[CommandSpec] {
        &self.specs
    }

    pub fn spec(&self, name: &str) -> Option<&CommandSpec> {
        self.specs.iter().find(|s| s.name() == name)
    }

    pub fn tools_list(&self) -> Json {
        let tools: Vec<Json> = self.specs.iter().map(schema_for).collect();
        json!({ "tools": tools })
    }

    pub fn call(&self, name: &str, arguments: &Json) -> Result<Json, String> {
        if name == "apps.resources" {
            if let Some(reporter) = &self.reporter {
                let board = arguments
                    .get("board")
                    .and_then(|b| b.as_str())
                    .ok_or_else(|| "missing argument board".to_string())?;
                return crate::tools::ok(reporter(board));
            }
        }
        let spec = self
            .spec(name)
            .ok_or_else(|| format!("unknown tool '{name}'"))?;
        let owner = arguments
            .get("owner")
            .and_then(|o| o.as_str())
            .unwrap_or("mcp");
        let principal = engine::model::Principal {
            id: owner.to_string(),
            role: "owner".to_string(),
            scope: None,
            writer: None,
        };
        let mut engine = self
            .engine
            .lock()
            .map_err(|_| "engine lock poisoned".to_string())?;
        tools::run(&mut engine, &principal, spec, arguments)
    }
}

pub fn schema_for(spec: &CommandSpec) -> Json {
    let mut props = serde_json::Map::new();
    let mut required: Vec<String> = Vec::new();
    for arg in &spec.positional {
        props.insert(
            arg.name.clone(),
            json!({
                "type": json_types(&arg.r#type),
                "description": arg.help,
            }),
        );
        if arg.required {
            required.push(arg.name.clone());
        }
    }
    for flag in &spec.flags {
        let t: Json = match flag.r#type {
            engine::registry::FlagType::Bool => Json::String("boolean".to_string()),
            engine::registry::FlagType::Int => Json::String("integer".to_string()),
            engine::registry::FlagType::Json => {
                Json::Array(vec![Json::String("object".to_string()), Json::String("array".to_string())])
            }
            _ => Json::String("string".to_string()),
        };
        let mut p = json!({ "type": t, "description": flag.help });
        if let Some(d) = &flag.default {
            p["default"] = Json::String(d.clone());
        }
        if !flag.allowed.is_empty() {
            p["enum"] = Json::Array(flag.allowed.iter().map(|a| Json::String(a.clone())).collect());
        }
        props.insert(flag.name.clone(), p);
    }
    json!({
        "name": spec.name(),
        "description": format!("{} — {}", spec.summary, spec.description),
        "inputSchema": {
            "type": "object",
            "properties": props,
            "required": required,
        },
    })
}

fn json_types(t: &ArgType) -> Json {
    match t {
        ArgType::Json => Json::Array(vec![Json::String("object".to_string()), Json::String("array".to_string())]),
        ArgType::Int | ArgType::Seq => Json::String("integer".to_string()),
        _ => Json::String("string".to_string()),
    }
}

pub fn handle_jsonrpc(mcp: &McpServer, body: &str) -> String {
    let req: Json = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => return json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"parse error"}}).to_string(),
    };
    let id = req.get("id").cloned().unwrap_or(Json::Null);
    let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let params = req.get("params").cloned().unwrap_or(Json::Null);
    let resp = match method {
        "initialize" => {
            let requested = params
                .get("protocolVersion")
                .and_then(|v| v.as_str())
                .unwrap_or("2025-06-18")
                .to_string();
            json!({
                "jsonrpc": "2.0", "id": id,
                "result": {
                    "protocolVersion": requested,
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "serverless-engine", "version": engine::version() },
                }
            })
        }
        "tools/list" => json!({ "jsonrpc": "2.0", "id": id, "result": mcp.tools_list() }),
        "tools/call" => {
            let name = params
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("")
                .to_string();
            let arguments = params.get("arguments").cloned().unwrap_or(Json::Null);
            match mcp.call(&name, &arguments) {
                Ok(result) => json!({
                    "jsonrpc": "2.0", "id": id,
                    "result": { "content": [ { "type": "text", "text": result.to_string() } ] }
                }),
                Err(e) => json!({
                    "jsonrpc": "2.0", "id": id,
                    "error": { "code": -32602, "message": e }
                }),
            }
        }
        "ping" => json!({ "jsonrpc": "2.0", "id": id, "result": {} }),
        _ if method.starts_with("notifications/") => json!({}),
        _ => json!({
            "jsonrpc": "2.0", "id": id,
            "error": { "code": -32601, "message": format!("method not found: {method}") }
        }),
    };
    resp.to_string()
}