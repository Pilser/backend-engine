use engine::model::Principal;
use engine::ServerlessEngine;
use http::HeaderMap;

pub fn bearer_token(headers: &HeaderMap) -> Option<String> {
    let auth = headers.get(http::header::AUTHORIZATION)?.to_str().ok()?;
    let rest = auth.strip_prefix("Bearer ")?;
    if rest.is_empty() {
        None
    } else {
        Some(rest.to_string())
    }
}

pub fn x_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get("X-Srv-Key")
        .or_else(|| headers.get("X-Api-Key"))
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.is_empty())
        .map(String::from)
}

pub fn token(headers: &HeaderMap) -> Option<String> {
    bearer_token(headers).or_else(|| x_token(headers))
}

pub fn resolve(
    engine: &ServerlessEngine,
    board_id: &str,
    headers: &HeaderMap,
    scope: Option<&str>,
) -> anyhow::Result<Principal> {
    engine.resolve_principal(board_id, token(headers).as_deref(), scope)
}

pub fn is_owner_or_admin(p: &Principal) -> bool {
    matches!(p.role.as_str(), "owner" | "admin")
}