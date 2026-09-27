//! Native plugins (Phase B): manifest validation + install/remove lifecycle.
//! Runs on the in-memory engine (with_defaults + block_on, no tokio).

use engine::ServerlessEngine;
use futures_executor::block_on;
use serde_json::json;

fn manifest() -> serde_json::Value {
    json!({
        "slug": "shop",
        "title": "Shop",
        "tables": [{"table": "plugin_shop_orders", "unique_key": "$.id"}],
        "recipes": [{"name": "notify", "when": {"event": "record.created", "table": "plugin_shop_orders"}, "actions": [{"$log": "x"}]}],
        "jobs": [{"name": "sync", "schedule": "@every 1h", "action": {"type": "http", "url": "https://example.com/ping"}}],
        "routes": [{"name": "orders", "method": "GET", "op": "query", "table": "plugin_shop_orders"}],
    })
}

#[test]
fn validation() {
    block_on(async {
        let mut e = ServerlessEngine::with_defaults();
        let mut bad = manifest();
        bad["slug"] = json!("Bad Slug!");
        assert!(engine::plugins::install(&mut e, &bad).await.is_err());
        let mut bad = manifest();
        bad["tables"] = json!([{"table": "orders"}]);
        assert!(engine::plugins::install(&mut e, &bad).await.is_err());
        let mut bad = manifest();
        bad["routes"] = json!([{"name": "x", "method": "GET", "op": "sql", "table": "t"}]);
        assert!(engine::plugins::install(&mut e, &bad).await.is_err());
    });
}

#[test]
fn merge_filter_json() {
    use engine::plugins::merge_filter_json;
    assert_eq!(merge_filter_json(None, None), json!(null));
    assert_eq!(merge_filter_json(Some(&json!({"a": 1})), None), json!({"a": 1}));
    assert_eq!(
        merge_filter_json(Some(&json!({"a": 1})), Some(&json!({"b": 2}))),
        json!({"$and": [{"a": 1}, {"b": 2}]})
    );
    assert_eq!(engine::plugins::binding_limit(&json!({}), None), 50);
    assert_eq!(engine::plugins::binding_limit(&json!({"limit": 10}), Some(99)), 10);
}

#[test]
fn route_bindings_resolve() {
    block_on(async {
        let mut e = ServerlessEngine::with_defaults();
        engine::plugins::install(&mut e, &manifest()).await.unwrap();
        let b = engine::plugins::find_route(&e, "shop", "orders").await.unwrap();
        assert!(b.is_some());
        assert_eq!(b.unwrap()["table"], json!("plugin_shop_orders"));
        assert!(engine::plugins::find_route(&e, "shop", "nope").await.unwrap().is_none());
        let routes = engine::plugins::routes_for(&e, "shop").await.unwrap();
        assert_eq!(routes.len(), 1);
    });
}

#[test]
fn install_remove_lifecycle() {
    block_on(async {
        let mut e = ServerlessEngine::with_defaults();
        let out = engine::plugins::install(&mut e, &manifest()).await.unwrap();
        assert_eq!(out["slug"], json!("shop"));
        assert!(e.get_table("plugin_shop_orders").await.unwrap().is_some());
        assert!(e.get_recipe("shop__notify").await.unwrap().is_some());
        assert!(e.list_jobs().await.unwrap().iter().any(|j| j.name == "shop__sync"));
        assert_eq!(engine::plugins::list(&e).await.unwrap().len(), 1);
        // Idempotent reinstall.
        engine::plugins::install(&mut e, &manifest()).await.unwrap();
        assert_eq!(engine::plugins::list(&e).await.unwrap().len(), 1);
        // Remove keeps tables without prune.
        let r = engine::plugins::remove(&mut e, "shop", false).await.unwrap();
        assert_eq!(r["tables_dropped"], json!([]));
        assert!(e.get_table("plugin_shop_orders").await.unwrap().is_some());
        assert!(e.get_recipe("shop__notify").await.unwrap().is_none());
        // Reinstall then prune.
        engine::plugins::install(&mut e, &manifest()).await.unwrap();
        let r = engine::plugins::remove(&mut e, "shop", true).await.unwrap();
        assert_eq!(r["tables_dropped"], json!(["plugin_shop_orders"]));
        assert!(engine::plugins::list(&e).await.unwrap().is_empty());
        assert!(engine::plugins::remove(&mut e, "shop", false).await.is_err());
    });
}
