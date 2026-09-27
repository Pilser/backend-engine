use crate::model::{Key, KeyRecord, Principal};
use crate::storage::database::{Database, Query, Row};
use crate::storage::ir::{FilterCond, Op, SrvFilter};
use crate::tables::{tenant_key, TABLE_KEYS, TABLE_SESSIONS, TABLE_USERS};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde_json::{json, Value as Json};
use sha2::{Digest, Sha256};

pub fn role_rank(role: &str) -> u8 {
    match role {
        "customer" => 0,
        "reader" | "list" => 1,
        "writer" => 2,
        "admin" => 3,
        "owner" => 4,
        _ => 0,
    }
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

pub fn hash_key(salt: &str, secret: &str) -> String {
    let mut h = Sha256::new();
    h.update(salt.as_bytes());
    h.update(secret.as_bytes());
    hex(&h.finalize())
}

pub async fn issue_key(
    db: &mut dyn Database,
    role: &str,
    writer: Option<&str>,
    scope: Option<&str>,
    tables: Option<Vec<String>>,
) -> anyhow::Result<(KeyRecord, String)> {
    let role = if role.is_empty() { "writer" } else { role };
    if role == "customer" && scope.is_none() {
        anyhow::bail!("customer keys require a scope (customer id)");
    }
    if role != "customer" && scope.is_some() {
        anyhow::bail!("only customer keys may carry a scope");
    }
    if tables.as_ref().map(|t| t.is_empty()).unwrap_or(false) {
        anyhow::bail!("tables scope must list at least one table (omit for all tables)");
    }
    let bucket = uuid::Uuid::new_v4().to_string();
    let salt = uuid::Uuid::new_v4().to_string();
    let key_secret = uuid::Uuid::new_v4().to_string();
    let rec = KeyRecord {
        bucket: bucket.clone(),
        key_hash: hash_key(&salt, &key_secret),
        salt,
        role: role.to_string(),
        writer: writer.map(String::from),
        scope: scope.map(String::from),
        revoked_at: None,
        tables,
    };
    db.insert(TABLE_KEYS, Row::new(Key::text(&bucket), serde_json::to_value(&rec)?)).await?;
    Ok((rec, key_secret))
}

pub async fn list_keys(db: &dyn Database) -> anyhow::Result<Vec<KeyRecord>> {
    let q = Query {
        filter: SrvFilter { conds: Vec::new() },
        orders: vec![],
        limit: usize::MAX,
        offset: 0,
    
        ttl: None,
    };
    let mut out = Vec::new();
    for row in db.query(TABLE_KEYS, &q).await?.rows {
        out.push(serde_json::from_value(row.data)?);
    }
    Ok(out)
}

pub async fn get_key(db: &dyn Database, bucket: &str) -> anyhow::Result<Option<KeyRecord>> {
    let key = Key::text(bucket);
    let Some(row) = db.get(TABLE_KEYS, &key).await? else {
        return Ok(None);
    };
    Ok(serde_json::from_value(row.data)?)
}

async fn find_active_key(db: &dyn Database, secret: &str) -> anyhow::Result<Option<KeyRecord>> {
    let q = Query {
        filter: SrvFilter {
            conds: vec![
                FilterCond { field: "$.revoked_at".to_string(), op: Op::Eq, value: Json::Null },
            ],
        },
        orders: vec![],
        limit: usize::MAX,
        offset: 0,
        ttl: None,
    };
    for row in db.query(TABLE_KEYS, &q).await?.rows {
        let kr: KeyRecord = serde_json::from_value(row.data)?;
        if hash_key(&kr.salt, secret) == kr.key_hash {
            return Ok(Some(kr));
        }
    }
    Ok(None)
}

pub async fn revoke_key(db: &mut dyn Database, bucket: &str) -> anyhow::Result<()> {
    let filter = SrvFilter {
        conds: vec![FilterCond { field: "$.bucket".to_string(), op: Op::Eq, value: Json::String(bucket.to_string()) }],
    };
    let q = Query { filter: filter.clone(), orders: vec![], limit: 1, offset: 0, ttl: None };
    let Some(row) = db.query(TABLE_KEYS, &q).await?.rows.into_iter().next() else {
        anyhow::bail!("key {bucket} not found");
    };
    let mut kr: KeyRecord = serde_json::from_value(row.data)?;
    kr.revoked_at = Some(crate::crud::now_str());
    db.delete(TABLE_KEYS, &filter).await?;
    db.insert(TABLE_KEYS, Row::new(Key::text(bucket), serde_json::to_value(&kr)?)).await?;
    Ok(())
}

pub async fn resolve_principal(
    db: &dyn Database,
    token: Option<&str>,
    scope: Option<&str>,
) -> anyhow::Result<Principal> {
    if let Some(t) = token {
        if let Some(u) = resolve_session(db, t).await? {
            return Ok(Principal {
                id: u.email.clone(),
                role: u.role.clone(),
                scope: None,
                writer: Some(u.email.clone()),
                tables: None,
            });
        }
        if let Some(u) = resolve_jwt_user(t) {
            return Ok(Principal {
                id: u.email.clone(),
                role: u.role.clone(),
                scope: None,
                writer: Some(u.email.clone()),
                tables: None,
            });
        }
        if let Some(kr) = find_active_key(db, t).await? {
            return Ok(Principal {
                id: kr.bucket.clone(),
                role: kr.role.clone(),
                scope: kr.scope.clone().or_else(|| scope.map(String::from)),
                writer: kr.writer.clone(),
                tables: kr.tables.clone(),
            });
        }
    }
    Ok(Principal { id: "anon".to_string(), role: "none".to_string(), scope: None, writer: None, tables: None })
}

pub fn force_scope(principal: &Principal, payload: &mut serde_json::Value) {
    if let Some(scope) = &principal.scope {
        if let Some(obj) = payload.as_object_mut() {
            obj.insert("customer_id".to_string(), Json::String(scope.clone()));
        }
    }
}

pub fn customer_cond(principal: &Principal) -> Option<FilterCond> {
    principal.scope.as_ref().map(|s| FilterCond {
        field: "$.customer_id".to_string(),
        op: Op::Eq,
        value: Json::String(s.clone()),
    })
}

pub fn require_role(principal: &Principal, min: u8) -> anyhow::Result<()> {
    if role_rank(&principal.role) < min {
        anyhow::bail!("forbidden: requires role rank {min} (principal role: {})", principal.role);
    }
    Ok(())
}

// ---- user auth (password login + sessions) --------------------------------

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct User {
    pub email: String,
    pub role: String,
    pub created_at: String,
}

const SESSION_TTL_SECS: i64 = 60 * 60 * 24 * 7;

fn now_iso() -> String {
    crate::crud::now_str()
}

fn expires_iso() -> String {
    crate::crud::subtract_seconds(&now_iso(), -SESSION_TTL_SECS)
}

fn user_key(email: &str) -> Key {
    Key::text(tenant_key(&email.to_lowercase()))
}

pub(crate) async fn find_user(db: &dyn Database, email: &str) -> anyhow::Result<Option<Json>> {
    Ok(db.get(TABLE_USERS, &user_key(email)).await?.map(|r| r.data))
}

/// List the tenant's users (email, role, created_at — never the hash).
pub async fn user_list(db: &dyn Database) -> anyhow::Result<Vec<User>> {
    let q = Query {
        filter: SrvFilter { conds: Vec::new() },
        orders: vec![],
        limit: usize::MAX,
        offset: 0,
        ttl: None,
    };
    let mut out = Vec::new();
    for row in db.query(TABLE_USERS, &q).await?.rows {
        out.push(serde_json::from_value(row.data)?);
    }
    Ok(out)
}

/// Change a user's role. The caller must be admin/owner. Also expires the
/// user's sessions so stale tokens don't keep the old role.
pub async fn user_set_role(
    db: &mut dyn Database,
    email: &str,
    role: &str,
    caller: &Principal,
) -> anyhow::Result<User> {
    require_role(caller, role_rank("admin"))?;
    let email = email.to_lowercase();
    let key = user_key(&email);
    let Some(row) = db.get(TABLE_USERS, &key).await? else {
        anyhow::bail!("user {email} not found");
    };
    let mut data = row.data;
    data["role"] = Json::String(role.to_string());
    db.update(TABLE_USERS, &key, &data).await?;
    // Expire the user's sessions (delete them).
    let q = Query {
        filter: SrvFilter {
            conds: vec![
                FilterCond { field: "$.email".to_string(), op: Op::Eq, value: Json::String(email.clone()) },
            ],
        },
        orders: vec![],
        limit: usize::MAX,
        offset: 0,
        ttl: None,
    };
    db.delete(TABLE_SESSIONS, &q.filter).await?;
    let user: User = serde_json::from_value(data)?;
    Ok(user)
}

/// Salted SHA-256 password hash, stored as `v1$<salt>$<hex>`.
/// bcrypt was removed: ~200ms CPU per login is fatal on Workers; see
/// WORKER-PORT-GUIDE.md §3. Legacy `$2` (bcrypt) hashes are NOT accepted —
/// re-register the user to migrate.
pub fn hash_password(password: &str) -> String {
    let salt = uuid::Uuid::new_v4().to_string();
    format!("v1${salt}${}", hash_key(&salt, password))
}

fn verify_password(password: &str, stored: &str) -> bool {
    let mut parts = stored.splitn(3, '$');
    match (parts.next(), parts.next(), parts.next()) {
        (Some("v1"), Some(salt), Some(expected)) => {
            constant_time_eq(hash_key(salt, password).as_bytes(), expected.as_bytes())
        }
        _ => false,
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Create a user. `role` is clamped: only an admin/owner caller may set a role
/// above reader; otherwise the new user is a reader.
/// Two modes, chosen per-call:
/// - `password_hash: Some(hash)` — import an existing `v1$…` hash verbatim.
///   Only accepted when the caller is admin/owner (same gate as the role
///   clamp), and the hash must use the current format (starts with `v1$`).
/// - `password_hash: None` — the engine salted-hashes `password` (>= 6 chars).
pub async fn user_signup(
    db: &mut dyn Database,
    email: &str,
    password: &str,
    password_hash: Option<&str>,
    role: &str,
    caller: &Principal,
) -> anyhow::Result<User> {
    if email.is_empty() {
        anyhow::bail!("email is required");
    }
    let email = email.to_lowercase();
    if find_user(db, &email).await?.is_some() {
        anyhow::bail!("user already exists");
    }
    let granted = if role_rank(&caller.role) >= role_rank("admin") { role } else { "reader" };
    let password_hash = match password_hash {
        Some(hash) => {
            require_role(caller, role_rank("admin"))?;
            if !hash.starts_with("v1$") {
                anyhow::bail!("password_hash must be a v1 sha256 hash (starts with v1$)");
            }
            hash.to_string()
        }
        None => {
            if password.len() < 6 {
                anyhow::bail!("a password of at least 6 characters is required");
            }
            hash_password(password)
        }
    };
    let user = User {
        email: email.clone(),
        role: granted.to_string(),
        created_at: now_iso(),
    };
    let mut data = serde_json::to_value(&user)?;
    data["password_hash"] = Json::String(password_hash);
    db.insert(TABLE_USERS, Row::new(user_key(&email), data)).await?;
    Ok(user)
}

/// Verify credentials and create a session token plus an optional signed JWT.
/// Returns `(opaque_token, jwt)`. The opaque token is revocable via logout; the
/// JWT is a self-contained HS256-signed credential (expiry 7 days).
pub async fn user_login(
    db: &mut dyn Database,
    email: &str,
    password: &str,
) -> anyhow::Result<(String, String)> {
    let row = find_user(db, email).await?
        .ok_or_else(|| anyhow::anyhow!("invalid email or password"))?;
    let stored = row
        .get("password_hash")
        .and_then(|h| h.as_str())
        .ok_or_else(|| anyhow::anyhow!("invalid email or password"))?;
    if !verify_password(password, stored) {
        anyhow::bail!("invalid email or password");
    }
    let email = row["email"].as_str().unwrap_or(email).to_string();
    let role = row["role"].as_str().unwrap_or("reader").to_string();
    issue_session(db, &email, &role).await
}

/// Create a session (token + JWT) for an already-verified user. Used by
/// password login (SSO was removed for the Workers port; see oauth removal).
pub async fn issue_session(
    db: &mut dyn Database,
    email: &str,
    role: &str,
) -> anyhow::Result<(String, String)> {
    let now = now_iso();
    let token = uuid::Uuid::new_v4().to_string();
    let session = json!({
        "token": token,
        "email": email,
        "role": role,
        "expires_at": expires_iso(),
        "created_at": now,
    });
    db.insert(TABLE_SESSIONS, Row::new(Key::text(&token), session)).await?;
    let jwt = issue_jwt(email, role);
    Ok((token, jwt))
}

fn issue_jwt(email: &str, role: &str) -> String {
    let claims = json!({
        "sub": email,
        "role": role,
        "iat": now_iso(),
        "exp": expires_iso(),
    });
    sign_jwt(&claims, &crate::secrets::master_key())
}

fn resolve_jwt_user(token: &str) -> Option<User> {
    let claims = verify_jwt(token, &crate::secrets::master_key())?;
    let exp = claims.get("exp").and_then(|e| e.as_str()).unwrap_or("");
    if !exp.is_empty() && exp < now_iso().as_str() {
        return None;
    }
    let email = claims.get("sub").and_then(|s| s.as_str())?.to_string();
    let role = claims.get("role").and_then(|r| r.as_str()).unwrap_or("reader").to_string();
    Some(User { email, role, created_at: now_iso() })
}

fn b64url(data: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(data)
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut k = key.to_vec();
    if k.len() > BLOCK {
        k = Sha256::digest(&k).to_vec();
    }
    k.resize(BLOCK, 0);
    let mut ipad = [0x36u8; BLOCK];
    let mut opad = [0x5cu8; BLOCK];
    for i in 0..BLOCK {
        ipad[i] ^= k[i];
        opad[i] ^= k[i];
    }
    let mut inner = Vec::with_capacity(BLOCK + data.len());
    inner.extend_from_slice(&ipad);
    inner.extend_from_slice(data);
    let inner = Sha256::digest(&inner);
    let mut outer = Vec::with_capacity(BLOCK + 32);
    outer.extend_from_slice(&opad);
    outer.extend_from_slice(&inner);
    Sha256::digest(&outer).into()
}

fn sign_jwt(claims: &Json, secret: &[u8; 32]) -> String {
    let header = json!({ "alg": "HS256", "typ": "JWT" });
    let h = b64url(header.to_string().as_bytes());
    let p = b64url(claims.to_string().as_bytes());
    let input = format!("{h}.{p}");
    let sig = b64url(&hmac_sha256(secret, input.as_bytes()));
    format!("{input}.{sig}")
}

fn verify_jwt(token: &str, secret: &[u8; 32]) -> Option<Json> {
    let mut parts = token.split('.');
    let h = parts.next()?;
    let p = parts.next()?;
    let sig = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    let input = format!("{h}.{p}");
    if sig != b64url(&hmac_sha256(secret, input.as_bytes())) {
        return None;
    }
    let payload = URL_SAFE_NO_PAD.decode(p).ok()?;
    serde_json::from_slice(&payload).ok()
}

/// Resolve a session token to its user, if present and unexpired.
pub async fn resolve_session(
    db: &dyn Database,
    token: &str,
) -> anyhow::Result<Option<User>> {
    let Some(row) = db.get(TABLE_SESSIONS, &Key::text(token)).await? else {
        return Ok(None);
    };
    let s = row.data;
    if let Some(exp) = s.get("expires_at").and_then(|e| e.as_str()) {
        if !exp.is_empty() && exp < now_iso().as_str() {
            return Ok(None);
        }
    }
    let email = s.get("email").and_then(|e| e.as_str()).unwrap_or("").to_string();
    let role = s.get("role").and_then(|r| r.as_str()).unwrap_or("reader").to_string();
    Ok(Some(User {
        email,
        role,
        created_at: now_iso(),
    }))
}

/// Revoke a session token.
pub async fn user_logout(db: &mut dyn Database, token: &str) -> anyhow::Result<bool> {
    let existed = db.get(TABLE_SESSIONS, &Key::text(token)).await?.is_some();
    if existed {
        db.delete(
            TABLE_SESSIONS,
            &SrvFilter {
                conds: vec![FilterCond {
                    field: "$.token".to_string(),
                    op: Op::Eq,
                    value: Json::String(token.to_string()),
                }],
            },
        ).await?;
    }
    Ok(existed)
}