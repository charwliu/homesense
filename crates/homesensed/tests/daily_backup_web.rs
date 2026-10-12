use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn read(address: std::net::SocketAddr, path: &str, authenticated: bool) -> (u16, serde_json::Value) {
    let mut socket=tokio::net::TcpStream::connect(address).await.unwrap();
    let auth=if authenticated {"Authorization: Bearer abcdefghijklmnopqrstuvwxyz0123456789\r\n"} else {""};
    socket.write_all(format!("GET {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n{auth}\r\n").as_bytes()).await.unwrap();
    let mut bytes=vec![];tokio::time::timeout(Duration::from_secs(3),socket.read_to_end(&mut bytes)).await.unwrap().unwrap();
    let text=String::from_utf8(bytes).unwrap();let (head,body)=text.split_once("\r\n\r\n").unwrap();
    (head.split_whitespace().nth(1).unwrap().parse().unwrap(),serde_json::from_str(body).unwrap())
}

#[tokio::test]
async fn backup_failure_is_visible_to_administrator_while_device_runtime_remains_ready() { verify(true,true).await; }
#[tokio::test]
async fn successful_backup_monitor_is_not_exposed_to_member() { verify(false,false).await; }

async fn verify(administrator:bool,broken:bool) {
    let root=std::env::temp_dir().join(format!("backup-web-{}",uuid::Uuid::new_v4()));
    let day=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()/86400;
    let snapshot=root.join(format!("daily-backups/automation-simulation.sqlite/daily-{day}.sqlite"));
    if broken {std::fs::create_dir_all(snapshot.parent().unwrap()).unwrap();std::fs::write(&snapshot,b"preserved invalid snapshot").unwrap();}
    let listener=std::net::TcpListener::bind("127.0.0.1:0").unwrap();let address=listener.local_addr().unwrap();drop(listener);
    let directory=root.clone();
    let service=tokio::spawn(async move {
        homesensed::service::serve_web_simulation(include_bytes!("../../../config/simulation.json"),br#"{"schema_version":1,"configuration_revision":1,"variables":[],"rules":[]}"#,&directory,Duration::from_secs(3),homesensed::service::WebSettings{address,secret:"abcdefghijklmnopqrstuvwxyz0123456789".into(),administrator,tls:None}).await
    });
    tokio::time::timeout(Duration::from_secs(2),async {
        while tokio::net::TcpStream::connect(address).await.is_err(){tokio::time::sleep(Duration::from_millis(10)).await;}
    }).await.unwrap();
    assert_eq!(read(address,"/api/v1/diagnostics",false).await.0,401);
    if administrator {
        let report=tokio::time::timeout(Duration::from_secs(2),async {
            loop {let (status,report)=read(address,"/api/v1/diagnostics",true).await;assert_eq!(status,200);
                if report["daily_backup"]["phase"]=="failed" {break report;}tokio::time::sleep(Duration::from_millis(10)).await;}
        }).await.unwrap();
        assert_eq!(report["daily_backup"]["failure"],"snapshot_verification");assert!(report["daily_backup"]["last_success_utc_day"].is_null());
        assert_eq!(report["ready"],true);assert_eq!(report["storage_healthy"],true);assert!(!report.to_string().contains(root.to_str().unwrap()));
        assert_eq!(read(address,"/health/ready",true).await.0,200);
        let (status,devices)=read(address,"/api/v1/devices",true).await;assert_eq!(status,200);assert!(!devices["devices"].as_array().unwrap().is_empty());
    } else {assert_eq!(read(address,"/api/v1/diagnostics",true).await.0,403);}
    service.await.unwrap().unwrap();
    if broken {assert_eq!(std::fs::read(snapshot).unwrap(),b"preserved invalid snapshot");}
    else {assert!(homesense_storage::backup::inspect(&snapshot,homesense_core::ExecutionContext::Simulation).is_ok());}
    let store=homesense_storage::Store::open(root.join("automation-simulation.sqlite")).unwrap();assert_eq!(store.active().unwrap().unwrap().revision,1);drop(store);
    std::fs::remove_dir_all(root).unwrap();
}
