use super::{arg_str, ok};
use engine::model::Principal;
use engine::ServerlessEngine;
use serde_json::{json, Value as Json};

pub async fn list(
    engine: &ServerlessEngine,
    _principal: &Principal,
    _arguments: &Json,
) -> Result<Json, String> {
    let subs = engine.list_subapps().await.map_err(|e| e.to_string())?;
    let out: Vec<Json> = subs
        .into_iter()
        .map(|s| {
            json!({
                "slug": s.slug,
                "title": s.title,
                "index": s.index,
                "created_at": s.created_at,
            })
        })
        .collect();
    ok(json!({ "subapps": out }))
}

pub async fn remove(
    engine: &mut ServerlessEngine,
    _principal: &Principal,
    arguments: &Json,
) -> Result<Json, String> {
    let slug = arg_str(arguments, "slug")?;
    let removed = engine.remove_subapp(slug).await.map_err(|e| e.to_string())?;
    // --prune also deletes every asset under the slug's folder.
    let pruned = if arguments.get("prune").and_then(|b| b.as_bool()).unwrap_or(false) {
        engine.delete_asset_prefix(slug).await
            .map_err(|e| e.to_string())?
    } else {
        0
    };
    ok(json!({ "slug": slug, "removed": removed, "pruned": pruned }))
}
