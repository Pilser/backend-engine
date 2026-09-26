//! Static SPA hosting (`GET /srv/…`). Ports the donor `static_site` handler:
//! sub-app slug routing, `index.html` fallbacks, `<base>` injection so
//! relative asset URLs survive deep-link refreshes, and long caching for
//! hashed assets. No `{board}` — one worker serves one app.

use worker::{Request, Response, Result, RouteContext};

use crate::{auth, cors};

pub async fn srv_root(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let _ = (&req, &ctx);
    // Trailing-slash canonical form so relative "./…" URLs in index.html
    // resolve against /srv/ instead of /.
    Ok(cors::redirect_to("/srv/"))
}

fn has_file_ext(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    let Some(dot) = name.rfind('.') else {
        return false;
    };
    dot > 0 && dot < name.len() - 1
}

fn inject_base_tag(html: &[u8], base_tag: &str) -> String {
    let src = String::from_utf8_lossy(html);
    if let Some(hi) = src.to_ascii_lowercase().find("<head") {
        if let Some(end) = src[hi..].find('>') {
            let pos = hi + end + 1;
            return format!("{}{}{}", &src[..pos], base_tag, &src[pos..]);
        }
    }
    // No <head>: prepend so the base still applies.
    format!("{base_tag}{src}")
}

pub async fn serve(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let rel = ctx.param("path").cloned().unwrap_or_default();
    let mut app = match auth::ctx_for(&req, &ctx).await {
        Ok(c) => c,
        Err(r) => return Ok(r),
    };
    let tenant = match app.engine.tenant().await {
        Ok(t) => t,
        Err(e) => return Ok(cors::srv(&e)),
    };
    if !tenant.public_reads && !auth::require_read(&app.principal) {
        return Ok(cors::deny("private app"));
    }

    let mut base = "/srv/".to_string();
    let mut target = rel.clone();
    let mut fallback = "index.html".to_string();

    // Sub-app slug routing: serve from the slug prefix with per-slug SPA
    // fallback so deep links inside the sub-app land on ITS index.html.
    let first_seg = rel.split('/').next().unwrap_or("");
    if !first_seg.is_empty() {
        if let Ok(Some(sub)) = app.engine.get_subapp(first_seg).await {
            base = format!("/srv/{}/", sub.slug);
            fallback = sub.index.clone();
            target = match rel.strip_prefix(&sub.slug) {
                Some(rest) if !rest.is_empty() => format!("{}{}", sub.slug, rest),
                _ => sub.index.clone(),
            };
        }
    }
    if target.is_empty() {
        target = fallback.clone();
    }

    let mut fetched = match app.engine.get_asset(&target).await {
        Ok(v) => v,
        Err(_) => None,
    };
    if fetched.is_none() && !target.ends_with(".html") {
        fetched = app
            .engine
            .get_asset(&format!("{target}/index.html"))
            .await
            .unwrap_or(None);
    }
    // SPA fallback: extensionless unknown paths serve the index.
    if fetched.is_none() && !has_file_ext(&target) {
        fetched = app.engine.get_asset(&fallback).await.unwrap_or(None);
    }
    let Some((data, content_type)) = fetched else {
        return Ok(cors::gone("not found"));
    };
    let (body, cache) = if content_type.starts_with("text/html") {
        let tag = format!("<base href=\"{base}\">");
        (inject_base_tag(&data, &tag).into_bytes(), "no-cache")
    } else {
        (data, "public, max-age=3600")
    };
    Ok(cors::bytes(body, &content_type, cache))
}
