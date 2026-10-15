//! Native API/WSS integration against an in-memory simulator, never a gateway.
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest};
use uuid::Uuid;
const SECRET: &str = "synthetic-keypad-native-test-token-0123456789";
async fn http(
    address: std::net::SocketAddr,
    token: &str,
    path: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
    let method = if body.is_some() { "POST" } else { "GET" };
    let body = body.map(|v| v.to_string()).unwrap_or_default();
    stream.write_all(format!("{method} {path} HTTP/1.1\r\nHost: {address}\r\nOrigin: http://{address}\r\nAuthorization: Bearer {token}\r\nIdempotency-Key: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",Uuid::new_v4(),body.len()).as_bytes()).await.unwrap();
    let mut bytes = vec![];
    tokio::time::timeout(Duration::from_secs(3), stream.read_to_end(&mut bytes))
        .await
        .unwrap()
        .unwrap();
    let text = String::from_utf8(bytes).unwrap();
    let (header, body) = text.split_once("\r\n\r\n").unwrap();
    (
        header.split_whitespace().nth(1).unwrap().parse().unwrap(),
        serde_json::from_str(body).unwrap(),
    )
}
#[tokio::test]
async fn parent_snapshot_scoped_led_control_and_wss_preserve_native_auth_and_unknown_actual() {
    let root = std::env::temp_dir().join(format!("keypad-native-{}", Uuid::new_v4()));
    let reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = reservation.local_addr().unwrap();
    drop(reservation);
    let config=br#"{"schema_version":2,"revision":1,"rooms":[{"id":"room","name":"Panel room"}],"gateways":[{"id":"bus","endpoint":"127.0.0.1:50007","modules":[{"id":"module","address":"41","profile":"ts6_86_rs485_v2_6_5_6key","verification":{"status":"simulated"},"led_mode":"manual"}]}],"devices":[],"keypads":[{"id":"panel","name":"Panel","module_id":"module","room":"room","enabled":true,"keys":[{"number":1,"label":"One","led_endpoint":{"id":"old:1","name":"LED one","enabled":true}},{"number":2,"label":"Two","led_endpoint":{"id":"old:2","name":"LED two","enabled":true}}]}]}"#;
    let mut config: Value = serde_json::from_slice(config).unwrap();
    config["keypads"][0]["keys"]=json!((1..=6).map(|n|json!({"number":n,"label":n.to_string(),"led_endpoint":{"id":format!("old:{n}"),"name":format!("LED {n}"),"enabled":true}})).collect::<Vec<_>>());
    let config = serde_json::to_vec(&config).unwrap();
    let directory = root.clone();
    let service = tokio::spawn(async move {
        homesensed::service::serve_web_simulation(
            &config,
            br#"{"schema_version":1,"configuration_revision":1,"variables":[],"rules":[]}"#,
            &directory,
            Duration::from_secs(7),
            homesensed::service::WebSimulation {
                address,
                secret: SECRET.into(),
                administrator: true,
                tls: None,
            },
        )
        .await
    });
    for _ in 0..200 {
        if tokio::net::TcpStream::connect(address).await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    if service.is_finished() {
        panic!("simulation startup failed: {:?}", service.await.unwrap());
    }
    let (status, initial) = http(address, SECRET, "/api/v1/devices", None).await;
    assert_eq!(status, 200, "{initial}");
    assert_eq!(initial["devices"].as_array().unwrap().len(), 1);
    assert_eq!(initial["devices"][0]["id"], "panel");
    assert_eq!(initial["devices"][0]["kind"], "Keypad");
    let (status, issued) = http(
        address,
        SECRET,
        "/api/v1/credentials",
        Some(json!({"role":"Member","devices":["old:1"],"scenes":[]})),
    )
    .await;
    assert_eq!(status, 201, "{issued}");
    let token = issued["secret"].as_str().unwrap();
    let (_, limited) = http(address, token, "/api/v1/devices", None).await;
    let keys = limited["devices"][0]["keys"].as_array().unwrap();
    assert_eq!(keys.len(), 6);
    for (i, key) in keys.iter().enumerate() {
        assert_eq!(key["number"], i + 1);
        if i > 0 {
            assert!(key["endpoint_id"].is_null());
            assert_eq!(key["can_write_led"], false);
        }
    }
    assert_eq!(keys[0]["can_write_led"], true);
    assert!(keys[0]["action"].is_null());
    let (status, rooms) = http(address, token, "/api/v1/rooms", None).await;
    assert_eq!(status, 200);
    assert_eq!(rooms["rooms"][0]["id"], "room");
    assert_eq!(
        initial["devices"][0]["settings"]["backlight"]["can_write"],
        true
    );
    assert!(initial["devices"][0]["settings"]["backlight"]["last_written"].is_null());
    let setting_body = |command: Value| json!({"keypad":"panel","revision":"1","request_id":Uuid::new_v4().to_string(),"command":command});
    let (code, _) = http(
        address,
        token,
        "/api/v1/keypad-settings",
        Some(setting_body(json!({"action":"backlight","mode":"bright"}))),
    )
    .await;
    assert_eq!(code, 403);
    let (code, partial_admin) = http(
        address,
        SECRET,
        "/api/v1/credentials",
        Some(json!({"role":"Administrator","devices":["old:1"],"scenes":[]})),
    )
    .await;
    assert_eq!(code, 201);
    let (code, _) = http(
        address,
        partial_admin["secret"].as_str().unwrap(),
        "/api/v1/keypad-settings",
        Some(setting_body(json!({"action":"sound","enabled":false}))),
    )
    .await;
    assert_eq!(
        code, 403,
        "administrator with one key still cannot control whole panel"
    );
    let (code, _) = http(
        address,
        SECRET,
        "/api/v1/keypad-settings",
        Some(setting_body(
            json!({"action":"work_mode","mode":"self_reset"}),
        )),
    )
    .await;
    assert_eq!(
        code, 403,
        "configuration remains manual; incompatible hardware mode must be refused"
    );
    for command in [
        json!({"action":"backlight","mode":"bright"}),
        json!({"action":"sound","enabled":false}),
        json!({"action":"work_mode","mode":"manual"}),
    ] {
        let (code, body) = http(
            address,
            SECRET,
            "/api/v1/keypad-settings",
            Some(setting_body(command)),
        )
        .await;
        assert_eq!(code, 202, "{body}");
        assert_eq!(body["outcome"], "accepted");
    }
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let (_, state) = http(address, SECRET, "/api/v1/devices", None).await;
            let settings = &state["devices"][0]["settings"];
            if settings["backlight"]["last_written"]["value"] == "bright"
                && settings["sound"]["last_written"]["value"] == false
                && settings["work_mode"]["last_written"]["value"] == "manual"
            {
                for field in ["backlight", "sound", "work_mode"] {
                    assert!(settings[field]["observed"].is_null());
                    assert_eq!(settings[field]["quality"], "Unknown");
                }
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let mut request = format!("ws://{address}/api/v1/events")
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("origin", format!("http://{address}").parse().unwrap());
    request
        .headers_mut()
        .insert("authorization", format!("Bearer {token}").parse().unwrap());
    let (mut socket, _) = tokio_tungstenite::client_async(
        request,
        tokio::net::TcpStream::connect(address).await.unwrap(),
    )
    .await
    .unwrap();
    socket
        .send(Message::Text(r#"{"cursor":null}"#.into()))
        .await
        .unwrap();
    let first = tokio::time::timeout(Duration::from_secs(2), socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let first: Value = serde_json::from_str(first.to_text().unwrap()).unwrap();
    assert_eq!(first["devices"][0]["id"], "panel");
    assert_eq!(first["devices"][0]["keys"].as_array().unwrap().len(), 6);
    for device in ["old:2", "panel"] {
        let (status,_) = http(address,token,"/api/v1/device-commands",Some(json!({"device":device,"request_id":Uuid::new_v4().to_string(),"command":{"action":"panel_led","on":true}}))).await;
        assert_eq!(
            status, 403,
            "single-key permission must not expand to {device}"
        );
    }
    let (status,accepted)=http(address,token,"/api/v1/device-commands",Some(json!({"device":"old:1","request_id":Uuid::new_v4().to_string(),"command":{"action":"panel_led","on":true}}))).await;
    assert_eq!(status, 202, "{accepted}");
    assert_eq!(accepted["outcome"], "accepted");
    let update = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let Some(Ok(Message::Text(text))) = socket.next().await {
                let value: Value = serde_json::from_str(&text).unwrap();
                if value["devices"][0]["keys"][0]["led"]["last_written"]["color"] == "red" {
                    break value;
                }
            }
        }
    })
    .await
    .unwrap();
    let panel = &update["devices"][0];
    assert_eq!(panel["keys"].as_array().unwrap().len(), 6);
    assert!(panel["keys"][0]["led"]["observed"].is_null());
    assert_eq!(panel["keys"][0]["led"]["quality"], "Unknown");
    assert_eq!(panel["keys"][0]["led"]["last_written"]["status"], "written");
    assert_eq!(panel["settings"]["work_mode"]["can_write"], false);
    socket.close(None).await.unwrap();
    service.await.unwrap().unwrap();
    let sql = rusqlite::Connection::open(root.join("automation-simulation.sqlite")).unwrap();
    let modes: i64 = sql
        .query_row(
            "SELECT count(*) FROM command_journal WHERE intent=x'0109c802' AND status='written'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        modes, 1,
        "only the explicitly requested C8 manual command is written"
    );
    let rejected: i64 = sql
        .query_row(
            "SELECT count(*) FROM command_journal WHERE intent=x'0109c801'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(rejected, 0);
    drop(sql);
    std::fs::remove_dir_all(root).unwrap();
}
