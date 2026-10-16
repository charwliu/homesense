//! Synthetic localhost regression for EOF while a periodic read is outstanding.
#[path="support/runtime_outage.rs"]
mod outage;
use std::{
    io::{Read, Write},
    time::Duration,
};

#[tokio::test]
async fn periodic_query_eof_uses_supervised_disconnect_path_without_output_or_replay() {
    let root = std::env::temp_dir().join(format!("periodic-query-eof-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let bus = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let mut devices: serde_json::Value =
        serde_json::from_slice(include_bytes!("../../../config/simulation.json")).unwrap();
    devices["refresh_policy"]=serde_json::json!({"idle_interval_ms":500,"active_interval_ms":100,"backoff_ms":[200,300],"quiet_rounds_per_step":2,"round_budget_ms":3500,"observation_grace_ms":1000});
    devices["gateways"][0]["endpoint"] = bus.local_addr().unwrap().to_string().into();
    devices["gateways"][0]["modules"][0]["verification"] = serde_json::json!({"status":"verified","evidence":"synthetic loopback only","verified_at_unix_ms":1});
    let (eof,wait_eof)=tokio::sync::oneshot::channel();
    let gateway = std::thread::spawn(move || {
        let (mut socket, _) = bus.accept().unwrap();
        socket.set_read_timeout(Some(Duration::from_secs(6))).unwrap();
        let mut frame = [0; 5];let mut queries=vec![];
        socket.read_exact(&mut frame).unwrap();
        assert_eq!(frame, [0xae, 0x15, 0xa0, 0xf2, 0xee]);queries.push(frame);
        for channel in 1..=8 {socket.write_all(&[0xae, 0x15, 0xd0 + channel, 42, 0xee]).unwrap();}
        socket.read_exact(&mut frame).unwrap();
        assert_eq!(frame, [0xae, 0x15, 0xa0, 0xf2, 0xee]);queries.push(frame);
        // EOF is consumed by the pending-read path before normal loop receive.
        drop(socket);drop(bus);let _=eof.send(());queries
    });
    let web=std::net::TcpListener::bind("127.0.0.1:0").unwrap();let web_address=web.local_addr().unwrap();drop(web);
    let input=serde_json::to_vec(&devices).unwrap();let directory=root.clone();
    let started=std::time::Instant::now();
    let service=tokio::spawn(async move {homesensed::automation_tcp::serve_managed(
        &input,br#"{"schema_version":1,"configuration_revision":1,"variables":[],"rules":[]}"#,
        &directory,Duration::from_secs(6),true,0,Duration::from_millis(20),
        homesensed::service::WebSettings{address:web_address,secret:"abcdefghijklmnopqrstuvwxyz0123456789".into(),administrator:true,tls:None},
    ).await});
    tokio::time::timeout(Duration::from_secs(5),wait_eof).await.unwrap().unwrap();
    let token="abcdefghijklmnopqrstuvwxyz0123456789";
    outage::wait_unavailable(web_address,token,"1",&service).await;
    outage::assert_gated(web_address,token,"1","simulation:light").await;
    outage::assert_empty_journal(&root);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!service.is_finished(),"EOF must not consume the six-second service window");
    outage::assert_gated(web_address,token,"1","simulation:light").await;
    tokio::time::timeout(Duration::from_secs(8),service).await.unwrap().unwrap().unwrap();
    assert!(started.elapsed()>=Duration::from_secs(6),"service ended before its original deadline");
    assert!(tokio::net::TcpStream::connect(web_address).await.is_err());
    assert_eq!(gateway.join().unwrap(),vec![[0xae,0x15,0xa0,0xf2,0xee];2]);
    let db = rusqlite::Connection::open(root.join("automation-tcp.sqlite")).unwrap();
    assert_eq!(
        db.query_row("SELECT count(*) FROM command_journal", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    drop(db);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn continuously_due_reads_yield_publication_between_completed_queries() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    let root =
        std::env::temp_dir().join(format!("periodic-query-handoff-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let bus = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    bus.set_nonblocking(true).unwrap();
    let modules=(1..=12).map(|address|serde_json::json!({"address":format!("{address:02X}"),"profile":if address<=7 {"r8i8_din_pro_v2_7_7"}else{"ad8_din_rs485_v2_2_1"},"verification":{"status":"verified","evidence":"synthetic loopback only","verified_at_unix_ms":1}})).collect::<Vec<_>>();
    let lights=(0..96).map(|i|serde_json::json!({"id":format!("lamp{i:03}"),"name":"anonymous","gateway":"bus","address":format!("{:02X}",i/8+1),"channel":i%8+1,"enabled":true})).collect::<Vec<_>>();
    let devices = serde_json::json!({"schema_version":1,"revision":1,"refresh_policy":serde_json::json!({"idle_interval_ms":500,"active_interval_ms":100,"backoff_ms":[200,300],"quiet_rounds_per_step":2,"round_budget_ms":3500,"observation_grace_ms":1000}),"gateways":[{"id":"bus","endpoint":bus.local_addr().unwrap().to_string(),"modules":modules}],"devices":lights});
    let rules = serde_json::json!({"schema_version":1,"configuration_revision":1,"variables":[],"rules":[]});
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = stop.clone();
    let expected_close = Arc::new([AtomicBool::new(false), AtomicBool::new(false)]);
    let close_armed = expected_close.clone();
    let (query_seen, query_wait) = tokio::sync::oneshot::channel();
    let gateway = std::thread::spawn(move || {
        let mut query_seen = Some(query_seen);
        let mut workers = vec![];
        let start = std::time::Instant::now();
        while !stopped.load(Ordering::Acquire) && start.elapsed() < Duration::from_secs(12) {
            let (mut socket, _) = match bus.accept() {
                Ok(socket) => socket,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(2));
                    continue;
                }
                Err(e) => panic!("synthetic accept: {e}"),
            };
            let connection = workers.len();
            let stopped = stopped.clone();
            let close_armed = close_armed.clone();
            let mut seen = if connection == 0 {
                query_seen.take()
            } else {
                None
            };
            workers.push(std::thread::spawn(move || {
                socket
                    .set_read_timeout(Some(Duration::from_millis(100)))
                    .unwrap();
                let mut queries = 0;
                loop {
                    let mut frame = [0; 5];
                    match socket.read_exact(&mut frame) {
                        Ok(()) => {}
                        Err(e)
                            if matches!(
                                e.kind(),
                                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                            ) =>
                        {
                            if stopped.load(Ordering::Acquire) {
                                break;
                            }
                            continue;
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                        Err(e)
                            if e.kind() == std::io::ErrorKind::ConnectionReset
                                && close_armed[connection].load(Ordering::Acquire) =>
                        {
                            break;
                        }
                        Err(e) => panic!("synthetic read: {e}"),
                    }
                    assert_eq!(frame[0], 0xae);
                    assert_eq!(frame[3..], [0xf2, 0xee]);
                    if connection == 0 && queries >= 61 {
                        if let Some(seen) = seen.take() {
                            let _ = seen.send(());
                        }
                        // Longer than the 40ms polling interval, so the next
                        // read remains due whenever this response completes.
                        std::thread::sleep(Duration::from_millis(60));
                    }
                    queries += 1;
                    let channels: Vec<u8> = if frame[1] <= 7 {
                        assert!((0xa1..=0xa8).contains(&frame[2]));
                        vec![frame[2] - 0xa0]
                    } else {
                        assert_eq!(frame[2], 0xa0);
                        (1..=8).collect()
                    };
                    for channel in channels {
                        let value = if frame[1] <= 7 { 0xf1 } else { 42 };
                        if socket
                            .write_all(&[0xae, frame[1], 0xd0 + channel, value, 0xee])
                            .is_err()
                        {
                            return queries;
                        }
                    }
                }
                queries
            }));
        }
        workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>()
    });
    let (publisher, receiver) = homesensed::automation_publication::channel();
    let device_bytes = serde_json::to_vec(&devices).unwrap();
    let rule_bytes = serde_json::to_vec(&rules).unwrap();
    let mut candidate = devices.clone();
    candidate["revision"] = 2.into();
    let mut next_rules = rules.clone();
    next_rules["configuration_revision"] = 2.into();
    let service = homesensed::automation_tcp::serve_with_publications(
        &device_bytes,
        &rule_bytes,
        &root,
        Duration::from_secs(5),
        true,
        0,
        Some(receiver),
    );
    let publication = async {
        tokio::time::timeout(Duration::from_secs(5), query_wait)
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())?;
        // Closing the old generation is intentional only after this request.
        expected_close[0].store(true, Ordering::Release);
        let result = tokio::time::timeout(
            Duration::from_secs(3),
            publisher.publish(
                serde_json::to_vec(&candidate).unwrap(),
                serde_json::to_vec(&next_rules).unwrap(),
                1,
            ),
        )
        .await
        .map_err(|e| e.to_string())?;
        if result.is_ok() {
            // Publication has synchronized the replacement; normal service
            // expiry may leave a final periodic reply unread on this stream.
            expected_close[1].store(true, Ordering::Release);
        }
        result
    };
    let (served, published) = tokio::join!(service, publication);
    stop.store(true, Ordering::Release);
    let queries = gateway.join().unwrap();
    assert_eq!(published.unwrap(), 2);
    served.unwrap();
    assert_eq!(queries.len(), 2);
    assert!(queries[0] > 61 && queries[1] >= 61);
    let db = rusqlite::Connection::open(root.join("automation-tcp.sqlite")).unwrap();
    assert_eq!(
        db.query_row("SELECT count(*) FROM command_journal", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    drop(db);
    std::fs::remove_dir_all(root).unwrap();
}
