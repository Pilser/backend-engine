//! Single-tenant engine contract tests (Phase 5b).
//!
//! End-to-end coverage of the async engine over the in-memory adapters — the
//! same behavioral contract the D1/R2 adapters must honor (verified live in
//! Phase 8). Runs natively in CI via `futures-executor::block_on`
//! (no tokio — still banned).

use engine::model::Principal;
use engine::storage::ir::{Agg, SrvFilter};
use engine::ServerlessEngine;
use futures_executor::block_on;
use serde_json::json;

fn owner() -> Principal {
    Principal { id: "test".to_string(), role: "owner".to_string(), scope: None, writer: None }
}

fn engine() -> ServerlessEngine {
    ServerlessEngine::with_defaults()
}

#[test]
fn tenant_config_lifecycle() {
    block_on(async {
        let mut e = engine();
        let t = e.tenant().await.unwrap();
        assert_eq!(t.title, engine::TENANT);
        e.update_tenant(&json!({"title": "shop", "public_reads": true})).await.unwrap();
        let t = e.tenant().await.unwrap();
        assert_eq!(t.title, "shop");
        assert!(t.public_reads);
    });
}

#[test]
fn table_lifecycle() {
    block_on(async {
        let mut e = engine();
        let cfg = e.create_table("orders", None, Some("$.id")).await.unwrap();
        assert_eq!(cfg.table, "orders");
        assert!(e.create_table("orders", None, None).await.is_err());
        assert!(e.get_table("orders").await.unwrap().is_some());
        assert_eq!(e.list_tables().await.unwrap().len(), 1);
        assert!(e.drop_table("orders").await.unwrap());
        assert!(!e.drop_table("orders").await.unwrap());
    });
}

#[test]
fn record_crud_and_query() {
    block_on(async {
        let mut e = engine();
        let p = owner();
        e.create_table("items", None, None).await.unwrap();
        let s1 = e.insert_record("items", json!({"name": "a", "price": 10}), None, false, &p).await.unwrap();
        let s2 = e.insert_record("items", json!({"name": "b", "price": 30}), None, false, &p).await.unwrap();
        assert!(s2 > s1);
        let rec = e.get_record("items", s1).await.unwrap().unwrap();
        assert_eq!(rec.payload["name"], json!("a"));
        assert_eq!(e.count_records("items").await.unwrap(), 2);

        let filter: SrvFilter =
            engine::parse_filter(&json!([{"field": "$.price", "op": "gte", "value": 20}])).unwrap();
        let out = e.query_records("items", &filter, &[], 10, 0).await.unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].payload["name"], json!("b"));

        let list = e.list_records("items", 10, None, 0, "desc").await.unwrap();
        assert_eq!(list[0].seq, s2);

        let merged =
            e.patch_record("items", s1, &json!({"$set": {"price": 15}}), None).await.unwrap();
        assert_eq!(merged["price"], json!(15));

        e.set_record("items", s1, json!({"name": "a2"}), None).await.unwrap();
        assert!(e.delete_record("items", s2).await.unwrap());
        assert_eq!(e.count_records("items").await.unwrap(), 1);
    });
}

#[test]
fn schema_computed_unique_redact() {
    block_on(async {
        let mut e = engine();
        let p = owner();
        e.create_table(
            "orders",
            Some(json!({
                "type": "object",
                "required": ["total"],
                "properties": {"total": {"type": "number"}, "status": {"type": "string"}}
            })),
            Some("$.id"),
        )
        .await
        .unwrap();
        // Missing required field / wrong type are rejected.
        assert!(e.insert_record("orders", json!({"id": "o1"}), None, false, &p).await.is_err());
        assert!(e
            .insert_record("orders", json!({"id": "o1", "total": "NaN"}), None, false, &p)
            .await
            .is_err());
        // Duplicate unique value rejected; upsert replaces.
        e.insert_record("orders", json!({"id": "o1", "total": 5}), None, false, &p).await.unwrap();
        assert!(e.insert_record("orders", json!({"id": "o1", "total": 6}), None, false, &p).await.is_err());
        let seq =
            e.insert_record("orders", json!({"id": "o1", "total": 7}), None, true, &p).await.unwrap();
        let rec = e.get_record("orders", seq).await.unwrap().unwrap();
        assert_eq!(rec.payload["total"], json!(7));

        // Computed fields derive on write.
        e.set_computed("orders", &json!({"$.tax": "$.total * 0.2"})).await.unwrap();
        let seq2 =
            e.insert_record("orders", json!({"id": "o2", "total": 10}), None, false, &p).await.unwrap();
        let rec2 = e.get_record("orders", seq2).await.unwrap().unwrap();
        assert_eq!(rec2.payload["tax"], json!(2.0));

        // Redaction masks on read only.
        e.set_redact("orders", &json!(["$.id"])).await.unwrap();
        let rec3 = e.get_record("orders", seq2).await.unwrap().unwrap();
        assert_eq!(rec3.payload["id"], json!("***"));

        // Validation rules block writes.
        e.set_validate("orders", &json!([{"when": "$.total < 0", "error": "negative"}])).await.unwrap();
        assert!(e
            .insert_record("orders", json!({"id": "o3", "total": -1}), None, false, &p)
            .await
            .is_err());
    });
}

#[test]
fn recipes_fire_on_write() {
    block_on(async {
        let mut e = engine();
        let p = owner();
        e.create_table("events", None, None).await.unwrap();
        e.add_recipe(&engine::Recipe {
            name: "stamp".to_string(),
            when_json: json!({"event": "record.created", "table": "events"}),
            match_json: None,
            enabled: true,
            dedup_on: None,
            actions_json: Some(json!([{"$set": {"$.stamped": true}}])),
            table: Some("events".to_string()),
        })
        .await
        .unwrap();
        let seq = e.insert_record("events", json!({"kind": "click"}), None, false, &p).await.unwrap();
        let rec = e.get_record("events", seq).await.unwrap().unwrap();
        assert_eq!(rec.payload["stamped"], json!(true));
        assert_eq!(e.list_recipes().await.unwrap().len(), 1);
        e.set_recipe_enabled("stamp", false).await.unwrap();
        let seq2 = e.insert_record("events", json!({"kind": "view"}), None, false, &p).await.unwrap();
        let rec2 = e.get_record("events", seq2).await.unwrap().unwrap();
        assert!(rec2.payload.get("stamped").is_none());
        assert!(e.remove_recipe("stamp").await.is_ok());
    });
}

#[test]
fn search_and_aggregate() {
    block_on(async {
        let mut e = engine();
        let p = owner();
        e.create_table("docs", None, None).await.unwrap();
        e.insert_record("docs", json!({"title": "red apple", "n": 1}), None, false, &p).await.unwrap();
        e.insert_record("docs", json!({"title": "green apple", "n": 2}), None, false, &p).await.unwrap();
        let hits = e
            .search_records("docs", "apple", &SrvFilter::default(), 10, 0, false)
            .await
            .unwrap();
        assert_eq!(hits.len(), 2);

        let count = e
            .aggregate_records("docs", &SrvFilter::default(), Agg::Count, None, None)
            .await
            .unwrap();
        assert_eq!(count[0]["value"], json!(2));
        let sum =
            e.aggregate_records("docs", &SrvFilter::default(), Agg::Sum, Some("$.n"), None).await.unwrap();
        assert_eq!(sum[0]["value"], json!(3.0));
    });
}

#[test]
fn keys_and_auth() {
    block_on(async {
        let mut e = engine();
        let (rec, secret) = e.issue_key("writer", None, None).await.unwrap();
        assert_eq!(e.list_keys().await.unwrap().len(), 1);
        let princ = e.resolve_principal(Some(&secret), None).await.unwrap();
        assert_eq!(princ.role, "writer");
        let anon = e.resolve_principal(None, None).await.unwrap();
        assert_eq!(anon.role, "none");
        e.revoke_key(&rec.bucket).await.unwrap();
        let princ2 = e.resolve_principal(Some(&secret), None).await.unwrap();
        assert_eq!(princ2.role, "none");
    });
}

#[test]
fn users_flow() {
    block_on(async {
        let mut e = engine();
        let admin = owner();
        let user =
            e.signup_user("a@x.y", "secret123", None, "reader", &admin).await.unwrap();
        assert_eq!(user.email, "a@x.y");
        let (token, _jwt) = e.login_user("a@x.y", "secret123").await.unwrap();
        assert!(e.login_user("a@x.y", "wrong").await.is_err());
        let me = e.user_by_token(&token).await.unwrap().unwrap();
        assert_eq!(me.email, "a@x.y");
        e.set_user_role("a@x.y", "writer", &admin).await.unwrap();
        assert_eq!(e.list_users().await.unwrap().len(), 1);
        assert!(e.logout_user(&token).await.unwrap());
        assert!(e.user_by_token(&token).await.unwrap().is_none());
    });
}

#[test]
fn secrets_roundtrip() {
    block_on(async {
        let mut e = engine();
        e.set_secret("API_KEY", "sk_live").await.unwrap();
        assert_eq!(e.secret_value("API_KEY").await.unwrap().as_deref(), Some("sk_live"));
        let list = e.list_secrets().await.unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "API_KEY");
        assert!(e.get_secret("API_KEY").await.unwrap().is_some());
        e.remove_secret("API_KEY").await.unwrap();
        assert!(e.secret_value("API_KEY").await.unwrap().is_none());
    });
}

#[test]
fn jobs_lifecycle() {
    block_on(async {
        let mut e = engine();
        e.add_job("ping", "*/5 * * * *", &json!({"type": "http", "url": "https://x.test"})).await.unwrap();
        assert!(e.get_job("ping").await.unwrap().is_some());
        assert_eq!(e.list_jobs().await.unwrap().len(), 1);
        // Force-due in the past, then sweep.
        e.job_reschedule("ping", Some("2000-01-01 00:00:00")).await.unwrap();
        let due = e.job_due("2026-01-01 00:00:00", 10).await.unwrap();
        assert!(due.iter().any(|j| j.name == "ping"));
        e.job_mark("ping", "2026-01-01 00:00:00", "ok", "fine").await.unwrap();
        e.job_run_insert("ping", "2026-01-01 00:00:00", 12, "ok", "fine", "").await.unwrap();
        assert_eq!(e.job_runs(None, 10).await.unwrap().len(), 1);
        assert!(e.remove_job("ping").await.unwrap());
    });
}

#[test]
fn hooks_fire_and_flush() {
    block_on(async {
        let mut e = engine();
        let p = owner();
        e.create_table("t", None, None).await.unwrap();
        e.register_hook("https://hooks.test/in", None).await.unwrap();
        assert_eq!(e.list_hooks().await.unwrap().len(), 1);
        e.insert_record("t", json!({"a": 1}), None, false, &p).await.unwrap();
        let due = e.hook_deliveries_due("2999-01-01 00:00:00", 10).await.unwrap();
        assert!(!due.is_empty());
        let id = due[0]["id"].as_str().unwrap().to_string();
        e.mark_hook_delivery(&id, 1, Some("200"), None, Some("2026-01-01 00:00:00")).await.unwrap();
        e.remove_hook("https://hooks.test/in").await.unwrap();
        assert!(e.list_hooks().await.unwrap().is_empty());
    });
}

#[test]
fn files_and_assets() {
    block_on(async {
        let mut e = engine();
        let p = owner();
        e.create_table("files", None, None).await.unwrap();
        let bytes = b"hello-bytes".to_vec();
        let seq = e
            .upload("files", "a.bin", "application/octet-stream", &bytes, &json!({}), None)
            .await
            .unwrap();
        assert!(seq > 0);
        assert!(!e.list_files().await.unwrap().is_empty());
        let rec = e.get_record("files", seq).await.unwrap().unwrap();
        let file_key = rec.payload["file"].as_str().unwrap().to_string();
        let (back, ct) = e.download("files", &file_key).await.unwrap().unwrap();
        assert_eq!(back, bytes);
        assert_eq!(ct, "application/octet-stream");
        let _ = p;
        e.put_asset("app/index.html", b"<h1>hi</h1>").await.unwrap();
        let (html, ct) = e.get_asset("app/index.html").await.unwrap().unwrap();
        assert_eq!(html, b"<h1>hi</h1>");
        assert!(ct.contains("text/html"));
        assert_eq!(e.list_assets().await.unwrap().len(), 1);
        assert!(e.delete_asset("app/index.html").await.unwrap());

        e.put_subapp("portal", Some("Portal"), None).await.unwrap();
        assert_eq!(e.list_subapps().await.unwrap().len(), 1);
        assert!(e.get_subapp("portal").await.unwrap().is_some());
        assert!(e.remove_subapp("portal").await.unwrap());
    });
}

#[test]
fn audit_and_ttl() {
    block_on(async {
        let mut e = engine();
        let p = owner();
        e.create_table("t", None, None).await.unwrap();
        e.set_audit(true).await.unwrap();
        e.insert_record("t", json!({"a": 1}), None, false, &p).await.unwrap();
        let log = e.audit_list(None, 10).await.unwrap();
        assert!(log.iter().any(|r| r["event"] == json!("created")));

        // Per-record expiry via a ttl field in the past.
        e.set_ttl("t", None, Some("$.exp")).await.unwrap();
        e.insert_record("t", json!({"exp": "2000-01-01 00:00:00"}), None, false, &p).await.unwrap();
        assert_eq!(e.count_records("t").await.unwrap(), 1);
        let swept = e.ttl_sweep().await.unwrap();
        assert_eq!(swept, 1);
        e.clear_ttl("t").await.unwrap();
    });
}

#[test]
fn links_and_rate() {
    block_on(async {
        let mut e = engine();
        let p = owner();
        e.create_table("child", None, None).await.unwrap();
        e.create_table("parent", None, None).await.unwrap();
        e.insert_record("parent", json!({"id": 7, "name": "P"}), None, false, &p).await.unwrap();
        e.set_link("child", "parent", "$.pid", "$.id").await.unwrap();
        assert!(e.get_link().await.unwrap().is_some());
        assert_eq!(e.list_links().await.unwrap().len(), 1);
        e.insert_record("child", json!({"pid": 7}), None, false, &p).await.unwrap();
        let joined = e.join_list("child", &SrvFilter::default(), 10, 0).await.unwrap();
        assert_eq!(joined.len(), 1);
        let text = serde_json::to_string(&joined[0].payload).unwrap();
        assert!(text.contains("\"name\":\"P\""), "parent did not join: {text}");
        e.clear_link().await.unwrap();

        e.set_rate(&json!({"submit": 2})).await.unwrap();
        let mut limiter = engine::policy::RateLimiter::new();
        let limits = engine::policy::RateLimits::from_json(&json!({"submit": 1}));
        limiter.check("k", "submit", &limits).unwrap();
        assert!(limiter.check("k", "submit", &limits).is_err());
    });
}
