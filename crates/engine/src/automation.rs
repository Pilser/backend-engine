use crate::events::EventKind;
use crate::model::{Key, Principal, Recipe};
use crate::storage::database::{Database, Query, Row};
use crate::storage::ir::{normalize_path, scalar_text, FilterCond, Op, SrvFilter};
use crate::tables::{tenant_key, TABLE_RECIPES, TABLE_RECIPE_RUNS};
use serde_json::{json, Value as Json};
use sha2::{Digest, Sha256};
use std::future::Future;

pub use crate::webhooks::valid_url;

fn sha256_hex(s: &str) -> String {
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    let digest = h.finalize();
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(64);
    for b in digest {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

fn recipe_run_key(recipe: &str, key: &str) -> Key {
    Key::text(tenant_key(&format!("{recipe}#{key}")))
}

async fn recipe_dedup_skip(
    db: &dyn Database,
    recipe: &Recipe,
    payload: &Json,
) -> anyhow::Result<bool> {
    let Some(path) = recipe.dedup_on.as_deref() else {
        return Ok(false);
    };
    let k = crate::expr::get_path(payload, &normalize_path(path));
    if k.is_null() {
        return Ok(false);
    }
    let key = sha256_hex(&serde_json::to_string(&k).unwrap_or_default());
    Ok(db.get(TABLE_RECIPE_RUNS, &recipe_run_key(&recipe.name, &key)).await?.is_some())
}

async fn recipe_dedup_mark(
    db: &mut dyn Database,
    recipe: &Recipe,
    payload: &Json,
) -> anyhow::Result<()> {
    let Some(path) = recipe.dedup_on.as_deref() else {
        return Ok(());
    };
    let k = crate::expr::get_path(payload, &normalize_path(path));
    if k.is_null() {
        return Ok(());
    }
    let key = sha256_hex(&serde_json::to_string(&k).unwrap_or_default());
    let data = serde_json::json!({
        "recipe": recipe.name,
        "dedup_key": key,
    });
    db.insert(TABLE_RECIPE_RUNS, Row::new(recipe_run_key(&recipe.name, &key), data)).await?;
    Ok(())
}

const MAX_ACTIONS: usize = 50;
/// How `$call` HTTP runs. `Inline` executes inside `apply_action` (historical
/// behavior: caller holds the engine Mutex during network I/O). `Defer`
/// collects fully-resolved requests so the caller can release the lock and
/// run the pure-HTTP part without stalling every other request.
pub enum HttpMode<'p> {
    Inline,
    Defer(&'p mut Vec<PendingHttp>),
}

/// A resolved outbound request: secrets substituted, templates rendered.
/// Execution needs NO database access. `result_into` selects the payload
/// path the response lands on (`$call` keeps `$.call_result`; `$send_email`
/// writes `$.email_result`).
pub struct PendingHttp {
    url: String,
    headers: Vec<(String, String)>,
    body: crate::http::HttpBody,
    timeout_ms: u64,
    result_into: Option<String>,
}

/// Execute deferred `$call`/`$send_email` plans. MUST run WITHOUT the engine
/// Mutex held — this is pure network I/O plus a write-back into the working
/// payload.
pub async fn execute_pending(calls: Vec<PendingHttp>, payload: &mut Json, logs: &mut Vec<String>) {
    for c in calls {
        let url = c.url.clone();
        let into = c.result_into.as_deref().unwrap_or("$.call_result").to_string();
        match crate::http::http_call_body(&c.url, &c.headers, &c.body, c.timeout_ms).await {
            Ok((status, resp_body)) => {
                let _ = set_at(payload, &into, json!({ "status": status, "body": resp_body }));
                logs.push(format!("call {url} -> {status}"));
            }
            Err(e) => logs.push(format!("call {url}: {e}")),
        }
    }
}


fn recipe_filter(name: &str) -> SrvFilter {
    SrvFilter {
        conds: vec![
            FilterCond { field: "$.name".to_string(), op: Op::Eq, value: Json::String(name.to_string()) },
        ],
    }
}

fn when_trigger_valid(when: &Json) -> bool {
    let s: Option<&str> = match when {
        Json::String(s) => Some(s.as_str()),
        Json::Object(m) => m.get("event").and_then(|v| v.as_str()),
        _ => None,
    };
    matches!(
        s,
        Some("record.created" | "record.updated" | "record.deleted" | "inbound")
    ) || s.map(|v| v.starts_with("cron:")).unwrap_or(false)
}

pub async fn recipe_add(db: &mut dyn Database, recipe: &Recipe) -> anyhow::Result<()> {
    if !when_trigger_valid(&recipe.when_json) {
        anyhow::bail!(
            "recipe when must be one of: record.created, record.updated, record.deleted, inbound, cron:<name>"
        );
    }
    match recipe.actions_json.as_ref().and_then(|a| a.as_array()) {
        Some(a) if !a.is_empty() => {}
        _ => anyhow::bail!("recipe needs at least one action"),
    }
    let data = serde_json::to_value(recipe)?;
    db.delete(TABLE_RECIPES, &recipe_filter(&recipe.name)).await?;
    db.insert(TABLE_RECIPES, Row::new(Key::text(tenant_key(&recipe.name)), data)).await?;
    Ok(())
}

pub async fn recipe_list(db: &dyn Database) -> anyhow::Result<Vec<Recipe>> {
    let q = Query {
        filter: SrvFilter { conds: Vec::new() },
        orders: vec![("$.name".to_string(), false)],
        limit: usize::MAX,
        offset: 0,
    
        ttl: None,
    };
    let mut out = Vec::new();
    for row in db.query(TABLE_RECIPES, &q).await?.rows {
        out.push(serde_json::from_value(row.data)?);
    }
    Ok(out)
}

pub async fn recipe_get(db: &dyn Database, name: &str) -> anyhow::Result<Option<Recipe>> {
    let key = Key::text(tenant_key(name));
    let Some(row) = db.get(TABLE_RECIPES, &key).await? else {
        return Ok(None);
    };
    Ok(serde_json::from_value(row.data)?)
}

pub async fn recipe_remove(db: &mut dyn Database, name: &str) -> anyhow::Result<()> {
    if db.delete(TABLE_RECIPES, &recipe_filter(name)).await? == 0 {
        anyhow::bail!("recipe '{name}' not found");
    }
    Ok(())
}

pub async fn recipe_enabled(db: &mut dyn Database, name: &str, enabled: bool) -> anyhow::Result<()> {
    let key = Key::text(tenant_key(name));
    let Some(row) = db.get(TABLE_RECIPES, &key).await? else {
        anyhow::bail!("recipe '{name}' not found");
    };
    let mut recipe: Recipe = serde_json::from_value(row.data)?;
    recipe.enabled = enabled;
    let data = serde_json::to_value(&recipe)?;
    db.update(TABLE_RECIPES, &key, &data).await?;
    Ok(())
}

pub struct DispatchOutcome {
    /// Fully-resolved `$call` requests to run WITHOUT any engine lock held.
    /// Execute via `execute_pending` after dropping the guard, then feed
    /// `apply_call_results` if a write-back of `$.call_result` is wanted.
    pub pending: Vec<PendingHttp>,
    /// Working payloads keyed by seq, to write back AFTER the calls run.
    pub writebacks: Vec<(i64, Json)>,
}

/// Boxed dispatch entry points: recipes can write records (`$upsert_other`,
/// `$patch_other`, `$transaction`, write-backs), and record writes dispatch
/// recipes — a genuine call cycle. Native `async fn` cannot express cyclic
/// futures (infinite type), so these four entry points return a boxed future
/// while their bodies live in `*_inner` async fns. Callers just `.await`.
pub fn dispatch<'a>(
    db: &'a mut dyn Database,
    table: &'a str,
    event: EventKind,
    seq: Option<i64>,
    payload: Option<Json>,
) -> std::pin::Pin<Box<dyn Future<Output = anyhow::Result<()>> + 'a>> {
    Box::pin(dispatch_inner(db, table, event, seq, payload))
}

async fn dispatch_inner(
    db: &mut dyn Database,
    table: &str,
    event: EventKind,
    seq: Option<i64>,
    payload: Option<Json>,
) -> anyhow::Result<()> {
    let mut out = DispatchOutcome { pending: Vec::new(), writebacks: Vec::new() };
    dispatch_phased(db, table, event, seq, payload, &mut out).await
}

/// Phase A only: recipe matching + DB actions + plan collection. NO network
/// I/O. The caller may hold the engine Mutex safely.
pub fn dispatch_phased<'a>(
    db: &'a mut dyn Database,
    table: &'a str,
    event: EventKind,
    seq: Option<i64>,
    payload: Option<Json>,
    out: &'a mut DispatchOutcome,
) -> std::pin::Pin<Box<dyn Future<Output = anyhow::Result<()>> + 'a>> {
    Box::pin(dispatch_phased_inner(db, table, event, seq, payload, out))
}

async fn dispatch_phased_inner(
    db: &mut dyn Database,
    table: &str,
    event: EventKind,
    seq: Option<i64>,
    payload: Option<Json>,
    out: &mut DispatchOutcome,
) -> anyhow::Result<()> {
    let payload = payload.unwrap_or(Json::Null);
    for recipe in recipe_list(db).await? {
        if let Some(rt) = recipe.table.as_deref() {
            if rt != table {
                continue;
            }
        }
        if !recipe.enabled || !trigger_matches(&recipe.when_json, event) || !conditions_pass(&recipe.when_json, &payload) {
            continue;
        }
        if let Some(m) = recipe.match_json.as_ref() {
            if !m.is_null() && !crate::storage::ir::parse_filter(m).map(|f| f.matches(&payload)).unwrap_or(false) {
                continue;
            }
        }
        if recipe_dedup_skip(db, &recipe, &payload).await? {
            continue;
        }
        let mut working = payload.clone();
        let mut logs: Vec<String> = Vec::new();
        let mut pending: Vec<PendingHttp> = Vec::new();
        {
            let mut mode = HttpMode::Defer(&mut pending);
            let _ = run_actions(db, table, &recipe, &mut working, &mut logs, &mut mode).await;
        }
        // Execute inline ONLY when there is nothing deferred (pure-DB recipes
        // finish here). Otherwise hand the calls to the caller's phase B.
        if pending.is_empty() {
            for l in &logs {
                eprintln!("[recipe {}] {l}", recipe.name);
            }
        } else {
            out.pending.extend(pending);
            logs.push(format!("deferred {} http call(s)", out.pending.len()));
        }
        let _ = recipe_dedup_mark(&mut *db, &recipe, &payload).await;
        if working != payload && seq.is_some() && matches!(event, EventKind::Created | EventKind::Updated) {
            // Write-back of non-deferred results happens NOW (still Phase A).
            // Deferred-call results are written back by the caller after
            // executing them (see apply_call_results).
            if let Err(e) = crate::crud::record_set_raw(&mut *db, table, seq.unwrap(), working.clone()).await {
                eprintln!("[recipe {}] write-back failed: {e}", recipe.name);
            }
            if !out.pending.is_empty() {
                out.writebacks.push((seq.unwrap(), working));
            }
        }
    }
    Ok(())
}

/// Phase C: merge executed `$call` results into stored records. Runs WITH the
/// engine lock re-acquired by the caller.
pub async fn apply_call_results(
    db: &mut dyn Database,
    table: &str,
    writebacks: &[(i64, Json)],
) {
    for (seq, working) in writebacks {
        if let Err(e) = crate::crud::record_set_raw(db, table, *seq, working.clone()).await {
            eprintln!("[recipe] deferred write-back failed: {e}");
        }
        let _ = working;
    }
}

pub fn dispatch_cron<'a>(
    db: &'a mut dyn Database,
    job_name: &'a str,
) -> std::pin::Pin<Box<dyn Future<Output = anyhow::Result<()>> + 'a>> {
    Box::pin(dispatch_cron_inner(db, job_name))
}

async fn dispatch_cron_inner(
    db: &mut dyn Database,
    job_name: &str,
) -> anyhow::Result<()> {
    let mut out = DispatchOutcome { pending: Vec::new(), writebacks: Vec::new() };
    dispatch_cron_phased(db, job_name, &mut out).await?;
    // Legacy behavior: execute any deferred HTTP before returning (caller
    // holds the lock). Lock-aware callers use the phased variant instead.
    let mut working = Json::Null;
    let mut logs = Vec::new();
    execute_pending(out.pending, &mut working, &mut logs).await;
    for l in &logs {
        eprintln!("[recipe cron:{job_name}] {l}");
    }
    Ok(())
}

/// Phase A only: cron recipe matching + DB actions; `$call` HTTP collected
/// into `out.pending` (no network I/O here).
pub fn dispatch_cron_phased<'a>(
    db: &'a mut dyn Database,
    job_name: &'a str,
    out: &'a mut DispatchOutcome,
) -> std::pin::Pin<Box<dyn Future<Output = anyhow::Result<()>> + 'a>> {
    Box::pin(dispatch_cron_phased_inner(db, job_name, out))
}

async fn dispatch_cron_phased_inner(
    db: &mut dyn Database,
    job_name: &str,
    out: &mut DispatchOutcome,
) -> anyhow::Result<()> {
    let wanted = format!("cron:{job_name}");
    for recipe in recipe_list(db).await? {
        if !recipe.enabled {
            continue;
        }
        let when = match &recipe.when_json {
            Json::String(s) => s.clone(),
            Json::Object(m) => m
                .get("event")
                .and_then(|v| v.as_str())
                .map(String::from)
                .unwrap_or_default(),
            _ => continue,
        };
        if when != wanted {
            continue;
        }
        if recipe_dedup_skip(db, &recipe, &Json::Null).await? {
            continue;
        }
        let mut working = Json::Null;
        let mut logs: Vec<String> = Vec::new();
        let table = recipe.table.as_deref().unwrap_or("records");
        let mut pending: Vec<PendingHttp> = Vec::new();
        {
            let mut mode = HttpMode::Defer(&mut pending);
            let _ = run_actions(db, table, &recipe, &mut working, &mut logs, &mut mode).await;
        }
        if pending.is_empty() {
            for l in &logs {
                eprintln!("[recipe cron:{job_name}] {l}",);
            }
        } else {
            out.pending.extend(pending);
        }
        let _ = recipe_dedup_mark(&mut *db, &recipe, &Json::Null).await;
    }
    Ok(())
}

fn trigger_matches(when: &Json, event: EventKind) -> bool {
    match when {
        Json::String(s) => s == "never" || s == event.name() || (event == EventKind::Cron && s.starts_with("cron:")),
        Json::Object(m) => match m.get("event").and_then(|v| v.as_str()) {
            Some(e) => e == "never" || e == event.name(),
            None => true,
        },
        _ => false,
    }
}

fn conditions_pass(when: &Json, payload: &Json) -> bool {
    let Some(m) = when.as_object() else {
        return true;
    };
    if let Some(w) = m.get("when").and_then(|v| v.as_str()) {
        if !crate::expr::truthy(w, payload).unwrap_or(false) {
            return false;
        }
    }
    let customer = crate::expr::get_path(payload, "$.customer_id");
    if let Some(scope) = m.get("scope").and_then(|v| v.as_str()) {
        if customer != Json::String(scope.to_string()) {
            return false;
        }
    }
    if let Some(cid) = m.get("customer_id").and_then(|v| v.as_str()) {
        if customer != Json::String(cid.to_string()) {
            return false;
        }
    }
    true
}

/// Template-substitute `{{$.path}}` in a string and return the result.
pub fn subst_strings_str(pattern: &str, payload: &Json) -> anyhow::Result<String> {
    template_swap(payload, pattern)
}

pub fn template_swap(payload: &Json, value: &str) -> anyhow::Result<String> {
    let mut out = String::new();
    let mut rest = value;
    while let Some(start) = rest.find("{{") {
        let Some(rel_end) = rest[start + 2..].find("}}") else {
            break;
        };
        let path = rest[start + 2..start + 2 + rel_end].trim();
        out.push_str(&rest[..start]);
        if !path.is_empty() {
            let v = crate::expr::get_path(payload, path);
            if !v.is_null() {
                out.push_str(&scalar_text(&v));
            }
        }
        rest = &rest[start + 2 + rel_end + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

pub async fn srv_http_call(url: &str, headers: &[(String, String)], body: &Json) -> anyhow::Result<Json> {
    if !valid_url(url) {
        anyhow::bail!("ssrf-blocked url '{url}'");
    }
    let (_, res) = crate::http::http_call(url, headers, body, 15_000).await?;
    Ok(res)
}

fn get_at(payload: &Json, path: &str) -> Json {
    crate::expr::get_path(payload, &normalize_path(path))
}

fn set_at(payload: &mut Json, path: &str, value: Json) -> anyhow::Result<()> {
    crate::expr::set_path(payload, &normalize_path(path), value)
        .map_err(|e| anyhow::anyhow!("set '{path}': {e}"))
}

fn remove_at(payload: &mut Json, path: &str) {
    crate::crud::unset_path(payload, path);
}

fn deep_merge(base: &mut Json, patch: &Json) {
    match (base, patch) {
        (Json::Object(b), Json::Object(p)) => {
            for (k, v) in p {
                match b.get_mut(k) {
                    Some(existing) => deep_merge(existing, v),
                    None => {
                        b.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        (b, p) => *b = p.clone(),
    }
}

fn render_braces(pattern: &str, payload: &Json) -> anyhow::Result<String> {
    let mut out = String::new();
    let mut rest = pattern;
    while let Some(start) = rest.find('{') {
        let Some(end) = rest[start..].find('}') else {
            out.push_str(rest);
            break;
        };
        out.push_str(&rest[..start]);
        let inner = &rest[start + 1..start + end];
        let rendered = if let Some(stripped) = inner.strip_prefix('$') {
            crate::expr::get_path(payload, &format!("${stripped}"))
        } else {
            crate::expr::get_path(payload, &normalize_path(inner))
        };
        out.push_str(&rendered.to_string());
        rest = &rest[start + end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

fn subst_strings(v: &mut Json, payload: &Json) -> anyhow::Result<()> {
    match v {
        Json::String(s) => *s = template_swap(payload, s)?,
        Json::Array(a) => {
            for el in a.iter_mut() {
                subst_strings(el, payload)?;
            }
        }
        Json::Object(m) => {
            for val in m.values_mut() {
                subst_strings(val, payload)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Type-aware substitution for `{{$.path}}` placeholders: when a string is a
/// single placeholder and the referenced value is a JSON number/bool/null, the
/// value is substituted as its JSON type (so `$inc` deltas stay numbers);
/// otherwise the string is rendered with `template_swap` (string concat).
pub fn subst_patch(v: Json, payload: &Json) -> anyhow::Result<Json> {
    match v {
        Json::String(s) => {
            let trimmed = s.trim();
            if trimmed.starts_with("{{") && trimmed.ends_with("}}") {
                let path = trimmed[2..trimmed.len() - 2].trim();
                let val = crate::expr::get_path(payload, path);
                if !val.is_null() {
                    match val {
                        Json::Number(_) | Json::Bool(_) => return Ok(val),
                        _ => {}
                    }
                }
            }
            Ok(Json::String(template_swap(payload, &s)?))
        }
        Json::Array(a) => {
            let mut out = Vec::with_capacity(a.len());
            for el in a {
                out.push(subst_patch(el, payload)?);
            }
            Ok(Json::Array(out))
        }
        Json::Object(m) => {
            let mut out = serde_json::Map::with_capacity(m.len());
            for (k, val) in m {
                out.insert(k, subst_patch(val, payload)?);
            }
            Ok(Json::Object(out))
        }
        other => Ok(other),
    }
}

/// Percent-encode a `{k: v}` map into `application/x-www-form-urlencoded`.
fn urlencode_form(form: &Json) -> String {
    fn enc(s: &str) -> String {
        let mut out = String::new();
        for b in s.as_bytes() {
            match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                    out.push(*b as char);
                }
                b' ' => out.push('+'),
                _ => out.push_str(&format!("%{:02X}", b)),
            }
        }
        out
    }
    let mut parts = Vec::new();
    if let Some(map) = form.as_object() {
        for (k, v) in map {
            parts.push(format!("{}={}", enc(k), enc(&crate::storage::ir::scalar_text(v))));
        }
    }
    parts.join("&")
}

/// Replace `{secret:name}` placeholders with the decrypted secret value.
pub fn resolve_secret_placeholders(
    secrets: &std::collections::HashMap<String, String>,
    text: &str,
) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("{secret:") {
        let Some(rel_end) = rest[start + 8..].find('}') else {
            break;
        };
        let name = &rest[start + 8..start + 8 + rel_end];
        out.push_str(&rest[..start]);
        if let Some(v) = secrets.get(name) {
            out.push_str(v);
        }
        rest = &rest[start + 8 + rel_end + 1..];
    }
    out.push_str(rest);
    out
}

pub fn subst_secret_json(
    secrets: &std::collections::HashMap<String, String>,
    v: &mut Json,
) {
    match v {
        Json::String(s) => *s = resolve_secret_placeholders(secrets, s),
        Json::Array(a) => {
            for el in a.iter_mut() {
                subst_secret_json(secrets, el);
            }
        }
        Json::Object(m) => {
            for val in m.values_mut() {
                subst_secret_json(secrets, val);
            }
        }
        _ => {}
    }
}

async fn run_actions(
    db: &mut dyn Database,
    table: &str,
    recipe: &Recipe,
    payload: &mut Json,
    logs: &mut Vec<String>,
    http_mode: &mut HttpMode<'_>,
) -> anyhow::Result<()> {
    let actions = recipe.actions_json.as_ref().and_then(|a| a.as_array()).cloned().unwrap_or_default();
    let mut n = 0;
    for action in actions {
        if n >= MAX_ACTIONS {
            logs.push("max actions reached, stopping".to_string());
            break;
        }
        let Some(obj) = action.as_object() else { continue };
        let Some((key, val)) = obj.iter().next() else { continue };
        if let Some(cond) = val.as_object().and_then(|m| m.get("when")).and_then(|w| w.as_str()) {
            if !crate::expr::truthy(cond, payload).unwrap_or(false) {
                logs.push(format!("{key} skipped (condition false)"));
                continue;
            }
        }
        match apply_action(db, table, key, val, payload, logs, http_mode).await {
            Ok(()) => n += 1,
            Err(e) => {
                logs.push(format!("{key}: {e}"));
                break;
            }
        }
    }
    Ok(())
}

fn str_field<'a>(obj: &'a serde_json::Map<String, Json>, name: &str) -> anyhow::Result<&'a str> {
    obj.get(name).and_then(|v| v.as_str()).ok_or_else(|| anyhow::anyhow!("missing string field '{name}'"))
}

async fn apply_action(
    db: &mut dyn Database,
    table: &str,
    key: &str,
    val: &Json,
    payload: &mut Json,
    logs: &mut Vec<String>,
    http_mode: &mut HttpMode<'_>,
) -> anyhow::Result<()> {
    match key {
        "$compute" => {
            let obj = val.as_object().ok_or_else(|| anyhow::anyhow!("$compute must be an object"))?;
            for (field, expr) in obj {
                let e = expr.as_str().ok_or_else(|| anyhow::anyhow!("$compute value for '{field}' must be a string expr"))?;
                let v = crate::expr::evaluate(e, payload).map_err(|err| anyhow::anyhow!("{field}: {err}"))?;
                set_at(payload, field, v)?;
            }
        }
        "$set" => {
            let mut resolved = val.clone();
            subst_strings(&mut resolved, payload)?;
            let obj = resolved.as_object().ok_or_else(|| anyhow::anyhow!("$set must be an object"))?;
            for (field, value) in obj {
                set_at(payload, field, value.clone())?;
            }
        }
        "$copy" | "$move" => {
            let obj = val.as_object().ok_or_else(|| anyhow::anyhow!("{key} must be an object"))?;
            let from = str_field(obj, "from")?;
            let to = str_field(obj, "to")?;
            let v = get_at(payload, from);
            set_at(payload, to, v)?;
            if key == "$move" {
                remove_at(payload, from);
            }
        }
        "$log" => {
            let msg = val.as_object().and_then(|m| m.get("message")).and_then(|v| v.as_str()).unwrap_or("");
            logs.push(template_swap(payload, msg)?);
        }
        "$upsert_other" => {
            let obj = val.as_object().ok_or_else(|| anyhow::anyhow!("$upsert_other must be an object"))?;
            // Single tenant: the legacy cross-board `board` field is ignored.
            let tbl = obj.get("table").and_then(|v| v.as_str()).unwrap_or(table);
            let mut p = obj.get("payload").cloned().unwrap_or(Json::Object(serde_json::Map::new()));
            subst_strings(&mut p, payload)?;
            let principal = Principal { id: crate::TENANT.to_string(), role: "owner".to_string(), scope: None, writer: None };
            crate::crud::record_insert(db, tbl, p, Some(&format!("recipe:{}", crate::TENANT)), true, &principal).await?;
        }
        "$patch_other" => {
            let obj = val.as_object().ok_or_else(|| anyhow::anyhow!("$patch_other must be an object"))?;
            let tbl = obj.get("table").and_then(|v| v.as_str()).unwrap_or(table);
            let filter = obj.get("filter").ok_or_else(|| anyhow::anyhow!("$patch_other needs \"filter\""))?;
            let patch = obj.get("patch").ok_or_else(|| anyhow::anyhow!("$patch_other needs \"patch\""))?;
            // Resolve {{$.field}} placeholders in the filter AND patch from the
            // triggering payload (with type-aware substitution so $inc deltas
            // stay numbers), e.g. filter {"item_id":"{{$.item_id}}"} and
            // patch {"$inc":{"quantity":"{{$.quantity}}"}}.
            let filter = subst_patch(filter.clone(), payload)?;
            let patch = subst_patch(patch.clone(), payload)?;
            crate::crud::record_patch_first_raw(db, tbl, &crate::storage::ir::parse_filter(&filter)?, &patch).await?;
        }
        // "$resolve_other": look up one row in another table by a filter and
        // copy a field of it into the working payload. Used to resolve
        // name->id mappings in recipes, e.g. learner.dormitory (name) ->
        // dormitory_residents.dormitory_id (uuid).
        //   {"$resolve_other":{"table":"dormitories","filter":{"name":"{{$.dormitory}}"},"field":"id","into":"$.dormitory_id"}}
        "$resolve_other" => {
            let obj = val.as_object().ok_or_else(|| anyhow::anyhow!("$resolve_other must be an object"))?;
            let tbl = obj.get("table").and_then(|v| v.as_str()).ok_or_else(|| anyhow::anyhow!("$resolve_other needs \"table\""))?;
            let filter = obj.get("filter").ok_or_else(|| anyhow::anyhow!("$resolve_other needs \"filter\""))?;
            let field = obj.get("field").and_then(|v| v.as_str()).unwrap_or("id");
            let into = obj.get("into").and_then(|v| v.as_str()).unwrap_or("$.resolved_id");
            let filter = subst_patch(filter.clone(), payload)?;
            let rows = crate::crud::scan_rows(db, tbl).await?;
            let mut found = None;
            if let Ok(sf) = crate::storage::ir::parse_filter(&filter) {
                for row in rows {
                    let rec: crate::model::Record = serde_json::from_value(row.data)?;
                    if sf.matches(&rec.payload) {
                        found = Some(rec.payload.get(field).cloned().unwrap_or(Json::Null));
                        break;
                    }
                }
            }
            if let Some(v) = found {
                set_at(payload, into, v)?;
            } else {
                logs.push(format!("$resolve_other: no row in {tbl} matched {filter}"));
            }
        }
        // "$create_user": create an engine auth user (email + password) from the
        // triggering payload and write the resulting email into `into` (default
        // $.created_user_email). Ports auto_create_student_account (student
        // portal accounts). The engine's users are email-keyed, so the learner's
        // user_id becomes the email, not a source UUID.
        //   {"$create_user":{"email":"{{$.email}}","password":"{{$.password}}","role":"student","into":"$.user_id"}}
        "$create_user" => {
            let obj = val.as_object().ok_or_else(|| anyhow::anyhow!("$create_user must be an object"))?;
            let email = subst_strings_str(obj.get("email").and_then(|v| v.as_str()).ok_or_else(|| anyhow::anyhow!("$create_user needs \"email\""))?, payload)?;
            let password = subst_strings_str(obj.get("password").and_then(|v| v.as_str()).unwrap_or("MigrationTempPass2026"), payload)?;
            let role = obj.get("role").and_then(|v| v.as_str()).unwrap_or("student");
            let into = obj.get("into").and_then(|v| v.as_str()).unwrap_or("$.created_user_email");
            let principal = Principal { id: crate::TENANT.to_string(), role: "owner".to_string(), scope: None, writer: None };
            let user = crate::auth::user_signup(db, &email, &password, None, role, &principal).await
                .map_err(|e| anyhow::anyhow!("$create_user: {e}"))?;
            set_at(payload, into, Json::String(user.email))?;
        }
        "$transaction" => {
            let steps = val.as_array().ok_or_else(|| anyhow::anyhow!("$transaction must be an array"))?;
            for step in steps {
                let map = step.as_object().ok_or_else(|| anyhow::anyhow!("$transaction step must be an object"))?;
                let tbl = map.get("table").and_then(|v| v.as_str()).unwrap_or(table);
                let filter = map.get("filter").ok_or_else(|| anyhow::anyhow!("$transaction step needs \"filter\""))?;
                let patch = map.get("patch").ok_or_else(|| anyhow::anyhow!("$transaction step needs \"patch\""))?;
                crate::crud::record_patch_first_raw(db, tbl, &crate::storage::ir::parse_filter(filter)?, patch).await?;
            }
        }
        "$call" | "$notify" => {
            let obj = val.as_object().ok_or_else(|| anyhow::anyhow!("{key} must be an object"))?;
            let secrets = crate::secrets::secrets_map(db).await?;
            let url = resolve_secret_placeholders(&secrets, str_field(obj, "url")?);
            if !valid_url(&url) {
                anyhow::bail!("ssrf-blocked url '{url}'");
            }
            let timeout = obj.get("timeout_ms").and_then(|t| t.as_u64()).unwrap_or(15_000);
            let mut headers: Vec<(String, String)> = Vec::new();
            if let Some(h) = obj.get("headers") {
                if let Some(o) = h.as_object() {
                    for (k, v) in o {
                        headers.push((k.clone(), resolve_secret_placeholders(&secrets, v.as_str().unwrap_or(""))));
                    }
                } else if let Some(arr) = h.as_array() {
                    for el in arr {
                        if let Some(pair) = el.as_array() {
                            if pair.len() == 2 {
                                headers.push((pair[0].as_str().unwrap_or("").to_string(), resolve_secret_placeholders(&secrets, pair[1].as_str().unwrap_or(""))));
                            }
                        }
                    }
                }
            }
            let mut body = obj.get("body").cloned().unwrap_or(Json::Null);
            subst_strings(&mut body, payload)?;
            subst_secret_json(&secrets, &mut body);
            let content_type = obj.get("content_type").and_then(|c| c.as_str()).unwrap_or("application/json").to_string();
            let http_body = if let Some(form) = body.get("form") {
                let encoded = urlencode_form(form);
                crate::http::HttpBody::Raw {
                    content_type: "application/x-www-form-urlencoded".to_string(),
                    bytes: encoded.into_bytes(),
                }
            } else if !content_type.starts_with("application/json") {
                let text = body.as_str().map(str::to_string).unwrap_or_else(|| crate::storage::ir::scalar_text(&body));
                crate::http::HttpBody::Raw { content_type, bytes: text.into_bytes() }
            } else {
                crate::http::HttpBody::Json(body)
            };
            // Phase A ends here: everything above touched the DB (secrets)
            // and the payload. The network call itself either runs inline
            // (legacy, caller holds the engine Mutex) or is deferred to a
            // lock-free phase via execute_pending().await.
            logs.push(format!("call {url}"));
            match http_mode {
                HttpMode::Inline => {
                    let (status, resp_body) = crate::http::http_call_body(&url, &headers, &http_body, timeout).await?;
                    set_at(payload, "$.call_result", json!({ "status": status, "body": resp_body }))?;
                }
                HttpMode::Defer(pending) => pending.push(PendingHttp { url, headers, body: http_body, timeout_ms: timeout, result_into: None }),
            }
        }
        // "$send_email": provider email via MAIL_* secrets. Same deferred-HTTP
        // machinery as `$call`, but the URL is a fixed provider endpoint (no
        // SSRF surface) and the result lands on `$.email_result`.
        //   {"$send_email":{"to":"{{$.email}}","subject":"Welcome","text":"hi"}}
        "$send_email" => {
            let obj = val.as_object().ok_or_else(|| anyhow::anyhow!("$send_email must be an object"))?;
            let secrets = crate::secrets::secrets_map(db).await?;
            let mut to_v = obj.get("to").cloned().ok_or_else(|| anyhow::anyhow!("$send_email needs \"to\""))?;
            subst_strings(&mut to_v, payload)?;
            let to = crate::email::parse_addrs(&to_v)?;
            let subject = subst_strings_str(obj.get("subject").and_then(|v| v.as_str()).ok_or_else(|| anyhow::anyhow!("$send_email needs \"subject\""))?, payload)?;
            let opt = |name: &str| -> anyhow::Result<Option<String>> {
                match obj.get(name) {
                    None => Ok(None),
                    Some(v) => Ok(Some(subst_strings_str(v.as_str().ok_or_else(|| anyhow::anyhow!("$send_email \"{name}\" must be a string"))?, payload)?)),
                }
            };
            let text = opt("text")?;
            let html = opt("html")?;
            let from = opt("from")?;
            let provider = secrets.get(crate::email::SECRET_PROVIDER).map(String::from).unwrap_or_else(|| "resend".to_string());
            let call = crate::email::build(
                &provider,
                secrets.get(crate::email::SECRET_API_KEY).map(String::as_str),
                secrets.get(crate::email::SECRET_FROM).map(String::as_str),
                &crate::email::EmailRequest { to: to.clone(), subject, text, html, from },
            )?;
            logs.push(format!("email via {provider} to {}", to.join(",")));
            let timeout = obj.get("timeout_ms").and_then(|t| t.as_u64()).unwrap_or(15_000);
            match http_mode {
                HttpMode::Inline => {
                    let (status, resp_body) = crate::http::http_call_body(&call.url, &call.headers, &crate::http::HttpBody::Json(call.body), timeout).await?;
                    set_at(payload, "$.email_result", json!({ "provider": call.provider, "status": status, "body": resp_body }))?;
                }
                HttpMode::Defer(pending) => pending.push(PendingHttp {
                    url: call.url,
                    headers: call.headers,
                    body: crate::http::HttpBody::Json(call.body),
                    timeout_ms: timeout,
                    result_into: Some("$.email_result".to_string()),
                }),
            }
        }
        "$format" => {
            let obj = val.as_object().ok_or_else(|| anyhow::anyhow!("$format must be an object"))?;
            let field = str_field(obj, "field")?;
            let pattern = obj
                .get("pattern")
                .and_then(|p| p.as_str())
                .ok_or_else(|| anyhow::anyhow!("$format needs a pattern string"))?;
            let rendered = render_braces(pattern, payload)?;
            set_at(payload, field, Json::String(rendered))?;
        }
        "$merge" => {
            let obj = val.as_object().ok_or_else(|| anyhow::anyhow!("$merge must be an object"))?;
            let into = str_field(obj, "into")?;
            let patch = obj.values().next().cloned().unwrap_or(Json::Null);
            let mut target = get_at(payload, into);
            deep_merge(&mut target, &patch);
            set_at(payload, into, target)?;
        }
        "$push" => {
            let obj = val.as_object().ok_or_else(|| anyhow::anyhow!("$push must be an object"))?;
            let path = str_field(obj, "path")?;
            let value = obj.get("value").cloned().unwrap_or(Json::Null);
            let mut arr = get_at(payload, path);
            if !arr.is_array() {
                arr = Json::Array(Vec::new());
            }
            arr.as_array_mut().unwrap().push(value);
            set_at(payload, path, arr)?;
        }
        "$pull" => {
            let obj = val.as_object().ok_or_else(|| anyhow::anyhow!("$pull must be an object"))?;
            let path = str_field(obj, "path")?;
            let m = obj.get("match").cloned().unwrap_or(Json::Null);
            let arr = get_at(payload, path);
            if arr.is_array() {
                let filter = crate::storage::ir::parse_filter(&m).unwrap_or_default();
                let keep: Vec<Json> = arr
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|x| !filter.matches(x))
                    .cloned()
                    .collect();
                set_at(payload, path, Json::Array(keep))?;
            }
        }
        "$sort" => {
            let obj = val.as_object().ok_or_else(|| anyhow::anyhow!("$sort must be an object"))?;
            let path = str_field(obj, "path")?;
            let by = obj.get("by").and_then(|b| b.as_str()).map(String::from);
            let desc = obj.get("order").and_then(|o| o.as_str()).map(|o| o == "desc").unwrap_or(false);
            let mut arr = get_at(payload, path);
            if arr.is_array() {
                let list = arr.as_array_mut().unwrap();
                list.sort_by(|a, b| {
                    let va = match &by {
                        Some(p) => get_at(a, p),
                        None => a.clone(),
                    };
                    let vb = match &by {
                        Some(p) => get_at(b, p),
                        None => b.clone(),
                    };
                    let ord = match (va.as_f64(), vb.as_f64()) {
                        (Some(x), Some(y)) => x.partial_cmp(&y).unwrap_or(std::cmp::Ordering::Equal),
                        _ => va.to_string().cmp(&vb.to_string()),
                    };
                    if desc {
                        ord.reverse()
                    } else {
                        ord
                    }
                });
                set_at(payload, path, arr)?;
            }
        }
        "$slice" => {
            let obj = val.as_object().ok_or_else(|| anyhow::anyhow!("$slice must be an object"))?;
            let path = str_field(obj, "path")?;
            let start = obj.get("start").and_then(|s| s.as_i64()).unwrap_or(0).max(0) as usize;
            let end = obj.get("end").and_then(|e| e.as_i64()).map(|e| e.max(0) as usize);
            let arr = get_at(payload, path);
            if arr.is_array() {
                let list = arr.as_array().unwrap();
                let kept: Vec<Json> = match end {
                    Some(e) => list.iter().skip(start).take(e.saturating_sub(start)).cloned().collect(),
                    None => list.iter().skip(start).cloned().collect(),
                };
                set_at(payload, path, Json::Array(kept))?;
            }
        }
        "$set_state" => {
            let obj = val.as_object().ok_or_else(|| anyhow::anyhow!("$set_state must be an object"))?;
            let state = obj.get("state").cloned().unwrap_or(Json::Null);
            set_at(payload, "state", state)?;
        }
        "$schedule" => {
            let obj = val.as_object().ok_or_else(|| anyhow::anyhow!("$schedule must be an object"))?;
            let name = obj.get("name").and_then(|n| n.as_str()).unwrap_or("recipe");
            let at = obj
                .get("at")
                .and_then(|a| a.as_str())
                .or_else(|| obj.get("schedule").and_then(|s| s.as_str()))
                .ok_or_else(|| anyhow::anyhow!("$schedule needs \"at\" (cron or @every)"))?;
            let action = obj.get("action").cloned().unwrap_or(Json::Null);
            let next = crate::jobs::job_add(db, name, at, &action).await?;
            logs.push(format!("scheduled job '{name}' next run {next}"));
        }
        other => anyhow::bail!("unknown action '{other}'"),
    }
    Ok(())
}