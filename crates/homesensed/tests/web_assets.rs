use homesensed::{web_api::Api, web_auth::Tokens, web_receipts::Receipts};
use http_body_util::BodyExt;
use hyper::{Request, StatusCode, body::Bytes};
use std::sync::{Arc, Mutex, RwLock};
fn api(root: &std::path::Path) -> Api {
    Api {
        daily_backup: Default::default(),
        hap_placement:Default::default(),hap_bridges: Default::default(),
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
async fn browser_entry_and_all_static_assets_are_retired_even_with_version_queries() {
    let root = std::env::temp_dir().join(format!("native-only-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let api = api(&root);
    for path in ["/", "/app.js", "/draft-json.js", "/style.css"] {
        for suffix in ["", "?v=obsolete", "?v=untrusted&x=1"] {
            for method in ["GET", "POST"] {
                let mut request = request(&format!("{path}{suffix}"));
                *request.method_mut() = method.parse().unwrap();
                let response = api.handle(request).await;
                assert_eq!(response.status(), StatusCode::GONE);
                let body = response.into_body().collect().await.unwrap().to_bytes();
                let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
                assert_eq!(body["error"], "native_app_required");
            }
        }
    }
    std::fs::remove_dir_all(root).unwrap();
}
#[tokio::test]
async fn native_api_and_liveness_still_enforce_existing_authentication_and_origin() {
    let root = std::env::temp_dir().join(format!("native-api-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let api = api(&root);
    assert_eq!(
        api.handle(request("/health/live")).await.status(),
        StatusCode::OK
    );
    for path in [
        "/api/v1/session",
        "/api/v1/devices",
        "/api/v1/rooms",
        "/api/v1/config/versions",
    ] {
        assert_eq!(
            api.handle(request(path)).await.status(),
            StatusCode::UNAUTHORIZED
        );
    }
    let mut evil = request("/");
    evil.headers_mut()
        .insert("origin", "http://evil.invalid".parse().unwrap());
    assert_eq!(api.handle(evil).await.status(), StatusCode::FORBIDDEN);
    std::fs::remove_dir_all(root).unwrap();
}
