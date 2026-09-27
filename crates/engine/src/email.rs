//! Outbound email: the recipe `$send_email` action, the `email send` CLI verb,
//! and `POST /api/email/send`.
//!
//! Provider credentials live in app SECRETS (never in recipes — the recipe
//! only carries to/subject/body, all templatable from the payload):
//! - `MAIL_PROVIDER`: `resend` (default) or `mailchannels`.
//! - `MAIL_API_KEY`: provider key. Required for resend; for mailchannels it
//!   is sent as `X-Api-Key` only when set (domain-locked sending works
//!   without one from Workers).
//! - `MAIL_FROM`: default sender, e.g. `hello@example.com`.
//!
//! No raw SMTP: Workers have no TCP sockets from WASM, so providers are
//! HTTPS APIs. Anything else stays a `$call` recipe away.

use serde_json::{json, Value as Json};

pub const SECRET_PROVIDER: &str = "MAIL_PROVIDER";
pub const SECRET_API_KEY: &str = "MAIL_API_KEY";
pub const SECRET_FROM: &str = "MAIL_FROM";

pub const RESEND_URL: &str = "https://api.resend.com/emails";
pub const MAILCHANNELS_URL: &str = "https://api.mailchannels.net/tx/v1/send";

pub struct EmailRequest {
    pub to: Vec<String>,
    pub subject: String,
    pub text: Option<String>,
    pub html: Option<String>,
    pub from: Option<String>,
}

/// A fully-resolved provider call: fixed URL (no SSRF surface — the recipe
/// never supplies a URL), headers, JSON body.
pub struct ProviderCall {
    pub provider: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Json,
}

/// Accept `"a@x.io"` / `"a@x.io, b@x.io"` / `["a@x.io", ...]`.
pub fn parse_addrs(v: &Json) -> anyhow::Result<Vec<String>> {
    let mut out = Vec::new();
    match v {
        Json::String(s) => {
            for part in s.split(',') {
                let t = part.trim();
                if !t.is_empty() {
                    out.push(t.to_string());
                }
            }
        }
        Json::Array(arr) => {
            for el in arr {
                let s = el.as_str().unwrap_or("").trim();
                if !s.is_empty() {
                    out.push(s.to_string());
                }
            }
        }
        _ => anyhow::bail!("\"to\" must be an address string or array"),
    }
    if out.is_empty() {
        anyhow::bail!("\"to\" needs at least one address");
    }
    Ok(out)
}

pub fn build(
    provider: &str,
    api_key: Option<&str>,
    from_default: Option<&str>,
    req: &EmailRequest,
) -> anyhow::Result<ProviderCall> {
    if req.to.is_empty() {
        anyhow::bail!("\"to\" needs at least one address");
    }
    if req.subject.trim().is_empty() {
        anyhow::bail!("\"subject\" must not be empty");
    }
    if req.text.as_deref().map(str::is_empty).unwrap_or(true)
        && req.html.as_deref().map(str::is_empty).unwrap_or(true)
    {
        anyhow::bail!("one of \"text\" or \"html\" is required");
    }
    let from = req
        .from
        .clone()
        .or_else(|| from_default.map(String::from))
        .ok_or_else(|| anyhow::anyhow!("no sender: pass \"from\" or set the MAIL_FROM secret"))?;
    match provider.to_ascii_lowercase().as_str() {
        "resend" => {
            let key = api_key
                .filter(|k| !k.is_empty())
                .ok_or_else(|| anyhow::anyhow!("resend needs the MAIL_API_KEY secret"))?;
            let mut body = serde_json::Map::new();
            body.insert("from".into(), Json::String(from));
            body.insert("to".into(), Json::Array(req.to.iter().cloned().map(Json::String).collect()));
            body.insert("subject".into(), Json::String(req.subject.clone()));
            if let Some(t) = &req.text {
                body.insert("text".into(), Json::String(t.clone()));
            }
            if let Some(h) = &req.html {
                body.insert("html".into(), Json::String(h.clone()));
            }
            Ok(ProviderCall {
                provider: "resend".into(),
                url: RESEND_URL.into(),
                headers: vec![
                    ("Authorization".into(), format!("Bearer {key}")),
                    ("Content-Type".into(), "application/json".into()),
                ],
                body: Json::Object(body),
            })
        }
        "mailchannels" => {
            let content: Vec<Json> = [("text", "text/plain"), ("html", "text/html")]
                .into_iter()
                .filter_map(|(k, ctype)| {
                    let v = if k == "text" { &req.text } else { &req.html };
                    v.as_ref().map(|s| {
                        json!({ "type": ctype, "value": s })
                    })
                })
                .collect();
            let mut headers = vec![("Content-Type".into(), "application/json".into())];
            if let Some(k) = api_key.filter(|k| !k.is_empty()) {
                headers.push(("X-Api-Key".into(), k.to_string()));
            }
            Ok(ProviderCall {
                provider: "mailchannels".into(),
                url: MAILCHANNELS_URL.into(),
                headers,
                body: json!({
                    "personalizations": [{ "to": req.to.iter().map(|e| json!({ "email": e })).collect::<Vec<_>>() }],
                    "from": { "email": from },
                    "subject": req.subject,
                    "content": content,
                }),
            })
        }
        other => anyhow::bail!("unknown MAIL_PROVIDER '{other}' (resend|mailchannels)"),
    }
}
