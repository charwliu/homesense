use homesensed::{web_api::Api, web_auth::Tokens, web_receipts::Receipts};
use http_body_util::BodyExt;
use hyper::{Request, StatusCode, body::Bytes};
use std::sync::{Arc, Mutex, RwLock};
fn hash(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn api(root: &std::path::Path) -> Api {
    Api {
        daily_backup: Default::default(),
        hap_bridges: Default::default(),
        hap_status: Default::default(),
        backup_source: None,
        publisher: None,
        origin: "http://localhost:8088".into(),
        tokens: Arc::new(Tokens::default()),
        runtime: RwLock::new(None),
        drafts: None,
        receipts: Mutex::new(Receipts::open(&root.join("web.sqlite")).unwrap()),
    }
}
fn request(path: &str) -> Request<Bytes> {
    Request::builder()
        .uri(path)
        .header("host", "localhost:8088")
        .body(Bytes::new())
        .unwrap()
}
#[tokio::test]
async fn static_scripts_are_no_store_and_identify_the_exact_served_bytes() {
    let root = std::env::temp_dir().join(format!("asset-scope-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let api = api(&root);
    for path in ["/app.js", "/draft-json.js"] {
        let response = api.handle(request(path)).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("cache-control")
                .and_then(|h| h.to_str().ok()),
            Some("no-store"),
            "{path} must not reuse bytes across software upgrades"
        );
        let version = response.headers()["x-homesense-asset-sha256"]
            .to_str()
            .unwrap()
            .to_owned();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(version, hash(&body));
    }
    std::fs::remove_dir_all(root).unwrap();
}
#[tokio::test]
async fn html_requests_current_versioned_scripts_and_exact_version_routes_work() {
    let root = std::env::temp_dir().join(format!("asset-html-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let api = api(&root);
    let response = api.handle(request("/")).await;
    assert_eq!(response.headers()["cache-control"], "no-store");
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let html = String::from_utf8(body.to_vec()).unwrap();
    for (path, source) in [
        ("/app.js", include_bytes!("../src/web/app.js").as_slice()),
        (
            "/draft-json.js",
            include_bytes!("../src/web/draft-json.js").as_slice(),
        ),
    ] {
        let version = hash(source);
        let uri = format!("{path}?v={version}");
        assert!(
            html.contains(&format!("src=\"{uri}\"")),
            "HTML must request the exact compiled asset version"
        );
        let response = api.handle(request(&uri)).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["cache-control"], "no-store");
        assert_eq!(response.headers()["x-homesense-asset-sha256"], version);
        assert_eq!(
            response
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .as_ref(),
            source
        );
    }
    std::fs::remove_dir_all(root).unwrap();
}
#[tokio::test]
async fn version_gate_rejects_stale_or_extra_queries_without_broadening_other_routes() {
    let root = std::env::temp_dir().join(format!("asset-query-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let api = api(&root);
    let version = hash(include_bytes!("../src/web/app.js"));
    assert_eq!(
        api.handle(request(&format!("/app.js?v={}", "0".repeat(64))))
            .await
            .status(),
        StatusCode::CONFLICT
    );
    for path in [
        "/app.js?version=anything",
        "/app.js?v=unknown",
        "/app.js?v=%61",
        "/app.js?",
        &format!("/app.js?v={version}&v={version}"),
        &format!("/app.js?v={version}&x=1"),
        &format!("/draft-json.js?v={version}"),
        &format!("/api/v1/devices?v={version}"),
        &format!("/?v={version}"),
        &format!("/style.css?v={version}"),
    ] {
        let response = api.handle(request(path)).await;
        assert!(
            [StatusCode::BAD_REQUEST, StatusCode::CONFLICT].contains(&response.status()),
            "{path}: {}",
            response.status()
        );
    }
    std::fs::remove_dir_all(root).unwrap();
}
