use super::ok;
use engine::model::Principal;
use engine::ServerlessEngine;
use serde_json::{json, Value as Json};

pub async fn list(
    engine: &ServerlessEngine,
    _principal: &Principal,
    _arguments: &Json,
) -> Result<Json, String> {
    let users = engine.list_users().await.map_err(|e| e.to_string())?;
    let out: Vec<Json> = users
        .into_iter()
        .map(|u| {
            json!({
                "email": u.email,
                "role": u.role,
                "created_at": u.created_at,
            })
        })
        .collect();
    ok(json!({ "users": out }))
}