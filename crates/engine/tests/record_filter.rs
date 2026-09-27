//! Record filter level (regression): `$.field` paths address the PAYLOAD,
//! envelope metadata (`$.table`, …) keeps matching. Before the match_view
//! fix, `eq` on payload fields resolved Null (matched nothing) while `neq`
//! matched everything — on every adapter.

use engine::model::Principal;
use engine::ServerlessEngine;
use futures_executor::block_on;
use serde_json::json;

fn owner() -> Principal {
    Principal { id: "test".to_string(), role: "owner".to_string(), scope: None, writer: None }
}

fn filter(f: serde_json::Value) -> engine::storage::ir::SrvFilter {
    engine::storage::ir::parse_filter(&f).unwrap()
}

#[test]
fn eq_matches_payload_fields() {
    block_on(async {
        let mut e = ServerlessEngine::with_defaults();
        let p = owner();
        e.create_table("shop", None, None).await.unwrap();
        e.insert_record("shop", json!({"item": "book"}), None, false, &p).await.unwrap();
        e.insert_record("shop", json!({"item": "pen"}), None, false, &p).await.unwrap();
        let hit = e
            .query_records("shop", &filter(json!({"item": {"eq": "book"}})), &[], 50, 0)
            .await
            .unwrap();
        assert_eq!(hit.len(), 1);
        assert_eq!(hit[0].payload["item"], json!("book"));
        let miss = e
            .query_records("shop", &filter(json!({"item": {"eq": "zzz"}})), &[], 50, 0)
            .await
            .unwrap();
        assert!(miss.is_empty());
        let bare = e
            .query_records("shop", &filter(json!({"item": "pen"})), &[], 50, 0)
            .await
            .unwrap();
        assert_eq!(bare.len(), 1);
    });
}

#[test]
fn table_scoping_survives() {
    block_on(async {
        let mut e = ServerlessEngine::with_defaults();
        let p = owner();
        e.create_table("a", None, None).await.unwrap();
        e.create_table("b", None, None).await.unwrap();
        e.insert_record("a", json!({"x": 1}), None, false, &p).await.unwrap();
        e.insert_record("b", json!({"x": 1}), None, false, &p).await.unwrap();
        let ra = e.query_records("a", &filter(json!({"x": {"eq": 1}})), &[], 50, 0).await.unwrap();
        assert_eq!(ra.len(), 1);
        let rb = e.query_records("b", &filter(json!({"x": {"eq": 1}})), &[], 50, 0).await.unwrap();
        assert_eq!(rb.len(), 1);
    });
}
