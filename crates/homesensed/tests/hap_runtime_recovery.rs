//! Paired scene HAP bridge across a synthetic, reconnecting AD8 gateway.
#[path = "../../homesense-hap/tests/support/mod.rs"]
mod support;
use hap::storage::Storage;
use homesense_hap::scene_graph::{SceneActivityGraph, SceneGraph, SceneSpec};
use serde_json::{Value, json};
use std::{
    net::{SocketAddr, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU8, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use uuid::Uuid;

const TOKEN: &str = "abcdefghijklmnopqrstuvwxyz0123456789";

fn free_address() -> SocketAddr {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
}
fn wait(mut ready: impl FnMut() -> bool, label: &str) {
    let deadline = Instant::now() + Duration::from_secs(8);
    while Instant::now() < deadline {
        if ready() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("timed out waiting for {label}");
}
fn web_ready(address: SocketAddr) -> Option<bool> {
    use std::io::{Read, Write};
    let mut socket = TcpStream::connect_timeout(&address, Duration::from_millis(300)).ok()?;
    socket.set_read_timeout(Some(Duration::from_secs(1))).ok()?;
    write!(socket, "GET /health/ready HTTP/1.1\r\nHost: {address}\r\nOrigin: http://{address}\r\nAuthorization: Bearer {TOKEN}\r\nConnection: close\r\n\r\n").ok()?;
    let mut bytes = Vec::new();
    socket.read_to_end(&mut bytes).ok()?;
    let text = String::from_utf8(bytes).ok()?;
    let (headers, body) = text.split_once("\r\n\r\n")?;
    let ready = serde_json::from_str::<Value>(body).ok()?["ready"].as_bool()?;
    let expected = if ready { "HTTP/1.1 200" } else { "HTTP/1.1 503" };
    headers.starts_with(expected).then_some(ready)
}
fn hap_graph(
    address: SocketAddr,
    controller: Uuid,
    signing: &ed25519_dalek::SigningKey,
    config: &hap::Config,
    scene_aid: u64,
    scene_iid: u64,
    run_scene: bool,
) {
    let mut socket = TcpStream::connect(address).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let mut session = support::verify(
        &mut socket,
        controller,
        signing,
        &config.device_id.to_string(),
        &config.device_ed25519_keypair.verifying_key(),
    );
    let (header, body) = session.request(
        &mut socket,
        b"GET /accessories HTTP/1.1\r\nHost: localhost\r\n\r\n",
    );
    assert!(header.starts_with("HTTP/1.1 200"), "{header}");
    let graph: Value = serde_json::from_slice(&body).unwrap();
    let accessories = graph["accessories"].as_array().unwrap();
    assert_eq!(accessories.len(), 4); // bridge, light, scene action, scene activity
    assert!(accessories.iter().any(|a| a["aid"] == scene_aid));
    assert!(
        accessories
            .iter()
            .flat_map(|a| a["services"].as_array().unwrap())
            .any(|s| s["type"] == "43")
    );
    if run_scene {
        let body =
            json!({"characteristics":[{"aid":scene_aid,"iid":scene_iid,"value":true}]}).to_string();
        let request = format!(
            "PUT /characteristics HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/hap+json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        let (header, response) = session.request(&mut socket, request.as_bytes());
        assert!(header.starts_with("HTTP/1.1 204"), "{header} {response:?}");
    }
}

#[tokio::test]
async fn paired_scene_and_light_survive_gateway_recovery_without_replay() {
    let root = std::env::temp_dir().join(format!("homesense-scene-recovery-{}", Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let bus = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let web = free_address();
    let hap_address = free_address();
    let devices = serde_json::to_vec(&json!({"schema_version":2,"revision":1,
        "gateways":[{"id":"bus","endpoint":bus.local_addr().unwrap().to_string(),"modules":[{"id":"dimmer","address":"11","profile":"ad8_din_rs485_v2_2_1","verification":{"status":"verified","verified_at_unix_ms":1,"evidence":"synthetic loopback"}}]}],
        "devices":[{"id":"light","name":"Synthetic light","module_id":"dimmer","channel":1,"enabled":true}]})).unwrap();
    let rules = serde_json::to_vec(&json!({"schema_version":1,"configuration_revision":1,"variables":[],"rules":[],
        "timed_scenes":[{"id":"demo","enabled":true,"review_evidence":"synthetic","max_lateness_ms":1000,"steps":[{"source":"synthetic","offset_ms":0,"device":"light","command":{"kind":"level","value":40,"ramp":false}}]}],
        "scene_tracking":[{"scene":"demo","enabled":true,"aggregation":"all","unknown_policy":"require_all_known","pending_policy":"block_scene","control_policy":"separate_action_and_read_only_activity","activity_name":"Demo activity","max_age_ms":1000,"review_evidence":"bounded synthetic freshness consumer","members":[{"id":"light","device":"light","predicate":{"kind":"power","on":true},"review_evidence":"synthetic"}]}]})).unwrap();
    let mut identity = homesense_hap::storage::IdentityStore::open(root.join("identity"))
        .await
        .unwrap();
    let initial = hap::Config {
        host: hap_address.ip(),
        port: hap_address.port(),
        name: "Paired scene recovery".into(),
        ..Default::default()
    };
    identity.save_config(&initial).await.unwrap();
    identity.initialize_aid_cache().await.unwrap();
    let action = SceneGraph::build(
        &identity,
        vec![SceneSpec {
            id: "demo".into(),
            name: "demo".into(),
        }],
    )
    .await
    .unwrap();
    let scene_aid = action.bindings[0].aid;
    let scene_iid = action.bindings[0].on_iid;
    SceneActivityGraph::build(
        &identity,
        vec![SceneSpec {
            id: "demo".into(),
            name: "Demo activity".into(),
        }],
    )
    .await
    .unwrap();
    let controller = Uuid::new_v4();
    let signing = ed25519_dalek::SigningKey::from_bytes(&[63; 32]);
    identity
        .save_pairing(&hap::pairing::Pairing::new(
            controller,
            hap::pairing::Permissions::Admin,
            signing.verifying_key().to_bytes(),
        ))
        .await
        .unwrap();
    let phase = Arc::new(AtomicU8::new(0));
    let writes = Arc::new(Mutex::new(Vec::<[u8; 5]>::new()));
    let bus_phase = phase.clone();
    let bus_writes = writes.clone();
    let gateway = tokio::spawn(async move {
        loop {
            let (mut socket, _) = bus.accept().await.unwrap();
            let phase = bus_phase.clone();
            let writes = bus_writes.clone();
            tokio::spawn(async move {
                let mut level = 30;
                loop {
                    let mut frame = [0; 5];
                    if socket.read_exact(&mut frame).await.is_err() {
                        break;
                    }
                    assert_eq!((frame[0], frame[1], frame[4]), (0xae, 0x11, 0xee));
                    if frame[2] == 0xa0 {
                        if phase.load(Ordering::Acquire) == 1 {
                            continue;
                        }
                        let reply: Vec<_> = (1..=8)
                            .flat_map(|channel| [0xae, 0x11, 0xd0 + channel, level, 0xee])
                            .collect();
                        if socket.write_all(&reply).await.is_err() {
                            break;
                        }
                    } else {
                        writes.lock().unwrap().push(frame);
                        level = frame[3];
                        if socket
                            .write_all(&[0xae, 0x11, 0xd1, level, 0xee])
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                }
            });
        }
    });
    let directory = root.clone();
    let service = tokio::spawn(async move {
        homesensed::automation_tcp::serve_combined(
            &devices,
            &rules,
            &directory,
            Duration::from_secs(17),
            true,
            2,
            Duration::from_millis(20),
            homesensed::service::WebSettings {
                address: web,
                secret: TOKEN.into(),
                administrator: true,
                tls: None,
            },
            homesensed::hap_endpoint::Settings {
                address: hap_address,
                scenes: vec!["demo".into()],
                motors: vec![],
            },
        )
        .await
    });
    tokio::task::spawn_blocking(move || wait(|| web_ready(web) == Some(true), "initial readiness"))
        .await
        .unwrap();
    let saved = identity.load_config().await.unwrap();
    let signing_for_client = signing.clone();
    tokio::task::spawn_blocking(move || {
        hap_graph(
            hap_address,
            controller,
            &signing_for_client,
            &saved,
            scene_aid,
            scene_iid,
            true,
        )
    })
    .await
    .unwrap();
    tokio::task::spawn_blocking({
        let writes = writes.clone();
        move || wait(|| writes.lock().unwrap().len() == 1, "scene command")
    })
    .await
    .unwrap();
    assert_eq!(writes.lock().unwrap()[0], [0xae, 0x11, 0x01, 40, 0xee]);
    phase.store(1, Ordering::Release);
    tokio::task::spawn_blocking(move || wait(|| web_ready(web) == Some(false), "gateway outage"))
        .await
        .unwrap();
    phase.store(2, Ordering::Release);
    tokio::task::spawn_blocking(move || wait(|| web_ready(web) == Some(true), "gateway recovery"))
        .await
        .unwrap();
    let saved = identity.load_config().await.unwrap();
    let signing_for_client = signing.clone();
    tokio::task::spawn_blocking(move || {
        hap_graph(
            hap_address,
            controller,
            &signing_for_client,
            &saved,
            scene_aid,
            scene_iid,
            false,
        )
    })
    .await
    .unwrap();
    assert_eq!(
        writes.lock().unwrap().len(),
        1,
        "scene command replayed during recovery"
    );
    service.await.unwrap().unwrap();
    gateway.abort();
    assert_eq!(identity.count_pairings().await.unwrap(), 1);
    assert_eq!(
        identity.load_config().await.unwrap().device_id,
        initial.device_id
    );
    if let Some(directory) = std::env::var_os("HAP_RECOVERY_ARTIFACT_ROOT") {
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(std::path::PathBuf::from(directory).join("paired-hap-recovery.json"),
            json!({"passed":true,"publication":"scene_only","scene_aid":scene_aid,"scene_iid":scene_iid,"command_count":1,"pairing_preserved":true}).to_string()).unwrap();
    }
    drop(identity);
    std::fs::remove_dir_all(root).unwrap();
}
