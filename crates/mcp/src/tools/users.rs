use super::{arg_str, ok};
use engine::model::Principal;
use engine::ServerlessEngine;
use serde_json::{json, Value as Json};

pub fn list(
    engine: &ServerlessEngine,
    _principal: &Principal,
    arguments: &Json,
) -> Result<Json, String> {
    let board = arg_str(arguments, "board")?;
    let users = engine.list_users(&board).map_err(|e| e.to_string())?;
    let out: Vec<Json> = users
        .into_iter()
        .map(|u| {
            json!({
                "email": u.email,
                "role": u.role,
                "board_id": u.board_id,
                "created_at": u.created_at,
            })
        })
        .collect();
    ok(json!({ "users": out }))
}