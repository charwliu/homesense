use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
const TOKEN: &str = "synthetic-away-http-token-abcdefghijklmnopqrstuvwxyz";
async fn http(
    address: std::net::SocketAddr,
    path: &str,
    body: Option<Value>,
) -> Result<(u16, Value), String> {
    http_token(address, path, body, TOKEN).await
}
async fn http_token(
    address: std::net::SocketAddr,
    path: &str,
    body: Option<Value>,
    token: &str,
) -> Result<(u16, Value), String> {
    let mut socket = tokio::net::TcpStream::connect(address)
        .await
        .map_err(|e| e.to_string())?;
    let method = if body.is_some() { "POST" } else { "GET" };
    let body = body.map(|v| v.to_string()).unwrap_or_default();
    socket.write_all(format!("{method} {path} HTTP/1.1\r\nHost: {address}\r\nOrigin: http://{address}\r\nAuthorization: Bearer {token}\r\nIdempotency-Key: synthetic-credential-once\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.map_err(|e|e.to_string())?;
    let mut bytes = vec![];
    tokio::time::timeout(Duration::from_secs(3), socket.read_to_end(&mut bytes))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    let text = String::from_utf8(bytes).map_err(|e| e.to_string())?;
    let (header, body) = text.split_once("\r\n\r\n").ok_or("invalid HTTP")?;
    Ok((
        header
            .split_whitespace()
            .nth(1)
            .ok_or("no status")?
            .parse()
            .map_err(|_| "invalid status")?,
        serde_json::from_str(body).map_err(|e| e.to_string())?,
    ))
}
#[tokio::test]
async fn visible_away_operation_flows_through_http_actor_audit_wire_feedback_and_leds() {
    let root = std::env::temp_dir().join(format!("away-http-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let bus = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let web = free.local_addr().unwrap();
    drop(free);
    let verified =
        json!({"status":"verified","evidence":"Synthetic localhost only","verified_at_unix_ms":1});
    let mut modules = vec![];
    let mut devices = vec![];
    let mut reviews = vec![];
    for (id, address, profile) in [
        ("relay", "27", "r8i8_din_pro_v2_7_7"),
        ("dimmer", "38", "ad8_din_rs485_v2_2_1"),
    ] {
        modules.push(json!({"id":id,"address":address,"profile":profile,"verification":verified}));
        let mut names = vec![];
        for channel in 1..=8 {
            let name = format!("{id}:{channel}");
            names.push(name.clone());
            devices.push(json!({"id":name,"name":"Synthetic lamp","module_id":id,"channel":channel,"enabled":true}));
        }
        reviews.push(json!({"module_id":id,"devices":names,"review_evidence":"All eight synthetic ports reviewed as lighting"}));
    }
    modules.push(json!({"id":"panel","address":"45","profile":"ts6_86_rs485_v2_6_5_6key","verification":verified,"led_mode":"manual"}));
    for channel in 1..=2 {
        devices.push(json!({"id":format!("led:{channel}"),"name":"Synthetic indicator","module_id":"panel","channel":channel,"enabled":true}));
    }
    let config = json!({"schema_version":2,"revision":1,"gateways":[{"id":"bus","endpoint":bus.local_addr().unwrap().to_string(),"modules":modules}],"devices":devices});
    let mut rules = json!({"schema_version":1,"configuration_revision":1,"variables":[],"rules":[],"away_off":{"enabled":true,"review_evidence":"Explicit synthetic Away operation","active_execution_policy":"cancel_affected_lighting","modules":reviews},"indicator_links":[{"id":"off-indicators","enabled":true,"review_evidence":"Synthetic two-indicator off link","lights":["relay:1","dimmer:1"],"leds":["led:1","led:2"],"scene":null}]});
    rules["variables"] = json!([{"id":"mode","kind":"integer","lifetime":"persistent"}]);
    rules["rules"] = json!([{"id":"synthetic-select","enabled":true,"review_evidence":"Synthetic initial selected state","source":"synthetic","trigger":{"kind":"startup_synchronized"},"actions":[{"source":"synthetic","action":{"kind":"set","variable":"mode","value":{"kind":"integer","value":1}}}]}]);
    rules["timed_scenes"]=json!([("bright",75,true),("relay-only",0,true),("off",0,false)].map(|(id,level,on)|json!({"id":id,"enabled":true,"review_evidence":"Synthetic scene","max_lateness_ms":1000,"steps":[{"source":"synthetic","offset_ms":60000,"device":"dimmer:1","command":{"kind":"level","value":level,"ramp":level>0}},{"source":"synthetic","offset_ms":60000,"device":"relay:1","command":{"kind":"switch","on":on}}]})));
    rules["scene_switch_groups"] = json!([{"id":"synthetic-mode","variable":"mode","review_evidence":"Synthetic selection and feedback reconciliation","members":[{"scene":"bright","value":1,"off_scene":"off"},{"scene":"relay-only","value":2,"off_scene":"off"}],"reconciliation":{"enabled":true,"policy":"unique_light_on_pattern","max_age_ms":1000,"review_evidence":"Synthetic feedback review"}}]);
    let frames = Arc::new(Mutex::new(vec![]));
    let controls = frames.clone();
    let expected_shutdown = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let shutdown_armed = expected_shutdown.clone();
    let server = std::thread::spawn(move || {
        let (mut socket, _) = bus.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(8)))
            .unwrap();
        let mut relay = true;
        let mut level = 75;
        loop {
            let mut frame = [0; 5];
            match socket.read_exact(&mut frame) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset
                    && shutdown_armed.load(std::sync::atomic::Ordering::Acquire) => break,
                Err(e) => panic!("synthetic gateway: {e}"),
            }
            let address = frame[1];
            if address == 0x45 {
                assert!((1..=2).contains(&frame[2]));
                assert_eq!(frame[3], 0xF2);
                controls.lock().unwrap().push(frame);
                continue;
            }
            assert!([0x27, 0x38].contains(&address));
            let value = if address == 0x27 {
                if relay { 0xF1 } else { 0xF2 }
            } else {
                level
            };
            if frame[2] == 0xA0 {
                let reply: Vec<_> = (1..=8)
                    .flat_map(|c| [0xAE, address, 0xD0 + c, value, 0xEE])
                    .collect();
                socket.write_all(&reply).unwrap();
            } else if (0xA1..=0xA8).contains(&frame[2]) {
                socket
                    .write_all(&[0xAE, address, frame[2] + 0x30, value, 0xEE])
                    .unwrap();
            } else {
                assert_eq!(
                    frame,
                    if address == 0x27 {
                        [0xAE, 0x27, 0, 0xF2, 0xEE]
                    } else {
                        [0xAE, 0x38, 0, 0, 0xEE]
                    }
                );
                controls.lock().unwrap().push(frame);
                if address == 0x27 {
                    relay = false;
                } else {
                    level = 0;
                }
                let value = if address == 0x27 { 0xF2 } else { 0 };
                let reply: Vec<_> = (1..=8)
                    .flat_map(|c| [0xAE, address, 0xD0 + c, value, 0xEE])
                    .collect();
                socket.write_all(&reply).unwrap();
            }
        }
    });
    let directory = root.clone();
    let config = serde_json::to_vec(&config).unwrap();
    let rules = serde_json::to_vec(&rules).unwrap();
    let service = tokio::spawn(async move {
        homesensed::automation_tcp::serve_managed(
            &config,
            &rules,
            &directory,
            Duration::from_secs(5),
            true,
            0,
            Duration::from_millis(20),
            homesensed::service::WebSettings {
                address: web,
                secret: TOKEN.into(),
                administrator: true,
                tls: None,
            },
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            assert!(!service.is_finished());
            if matches!(http(web, "/api/v1/devices", None).await, Ok((200, _))) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        http(web, "/api/v1/away-off", None).await.unwrap().1["available"],
        true
    );
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let (_, scenes) = http(web, "/api/v1/scenes", None).await.unwrap();
            if scenes["scenes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v["id"] == "bright" && v["selected"] == true)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();

    let (_, initial) = http(web, "/api/v1/devices", None).await.unwrap();
    let subset = initial["devices"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v["id"].as_str())
        .filter(|id| *id != "relay:8")
        .collect::<Vec<_>>();
    let (code, issued) = http(
        web,
        "/api/v1/credentials",
        Some(json!({"role":"Member","devices":subset,"scenes":[]})),
    )
    .await
    .unwrap();
    assert_eq!(code, 201, "{issued}");
    let partial = issued["secret"].as_str().unwrap();
    assert_eq!(
        http_token(web, "/api/v1/away-off", None, partial)
            .await
            .unwrap()
            .1["available"],
        false
    );
    assert_eq!(
        http_token(
            web,
            "/api/v1/away-off",
            Some(json!({"revision":"1","request_id":"restricted-away"})),
            partial
        )
        .await
        .unwrap()
        .0,
        403
    );
    assert!(frames.lock().unwrap().is_empty());
    let body = json!({"revision":"1","request_id":"away-once"});
    let (code, receipt) = http(web, "/api/v1/away-off", Some(body.clone()))
        .await
        .unwrap();
    assert_eq!(code, 202);
    assert_eq!(receipt["outcome"], "accepted");
    // Admission may accept the operation while a query owns the bus. This
    // grants no physical send lease yet; it must never pre-admit future modules.
    let initial=receipt["commands"].as_array().unwrap();
    assert!(initial.len()<=1,"at most the current module has a send lease");
    assert!(!receipt["operation"].as_str().unwrap().is_empty());
    if let Some(command)=initial.first() {
        let sql=rusqlite::Connection::open(root.join("automation-tcp.sqlite")).unwrap();
        let (module,ports):(String,i64)=sql.query_row("SELECT min(module_id),count(*) FROM module_off_effect WHERE command_id=?1",[i64::try_from(command.as_u64().unwrap()).unwrap()],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
        assert_eq!((module.as_str(),ports),("dimmer",8),"initial lease covers exactly the first module");
    }
    assert_eq!(receipt["physical_result_confirmed"], false);
    let (code, repeated) = http(web, "/api/v1/away-off", Some(body)).await.unwrap();
    assert_eq!(code, 200);
    assert_eq!(repeated, receipt);
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let (code, view) = http(web, "/api/v1/devices", None).await.unwrap();
            if code == 200 {
                let lights = view["devices"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|v| ["Switch", "Dimmer"].contains(&v["kind"].as_str().unwrap_or("")))
                    .collect::<Vec<_>>();
                if lights.len() == 16
                    && lights.iter().all(|v| {
                        v["quality"] == "Confirmed"
                            && (v["observed"]["level"] == 0 || v["observed"]["on"] == false)
                    })
                    && frames
                        .lock()
                        .unwrap()
                        .iter()
                        .filter(|f| f[1] == 0x45)
                        .count()
                        == 2
                {
                    // The physical LED proof and the read-only scene projection
                    // advance independently. Require both to settle within this
                    // same original two-second budget, rather than assuming a
                    // cross-stream atomic update after only the LED is written.
                    let (code, scenes)=http(web,"/api/v1/scenes",None).await.unwrap();
                    let selected=scenes["scenes"].as_array().unwrap().iter()
                        .filter(|v|v["id"]=="bright" || v["id"]=="relay-only")
                        .collect::<Vec<_>>();
                    if code==200 && selected.len()==2 && selected.iter().all(|v|v["selected"]==false) {break;}
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let (_, scenes) = http(web, "/api/v1/scenes", None).await.unwrap();
    assert!(
        scenes["scenes"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|v| v["id"] == "bright" || v["id"] == "relay-only")
            .all(|v| v["selected"] == false),
        "{scenes}"
    );
    // All control/feedback checks are complete; only the intentional service
    // shutdown may close a socket with unread final periodic feedback via RST.
    // The native API remains available; browser management entry points are retired.
    for path in ["/", "/app.js"] {
        let (code,body)=http(web,path,None).await.unwrap();
        assert_eq!(code,410);assert_eq!(body["error"],"native_app_required");
    }
    expected_shutdown.store(true, std::sync::atomic::Ordering::Release);
    service.await.unwrap().unwrap();
    server.join().unwrap();
    let controls = frames.lock().unwrap();
    assert_eq!(
        controls
            .iter()
            .filter(|f| f[1] != 0x45)
            .copied()
            .collect::<Vec<_>>(),
        vec![[0xAE, 0x38, 0, 0, 0xEE], [0xAE, 0x27, 0, 0xF2, 0xEE]]
    );
    assert_eq!(controls.iter().filter(|f| f[1] == 0x45).count(), 2);
    let store = homesense_storage::Store::open(root.join("automation-tcp.sqlite")).unwrap();
    let levels = store.light_restore_levels(1).unwrap();
    assert_eq!(levels.len(), 8);
    assert!(levels.values().all(|v| v.get() == 75));
    drop(store);
    let sql = rusqlite::Connection::open(root.join("automation-tcp.sqlite")).unwrap();
    assert_eq!(
        sql.query_row("SELECT count(*) FROM module_off_effect", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        16
    );
    assert_eq!(
        sql.query_row(
            "SELECT count(*) FROM command_journal WHERE intent=x'01080000' AND status='written'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        2
    );
    drop(sql);
    std::fs::remove_dir_all(root).unwrap();
}


#[tokio::test]
async fn shared_producer_without_hap_or_indicators_and_away_preserve_settled_brightness() {
    for combined in [false, true] {
        let root = std::env::temp_dir().join(format!("away-no-projection-{}", uuid::Uuid::new_v4()));
        let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let web = free.local_addr().unwrap();
        drop(free);
        let mut modules = vec![];
        let mut devices = vec![];
        let mut reviews = vec![];
        for (id, address, profile) in [("relay", "17", "r8i8_din_pro_v2_7_7"), ("dimmer", "28", "ad8_din_rs485_v2_2_1")] {
            modules.push(json!({"id":id,"address":address,"profile":profile,"verification":{"status":"simulated"}}));
            let mut names = vec![];
            for channel in 1..=8 {
                let name = format!("{id}:{channel}");
                names.push(name.clone());
                let mut device = json!({"id":name,"name":"Synthetic light","module_id":id,"channel":channel,"enabled":true});
                if id == "dimmer" {
                    device["default_on_level"] = json!(100);
                    if combined { device["supply"] = json!({"module_id":"relay","channel":channel}); }
                }
                devices.push(device);
            }
            reviews.push(json!({"module_id":id,"devices":names,"review_evidence":"Eight synthetic lighting outputs"}));
        }
        let config = serde_json::to_vec(&json!({"schema_version":2,"revision":1,"gateways":[{"id":"simulation","endpoint":"127.0.0.1:1","modules":modules}],"devices":devices})).unwrap();
        let rules = serde_json::to_vec(&json!({"schema_version":1,"configuration_revision":1,"variables":[],"rules":[],"away_off":{"enabled":true,"review_evidence":"Synthetic no-projection case","active_execution_policy":"cancel_affected_lighting","modules":reviews}})).unwrap();
        let directory = root.clone();
        let service = tokio::spawn(async move {
            homesensed::service::serve_web_simulation(&config, &rules, &directory, Duration::from_secs(4), homesensed::service::WebSimulation {address:web,secret:TOKEN.into(),administrator:true,tls:None}).await
        });
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                assert!(!service.is_finished());
                if matches!(http(web, "/api/v1/devices", None).await, Ok((200, _))) {break;}
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }).await.unwrap();
        let (code, seed) = http(web, "/api/v1/device-commands", Some(json!({"device":"dimmer:1","request_id":"seed37","command":{"action":"brightness","level":37}}))).await.unwrap();
        assert_eq!(code,202,"{seed}");
        // Wait for a fresh physical sample strictly after both completed writes,
        // rather than assuming the first echo is settled feedback.
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let sql = rusqlite::Connection::open(root.join("automation-simulation.sqlite")).unwrap();
                let mut last_written = 0i64;
                let mut complete = true;
                for command in seed["commands"].as_array().unwrap() {
                    let (status, at):(String,i64) = sql.query_row("SELECT status,updated_ms FROM command_journal WHERE command_id=?1",[i64::try_from(command.as_u64().unwrap()).unwrap()],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
                    complete &= status == "written";
                    last_written = last_written.max(at);
                }
                drop(sql);
                // Read raw physical slots independently of the shared publication.
                // The server-owned producer must also remember confirmed
                // brightness with zero HAP or indicator consumers.
                let (_, view) = http(web, "/api/v1/modules", None).await.unwrap();
                let modules = view["modules"].as_array().unwrap();
                let slot = |module: &str| &modules.iter().find(|m|m["module_id"]==module).unwrap()["slots"][0]["binding"];
                let fresh = |sample: &serde_json::Value| sample["quality"]=="confirmed"
                    && sample["observed_ms"].as_str().and_then(|s|s.parse::<i64>().ok()).is_some_and(|at|at>last_written);
                let dimmer = slot("dimmer");
                let supply_ready = !combined || (fresh(slot("relay")) && slot("relay")["actual"]["on"]==true);
                let saved=homesense_storage::Store::open(root.join("automation-simulation.sqlite")).unwrap().light_restore_levels(1).unwrap();
                let remembered=saved.get(&homesense_core::DeviceId::new("dimmer:1").unwrap()).is_some_and(|v|v.get()==37);
                if complete && fresh(dimmer) && dimmer["actual"]==json!({"level":37,"on":true}) && supply_ready && remembered {break;}
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }).await.unwrap();
        assert_eq!(homesense_storage::Store::open(root.join("automation-simulation.sqlite")).unwrap().light_restore_levels(1).unwrap().get(&homesense_core::DeviceId::new("dimmer:1").unwrap()).map(|p|p.get()),Some(37),"the independent producer captures settled feedback without client projection");
        let (code, receipt) = http(web, "/api/v1/away-off", Some(json!({"revision":"1","request_id":"off-once"}))).await.unwrap();
        assert_eq!(code,202,"{receipt}");
        let initial=receipt["commands"].as_array().unwrap();
        assert!(initial.len()<=1,"a busy bus may defer its first lease, never admit a future module");
        assert!(!receipt["operation"].as_str().unwrap().is_empty());
        if let Some(command)=initial.first() {
            let sql=rusqlite::Connection::open(root.join("automation-simulation.sqlite")).unwrap();
            let(module,ports):(String,i64)=sql.query_row("SELECT min(module_id),count(*) FROM module_off_effect WHERE command_id=?1",[i64::try_from(command.as_u64().unwrap()).unwrap()],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
            assert_eq!((module.as_str(),ports),("dimmer",8));
        }
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let (_, view) = http(web, "/api/v1/devices", None).await.unwrap();
                if view["devices"].as_array().unwrap().iter().all(|d| d["quality"]=="Confirmed" && d["observed"]["on"]==false) {break;}
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }).await.unwrap();
        let saved = homesense_storage::Store::open(root.join("automation-simulation.sqlite")).unwrap().light_restore_levels(1).unwrap();
        assert_eq!(saved.get(&homesense_core::DeviceId::new("dimmer:1").unwrap()).map(|p|p.get()),Some(37),"Away must capture settled feedback even without HAP or indicator projections");
        let (code, on) = http(web, "/api/v1/device-commands", Some(json!({"device":"dimmer:1","request_id":"restore37","command":{"action":"power","on":true}}))).await.unwrap();
        assert_eq!(code,202,"{on}");
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let (_, view) = http(web, "/api/v1/devices", None).await.unwrap();
                let light = view["devices"].as_array().unwrap().iter().find(|d|d["id"]=="dimmer:1").unwrap();
                if light["quality"]=="Confirmed" && light["observed"]==json!({"level":37,"on":true}) {break;}
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }).await.unwrap();
        service.await.unwrap().unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
}
