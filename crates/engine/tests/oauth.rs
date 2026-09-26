//! Generic OIDC tests (Phase: provider-agnostic OAuth).
//!
//! Crypto is exercised for real (generated RSA key, sign → verify) without
//! network: discovery/exchange/callback stay live-matrix items (Phase 8).
//! Runs natively in CI via `futures-executor::block_on`.

use base64::Engine as _;
use engine::ServerlessEngine;
use futures_executor::block_on;
use serde_json::json;

fn b64u(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn unb64u(s: &str) -> Vec<u8> {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(s).unwrap()
}

#[test]
fn pkce_rfc7636_vector() {
    // RFC 7636 Appendix B test vector.
    let v = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
    assert_eq!(
        engine::oauth::pkce_challenge_s256(v),
        "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
    );
    let mine = engine::oauth::pkce_verifier();
    assert_eq!(mine.len(), 64);
    assert!(!engine::oauth::pkce_challenge_s256(&mine).is_empty());
}

#[test]
fn state_seal_roundtrip() {
    engine::secrets::set_master_key("oauth-test-master-key-00000000000001").unwrap();
    let sealed = engine::oauth::seal_state("st-1", Some("verifier-9"), "/app").unwrap();
    // Cookie-safe alphabet only.
    assert!(sealed.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'));
    let open = engine::oauth::open_state(&sealed).unwrap();
    assert_eq!(open.state, "st-1");
    assert_eq!(open.verifier.as_deref(), Some("verifier-9"));
    assert_eq!(open.redirect, "/app");
    // Tamper → rejected.
    let mut bad = sealed.clone();
    bad.pop();
    bad.push(if bad.ends_with('A') { 'B' } else { 'A' });
    assert!(engine::oauth::open_state(&bad).is_err());
    assert!(engine::oauth::open_state("!!!not-base64!!!").is_err());
}

#[test]
fn redirect_sanitizer() {
    assert_eq!(engine::oauth::sanitize_redirect("/app"), "/app");
    assert_eq!(engine::oauth::sanitize_redirect("/a/b?x=1"), "/a/b?x=1");
    assert_eq!(engine::oauth::sanitize_redirect("https://evil.test/"), "/");
    assert_eq!(engine::oauth::sanitize_redirect("//evil.test/"), "/");
    assert_eq!(engine::oauth::sanitize_redirect(""), "/");
}

#[test]
fn authorize_url_shape() {
    let d = oauth_discovery();
    let cfg = oauth_config();
    let url = engine::oauth::authorize_url(&d, &cfg, "st", Some("CHALL"), "https://app.test/cb");
    assert!(url.starts_with("https://idp.test/auth?"));
    for part in ["client_id=cli", "response_type=code", "state=st", "code_challenge=CHALL", "code_challenge_method=S256"] {
        assert!(url.contains(part), "missing {part} in {url}");
    }
    assert!(url.contains("redirect_uri=https%3A%2F%2Fapp.test%2Fcb"));
}

fn oauth_config() -> engine::oauth::OAuthConfig {
    engine::oauth::OAuthConfig {
        issuer: "https://idp.test".to_string(),
        client_id: "cli".to_string(),
        client_secret: None,
        scopes: "openid email profile".to_string(),
    }
}

fn oauth_discovery() -> engine::oauth::Discovery {
    engine::oauth::Discovery {
        authorization_endpoint: "https://idp.test/auth".to_string(),
        token_endpoint: "https://idp.test/token".to_string(),
        jwks_uri: "https://idp.test/jwks".to_string(),
    }
}

struct TestIdp {
    jwk: serde_json::Value,
    _priv_unused: (),
}

fn test_idp() -> (TestIdp, rsa::RsaPrivateKey) {
    use rsa::traits::PublicKeyParts as _;
    let privkey = rsa::RsaPrivateKey::new(&mut rand08::thread_rng(), 2048).unwrap();
    let jwk = json!({
        "kty": "RSA",
        "kid": "k1",
        "alg": "RS256",
        "n": b64u(&privkey.n().to_bytes_be()),
        "e": b64u(&privkey.e().to_bytes_be()),
    });
    (TestIdp { jwk, _priv_unused: () }, privkey)
}

fn sign(privkey: &rsa::RsaPrivateKey, claims: serde_json::Value) -> String {
    use signature::{SignatureEncoding as _, Signer as _};
    let header = b64u(br#"{"alg":"RS256","kid":"k1","typ":"JWT"}"#);
    let payload = b64u(claims.to_string().as_bytes());
    let input = format!("{header}.{payload}");
    let signer = rsa::pkcs1v15::SigningKey::<sha2_010::Sha256>::new(privkey.clone());
    let sig = signer.sign(input.as_bytes());
    format!("{input}.{}", b64u(&sig.to_vec()))
}

fn claims() -> serde_json::Value {
    let now = chrono::Utc::now().timestamp();
    json!({
        "iss": "https://idp.test",
        "aud": "cli",
        "exp": now + 300,
        "email": "Ada@X.Test",
    })
}

#[test]
fn rs256_roundtrip_and_rejections() {
    let (idp, privkey) = test_idp();
    let keys = vec![idp.jwk];
    let cfg = oauth_config();
    let now = chrono::Utc::now().timestamp();

    // Valid token: email lowercased by extraction.
    let token = sign(&privkey, claims());
    let out = engine::oauth::verify_with_keys(&keys, &cfg, &token, now).unwrap();
    assert_eq!(engine::oauth::email_from_claims(&out).unwrap(), "ada@x.test");

    // Wrong audience.
    let mut bad_aud = claims();
    bad_aud["aud"] = json!("other");
    assert!(engine::oauth::verify_with_keys(&keys, &cfg, &sign(&privkey, bad_aud), now).is_err());

    // Expired.
    let mut expired = claims();
    expired["exp"] = json!(now - 500);
    assert!(engine::oauth::verify_with_keys(&keys, &cfg, &sign(&privkey, expired), now).is_err());

    // Tampered payload (flip a byte, re-encode — signature must fail).
    let parts: Vec<&str> = token.split('.').collect();
    let mut payload = unb64u(parts[1]);
    payload[0] ^= 1;
    let bad = format!("{}.{}.{}", parts[0], b64u(&payload), parts[2]);
    assert!(engine::oauth::verify_with_keys(&keys, &cfg, &bad, now).is_err());

    // Unknown kid.
    assert!(engine::oauth::verify_with_keys(&[], &cfg, &token, now).is_err());

    // No email claim.
    let mut no_email = claims();
    no_email.as_object_mut().unwrap().remove("email");
    let out2 = engine::oauth::verify_with_keys(&keys, &cfg, &sign(&privkey, no_email), now).unwrap();
    assert!(engine::oauth::email_from_claims(&out2).is_err());
}

#[test]
fn oauth_config_from_secrets_and_login() {
    block_on(async {
        let mut e = ServerlessEngine::with_defaults();
        // Unconfigured → None.
        let none = engine::oauth::config_from_secrets(e.database()).await.unwrap();
        assert!(none.is_none());
        // Configure via tenant secrets.
        e.set_secret("OAUTH_ISSUER", "https://idp.test/").await.unwrap();
        e.set_secret("OAUTH_CLIENT_ID", "cli").await.unwrap();
        let cfg = engine::oauth::config_from_secrets(e.database()).await.unwrap().unwrap();
        assert_eq!(cfg.issuer, "https://idp.test");
        assert_eq!(cfg.client_id, "cli");
        assert!(cfg.client_secret.is_none());
        // Provision-on-login: new user becomes a reader with a session.
        let (token, _jwt) = e.oauth_login("New@X.Test").await.unwrap();
        let me = e.user_by_token(&token).await.unwrap().unwrap();
        assert_eq!(me.email, "new@x.test");
        assert_eq!(me.role, "reader");
        // Second login reuses the user.
        let (token2, _) = e.oauth_login("new@x.test").await.unwrap();
        assert!(!token2.is_empty());
    });
}
