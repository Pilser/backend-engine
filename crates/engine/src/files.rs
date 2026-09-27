use crate::model::{Key, Principal, SubApp};
use crate::storage::database::{Database, Query, Row};
use crate::storage::ir::{FilterCond, Op, SrvFilter};
use crate::storage::object_store::{KeyInfo, ObjectStore};
use crate::tables::{tenant_key, TABLE_RECORDS, TABLE_SUBAPPS};
use serde_json::{json, Value as Json};

fn file_id() -> String {
    const CHARS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    // chrono reads the host clock on wasm (js_sys::Date); SystemTime panics.
    let nanos = chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0) as u64;
    let mut out = String::from("f_");
    let mut n = nanos;
    for _ in 0..16 {
        out.push(CHARS[(n % 36) as usize] as char);
        n /= 36;
    }
    out
}

pub async fn file_upload(
    db: &mut dyn Database,
    store: &dyn ObjectStore,
    table: &str,
    filename: &str,
    content_type: &str,
    bytes: &[u8],
    meta: &Json,
    folder: Option<&str>,
) -> anyhow::Result<i64> {
    // Optional subfolder for organization: {tenant}/files/{folder}/f_<id>.bin.
    // Without a folder (the default), uploads stay flat: {tenant}/files/f_<id>.bin.
    let tenant = crate::TENANT;
    let key = match folder {
        Some(f) if !f.is_empty() => format!("{tenant}/files/{}/f_{}.bin", f, file_id()),
        _ => format!("{tenant}/files/f_{}.bin", file_id()),
    };
    store.put(&key, bytes).await?;
    let mut payload = serde_json::json!({
        "file": key,
        "name": filename,
        "type": content_type,
        "size": bytes.len(),
    });
    if let Some(obj) = meta.as_object() {
        if let Some(p) = payload.as_object_mut() {
            for (k, v) in obj {
                p.insert(k.clone(), v.clone());
            }
        }
    }
    let principal =
        Principal { id: crate::TENANT.to_string(), role: "owner".to_string(), scope: None, writer: None, tables: None };
    crate::crud::record_insert(db, table, payload, None, false, &principal).await
}

async fn content_type_for(db: &dyn Database, table: &str, file: &str) -> anyhow::Result<String> {
    let q = Query {
        filter: SrvFilter {
            conds: vec![
                FilterCond {
                    field: "$.table".to_string(),
                    op: Op::Eq,
                    value: Json::String(table.to_string()),
                },
                FilterCond {
                    field: "$.payload.file".to_string(),
                    op: Op::Eq,
                    value: Json::String(file.to_string()),
                },
            ],
        },
        orders: vec![],
        limit: 1,
        offset: 0,
        ttl: None,
    };
    let Some(row) = db.query(TABLE_RECORDS, &q).await?.rows.into_iter().next() else {
        return Ok("application/octet-stream".to_string());
    };
    let ct = row
        .data
        .pointer("/payload/type")
        .and_then(|v| v.as_str())
        .unwrap_or("application/octet-stream");
    Ok(ct.to_string())
}

pub async fn file_download(
    db: &dyn Database,
    store: &dyn ObjectStore,
    table: &str,
    file: &str,
) -> anyhow::Result<Option<(Vec<u8>, String)>> {
    let prefix = format!("{}/files/", crate::TENANT);
    if !file.starts_with(&prefix) {
        return Ok(None);
    }
    let Some(bytes) = store.get(file).await? else {
        return Ok(None);
    };
    let content_type = content_type_for(db, table, file).await?;
    Ok(Some((bytes, content_type)))
}

pub async fn file_list(store: &dyn ObjectStore) -> anyhow::Result<Vec<KeyInfo>> {
    store.list(&format!("{}/files/", crate::TENANT)).await
}

// ---- tenant static assets (front-end hosting) -------------------------

pub fn asset_prefix() -> String {
    format!("{}/assets/", crate::TENANT)
}

pub fn sanitize_asset_path(rel: &str) -> anyhow::Result<String> {
    let rel = rel.trim_start_matches('/');
    if rel.is_empty() {
        anyhow::bail!("asset path is empty");
    }
    if rel.split('/').any(|s| s.is_empty() || s == "." || s == "..") {
        anyhow::bail!("asset path contains invalid segments");
    }
    if rel.starts_with('\\') || rel.contains('\\') {
        anyhow::bail!("asset path uses backslashes");
    }
    Ok(rel.to_string())
}

pub fn asset_key(rel: &str) -> anyhow::Result<String> {
    let rel = sanitize_asset_path(rel)?;
    Ok(format!("{}{rel}", asset_prefix()))
}

pub fn content_type_from_path(rel: &str) -> &'static str {
    let lower = rel.to_ascii_lowercase();
    let ext = lower.rsplit('.').next().unwrap_or("");
    match ext {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" => "application/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        "map" => "application/json",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "eot" => "application/vnd.ms-fontobject",
        "txt" => "text/plain; charset=utf-8",
        "md" => "text/markdown; charset=utf-8",
        "pdf" => "application/pdf",
        "wasm" => "application/wasm",
        "mp3" => "audio/mpeg",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "xml" => "application/xml",
        "csv" => "text/csv; charset=utf-8",
        _ => "application/octet-stream",
    }
}

pub async fn asset_put(
    store: &dyn ObjectStore,
    rel: &str,
    bytes: &[u8],
) -> anyhow::Result<()> {
    let key = asset_key(rel)?;
    store.put(&key, bytes).await?;
    Ok(())
}

pub async fn asset_get(
    store: &dyn ObjectStore,
    rel: &str,
) -> anyhow::Result<Option<(Vec<u8>, String)>> {
    let key = asset_key(rel)?;
    let Some(bytes) = store.get(&key).await? else {
        return Ok(None);
    };
    Ok(Some((bytes, content_type_from_path(&key).to_string())))
}

pub async fn asset_head(store: &dyn ObjectStore, rel: &str) -> anyhow::Result<Option<crate::storage::object_store::BlobMeta>> {
    let key = asset_key(rel)?;
    store.head(&key).await
}

pub async fn asset_delete(store: &dyn ObjectStore, rel: &str) -> anyhow::Result<bool> {
    let key = asset_key(rel)?;
    if store.head(&key).await?.is_none() {
        return Ok(false);
    }
    store.delete(&key).await?;
    Ok(true)
}

/// Delete every asset under a relative prefix (e.g. a sub-app slug) and
/// return how many files were removed.
pub async fn asset_delete_prefix(store: &dyn ObjectStore, prefix: &str) -> anyhow::Result<usize> {
    let p = sanitize_asset_path(prefix)?;
    let full_prefix = format!("{}{}/", asset_prefix(), p);
    let keys = store.list(&full_prefix).await?;
    let mut n = 0usize;
    for k in keys {
        store.delete(&k.key).await?;
        n += 1;
    }
    Ok(n)
}

pub async fn asset_list(store: &dyn ObjectStore) -> anyhow::Result<Vec<KeyInfo>> {
    let prefix = asset_prefix();
    Ok(store.list(&prefix).await?
        .into_iter()
        .map(|k| KeyInfo { key: k.key.trim_start_matches(&prefix).to_string(), size: k.size })
        .collect())
}

// ---- sub-apps (named dist folders under a board's asset namespace) -------

pub fn validate_slug(slug: &str) -> anyhow::Result<()> {
    let slug = slug.trim_matches('/');
    if slug.is_empty() || slug.contains('/') || slug == "." || slug == ".." {
        anyhow::bail!("slug must be a single path segment (no '/', '.', '..')");
    }
    Ok(())
}

pub async fn subapp_put(
    db: &mut dyn Database,
    slug: &str,
    title: Option<&str>,
    index: Option<&str>,
) -> anyhow::Result<()> {
    validate_slug(slug)?;
    let sub = SubApp {
        slug: slug.to_string(),
        title: title.map(String::from),
        index: index
            .map(String::from)
            .unwrap_or_else(|| format!("{slug}/index.html")),
        created_at: crate::crud::now_str(),
    };
    let data = serde_json::to_value(&sub)?;
    db.insert(TABLE_SUBAPPS, Row::new(Key::text(tenant_key(slug)), data)).await?;
    Ok(())
}

pub async fn subapp_list(db: &dyn Database) -> anyhow::Result<Vec<SubApp>> {
    let q = Query {
        filter: SrvFilter { conds: Vec::new() },
        orders: vec![("$.slug".to_string(), false)],
        limit: usize::MAX,
        offset: 0,
        ttl: None,
    };
    let mut out = Vec::new();
    for row in db.query(TABLE_SUBAPPS, &q).await?.rows {
        out.push(serde_json::from_value(row.data)?);
    }
    Ok(out)
}

pub async fn subapp_get(db: &dyn Database, slug: &str) -> anyhow::Result<Option<SubApp>> {
    let key = Key::text(tenant_key(slug));
    let Some(row) = db.get(TABLE_SUBAPPS, &key).await? else {
        return Ok(None);
    };
    Ok(serde_json::from_value(row.data)?)
}

pub async fn subapp_remove(db: &mut dyn Database, slug: &str) -> anyhow::Result<bool> {
    let n = db.delete(
        TABLE_SUBAPPS,
        &SrvFilter {
            conds: vec![
                FilterCond { field: "$.slug".to_string(), op: Op::Eq, value: json!(slug) },
            ],
        },
    ).await?;
    Ok(n > 0)
}