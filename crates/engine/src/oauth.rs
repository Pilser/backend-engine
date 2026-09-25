//! Per-board OAuth (Microsoft Entra ID) support.
//!
//! Configuration is stored per BOARD as encrypted secrets (NOT engine-global
//! env), so a multi-tenant engine can host different Entra apps per tenant:
//!   MICROSOFT_TENANT_ID   the Entra tenant id (issuer)
//!   MICROSOFT_CLIENT_ID   the Entra app (client) id (audience)
//!   MICROSOFT_CLIENT_SECRET (optional) — needed for the authorization-code
//!     exchange; if absent the flow uses PKCE.
//!
//! Flow (redirect):
//!   1. GET  /api/srv/<board>/auth/oauth/microsoft/start
//!      -> 302 to the Entra authorize URL (with state + PKCE or secret).
//!   2. Entra redirects to
//!      /api/srv/<board>/auth/oauth/microsoft/callback?code=...&state=...
//!      The engine exchanges the code, verifies the id_token (RS256 against
//!      the tenant JWKS), logs in or creates the user by email, and redirects
//!      to `redirect` (a query param from `start`) with a session token.

use crate::secrets::secrets_map;
use crate::storage::database::Database;
use serde_json::{json, Value as Json};
use std::collections::HashMap;

const AUTH_BASE: &str = "https://login.microsoftonline.com";
const JWKS_PATH: &str = "discovery/v2.0/keys";

/// Per-board Entra config, read from the board's encrypted secrets.
pub struct OAuthConfig {
    pub tenant_id: String,
    pub client_id: String,
    pub client_secret: Option<String>,
}

pub fn config_from_secrets(db: &dyn Database, board: &str) -> anyhow::Result<Option<OAuthConfig>> {
    let map = secrets_map(db, board)?;
    let tenant = map.get("MICROSOFT_TENANT_ID").cloned();
    let client = map.get("MICROSOFT_CLIENT_ID").cloned();
    match (tenant, client) {
        (Some(tenant_id), Some(client_id)) => Ok(Some(OAuthConfig {
            tenant_id,
            client_id,
            client_secret: map.get("MICROSOFT_CLIENT_SECRET").cloned(),
        })),
        _ => Ok(None),
    }
}

/// The Entra authorization URL. `redirect_uri` is this engine's callback URL.
pub fn authorize_url(cfg: &OAuthConfig, state: &str, code_challenge: Option<&str>, redirect_uri: &str) -> String {
    let mut params = vec![
        ("client_id".to_string(), cfg.client_id.clone()),
        ("response_type".to_string(), "code".to_string()),
        ("redirect_uri".to_string(), redirect_uri.to_string()),
        ("response_mode".to_string(), "query".to_string()),
        ("scope".to_string(), "openid email profile".to_string()),
        ("state".to_string(), state.to_string()),
    ];
    if let Some(cc) = code_challenge {
        params.push(("code_challenge".to_string(), cc.to_string()));
        params.push(("code_challenge_method".to_string(), "S256".to_string()));
    }
    let qs = params
        .iter()
        .map(|(k, v)| format!("{k}={}", urlencode(v)))
        .collect::<Vec<_>>()
        .join("&");
    format!("{AUTH_BASE}/{}/oauth2/v2.0/authorize?{qs}", cfg.tenant_id)
}

/// Exchange an authorization code for tokens (id_token) and return the raw
/// id_token string. Uses the client secret if present, else PKCE.
pub fn exchange_code(
    cfg: &OAuthConfig,
    code: &str,
    code_verifier: Option<&str>,
    redirect_uri: &str,
) -> anyhow::Result<String> {
    let url = format!("{AUTH_BASE}/{}/oauth2/v2.0/token", cfg.tenant_id);
    let mut form: Vec<(String, String)> = vec![
        ("client_id".to_string(), cfg.client_id.clone()),
        ("grant_type".to_string(), "authorization_code".to_string()),
        ("code".to_string(), code.to_string()),
        ("redirect_uri".to_string(), redirect_uri.to_string()),
        ("scope".to_string(), "openid email profile".to_string()),
    ];
    if let Some(secret) = &cfg.client_secret {
        form.push(("client_secret".to_string(), secret.clone()));
    }
    if let Some(cv) = code_verifier {
        form.push(("code_verifier".to_string(), cv.to_string()));
    }
    let body: Json = json!(form.iter().map(|(k, v)| (k.clone(), v.clone())).collect::<HashMap<_, _>>());
    let (status, resp) = crate::http::http_call(&url, &[("Content-Type".into(), "application/x-www-form-urlencoded".into())], &body, 15_000)?;
    if status != 200 {
        anyhow::bail!("token exchange failed: HTTP {status}: {resp}");
    }
    resp.get("id_token")
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| anyhow::anyhow!("token response missing id_token: {resp}"))
}

/// Verify an Entra ID token (RS256) against the tenant JWKS and return its
/// claims. Checks: signature, `aud` == client_id, `iss` == the tenant issuer,
/// and `exp`. Returns the email + upn-ish id for user mapping.
pub fn verify_id_token(cfg: &OAuthConfig, token: &str) -> anyhow::Result<Json> {
    let jwks_url = format!("{AUTH_BASE}/{}/{}", cfg.tenant_id, JWKS_PATH);
    let (status, jwks) = crate::http::http_call(&jwks_url, &[], &Json::Null, 15_000)?;
    if status != 200 {
        anyhow::bail!("JWKS fetch failed: HTTP {status}");
    }
    let keys: Vec<Json> = jwks
        .get("keys")
        .and_then(|k| k.as_array())
        .cloned()
        .unwrap_or_default();
    // Parse the token header to learn the kid, then find the matching key.
    let header_json = decode_jwt_part(token, 0)?;
    let kid = header_json.get("kid").and_then(|k| k.as_str()).unwrap_or("");
    let key = keys.iter().find(|k| k.get("kid").and_then(|v| v.as_str()) == Some(kid)).ok_or_else(|| {
        anyhow::anyhow!("no JWKS key matches kid '{kid}'")
    })?;
    let jwk: jsonwebtoken::jwk::Jwk = serde_json::from_value(key.clone())
        .map_err(|e| anyhow::anyhow!("bad JWK for kid {kid}: {e}"))?;
    let decoding_key = jsonwebtoken::DecodingKey::from_jwk(&jwk)
        .map_err(|e| anyhow::anyhow!("jwk -> key: {e}"))?;
    // Entra issues RS256 id_tokens.
    let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::RS256);
    validation.set_audience(&[cfg.client_id.as_str()]);
    validation.set_issuer(&[format!("https://login.microsoftonline.com/{}/v2.0", cfg.tenant_id)]);
    validation.set_required_spec_claims(&["exp", "iss", "aud"]);
    let data = jsonwebtoken::decode::<serde_json::Value>(token, &decoding_key, &validation)
        .map_err(|e| anyhow::anyhow!("id_token verification failed: {e}"))?;
    Ok(data.claims)
}

/// Build a unique email from an Entra id_token's preferred_username or email.
pub fn email_from_claims(claims: &Json) -> anyhow::Result<String> {
    let email = claims
        .get("email")
        .or_else(|| claims.get("preferred_username"))
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("id_token has no email/preferred_username claim"))?;
    Ok(email)
}

fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

fn decode_jwt_part(token: &str, index: usize) -> anyhow::Result<Json> {
    let part = token.split('.').nth(index).ok_or_else(|| anyhow::anyhow!("malformed JWT"))?;
    use base64::Engine;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(part)
        .map_err(|e| anyhow::anyhow!("bad JWT part: {e}"))?;
    Ok(serde_json::from_slice(&bytes)?)
}
