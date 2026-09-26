//! Generic OIDC login routes (any provider — Entra ID, Google, Keycloak…).
//!
//! `GET /api/auth/oauth/start?redirect=/app` → 302 to the provider (state +
//! PKCE sealed in an HttpOnly cookie so it survives across isolates).
//! `GET /api/auth/oauth/callback?code=…&state=…` → verifies, provisions the
//! user, 302s back to `redirect?srv_token=…`.
//! Provider config comes from tenant secrets (`OAUTH_ISSUER`,
//! `OAUTH_CLIENT_ID`, optional `OAUTH_CLIENT_SECRET`/`OAUTH_SCOPES`).
//! Both routes are public (they *are* the login door).

use worker::{Request, Response, Result, RouteContext};

use crate::{auth, cors, query};

fn redirect_with(url: &str, cookies: &[String]) -> Response {
    let h = worker::Headers::new();
    let _ = h.set("location", url);
    let _ = h.set("cache-control", "no-store");
    for c in cookies {
        let _ = h.append("set-cookie", c);
    }
    Response::empty().unwrap().with_headers(h).with_status(302)
}

fn is_https(req: &Request) -> bool {
    req.url().ok().map(|u| u.as_str().starts_with("https://")).unwrap_or(false)
}

fn callback_url(req: &Request) -> String {
    let origin = req
        .url()
        .ok()
        .map(|u| {
            let s = u.as_str().to_string();
            match s.find("://").and_then(|i| s[i + 3..].find('/').map(|j| i + 3 + j)) {
                Some(end) => s[..end].to_string(),
                None => s,
            }
        })
        .unwrap_or_default();
    format!("{origin}/api/auth/oauth/callback")
}

fn read_state_cookie(req: &Request) -> Option<String> {
    let prefix = format!("{}=", engine::oauth::STATE_COOKIE);
    req.headers().get("cookie").ok().flatten().and_then(|all| {
        all.split(';').find_map(|part| {
            let part = part.trim();
            part.strip_prefix(&prefix)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        })
    })
}

pub async fn start(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let p = query::params(&req);
    let engine = match auth::engine_for(&ctx.env).await {
        Ok(e) => e,
        Err(e) => return Ok(cors::err(500, &e)),
    };
    let Some(cfg) = engine::oauth::config_from_secrets(engine.database())
        .await
        .unwrap_or(None)
    else {
        return Ok(cors::err(
            502,
            "OAuth not configured (set OAUTH_ISSUER + OAUTH_CLIENT_ID secrets)",
        ));
    };
    let discovery = match engine::oauth::discover(&cfg.issuer).await {
        Ok(d) => d,
        Err(e) => return Ok(cors::err(502, &format!("provider discovery failed: {e}"))),
    };
    let redirect = engine::oauth::sanitize_redirect(p.get("redirect").map(|s| s.as_str()).unwrap_or("/"));
    let state = uuid::Uuid::new_v4().to_string();
    // PKCE when no client secret (public clients); confidential clients send
    // the secret at the token endpoint instead.
    let verifier = if cfg.client_secret.is_none() { Some(engine::oauth::pkce_verifier()) } else { None };
    let challenge = verifier.as_deref().map(engine::oauth::pkce_challenge_s256);
    let sealed = match engine::oauth::seal_state(&state, verifier.as_deref(), &redirect) {
        Ok(s) => s,
        Err(e) => return Ok(cors::err(500, &format!("state seal failed: {e}"))),
    };
    let url = engine::oauth::authorize_url(&discovery, &cfg, &state, challenge.as_deref(), &callback_url(&req));
    let mut cookie = format!(
        "{}={sealed}; Path=/; Max-Age=600; HttpOnly; SameSite=Lax",
        engine::oauth::STATE_COOKIE
    );
    if is_https(&req) {
        cookie.push_str("; Secure");
    }
    Ok(redirect_with(&url, &[cookie]))
}

pub async fn callback(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let p = query::params(&req);
    let code = p.get("code").cloned().unwrap_or_default();
    let state = p.get("state").cloned().unwrap_or_default();
    if code.is_empty() || state.is_empty() {
        return Ok(cors::err(400, "missing code/state"));
    }
    let envelope = match read_state_cookie(&req).map(|c| engine::oauth::open_state(&c)) {
        Some(Ok(env)) => env,
        _ => return Ok(cors::err(400, "invalid or expired oauth state")),
    };
    if envelope.state != state {
        return Ok(cors::err(400, "oauth state mismatch"));
    }
    let mut engine = match auth::engine_for(&ctx.env).await {
        Ok(e) => e,
        Err(e) => return Ok(cors::err(500, &e)),
    };
    let cfg = match engine::oauth::config_from_secrets(engine.database()).await.unwrap_or(None) {
        Some(c) => c,
        None => return Ok(cors::err(502, "OAuth not configured")),
    };
    let discovery = match engine::oauth::discover(&cfg.issuer).await {
        Ok(d) => d,
        Err(e) => return Ok(cors::err(502, &format!("provider discovery failed: {e}"))),
    };
    let id_token = match engine::oauth::exchange_code(
        &discovery,
        &cfg,
        &code,
        envelope.verifier.as_deref(),
        &callback_url(&req),
    )
    .await
    {
        Ok(t) => t,
        Err(e) => return Ok(cors::err(400, &format!("token exchange failed: {e}"))),
    };
    let claims = match engine::oauth::verify_id_token(&discovery, &cfg, &id_token).await {
        Ok(c) => c,
        Err(e) => return Ok(cors::err(401, &format!("id_token rejected: {e}"))),
    };
    let email = match engine::oauth::email_from_claims(&claims) {
        Ok(e) => e,
        Err(e) => return Ok(cors::err(401, &e.to_string())),
    };
    let (token, _jwt) = match engine.oauth_login(&email).await {
        Ok(t) => t,
        Err(e) => return Ok(cors::srv(&e)),
    };
    let sep = if envelope.redirect.contains('?') { "&" } else { "?" };
    let url = format!("{}{}srv_token={token}", envelope.redirect, sep);
    let clear = format!("{}=; Path=/; Max-Age=0; HttpOnly; SameSite=Lax", engine::oauth::STATE_COOKIE);
    Ok(redirect_with(&url, &[clear]))
}
