//! Asset validators (edge-cache program): every stored asset must expose a
//! stable, content-derived validator via head(), on every adapter. The
//! worker serves it as ETag and answers 304s from it.

use engine::ServerlessEngine;
use futures_executor::block_on;

#[test]
fn head_carries_validator() {
    block_on(async {
        let e = ServerlessEngine::with_defaults();
        e.put_asset("app.js", b"console.log(1)").await.unwrap();
        let m = e.head_asset("app.js").await.unwrap().expect("meta");
        assert!(!m.sha256.is_empty());
        assert!(m.size > 0);
        // Stable: same bytes, same validator.
        e.put_asset("app.js", b"console.log(1)").await.unwrap();
        let m2 = e.head_asset("app.js").await.unwrap().expect("meta");
        assert_eq!(m.sha256, m2.sha256);
        // Rewrite changes it.
        e.put_asset("app.js", b"console.log(2)").await.unwrap();
        let m3 = e.head_asset("app.js").await.unwrap().expect("meta");
        assert_ne!(m.sha256, m3.sha256);
        assert!(e.head_asset("missing.js").await.unwrap().is_none());
    });
}
