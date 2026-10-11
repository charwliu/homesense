use homesensed::automation_publication::channel;
use std::{future::Future, task::Poll};
async fn poll_once<F: Future + Unpin>(future: &mut F) -> Poll<F::Output> {
    std::future::poll_fn(|cx| Poll::Ready(std::pin::Pin::new(&mut *future).poll(cx))).await
}
#[tokio::test]
async fn bounded_queue_preserves_payload_and_releases_waiters_on_close() {
    let (publisher, mut receiver) = channel();
    let mut first = Box::pin(publisher.publish(b"devices".to_vec(), b"rules".to_vec(), 3));
    let mut second = Box::pin(publisher.publish(vec![1], vec![2], 3));
    assert!(poll_once(&mut first).await.is_pending());
    assert!(poll_once(&mut second).await.is_pending());
    assert!(
        publisher
            .publish(vec![1], vec![2], 3)
            .await
            .unwrap_err()
            .contains("busy")
    );
    let pending = receiver.receive().await.unwrap();
    assert_eq!(pending.devices(), b"devices");
    assert_eq!(pending.rules(), b"rules");
    assert_eq!(pending.expected_revision(), 3);
    pending.complete(Ok(4));
    assert_eq!(first.await.unwrap(), 4);
    receiver.close();
    assert!(second.await.unwrap_err().contains("before acceptance"));
    assert!(publisher.publish(vec![1], vec![2], 4).await.is_err());
}
#[tokio::test]
async fn abandoned_request_is_skipped_before_live_request() {
    let (publisher, mut receiver) = channel();
    let mut abandoned = Box::pin(publisher.publish(vec![1], vec![2], 1));
    assert!(poll_once(&mut abandoned).await.is_pending());
    drop(abandoned);
    let mut live = Box::pin(publisher.publish(vec![3], vec![4], 2));
    assert!(poll_once(&mut live).await.is_pending());
    let request = receiver.receive().await.unwrap();
    assert_eq!(request.expected_revision(), 2);
    drop(live);
    assert!(request.abandoned());
    request.complete(Ok(3));
}
#[tokio::test]
async fn invalid_sizes_and_revision_do_not_enter_queue() {
    let (publisher, mut receiver) = channel();
    for (devices, rules, revision) in [
        (vec![], vec![1], 1),
        (vec![1], vec![], 1),
        (vec![1], vec![1], 0),
        (vec![0; 4 * 1024 * 1024 + 1], vec![1], 1),
    ] {
        assert!(publisher.publish(devices, rules, revision).await.is_err());
    }
    drop(publisher);
    assert!(receiver.receive().await.is_none());
}

#[test]
fn tcp_candidate_requires_queryable_outputs_before_publication() {
    use hap::serde_json::{json, to_vec, Value};
    let mut devices: Value = hap::serde_json::from_slice(include_bytes!(
        "../../../config/simulation-home.json"
    )).unwrap();
    let rules = to_vec(&json!({"schema_version":1,"configuration_revision":1,
        "variables":[],"rules":[]})).unwrap();
    // A simulation-only model must not pass a live publication preflight.
    assert!(homesensed::automation_tcp::validate_candidate(&to_vec(&devices).unwrap(), &rules).is_err());
    devices["gateways"][0]["modules"][0]["verification"] = json!({
        "status":"verified","evidence":"offline fixture","verified_at_unix_ms":1
    });
    assert!(homesensed::automation_tcp::validate_candidate(&to_vec(&devices).unwrap(), &rules).is_ok());
    devices["gateways"][0]["modules"][0]["profile"] = "ts6_86_rs485_v2_6_5_6key".into();
    devices["gateways"][0]["modules"][0]["led_mode"] = "manual".into();
    devices["devices"][0].as_object_mut().unwrap().remove("default_on_level");
    let error = homesensed::automation_tcp::validate_candidate(&to_vec(&devices).unwrap(), &rules).err().unwrap();
    assert!(error.contains("queryable output"), "{error}");
}

#[tokio::test]
async fn candidate_fades_respect_bus_capacity_but_disabled_drafts_remain_reviewable() {
    use hap::serde_json::{json,to_vec,Value};
    let mut devices:Value=hap::serde_json::from_slice(include_bytes!("../../../config/simulation.json")).unwrap();
    devices["gateways"][0]["modules"][0]["verification"]=json!({"status":"verified","evidence":"synthetic","verified_at_unix_ms":1});
    let mut rules=json!({"schema_version":1,"configuration_revision":1,"variables":[],"rules":[],"timed_scenes":[{"id":"fade","enabled":true,"review_evidence":"synthetic","max_lateness_ms":1000,"steps":[{"source":"synthetic","offset_ms":0,"device":"simulation:light","command":{"kind":"level","value":50,"ramp":false},"fade":{"duration_ms":1000,"interval_ms":100,"lateness_ms":100}}]}]});
    let check=|rules:&Value,interval|homesensed::automation_tcp::validate_candidate_with_write_interval(&to_vec(&devices).unwrap(),&to_vec(rules).unwrap(),std::time::Duration::from_millis(interval));
    assert!(check(&rules,20).is_ok());
    assert!(check(&rules,200).err().unwrap().contains("fade step"));
    let root=std::env::temp_dir().join(format!("fade-preflight-{}",uuid::Uuid::new_v4()));
    let error=homesensed::automation_tcp::serve_with_write_interval(&to_vec(&devices).unwrap(),&to_vec(&rules).unwrap(),&root,std::time::Duration::from_secs(1),true,0,None,std::time::Duration::from_millis(200)).await.unwrap_err();
    assert!(error.contains("fade step"));
    assert!(!root.exists(),"invalid startup must not create state");
    rules["timed_scenes"][0]["enabled"]=false.into();
    assert!(check(&rules,200).is_ok());
    rules["timed_scenes"][0]["enabled"]=true.into();
    rules["timed_scenes"][0]["steps"][0]["fade"]["interval_ms"]=200.into();
    assert!(check(&rules,200).is_ok());
    assert!(check(&rules,0).is_err());
}
