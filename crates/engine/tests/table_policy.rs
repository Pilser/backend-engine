//! Per-table access policy (S1: P0 proposals): anon_read_open matrix,
//! policy_set patch semantics, and backward-compatible deserialization.

use engine::model::TableConfig;
use engine::ServerlessEngine;
use futures_executor::block_on;
use serde_json::json;

fn cfg(public_read: Option<bool>, write_only: Option<bool>) -> TableConfig {
    TableConfig {
        table: "t".into(),
        schema_json: None,
        unique_key: None,
        computed_json: None,
        validate_json: None,
        redact_json: None,
        ttl_seconds: None,
        ttl_field: None,
        created_at: None,
        public_read,
        write_only,
    }
}

#[test]
fn anon_read_matrix() {
    assert!(cfg(None, None).anon_read_open(true));
    assert!(!cfg(None, None).anon_read_open(false));
    assert!(cfg(Some(true), None).anon_read_open(false));
    assert!(!cfg(Some(false), None).anon_read_open(true));
    assert!(!cfg(None, Some(true)).anon_read_open(true));
    assert!(!cfg(Some(true), Some(true)).anon_read_open(true));
    assert!(cfg(None, None).anon_submit_open() == false);
    assert!(cfg(None, Some(true)).anon_submit_open());
}

#[test]
fn old_configs_deserialize() {
    let c: TableConfig = serde_json::from_value(json!({"table": "t"})).unwrap();
    assert_eq!((c.public_read, c.write_only), (None, None));
}

#[test]
fn policy_set_patch() {
    block_on(async {
        let mut e = ServerlessEngine::with_defaults();
        e.create_table("box", None, None).await.unwrap();
        e.set_table_policy("box", &json!({"public_read": true})).await.unwrap();
        let c = e.get_table("box").await.unwrap().unwrap();
        assert_eq!((c.public_read, c.write_only), (Some(true), None));
        e.set_table_policy("box", &json!({"public_read": null, "write_only": true})).await.unwrap();
        let c = e.get_table("box").await.unwrap().unwrap();
        assert_eq!((c.public_read, c.write_only), (None, Some(true)));
        assert!(e.set_table_policy("box", &json!({"public_read": "yes"})).await.is_err());
        assert!(e.set_table_policy("nope", &json!({"public_read": true})).await.is_err());
    });
}
