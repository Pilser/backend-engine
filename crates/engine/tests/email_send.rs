//! Outbound email (Phase A): provider builders + engine send path.
//! Network never happens here — without an installed caller the StubCaller
//! answers 200, so these tests pin request shapes and validation.

use engine::ServerlessEngine;
use futures_executor::block_on;
use serde_json::json;

fn req(to: &str) -> engine::email::EmailRequest {
    engine::email::EmailRequest {
        to: vec![to.to_string()],
        subject: "Hi".into(),
        text: Some("hello".into()),
        html: None,
        from: None,
    }
}

#[test]
fn resend_shape() {
    let c = engine::email::build("resend", Some("k"), Some("a@x.io"), &req("b@x.io")).unwrap();
    assert_eq!(c.provider, "resend");
    assert_eq!(c.url, engine::email::RESEND_URL);
    assert!(c.headers.iter().any(|(k, v)| k == "Authorization" && v == "Bearer k"));
    assert_eq!(c.body["to"], json!(["b@x.io"]));
    assert_eq!(c.body["from"], json!("a@x.io"));
}

#[test]
fn mailchannels_shape() {
    let c = engine::email::build("mailchannels", None, Some("a@x.io"), &req("b@x.io")).unwrap();
    assert_eq!(c.provider, "mailchannels");
    assert_eq!(c.url, engine::email::MAILCHANNELS_URL);
    assert!(!c.headers.iter().any(|(k, _)| k == "X-Api-Key"));
    assert_eq!(c.body["personalizations"][0]["to"][0]["email"], json!("b@x.io"));
    let c2 = engine::email::build("mailchannels", Some("k"), Some("a@x.io"), &req("b@x.io")).unwrap();
    assert!(c2.headers.iter().any(|(k, _)| k == "X-Api-Key"));
}

#[test]
fn validation() {
    assert!(engine::email::build("resend", None, Some("a@x.io"), &req("b@x.io")).is_err());
    assert!(engine::email::build("nope", Some("k"), Some("a@x.io"), &req("b@x.io")).is_err());
    assert!(engine::email::build("resend", Some("k"), None, &req("b@x.io")).is_err());
    let mut no_body = req("b@x.io");
    no_body.text = None;
    assert!(engine::email::build("resend", Some("k"), Some("a@x.io"), &no_body).is_err());
    assert!(engine::email::parse_addrs(&json!("a@x.io, b@x.io")).unwrap().len() == 2);
    assert!(engine::email::parse_addrs(&json!("")).is_err());
}

#[test]
fn email_received_trigger() {
    assert_eq!(engine::events::EventKind::Email.name(), "email.received");
    block_on(async {
        let mut e = ServerlessEngine::with_defaults();
        let ok_recipe = engine::model::Recipe {
            name: "mail_log".into(),
            when_json: json!({"event": "email.received"}),
            match_json: None,
            enabled: true,
            dedup_on: None,
            actions_json: Some(json!([{"$log": "mail"}])),
            table: None,
        };
        assert!(e.add_recipe(&ok_recipe).await.is_ok());
    });
}

#[test]
fn engine_send_uses_secrets() {
    block_on(async {
        let mut e = ServerlessEngine::with_defaults();
        e.set_secret("MAIL_PROVIDER", "resend").await.unwrap();
        e.set_secret("MAIL_API_KEY", "k").await.unwrap();
        e.set_secret("MAIL_FROM", "a@x.io").await.unwrap();
        let out = e.send_email("b@x.io", "Hi", Some("hello"), None, None).await.unwrap();
        assert_eq!(out["provider"], json!("resend"));
        assert_eq!(out["status"], json!(200)); // StubCaller: no network in tests
    });
}
