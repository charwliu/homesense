//! Policy file reaches both actual simulation service modes; localhost only.
use homesensed::service::{UnifiedSimulation, WebSettings};
use serde_json::{Value, json};
use std::{path::PathBuf, time::Duration};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use uuid::Uuid;
fn directory() -> PathBuf {
    let p = std::env::temp_dir().join(format!("receipt-service-{}", Uuid::new_v4()));
    std::fs::create_dir(&p).unwrap();
    p
}
fn port() -> std::net::SocketAddr {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap()
}
fn rules() -> Vec<u8> {
    serde_json::to_vec(&json!({"schema_version":1,"configuration_revision":1,"variables":[],"rules":[{"id":"startup","enabled":true,"review_evidence":"synthetic startup release","source":"startup","trigger":{"kind":"startup_synchronized"},"actions":[{"source":"output","action":{"kind":"command","device":"simulation:light","command":{"kind":"level","value":60,"ramp":false}}}]}],"timed_scenes":[{"id":"later","enabled":true,"review_evidence":"synthetic published action","max_lateness_ms":1000,"steps":[{"source":"member","offset_ms":60000,"device":"simulation:light","command":{"kind":"level","value":30,"ramp":false}}]}]})).unwrap()
}
#[tokio::test]
async fn invalid_policy_stops_both_service_modes_before_startup_admission_or_any_state_file() {
    for combined in [false, true] {
        let p = directory();
        std::fs::write(
            p.join("web-receipt-policy.json"),
            r#"{"max_records":2,"expiry":{"kind":"delete_and_replay"}}"#,
        )
        .unwrap();
        let address = port();
        let web = WebSettings {
            address,
            secret: "synthetic-local-credential-abcdefghijklmnopqrstuvwxyz".into(),
            administrator: true,
            tls: None,
        };
        let result = if combined {
            homesensed::service::serve_combined_simulation(
                include_bytes!("../../../config/simulation.json"),
                &rules(),
                &p,
                Duration::from_millis(100),
                UnifiedSimulation {
                    address: port(),
                    scenes: vec!["later".into()],
                },
                web,
            )
            .await
        } else {
            homesensed::service::serve_web_simulation(
                include_bytes!("../../../config/simulation.json"),
                &rules(),
                &p,
                Duration::from_millis(100),
                web,
            )
            .await
        };
        assert!(result.unwrap_err().contains("unknown variant"));
        assert_eq!(
            std::fs::read_dir(&p).unwrap().count(),
            1,
            "invalid policy must precede startup state and action release"
        );
        std::fs::remove_dir_all(p).unwrap();
    }
}
async fn http(
    address: std::net::SocketAddr,
    secret: &str,
    path: &str,
    body: Option<Value>,
) -> Result<(u16, Value), String> {
    let mut stream = tokio::net::TcpStream::connect(address)
        .await
        .map_err(|e| e.to_string())?;
    let payload = body.as_ref().map(|b| b.to_string()).unwrap_or_default();
    let method = if body.is_some() { "POST" } else { "GET" };
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {address}\r\nOrigin: http://{address}\r\nAuthorization: Bearer {secret}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
        payload.len()
    );
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    let mut bytes = vec![];
    stream
        .read_to_end(&mut bytes)
        .await
        .map_err(|e| e.to_string())?;
    let text = String::from_utf8(bytes).map_err(|e| e.to_string())?;
    let (header, body) = text.split_once("\r\n\r\n").ok_or("missing HTTP body")?;
    let status = header
        .split_whitespace()
        .nth(1)
        .ok_or("missing HTTP status")?
        .parse()
        .map_err(|_| "bad HTTP status")?;
    let body = serde_json::from_str(body).map_err(|e| e.to_string())?;
    Ok((status, body))
}
#[tokio::test]
async fn policy_file_limits_actual_http_ledger_in_both_service_modes() {
    for combined in [false, true] {
        let p = directory();
        std::fs::write(
            p.join("web-receipt-policy.json"),
            r#"{"max_records":1,"expiry":{"kind":"mark_expired_preserve_fence","window_ms":1}}"#,
        )
        .unwrap();
        let address = port();
        let secret = "synthetic-local-credential-abcdefghijklmnopqrstuvwxyz";
        let web = WebSettings {
            address,
            secret: secret.into(),
            administrator: true,
            tls: None,
        };
        let source = rules();
        let mut parsed: Value = serde_json::from_slice(&source).unwrap();
        parsed["rules"] = json!([]);
        let source = serde_json::to_vec(&parsed).unwrap();
        let hp = port();
        let owned = p.clone();
        let task = tokio::spawn(async move {
            if combined {
                homesensed::service::serve_combined_simulation(
                    include_bytes!("../../../config/simulation.json"),
                    &source,
                    &owned,
                    Duration::from_millis(1500),
                    UnifiedSimulation {
                        address: hp,
                        scenes: vec!["later".into()],
                    },
                    web,
                )
                .await
            } else {
                homesensed::service::serve_web_simulation(
                    include_bytes!("../../../config/simulation.json"),
                    &source,
                    &owned,
                    Duration::from_millis(1500),
                    web,
                )
                .await
            }
        });
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if matches!(
                    http(address, secret, "/health/ready", None).await,
                    Ok((200, _))
                ) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        let diagnostics = http(address, secret, "/api/v1/diagnostics", None)
            .await
            .unwrap();
        assert_eq!(diagnostics.0, 200);
        assert_eq!(diagnostics.1["web_receipt_policy"]["max_records"], 1);
        let make = |key: &str| json!({"device":"simulation:light","revision":"1","request_id":key,"command":{"action":"brightness","level":40}});
        let first = http(
            address,
            secret,
            "/api/v1/device-commands",
            Some(make("one")),
        )
        .await
        .unwrap();
        assert_eq!(first.0, 202);
        tokio::time::sleep(Duration::from_millis(3)).await;
        let same = http(
            address,
            secret,
            "/api/v1/device-commands",
            Some(make("one")),
        )
        .await
        .unwrap();
        assert_eq!(same.0, 200);
        assert_eq!(same.1, first.1);
        let fresh = http(
            address,
            secret,
            "/api/v1/device-commands",
            Some(make("two")),
        )
        .await
        .unwrap();
        assert_eq!(fresh.0, 503);
        task.await.unwrap().unwrap();
        let sql = rusqlite::Connection::open(p.join("web-receipts.sqlite")).unwrap();
        assert_eq!(
            sql.query_row("SELECT (SELECT count(*) FROM web_receipt_legacy)+(SELECT count(*) FROM web_receipt_scoped)", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert!(
            sql.query_row("SELECT expired_ms FROM web_receipt_scoped", [], |r| r
                .get::<_, Option<i64>>(0))
                .unwrap()
                .is_some()
        );
        drop(sql);
        std::fs::remove_dir_all(p).unwrap();
    }
}
