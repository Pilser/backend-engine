use crate::model::{Hook, Key};
use crate::storage::database::{Database, Query, Row};
use crate::storage::ir::{FilterCond, Op, SrvFilter};
use crate::tables::{tenant_key, TABLE_HOOK_DELIVERIES, TABLE_HOOKS};
use base64::Engine;
use serde_json::Value as Json;
use sha2::{Digest, Sha256};

fn hex(digest: &[u8]) -> String {
    const MAP: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(digest.len() * 2);
    for b in digest {
        out.push(MAP[(b >> 4) as usize] as char);
        out.push(MAP[(b & 0x0f) as usize] as char);
    }
    out
}

fn private_ip(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_multicast()
                || v4.is_broadcast()
        }
        std::net::IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unicast_link_local()
                || v6.is_unspecified()
                || v6.is_multicast()
        }
    }
}

pub fn valid_url(url: &str) -> bool {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return false;
    }
    let Some(rest) = url.split_once("://").map(|(_, r)| r) else {
        return false;
    };
    let host = rest.split('/').next().unwrap_or("").split('@').last().unwrap_or("");
    let host = host.trim_end_matches(']');
    let host = host.split(':').next().unwrap_or("").trim_matches(|c| c == '[' || c == ']');
    if host.is_empty() || host.to_lowercase() == "localhost" {
        return false;
    }
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        if private_ip(ip) {
            return false;
        }
    }
    true
}

fn hmac_sha256(message: &[u8], secret: &[u8]) -> Vec<u8> {
    const BLOCK: usize = 64;
    let mut key = [0u8; BLOCK];
    if secret.len() > BLOCK {
        let mut h = Sha256::new();
        h.update(secret);
        let d = h.finalize();
        key[..d.len()].copy_from_slice(&d);
    } else {
        key[..secret.len()].copy_from_slice(secret);
    }
    let ipad: Vec<u8> = key.iter().map(|b| b ^ 0x36).collect();
    let opad: Vec<u8> = key.iter().map(|b| b ^ 0x5c).collect();
    let mut inner = Sha256::new();
    inner.update(&ipad);
    inner.update(message);
    let inner_hash = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(&opad);
    outer.update(&inner_hash);
    let digest = outer.finalize();
    let mut out = Vec::with_capacity(digest.len());
    out.extend_from_slice(&digest);
    out
}

pub fn hmac_base64(message: &str, secret: &str) -> String {
    base64::engine::general_purpose::STANDARD
        .encode(hmac_sha256(message.as_bytes(), secret.as_bytes()))
}

pub fn hmac_hex(message: &str, secret: &str) -> String {
    hex(&hmac_sha256(message.as_bytes(), secret.as_bytes()))
}

pub async fn webhook_secret_set(
    db: &mut dyn Database,
    secret: Option<&str>,
) -> anyhow::Result<()> {
    let mut board = crate::crud::tenant_config(db).await?;
    board.webhook_secret = secret.map(String::from);
    crate::crud::save_tenant(db, &board).await
}

fn hook_filter(url: &str) -> SrvFilter {
    SrvFilter {
        conds: vec![
            FilterCond {
                field: "$.url".to_string(),
                op: Op::Eq,
                value: Json::String(url.to_string()),
            },
        ],
    }
}

pub async fn hook_register(
    db: &mut dyn Database,
    url: &str,
    secret: Option<&str>,
) -> anyhow::Result<()> {
    if !valid_url(url) {
        anyhow::bail!("webhook url must start with http:// or https://");
    }
    let hook = Hook { url: url.to_string(), secret: secret.map(String::from) };
    let mut data = serde_json::to_value(&hook)?;
    data["created_at"] = Json::String(crate::crud::now_str());
    db.delete(TABLE_HOOKS, &hook_filter(url)).await?;
    db.insert(TABLE_HOOKS, Row::new(Key::text(tenant_key(url)), data)).await?;
    Ok(())
}

pub async fn hook_list(db: &dyn Database) -> anyhow::Result<Vec<Hook>> {
    let q = Query {
        filter: SrvFilter {
            conds: Vec::new(),
        },
        orders: vec![("$.created_at".to_string(), false)],
        limit: usize::MAX,
        offset: 0,
        ttl: None,
    };
    let mut out = Vec::new();
    for row in db.query(TABLE_HOOKS, &q).await?.rows {
        out.push(serde_json::from_value(row.data)?);
    }
    Ok(out)
}

pub async fn hook_remove(db: &mut dyn Database, url: &str) -> anyhow::Result<()> {
    if db.delete(TABLE_HOOKS, &hook_filter(url)).await? == 0 {
        anyhow::bail!("hook not found");
    }
    Ok(())
}

pub async fn enqueue_delivery(
    db: &mut dyn Database,
    url: &str,
    payload: &Json,
) -> anyhow::Result<()> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = crate::crud::now_str();
    let delivery = serde_json::json!({
        "id": id,
        "url": url,
        "payload": payload,
        "attempts": 0,
        "status": "pending",
        "next_attempt": now,
        "created_at": now,
    });
    db.insert(TABLE_HOOK_DELIVERIES, Row::new(Key::text(&id), delivery)).await?;
    Ok(())
}

pub async fn fire_hooks(
    db: &mut dyn Database,
    payload: &Json,
) -> anyhow::Result<()> {
    for hook in hook_list(db).await? {
        enqueue_delivery(db, &hook.url, payload).await?;
    }
    Ok(())
}

pub async fn hook_deliveries_due(
    db: &dyn Database,
    now_iso: &str,
    limit: usize,
) -> anyhow::Result<Vec<Json>> {
    let q = Query {
        filter: SrvFilter {
            conds: vec![FilterCond {
                field: "$.next_attempt".to_string(),
                op: Op::Lte,
                value: Json::String(now_iso.to_string()),
            }],
        },
        orders: vec![("$.created_at".to_string(), true)],
        limit: limit * 4,
        offset: 0,
        ttl: None,
    };
    let mut out = Vec::new();
    for row in db.query(TABLE_HOOK_DELIVERIES, &q).await?.rows {
        if out.len() >= limit {
            break;
        }
        if row.data.get("delivered_at").map(|v| !v.is_null()).unwrap_or(false) {
            continue;
        }
        match row.data.get("next_attempt").and_then(|v| v.as_str()) {
            Some(next) if next <= now_iso => out.push(row.data),
            _ => {}
        }
    }
    Ok(out)
}

pub async fn hook_mark_delivery(
    db: &mut dyn Database,
    id: &str,
    attempts: i64,
    last_status: Option<&str>,
    next_attempt: Option<&str>,
    delivered_at: Option<&str>,
) -> anyhow::Result<()> {
    let key = Key::text(id.to_string());
    let Some(row) = db.get(TABLE_HOOK_DELIVERIES, &key).await? else {
        return Ok(());
    };
    let mut data = row.data;
    data["attempts"] = Json::from(attempts);
    match last_status {
        Some(s) => data["last_status"] = Json::String(s.to_string()),
        None => {
            data.as_object_mut().map(|m| m.remove("last_status"));
        }
    }
    match next_attempt {
        Some(n) => data["next_attempt"] = Json::String(n.to_string()),
        None => {
            data.as_object_mut().map(|m| m.remove("next_attempt"));
        }
    }
    match delivered_at {
        Some(d) => data["delivered_at"] = Json::String(d.to_string()),
        None => {
            data.as_object_mut().map(|m| m.remove("delivered_at"));
        }
    }
    db.update(TABLE_HOOK_DELIVERIES, &key, &data).await?;
    Ok(())
}