//! Multi-bridge encrypted controllers against one synthetic reconnecting bus.
#[path = "support/pair_setup.rs"]
mod pair_setup;
#[path = "../../homesense-hap/tests/support/mod.rs"]
mod support;
use hap::storage::Storage;
use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use uuid::Uuid;
fn free_address() -> SocketAddr {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
}
async fn view(address: SocketAddr) -> Option<serde_json::Value> {
    let mut socket = tokio::net::TcpStream::connect(address).await.ok()?;
    socket.write_all(format!("GET /api/v1/devices HTTP/1.1\r\nHost: {address}\r\nOrigin: http://{address}\r\nAuthorization: Bearer abcdefghijklmnopqrstuvwxyz0123456789\r\nConnection: close\r\n\r\n").as_bytes()).await.ok()?;
    let mut bytes = vec![];
    tokio::time::timeout(Duration::from_secs(2), socket.read_to_end(&mut bytes))
        .await
        .ok()?
        .ok()?;
    let text = String::from_utf8(bytes).ok()?;
    let (headers, body) = text.split_once("\r\n\r\n")?;
    headers
        .starts_with("HTTP/1.1 200")
        .then(|| serde_json::from_str(body).unwrap())
}
async fn exercise(
    store: &homesense_hap::storage::IdentityStore,
    address: SocketAddr,
    controller: Uuid,
    signing: ed25519_dalek::SigningKey,
    index: usize,
) -> std::net::TcpStream {
    let config = store.load_config().await.unwrap();
    tokio::task::spawn_blocking(move || {
        let mut socket=std::net::TcpStream::connect(address).unwrap();socket.set_read_timeout(Some(Duration::from_secs(3))).unwrap();socket.set_write_timeout(Some(Duration::from_secs(3))).unwrap();
        let mut session=support::verify(&mut socket,controller,&signing,&config.device_id.to_string(),&config.device_ed25519_keypair.verifying_key());
        let (header,body)=session.request(&mut socket,b"GET /accessories HTTP/1.1\r\nHost: localhost\r\n\r\n");assert!(header.starts_with("HTTP/1.1 200"));
        let graph:serde_json::Value=serde_json::from_slice(&body).unwrap();assert_eq!(graph["accessories"].as_array().unwrap().len(),1);
        let text=String::from_utf8(body).unwrap();assert!(!text.contains(&format!("Lamp {index}")));assert!(!text.contains(&format!("Lamp {}",1-index)));
        socket
    }).await.unwrap()
}
#[tokio::test]
async fn legacy_light_only_bridges_recover_without_publishing_lights() {
    run_pairing(false).await;
}
#[tokio::test]
async fn fresh_pair_setup_on_legacy_bridges_survives_recovery_and_restart() {
    run_pairing(true).await;
}
async fn run_pairing(fresh: bool) {
    let root =
        std::env::temp_dir().join(format!("homesense-multiple-encrypted-{}", Uuid::new_v4()));
    let bus = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut devices: serde_json::Value =
        serde_json::from_slice(include_bytes!("../../../config/simulation.json")).unwrap();
    devices["gateways"][0]["endpoint"] = bus.local_addr().unwrap().to_string().into();
    devices["gateways"][0]["modules"][0]["verification"] = serde_json::json!({"status":"verified","evidence":"synthetic loopback","verified_at_unix_ms":1});
    devices["devices"][0]["name"] = "Lamp 0".into();
    let mut second = devices["devices"][0].clone();
    second["id"] = "second".into();
    second["name"] = "Lamp 1".into();
    second["channel"] = 2.into();
    devices["devices"].as_array_mut().unwrap().push(second);
    let devices = serde_json::to_vec(&devices).unwrap();
    let rules = br#"{"schema_version":1,"configuration_revision":1,"variables":[],"rules":[]}"#;
    let config =
        homesense_config::Configuration::parse(&devices, homesense_core::ExecutionContext::Live)
            .unwrap();
    let document = homesense_config::rule_document::Document::parse(rules, &config).unwrap();
    let addresses = [free_address(), free_address()];
    let web = free_address();
    let plan=serde_json::to_vec(&serde_json::json!({"schema_version":1,"bridges":[
        {"id":"one","name":"Floor 0","address":addresses[0],"lights":["simulation:light"],"scenes":[],"motors":[]},
        {"id":"two","name":"Floor 1","address":addresses[1],"lights":["second"],"scenes":[],"motors":[]}
    ]})).unwrap();
    let mut prepared = homesensed::hap_bridges::prepare(&plan, &config, &document, &root)
        .await
        .unwrap();
    let controllers = [Uuid::new_v4(), Uuid::new_v4()];
    let keys = [
        ed25519_dalek::SigningKey::from_bytes(&[21; 32]),
        ed25519_dalek::SigningKey::from_bytes(&[22; 32]),
    ];
    let mut initial = vec![];
    for (index, bridge) in prepared.iter_mut().enumerate() {
        bridge.identity.initialize_aid_cache().await.unwrap();
        homesense_hap::graph::LightGraph::build(
            &bridge.identity,
            &bridge.name,
            vec![homesense_hap::graph::LightSpec {
                device: bridge.lights.first().unwrap().clone(),
                name: format!("Lamp {index}"),
                dimmable: true,
                default_on_level: None,
            }],
        )
        .await
        .unwrap();
        if !fresh {
            bridge
                .identity
                .save_pairing(&hap::pairing::Pairing::new(
                    controllers[index],
                    hap::pairing::Permissions::Admin,
                    keys[index].verifying_key().to_bytes(),
                ))
                .await
                .unwrap();
        }
        initial.push(bridge.identity.load_config().await.unwrap());
        assert!(
            !bridge
                .identity
                .controller_is_paired(controllers[1 - index])
                .unwrap()
        );
    }
    let writes = Arc::new(Mutex::new(vec![]));
    let received = writes.clone();
    let (disconnect, mut disconnected) = tokio::sync::oneshot::channel();
    let gateway = tokio::spawn(async move {
        for epoch in 0..if fresh { 3 } else { 2 } {
            let (mut socket, _) = bus.accept().await.unwrap();
            let mut levels = if epoch == 0 { [20, 21] } else { [60, 61] };
            loop {
                let mut frame = [0; 5];
                tokio::select! {
                    result=socket.read_exact(&mut frame)=>if result.is_err(){break;},
                    _=&mut disconnected,if epoch==0=>break,
                }
                if frame[2] == 0xa0 {
                    assert_eq!(frame[3], 0xf2);
                    let replies: Vec<_> = (0..8).flat_map(|index| {
                        [0xae, 0x15, 0xd1 + index, levels.get(index as usize).copied().unwrap_or(0), 0xee]
                    }).collect();
                    if socket.write_all(&replies).await.is_err() { break; }
                    continue;
                }
                let channel = match frame[2] {
                    0xa1 | 0x01 => 0,
                    0xa2 | 0x02 => 1,
                    _ => panic!("unexpected frame {frame:?}"),
                };
                if frame[2] < 0xa0 {
                    levels[channel] = frame[3];
                    received.lock().unwrap().push((epoch, channel, frame[3]));
                }
                if socket
                    .write_all(&[0xae, 0x15, 0xd1 + channel as u8, levels[channel], 0xee])
                    .await
                    .is_err()
                {
                    break;
                }
            }
        }
    });
    let restart_devices = devices.clone();
    let restart_plan = plan.clone();
    let directory = root.clone();
    let service = tokio::spawn(async move {
        homesensed::automation_tcp::serve_bridges(
            &devices,
            rules,
            &directory,
            // Fresh SRP setup for both identities precedes the same recovery checks.
            Duration::from_secs(if fresh {16}else{8}),
            true,
            1,
            Duration::from_millis(20),
            homesensed::service::WebSettings {
                address: web,
                secret: "abcdefghijklmnopqrstuvwxyz0123456789".into(),
                administrator: true,
                tls: None,
            },
            plan,
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(3), async {
        while view(web).await.is_none() {
            assert!(!service.is_finished());
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    if fresh {
        for index in 0..2 {
            let config = prepared[index].identity.load_config().await.unwrap();
            assert_eq!(prepared[index].identity.count_pairings().await.unwrap(), 0);
            let address = addresses[index];
            let id = controllers[index];
            let signing = keys[index].clone();
            tokio::task::spawn_blocking(move || {
                let mut stream = std::net::TcpStream::connect(address).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                pair_setup::setup(
                    &mut stream,
                    &config.pin.to_string(),
                    id,
                    &signing,
                    &config.device_id.to_string(),
                    &config.device_ed25519_keypair.verifying_key(),
                );
            })
            .await
            .unwrap();
            let paired = prepared[index].identity.load_pairing(&id).await.unwrap();
            assert_eq!(paired.public_key, keys[index].verifying_key().to_bytes());
            assert_eq!(paired.permissions, hap::pairing::Permissions::Admin);
            assert_eq!(prepared[index].identity.count_pairings().await.unwrap(), 1);
        }
    }
    // A controller paired only with the other bridge must not authenticate,
    // even though both bridges share the runtime admission owner.
    for index in 0..2 {
        let config = prepared[index].identity.load_config().await.unwrap();
        let other = controllers[1 - index];
        let signing = keys[1 - index].clone();
        let address = addresses[index];
        tokio::task::spawn_blocking(move || {
            let mut socket = std::net::TcpStream::connect(address).unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            socket
                .set_write_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let result = support::verify_outcome(
                &mut socket,
                other,
                &signing,
                &config.device_id.to_string(),
                &config.device_ed25519_keypair.verifying_key(),
            );
            assert_eq!(
                result.err(),
                Some(vec![1]),
                "expected unknown-pairing rejection"
            );
        })
        .await
        .unwrap();
    }
    let mut old = vec![];
    for index in 0..2 {
        old.push(
            exercise(
                &prepared[index].identity,
                addresses[index],
                controllers[index],
                keys[index].clone(),
                index,
            )
            .await,
        );
    }
    assert!(writes.lock().unwrap().is_empty());
    let mut numbers = vec![];
    for bridge in &prepared {
        numbers.push(
            bridge
                .identity
                .load_config()
                .await
                .unwrap()
                .configuration_number,
        );
    }
    disconnect.send(()).unwrap();
    for mut socket in old {
        tokio::task::spawn_blocking(move || {
            use std::io::Read;
            let mut byte = [0];
            match socket.read(&mut byte) {
                Ok(0) => {}
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted
                    ) => {}
                other => panic!("old session not retired: {other:?}"),
            }
        })
        .await
        .unwrap();
    }
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let Some(snapshot) = view(web).await
                && snapshot["devices"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|d| d["observed"]["level"] == 60 && d["reported"]["level"] == 60)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    for index in 0..2 {
        drop(
            exercise(
                &prepared[index].identity,
                addresses[index],
                controllers[index],
                keys[index].clone(),
                index,
            )
            .await,
        );
        let after = prepared[index].identity.load_config().await.unwrap();
        assert_eq!(after.device_id, initial[index].device_id);
        assert_eq!(
            after.device_ed25519_keypair.to_bytes(),
            initial[index].device_ed25519_keypair.to_bytes()
        );
        assert_eq!(after.configuration_number, numbers[index]);
        assert!(
            prepared[index]
                .identity
                .controller_is_paired(controllers[index])
                .unwrap()
        );
    }
    service.await.unwrap().unwrap();
    if fresh {
        let directory = root.clone();
        let restarted = tokio::spawn(async move {
            homesensed::automation_tcp::serve_bridges(
                &restart_devices,
                rules,
                &directory,
                Duration::from_secs(3),
                true,
                0,
                Duration::from_millis(20),
                homesensed::service::WebSettings {
                    address: web,
                    secret: "abcdefghijklmnopqrstuvwxyz0123456789".into(),
                    administrator: true,
                    tls: None,
                },
                restart_plan,
            )
            .await
        });
        tokio::time::timeout(Duration::from_secs(2), async {
            while view(web).await.is_none() {
                assert!(!restarted.is_finished());
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        for index in 0..2 {
            let reopened = homesense_hap::storage::IdentityStore::open(
                root.join("hap-bridges")
                    .join(if index == 0 { "one" } else { "two" })
                    .join("identity"),
            )
            .await
            .unwrap();
            assert_eq!(reopened.count_pairings().await.unwrap(), 1);
            drop(
                exercise(
                    &reopened,
                    addresses[index],
                    controllers[index],
                    keys[index].clone(),
                    index,
                )
                .await,
            );
            let saved = reopened.load_config().await.unwrap();
            assert_eq!(saved.device_id, initial[index].device_id);
            assert_eq!(
                saved.device_ed25519_keypair.to_bytes(),
                initial[index].device_ed25519_keypair.to_bytes()
            );
            assert_eq!(saved.configuration_number, numbers[index]);
        }
        restarted.await.unwrap().unwrap();
    }
    gateway.await.unwrap();
    assert!(writes.lock().unwrap().is_empty());
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn scene_only_and_motor_only_bridges_control_without_exposing_lights() {
    let root = std::env::temp_dir().join(format!("homesense-actions-only-{}", Uuid::new_v4()));
    let bus = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut raw: serde_json::Value =
        serde_json::from_slice(include_bytes!("../../../config/simulation.json")).unwrap();
    raw["gateways"][0]["endpoint"] = bus.local_addr().unwrap().to_string().into();
    let verified = serde_json::json!({"status":"verified","evidence":"synthetic loopback","verified_at_unix_ms":1});
    raw["gateways"][0]["modules"][0]["verification"] = verified.clone();
    raw["gateways"][0]["modules"].as_array_mut().unwrap().push(
        serde_json::json!({"address":"22","profile":"c4_din_pro_v2_7_7","verification":verified}),
    );
    raw["devices"].as_array_mut().unwrap().push(serde_json::json!({"id":"motor","name":"Curtain","gateway":"simulation","address":"22","channel":2,"enabled":true,"motor_calibration":{"open_direction":"up","stop_verified":true,"evidence":"synthetic","verified_at_unix_ms":1}}));
    let devices = serde_json::to_vec(&raw).unwrap();
    let rules=serde_json::to_vec(&serde_json::json!({"schema_version":1,"configuration_revision":1,"variables":[],"rules":[],"timed_scenes":[{"id":"demo","enabled":true,"review_evidence":"synthetic","max_lateness_ms":1000,"steps":[{"source":"fixture","offset_ms":0,"device":"simulation:light","command":{"kind":"level","value":55,"ramp":false}}]}]})).unwrap();
    let config =
        homesense_config::Configuration::parse(&devices, homesense_core::ExecutionContext::Live)
            .unwrap();
    let document = homesense_config::rule_document::Document::parse(&rules, &config).unwrap();
    let addresses = [free_address(), free_address()];
    let web = free_address();
    let plan=serde_json::to_vec(&serde_json::json!({"schema_version":1,"bridges":[
        {"id":"scenes","name":"Scenes","address":addresses[0],"lights":["simulation:light"],"scenes":["demo"],"motors":[]},
        {"id":"motors","name":"Motors","address":addresses[1],"motors":["motor"]}
    ]})).unwrap();
    let mut prepared = homesensed::hap_bridges::prepare(&plan, &config, &document, &root)
        .await
        .unwrap();
    let controller = Uuid::new_v4();
    let signing = ed25519_dalek::SigningKey::from_bytes(&[27; 32]);
    for bridge in &mut prepared {
        bridge.identity.initialize_aid_cache().await.unwrap();
        homesense_hap::graph::LightGraph::build(&bridge.identity, &bridge.name, vec![])
            .await
            .unwrap();
        if bridge.id == "scenes" {
            homesense_hap::scene_graph::SceneGraph::build(
                &bridge.identity,
                vec![homesense_hap::scene_graph::SceneSpec {
                    id: "demo".into(),
                    name: "demo".into(),
                }],
            )
            .await
            .unwrap();
        } else {
            homesense_hap::motor_graph::MotorGraph::build(
                &bridge.identity,
                vec![homesense_hap::motor_graph::MotorSpec {
                    device: homesense_core::DeviceId::new("motor").unwrap(),
                    name: "Curtain".into(),
                }],
            )
            .await
            .unwrap();
        }
        bridge
            .identity
            .save_pairing(&hap::pairing::Pairing::new(
                controller,
                hap::pairing::Permissions::Admin,
                signing.verifying_key().to_bytes(),
            ))
            .await
            .unwrap();
    }
    let writes = Arc::new(Mutex::new(vec![]));
    let received = writes.clone();
    let gateway = tokio::spawn(async move {
        let (mut socket, _) = bus.accept().await.unwrap();
        let mut level = 0;
        loop {
            let mut frame = [0; 5];
            if socket.read_exact(&mut frame).await.is_err() {
                break;
            }
            if frame[1] == 0x22 {
                received.lock().unwrap().push(frame);
                continue;
            }
            if frame[2] == 0xa0 {
                assert_eq!(frame, [0xae, 0x15, 0xa0, 0xf2, 0xee]);
                let replies: Vec<_> = (1..=8).flat_map(|channel| [0xae, 0x15, 0xd0 + channel, level, 0xee]).collect();
                if socket.write_all(&replies).await.is_err() { break; }
                continue;
            }
            if frame[2] == 1 {
                level = frame[3];
                received.lock().unwrap().push(frame);
            } else {
                assert_eq!(frame[2], 0xa1);
            }
            if socket
                .write_all(&[0xae, 0x15, 0xd1, level, 0xee])
                .await
                .is_err()
            {
                break;
            }
        }
    });
    let directory = root.clone();
    let service = tokio::spawn(async move {
        homesensed::automation_tcp::serve_bridges(
            &devices,
            &rules,
            &directory,
            Duration::from_secs(5),
            true,
            0,
            Duration::from_millis(20),
            homesensed::service::WebSettings {
                address: web,
                secret: "abcdefghijklmnopqrstuvwxyz0123456789".into(),
                administrator: true,
                tls: None,
            },
            plan,
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(3), async {
        while view(web).await.is_none() {
            assert!(!service.is_finished());
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    for (index, bridge) in prepared.iter().enumerate() {
        let config = bridge.identity.load_config().await.unwrap();
        let address = addresses[index];
        let signing = signing.clone();
        let writes = writes.clone();
        tokio::task::spawn_blocking(move || {
            let mut socket=std::net::TcpStream::connect(address).unwrap();socket.set_read_timeout(Some(Duration::from_secs(2))).unwrap();socket.set_write_timeout(Some(Duration::from_secs(2))).unwrap();
            let mut session=support::verify(&mut socket,controller,&signing,&config.device_id.to_string(),&config.device_ed25519_keypair.verifying_key());
            let (header,body)=session.request(&mut socket,b"GET /accessories HTTP/1.1\r\nHost: localhost\r\n\r\n");assert!(header.starts_with("HTTP/1.1 200"));
            let graph:serde_json::Value=serde_json::from_slice(&body).unwrap();let accessories=graph["accessories"].as_array().unwrap();assert_eq!(accessories.len(),2);assert!(!String::from_utf8(body).unwrap().contains("模拟调光灯"));
            for accessory in accessories.iter().skip(1) {
                let before=writes.lock().unwrap().len();
                let characteristic=accessory["services"].as_array().unwrap().iter().flat_map(|s|s["characteristics"].as_array().unwrap()).find(|c|c["type"]==if index==0 {"25"}else{"7C"}).unwrap();
                let body=serde_json::json!({"characteristics":[{"aid":accessory["aid"],"iid":characteristic["iid"],"value":if index==0 {serde_json::json!(true)}else{serde_json::json!(100)} }]}).to_string();let request=format!("PUT /characteristics HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/hap+json\r\nContent-Length: {}\r\n\r\n{body}",body.len());
                let (header,body)=session.request(&mut socket,request.as_bytes());assert!(header.starts_with("HTTP/1.1 204"),"{header} {body:?}");
                let deadline=std::time::Instant::now()+Duration::from_secs(2);while writes.lock().unwrap().len()==before {assert!(std::time::Instant::now()<deadline);std::thread::sleep(Duration::from_millis(5));}
            }
        }).await.unwrap();
    }
    service.await.unwrap().unwrap();
    gateway.await.unwrap();
    assert_eq!(
        *writes.lock().unwrap(),
        vec![[0xae, 0x15, 1, 55, 0xee], [0xae, 0x22, 2, 0xf1, 0xee]]
    );
    std::fs::remove_dir_all(root).unwrap();
}
