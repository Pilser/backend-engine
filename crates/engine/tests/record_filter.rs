//! Record filter level (regression): `$.field` paths address the PAYLOAD,
//! envelope metadata (`$.table`, …) keeps matching. Before the match_view
//! fix, `eq` on payload fields resolved Null (matched nothing) while `neq`
//! matched everything — on every adapter.

use engine::model::Principal;
use engine::ServerlessEngine;
use futures_executor::block_on;
use serde_json::json;

fn owner() -> Principal {
    Principal { id: "test".to_string(), role: "owner".to_string(), scope: None, writer: None, tables: None }
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
fn same_seq_across_tables_stays_isolated() {
    // The vanishing-rows hunt (2026-09-27): the D1 adapter stored every
    // table's records in one physical table keyed by bare per-table seqs,
    // so `INSERT OR REPLACE` let tables silently destroy each other's rows.
    // Contract: same seq in different tables must never collide — on ANY
    // adapter (memory is table-scoped; D1 namespaces physical keys).
    block_on(async {
        let mut e = ServerlessEngine::with_defaults();
        let p = owner();
        e.create_table("t1", None, None).await.unwrap();
        e.create_table("t2", None, None).await.unwrap();
        let s1 = e.insert_record("t1", json!({"v": "one"}), None, false, &p).await.unwrap();
        let s2 = e.insert_record("t2", json!({"v": "two"}), None, false, &p).await.unwrap();
        assert_eq!((s1, s2), (1, 1));
        let r1 = e.get_record("t1", 1).await.unwrap().unwrap();
        let r2 = e.get_record("t2", 1).await.unwrap().unwrap();
        assert_eq!(r1.payload["v"], json!("one"));
        assert_eq!(r2.payload["v"], json!("two"));
        let _ = e.patch_record("t1", 1, &json!({"v": "uno"}), None).await.unwrap();
        assert_eq!(e.get_record("t2", 1).await.unwrap().unwrap().payload["v"], json!("two"));
        assert!(e.delete_record("t1", 1).await.unwrap());
        assert!(e.get_record("t2", 1).await.unwrap().is_some());
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
