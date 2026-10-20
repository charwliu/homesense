//! Real localhost Web/publication/HAP path with synthetic, query-only gateways.
#[path="support/runtime_outage.rs"]
mod outage;
#[path = "../../homesense-hap/tests/support/mod.rs"]
mod support;
use hap::{
    serde_json::{Value, json},
    storage::Storage,
};
use homesense_core::{DeviceId, ExecutionContext};
use homesense_hap::{
    graph::{LightGraph, LightSpec},
    storage::IdentityStore,
};
use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    sync::{Arc, Mutex,atomic::{AtomicBool,Ordering}},
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use uuid::Uuid;

const TOKEN: &str = "module-publication-synthetic-test-token";
fn free_address() -> SocketAddr {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
}
async fn http(address: SocketAddr, path: &str, body: Option<Value>) -> (u16, Value) {
    let mut socket = tokio::net::TcpStream::connect(address).await.unwrap();
    let method = if body.is_some() { "POST" } else { "GET" };
    let body = body.map_or(String::new(), |v| v.to_string());
    // Each existing fixture call represents a distinct management intent.
    let idempotency = if method == "POST" && path.starts_with("/api/v1/config/drafts/") {
        format!("Idempotency-Key: {}\r\n", Uuid::new_v4())
    } else { String::new() };
    socket.write_all(format!("{method} {path} HTTP/1.1\r\nHost: {address}\r\nOrigin: http://{address}\r\nAuthorization: Bearer {TOKEN}\r\n{idempotency}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
    let mut bytes = vec![];
    tokio::time::timeout(Duration::from_secs(4), socket.read_to_end(&mut bytes))
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

async fn pending_activation(address:SocketAddr,path:&str,body:Value)->Option<(u16,Value)> {
    let mut socket=tokio::net::TcpStream::connect(address).await.unwrap();let body=body.to_string();
    socket.write_all(format!("POST {path} HTTP/1.1\r\nHost: {address}\r\nOrigin: http://{address}\r\nAuthorization: Bearer {TOKEN}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
    let mut bytes=vec![];
    match tokio::time::timeout(Duration::from_secs(7),socket.read_to_end(&mut bytes)).await.unwrap(){
        Ok(_)=>{},
        Err(e) if bytes.is_empty() && matches!(e.kind(),std::io::ErrorKind::ConnectionReset|std::io::ErrorKind::ConnectionAborted)=>return None,
        Err(e)=>panic!("activation response: {e}"),
    }
    if bytes.is_empty(){return None;}
    let text=String::from_utf8(bytes).unwrap();let (header,body)=text.split_once("\r\n\r\n").unwrap();
    Some((header.split_whitespace().nth(1).unwrap().parse().unwrap(),serde_json::from_str(body).unwrap()))
}
async fn assert_hap_light_unavailable_without_feedback(store:&IdentityStore,address:SocketAddr,controller:Uuid,signing:ed25519_dalek::SigningKey,graph:&Value) {
    let config=store.load_config().await.unwrap();let id=config.device_id.to_string();let public=config.device_ed25519_keypair.verifying_key();
    let light=&graph["accessories"][1];let aid=light["aid"].as_u64().unwrap();
    let values=light["services"].as_array().unwrap().iter().flat_map(|service|service["characteristics"].as_array().unwrap()).filter(|c|(c["format"]=="bool" && c["perms"].as_array().unwrap().iter().any(|permission|permission=="ev")) || (c["format"]=="int" && c["unit"]=="percentage")).map(|c|c["iid"].as_u64().unwrap()).collect::<Vec<_>>();
    assert_eq!(values.len(),2);
    tokio::task::spawn_blocking(move || {
        let mut socket=TcpStream::connect(address).unwrap();socket.set_read_timeout(Some(Duration::from_secs(2))).unwrap();socket.set_write_timeout(Some(Duration::from_secs(2))).unwrap();
        let mut session=support::verify(&mut socket,controller,&signing,&id,&public);
        let request=format!("GET /characteristics?id={aid}.{},{}.{} HTTP/1.1\r\nHost: localhost\r\n\r\n",values[0],aid,values[1]);
        let (header,body)=session.request(&mut socket,request.as_bytes());assert!(header.starts_with("HTTP/1.1 207"),"{header}");
        let body:Value=serde_json::from_slice(&body).unwrap();let characteristics=body["characteristics"].as_array().unwrap();assert_eq!(characteristics.len(),2);
        for value in characteristics {assert!(value["status"]==-70402 || value["status"]==-70409,"{value}");assert!(value.get("value").is_none(),"unavailable HAP light must not expose remembered state: {value}");}
    }).await.unwrap();
}

type IdentifierRows=(Vec<(String,i64)>,Vec<(i64,String,i64)>);
fn identifier_rows(path:&std::path::Path)->IdentifierRows {
    let db=rusqlite::Connection::open(path).unwrap();
    let mut query=db.prepare("SELECT device,aid FROM accessory_ids ORDER BY device").unwrap();
    let aids=query.query_map([],|r|Ok((r.get(0)?,r.get(1)?))).unwrap().collect::<Result<Vec<_>,_>>().unwrap();
    let mut query=db.prepare("SELECT aid,resource,iid FROM instance_ids ORDER BY aid,resource").unwrap();
    let iids=query.query_map([],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap().collect::<Result<Vec<_>,_>>().unwrap();
    (aids,iids)
}
async fn read_hap(
    store: &IdentityStore,
    address: SocketAddr,
    controller: Uuid,
    signing: ed25519_dalek::SigningKey,
) -> (TcpStream, Value) {
    let config = store.load_config().await.unwrap();
    let id = config.device_id.to_string();
    let public = config.device_ed25519_keypair.verifying_key();
    tokio::task::spawn_blocking(move || {
        let mut socket = TcpStream::connect(address).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        socket
            .set_write_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut session = support::verify(&mut socket, controller, &signing, &id, &public);
        let (header, body) = session.request(
            &mut socket,
            b"GET /accessories HTTP/1.1\r\nHost: localhost\r\n\r\n",
        );
        assert!(header.starts_with("HTTP/1.1 200"), "{header}");
        (socket, serde_json::from_slice(&body).unwrap())
    })
    .await
    .unwrap()
}
fn bridge_ids(graph: &Value) -> (u64, Vec<u64>) {
    let light = &graph["accessories"][0];
    (
        light["aid"].as_u64().unwrap(),
        light["services"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|s| s["characteristics"].as_array().unwrap())
            .map(|c| c["iid"].as_u64().unwrap())
            .collect(),
    )
}

async fn assert_retired(old_session:TcpStream) {
    tokio::task::spawn_blocking(move || {
        let mut old_session = old_session;
        old_session
            .set_read_timeout(Some(Duration::from_millis(500)))
            .unwrap();
        let mut byte = [0];
        match old_session.read(&mut byte) {
            Ok(0) => {}
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted
                ) => {}
            other => panic!("old HAP connection survived publication: {other:?}"),
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn stable_module_remap_publishes_all_ports_preserves_pairing_and_retires_old_hap() {
    exercise(false).await;
}
#[tokio::test]
async fn missing_unbound_port_during_remap_fails_closed_without_control_or_replay() {
    exercise(true).await;
}
async fn exercise(omit: bool) {
    let directory = std::env::temp_dir().join(format!("module-online-{}", Uuid::new_v4()));
    std::fs::create_dir(&directory).unwrap();
    let bus = TcpListener::bind("127.0.0.1:0").unwrap();
    let web = free_address();
    let hap_address = free_address();
    let verified =
        json!({"status":"verified","evidence":"synthetic localhost only","verified_at_unix_ms":1});
    let devices = json!({"schema_version":2,"revision":1,"gateways":[{"id":"bus","endpoint":bus.local_addr().unwrap().to_string(),"modules":[
        {"id":"module-light","address":"15","profile":"ad8_din_rs485_v2_2_1","verification":verified},
        {"id":"module-unbound","address":"16","profile":"ad8_din_rs485_v2_2_1","verification":verified}]}],"devices":[
        {"id":"light","name":"Synthetic light","module_id":"module-light","channel":1,"enabled":true},
        {"id":"disabled","name":"Disabled port","module_id":"module-light","channel":2,"enabled":false}]});
    let rules = json!({"schema_version":1,"configuration_revision":1,"variables":[],"rules":[],"timed_scenes":[{"id":"scene","enabled":true,"review_evidence":"synthetic only","max_lateness_ms":1000,"steps":[{"source":"synthetic","offset_ms":0,"device":"light","command":{"kind":"level","value":40,"ramp":true}}]}]});
    let frames = Arc::new(Mutex::new(vec![]));
    let tap = frames.clone();
    let stop=Arc::new(AtomicBool::new(false));let stopped=stop.clone();
    bus.set_nonblocking(true).unwrap();
    let gateway = std::thread::spawn(move || {
        let mut workers = vec![];
        while !stopped.load(Ordering::Acquire) {
            let (mut socket, _) = match bus.accept(){
                Ok(value)=>value,
                Err(e) if e.kind()==std::io::ErrorKind::WouldBlock=>{std::thread::sleep(Duration::from_millis(2));continue;},
                Err(e)=>panic!("synthetic accept: {e}"),
            };
            socket.set_nonblocking(false).unwrap();
            let generation=workers.len();
            let tap = tap.clone();
            workers.push(std::thread::spawn(move || {
                socket
                    .set_read_timeout(Some(Duration::from_secs(8)))
                    .unwrap();
                loop {
                    let mut frame = [0; 5];
                    match socket.read_exact(&mut frame) {
                        Ok(()) => {}
                        Err(e)
                            if matches!(
                                e.kind(),
                                std::io::ErrorKind::UnexpectedEof
                                    | std::io::ErrorKind::ConnectionReset
                            ) =>
                        {
                            break;
                        }
                        Err(e) => panic!("synthetic gateway: {e}"),
                    }
                    assert_eq!(
                        (frame[0], frame[3], frame[4]),
                        (0xae, 0xf2, 0xee),
                        "actuator output forbidden"
                    );
                    assert!((0xa0..=0xa8).contains(&frame[2]), "only queries allowed");
                    assert!(
                        if generation == 0 {
                            [0x15, 0x16].contains(&frame[1])
                        } else {
                            [0x16, 0x61].contains(&frame[1])
                        },
                        "query used obsolete mapping"
                    );
                    tap.lock().unwrap().push((generation, frame));
                    if generation >= 1 {
                        // Old physical address on the new stream must not seed the remapped module.
                        socket.write_all(&[0xae, 0x15, 0xd1, 99, 0xee]).unwrap();
                    }
                    let channels: Vec<_> = if frame[2] == 0xa0 {
                        (1..=8).collect()
                    } else {
                        vec![frame[2] - 0xa0]
                    };
                    let reply: Vec<_> = channels
                        .into_iter()
                        .filter(|channel| {
                            !(omit && generation >= 1 && frame[1] == 0x16 && *channel == 8)
                        })
                        .flat_map(|channel| {
                            [
                                0xae,
                                frame[1],
                                0xd0 + channel,
                                if generation == 0 {
                                    11
                                } else if frame[1] == 0x61 {
                                    44
                                } else {
                                    22
                                },
                                0xee,
                            ]
                        })
                        .collect();
                    socket.write_all(&reply).unwrap();
                }
            }));
        }
        for worker in workers {
            worker.join().unwrap();
        }
    });
    let controller = Uuid::new_v4();
    let signing = ed25519_dalek::SigningKey::from_bytes(&[82; 32]);
    let identity_path = directory.join("hap-bridges/synthetic/identity");
    std::fs::create_dir_all(identity_path.parent().unwrap()).unwrap();
    let mut identity = IdentityStore::open(&identity_path)
        .await
        .unwrap();
    let identity_config = hap::Config {
        host: hap_address.ip(),
        port: hap_address.port(),
        ..Default::default()
    };
    identity.save_config(&identity_config).await.unwrap();
    identity.initialize_aid_cache().await.unwrap();
    let retired_lights = LightGraph::build(
        &identity,
        "HomeSense",
        vec![LightSpec {
            device: DeviceId::new("light").unwrap(),
            name: "Synthetic light".into(),
            dimmable: true,
            default_on_level: None,
        }],
    )
    .await
    .unwrap();
    let retired_graph=json!({"accessories":[serde_json::to_value(&retired_lights.bridge).unwrap(),serde_json::to_value(retired_lights.lights.values().next().unwrap()).unwrap()]});
    let old_identifiers=identifier_rows(&identity_path.join("identity.sqlite"));
    identity
        .save_pairing(&hap::pairing::Pairing::new(
            controller,
            hap::pairing::Permissions::Admin,
            signing.verifying_key().to_bytes(),
        ))
        .await
        .unwrap();
    let input = serde_json::to_vec(&devices).unwrap();
    let automation = serde_json::to_vec(&rules).unwrap();
    let root = directory.clone();
    let started=std::time::Instant::now();
    let plan = serde_json::to_vec(&json!({"schema_version":1,"bridges":[{
        "id":"synthetic", "name":identity_config.name.clone(), "address":hap_address.to_string(),
        "all_lights":true, "scenes":[], "motors":[]
    }]})).unwrap();
    let service = tokio::spawn(async move {
        homesensed::automation_tcp::serve_bridges(
            &input,
            &automation,
            &root,
            Duration::from_secs(6),
            true,
            0,
            Duration::from_millis(20),
            homesensed::service::WebSettings {
                address: web,
                secret: TOKEN.into(),
                administrator: true,
                tls: None,
            },
            plan,
        )
        .await
    });
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while tokio::net::TcpStream::connect(web).await.is_err() {
        if service.is_finished() {
            panic!(
                "service ended before initial readiness: {:?}",
                service.await
            );
        }
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let (code, before) = http(web, "/api/v1/modules", None).await;
    assert_eq!(code, 200);
    assert!(
        before["modules"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["fresh_channels"] == 8)
    );
    let (_, initial_view)=http(web,"/api/v1/devices",None).await;
    assert_eq!(initial_view["devices"][0]["observed"]["level"],11);
    let (old_session, old_graph) =
        read_hap(&identity, hap_address, controller, signing.clone()).await;
    assert_eq!(old_graph["accessories"].as_array().unwrap().len(),2,"logical light is published");
    let source = Uuid::new_v4();
    let candidate = Uuid::new_v4();
    let mut source_devices = devices.clone();
    source_devices["revision"] = 2.into();
    let mut source_rules = rules.clone();
    source_rules["configuration_revision"] = 2.into();
    assert_eq!(http(web, &format!("/api/v1/config/drafts/{source}"), Some(json!({"expected_revision":1,"revision":2,"devices":source_devices,"automation":source_rules}))).await.0, 200);
    let edit = json!({"id":candidate,"expected_revision":"1","revision":"3","module_id":"module-light","new_address":"61","profile":"ad8_din_rs485_v2_2_1","verification":{"status":"verified","evidence":"synthetic localhost only","verified_at_unix_ms":"1"}});
    let (code, replacement) = http(
        web,
        &format!("/api/v1/config/drafts/{source}/module-replacement"),
        Some(edit),
    )
    .await;
    assert_eq!(code, 200, "{replacement}");
    assert_eq!(replacement["active"], false);
    assert_eq!(replacement["impact"]["hardware_address_changed"], false);
    let (_, preview) = http(web, &format!("/api/v1/config/previews/{candidate}"), None).await;
    assert_eq!(
        preview["impact"]["connection_rebuild_required"], true,
        "{preview}"
    );
    let activation = json!({"request_id":"synthetic-remap","boot":before["boot"]});
    if omit {
        // A publication is now allowed to wait for feedback until the original
        // six-second service deadline. Ordinary requests keep their 4s guard;
        // only this deliberately pending activation has a separate bounded read.
        let path=format!("/api/v1/config/activations/{candidate}");
        let activation_request={let path=path.clone();let body=activation.clone();tokio::spawn(async move {pending_activation(web,&path,body).await})};
        outage::wait_unavailable(web,TOKEN,"3",&service).await;
        outage::assert_gated(web,TOKEN,"3","light").await;
        outage::assert_empty_journal(&directory);
        assert_eq!(http(web,"/api/v1/modules",None).await.0,503);
        let (code,outcome)=http(web,&path,Some(activation)).await;
        assert_eq!(code,200);assert_eq!(outcome["outcome"],"unknown");assert_eq!(outcome["retry"],false);
        assert_ne!(outcome["outcome"],"activated","missing unbound channel cannot open readiness");
        assert!(!activation_request.is_finished(),"partial feedback cannot acknowledge activation");
        assert!(!service.is_finished());
        assert_hap_light_unavailable_without_feedback(&identity,hap_address,controller,signing,&retired_graph).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!service.is_finished(),"partial feedback must not end the six-second window early");
        outage::assert_gated(web,TOKEN,"3","light").await;
        outage::assert_empty_journal(&directory);
        tokio::time::timeout(Duration::from_secs(8),service).await.unwrap().unwrap().unwrap();
        assert!(started.elapsed()>=Duration::from_secs(6),"service ended before its original deadline");
        if let Some((code,outcome))=activation_request.await.unwrap(){
            assert_eq!(code,202);assert_eq!(outcome["outcome"],"unknown","{outcome}");assert_eq!(outcome["retry"],false);
        }
        // A clean shutdown may retire the HTTP request before its response is
        // serialized. Require its durable no-replay fence, independently of EOF.
        let receipt=rusqlite::Connection::open(directory.join("web-receipts.sqlite")).unwrap();
        let (count,response):(i64,Option<String>)=receipt.query_row("SELECT count(*),response FROM web_receipt_scoped WHERE endpoint=?1 AND request_key='synthetic-remap'",[&path],|row|Ok((row.get(0)?,row.get(1)?))).unwrap();
        assert_eq!(count,1);
        if let Some(response)=response {let value:Value=serde_json::from_str(&response).unwrap();assert_eq!(value["outcome"],"unknown");assert_eq!(value["retry"],false);}
        assert_retired(old_session).await;
        assert!(tokio::net::TcpStream::connect(web).await.is_err());
        assert!(tokio::net::TcpStream::connect(hap_address).await.is_err());
    } else {
        let (_,outcome)=http(web,&format!("/api/v1/config/activations/{candidate}"),Some(activation.clone())).await;
        assert_retired(old_session).await;
        assert_eq!(outcome["outcome"], "activated", "{outcome}");
        assert_eq!(
            http(
                web,
                &format!("/api/v1/config/activations/{candidate}"),
                Some(activation)
            )
            .await
            .0,
            200
        );
        let (_, after) = http(web, "/api/v1/modules", None).await;
        assert_eq!(after["revision"], "3");
        assert_ne!(after["transport_epoch"], before["transport_epoch"]);
        assert!(
            after["modules"]
                .as_array()
                .unwrap()
                .iter()
                .all(|m| m["fresh_channels"] == 8)
        );
        let remapped = after["modules"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["module_id"] == "module-light")
            .unwrap();
        assert_eq!(remapped["address"], "61");
        assert_eq!(remapped["slots"][0]["actual"]["level"], 44);
        assert_eq!(remapped["slots"][1]["binding"]["actions"], json!([]));
        let (_, view) = http(web, "/api/v1/devices", None).await;
        assert_eq!(view["devices"].as_array().unwrap().len(), 1);
        assert_eq!(view["devices"][0]["id"], "light");
        assert_eq!(view["devices"][0]["observed"]["level"], 44);
        let (fresh_session, fresh_graph) =
            read_hap(&identity, hap_address, controller, signing).await;
        assert_eq!(fresh_graph["accessories"].as_array().unwrap().len(),2);
        assert_eq!(bridge_ids(&fresh_graph), bridge_ids(&old_graph));
        assert_eq!(identifier_rows(&identity_path.join("identity.sqlite")),old_identifiers,"light AID/IIDs remain stable");
        // No retired light handle can leak the obsolete address's 99%.
        // Confirm the remapped value through the native device API instead.
        assert_eq!(view["devices"][0]["observed"]["level"],44,"old-address99 must not reach native state");
        drop(fresh_session);
        let stale = json!({"module_id":"module-light","scope":{"kind":"whole_module"},"revision":"1","request_id":"obsolete-query"});
        assert_eq!(
            http(web, "/api/v1/module-queries", Some(stale)).await.0,
            409
        );
        assert_eq!(http(web, "/api/v1/module-commands", Some(json!({"module_id":"module-light","channel":2,"revision":"3","request_id":"disabled-control","command":{"action":"brightness","level":80}}))).await.0, 400);
        service.await.unwrap().unwrap();
    }
    stop.store(true,Ordering::Release);
    gateway.join().unwrap();
    let preserved = identity.load_config().await.unwrap();
    assert_eq!(preserved.device_id, identity_config.device_id);
    assert_eq!(
        preserved.device_ed25519_keypair.verifying_key(),
        identity_config.device_ed25519_keypair.verifying_key()
    );
    assert!(identity.controller_is_paired(controller).unwrap());
    let store = homesense_storage::Store::open(directory.join("automation-tcp.sqlite")).unwrap();
    let active = store
        .active_bundle(ExecutionContext::Live)
        .unwrap()
        .unwrap();
    assert_eq!(active.configuration.revision(), 3);
    let saved: Value = serde_json::from_slice(&store.active().unwrap().unwrap().payload).unwrap();
    assert_eq!(saved["devices"], devices["devices"]);
    assert_eq!(
        active.automation.unwrap().timed_scenes["scene"].steps.len(),
        1
    );
    let saved_rules: Value =
        serde_json::from_slice(&store.automation_payload(3).unwrap().unwrap()).unwrap();
    assert_eq!(saved_rules["timed_scenes"], rules["timed_scenes"]);
    let sql = rusqlite::Connection::open(directory.join("automation-tcp.sqlite")).unwrap();
    assert_eq!(
        sql.query_row("SELECT count(*) FROM command_journal", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        sql.query_row("SELECT count(*) FROM scene_run", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    let frames = frames.lock().unwrap();
    assert_eq!(
        frames
            .iter()
            .filter(|(generation, _)| *generation == 0)
            .take(2)
            .map(|(_, frame)| *frame)
            .collect::<Vec<_>>(),
        vec![
            [0xae, 0x15, 0xa0, 0xf2, 0xee],
            [0xae, 0x16, 0xa0, 0xf2, 0xee]
        ]
    );
    assert!(
        frames
            .iter()
            .filter(|(generation, _)| *generation >= 1)
            .all(|(_, frame)| frame[1] != 0x15)
    );
    for address in [0x61,0x16] {
        assert!(frames.iter().any(|(generation,frame)|*generation>=1 && *frame==[0xae,address,0xa0,0xf2,0xee]),"new registered module was never queried: {address:02x}");
    }
    if omit {
        let connections:std::collections::BTreeSet<_>=frames.iter().filter_map(|(generation,_)|(*generation>=1).then_some(*generation)).collect();
        assert!(connections.len()>=2,"partial feedback must trigger further supervised rounds, not exhaust the service");
    }
    drop(frames);
    drop(sql);
    drop(store);
    drop(identity);
    std::fs::remove_dir_all(directory).unwrap();
}
