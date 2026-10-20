//! Encrypted controller against the actual unified simulation service.
#[path = "../../homesense-hap/tests/support/mod.rs"]
mod support;
use hap::{
    pairing::{Pairing, Permissions},
    storage::Storage,
};
use homesense_hap::{
    graph::{LightGraph, LightSpec},
    scene_graph::{SceneGraph, SceneSpec},
    storage::IdentityStore,
};
use std::{
    net::{TcpListener, TcpStream},
    time::{Duration, Instant},
};
use uuid::Uuid;
#[tokio::test]
async fn unified_service_retires_light_handles_and_does_not_deadlock_scene_writes() {
    run(false).await;
}
#[tokio::test]
async fn combined_service_keeps_native_light_control_and_paired_scene_identity() {
    run(true).await;
}
async fn run(combined:bool) {
    let root = std::env::temp_dir().join(format!("unified-hap-{}", Uuid::new_v4()));
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
    let light = lights.bindings[0].clone();
    let scenes = SceneGraph::build(
        &identity,
        vec![SceneSpec {
            id: "demo".into(),
            name: "demo".into(),
        }],
    )
    .await
    .unwrap();
    let scene = scenes.bindings[0].clone();
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
    let devices = include_bytes!("../../../config/simulation.json");
    let rules=br#"{"schema_version":1,"configuration_revision":1,"variables":[],"rules":[],"timed_scenes":[{"id":"demo","enabled":true,"review_evidence":"simulation","max_lateness_ms":1000,"steps":[{"source":"fixture","offset_ms":60000,"device":"simulation:light","command":{"kind":"level","value":30,"ramp":false}}]}]}"#;
    drop(reservation);
    let web_reservation=TcpListener::bind("127.0.0.1:0").unwrap();
    let web_address=web_reservation.local_addr().unwrap();
    drop(web_reservation);
    let directory = root.clone();
    let service = tokio::spawn(async move {
        let hap=homesensed::service::UnifiedSimulation{address,scenes:vec!["demo".into()]};
        if combined {
            homesensed::service::serve_combined_simulation(devices,rules,&directory,Duration::from_secs(5),hap,homesensed::service::WebSettings{address:web_address,secret:"abcdefghijklmnopqrstuvwxyz0123456789".into(),administrator:true,tls:None}).await
        } else {
            homesensed::service::serve_unified_simulation(devices,rules,&directory,Duration::from_secs(5),hap).await
        }
    });
    let client = tokio::task::spawn_blocking(move || {
        let deadline = Instant::now() + Duration::from_secs(3);
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
        let mut session = support::verify(
            &mut stream,
            controller,
            &signing,
            &accessory_id,
            &accessory_key,
        );
        let (headers, body) = session.request(
            &mut stream,
            b"GET /accessories HTTP/1.1\r\nHost: localhost\r\n\r\n",
        );
        assert!(headers.starts_with("HTTP/1.1 200"));
        let graph: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(graph["accessories"].as_array().unwrap().len(), 2);
        assert!(graph["accessories"].as_array().unwrap().iter().all(|a| a["services"].as_array().unwrap().iter().all(|s| s["type"] != "43")));
        let mut write = |items: serde_json::Value| {
            let body = serde_json::json!({"characteristics":items}).to_string();
            let request = format!(
                "PUT /characteristics HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/hap+json\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            session.request(&mut stream, request.as_bytes())
        };
        for index in 0..5 {
            let (h, b) =
                write(serde_json::json!([{"aid":scene.aid,"iid":scene.on_iid,"value":true}]));
            assert!(h.starts_with("HTTP/1.1 204"), "{h} {b:?}");
            let (h, b) = write(
                serde_json::json!([{"aid":light.aid,"iid":light.brightness_iid.unwrap(),"value":30}]),
            );
            assert!(h.starts_with("HTTP/1.1 207"), "{h} {b:?}");
            let rejected: serde_json::Value = serde_json::from_slice(&b).unwrap();
            assert_eq!(rejected["characteristics"][0]["status"], -70409);
            if combined {
                let (status, value) = web_request(web_address, "/api/v1/device-commands", Some(serde_json::json!({"request_id":format!("web-level-{index}"),"device":"simulation:light","command":{"action":"brightness","level":30}})));
                assert_eq!(status, 202, "{value}");
            }
        }
        if combined {
            let deadline=Instant::now()+Duration::from_secs(2);
            loop {
                let (status,value)=web_request(web_address,"/api/v1/devices",None);
                assert_eq!(status,200);
                if value["devices"][0]["observed"]["level"]==30 {break;}
                assert!(Instant::now()<deadline,"{value}");
                std::thread::sleep(Duration::from_millis(10));
            }
            let (status,value)=web_request(web_address,&format!("/api/v1/config/activations/{}",Uuid::new_v4()),Some(serde_json::json!({"request_id":"no-publication","boot":"test"})));
            assert_eq!(status,503);assert_eq!(value["error"],"publication_unavailable");
            let (status,value)=web_request(web_address,"/api/v1/device-commands",Some(serde_json::json!({"request_id":"web-level","device":"simulation:light","command":{"action":"brightness","level":40}})));
            assert_eq!(status,202,"{value}");
        }
        let (h, _) = write(
            serde_json::json!([{"aid":light.aid,"iid":light.on_iid,"value":true},{"aid":scene.aid,"iid":scene.on_iid,"value":true}]),
        );
        assert!(h.starts_with("HTTP/1.1 207"));
        let request = format!(
            "GET /characteristics?id={}.{} HTTP/1.1\r\nHost: localhost\r\n\r\n",
            scene.aid, scene.on_iid
        );
        let (h, b) = session.request(&mut stream, request.as_bytes());
        assert!(h.starts_with("HTTP/1.1 200"));
        let value: serde_json::Value = serde_json::from_slice(&b).unwrap();
        assert_eq!(value["characteristics"][0]["value"], false);
        if combined {
            let deadline=Instant::now()+Duration::from_secs(2);
            loop {
                let (status, value)=web_request(web_address,"/api/v1/devices",None);
                assert_eq!(status,200);
                if value["devices"][0]["observed"]["level"]==40 {break;}
                assert!(Instant::now()<deadline,"{value}");std::thread::sleep(Duration::from_millis(10));
            }
        }
        let payload=serde_json::json!({"characteristics":[{"aid":scene.aid,"iid":scene.on_iid,"ev":true,"value":true}]}).to_string();
        let request = format!(
            "PUT /characteristics HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/hap+json\r\nContent-Length: {}\r\n\r\n{}",
            payload.len(),
            payload
        );
        let first = session.request(&mut stream, request.as_bytes());
        let second = session.request(&mut stream, b"");
        assert!(
            [&first.0, &second.0]
                .iter()
                .any(|h| h.starts_with("HTTP/1.1 204"))
        );
        let event = if first.0.starts_with("EVENT/") {
            first
        } else {
            second
        };
        assert!(event.0.starts_with("EVENT/"), "{}", event.0);
        let event: serde_json::Value = serde_json::from_slice(&event.1).unwrap();
        assert_eq!(event["characteristics"][0]["value"], false);
    });
    let client_result = client.await;
    let service_result = service.await;
    assert!(
        service_result.as_ref().is_ok_and(|r| r.is_ok()),
        "service: {service_result:?}"
    );
    client_result.unwrap();
    let number=identity.load_config().await.unwrap().configuration_number;
    homesensed::service::serve_unified_simulation(devices,rules,&root,Duration::from_millis(100),homesensed::service::UnifiedSimulation{address,scenes:vec!["demo".into()]}).await.unwrap();
    assert_eq!(identity.load_config().await.unwrap().configuration_number,number);
    assert!(identity.controller_is_paired(controller).unwrap());
    let sql = rusqlite::Connection::open(root.join("automation-simulation.sqlite")).unwrap();
    assert_eq!(
        sql.query_row("SELECT count(*) FROM scene_run", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        6
    );
    drop(sql);
    drop(identity);
    std::fs::remove_dir_all(root).unwrap();
}

fn web_request(address:std::net::SocketAddr,path:&str,body:Option<serde_json::Value>)->(u16,serde_json::Value) {
    use std::io::{Read,Write};
    let mut stream=TcpStream::connect(address).unwrap();stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    let text=body.as_ref().map_or(String::new(),|v|v.to_string());
    write!(stream,"{} {path} HTTP/1.1\r\nHost: {address}\r\nOrigin: http://{address}\r\nAuthorization: Bearer abcdefghijklmnopqrstuvwxyz0123456789\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",if body.is_some(){"POST"}else{"GET"},text.len()).unwrap();
    let mut response=String::new();stream.read_to_string(&mut response).unwrap();let (header,body)=response.split_once("\r\n\r\n").unwrap();
    (header.split_whitespace().nth(1).unwrap().parse().unwrap(),serde_json::from_str(body).unwrap())
}

#[tokio::test]
async fn occupied_hap_listener_prevents_startup_rule_execution() {
    let root=std::env::temp_dir().join(format!("hap-startup-gate-{}",Uuid::new_v4()));
    let occupied=TcpListener::bind("127.0.0.1:0").unwrap();
    let address=occupied.local_addr().unwrap();
    let rules=serde_json::json!({"schema_version":1,"configuration_revision":1,
        "variables":[{"id":"booted","kind":"integer","lifetime":"persistent"}],
        "rules":[{"id":"boot","enabled":true,"review_evidence":"synthetic","source":"fixture","trigger":{"kind":"startup_synchronized"},"actions":[{"source":"fixture","action":{"kind":"set","variable":"booted","value":{"kind":"integer","value":1}}}]}],
        "timed_scenes":[{"id":"demo","enabled":true,"review_evidence":"synthetic","max_lateness_ms":1000,"steps":[{"source":"fixture","offset_ms":0,"device":"simulation:light","command":{"kind":"level","value":30,"ramp":false}}]}]});
    let result=homesensed::service::serve_unified_simulation(include_bytes!("../../../config/simulation.json"),&serde_json::to_vec(&rules).unwrap(),&root,Duration::from_secs(1),homesensed::service::UnifiedSimulation{address,scenes:vec!["demo".into()]}).await;
    assert!(result.is_err());
    let sql=rusqlite::Connection::open(root.join("automation-simulation.sqlite")).unwrap();
    assert_eq!(sql.query_row("SELECT count(*) FROM rule_admission",[],|r|r.get::<_,i64>(0)).unwrap(),0);
    assert_eq!(sql.query_row("SELECT count(*) FROM command_journal",[],|r|r.get::<_,i64>(0)).unwrap(),0);
    drop(sql);drop(occupied);std::fs::remove_dir_all(root).unwrap();
}
