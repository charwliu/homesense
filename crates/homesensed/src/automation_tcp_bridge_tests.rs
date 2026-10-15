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
                    assert!([0xa0,0xa1,0xa2].contains(&frame[2]),"unexpected control write: {frame:?}");
                    assert_eq!(frame[3],0xf2);
                    let channels:Vec<_>=if frame[2]==0xa0 {(1..=8).collect()} else {vec![frame[2]-0xa0]};
                    let replies:Vec<_>=channels.into_iter().flat_map(|c|[0xae,0x15,0xd0+c,0,0xee]).collect();
                    if socket.write_all(&replies).await.is_err(){break;}
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
        if let Ok((200,value))=http(web,"/api/v1/diagnostics",None).await
            && value["revision"]=="2" && value["hap"]=="listening" {assert_eq!(value["hap_bridges"]["two"],"listening");break;}
        tokio::time::sleep(Duration::from_millis(10)).await;
    }}).await.unwrap();
    let one=tokio::net::TcpStream::connect(first).await.unwrap();let two=tokio::net::TcpStream::connect(second).await.unwrap();
    service.await.unwrap().unwrap();drop((one,two));gateway.abort();let _=gateway.await;
    assert!(tokio::net::TcpStream::connect(first).await.is_err());assert!(tokio::net::TcpStream::connect(second).await.is_err());
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn committed_publication_offline_keeps_diagnostics_and_returns_explicit_revision() {
    let root=std::env::temp_dir().join(format!("homesense-offline-publication-{}",Uuid::new_v4()));
    let bus=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut devices:serde_json::Value=serde_json::from_slice(include_bytes!("../../../config/simulation.json")).unwrap();
    devices["gateways"][0]["endpoint"]=bus.local_addr().unwrap().to_string().into();
    devices["gateways"][0]["modules"][0]["verification"]=serde_json::json!({"status":"verified","evidence":"synthetic loopback fixture","verified_at_unix_ms":1});
    let gateway=tokio::spawn(async move {
        let (mut socket,_)=bus.accept().await.unwrap();
        loop {
            let mut frame=[0;5];if socket.read_exact(&mut frame).await.is_err(){break;}
            assert_eq!(frame,[0xae,0x15,0xa0,0xf2,0xee],"unexpected control write");
            let replies:Vec<_>=(1..=8).flat_map(|c|[0xae,0x15,0xd0+c,0,0xee]).collect();
            if socket.write_all(&replies).await.is_err(){break;}
        }
    });
    let mut rules=serde_json::json!({"schema_version":1,"configuration_revision":1,"variables":[],"rules":[]});
    let web=free_address();
    // Hold an unlistening port throughout: recovery must never become ready.
    let offline=tokio::net::TcpSocket::new_v4().unwrap();
    offline.bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let offline_address=offline.local_addr().unwrap();
    let (publisher,receiver)=crate::automation_publication::channel();
    let service={let directory=root.clone();let devices=serde_json::to_vec(&devices).unwrap();let rules=serde_json::to_vec(&rules).unwrap();let publisher=publisher.clone();tokio::spawn(async move {
        serve_inner(&devices,&rules,&directory,Duration::from_secs(5),true,0,Some(receiver),Duration::from_millis(20),Some(crate::service::WebSettings{address:web,secret:"abcdefghijklmnopqrstuvwxyz0123456789".into(),administrator:true,tls:None}),Some(publisher),None,None,None).await
    })};
    tokio::time::timeout(Duration::from_secs(3),async {loop {
        assert!(!service.is_finished());
        if let Ok((200,value))=http(web,"/api/v1/diagnostics",None).await && value["gateway_ready"]==true {break;}
        tokio::time::sleep(Duration::from_millis(10)).await;
    }}).await.unwrap();
    devices["revision"]=2.into();devices["gateways"][0]["endpoint"]=offline_address.to_string().into();rules["configuration_revision"]=2.into();
    let publishing=tokio::spawn(async move {publisher.publish(serde_json::to_vec(&devices).unwrap(),serde_json::to_vec(&rules).unwrap(),1).await});
    tokio::time::timeout(Duration::from_secs(2),async {loop {
        assert!(!service.is_finished());
        if let Ok((200,value))=http(web,"/api/v1/diagnostics",None).await && value["revision"]=="2" {
            assert_eq!(value["gateway_ready"],false);assert_eq!(value["ready"],false);break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }}).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!service.is_finished(),"offline transport must preserve the established listener");
    assert!(!publishing.is_finished(),"publication must await fresh feedback or the service deadline");
    assert_eq!(homesense_storage::Store::open(root.join("automation-tcp.sqlite")).unwrap().active().unwrap().unwrap().revision,2);
    service.await.unwrap().unwrap();
    let error=publishing.await.unwrap().unwrap_err();
    assert!(error.contains("committed revision 2") && error.contains("feedback unavailable"),"{error}");
    assert!(!error.contains("response lost"),"{error}");
    gateway.await.unwrap();drop(offline);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn postcommit_bridge_rebuild_failure_completes_publication_with_committed_revision() {
    let root=std::env::temp_dir().join(format!("homesense-activation-failure-{}",Uuid::new_v4()));
    let bus=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut devices:serde_json::Value=serde_json::from_slice(include_bytes!("../../../config/simulation.json")).unwrap();
    devices["gateways"][0]["endpoint"]=bus.local_addr().unwrap().to_string().into();
    devices["gateways"][0]["modules"][0]["verification"]=serde_json::json!({"status":"verified","evidence":"synthetic loopback fixture","verified_at_unix_ms":1});
    let gateway=tokio::spawn(async move {
        let mut workers=tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                result=bus.accept()=>{
                    let (mut socket,_)=result.unwrap();
                    workers.spawn(async move {loop {
                        let mut frame=[0;5];if socket.read_exact(&mut frame).await.is_err(){break;}
                        assert_eq!(frame,[0xae,0x15,0xa0,0xf2,0xee],"unexpected control write");
                        let replies:Vec<_>=(1..=8).flat_map(|c|[0xae,0x15,0xd0+c,0,0xee]).collect();
                        if socket.write_all(&replies).await.is_err(){break;}
                    }});
                },
                result=workers.join_next(),if !workers.is_empty()=>{result.unwrap().unwrap();},
            }
        }
    });
    let mut rules=serde_json::json!({"schema_version":1,"configuration_revision":1,"variables":[],"rules":[]});
    let web=free_address();let hap=free_address();
    let plan=serde_json::to_vec(&serde_json::json!({"schema_version":1,"bridges":[{"id":"one","name":"Synthetic","address":hap,"lights":["simulation:light"],"scenes":[],"motors":[]}]})).unwrap();
    let (publisher,receiver)=crate::automation_publication::channel();
    let service={let directory=root.clone();let devices=serde_json::to_vec(&devices).unwrap();let rules=serde_json::to_vec(&rules).unwrap();let publisher=publisher.clone();tokio::spawn(async move {
        serve_inner(&devices,&rules,&directory,Duration::from_secs(5),true,0,Some(receiver),Duration::from_millis(20),Some(crate::service::WebSettings{address:web,secret:"abcdefghijklmnopqrstuvwxyz0123456789".into(),administrator:true,tls:None}),Some(publisher),None,Some(plan),None).await
    })};
    tokio::time::timeout(Duration::from_secs(3),async {loop {
        assert!(!service.is_finished());
        if let Ok((200,value))=http(web,"/api/v1/diagnostics",None).await && value["hap"]=="listening" {break;}
        tokio::time::sleep(Duration::from_millis(10)).await;
    }}).await.unwrap();
    // Only a generated fixture directory is replaced. The running old bridge
    // retains its open identity; recreating a bridge must reject the new file.
    std::fs::rename(root.join("hap-bridges"),root.join("retired-identities")).unwrap();
    std::fs::write(root.join("hap-bridges"),b"synthetic non-directory").unwrap();
    devices["revision"]=2.into();rules["configuration_revision"]=2.into();
    let error=publisher.publish(serde_json::to_vec(&devices).unwrap(),serde_json::to_vec(&rules).unwrap(),1).await.unwrap_err();
    assert!(error.contains("committed revision 2") && error.contains("runtime activation failed") && error.contains("HAP bridge directory must be a real directory"),"{error}");
    assert!(!error.contains("response lost"),"{error}");
    let terminal=service.await.unwrap().unwrap_err();assert_eq!(terminal,error);
    assert_eq!(homesense_storage::Store::open(root.join("automation-tcp.sqlite")).unwrap().active().unwrap().unwrap().revision,2);
    gateway.abort();let _=gateway.await;
    std::fs::remove_dir_all(root).unwrap();
}
