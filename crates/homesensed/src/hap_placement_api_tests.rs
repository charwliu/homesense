//! Placement metadata is administrator-only and cannot outlive its runtime.
use super::*;

#[tokio::test]
async fn actual_placement_api_requires_live_matching_publication_and_emits_no_device_commands() {
    use crate::hap_placement::{Accessory,Bridge,Kind,Publication};
    let h=harness(devices(),automation()).await;
    let (control,mut receiver)=crate::automation_control::channel(h.controller,h.generation.clone());
    let mut scenes=crate::scene_scheduler::SceneScheduler::new(h.actor.clone(),h.controller,8,h.actor.monotonic_ms()).unwrap();
    let tokens=std::sync::Arc::new(Tokens::default());
    let admin="abcdefghijklmnopqrstuvwxyz0123456789";
    let viewer="abcdefghijklmnopqrstuvwxyz9876543210";
    for (secret,role) in [(admin,Role::Administrator),(viewer,Role::Viewer)] {
        tokens.insert(uuid::Uuid::new_v4(),secret,Grant{controller:h.controller,role,scenes:Default::default(),devices:Default::default()}).unwrap();
    }
    let api=crate::web_api::Api{
        daily_backup:Default::default(),hap_placement:Default::default(),hap_bridges:Default::default(),hap_status:Default::default(),
        backup_source:None,publisher:None,origin:"http://127.0.0.1:8088".into(),tokens,
        runtime:std::sync::RwLock::new(Some(crate::web_api::Runtime{control,revision:1,boot:h.actor.boot_id().into(),scenes:Default::default()})),
        receipts:std::sync::Mutex::new(crate::web_receipts::Receipts::open(&h.directory.join("placement-http.sqlite")).unwrap()),drafts:None,
    };
    let request=|secret:&str,method:&str|hyper::Request::builder().uri("/api/v1/homekit/placement").method(method)
        .header("host","127.0.0.1:8088").header("authorization",format!("Bearer {secret}")).header("origin","http://127.0.0.1:8088").body(hyper::body::Bytes::new()).unwrap();
    assert_eq!(api.handle(request(viewer,"GET")).await.status(),hyper::StatusCode::FORBIDDEN);
    assert_eq!(api.handle(request(admin,"POST")).await.status(),hyper::StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(poll_request(&h,&mut receiver,&mut scenes,api.handle(request(admin,"GET"))).await.status(),hyper::StatusCode::SERVICE_UNAVAILABLE);
    let publication=Publication::new(h.actor.boot_id().into(),1,vec![Bridge{id:"one".into(),name:"Bridge".into(),available:true,accessories:vec![Accessory{kind:Kind::Light,id:"light".into(),aid:2,serial_number:"HS-2".into(),room_id:Some("room".into())}]}]);
    *api.hap_placement.write().unwrap()=Some(publication.clone());
    api.hap_bridges.write().unwrap().insert("one".into(),crate::hap_bridge_group::Status::Listening);
    let response=poll_request(&h,&mut receiver,&mut scenes,api.handle(request(admin,"GET"))).await;
    assert_eq!(response.status(),hyper::StatusCode::OK);
    let value:Json=serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(value,serde_json::to_value(&publication).unwrap());
    api.hap_bridges.write().unwrap().insert("one".into(),crate::hap_bridge_group::Status::Degraded);
    let response=poll_request(&h,&mut receiver,&mut scenes,api.handle(request(admin,"GET"))).await;
    let value:Json=serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(value["bridges"][0]["available"],false);
    for (boot,revision) in [("other-boot",1),(h.actor.boot_id(),2)] {
        *api.hap_placement.write().unwrap()=Some(Publication::new(boot.into(),revision,publication.bridges.clone()));
        assert_eq!(poll_request(&h,&mut receiver,&mut scenes,api.handle(request(admin,"GET"))).await.status(),hyper::StatusCode::SERVICE_UNAVAILABLE);
    }
    *api.runtime.write().unwrap()=None;
    assert_eq!(api.handle(request(admin,"GET")).await.status(),hyper::StatusCode::SERVICE_UNAVAILABLE);
    h.no_output();h.close().await;
}
