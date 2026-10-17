//! Preserved capability fences around the one-turn Admission selection read.
use super::*;
use crate::web_auth::{Grant,Role,Tokens};

#[tokio::test]
async fn coherent_selection_matches_ordinary_projection_and_checks_all_scope_bounds() {
    let f=Fixture::new().await;let generation=f.receiver.generation.clone();let names=crate::scene_switches::variable_names(&f.document);assert!(!names.is_empty());
    let ordinary=f.admission.scene_selection_snapshot(f.extra,generation.clone(),names.clone()).await.unwrap();
    let combined=f.admission.published_scene_selection_snapshot(f.controller,f.extra,generation.clone(),1,names.clone()).await.unwrap();
    assert_eq!(combined.values.keys().collect::<Vec<_>>(),ordinary.keys().collect::<Vec<_>>());
    for (name,value) in &combined.values {
        let expected=&ordinary[name];
        assert_eq!(value.stored,expected.stored);assert_eq!(value.evidence,expected.evidence);assert_eq!(value.evidence_current,expected.evidence_current);
        assert_eq!(value.initialization,expected.initialization);assert_eq!(value.physical,expected.physical);assert_eq!(value.component_inferred,expected.component_inferred);
    }
    assert_eq!(combined.mapping_readable,f.admission.diagnostics(f.extra,generation.clone()).await.unwrap().mapping_readable());
    assert!(f.admission.published_scene_selection_snapshot(Uuid::new_v4(),f.extra,generation.clone(),1,names.clone()).await.is_err());
    assert!(f.admission.published_scene_selection_snapshot(f.controller,Uuid::new_v4(),generation.clone(),1,names.clone()).await.is_err());
    assert!(f.admission.published_scene_selection_snapshot(f.controller,f.extra,generation.clone(),2,names.clone()).await.is_err());
    for invalid in [vec!["".into()],vec!["x".repeat(257)],vec![names[0].clone();65],vec![names[0].clone(),names[0].clone()],vec!["missing-variable".into()]] {
        assert!(f.admission.published_scene_selection_snapshot(f.controller,f.extra,generation.clone(),1,invalid).await.is_err());
    }
    for epoch in [0,1,2] {
        let mut wrong=generation.clone();match epoch {0=>wrong.epochs.configuration+=1,1=>wrong.epochs.transport+=1,_=>wrong.epochs.control+=1}
        assert!(f.admission.published_scene_selection_snapshot(f.controller,f.extra,wrong,1,names.clone()).await.is_err());
    }
    let other=Fixture::new().await;assert_eq!(other.receiver.generation.epochs,generation.epochs);assert_ne!(other.admission.boot_id(),f.admission.boot_id());
    assert!(f.admission.published_scene_selection_snapshot(f.controller,f.extra,other.receiver.generation.clone(),1,names).await.is_err());
    assert!(f.read(f.extra,1).await.is_ok(),"rejected foreign reads cannot poison this owner");other.finish().await;f.finish().await;
}

#[tokio::test]
async fn coherent_selection_rechecks_requested_authority_after_projection_without_a_second_fifo_wait() {
    let mut f=Fixture::new().await;f.receiver.close();f.admission.clone().shutdown().await.unwrap();
    let base=f.controller;let requested=f.extra;let allowed=Arc::new(AtomicBool::new(true));let checks=Arc::new(std::sync::atomic::AtomicUsize::new(0));let permit=allowed.clone();let count=checks.clone();
    f.admission=Admission::open(f.directory.join("runtime.sqlite"),crate::admission::Options{context:ExecutionContext::Simulation,..Default::default()},move|id| {
        if id==base{return true;}
        if id!=requested{return false;}
        count.fetch_add(1,Ordering::AcqRel);
        // First requested-reader authorization succeeds, but an external revoke
        // is immediately visible to the original final authorization fence.
        permit.swap(false,Ordering::AcqRel)
    }).await.unwrap();f.admission.synchronized(1,1).await.unwrap();
    let(control,mut receiver)=channel(base,f.admission.task_generation().await.unwrap());receiver.bind_selection_reader(&f.admission,&f.document,f.stop.subscribe()).await.unwrap();f.control=control;f.receiver=receiver;
    let result=f.read(requested,1).await;let checks=checks.load(Ordering::Acquire);let was_revoked=!allowed.load(Ordering::Acquire);
    assert!(f.read(base,1).await.is_ok(),"base owner remains authorized independently");f.finish().await;
    assert!(was_revoked);assert_eq!(checks,2,"the requested owner is checked before and after projection");assert!(result.is_err(),"an initial reader grant cannot survive its final revocation");
}

#[tokio::test]
async fn coherent_selection_source_revoked_during_actor_wait_cannot_return_an_old_valid_value() {
    let f=Fixture::new().await;let tokens=Arc::new(Tokens::default());let credential=Uuid::new_v4();let secret="synthetic-selection-source-0123456789";
    tokens.insert(credential,secret,Grant{controller:f.controller,role:Role::Member,scenes:["lights-on".into()].into(),devices:Default::default()}).unwrap();
    let action=Action::SceneAction{controller:f.controller,revision:1,scene:"lights-on".into(),execute:true};
    let lease=tokens.module_off_lease(Some(&format!("Bearer {secret}")),None,None).unwrap();let source=crate::source_auth::Source::capture(lease,&action,None).unwrap();
    let scoped=f.admission.with_source(Some(source));let(control,mut receiver)=channel(f.controller,f.receiver.generation.clone());receiver.bind_selection_reader(&scoped,&f.document,f.stop.subscribe()).await.unwrap();
    assert!(control.request(Action::SceneSelectionDetails{controller:f.extra,revision:1}).await.is_ok());
    f.hold_next.store(true,Ordering::Release);let read_control=control.clone();let requested=f.extra;
    let read=tokio::spawn(async move{read_control.request(Action::SceneSelectionDetails{controller:requested,revision:1}).await});
    let entered=tokio::time::timeout(Duration::from_secs(2),f.entered.notified()).await;
    tokens.revoke(credential).unwrap();{let(lock,wake)=&*f.release;*lock.lock().unwrap()=true;wake.notify_all();}
    let result=tokio::time::timeout(Duration::from_secs(2),read).await.unwrap().unwrap();
    let still_rejected=control.request(Action::SceneSelectionDetails{controller:f.extra,revision:1}).await.is_err();
    let unscoped_ok=f.read(f.extra,1).await.is_ok();receiver.close();f.finish().await;
    assert!(entered.is_ok());assert!(result.is_err());assert!(still_rejected);assert!(unscoped_ok,"source revoke cannot invalidate an independent current owner");
}
