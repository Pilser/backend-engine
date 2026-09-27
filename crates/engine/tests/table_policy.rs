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
        allow_anon_submit: None,
        max_rows: None,
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
fn scoped_keys_and_allows_table() {
    block_on(async {
        let mut e = ServerlessEngine::with_defaults();
        let (_rec, secret) = e
            .issue_key("writer", None, None, Some(vec!["a".to_string()]))
            .await
            .unwrap();
        let p = e.resolve_principal(Some(&secret), None).await.unwrap();
        assert!(p.allows_table("a"));
        assert!(!p.allows_table("b"));
        assert!(e.issue_key("writer", None, None, Some(vec![])).await.is_err());
        let (_r2, s2) = e.issue_key("reader", None, None, None).await.unwrap();
        let p2 = e.resolve_principal(Some(&s2), None).await.unwrap();
        assert!(p2.allows_table("anything"));
    });
}

#[test]
fn email_format() {
    block_on(async {
        let mut e = ServerlessEngine::with_defaults();
        e.create_table(
            "contacts",
            Some(json!({"type": "object", "properties": {"email": {"type": "string", "format": "email"}}})),
            None,
        )
        .await
        .unwrap();
        let p = engine::model::Principal {
            id: "t".into(),
            role: "owner".into(),
            scope: None,
            writer: None,
            tables: None,
        };
        assert!(e.insert_record("contacts", json!({"email": "a@x.io"}), None, false, &p).await.is_ok());
        assert!(e.insert_record("contacts", json!({"email": "not-an-email"}), None, false, &p).await.is_err());
        assert!(e.insert_record("contacts", json!({"email": "a@b"}), None, false, &p).await.is_err());
    });
}

#[test]
fn unset_strips_fields() {
    block_on(async {
        let mut e = ServerlessEngine::with_defaults();
        let p = engine::model::Principal {
            id: "t".into(),
            role: "owner".into(),
            scope: None,
            writer: None,
            tables: None,
        };
        e.create_table("forms", None, None).await.unwrap();
        e.add_recipe(&engine::model::Recipe {
            name: "strip".into(),
            when_json: json!({"event": "record.created", "table": "forms"}),
            match_json: None,
            enabled: true,
            dedup_on: None,
            actions_json: Some(json!([{"$unset": ["$.approved", "$.role"]}])),
            table: None,
        })
        .await
        .unwrap();
        let seq = e
            .insert_record("forms", json!({"msg": "hi", "approved": true, "role": "admin"}), None, false, &p)
            .await
            .unwrap();
        let rec = e.get_record("forms", seq).await.unwrap().unwrap();
        assert_eq!(rec.payload.get("msg"), Some(&json!("hi")));
        assert!(rec.payload.get("approved").is_none());
        assert!(rec.payload.get("role").is_none());
    });
}

#[test]
fn max_rows_trims_oldest() {
    block_on(async {
        let mut e = ServerlessEngine::with_defaults();
        let p = engine::model::Principal {
            id: "t".into(),
            role: "owner".into(),
            scope: None,
            writer: None,
            tables: None,
        };
        e.create_table("log", None, None).await.unwrap();
        e.set_table_policy("log", &json!({"max_rows": 2})).await.unwrap();
        for i in 1..=4 {
            e.insert_record("log", json!({"n": i}), None, false, &p).await.unwrap();
        }
        e.ttl_sweep().await.unwrap();
        let rows = e.list_records("log", 50, None, 0, "asc").await.unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].payload["n"], json!(3));
    });
}

#[test]
fn access_audit_answers() {
    let inbox = cfg(None, Some(true));
    let a = inbox.access_audit(false);
    assert!(a["anon_can"].as_array().unwrap().contains(&json!("submit")));
    assert!(a["anon_cannot"].as_array().unwrap().contains(&json!("list")));
    let open = cfg(Some(true), None);
    assert!(open.access_audit(false)["anon_can"].as_array().unwrap().len() == 6);
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
