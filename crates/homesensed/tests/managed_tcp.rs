#[path = "../../homesense-hap/tests/support/mod.rs"]
mod support;
use hap::storage::Storage;
use std::{
    io::{Read, Write},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU8, Ordering},
    },
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use uuid::Uuid;
async fn request(
    address: std::net::SocketAddr,
    path: &str,
    body: Option<serde_json::Value>,
) -> (u16, serde_json::Value) {
    let mut socket = tokio::net::TcpStream::connect(address).await.unwrap();
    let text = body.as_ref().map_or(String::new(), |v| v.to_string());
    socket.write_all(format!("{} {path} HTTP/1.1\r\nHost: {address}\r\nOrigin: http://{address}\r\nAuthorization: Bearer abcdefghijklmnopqrstuvwxyz0123456789\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",if body.is_some(){"POST"}else{"GET"},text.len()).as_bytes()).await.unwrap();
    let mut response = vec![];
    tokio::time::timeout(Duration::from_secs(5), socket.read_to_end(&mut response))
        .await
        .unwrap()
        .unwrap();
    let response = String::from_utf8(response).unwrap();
    let (header, body) = response.split_once("\r\n\r\n").unwrap();
    (
        header.split_whitespace().nth(1).unwrap().parse().unwrap(),
        serde_json::from_str(body).unwrap(),
    )
}
#[tokio::test]
async fn managed_tcp_uses_one_owner_and_recovers_view_after_publication() { run_managed(false).await; }
#[tokio::test]
async fn combined_tcp_rebuilds_hap_and_retires_old_sessions_after_publication() { run_managed(true).await; }
async fn run_managed(combined:bool) {
    let root = std::env::temp_dir().join(format!("homesense-managed-{}", Uuid::new_v4()));
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let bus = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let mut devices: serde_json::Value =
        serde_json::from_slice(include_bytes!("../../../config/simulation.json")).unwrap();
    devices["gateways"][0]["endpoint"] = bus.local_addr().unwrap().to_string().into();
    devices["gateways"][0]["modules"][0]["verification"] = serde_json::json!({"status":"verified","evidence":"synthetic loopback fixture","verified_at_unix_ms":1});
    let mut rules = serde_json::json!({"schema_version":1,"configuration_revision":1,"variables":[],"rules":[]});
    if combined {rules["timed_scenes"]=serde_json::json!([{"id":"demo","enabled":true,"review_evidence":"synthetic","max_lateness_ms":1000,"steps":[{"source":"fixture","offset_ms":60000,"device":"simulation:light","command":{"kind":"level","value":90,"ramp":false}}]}]);}
    let writes = Arc::new(Mutex::new(vec![]));
    let received = writes.clone();
    let server = std::thread::spawn(move || {
        let level = Arc::new(AtomicU8::new(42));
        let mut workers = vec![];
        for _ in 0..2 {
            let (mut stream, _) = bus.accept().unwrap();
            let level = level.clone();
            let writes = received.clone();
            workers.push(std::thread::spawn(move || {
                stream
                    .set_read_timeout(Some(Duration::from_secs(10)))
                    .unwrap();
                loop {
                    let mut frame = [0; 5];
                    match stream.read_exact(&mut frame) {
                        Ok(()) => {}
                        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                        Err(e) => panic!("bus: {e}"),
                    };
                    match frame[2] {
                        0xa1 => assert_eq!(frame, [0xae, 0x15, 0xa1, 0xf2, 0xee]),
                        0x01 => {
                            writes.lock().unwrap().push(frame[3]);
                            level.store(frame[3], Ordering::SeqCst);
                        }
                        _ => panic!("unexpected bus command"),
                    }
                    stream
                        .write_all(&[0xae, 0x15, 0xd1, level.load(Ordering::SeqCst), 0xee])
                        .unwrap();
                }
            }));
        }
        for worker in workers {
            worker.join().unwrap();
        }
    });
    let service_devices = serde_json::to_vec(&devices).unwrap();
    let service_rules = serde_json::to_vec(&rules).unwrap();
    let hap_listener=std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let hap_address=hap_listener.local_addr().unwrap();drop(hap_listener);
    let controller=Uuid::new_v4();
    let signing=ed25519_dalek::SigningKey::from_bytes(&[23;32]);
    let mut identity=None;
    if combined {
        std::fs::create_dir_all(&root).unwrap();
        let mut store=homesense_hap::storage::IdentityStore::open(root.join("identity")).await.unwrap();
        let config=hap::Config{host:hap_address.ip(),port:hap_address.port(),..Default::default()};
        store.save_config(&config).await.unwrap();store.initialize_aid_cache().await.unwrap();
        homesense_hap::graph::LightGraph::build(&store,"HomeSense",vec![homesense_hap::graph::LightSpec{device:homesense_core::DeviceId::new("simulation:light").unwrap(),name:"模拟调光灯".into(),dimmable:true,default_on_level:None}]).await.unwrap();
        store.save_pairing(&hap::pairing::Pairing::new(controller,hap::pairing::Permissions::Admin,signing.verifying_key().to_bytes())).await.unwrap();
        identity=Some(store);
    }
    let directory = root.clone();
    let service = tokio::spawn(async move {
        if combined {
            return homesensed::automation_tcp::serve_combined(&service_devices,&service_rules,&directory,Duration::from_secs(6),true,0,Duration::from_millis(20),homesensed::service::WebSettings{address,secret:"abcdefghijklmnopqrstuvwxyz0123456789".into(),administrator:true,tls:None},homesensed::hap_endpoint::Settings{address:hap_address,scenes:vec!["demo".into()]}).await;
        }
        homesensed::automation_tcp::serve_managed(
            &service_devices,
            &service_rules,
            &directory,
            Duration::from_secs(6),
            true,
            0,
            Duration::from_millis(20),
            homesensed::service::WebSettings {
                address,
                secret: "abcdefghijklmnopqrstuvwxyz0123456789".into(),
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
    if service.is_finished() {panic!("service startup failed: {:?}",service.await);}
    let (code, snapshot) = request(address, "/api/v1/devices", None).await;
    assert_eq!(code, 200);
    assert_eq!(snapshot["devices"][0]["observed"]["level"], 42);
    let mut old_session=None;
    let mut old_number=None;
    if let Some(store)=&identity {
        old_session=Some(hap_write(store,hap_address,controller,signing.clone(),30,false).await);
        old_number=Some(store.load_config().await.unwrap().configuration_number);
    } else {
    let result=request(address,"/api/v1/device-commands",Some(serde_json::json!({"request_id":"one","device":"simulation:light","command":{"action":"brightness","level":30}}))).await;
    assert_eq!(result.1["outcome"], "accepted");
    }
    for _ in 0..30 {
        if writes.lock().unwrap().as_slice() == [30] {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    devices["revision"] = 2.into();
    devices["devices"][0]["name"] = "Renamed".into();
    let mut rules = rules;
    rules["configuration_revision"] = 2.into();
    if combined {
        let rejected=Uuid::new_v4();
        let incompatible=serde_json::json!({"schema_version":1,"configuration_revision":2,"variables":[],"rules":[]});
        assert_eq!(request(address,&format!("/api/v1/config/drafts/{rejected}"),Some(serde_json::json!({"expected_revision":1,"revision":2,"devices":devices,"automation":incompatible}))).await.0,200);
        let failed=request(address,&format!("/api/v1/config/activations/{rejected}"),Some(serde_json::json!({"request_id":"incompatible","boot":snapshot["boot"]}))).await;
        assert_ne!(failed.1["outcome"],"activated");
        assert_eq!(request(address,"/api/v1/devices",None).await.1["revision"],1);
    }
    let id = Uuid::new_v4();
    assert_eq!(request(address,&format!("/api/v1/config/drafts/{id}"),Some(serde_json::json!({"expected_revision":1,"revision":2,"devices":devices,"automation":rules}))).await.0,200);
    let activation = serde_json::json!({"request_id":"publish","boot":snapshot["boot"]});
    let result = request(
        address,
        &format!("/api/v1/config/activations/{id}"),
        Some(activation.clone()),
    )
    .await;
    assert_eq!(result.1["outcome"], "activated", "{result:?}");
    assert_eq!(
        request(
            address,
            &format!("/api/v1/config/activations/{id}"),
            Some(activation)
        )
        .await
        .0,
        200
    );
    if let Some(mut stream)=old_session {
        tokio::task::spawn_blocking(move || {let mut byte=[0];match stream.read(&mut byte){Ok(0)=>{},Err(e) if matches!(e.kind(),std::io::ErrorKind::ConnectionReset|std::io::ErrorKind::ConnectionAborted)=>{},other=>panic!("old HAP session survived publication: {other:?}")}}).await.unwrap();
    }
    let (_, next) = request(address, "/api/v1/devices", None).await;
    assert_eq!(next["revision"], 2);
    assert_eq!(next["devices"][0]["observed"]["level"], 30);
    if let Some(store)=&identity {
        let fresh=hap_write(store,hap_address,controller,signing.clone(),40,true).await;drop(fresh);
        assert!(store.load_config().await.unwrap().configuration_number>old_number.unwrap());
        assert!(store.controller_is_paired(controller).unwrap());
    } else {
    let result=request(address,"/api/v1/device-commands",Some(serde_json::json!({"request_id":"two","device":"simulation:light","command":{"action":"brightness","level":40}}))).await;
    assert_eq!(result.1["outcome"], "accepted");
    }
    service.await.unwrap().unwrap();
    server.join().unwrap();
    assert_eq!(*writes.lock().unwrap(), vec![30, 40]);
    assert_eq!(
        std::fs::read_dir(root.join("publication-backups"))
            .unwrap()
            .count(),
        1
    );
    if combined {
        let sql=rusqlite::Connection::open(root.join("automation-tcp.sqlite")).unwrap();
        assert_eq!(sql.query_row("SELECT count(*) FROM scene_run",[],|r|r.get::<_,i64>(0)).unwrap(),2);
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn occupied_management_port_never_runs_startup_actions() {
    let root = std::env::temp_dir().join(format!("homesense-managed-bind-{}", Uuid::new_v4()));
    let occupied = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let bus = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let mut devices: serde_json::Value =
        serde_json::from_slice(include_bytes!("../../../config/simulation.json")).unwrap();
    devices["gateways"][0]["endpoint"] = bus.local_addr().unwrap().to_string().into();
    devices["gateways"][0]["modules"][0]["verification"] = serde_json::json!({"status":"verified","evidence":"synthetic loopback fixture","verified_at_unix_ms":1});
    let server = std::thread::spawn(move || {
        let (mut stream, _) = bus.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut frame = [0; 5];
        stream.read_exact(&mut frame).unwrap();
        assert_eq!(frame, [0xae, 0x15, 0xa1, 0xf2, 0xee]);
        stream.write_all(&[0xae, 0x15, 0xd1, 42, 0xee]).unwrap();
        assert_eq!(stream.read(&mut frame).unwrap(), 0);
    });
    let rules = serde_json::json!({"schema_version":1,"configuration_revision":1,"variables":[],"rules":[{"id":"startup","enabled":true,"review_evidence":"synthetic","source":"fixture","trigger":{"kind":"startup_synchronized"},"actions":[{"source":"fixture","action":{"kind":"command","device":"simulation:light","command":{"kind":"level","value":30,"ramp":false}}}]}]});
    let result = homesensed::automation_tcp::serve_managed(
        &serde_json::to_vec(&devices).unwrap(),
        &serde_json::to_vec(&rules).unwrap(),
        &root,
        Duration::from_secs(1),
        true,
        0,
        Duration::from_millis(20),
        homesensed::service::WebSettings {
            address: occupied.local_addr().unwrap(),
            secret: "abcdefghijklmnopqrstuvwxyz0123456789".into(),
            administrator: false,
            tls: None,
        },
    )
    .await;
    assert!(result.is_err());
    server.join().unwrap();
    drop(occupied);
    std::fs::remove_dir_all(root).unwrap();
}

async fn hap_write(store:&homesense_hap::storage::IdentityStore,address:std::net::SocketAddr,controller:Uuid,signing:ed25519_dalek::SigningKey,level:u8,renamed:bool)->std::net::TcpStream {
    let settings=store.load_config().await.unwrap();
    let id=settings.device_id.to_string();let key=settings.device_ed25519_keypair.verifying_key();
    tokio::task::spawn_blocking(move || {
        let mut stream=std::net::TcpStream::connect(address).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();stream.set_write_timeout(Some(Duration::from_secs(2))).unwrap();
        let mut session=support::verify(&mut stream,controller,&signing,&id,&key);
        let (header,body)=session.request(&mut stream,b"GET /accessories HTTP/1.1\r\nHost: localhost\r\n\r\n");assert!(header.starts_with("HTTP/1.1 200"));
        if renamed {assert!(String::from_utf8_lossy(&body).contains("Renamed"));}
        let graph:serde_json::Value=serde_json::from_slice(&body).unwrap();
        assert_eq!(graph["accessories"].as_array().unwrap().len(),3);
        let scene=&graph["accessories"][2];
        let scene_iid=scene["services"].as_array().unwrap().iter().flat_map(|s|s["characteristics"].as_array().unwrap()).find(|c|c["type"]=="25").unwrap()["iid"].as_u64().unwrap();
        let body=serde_json::json!({"characteristics":[{"aid":scene["aid"],"iid":scene_iid,"value":true}]}).to_string();
        let request=format!("PUT /characteristics HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/hap+json\r\nContent-Length: {}\r\n\r\n{body}",body.len());
        let (header,body)=session.request(&mut stream,request.as_bytes());assert!(header.starts_with("HTTP/1.1 204"),"{header} {body:?}");
        let light=&graph["accessories"][1];let aid=light["aid"].as_u64().unwrap();
        let iid=light["services"].as_array().unwrap().iter().flat_map(|s|s["characteristics"].as_array().unwrap()).find(|c|c["format"]=="int" && c["unit"]=="percentage").unwrap()["iid"].as_u64().unwrap();
        let body=serde_json::json!({"characteristics":[{"aid":aid,"iid":iid,"value":level}]}).to_string();
        let request=format!("PUT /characteristics HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/hap+json\r\nContent-Length: {}\r\n\r\n{body}",body.len());
        let (header,body)=session.request(&mut stream,request.as_bytes());assert!(header.starts_with("HTTP/1.1 204"),"{header} {body:?}");
        stream
    }).await.unwrap()
}
