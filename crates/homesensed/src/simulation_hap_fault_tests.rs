use super::*;
use crate::automation_tcp::fault_tests::{http,free_address};
#[tokio::test]
async fn simulation_hap_failure_keeps_web_and_scene_execution_alive() {
    let root=std::env::temp_dir().join(format!("homesense-simulation-hap-fault-{}",Uuid::new_v4()));
    let web=free_address();let hap=free_address();let directory=root.clone();
    let rules=serde_json::json!({"schema_version":1,"configuration_revision":1,"variables":[],"rules":[],"timed_scenes":[{"id":"later","enabled":true,"review_evidence":"synthetic","max_lateness_ms":1000,"steps":[{"source":"fixture","offset_ms":500,"device":"simulation:light","command":{"kind":"level","value":60,"ramp":false}}]}]});
    let service=tokio::spawn(async move {
        serve_automation_simulation_options(include_bytes!("../../../config/simulation.json"),&serde_json::to_vec(&rules).unwrap(),&directory,Duration::from_secs(3),None,None,Some(WebSettings{address:web,secret:"abcdefghijklmnopqrstuvwxyz0123456789".into(),administrator:true,tls:None}),Some(UnifiedSimulation{address:hap,scenes:vec!["later".into()]}),Some(Duration::from_millis(500))).await
    });
    tokio::time::timeout(Duration::from_secs(2),async {
        loop {
            assert!(!service.is_finished());
            if let Ok((200,data))=http(web,"/api/v1/diagnostics",None).await {
                if data["hap"]=="degraded" {break;}
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.unwrap();
    assert!(!service.is_finished());
    assert!(tokio::net::TcpStream::connect(hap).await.is_err());
    assert_eq!(http(web,"/health/ready",None).await.unwrap().0,200);
    let accepted=http(web,"/api/v1/scene-actions",Some(serde_json::json!({"scene":"later","request_id":"after-fault"}))).await.unwrap();
    assert_eq!(accepted.1["outcome"],"accepted");
    tokio::time::timeout(Duration::from_secs(2),async {
        loop {
            let (_,data)=http(web,"/api/v1/devices",None).await.unwrap();
            if data["devices"][0]["observed"]["level"]==60 {break;}
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.unwrap();
    service.await.unwrap().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}
