//! Site + request routes (Phase C/D): exact-path serving and declarative
//! proxying, so one worker = one app = one site with zero shim code.
//!
//! A site route lives in the `site_routes` table:
//! `{path, owner ("tenant" | "plugin:<slug>"), method, kind, spec}`.
//! - `redirect`: `{to, status?}`.
//! - `text`: `{body, content_type? ("text/plain"), headers?}`.
//! - `asset`: `{path: "<rel>"}` (serve a stored asset at an exact path).
//! - `query`: `{table, filter?, order?, limit?, format?: json|sitemap,
//!   url_field?, prefix?, s_maxage?}` — data-driven, incl. sitemaps.
//! - `proxy`: `{target, inject_headers?, forward_query? (default true),
//!   methods?}` — request-time fetch (Phase D).
//!
//! Rules: paths start with `/`; exact match wins, then longest `/*`
//! prefix; `/api/*`, `/mcp`, `/srv/*`, `/ws`, `/healthz` are reserved
//! (engine owns them); same path twice (any owner) is a conflict error.
//! Site routes are PUBLIC surface: query ops run as anonymous (only data
//! visible to anonymous is servable — fail loudly otherwise).

use serde_json::Value as Json;

pub const TABLE_SITE_ROUTES: &str = "site_routes";

const RESERVED: [&str; 6] = ["/api/", "/mcp", "/srv/", "/srv", "/ws", "/healthz"];

fn sys_principal() -> crate::model::Principal {
    crate::model::Principal {
        id: crate::TENANT.to_string(),
        role: "owner".to_string(),
        scope: None,
        writer: None,
        tables: None,
    }
}

pub fn valid_path(path: &str) -> Result<(), String> {
    if !path.starts_with('/') || path.len() < 2 {
        return Err(format!("bad site path '{path}' (must start with /, longer than just /)"));
    }
    if path.contains("//") || path.contains(' ') || path.contains('?') || path.contains('#') {
        return Err(format!("bad site path '{path}'"));
    }
    for r in RESERVED {
        if path == r || path.starts_with(&format!("{r}/")) || (r.ends_with('/') && path.starts_with(r)) {
            return Err(format!("site path '{path}' collides with reserved '{r}'"));
        }
    }
    Ok(())
}

pub fn valid_spec(spec: &Json) -> Result<(), String> {
    let m = spec.as_object().ok_or_else(|| "route spec must be an object".to_string())?;
    // Unknown role names fail here, never silently (a typo must not open
    // or close a route by accident).
    parse_allow_roles(spec)?;
    let kind = m.get("kind").and_then(|v| v.as_str()).unwrap_or("");
    match kind {
        "redirect" => {
            if m.get("to").and_then(|v| v.as_str()).map(|s| !s.is_empty()).unwrap_or(false) {
                Ok(())
            } else {
                Err("redirect route needs \"to\"".to_string())
            }
        }
        "text" => {
            if m.contains_key("body") {
                Ok(())
            } else {
                Err("text route needs \"body\"".to_string())
            }
        }
        "asset" => {
            if m.get("path").and_then(|v| v.as_str()).map(|s| !s.is_empty()).unwrap_or(false) {
                Ok(())
            } else {
                Err("asset route needs \"path\"".to_string())
            }
        }
        "query" => {
            if m.get("table").and_then(|v| v.as_str()).map(|s| !s.is_empty()).unwrap_or(false) {
                Ok(())
            } else {
                Err("query route needs \"table\"".to_string())
            }
        }
        "proxy" => {
            if m.get("target").and_then(|v| v.as_str()).map(|s| !s.is_empty()).unwrap_or(false) {
                Ok(())
            } else {
                Err("proxy route needs \"target\"".to_string())
            }
        }
        other => Err(format!("bad route kind '{other}' (redirect|text|asset|query|proxy)")),
    }
}

async fn ensure_table(engine: &mut crate::ServerlessEngine) -> anyhow::Result<()> {
    if engine.get_table(TABLE_SITE_ROUTES).await?.is_none() {
        engine.create_table(TABLE_SITE_ROUTES, None, None).await?;
    }
    Ok(())
}

/// Insert or replace one route. Conflicts (same path, different owner)
/// are rejected — no silent shadowing.
pub async fn route_put(engine: &mut crate::ServerlessEngine, owner: &str, spec: &Json) -> anyhow::Result<()> {
    let m = spec.as_object().ok_or_else(|| anyhow::anyhow!("route spec must be an object"))?;
    let path = m.get("path").and_then(|v| v.as_str()).unwrap_or("");
    valid_path(path).map_err(|e| anyhow::anyhow!("{e}"))?;
    valid_spec(spec).map_err(|e| anyhow::anyhow!("{e}"))?;
    ensure_table(engine).await?;
    let p = sys_principal();
    for rec in engine.list_records(TABLE_SITE_ROUTES, 10_000, None, 0, "asc").await? {
        let same_path = rec.payload.get("path").and_then(|v| v.as_str()) == Some(path);
        let same_owner = rec.payload.get("owner").and_then(|v| v.as_str()) == Some(owner);
        if same_path && !same_owner {
            let other = rec.payload.get("owner").and_then(|v| v.as_str()).unwrap_or("?");
            anyhow::bail!("site path '{path}' already owned by '{other}'");
        }
        if same_path && same_owner {
            engine.delete_record(TABLE_SITE_ROUTES, rec.seq).await?;
        }
    }
    let mut row = serde_json::Map::new();
    row.insert("path".into(), Json::String(path.to_string()));
    row.insert("owner".into(), Json::String(owner.to_string()));
    row.insert("spec".into(), spec.clone());
    engine.insert_record(TABLE_SITE_ROUTES, Json::Object(row), None, false, &p).await?;
    Ok(())
}

pub async fn route_remove(engine: &mut crate::ServerlessEngine, path: &str) -> anyhow::Result<bool> {
    if engine.get_table(TABLE_SITE_ROUTES).await?.is_none() {
        return Ok(false);
    }
    for rec in engine.list_records(TABLE_SITE_ROUTES, 10_000, None, 0, "asc").await? {
        if rec.payload.get("path").and_then(|v| v.as_str()) == Some(path) {
            engine.delete_record(TABLE_SITE_ROUTES, rec.seq).await?;
            return Ok(true);
        }
    }
    Ok(false)
}

pub async fn route_list(engine: &crate::ServerlessEngine) -> anyhow::Result<Vec<Json>> {
    if engine.get_table(TABLE_SITE_ROUTES).await?.is_none() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for rec in engine.list_records(TABLE_SITE_ROUTES, 10_000, None, 0, "asc").await? {
        out.push(rec.payload);
    }
    Ok(out)
}

/// Match method+path: exact first, then longest `/*` prefix. Returns the
/// stored row `{path, owner, spec}`. Method enforcement lives in the
/// dispatcher (405s), not the matcher.
pub async fn route_match(
    engine: &crate::ServerlessEngine,
    _method: &str,
    path: &str,
) -> anyhow::Result<Option<Json>> {
    let mut exact: Option<Json> = None;
    let mut prefix: Option<(usize, Json)> = None;
    for row in route_list(engine).await? {
        let rp = row.get("path").and_then(|v| v.as_str()).unwrap_or("");
        let matched = if let Some(pre) = rp.strip_suffix("/*") {
            path == pre || path.starts_with(&format!("{pre}/"))
        } else {
            path == rp
        };
        if !matched {
            continue;
        }
        if rp.ends_with("/*") {
            let len = rp.len();
            if prefix.as_ref().map(|(l, _)| len > *l).unwrap_or(true) {
                prefix = Some((len, row));
            }
        } else if exact.is_none() {
            exact = Some(row);
        }
    }
    Ok(exact.or_else(|| prefix.map(|(_, r)| r)))
}

/// Render a sitemap XML from query rows. `url_field` (default `slug`)
/// feeds `{base}{prefix}{value}` locations, XML-escaped.
pub fn render_sitemap(base: &str, prefix: &str, url_field: &str, rows: &[Json]) -> String {
    fn esc(s: &str) -> String {
        s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
    }
    let base = base.trim_end_matches('/');
    let mut out = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">\n");
    for r in rows {
        let payload = r.get("payload").unwrap_or(r);
        if let Some(v) = payload.get(url_field).and_then(|v| v.as_str()) {
            out.push_str(&format!("  <url><loc>{}{}{}</loc></url>\n", esc(base), esc(prefix), esc(v)));
        }
    }
    out.push_str("</urlset>\n");
    out
}

/// Role tiers for keyed routes (`allow_roles`): `reader` admits
/// reader/writer/admin/owner, `writer` admits writer/admin/owner, `admin`
/// admits admin/owner. Absent/empty list = public (anonymous). Pure.
pub fn role_allowed(principal_role: &str, allow: &[String]) -> bool {
    if allow.is_empty() {
        return true;
    }
    let tier = |r: &str| match r {
        "owner" => 3,
        "admin" => 2,
        "writer" => 1,
        "reader" | "list" => 0,
        _ => -1,
    };
    let have = tier(principal_role);
    have >= 0
        && allow.iter().any(|w| {
            let want = match w.as_str() {
                "reader" | "list" => 0,
                "writer" => 1,
                "admin" | "owner" => 2,
                _ => 99,
            };
            want != 99 && have >= want
        })
}

/// Extract + validate `allow_roles` (array of known role names, may be
/// empty = public). Unknown names are rejected at write time, never
/// silently ignored (a typo must not open or close a route by accident).
pub fn parse_allow_roles(spec: &Json) -> Result<Vec<String>, String> {
    let Some(v) = spec.get("allow_roles") else {
        return Ok(Vec::new());
    };
    let arr = v.as_array().ok_or_else(|| "allow_roles must be an array".to_string())?;
    let mut out = Vec::new();
    for x in arr {
        let s = x.as_str().ok_or_else(|| "allow_roles entries must be strings".to_string())?;
        match s {
            "reader" | "list" | "writer" | "admin" | "owner" => out.push(s.to_string()),
            _ => return Err(format!("unknown role '{s}' in allow_roles")),
        }
    }
    Ok(out)
}

/// Render a proxy target template: `{{$.path}}` (rest after prefix),
/// `{{$.tenant}}`, `{{$.query}}` (raw query string, may be empty).
/// Pure (unit-tested); SSRF-gated at the call site, not here.
pub fn render_target(template: &str, tenant: &str, path: &str, query: &str) -> String {
    template
        .replace("{{$.path}}", path)
        .replace("{{$.tenant}}", tenant)
        .replace("{{$.query}}", query)
}
