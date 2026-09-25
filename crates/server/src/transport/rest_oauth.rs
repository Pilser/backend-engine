use crate::Server;
use engine::model::Principal;
use http_body_util::{BodyExt, Full};
use hyper::{Response, StatusCode};
use std::collections::HashMap;

use super::rest::{err_json, BoxBodyResp};

fn redirect(url: String) -> Response<BoxBodyResp> {
    Response::builder()
        .status(StatusCode::FOUND)
        .header("location", url)
        .header("cache-control", "no-store")
        .body(BoxBodyResp::new(
            Full::new(bytes::Bytes::new())
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e)),
        ))
        .unwrap()
}

/// GET /api/srv/<board>/auth/oauth/microsoft/start
/// Redirects to the Entra authorize URL. Board-scoped config comes from the
/// board's secrets (MICROSOFT_TENANT_ID / MICROSOFT_CLIENT_ID). `redirect`
/// (the app URL to return to) and `code_challenge` may be passed as query
/// params.
pub fn microsoft_start(
    server: &Server,
    board: &str,
    params: &HashMap<String, String>,
) -> Response<BoxBodyResp> {
    let config = match server.engine.lock().unwrap().oauth_config(board) {
        Ok(Some(c)) => c,
        Ok(None) => {
            return err_json(
                StatusCode::SERVICE_UNAVAILABLE,
                "Microsoft SSO not configured for this board (set MICROSOFT_TENANT_ID and MICROSOFT_CLIENT_ID secrets)",
            )
        }
        Err(e) => return err_json(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    };
    let state = uuid::Uuid::new_v4().to_string();
    let code_challenge = params.get("code_challenge").map(|s| s.as_str());
    let redirect_uri = format!(
        "{}/api/srv/{board}/auth/oauth/microsoft/callback",
        engine_base(server)
    );
    let url = engine::oauth::authorize_url(&config, &state, code_challenge, &redirect_uri);
    // Store the state so the callback can validate it (per board, short-lived).
    if let Ok(e) = server.engine.lock() {
        let _ = e.set_oauth_state(board, &state);
    }
    redirect(url)
}

/// GET /api/srv/<board>/auth/oauth/microsoft/callback?code=...&state=...
/// Exchanges the code, verifies the id_token, logs in or creates the user by
/// email, and redirects to the app with an engine session token (as a query
/// fragment is not visible to the SPA; we use a cookie-free approach: the
/// token is appended as `?srv_token=` and the app's auth reads it once).
pub fn microsoft_callback(
    server: &Server,
    board: &str,
    params: &HashMap<String, String>,
) -> Response<BoxBodyResp> {
    let code = match params.get("code") {
        Some(c) if !c.is_empty() => c.clone(),
        _ => return err_json(StatusCode::BAD_REQUEST, "missing code"),
    };
    let state = params.get("state").map(|s| s.as_str()).unwrap_or("");
    let config = match server.engine.lock().unwrap().oauth_config(board) {
        Ok(Some(c)) => c,
        Ok(None) => return err_json(StatusCode::SERVICE_UNAVAILABLE, "Microsoft SSO not configured"),
        Err(e) => return err_json(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    };
    // Validate state (best-effort).
    let state_ok = server
        .engine
        .lock()
        .unwrap()
        .check_oauth_state(board, state);
    if !state_ok {
        return err_json(StatusCode::BAD_REQUEST, "invalid oauth state");
    }
    let redirect_uri = format!(
        "{}/api/srv/{board}/auth/oauth/microsoft/callback",
        engine_base(server)
    );
    let id_token = match engine::oauth::exchange_code(&config, &code, None, &redirect_uri) {
        Ok(t) => t,
        Err(e) => return err_json(StatusCode::BAD_REQUEST, &format!("token exchange: {e}")),
    };
    let claims = match engine::oauth::verify_id_token(&config, &id_token) {
        Ok(c) => c,
        Err(e) => return err_json(StatusCode::UNAUTHORIZED, &format!("id_token verify: {e}")),
    };
    let email = match engine::oauth::email_from_claims(&claims) {
        Ok(e) => e,
        Err(e) => return err_json(StatusCode::UNAUTHORIZED, &e.to_string()),
    };
    // Log in or create the user (role: reader; an admin can promote later).
    let result = {
        let mut e = server.engine.lock().unwrap();
        let existing = e.user_by_email(board, &email).ok().flatten();
        if existing.is_none() {
            let principal = Principal { id: board.to_string(), role: "owner".to_string(), scope: None, writer: None };
            // Create with a random password (never used; SSO is the login path).
            let pw = uuid::Uuid::new_v4().to_string();
            let _ = e.signup_user(board, &email, &pw, None, "reader", &principal);
        }
        e.login_user_by_email(board, &email)
    };
    let (token, _jwt) = match result {
        Ok(t) => t,
        Err(e) => return err_json(StatusCode::INTERNAL_SERVER_ERROR, &format!("login: {e}")),
    };
    // Redirect back to the app with the session token. The app's engineClient
    // detects ?srv_token= and persists the session.
    let app_redirect = params.get("redirect").map(|s| s.as_str()).unwrap_or("");
    let base = engine_base(server);
    let sep = if app_redirect.contains('?') { "&" } else { "?" };
    let app_url = format!("{base}{app_redirect}{sep}srv_token={token}");
    redirect(app_url)
}

fn engine_base(server: &Server) -> String {
    // OAuth requires an absolute redirect_uri Microsoft can reach. Default to
    // the configured public base (SRV_PUBLIC_URL) or localhost.
    let _ = server;
    std::env::var("SRV_PUBLIC_URL").unwrap_or_else(|_| "http://127.0.0.1:7070".to_string())
}

