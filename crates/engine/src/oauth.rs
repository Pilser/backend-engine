//! Generic OIDC login — any provider (Entra ID, Google, Keycloak, Auth0…).
//!
//! Tenant config lives in encrypted secrets (NOT env):
//!   OAUTH_ISSUER         e.g. https://login.microsoftonline.com/<tenant>/v2.0
//!   OAUTH_CLIENT_ID      the provider-side app (client) id (audience)
//!   OAUTH_CLIENT_SECRET  optional — confidential clients send it; without it
//!                        the flow uses PKCE (S256)
//!   OAUTH_SCOPES         optional, default `openid email profile`
//!
//! Flow (browser redirects, like the old Microsoft-only flow but discovered):
//!   1. GET  /api/auth/oauth/start?redirect=/app
//!       → 302 to the provider authorize URL (state + PKCE or secret).
//!   2. Provider redirects to /api/auth/oauth/callback?code=…&state=…
//!      The engine exchanges the code, verifies the id_token (RS256 against
//!      the discovered JWKS), logs in or creates the user by email, and
//!      redirects to `redirect` with `?srv_token=`.
//!
//! Redirect-safety: `redirect` must be an in-app path (single leading `/`).
//! State portability: the state envelope (state+verifier+redirect) travels in
//! a sealed HttpOnly cookie — memory stores do not survive across isolates.

use crate::secrets::secrets_map;
use crate::storage::database::Database;
use base64::Engine as _;
use serde_json::{json, Value as Json};
use std::collections::HashMap;

/// State cookie name (shared with the HTTP layer).
pub const STATE_COOKIE: &str = "oauth_state";
const STATE_TTL_SECS: i64 = 600;
const CLOCK_SKEW_SECS: i64 = 60;

/// Per-tenant OIDC config, read from the tenant's encrypted secrets.
pub struct OAuthConfig {
    pub issuer: String,
    pub client_id: String,
    pub client_secret: Option<String>,
    pub scopes: String,
}

pub async fn config_from_secrets(db: &dyn Database) -> anyhow::Result<Option<OAuthConfig>> {
    let map = secrets_map(db).await?;
    let get = |k: &str| map.get(k).cloned().filter(|v| !v.is_empty());
    match (get("OAUTH_ISSUER"), get("OAUTH_CLIENT_ID")) {
        (Some(issuer), Some(client_id)) => {
            let issuer = issuer.trim_end_matches('/').to_string();
            if !is_issuer_url(&issuer) {
                anyhow::bail!("OAUTH_ISSUER must be an https:// URL (http only for localhost)");
            }
            Ok(Some(OAuthConfig {
                issuer,
                client_id,
                client_secret: get("OAUTH_CLIENT_SECRET"),
                scopes: get("OAUTH_SCOPES").unwrap_or_else(|| "openid email profile".to_string()),
            }))
        }
        _ => Ok(None),
    }
}

fn is_issuer_url(s: &str) -> bool {
    if s.starts_with("https://") && s.len() > "https://".len() {
        return true;
    }
    // Loopback http for local IdP development only.
    if let Some(rest) = s.strip_prefix("http://") {
        let host = rest.split('/').next().unwrap_or("");
        let host = host.split(':').next().unwrap_or("");
        return host == "localhost" || host == "127.0.0.1" || host == "[::1]";
    }
    false
}

/// OIDC discovery document endpoints.
pub struct Discovery {
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub jwks_uri: String,
}

pub async fn discover(issuer: &str) -> anyhow::Result<Discovery> {
    let url = format!("{issuer}/.well-known/openid-configuration");
    crate::webhooks::valid_url(&url)
        .then_some(())
        .ok_or_else(|| anyhow::anyhow!("issuer is not a fetchable https URL"))?;
    let (status, doc) = crate::http::http_call(&url, &[], &Json::Null, 15_000).await?;
    if status != 200 {
        anyhow::bail!("discovery failed: HTTP {status}");
    }
    let str_field = |k: &str| {
        doc.get(k)
            .and_then(|v| v.as_str())
            .map(String::from)
            .ok_or_else(|| anyhow::anyhow!("discovery doc missing '{k}'"))
    };
    Ok(Discovery {
        authorization_endpoint: str_field("authorization_endpoint")?,
        token_endpoint: str_field("token_endpoint")?,
        jwks_uri: str_field("jwks_uri")?,
    })
}

/// PKCE verifier: 64 hex chars (uuid v4 × 2 — no extra deps, wasm-safe RNG).
pub fn pkce_verifier() -> String {
    format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple())
}

/// PKCE S256 challenge: base64url(sha256(verifier)), no padding.
pub fn pkce_challenge_s256(verifier: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(verifier.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest)
}

/// The provider authorize URL. `redirect_uri` is this engine's callback URL.
pub fn authorize_url(
    discovery: &Discovery,
    cfg: &OAuthConfig,
    state: &str,
    code_challenge: Option<&str>,
    redirect_uri: &str,
) -> String {
    let mut params = vec![
        ("client_id".to_string(), cfg.client_id.clone()),
        ("response_type".to_string(), "code".to_string()),
        ("redirect_uri".to_string(), redirect_uri.to_string()),
        ("response_mode".to_string(), "query".to_string()),
        ("scope".to_string(), cfg.scopes.clone()),
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
    format!("{}?{qs}", discovery.authorization_endpoint)
}

/// Exchange an authorization code for tokens and return the raw id_token.
/// Sends the client secret when configured, else the PKCE verifier.
pub async fn exchange_code(
    discovery: &Discovery,
    cfg: &OAuthConfig,
    code: &str,
    code_verifier: Option<&str>,
    redirect_uri: &str,
) -> anyhow::Result<String> {
    let mut form: Vec<(String, String)> = vec![
        ("grant_type".to_string(), "authorization_code".to_string()),
        ("code".to_string(), code.to_string()),
        ("redirect_uri".to_string(), redirect_uri.to_string()),
    ];
    if let Some(secret) = &cfg.client_secret {
        form.push(("client_id".to_string(), cfg.client_id.clone()));
        form.push(("client_secret".to_string(), secret.clone()));
    } else {
        form.push(("client_id".to_string(), cfg.client_id.clone()));
        if let Some(cv) = code_verifier {
            form.push(("code_verifier".to_string(), cv.to_string()));
        }
    }
    let encoded =
        form.iter().map(|(k, v)| format!("{}={}", urlencode(k), urlencode(v))).collect::<Vec<_>>().join("&");
    let body = crate::http::HttpBody::Raw {
        content_type: "application/x-www-form-urlencoded".to_string(),
        bytes: encoded.into_bytes(),
    };
    let (status, resp) =
        crate::http::http_call_body(&discovery.token_endpoint, &[("Accept".into(), "application/json".into())], &body, 15_000)
            .await?;
    if status != 200 {
        anyhow::bail!("token exchange failed: HTTP {status}: {resp}");
    }
    resp.get("id_token")
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| anyhow::anyhow!("token response missing id_token: {resp}"))
}

/// Verify an OIDC id_token (RS256) against the JWKS and return its claims.
/// Checks: signature, `aud` == client_id, `iss` == issuer, `exp` (60 s skew).
pub async fn verify_id_token(
    discovery: &Discovery,
    cfg: &OAuthConfig,
    token: &str,
) -> anyhow::Result<Json> {
    crate::webhooks::valid_url(&discovery.jwks_uri)
        .then_some(())
        .ok_or_else(|| anyhow::anyhow!("JWKS uri is not fetchable"))?;
    let (status, jwks) = crate::http::http_call(&discovery.jwks_uri, &[], &Json::Null, 15_000).await?;
    if status != 200 {
        anyhow::bail!("JWKS fetch failed: HTTP {status}");
    }
    let keys: Vec<Json> = jwks
        .get("keys")
        .and_then(|k| k.as_array())
        .cloned()
        .unwrap_or_default();
    verify_with_keys(&keys, cfg, token, now_unix())
}

fn now_unix() -> i64 {
    chrono::Utc::now().timestamp()
}

fn decode_jwt_part(token: &str, index: usize) -> anyhow::Result<Json> {
    let part = token.split('.').nth(index).ok_or_else(|| anyhow::anyhow!("malformed JWT"))?;
    use base64::Engine;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(part)
        .map_err(|e| anyhow::anyhow!("bad JWT part: {e}"))?;
    Ok(serde_json::from_slice(&bytes)?)
}

fn b64url_to_bytes(s: &str) -> anyhow::Result<Vec<u8>> {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(s)
        .map_err(|e| anyhow::anyhow!("bad base64url: {e}"))
}

/// Pure (no-I/O) RS256 verification against a JWKS `keys` array. Split out
/// so tests exercise the crypto without network.
pub fn verify_with_keys(
    keys: &[Json],
    cfg: &OAuthConfig,
    token: &str,
    now_unix: i64,
) -> anyhow::Result<Json> {
    let header = decode_jwt_part(token, 0)?;
    let alg = header.get("alg").and_then(|v| v.as_str()).unwrap_or("");
    if alg != "RS256" {
        anyhow::bail!("unsupported id_token alg '{alg}' (only RS256)");
    }
    let kid = header.get("kid").and_then(|v| v.as_str()).unwrap_or("");
    let candidates: Vec<&Json> = keys
        .iter()
        .filter(|k| {
            k.get("kty").and_then(|v| v.as_str()) == Some("RSA")
                && (kid.is_empty() || k.get("kid").and_then(|v| v.as_str()) == Some(kid))
        })
        .collect();
    let key = candidates.into_iter().next().ok_or_else(|| {
        if kid.is_empty() {
            anyhow::anyhow!("no RSA key in JWKS")
        } else {
            anyhow::anyhow!("no JWKS key matches kid '{kid}'")
        }
    })?;
    let n_b64 = key.get("n").and_then(|v| v.as_str()).ok_or_else(|| anyhow::anyhow!("JWK missing n"))?;
    let e_b64 = key.get("e").and_then(|v| v.as_str()).ok_or_else(|| anyhow::anyhow!("JWK missing e"))?;
    let n = num_bigint(n_b64)?;
    let e = num_bigint(e_b64)?;
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        anyhow::bail!("malformed JWT");
    }
    let signing_input = format!("{}.{}", parts[0], parts[1]);
    let sig_bytes = b64url_to_bytes(parts[2])?;
    rsa_verify(&n, &e, signing_input.as_bytes(), &sig_bytes)?;
    let claims = decode_jwt_part(token, 1)?;
    check_claims(&claims, cfg, now_unix)?;
    Ok(claims)
}

fn num_bigint(b64: &str) -> anyhow::Result<rsa::BigUint> {
    Ok(rsa::BigUint::from_bytes_be(&b64url_to_bytes(b64)?))
}

fn rsa_verify(n: &rsa::BigUint, e: &rsa::BigUint, msg: &[u8], sig: &[u8]) -> anyhow::Result<()> {
    use rsa::pkcs1v15::VerifyingKey;
    use sha2_010::Sha256;
    use signature::Verifier;
    let pk = rsa::RsaPublicKey::new(n.clone(), e.clone())
        .map_err(|e| anyhow::anyhow!("bad RSA key: {e}"))?;
    let vk = VerifyingKey::<Sha256>::new(pk);
    let signature = rsa::pkcs1v15::Signature::try_from(sig)
        .map_err(|e| anyhow::anyhow!("bad signature bytes: {e}"))?;
    vk.verify(msg, &signature).map_err(|e| anyhow::anyhow!("id_token signature invalid: {e}"))
}

fn check_claims(claims: &Json, cfg: &OAuthConfig, now_unix: i64) -> anyhow::Result<()> {
    // aud: string or array containing client_id.
    let aud_ok = match claims.get("aud") {
        Some(Json::String(a)) => a == &cfg.client_id,
        Some(Json::Array(arr)) => arr.iter().any(|a| a.as_str() == Some(cfg.client_id.as_str())),
        _ => false,
    };
    if !aud_ok {
        anyhow::bail!("id_token aud mismatch");
    }
    // iss: exact match modulo trailing slash.
    let iss = claims.get("iss").and_then(|v| v.as_str()).unwrap_or("");
    if iss.trim_end_matches('/') != cfg.issuer.trim_end_matches('/') {
        anyhow::bail!("id_token iss mismatch");
    }
    // exp: required; nbf: honored when present. 60 s clock skew.
    let exp = claims.get("exp").and_then(|v| v.as_i64()).ok_or_else(|| anyhow::anyhow!("id_token missing exp"))?;
    if now_unix > exp + CLOCK_SKEW_SECS {
        anyhow::bail!("id_token expired");
    }
    if let Some(nbf) = claims.get("nbf").and_then(|v| v.as_i64()) {
        if now_unix + CLOCK_SKEW_SECS < nbf {
            anyhow::bail!("id_token not yet valid");
        }
    }
    Ok(())
}

/// Build a unique login id from an id_token's email-ish claims.
pub fn email_from_claims(claims: &Json) -> anyhow::Result<String> {
    let email = claims
        .get("email")
        .or_else(|| claims.get("preferred_username"))
        .or_else(|| claims.get("upn"))
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("id_token has no email/preferred_username claim"))?;
    Ok(email)
}

// ---- sealed state envelope (cookie-transportable) --------------------------

/// Seal `{state, verifier?, redirect, exp}` for the state cookie (AES-GCM via
/// the master key; base64url so it survives cookie transport).
pub fn seal_state(state: &str, verifier: Option<&str>, redirect: &str) -> anyhow::Result<String> {
    use base64::Engine;
    let env = json!({
        "v": 1,
        "state": state,
        "verifier": verifier,
        "redirect": redirect,
        "exp": chrono::Utc::now().timestamp() + STATE_TTL_SECS,
    });
    let sealed = crate::secrets::encrypt_value(env.to_string().as_bytes())?;
    let raw = base64::engine::general_purpose::STANDARD
        .decode(&sealed)
        .map_err(|e| anyhow::anyhow!("seal encode: {e}"))?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw))
}

pub struct OpenedState {
    pub state: String,
    pub verifier: Option<String>,
    pub redirect: String,
}

/// Open + validate a state envelope (authenticity, expiry).
pub fn open_state(encoded: &str) -> anyhow::Result<OpenedState> {
    use base64::Engine;
    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|e| anyhow::anyhow!("bad state envelope: {e}"))?;
    let sealed = base64::engine::general_purpose::STANDARD.encode(raw);
    let pt = crate::secrets::decrypt_value(&sealed)?;
    let env: Json = serde_json::from_slice(&pt)?;
    let exp = env.get("exp").and_then(|v| v.as_i64()).unwrap_or(0);
    if chrono::Utc::now().timestamp() > exp {
        anyhow::bail!("oauth state expired");
    }
    Ok(OpenedState {
        state: env.get("state").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        verifier: env.get("verifier").and_then(|v| v.as_str()).map(String::from),
        redirect: env.get("redirect").and_then(|v| v.as_str()).unwrap_or("/").to_string(),
    })
}

/// In-app redirect targets only: a single leading `/`, never `//` or a scheme.
pub fn sanitize_redirect(raw: &str) -> String {
    if raw.starts_with('/') && !raw.starts_with("//") && !raw.contains("://") && !raw.contains('\\') {
        raw.to_string()
    } else {
        "/".to_string()
    }
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

/// Parse an `application/x-www-form-urlencoded` body into a map.
pub fn parse_form(body: &[u8]) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for pair in String::from_utf8_lossy(body).split('&') {
        let mut kv = pair.splitn(2, '=');
        if let (Some(k), Some(v)) = (kv.next(), kv.next()) {
            out.insert(urldecode(k), urldecode(v));
        }
    }
    out
}

fn urldecode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hex = |b: u8| match b {
                    b'0'..=b'9' => Some(b - b'0'),
                    b'a'..=b'f' => Some(b - b'a' + 10),
                    b'A'..=b'F' => Some(b - b'A' + 10),
                    _ => None,
                };
                match (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                    (Some(h), Some(l)) => {
                        out.push((h << 4 | l) as char);
                        i += 3;
                    }
                    _ => {
                        out.push('%');
                        i += 1;
                    }
                }
            }
            b => {
                out.push(b as char);
                i += 1;
            }
        }
    }
    out
}
