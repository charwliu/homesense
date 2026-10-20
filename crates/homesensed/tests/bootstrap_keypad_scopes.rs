//! First-use credentials and retained scopes, using only an in-memory bus.
use homesense_core::ExecutionContext;
use serde_json::{Value, json};
use std::{net::SocketAddr, path::Path, time::Duration};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use uuid::Uuid;

const SECRET: &str = "synthetic-bootstrap-keypad-scope-0123456789";

fn inputs() -> (Vec<u8>, Vec<u8>) {
    let mut config: Value = serde_json::from_slice(include_bytes!(
        "../../../config/test/native-app-keypad/devices.json"
    ))
    .unwrap();
    config["keypads"][0]["keys"][1]["action"] = json!({"kind":"scene_execute","scene":"execute-hidden","review_evidence":"synthetic hidden execute parent"});
    config["keypads"][0]["keys"][2]["led_endpoint"]["enabled"] = false.into();
    config["keypads"][0]["keys"][2]["action"] = json!({"kind":"scene_execute","scene":"disabled-key-hidden","review_evidence":"synthetic disabled key"});
    let mut disabled = config["keypads"][0].clone();
    disabled["id"] = "disabled-panel".into();
    disabled["module_id"] = "disabled-panel-module".into();
    disabled["enabled"] = false.into();
    for key in disabled["keys"].as_array_mut().unwrap() {
        key["led_endpoint"]["id"] = format!("disabled-{}", key["number"]).into();
        key["action"] = Value::Null;
    }
    disabled["keys"][0]["action"] = json!({"kind":"scene_execute","scene":"disabled-panel-hidden","review_evidence":"synthetic disabled panel"});
    config["keypads"].as_array_mut().unwrap().push(disabled);
    config["gateways"][0]["modules"].as_array_mut().unwrap().push(json!({"id":"disabled-panel-module","address":"42","profile":"ts6_86_rs485_v2_6_5_6key","verification":{"status":"simulated"},"led_mode":"manual"}));
    let mut rules: Value = serde_json::from_slice(include_bytes!(
        "../../../config/test/native-app-keypad/automation.json"
    ))
    .unwrap();
    rules["scene_switch_groups"][0]["reconciliation"]["enabled"] = false.into();
    let hidden = [
        "reading",
        "meeting",
        "execute-hidden",
        "nested-hidden",
        "disabled-key-hidden",
        "disabled-panel-hidden",
        "unmapped-hidden",
    ];
    rules["scene_metadata"] = Value::Array(
        hidden
            .iter()
            .map(|id| json!({"id":id,"name":id,"catalog_hidden":true}))
            .collect(),
    );
    let mut immediate: Vec<_> = hidden[4..].iter().map(|id| json!({"id":id,"enabled":true,"review_evidence":"synthetic hidden scope boundary","activate":[{"source":"synthetic","action":{"kind":"command","device":"fixture-light","command":{"kind":"level","value":50,"ramp":false}}}]})).collect();
    immediate.push(json!({"id":"nested-hidden","enabled":true,"review_evidence":"synthetic nested body","activate":[{"source":"synthetic","action":{"kind":"command","device":"fixture-light","command":{"kind":"level","value":0,"ramp":false}}},{"source":"synthetic","action":{"kind":"command","device":"fixture-downlight","command":{"kind":"level","value":100,"ramp":false}}}]}));
    immediate.push(json!({"id":"execute-hidden","enabled":true,"review_evidence":"synthetic explicit wrapper","activate":[{"source":"synthetic","action":{"kind":"scene","id":"nested-hidden","activate":true}}]}));
    rules["immediate_scenes"] = Value::Array(immediate);
    (
        serde_json::to_vec(&config).unwrap(),
        serde_json::to_vec(&rules).unwrap(),
    )
}

async fn http(address: SocketAddr, path: &str, body: Option<Value>) -> (u16, Value) {
    let mut socket = tokio::net::TcpStream::connect(address).await.unwrap();
    let method = if body.is_some() { "POST" } else { "GET" };
    let body = body.map_or(String::new(), |b| b.to_string());
    socket.write_all(format!("{method} {path} HTTP/1.1\r\nHost: {address}\r\nOrigin: http://{address}\r\nAuthorization: Bearer {SECRET}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
    let mut response = Vec::new();
    tokio::time::timeout(Duration::from_secs(3), socket.read_to_end(&mut response))
        .await
        .unwrap()
        .unwrap();
    let text = String::from_utf8(response).unwrap();
    let (header, body) = text.split_once("\r\n\r\n").unwrap();
    (
        header.split_whitespace().nth(1).unwrap().parse().unwrap(),
        serde_json::from_str(body).unwrap(),
    )
}

async fn start(
    root: &Path,
    administrator: bool,
    seconds: u64,
) -> (SocketAddr, tokio::task::JoinHandle<Result<(), String>>) {
    let (config, rules) = inputs();
    let parsed = homesense_config::Configuration::parse(&config, ExecutionContext::Simulation)
        .expect("valid synthetic devices");
    homesense_config::rule_document::Document::parse(&rules, &parsed)
        .expect("valid synthetic automation");
    if !root.join("automation-simulation.sqlite").exists() {
        std::fs::create_dir(root).unwrap();
        let mut store =
            homesense_storage::Store::open(root.join("automation-simulation.sqlite")).unwrap();
        store
            .publish_automation(None, &config, &rules, ExecutionContext::Simulation, &[])
            .unwrap();
        store
            .accept_queue_batch_with_variables(
                "seed",
                &[],
                &[],
                0,
                None,
                &[homesense_storage::variables::Update {
                    revision: 1,
                    name: "selection".into(),
                    expected_version: 0,
                    value: Some(homesense_core::rule::Value::Integer(0)),
                }],
            )
            .unwrap();
    }
    let address = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let directory = root.to_owned();
    let task = tokio::spawn(async move {
        homesensed::service::serve_web_simulation(
            &config,
            &rules,
            &directory,
            Duration::from_secs(seconds),
            homesensed::service::WebSimulation {
                address,
                secret: SECRET.into(),
                administrator,
                tls: None,
            },
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            assert!(!task.is_finished(), "bootstrap service exited");
            if tokio::net::TcpStream::connect(address).await.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    (address, task)
}

async fn expect_level(address: SocketAddr, id: &str, level: u64) {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let (status, devices) = http(address, "/api/v1/devices", None).await;
            assert_eq!(status, 200, "{devices}");
            if devices["devices"].as_array().unwrap().iter().any(|device| {
                device["id"] == id
                    && device["observed"]["level"] == level
                    && device["observed"]["on"] == (level > 0)
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("accepted hidden keypad action must reach its physical target");
}

async fn first_use(administrator: bool) {
    let root = std::env::temp_dir().join(format!("bootstrap-keypad-{}", Uuid::new_v4()));
    let (address, task) = start(&root, administrator, 4).await;
    let sql = rusqlite::Connection::open(root.join("web-credentials.sqlite")).unwrap();
    let grant: Value = serde_json::from_str(
        &sql.query_row("SELECT grant_json FROM web_credential", [], |r| {
            r.get::<_, String>(0)
        })
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        grant["role"],
        if administrator {
            "Administrator"
        } else {
            "Member"
        }
    );
    assert_eq!(
        grant["scenes"],
        json!(["execute-hidden", "off", "reading"]),
        "active hidden key parents must be usable; disabled, unbound and nested helpers must not gain independent scope"
    );
    let (status, scenes) = http(address, "/api/v1/scenes", None).await;
    assert_eq!(status, 200);
    let scene_ids: Vec<_> = scenes["scenes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        scene_ids,
        ["off"],
        "scope must not create independent scene cards"
    );
    let (_, devices) = http(address, "/api/v1/devices", None).await;
    let panel = devices["devices"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["id"] == "panel")
        .unwrap();
    assert_eq!(panel["keys"][0]["action"]["kind"], "scene_toggle");
    assert_eq!(panel["keys"][0]["activation"]["available"], true, "{panel}");
    assert_eq!(panel["keys"][1]["action"]["kind"], "scene_execute");
    assert_eq!(panel["keys"][1]["activation"]["available"], true, "{panel}");
    assert_eq!(panel["keys"][2]["configured"], false);
    assert!(panel["keys"][2]["action"].is_null());
    assert!(panel["keys"][2]["activation"].is_null());
    if !administrator {
        assert_eq!(http(address, "/api/v1/credentials", None).await.0, 403);
    }
    let (status, receipt) = http(address, "/api/v1/keypad-actions", Some(json!({"revision":"1","keypad":"panel","key":1,"request_id":"hidden-toggle","gesture":"activate"}))).await;
    assert_eq!(status, 202, "{receipt}");
    expect_level(address, "fixture-light", 100).await;
    let (status, receipt) = http(address, "/api/v1/keypad-actions", Some(json!({"revision":"1","keypad":"panel","key":2,"request_id":"hidden-execute","gesture":"activate"}))).await;
    assert_eq!(status, 202, "{receipt}");
    expect_level(address, "fixture-downlight", 100).await;
    expect_level(address, "fixture-light", 0).await;
    task.await.unwrap().unwrap();
    drop(sql);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn new_administrator_can_use_hidden_key_parents_without_exposing_scene_cards() {
    first_use(true).await;
}
#[tokio::test]
async fn new_member_can_use_hidden_key_parents_without_gaining_administration() {
    first_use(false).await;
}

#[tokio::test]
async fn existing_narrow_credential_is_unchanged_after_configuration_bootstrap() {
    let root = std::env::temp_dir().join(format!("bootstrap-retained-{}", Uuid::new_v4()));
    let (_, task) = start(&root, true, 1).await;
    task.await.unwrap().unwrap();
    let sql = rusqlite::Connection::open(root.join("web-credentials.sqlite")).unwrap();
    let mut grant: Value = serde_json::from_str(
        &sql.query_row("SELECT grant_json FROM web_credential", [], |r| {
            r.get::<_, String>(0)
        })
        .unwrap(),
    )
    .unwrap();
    grant["scenes"] = json!(["off"]);
    grant["devices"] = json!(["legacy-key-1"]);
    sql.execute(
        "UPDATE web_credential SET grant_json=?1",
        [grant.to_string()],
    )
    .unwrap();
    let row = || {
        sql.query_row(
            "SELECT id,hex(digest),grant_json FROM web_credential",
            [],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            },
        )
        .unwrap()
    };
    let before = row();
    let (address, task) = start(&root, true, 2).await;
    assert_eq!(
        row(),
        before,
        "restart must not expand an initialized scope"
    );
    assert_eq!(http(address, "/api/v1/keypad-actions", Some(json!({"revision":"1","keypad":"panel","key":1,"request_id":"narrow","gesture":"activate"}))).await.0, 403);
    task.await.unwrap().unwrap();
    assert_eq!(row(), before);
    drop(sql);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn revoked_bootstrap_credential_is_not_recreated() {
    let root = std::env::temp_dir().join(format!("bootstrap-revoked-{}", Uuid::new_v4()));
    let (_, task) = start(&root, true, 1).await;
    task.await.unwrap().unwrap();
    let sql = rusqlite::Connection::open(root.join("web-credentials.sqlite")).unwrap();
    sql.execute("DELETE FROM web_credential", []).unwrap();
    let (address, task) = start(&root, true, 2).await;
    assert_eq!(http(address, "/api/v1/session", None).await.0, 401);
    assert_eq!(
        sql.query_row("SELECT count(*) FROM web_credential", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    task.await.unwrap().unwrap();
    drop(sql);
    std::fs::remove_dir_all(root).unwrap();
}
