use super::{arg_str, ok};
use engine::model::Principal;
use engine::ServerlessEngine;
use serde_json::{json, Value as Json};

fn opt_str(arguments: &Json, name: &str) -> Option<String> {
    arguments
        .get(name)
        .and_then(|v| v.as_str())
        .map(String::from)
}

pub async fn send(
    engine: &mut ServerlessEngine,
    _principal: &Principal,
    arguments: &Json,
) -> Result<Json, String> {
    let to = arg_str(arguments, "to")?;
    let subject = arg_str(arguments, "subject")?;
    let out = engine
        .send_email(to, subject, opt_str(arguments, "text").as_deref(), opt_str(arguments, "html").as_deref(), opt_str(arguments, "from").as_deref())
        .await
        .map_err(|e| e.to_string())?;
    ok(json!({ "provider": out["provider"], "status": out["status"] }))
}
