use crate::client::ToolCall;
use serde_json::Value as Json;

pub fn render(_call: &ToolCall, out: &Json) -> String {
    if is_tty() {
        render_human(out)
    } else {
        serde_json::to_string_pretty(out).unwrap_or_else(|_| out.to_string())
    }
}

fn render_human(out: &Json) -> String {
    if let Some(status) = out.get("status").and_then(|s| s.as_str()) {
        if status != "ok" {
            return format!("status: {status}\n");
        }
    }
    let result = out.get("result").cloned().unwrap_or_else(|| out.clone());
    match &result {
        Json::Object(map) => {
            let mut lines = Vec::new();
            for (k, v) in map {
                match v {
                    Json::Array(items) if !items.is_empty() && items.iter().all(Json::is_object) => {
                        lines.push(format!("{k}:"));
                        for it in items {
                            let sub: Vec<String> = it
                                .as_object()
                                .map(|m| m.iter().map(|(kk, vv)| format!("  {kk}: {}", pretty(vv))).collect())
                                .unwrap_or_default();
                            lines.extend(sub);
                        }
                    }
                    Json::Array(items) => {
                        lines.push(format!("{k}: {}", items.len()));
                        for it in items {
                            lines.push(format!("  - {}", pretty(it)));
                        }
                    }
                    other => lines.push(format!("{k}: {}", pretty(other))),
                }
            }
            lines.join("\n") + "\n"
        }
        other => pretty(other) + "\n",
    }
}

fn pretty(v: &Json) -> String {
    match v {
        Json::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn is_tty() -> bool {
    std::io::IsTerminal::is_terminal(&std::io::stdout())
}

