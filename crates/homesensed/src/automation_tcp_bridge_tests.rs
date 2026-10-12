use super::*;
use super::fault_tests::{free_address,http};
use tokio::io::{AsyncReadExt,AsyncWriteExt};
#[tokio::test]
async fn multiple_bridges_rebuild_and_isolate_occupied_listener() {
    let root=std::env::temp_dir().join(format!("homesense-multiple-{}",Uuid::new_v4()));
    let bus=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut devices:serde_json::Value=serde_json::from_slice(include_bytes!("../../../config/simulation.json")).unwrap();
    devices["gateways"][0]["endpoint"]=bus.local_addr().unwrap().to_string().into();
    devices["gateways"][0]["modules"][0]["verification"]=serde_json::json!({"status":"verified","evidence":"synthetic loopback fixture","verified_at_unix_ms":1});
    let mut second=devices["devices"][0].clone();second["id"]="second".into();second["channel"]=2.into();devices["devices"].as_array_mut().unwrap().push(second);
    let mut rules=serde_json::json!({"schema_version":1,"configuration_revision":1,"variables":[],"rules":[]});
    let gateway=tokio::spawn(async move {
        loop {
            let (mut socket,_)=bus.accept().await.unwrap();
            tokio::spawn(async move {
                loop {
                    let mut frame=[0;5];if socket.read_exact(&mut frame).await.is_err(){break;}
                    assert!([0xa1,0xa2].contains(&frame[2]),"unexpected control write: {frame:?}");
                    if socket.write_all(&[0xae,0x15,frame[2]+0x30,0,0xee]).await.is_err(){break;}
                }
            });
        }
    });
    let web=free_address();let first=free_address();
    let reserved=std::net::TcpListener::bind("127.0.0.1:0").unwrap();let second=reserved.local_addr().unwrap();
    let plan=serde_json::to_vec(&serde_json::json!({"schema_version":1,"bridges":[
        {"id":"one","name":"First","address":first,"lights":["simulation:light"],"scenes":[],"motors":[]},
        {"id":"two","name":"Second","address":second,"lights":["second"],"scenes":[],"motors":[]}
    ]})).unwrap();
    let (publisher,receiver)=crate::automation_publication::channel();
    let service={let directory=root.clone();let devices=serde_json::to_vec(&devices).unwrap();let rules=serde_json::to_vec(&rules).unwrap();let publisher=publisher.clone();tokio::spawn(async move {
        serve_inner(&devices,&rules,&directory,Duration::from_secs(5),true,0,Some(receiver),Duration::from_millis(20),Some(crate::service::WebSettings{address:web,secret:"abcdefghijklmnopqrstuvwxyz0123456789".into(),administrator:true,tls:None}),Some(publisher),None,Some(plan),None).await
    })};
    tokio::time::timeout(Duration::from_secs(3),async {loop {
        assert!(!service.is_finished());
        if let Ok((200,value))=http(web,"/api/v1/diagnostics",None).await {
            assert_eq!(value["hap"],"degraded");assert_eq!(value["hap_bridges"]["one"],"listening");assert_eq!(value["hap_bridges"]["two"],"degraded");break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }}).await.unwrap();
    let mut old=tokio::net::TcpStream::connect(first).await.unwrap();
    devices["revision"]=2.into();rules["configuration_revision"]=2.into();
    let mut invalid=devices.clone();invalid["devices"][0]["enabled"]=false.into();
    assert!(publisher.publish(serde_json::to_vec(&invalid).unwrap(),serde_json::to_vec(&rules).unwrap(),1).await.is_err());
    assert_eq!(http(web,"/api/v1/diagnostics",None).await.unwrap().1["revision"],"1");
    drop(reserved);
    assert_eq!(publisher.publish(serde_json::to_vec(&devices).unwrap(),serde_json::to_vec(&rules).unwrap(),1).await.unwrap(),2);
    let mut byte=[0];assert!(matches!(tokio::time::timeout(Duration::from_secs(1),old.read(&mut byte)).await.unwrap(),Ok(0)|Err(_)));
    tokio::time::timeout(Duration::from_secs(2),async {loop {
        if let Ok((200,value))=http(web,"/api/v1/diagnostics",None).await {
            if value["revision"]=="2" && value["hap"]=="listening" {assert_eq!(value["hap_bridges"]["two"],"listening");break;}
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }}).await.unwrap();
    let one=tokio::net::TcpStream::connect(first).await.unwrap();let two=tokio::net::TcpStream::connect(second).await.unwrap();
    service.await.unwrap().unwrap();drop((one,two));gateway.abort();let _=gateway.await;
    assert!(tokio::net::TcpStream::connect(first).await.is_err());assert!(tokio::net::TcpStream::connect(second).await.is_err());
    std::fs::remove_dir_all(root).unwrap();
}
