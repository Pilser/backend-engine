use super::{ok};
use engine::model::Principal;
use engine::ServerlessEngine;
use engine::TENANT;
use serde_json::{json, Value as Json};

// Single tenant: one Worker = one app. There is no create/list/delete of
// apps — the app IS the deployment. `show`/`update`/`resources` operate on
// the tenant config row (created lazily on first use).

pub async fn show(
    engine: &mut ServerlessEngine,
    _principal: &Principal,
    _arguments: &Json,
) -> Result<Json, String> {
    let tenant = engine.tenant().await.map_err(|e| e.to_string())?;
    ok(serde_json::to_value(&tenant).map_err(|e| e.to_string())?)
}

pub async fn resources(
    engine: &mut ServerlessEngine,
    _principal: &Principal,
    _arguments: &Json,
) -> Result<Json, String> {
    let tenant = engine.tenant().await.map_err(|e| e.to_string())?;
    let mut records: i64 = 0;
    if let Ok(tables) = engine.list_tables().await {
        for t in tables {
            records += engine.count_records(&t.table).await.map_err(|e| e.to_string())?;
        }
    }
    let mut storage: u64 = 0;
    if let Ok(keys) = engine.object_store().list(&format!("{TENANT}/files/")).await {
        storage += keys.iter().map(|k| k.size).sum::<u64>();
    }
    if let Ok(keys) = engine.object_store().list(&format!("{TENANT}/assets/")).await {
        storage += keys.iter().map(|k| k.size).sum::<u64>();
    }
    let limits = engine::policy::RateLimits::from_json(tenant.rate_json.as_ref().unwrap_or(&Json::Null));
    ok(json!({
        "tenant": TENANT,
        "records": { "count": records },
        "storage": { "bytes": storage },
        "rate": {
            "limits": {
                "submit": limits.submit,
                "upload": limits.upload,
                "search": limits.search,
                "read": limits.read,
                "per_day": limits.per_day,
            }
        }
    }))
}

pub async fn update(
    engine: &mut ServerlessEngine,
    _principal: &Principal,
    arguments: &Json,
) -> Result<Json, String> {
    // Tenant-level knobs only. Table-level schema/computed/validate/redact/ttl
    // live on the per-table verbs (tables.*).
    let mut patch = serde_json::Map::new();
    if let Some(v) = arguments.get("title") {
        patch.insert("title".to_string(), v.clone());
    }
    if let Some(v) = arguments.get("public_reads") {
        patch.insert("public_reads".to_string(), v.clone());
    }
    if !patch.is_empty() {
        engine.update_tenant(&Json::Object(patch)).await
            .map_err(|e| e.to_string())?;
    }
    if let Some(v) = arguments.get("audit") {
        engine.set_audit(v.as_bool().unwrap_or(false)).await.map_err(|e| e.to_string())?;
    }
    if let Some(v) = arguments.get("rate") {
        engine.set_rate(v).await.map_err(|e| e.to_string())?;
    }
    ok(json!({ "tenant": TENANT, "updated": true }))
}

pub fn _unused(_: i64) {}
