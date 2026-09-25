use super::{arg_str, ok};
use base64::Engine;
use engine::model::Principal;
use engine::ServerlessEngine;
use serde_json::{json, Value as Json};

pub fn list(
    engine: &ServerlessEngine,
    _principal: &Principal,
    arguments: &Json,
) -> Result<Json, String> {
    let board = arg_str(arguments, "board")?;
    let slug = arguments.get("slug").and_then(|s| s.as_str());
    let no_subapps = arguments.get("no_subapps").and_then(|b| b.as_bool()).unwrap_or(false);
    let files = engine.list_assets(board).map_err(|e| e.to_string())?;
    // Registered sub-app prefixes (to exclude when asking for main only).
    let subs: Vec<String> = if no_subapps && slug.is_none() {
        engine.list_subapps(board).map_err(|e| e.to_string())?.into_iter().map(|s| s.slug).collect()
    } else {
        Vec::new()
    };
    let out: Vec<Json> = files
        .into_iter()
        .filter(|f| {
            match slug {
                Some(s) => f.key.starts_with(&format!("{s}/")),
                None if no_subapps => !subs.iter().any(|p| f.key.starts_with(&format!("{p}/"))),
                None => true,
            }
        })
        .map(|f| json!({ "key": f.key, "size": f.size }))
        .collect();
    ok(json!({ "files": out }))
}

pub fn export(
    engine: &ServerlessEngine,
    _principal: &Principal,
    arguments: &Json,
) -> Result<Json, String> {
    let board = arg_str(arguments, "board")?;
    let slug = arguments.get("slug").and_then(|s| s.as_str());
    let files = engine.list_assets(board).map_err(|e| e.to_string())?;
    // Main-dist export excludes every registered sub-app prefix; --slug
    // exports exactly that sub-app.
    let subs: Vec<String> = if slug.is_none() {
        engine.list_subapps(board).map_err(|e| e.to_string())?.into_iter().map(|s| s.slug).collect()
    } else {
        Vec::new()
    };
    let mut out: Vec<Json> = Vec::new();
    for f in files {
        let rel = f.key.as_str();
        let include = match slug {
            Some(s) => rel.starts_with(&format!("{s}/")),
            None => !subs.iter().any(|p| rel.starts_with(&format!("{p}/"))),
        };
        if !include {
            continue;
        }
        let (bytes, _ct) = engine
            .get_asset(board, rel)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("asset '{rel}' missing"))?;
        let rel_clean = match slug {
            Some(s) => rel.strip_prefix(&format!("{s}/")).unwrap_or(rel).to_string(),
            None => rel.to_string(),
        };
        out.push(json!({
            "path": rel_clean,
            "size": bytes.len(),
            "content_base64": base64::engine::general_purpose::STANDARD.encode(&bytes),
        }));
    }
    ok(json!({ "slug": slug, "files": out }))
}

pub fn import(
    engine: &mut ServerlessEngine,
    _principal: &Principal,
    arguments: &Json,
) -> Result<Json, String> {
    let board = arg_str(arguments, "board")?;
    let slug = arguments.get("slug").and_then(|s| s.as_str());
    let files = arguments
        .get("files")
        .and_then(|f| f.as_array())
        .ok_or_else(|| "missing 'files' array [{path, content_base64}]".to_string())?;
    if let Some(s) = slug {
        engine
            .put_subapp(board, s, arguments.get("title").and_then(|t| t.as_str()), None)
            .map_err(|e| e.to_string())?;
    }
    let mut imported = 0usize;
    for f in files {
        let path = f
            .get("path")
            .and_then(|p| p.as_str())
            .ok_or_else(|| "each file needs a 'path'".to_string())?;
        let b64 = f
            .get("content_base64")
            .and_then(|c| c.as_str())
            .ok_or_else(|| format!("file '{path}' needs 'content_base64'"))?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .map_err(|e| format!("file '{path}' bad base64: {e}"))?;
        let rel = match slug {
            Some(s) => format!("{s}/{path}"),
            None => path.to_string(),
        };
        engine.put_asset(board, &rel, &bytes).map_err(|e| e.to_string())?;
        imported += 1;
    }
    ok(json!({ "slug": slug, "imported": imported }))
}

pub fn put(
    engine: &mut ServerlessEngine,
    _principal: &Principal,
    arguments: &Json,
) -> Result<Json, String> {
    let board = arg_str(arguments, "board")?;
    let path = arg_str(arguments, "path")?;
    let content = arguments
        .get("content")
        .and_then(|c| c.as_str())
        .unwrap_or("");
    let slug = arguments.get("slug").and_then(|s| s.as_str());
    // When a slug is given, the path is relative to the sub-app and the
    // sub-app is auto-registered (upsert) so /srv/{board}/{slug} serves it.
    let rel = match slug {
        Some(s) => format!("{s}/{path}"),
        None => path.to_string(),
    };
    if let Some(s) = slug {
        engine
            .put_subapp(board, s, arguments.get("title").and_then(|t| t.as_str()), None)
            .map_err(|e| e.to_string())?;
    }
    engine
        .put_asset(board, &rel, content.as_bytes())
        .map_err(|e| e.to_string())?;
    ok(json!({ "asset": rel, "bytes": content.len(), "slug": slug }))
}

pub fn get(
    engine: &ServerlessEngine,
    _principal: &Principal,
    arguments: &Json,
) -> Result<Json, String> {
    let board = arg_str(arguments, "board")?;
    let path = arg_str(arguments, "path")?;
    let Some((bytes, content_type)) = engine.get_asset(board, path).map_err(|e| e.to_string())?
    else {
        return Err(format!("asset '{path}' not found"));
    };
    ok(json!({
        "asset": path,
        "content_type": content_type,
        "content": String::from_utf8_lossy(&bytes).to_string(),
    }))
}

pub fn upload(
    engine: &mut ServerlessEngine,
    _principal: &Principal,
    arguments: &Json,
) -> Result<Json, String> {
    let board = arg_str(arguments, "board")?;
    let filename = arguments.get("filename").and_then(|f| f.as_str()).unwrap_or("upload").to_string();
    let table = arguments.get("table").and_then(|t| t.as_str()).unwrap_or("files").to_string();
    let folder = arguments.get("folder").and_then(|f| f.as_str());
    let content_type = arguments.get("content_type").and_then(|c| c.as_str()).unwrap_or("application/octet-stream").to_string();
    let b64 = arg_str(arguments, "content_base64")?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|e| format!("bad base64: {e}"))?;
    let meta = json!({ "name": filename, "type": content_type });
    let seq = engine
        .upload(&board, &table, &filename, &content_type, &bytes, &meta, folder)
        .map_err(|e| e.to_string())?;
    ok(json!({ "seq": seq, "table": table, "folder": folder, "bytes": bytes.len() }))
}

pub fn delete(
    engine: &ServerlessEngine,
    _principal: &Principal,
    arguments: &Json,
) -> Result<Json, String> {
    let board = arg_str(arguments, "board")?;
    let path = arg_str(arguments, "path")?;
    // --slug <name> targets a file inside a sub-app without typing the prefix.
    let rel = match arguments.get("slug").and_then(|s| s.as_str()) {
        Some(s) => format!("{s}/{path}"),
        None => path.to_string(),
    };
    let deleted = engine.delete_asset(board, &rel).map_err(|e| e.to_string())?;
    ok(json!({ "asset": rel, "deleted": deleted }))
}