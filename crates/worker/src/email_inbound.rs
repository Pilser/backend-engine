//! Inbound email (Cloudflare Email Routing `email()` handler, Phase A).
//!
//! Point the domain's Email Routing at this worker (Cloudflare dashboard —
//! no code, no extra worker). Each message is stored in the `email_log`
//! table (`direction: "in"`, body truncated to 4000 chars like the outbound
//! log) and `email.received` recipes run against
//! `{from, to, subject, body, ...}`.
//!
//! Anti-abuse is recipe-side: gate with `match: {"from": ...}` allowlists.
//! Recipe dedup guards repeats; never `$send_email` back to `{{$.from}}`
//! unconditionally (bounce loops).

use engine::model::Principal;
use serde_json::json;
use worker::{Env, ForwardableEmailMessage, Result};

const BODY_LIMIT: usize = 4000;

fn trunc(s: &str, n: usize) -> String {
    if s.len() <= n {
        return s.to_string();
    }
    let mut end = n;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// Best-effort text extraction: plain body, or the first text/plain MIME
/// part of a multipart message. Raw fidelity is intentionally NOT kept —
/// the record is a trigger payload, not an archive.
fn text_body(raw: &str) -> String {
    let norm = raw.replace("\r\n", "\n");
    if let Some(pos) = norm.to_lowercase().find("content-type: text/plain") {
        let after = &norm[pos..];
        if let Some(i) = after.find("\n\n") {
            let part = &after[i + 2..];
            let end = part.find("\n--").map(|e| e).unwrap_or(part.len());
            return part[..end].trim().to_string();
        }
    }
    norm.split_once("\n\n").map(|(_, b)| b.trim().to_string()).unwrap_or_default()
}

fn sys_principal() -> Principal {
    Principal { id: engine::TENANT.to_string(), role: "owner".to_string(), scope: None, writer: None, tables: None }
}

pub async fn handle(message: ForwardableEmailMessage, env: &Env) -> Result<()> {
    let mut engine = crate::auth::engine_for(env)
        .await
        .map_err(worker::Error::RustError)?;
    engine.install_http_caller(Box::new(crate::http_caller::FetchCaller));

    let from = message.from();
    let to = message.to();
    let subject = message
        .headers()
        .get("subject")
        .unwrap_or(None)
        .unwrap_or_default();
    let raw = message.raw_bytes().await.unwrap_or_default();
    let body = trunc(&text_body(&String::from_utf8_lossy(&raw)), BODY_LIMIT);

    // Convention table (schemaless, like `files`): ensure idempotently.
    let _ = engine.create_table("email_log", None, None).await;
    let payload = json!({
        "direction": "in",
        "from": from,
        "to": to,
        "subject": subject,
        "body": body,
    });
    let seq = engine
        .insert_record("email_log", payload.clone(), None, false, &sys_principal())
        .await
        .map_err(|e| worker::Error::RustError(e.to_string()))?;
    engine
        .dispatch_recipes("email_log", engine::events::EventKind::Email, Some(seq), Some(payload))
        .await
        .map_err(|e| worker::Error::RustError(e.to_string()))?;
    Ok(())
}
