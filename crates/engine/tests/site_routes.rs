//! Site + request routes: validation, matching, sitemap render, manifest
//! wiring. Runs on the in-memory engine (with_defaults + block_on).

use engine::ServerlessEngine;
use futures_executor::block_on;
use serde_json::json;

#[test]
fn path_rules() {
    assert!(engine::site::valid_path("/robots.txt").is_ok());
    assert!(engine::site::valid_path("/docs/*").is_ok());
    assert!(engine::site::valid_path("/").is_err());
    assert!(engine::site::valid_path("nope").is_err());
    assert!(engine::site::valid_path("/api/x").is_err());
    assert!(engine::site::valid_path("/mcp").is_err());
    assert!(engine::site::valid_path("/srv/y").is_err());
    assert!(engine::site::valid_path("/ws").is_err());
    assert!(engine::site::valid_path("/healthz").is_err());
    assert!(engine::site::valid_path("/a b").is_err());
}

#[test]
fn render_helpers() {
    let xml = engine::site::render_sitemap(
        "https://x.example.com",
        "/p/",
        "slug",
        &[json!({"payload": {"slug": "a&b"}}), json!({"nope": 1})],
    );
    assert!(xml.contains("<loc>https://x.example.com/p/a&amp;b</loc>"));
    assert_eq!(xml.matches("<url>").count(), 1);
    assert_eq!(
        engine::site::render_target("https://cdn.example.com/{{$.path}}?t={{$.tenant}}&{{$.query}}", "acme", "a/b", "x=1"),
        "https://cdn.example.com/a/b?t=acme&x=1"
    );
}

#[test]
fn route_lifecycle() {
    block_on(async {
        let mut e = ServerlessEngine::with_defaults();
        engine::site::route_put(&mut e, "tenant", &json!({"path": "/r", "kind": "redirect", "to": "/srv/"})).await.unwrap();
        engine::site::route_put(&mut e, "tenant", &json!({"path": "/d/*", "kind": "text", "body": "x"})).await.unwrap();
        let hit = engine::site::route_match(&e, "GET", "/r").await.unwrap().unwrap();
        assert_eq!(hit["spec"]["kind"], json!("redirect"));
        // Exact beats prefix; longest prefix wins.
        engine::site::route_put(&mut e, "tenant", &json!({"path": "/d/y", "kind": "text", "body": "y"})).await.unwrap();
        let hit = engine::site::route_match(&e, "GET", "/d/y").await.unwrap().unwrap();
        assert_eq!(hit["path"], json!("/d/y"));
        let hit = engine::site::route_match(&e, "GET", "/d/z").await.unwrap().unwrap();
        assert_eq!(hit["path"], json!("/d/*"));
        assert!(engine::site::route_match(&e, "GET", "/nope").await.unwrap().is_none());
        assert!(engine::site::route_remove(&mut e, "/r").await.unwrap());
        assert!(!engine::site::route_remove(&mut e, "/r").await.unwrap());
    });
}

#[test]
fn manifest_site_routes() {
    block_on(async {
        let mut e = ServerlessEngine::with_defaults();
        let m = json!({
            "slug": "docs",
            "tables": [{"table": "plugin_docs_pages"}],
            "site_routes": [
                {"path": "/robots.txt", "kind": "text", "body": "User-agent: *\nAllow: /\n"},
                {"path": "/sitemap.xml", "kind": "query", "table": "plugin_docs_pages"}
            ],
            "request_routes": [
                {"path": "/cdn/*", "kind": "proxy", "target": "https://cdn.example.com/{{$.path}}"}
            ],
        });
        let out = engine::plugins::install(&mut e, &m).await.unwrap();
        assert_eq!(out["site_routes"].as_array().unwrap().len(), 3);
        let hit = engine::site::route_match(&e, "GET", "/robots.txt").await.unwrap().unwrap();
        assert_eq!(hit["owner"], json!("plugin:docs"));
        let pre = engine::site::route_match(&e, "GET", "/cdn/a/b").await.unwrap().unwrap();
        assert_eq!(pre["spec"]["kind"], json!("proxy"));
        // Conflict: same path, other owner.
        assert!(engine::site::route_put(&mut e, "tenant", &json!({"path": "/robots.txt", "kind": "text", "body": "x"})).await.is_err());
        // Remove prunes owned site rows.
        engine::plugins::remove(&mut e, "docs", true).await.unwrap();
        assert!(engine::site::route_list(&e).await.unwrap().is_empty());
    });
}
