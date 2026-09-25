use crate::model::{Key, Secret};
use crate::storage::database::{Database, Query, Row};
use crate::storage::ir::{FilterCond, Op, SrvFilter};
use crate::tables::{scoped_key, TABLE_APP_SECRETS};
use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine;
use rand::RngExt;
use serde_json::Value as Json;
use sha2::{Digest, Sha256};
use std::collections::HashMap;

const NONCE_LEN: usize = 12;

pub(crate) fn master_key() -> [u8; 32] {
    const DEV: &str = "srv-fixed-dev-master-key-do-not-use-in-production";
    let gen = match std::env::var("SRV_SECRET_KEY") {
        Ok(k) if !k.is_empty() => k,
        _ => DEV.to_string(),
    };
    let bytes = gen.as_bytes();
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = bytes[i % bytes.len().max(1)];
    }
    out
}

fn hex(digest: &[u8]) -> String {
    const MAP: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(digest.len() * 2);
    for b in digest {
        out.push(MAP[(b >> 4) as usize] as char);
        out.push(MAP[(b & 0x0f) as usize] as char);
    }
    out
}

pub fn fingerprint(plain: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(plain);
    hex(&h.finalize())
}

pub fn encrypt_value(plaintext: &[u8]) -> anyhow::Result<String> {
    let cipher = Aes256Gcm::new_from_slice(&master_key()).map_err(|e| anyhow::anyhow!("key: {e}"))?;
    let mut nonce_bytes = [0u8; NONCE_LEN];
    rand::rng().fill(&mut nonce_bytes);
    let nonce = Nonce::from(nonce_bytes);
    let ct = cipher.encrypt(&nonce, plaintext).map_err(|e| anyhow::anyhow!("encrypt: {e}"))?;
    let mut blob = nonce_bytes.to_vec();
    blob.extend_from_slice(&ct);
    Ok(base64::engine::general_purpose::STANDARD.encode(&blob))
}

pub fn decrypt_value(encoded: &str) -> anyhow::Result<Vec<u8>> {
    let blob = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|e| anyhow::anyhow!("decode: {e}"))?;
    if blob.len() < NONCE_LEN {
        anyhow::bail!("ciphertext too short");
    }
    let cipher = Aes256Gcm::new_from_slice(&master_key()).map_err(|e| anyhow::anyhow!("key: {e}"))?;
    let (nonce, ct) = blob.split_at(NONCE_LEN);
    let nonce = Nonce::from(<[u8; NONCE_LEN]>::try_from(nonce).map_err(|_| anyhow::anyhow!("nonce len"))?);
    let pt = cipher
        .decrypt(&nonce, ct)
        .map_err(|e| anyhow::anyhow!("decrypt: {e}"))?;
    Ok(pt)
}

fn secret_filter(board_id: &str, name: &str) -> SrvFilter {
    SrvFilter {
        conds: vec![
            FilterCond { field: "$.board_id".to_string(), op: Op::Eq, value: Json::String(board_id.to_string()) },
            FilterCond { field: "$.name".to_string(), op: Op::Eq, value: Json::String(name.to_string()) },
        ],
    }
}

fn secret_query(db: &dyn Database, board_id: &str) -> anyhow::Result<Vec<Secret>> {
    let q = Query {
        filter: SrvFilter {
            conds: vec![FilterCond {
                field: "$.board_id".to_string(),
                op: Op::Eq,
                value: Json::String(board_id.to_string()),
            }],
        },
        orders: vec![("$.name".to_string(), false)],
        limit: usize::MAX,
        offset: 0,
        ttl: None,
    };
    let mut out = Vec::new();
    for row in db.query(TABLE_APP_SECRETS, &q)?.rows {
        out.push(serde_json::from_value(row.data)?);
    }
    Ok(out)
}

pub fn secret_set(db: &mut dyn Database, board_id: &str, name: &str, value: &str) -> anyhow::Result<()> {
    let name = name.to_uppercase();
    let secret = Secret {
        name: name.clone(),
        value_encrypted: encrypt_value(value.as_bytes())?,
        fingerprint: fingerprint(value.as_bytes()),
    };
    db.delete(TABLE_APP_SECRETS, &secret_filter(board_id, &name))?;
    let mut data = serde_json::to_value(&secret)?;
    data["board_id"] = Json::String(board_id.to_string());
    db.insert(TABLE_APP_SECRETS, Row::new(Key::text(scoped_key(board_id, &name)), data))?;
    Ok(())
}

pub fn secret_list(db: &dyn Database, board_id: &str) -> anyhow::Result<Vec<Secret>> {
    secret_query(db, board_id)
}

pub fn secret_get(db: &dyn Database, board_id: &str, name: &str) -> anyhow::Result<Option<Secret>> {
    let key = Key::text(scoped_key(board_id, &name.to_uppercase()));
    let Some(row) = db.get(TABLE_APP_SECRETS, &key)? else {
        return Ok(None);
    };
    Ok(serde_json::from_value(row.data)?)
}

pub fn secret_value(db: &dyn Database, board_id: &str, name: &str) -> anyhow::Result<Option<String>> {
    let Some(secret) = secret_get(db, board_id, name)? else {
        return Ok(None);
    };
    let pt = decrypt_value(&secret.value_encrypted)?;
    Ok(Some(String::from_utf8(pt)?))
}

pub fn secret_remove(db: &mut dyn Database, board_id: &str, name: &str) -> anyhow::Result<()> {
    db.delete(TABLE_APP_SECRETS, &secret_filter(board_id, &name.to_uppercase()))?;
    Ok(())
}

pub fn secrets_map(db: &dyn Database, board_id: &str) -> anyhow::Result<HashMap<String, String>> {
    let mut map = HashMap::new();
    for s in secret_query(db, board_id)? {
        if let Ok(pt) = decrypt_value(&s.value_encrypted) {
            if let Ok(text) = String::from_utf8(pt) {
                map.insert(s.name, text);
            }
        }
    }
    Ok(map)
}