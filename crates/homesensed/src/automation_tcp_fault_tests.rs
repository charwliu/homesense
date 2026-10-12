use super::*;
use tokio::io::{AsyncReadExt,AsyncWriteExt};

pub(crate) async fn http(address:std::net::SocketAddr, path:&str, body:Option<serde_json::Value>)->Result<(u16,serde_json::Value),String> {
    let mut socket=tokio::net::TcpStream::connect(address).await.map_err(|e|e.to_string())?;
    let body=body.map(|v|v.to_string());
    let method=if body.is_some(){"POST"}else{"GET"};
    let body=body.unwrap_or_default();
    socket.write_all(format!("{method} {path} HTTP/1.1\r\nHost: {address}\r\nOrigin: http://{address}\r\nAuthorization: Bearer abcdefghijklmnopqrstuvwxyz0123456789\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.map_err(|e|e.to_string())?;
    let mut bytes=vec![];
    socket.read_to_end(&mut bytes).await.map_err(|e|e.to_string())?;
    let text=String::from_utf8(bytes).unwrap();let (header,body)=text.split_once("\r\n\r\n").unwrap();
    Ok((header.split_whitespace().nth(1).unwrap().parse().unwrap(),serde_json::from_str(body).unwrap()))
}
pub(crate) fn free_address()->std::net::SocketAddr {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap()
}
#[tokio::test]
async fn hap_task_failure_preserves_web_delayed_scene_and_single_startup() {run(false).await;}
#[tokio::test]
async fn occupied_tcp_hap_port_preserves_web_and_single_startup() {run(true).await;}
async fn run(occupied:bool) {
    let root=std::env::temp_dir().join(format!("homesense-hap-fault-{}",Uuid::new_v4()));
    let bus=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut devices:serde_json::Value=serde_json::from_slice(include_bytes!("../../../config/simulation.json")).unwrap();
    devices["gateways"][0]["endpoint"]=bus.local_addr().unwrap().to_string().into();
    devices["gateways"][0]["modules"][0]["verification"]=serde_json::json!({"status":"verified","evidence":"synthetic loopback fixture","verified_at_unix_ms":1});
    let rules=serde_json::json!({"schema_version":1,"configuration_revision":1,"variables":[],"rules":[{"id":"boot","enabled":true,"review_evidence":"synthetic","source":"fixture","trigger":{"kind":"startup_synchronized"},"actions":[{"source":"fixture","action":{"kind":"command","device":"simulation:light","command":{"kind":"level","value":30,"ramp":false}}}]}],"timed_scenes":[{"id":"later","enabled":true,"review_evidence":"synthetic","max_lateness_ms":1000,"steps":[{"source":"fixture","offset_ms":1500,"device":"simulation:light","command":{"kind":"level","value":60,"ramp":false}}]}]});
    let writes=std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));let observed=writes.clone();
    let gateway=tokio::spawn(async move {
        let (mut socket,_)=bus.accept().await.unwrap();let mut level=0;
        loop {
            let mut frame=[0;5];if socket.read_exact(&mut frame).await.is_err(){break;}
            match frame[2] {0xa1=>assert_eq!(frame,[0xae,0x15,0xa1,0xf2,0xee]),0x01=>{level=frame[3];observed.lock().unwrap().push(level);},_=>panic!("unexpected frame")}
            if socket.write_all(&[0xae,0x15,0xd1,level,0xee]).await.is_err(){break;}
        }
    });
    let web=free_address();let reserved=occupied.then(||std::net::TcpListener::bind("127.0.0.1:0").unwrap());let hap=reserved.as_ref().map_or_else(free_address,|listener|listener.local_addr().unwrap());let directory=root.clone();
    let (publisher,receiver)=crate::automation_publication::channel();
    let service=tokio::spawn(async move {
        serve_inner(&serde_json::to_vec(&devices).unwrap(),&serde_json::to_vec(&rules).unwrap(),&directory,Duration::from_secs(4),true,0,Some(receiver),Duration::from_millis(20),Some(crate::service::WebSettings{address:web,secret:"abcdefghijklmnopqrstuvwxyz0123456789".into(),administrator:true,tls:None}),Some(publisher),Some(crate::hap_endpoint::Settings{address:hap,scenes:vec!["later".into()],motors:vec![]}),(!occupied).then_some(Duration::from_millis(800))).await
    });
    tokio::time::timeout(Duration::from_secs(3),async {
        loop {
            assert!(!service.is_finished(),"service ended before readiness");
            if let Ok((200,_))=http(web,"/api/v1/devices",None).await {break;}
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.unwrap();
    assert_eq!(http(web,"/api/v1/diagnostics",None).await.unwrap().1["hap"],if occupied {"degraded"}else{"listening"});
    // Keep a real HAP session open and submit delayed work before the fault.
    let mut old_hap=if occupied {None}else{Some(tokio::net::TcpStream::connect(hap).await.unwrap())};
    let accepted=http(web,"/api/v1/scene-actions",Some(serde_json::json!({"scene":"later","request_id":"before-fault"}))).await.unwrap();
    assert_eq!(accepted.1["outcome"],"accepted");
    let mut byte=[0];
    if let Some(socket)=&mut old_hap {
        let closed=tokio::time::timeout(Duration::from_secs(2),socket.read(&mut byte)).await.unwrap();
        assert!(matches!(closed,Ok(0)|Err(_)),"old HAP session remained open");
    }
    assert!(!service.is_finished());
    if !occupied {assert!(tokio::net::TcpStream::connect(hap).await.is_err());}
    assert_eq!(http(web,"/api/v1/devices",None).await.unwrap().0,200);
    assert_eq!(http(web,"/health/ready",None).await.unwrap().0,200);
    tokio::time::timeout(Duration::from_secs(1),async {
        loop {
            if http(web,"/api/v1/diagnostics",None).await.unwrap().1["hap"]=="degraded" {break;}
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2),async {
        while !writes.lock().unwrap().contains(&60) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.unwrap();
    // Wait for the first run to finish: repeated scene activation intentionally
    // replaces pending actions, which is unrelated to endpoint isolation.
    // A new accepted action after degradation uses the original runtime too.
    let accepted=http(web,"/api/v1/scene-actions",Some(serde_json::json!({"scene":"later","request_id":"after-fault"}))).await.unwrap();
    assert_eq!(accepted.1["outcome"],"accepted");
    service.await.unwrap().unwrap();gateway.await.unwrap();
    let writes=writes.lock().unwrap();
    assert_eq!(writes.iter().filter(|&&value|value==30).count(),1,"startup replayed");
    assert_eq!(writes.iter().filter(|&&value|value==60).count(),2,"delayed work lost or replayed");
    drop(writes);
    std::fs::remove_dir_all(root).unwrap();
}
