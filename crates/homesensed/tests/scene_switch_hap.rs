//! Stateful switches over the real unified service and encrypted HAP records.
//! Synthetic source only; no field bindings or private Control4 data.
#[path = "../../homesense-hap/tests/support/mod.rs"]
mod support;
use hap::{
    pairing::{Pairing, Permissions},
    storage::Storage,
};
use homesense_hap::{
    graph::{LightGraph, LightSpec},
    scene_graph::{SceneBinding, SceneGraph, SceneSpec},
    storage::IdentityStore,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    net::{TcpListener, TcpStream},
    time::{Duration, Instant},
};
use uuid::Uuid;

fn rules() -> Vec<u8> {
    let scenes = [("reading",100),("daily",60),("eco",17),("off",0)].map(|(id,level)|json!({"id":id,"enabled":true,"review_evidence":"synthetic reviewed scene","max_lateness_ms":1000,"steps":[{"source":"synthetic","offset_ms":60000,"device":"simulation:light","command":{"kind":"level","value":level,"ramp":false}}]}));
    serde_json::to_vec(&json!({"schema_version":1,"configuration_revision":1,"variables":[{"id":"selection","kind":"integer","lifetime":"persistent"}],"rules":[],"timed_scenes":scenes,
        "scene_switch_groups":[{"id":"room","variable":"selection","review_evidence":"synthetic toggle and exclusion","members":[{"scene":"reading","value":1,"off_scene":"off"},{"scene":"daily","value":2,"off_scene":"off"},{"scene":"eco","value":3,"off_scene":"off"}],"reset_scenes":["off"]}]})).unwrap()
}
struct Client {
    stream: TcpStream,
    session: support::Session,
    bindings: BTreeMap<String, SceneBinding>,
    events: Vec<Value>,
}
impl Client {
    fn request(&mut self, method: &str, path: &str, body: Option<Value>) -> (String, Value) {
        let body = body.map(|v| v.to_string()).unwrap_or_default();
        let request = format!(
            "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/hap+json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        let mut response = self.session.request(&mut self.stream, request.as_bytes());
        for _ in 0..32 {
            if !response.0.starts_with("EVENT/") {
                let value = if response.1.is_empty() {
                    Value::Null
                } else {
                    serde_json::from_slice(&response.1).unwrap()
                };
                return (response.0, value);
            }
            self.events
                .push(serde_json::from_slice(&response.1).unwrap());
            response = self.session.request(&mut self.stream, b"");
        }
        panic!("unbounded event stream");
    }
    fn writes(&mut self, items: &[(&str, Option<bool>, Option<bool>)], success: bool) {
        let items = items
            .iter()
            .map(|(id, value, event)| {
                let b = &self.bindings[*id];
                let mut v = json!({"aid":b.aid,"iid":b.on_iid});
                if let Some(on) = value {
                    v["value"] = json!(on);
                }
                if let Some(on) = event {
                    v["ev"] = json!(on);
                }
                v
            })
            .collect::<Vec<_>>();
        let (h, v) = self.request(
            "PUT",
            "/characteristics",
            Some(json!({"characteristics":items})),
        );
        if success {
            assert!(h.starts_with("HTTP/1.1 204"), "{h} {v}");
        } else {
            assert!(h.starts_with("HTTP/1.1 207"), "{h} {v}");
            assert!(
                v["characteristics"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|c| c["status"].as_i64().unwrap() != 0)
            );
        }
    }
    fn states(&mut self) -> BTreeMap<String, Option<bool>> {
        let ids = self
            .bindings
            .values()
            .map(|b| format!("{}.{}", b.aid, b.on_iid))
            .collect::<Vec<_>>()
            .join(",");
        let (h, v) = self.request("GET", &format!("/characteristics?id={ids}"), None);
        assert!(
            h.starts_with("HTTP/1.1 200") || h.starts_with("HTTP/1.1 207"),
            "{h} {v}"
        );
        self.bindings
            .iter()
            .map(|(id, b)| {
                let c = v["characteristics"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|c| c["aid"] == b.aid && c["iid"] == b.on_iid)
                    .unwrap();
                let value = c["value"].as_bool();
                if value.is_none() {
                    assert_eq!(c["status"], -70402, "{c}");
                }
                (id.clone(), value)
            })
            .collect()
    }
    fn selected(&mut self, selected: Option<&str>) {
        let states = self.states();
        for id in ["reading", "daily", "eco"] {
            assert_eq!(states[id], Some(selected == Some(id)), "{states:?}");
        }
        assert_eq!(states["off"], Some(false));
    }
}
fn receipts(path: &std::path::Path) -> Vec<String> {
    let db = rusqlite::Connection::open(path).unwrap();
    db.prepare("SELECT definition FROM scene_run ORDER BY rowid")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

#[tokio::test]
async fn encrypted_scene_switches_keep_selection_exclude_peers_and_restart_without_replay() {
    let root = std::env::temp_dir().join(format!("stateful-hap-{}", Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let reservation = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = reservation.local_addr().unwrap();
    let mut identity = IdentityStore::open(root.join("identity")).await.unwrap();
    let settings = hap::Config {
        host: address.ip(),
        port: address.port(),
        ..Default::default()
    };
    let accessory_id = settings.device_id.to_string();
    let accessory_key = settings.device_ed25519_keypair.verifying_key();
    identity.save_config(&settings).await.unwrap();
    let lights = LightGraph::build(
        &identity,
        "HomeSense",
        vec![LightSpec {
            device: homesense_core::DeviceId::new("simulation:light").unwrap(),
            name: "Light".into(),
            dimmable: true,
            default_on_level: Some(homesense_core::Percent::new(100).unwrap()),
        }],
    )
    .await
    .unwrap();
    let scenes = SceneGraph::build(
        &identity,
        ["reading", "daily", "eco", "off"]
            .map(|id| SceneSpec {
                id: id.into(),
                name: id.into(),
            })
            .into(),
    )
    .await
    .unwrap();
    let bindings = scenes
        .bindings
        .into_iter()
        .map(|b| (b.scene.clone(), b))
        .collect::<BTreeMap<_, _>>();
    struct Reject;
    impl hap::server::WriteBatchHandler for Reject {
        fn write(&self, _: Uuid, _: &[u8]) -> Result<(), hap::server::WriteStatus> {
            Err(hap::server::WriteStatus::ServiceCommunicationFailure)
        }
    }
    let seed = hap::server::IpServer::new_loopback(settings, identity.clone())
        .await
        .unwrap();
    let mut handles: Vec<hap::server::AccessoryHandle> = vec![std::sync::Arc::new(
        hap::futures::lock::Mutex::new(Box::new(lights.bridge)),
    )];
    for (_, light) in lights.lights {
        handles.push(std::sync::Arc::new(hap::futures::lock::Mutex::new(
            Box::new(light),
        )));
    }
    for (_, scene) in scenes.scenes {
        handles.push(std::sync::Arc::new(hap::futures::lock::Mutex::new(
            Box::new(scene),
        )));
    }
    seed.install_startup_graph(handles, std::sync::Arc::new(Reject))
        .await
        .unwrap();
    drop(seed);
    let controller = Uuid::new_v4();
    let signing = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
    identity
        .save_pairing(&Pairing::new(
            controller,
            Permissions::Admin,
            signing.verifying_key().to_bytes(),
        ))
        .await
        .unwrap();
    let mut configuration_number = None;
    drop(reservation);
    let path = root.join("automation-simulation.sqlite");
    for restarting in [false, true] {
        let directory = root.clone();
        let service = tokio::spawn(async move {
            homesensed::service::serve_unified_simulation(
                include_bytes!("../../../config/simulation.json"),
                &rules(),
                &directory,
                Duration::from_secs(4),
                homesensed::service::UnifiedSimulation {
                    address,
                    scenes: ["reading", "daily", "eco", "off"].map(String::from).into(),
                },
            )
            .await
        });
        let client_bindings = bindings.clone();
        let client_path = path.clone();
        let signing = signing.clone();
        let client_accessory_id = accessory_id.clone();
        let client = tokio::task::spawn_blocking(move || {
            let deadline = Instant::now() + Duration::from_secs(2);
            let mut stream = loop {
                match TcpStream::connect(address) {
                    Ok(s) => break s,
                    Err(e) => {
                        assert!(Instant::now() < deadline, "{e}");
                        std::thread::sleep(Duration::from_millis(10));
                    }
                }
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let session = support::verify(
                &mut stream,
                controller,
                &signing,
                &client_accessory_id,
                &accessory_key,
            );
            let mut c = Client {
                stream,
                session,
                bindings: client_bindings,
                events: vec![],
            };
            let (h, g) = c.request("GET", "/accessories", None);
            assert!(h.starts_with("HTTP/1.1 200"));
            assert_eq!(g["accessories"].as_array().unwrap().len(), 6);
            if restarting {
                c.selected(Some("eco"));
                assert_eq!(
                    receipts(&client_path),
                    [
                        "off", "reading", "off", "daily", "eco", "off", "reading", "eco"
                    ]
                );
                return;
            }
            let states = c.states();
            for id in ["reading", "daily", "eco"] {
                assert_eq!(states[id], None);
            }
            assert_eq!(states["off"], Some(false));
            c.writes(
                &[
                    ("reading", None, Some(true)),
                    ("daily", None, Some(true)),
                    ("eco", None, Some(true)),
                ],
                true,
            );
            assert!(receipts(&client_path).is_empty());
            c.writes(&[("reading", Some(false), None)], false);
            assert!(receipts(&client_path).is_empty());
            c.writes(&[("off", Some(true), None)], true);
            std::thread::sleep(Duration::from_millis(150));
            c.selected(None);
            c.writes(&[("reading", Some(true), None)], true);
            std::thread::sleep(Duration::from_millis(150));
            c.selected(Some("reading"));
            c.writes(&[("reading", Some(true), None)], true);
            c.selected(Some("reading"));
            assert_eq!(receipts(&client_path), ["off", "reading"]);
            c.writes(&[("reading", Some(false), None)], true);
            std::thread::sleep(Duration::from_millis(150));
            c.selected(None);
            c.writes(&[("daily", Some(true), None)], true);
            std::thread::sleep(Duration::from_millis(150));
            c.selected(Some("daily"));
            c.events.clear();
            c.writes(&[("eco", Some(true), None)], true);
            std::thread::sleep(Duration::from_millis(150));
            c.selected(Some("eco"));
            let changes = c
                .events
                .iter()
                .flat_map(|e| e["characteristics"].as_array().unwrap())
                .collect::<Vec<_>>();
            let daily = c.bindings["daily"].aid;
            let eco = c.bindings["eco"].aid;
            let daily_off = changes
                .iter()
                .position(|item| item["aid"] == daily && item["value"] == false)
                .expect("missing old selection Off event");
            let eco_on = changes
                .iter()
                .position(|item| item["aid"] == eco && item["value"] == true)
                .expect("missing new selection On event");
            assert!(
                daily_off < eco_on,
                "old selection Off must precede new On: {changes:?}"
            );
            c.writes(&[("daily", Some(false), None)], true);
            c.selected(Some("eco"));
            assert_eq!(
                receipts(&client_path),
                ["off", "reading", "off", "daily", "eco"]
            );
            c.writes(
                &[("daily", Some(true), None), ("reading", Some(true), None)],
                false,
            );
            c.selected(Some("eco"));
            c.writes(
                &[("off", Some(true), None), ("reading", Some(true), None)],
                false,
            );
            c.selected(Some("eco"));
            assert_eq!(receipts(&client_path).len(), 5);
            c.writes(&[("off", Some(true), None)], true);
            std::thread::sleep(Duration::from_millis(150));
            c.selected(None);
            c.writes(&[("reading", Some(true), None)], true);
            std::thread::sleep(Duration::from_millis(150));
            c.selected(Some("reading"));
            c.writes(
                &[("reading", Some(false), None), ("eco", Some(true), None)],
                true,
            );
            std::thread::sleep(Duration::from_millis(150));
            c.selected(Some("eco"));
            assert_eq!(
                receipts(&client_path),
                [
                    "off", "reading", "off", "daily", "eco", "off", "reading", "eco"
                ]
            );
        });
        let result = client.await;
        assert!(service.await.unwrap().is_ok());
        result.unwrap();
        let saved = identity.load_config().await.unwrap();
        if let Some(number) = configuration_number {
            assert_eq!(saved.configuration_number, number);
        } else {
            configuration_number = Some(saved.configuration_number);
        }
        assert_eq!(saved.device_id.to_string(), accessory_id);
        assert_eq!(saved.device_ed25519_keypair.verifying_key(), accessory_key);
        assert!(identity.controller_is_paired(controller).unwrap());
    }
    let db = rusqlite::Connection::open(&path).unwrap();
    assert_eq!(
        db.query_row("SELECT count(*) FROM command_journal", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    drop(db);
    drop(identity);
    std::fs::remove_dir_all(root).unwrap();
}
