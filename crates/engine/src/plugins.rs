//! Native plugins (Phase B): one manifest installs everything a plugin is —
//! dist slug, tables, recipes, jobs, routes, email intent — in a single
//! admin call. The machine-readable standard lives in `docs/PLUGINS.md`.
//!
//! Conventions (enforced here, so every plugin behaves the same):
//! - `slug`: `^[a-z0-9][a-z0-9-]{0,39}$`; namespace `ns` = slug with `-`→`_`.
//! - plugin tables MUST be named `plugin_<ns>_*` (collision-proof, prunable).
//! - recipe/job names are auto-prefixed `<slug>__<name>` (already-prefixed
//!   names pass through, so reinstalls are idempotent).
//! - routes are stored validated in `plugin_routes` (executed in Phase C).
//! - install is idempotent: same slug reinstalls (replaces) cleanly.
//! - remove drops recipes/jobs/routes/registration; tables drop ONLY with
//!   `prune=true` (data loss is explicit).

use serde_json::{json, Value as Json};

use crate::model::Principal;

pub const TABLE_PLUGINS: &str = "plugins";
pub const TABLE_ROUTES: &str = "plugin_routes";

fn sys_principal() -> Principal {
    Principal { id: crate::TENANT.to_string(), role: "owner".to_string(), scope: None, writer: None }
}

fn valid_slug(slug: &str) -> bool {
    let b = slug.as_bytes();
    if b.is_empty() || b.len() > 40 || !b[0].is_ascii_alphanumeric() {
        return false;
    }
    b.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'-')
}

fn valid_route_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
}

/// Validate a single route binding (execution lives in Phase C).
fn valid_route(r: &Json) -> Result<(), String> {
    let m = r.as_object().ok_or_else(|| "route must be an object".to_string())?;
    let name = m.get("name").and_then(|v| v.as_str()).unwrap_or("");
    if !valid_route_name(name) {
        return Err(format!("bad route name '{name}'"));
    }
    let method = m.get("method").and_then(|v| v.as_str()).unwrap_or("GET");
    if !matches!(method, "GET" | "POST" | "PUT" | "PATCH" | "DELETE") {
        return Err(format!("bad route method '{method}'"));
    }
    let op = m.get("op").and_then(|v| v.as_str()).unwrap_or("");
    if !matches!(op, "query" | "get" | "submit" | "aggregate") {
        return Err(format!("bad route op '{op}' (query|get|submit|aggregate)"));
    }
    let table = m.get("table").and_then(|v| v.as_str()).unwrap_or("");
    if table.is_empty() {
        return Err("route needs a table".to_string());
    }
    Ok(())
}

fn prefixed(slug: &str, name: &str) -> String {
    let p = format!("{slug}__");
    if name.starts_with(&p) {
        name.to_string()
    } else {
        format!("{p}{name}")
    }
}

async fn ensure_table(engine: &mut crate::ServerlessEngine, table: &str) -> anyhow::Result<()> {
    if engine.get_table(table).await?.is_none() {
        engine.create_table(table, None, None).await?;
    }
    Ok(())
}

async fn replace_records(
    engine: &mut crate::ServerlessEngine,
    table: &str,
    keep: impl Fn(&Json) -> bool,
    rows: Vec<Json>,
) -> anyhow::Result<()> {
    ensure_table(engine, table).await?;
    let p = sys_principal();
    for rec in engine.list_records(table, 10_000, None, 0, "asc").await? {
        if !keep(&rec.payload) {
            engine.delete_record(table, rec.seq).await?;
        }
    }
    for payload in rows {
        engine.insert_record(table, payload, None, false, &p).await?;
    }
    Ok(())
}

/// Validate + install a plugin manifest. Returns a summary JSON.
pub async fn install(engine: &mut crate::ServerlessEngine, manifest: &Json) -> anyhow::Result<Json> {
    let m = manifest.as_object().ok_or_else(|| anyhow::anyhow!("manifest must be an object"))?;
    let slug = m.get("slug").and_then(|v| v.as_str()).unwrap_or("");
    if !valid_slug(slug) {
        anyhow::bail!("bad slug '{slug}' (^[a-z0-9][a-z0-9-]{{0,39}}$)");
    }
    let ns: String = slug.chars().map(|c| if c == '-' { '_' } else { c }).collect();
    let prefix = format!("plugin_{ns}_");
    let title = m.get("title").and_then(|v| v.as_str()).unwrap_or(slug);
    let version = m.get("version").and_then(|v| v.as_str()).unwrap_or("0.1.0");

    // Tables (namespaced, enforced).
    let mut tables = Vec::new();
    for t in m.get("tables").and_then(|v| v.as_array()).cloned().unwrap_or_default() {
        let name = t.get("table").and_then(|v| v.as_str()).unwrap_or("");
        if !name.starts_with(&prefix) {
            anyhow::bail!("plugin table '{name}' must start with '{prefix}'");
        }
        let schema = t.get("schema").cloned();
        let unique = t.get("unique_key").and_then(|v| v.as_str()).map(String::from);
        if engine.get_table(name).await?.is_none() {
            engine.create_table(name, schema, unique.as_deref()).await?;
        }
        tables.push(name.to_string());
    }

    // Recipes (auto-prefixed, validated by add_recipe).
    let mut recipes = Vec::new();
    for r in m.get("recipes").and_then(|v| v.as_array()).cloned().unwrap_or_default() {
        let name = r.get("name").and_then(|v| v.as_str()).unwrap_or("");
        if name.is_empty() {
            anyhow::bail!("plugin recipe needs a name");
        }
        let full = prefixed(slug, name);
        let recipe = crate::model::Recipe {
            name: full.clone(),
            when_json: r.get("when").cloned().unwrap_or(Json::Null),
            match_json: r.get("match").cloned(),
            enabled: r.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true),
            dedup_on: r.get("dedup_on").and_then(|v| v.as_str()).map(String::from),
            actions_json: r.get("actions").cloned(),
            table: r.get("table").and_then(|v| v.as_str()).map(String::from),
        };
        engine.add_recipe(&recipe).await?;
        recipes.push(full);
    }

    // Jobs (auto-prefixed).
    let mut jobs = Vec::new();
    for j in m.get("jobs").and_then(|v| v.as_array()).cloned().unwrap_or_default() {
        let name = j.get("name").and_then(|v| v.as_str()).unwrap_or("");
        if name.is_empty() {
            anyhow::bail!("plugin job needs a name");
        }
        let full = prefixed(slug, name);
        let schedule = j.get("schedule").and_then(|v| v.as_str()).unwrap_or("");
        let action = j.get("action").cloned().unwrap_or(Json::Null);
        engine.add_job(&full, schedule, &action).await?;
        jobs.push(full);
    }

    // Routes (validated, stored for Phase C execution).
    let mut routes = Vec::new();
    for r in m.get("routes").and_then(|v| v.as_array()).cloned().unwrap_or_default() {
        valid_route(&r).map_err(|e| anyhow::anyhow!("{e}"))?;
        let row = r.as_object().cloned().unwrap_or_default();
        let mut stored = serde_json::Map::new();
        stored.insert("slug".into(), Json::String(slug.to_string()));
        stored.insert(
            "key".into(),
            Json::String(format!(
                "{slug}/{}/{}",
                r.get("method").and_then(|v| v.as_str()).unwrap_or("GET"),
                r.get("name").and_then(|v| v.as_str()).unwrap_or("")
            )),
        );
        stored.insert("route".into(), Json::Object(row));
        routes.push(Json::Object(stored));
    }
    replace_records(engine, TABLE_ROUTES, |p| p.get("slug").and_then(|v| v.as_str()) != Some(slug), routes).await?;

    // Sub-app registration (dist bytes arrive separately via files put --slug).
    if let Some(sub) = m.get("subapp").and_then(|v| v.as_object()) {
        let title = sub.get("title").and_then(|v| v.as_str());
        let index = sub.get("index").and_then(|v| v.as_str());
        engine.put_subapp(slug, title, index).await?;
    }

    // Email intent is declarative (routing happens dashboard-side).
    let email_inbound = m
        .get("email")
        .and_then(|v| v.get("inbound"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    // Plugin record (idempotent by slug).
    let record = json!({
        "slug": slug, "title": title, "version": version,
        "tables": tables, "recipes": recipes, "jobs": jobs,
        "email_inbound": email_inbound,
        "installed_at": crate::crud::now_str(),
    });
    replace_records(engine, TABLE_PLUGINS, |p| p.get("slug").and_then(|v| v.as_str()) != Some(slug), vec![record]).await?;

    Ok(json!({
        "ok": true, "slug": slug,
        "tables": tables, "recipes": recipes, "jobs": jobs,
    }))
}

/// Remove a plugin. `prune=true` also drops its tables (data loss).
pub async fn remove(engine: &mut crate::ServerlessEngine, slug: &str, prune: bool) -> anyhow::Result<Json> {
    let rec = engine
        .list_records(TABLE_PLUGINS, 10_000, None, 0, "asc")
        .await?
        .into_iter()
        .find(|r| r.payload.get("slug").and_then(|v| v.as_str()) == Some(slug));
    let Some(rec) = rec else {
        anyhow::bail!("plugin '{slug}' is not installed");
    };
    for r in rec.payload.get("recipes").and_then(|v| v.as_array()).cloned().unwrap_or_default() {
        if let Some(name) = r.as_str() {
            let _ = engine.remove_recipe(name).await;
        }
    }
    for j in rec.payload.get("jobs").and_then(|v| v.as_array()).cloned().unwrap_or_default() {
        if let Some(name) = j.as_str() {
            let _ = engine.remove_job(name).await;
        }
    }
    let mut dropped = Vec::new();
    if prune {
        for t in rec.payload.get("tables").and_then(|v| v.as_array()).cloned().unwrap_or_default() {
            if let Some(name) = t.as_str() {
                if engine.drop_table(name).await.unwrap_or(false) {
                    dropped.push(name.to_string());
                }
            }
        }
    }
    replace_records(engine, TABLE_ROUTES, |r| r.get("slug").and_then(|v| v.as_str()) != Some(slug), vec![]).await?;
    engine.remove_subapp(slug).await.unwrap_or(false);
    engine.delete_record(TABLE_PLUGINS, rec.seq).await?;
    Ok(json!({ "ok": true, "slug": slug, "prune": prune, "tables_dropped": dropped }))
}

pub async fn list(engine: &crate::ServerlessEngine) -> anyhow::Result<Vec<Json>> {
    if engine.get_table(TABLE_PLUGINS).await?.is_none() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for rec in engine.list_records(TABLE_PLUGINS, 10_000, None, 0, "asc").await? {
        out.push(rec.payload);
    }
    Ok(out)
}

pub async fn get(engine: &crate::ServerlessEngine, slug: &str) -> anyhow::Result<Option<Json>> {
    Ok(list(engine).await?.into_iter().find(|p| p.get("slug").and_then(|v| v.as_str()) == Some(slug)))
}
