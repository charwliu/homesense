use super::*;
use crate::automation_tcp::fault_tests::{http,free_address};

pub(super) struct Lifecycle {
    pub(super) ready:tokio::sync::oneshot::Sender<()>,
    pub(super) fail_after:Option<(tokio::sync::oneshot::Receiver<()>,Duration)>,
}

#[tokio::test]
async fn simulation_hap_failure_keeps_web_and_scene_execution_alive() {run(false,Duration::ZERO).await;}
#[tokio::test]
async fn occupied_simulation_hap_port_preserves_web_control() {run(true,Duration::ZERO).await;}
#[tokio::test]
async fn simulation_hap_failure_after_slow_startup_preserves_web_control() {run(false,Duration::from_millis(2200)).await;}

async fn run(occupied:bool,startup_delay:Duration) {
    let root=std::env::temp_dir().join(format!("homesense-simulation-hap-fault-{}",Uuid::new_v4()));
    let web=free_address();let reserved=occupied.then(||std::net::TcpListener::bind("127.0.0.1:0").unwrap());let hap=reserved.as_ref().map_or_else(free_address,|listener|listener.local_addr().unwrap());let directory=root.clone();
    let rules=serde_json::json!({"schema_version":1,"configuration_revision":1,"variables":[],"rules":[],"timed_scenes":[{"id":"later","enabled":true,"review_evidence":"synthetic","max_lateness_ms":1000,"steps":[{"source":"fixture","offset_ms":500,"device":"simulation:light","command":{"kind":"level","value":60,"ramp":false}}]}]});
    let (ready_send,ready_receive)=tokio::sync::oneshot::channel();
    let (fault_send,fault_receive)=tokio::sync::oneshot::channel();
    let lifecycle=Lifecycle {ready:ready_send,fail_after:(!occupied).then_some((fault_receive,Duration::from_millis(500)))};
    let service=tokio::spawn(async move {
        // Cold setup is outside the fault-response clock. Runtime duration
        // remains 3 seconds, including in the controlled slow-start case.
        tokio::time::sleep(startup_delay).await;
        serve_automation_simulation_options(include_bytes!("../../../config/simulation.json"),&serde_json::to_vec(&rules).unwrap(),&directory,Duration::from_secs(3),None,None,Some(WebSettings{address:web,secret:"abcdefghijklmnopqrstuvwxyz0123456789".into(),administrator:true,tls:None}),Some(UnifiedSimulation{address:hap,scenes:vec!["later".into()]}),Some(lifecycle)).await
    });
    tokio::time::timeout(Duration::from_secs(10),ready_receive).await.expect("simulation startup deadline").expect("simulation ended before listeners were ready");
    assert!(!service.is_finished());
    assert_eq!(http(web,"/health/ready",None).await.unwrap().0,200);
    if !occupied {
        // Prove a healthy baseline before arming failure. A hook that fails to
        // abort HAP must still fail the original 2-second wait below.
        let (status,data)=http(web,"/api/v1/diagnostics",None).await.unwrap();
        assert_eq!(status,200);
        assert_eq!(data["hap"],"listening");
        assert!(tokio::net::TcpStream::connect(hap).await.is_ok());
        fault_send.send(()).unwrap();
    }
    tokio::time::timeout(Duration::from_secs(2),async {
        loop {
            assert!(!service.is_finished());
            if let Ok((200,data))=http(web,"/api/v1/diagnostics",None).await
                && data["hap"]=="degraded" {break;}
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.unwrap();
    assert!(!service.is_finished());
    if !occupied {assert!(tokio::net::TcpStream::connect(hap).await.is_err());}
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
