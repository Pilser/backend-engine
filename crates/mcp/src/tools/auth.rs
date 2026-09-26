use super::{arg_str, ok};
use engine::model::Principal;
use engine::ServerlessEngine;
use serde_json::{json, Value as Json};

pub async fn signup(
    engine: &mut ServerlessEngine,
    principal: &Principal,
    arguments: &Json,
) -> Result<Json, String> {
    let email = arg_str(arguments, "email")?;
    let password = arguments.get("password").and_then(|p| p.as_str()).unwrap_or("");
    let password_hash = arguments.get("password_hash").and_then(|p| p.as_str());
    if !password.is_empty() == password_hash.is_some() {
        return Err("provide exactly one of password or password_hash".to_string());
    }
    let role = arguments.get("role").and_then(|r| r.as_str()).unwrap_or("reader");
    let user = engine.signup_user(&email, password, password_hash, role, principal).await
        .map_err(|e| e.to_string())?;
    ok(json!({ "user": user }))
}

pub async fn login(
    engine: &mut ServerlessEngine,
    _principal: &Principal,
    arguments: &Json,
) -> Result<Json, String> {
    let email = arg_str(arguments, "email")?;
    let password = arg_str(arguments, "password")?;
    let (token, jwt) = engine.login_user(&email, password).await.map_err(|e| e.to_string())?;
    ok(json!({ "token": token, "jwt": jwt }))
}

pub async fn me(
    engine: &ServerlessEngine,
    _principal: &Principal,
    arguments: &Json,
) -> Result<Json, String> {
    let token = arg_str(arguments, "token")?;
    match engine.user_by_token(token).await.map_err(|e| e.to_string())? {
        Some(user) => ok(json!({ "user": user })),
        None => Err("invalid or expired session".to_string()),
    }
}

pub async fn logout(
    engine: &mut ServerlessEngine,
    _principal: &Principal,
    arguments: &Json,
) -> Result<Json, String> {
    let token = arg_str(arguments, "token")?;
    engine.logout_user(token).await.map_err(|e| e.to_string())?;
    ok(json!({ "logged_out": true }))
}

pub async fn set_role(
    engine: &mut ServerlessEngine,
    principal: &Principal,
    arguments: &Json,
) -> Result<Json, String> {
    let email = arg_str(arguments, "email")?;
    let role = arg_str(arguments, "role")?;
    let user = engine.set_user_role(&email, &role, principal).await
        .map_err(|e| e.to_string())?;
    ok(json!({ "user": user }))
}