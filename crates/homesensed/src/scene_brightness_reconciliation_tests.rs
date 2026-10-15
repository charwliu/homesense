//! Generated synthetic scene inference. No source project data or physical IO.
use super::*;
use homesense_core::{Command,DeviceId,LevelTransition,Percent};
use std::sync::atomic::{AtomicU64,Ordering};

fn collision_inputs()->(Vec<u8>,Json) {
    let (config,mut rules)=reconciled();
    let mut config:Json=serde_json::from_slice(&config).unwrap();
    config["gateways"][0]["modules"][0]["profile"]=json!("ad8_din_rs485_v2_2_1");
    rules["timed_scenes"][1]["steps"][0]["command"]["value"]=json!(30);
    rules["timed_scenes"][1]["steps"][1]["command"]["value"]=json!(0);
    rules["scene_switch_groups"][0]["reconciliation"]["policy"]=json!("power_pattern_then_profile_level");
    for scene in rules["timed_scenes"].as_array_mut().unwrap(){for step in scene["steps"].as_array_mut().unwrap(){step["command"]["ramp"]=json!(true);}}
    (serde_json::to_vec(&config).unwrap(),rules)
}
async fn clocked(config:Vec<u8>,rules:Json)->(Harness,&'static AtomicU64) {
    let mut h=harness(config.clone(),rules).await;
    h.actor.shutdown().await.unwrap();
    let clock:&'static AtomicU64=Box::leak(Box::new(AtomicU64::new(100)));
    let controller=h.controller;
    h.actor=Admission::open_with_test_time(h.directory.join("runtime.sqlite"),Options{context:ExecutionContext::Simulation,..Default::default()},move|id|id==controller,clock).await.unwrap();
    h.generation=h.actor.task_generation().await.unwrap();
    let config=homesense_config::Configuration::parse(&config,ExecutionContext::Simulation).unwrap();
    h.feedback=Feedback::new(&config,ExecutionContext::Simulation,uuid::Uuid::parse_str(h.actor.boot_id()).unwrap().as_u128(),h.generation.epochs.transport).unwrap();
    (h,clock)
}
fn relay(h:&mut Harness,on:bool) {
    h.feedback.apply(&GatewayRead{gateway_id:"bus".into(),transport_epoch:h.generation.epochs.transport,received_at_ms:h.actor.monotonic_ms(),result:hb_transport::Receive::Frames{transport_epoch:h.generation.epochs.transport,frames:vec![hb_protocol::RawFrame{address:3,command:0xd1,value:if on{0xf1}else{0xf2}}]}}).unwrap();
}
#[tokio::test]
async fn collision_refinement_selects_only_unique_actual_levels_and_keeps_other_patterns_power_only() {
    for (main,down,selected) in [(Some(100),Some(0),Some(1)),(Some(30),Some(0),Some(2)),(Some(40),Some(0),None),(Some(0),Some(1),Some(3)),(Some(0),Some(80),Some(3)),(Some(0),Some(0),Some(0)),(None,Some(0),None),(Some(30),None,None),(Some(30),Some(30),Some(0))] {
        let (config,rules)=collision_inputs();let (mut h,_)=clocked(config,rules).await;
        history(&h,1);levels(&mut h,main,down);reconcile(&h).await;
        let snapshot=h.selection().await;
        assert_eq!(snapshot.stored.value,selected.map(Value::Integer),"main={main:?} down={down:?}");
        assert_eq!(snapshot.physical.unwrap().history_value,Some(Value::Integer(1)),"inferred selection must not rewrite intent");
        let details=crate::scene_switches::details(&h.document,&[("selection".into(),h.selection().await)].into()).unwrap();
        assert_eq!(details["second"].selection_source,"physical_feedback");
        assert_eq!(details["second"].selected,selected.map(|value|value==2));
        h.no_output();h.close().await;
    }
}
#[tokio::test]
async fn collision_refinement_feedback_change_and_exact_expiry_revoke_old_selection() {
    let (config,mut rules)=collision_inputs();rules["scene_switch_groups"][0]["reconciliation"]["max_age_ms"]=json!(100);
    let (mut h,time)=clocked(config,rules).await;
    levels(&mut h,Some(100),Some(0));reconcile(&h).await;assert_eq!(h.selection().await.stored.value,Some(Value::Integer(1)));
    let old=projection(&h).await;
    time.store(101,Ordering::Release);levels(&mut h,Some(30),Some(0));
    assert_eq!(h.actor.reconcile_scene_selection(h.controller,h.generation.clone(),old).await,Err(hap::server::WriteStatus::ResourceBusy));
    assert_eq!(h.selection().await.stored.value,None,"changed read set must mask prior projection immediately");
    reconcile(&h).await;assert_eq!(h.selection().await.stored.value,Some(Value::Integer(2)));
    time.store(200,Ordering::Release);assert_eq!(h.selection().await.stored.value,Some(Value::Integer(2)));
    time.store(201,Ordering::Release);assert_eq!(h.selection().await.stored.value,None,"age exactly max_age is expired");
    assert!(source(&h).await.sample().await.unwrap().values().all(Option::is_none));
    time.store(202,Ordering::Release);levels(&mut h,Some(40),Some(0));reconcile(&h).await;assert_eq!(h.selection().await.stored.value,None,"fresh but unmatched brightness still unknown");
    time.store(203,Ordering::Release);levels(&mut h,Some(0),Some(83));reconcile(&h).await;assert_eq!(h.selection().await.stored.value,Some(Value::Integer(3)),"unique power ignores nonmatching brightness");
    h.no_output();h.close().await;
}
#[tokio::test]
async fn collision_refinement_never_uses_hidden_supply_off_unknown_or_stale_as_lit_brightness() {
    let (config,mut rules)=collision_inputs();let mut config:Json=serde_json::from_slice(&config).unwrap();
    config["devices"][0]["supply_relay"]=json!("switch");config["devices"][0]["default_on_level"]=json!(100);
    rules["scene_switch_groups"][0]["reconciliation"]["max_age_ms"]=json!(100);
    // Combined logical targets are normalized Immediate; physical AD8-only
    // outputs remain native Ramp. Validate the actual encoder contract too.
    for scene in rules["timed_scenes"].as_array_mut().unwrap(){for step in scene["steps"].as_array_mut().unwrap(){if step["device"]=="light"{step["command"]["ramp"]=json!(false);}}}
    let parsed=homesense_config::Configuration::parse(&serde_json::to_vec(&config).unwrap(),ExecutionContext::Simulation).unwrap();
    crate::configuration_publish::validate_automation(&parsed,&serde_json::to_vec(&rules).unwrap(),ExecutionContext::Simulation).unwrap();
    let (mut h,time)=clocked(serde_json::to_vec(&config).unwrap(),rules).await;
    levels(&mut h,Some(30),Some(0));reconcile(&h).await;assert_eq!(h.selection().await.stored.value,None,"unknown supply cannot select low scene");
    relay(&mut h,false);reconcile(&h).await;assert_eq!(h.selection().await.stored.value,Some(Value::Integer(0)),"old dimmer setting behind off supply is not a lit scene");
    time.store(101,Ordering::Release);relay(&mut h,true);reconcile(&h).await;assert_eq!(h.selection().await.stored.value,Some(Value::Integer(2)));
    time.store(201,Ordering::Release);levels(&mut h,Some(30),Some(0));reconcile(&h).await;assert_eq!(h.selection().await.stored.value,None,"fresh dimmer cannot conceal expired supply");
    relay(&mut h,false);reconcile(&h).await;assert_eq!(h.selection().await.stored.value,Some(Value::Integer(0)));
    h.no_output();h.close().await;
}
#[tokio::test]
async fn collision_refinement_pending_and_written_intent_cannot_replace_new_feedback() {
    let (config,rules)=collision_inputs();let (mut h,time)=clocked(config,rules).await;
    levels(&mut h,Some(30),Some(0));reconcile(&h).await;assert_eq!(h.selection().await.stored.value,Some(Value::Integer(2)));
    h.actor.submit_rule_update(h.generation.clone(),homesense_hap::requests::Batch{controller:h.controller,intents:vec![homesense_hap::requests::Intent{device:DeviceId::new("light").unwrap(),command:Command::SetLevel{level:Percent::new(0).unwrap(),transition:LevelTransition::Immediate}}],subscriptions:vec![]},vec![]).await.unwrap();
    assert_eq!(h.selection().await.stored.value,None);
    let proof=projection(&h).await;
    assert_eq!(h.actor.reconcile_scene_selection(h.controller,h.generation.clone(),proof).await,Err(hap::server::WriteStatus::ResourceBusy));
    let ticket=h.actor.dispatch().await.unwrap().unwrap();h.actor.finish_write(ticket.command.id,true).await.unwrap();
    reconcile(&h).await;assert_eq!(h.selection().await.stored.value,None,"old same-time reply cannot settle a Written command");
    time.store(101,Ordering::Release);levels(&mut h,Some(100),Some(0));reconcile(&h).await;assert_eq!(h.selection().await.stored.value,Some(Value::Integer(1)));
    assert_eq!(h.sql().query_row("SELECT count(*) FROM command_journal",[],|r|r.get::<_,i64>(0)).unwrap(),1);
    h.close().await;
}
#[tokio::test]
async fn collision_refinement_old_epoch_and_replaced_policy_cannot_keep_selection() {
    let (config,rules)=collision_inputs();let (mut h,time)=clocked(config,rules).await;
    levels(&mut h,Some(30),Some(0));reconcile(&h).await;assert_eq!(h.selection().await.stored.value,Some(Value::Integer(2)));
    let old=projection(&h).await;h.actor.transport_failed().await.unwrap();h.actor.recover_synchronized(2,1).await.unwrap();h.generation=h.actor.task_generation().await.unwrap();
    assert_eq!(h.actor.reconcile_scene_selection(h.controller,h.generation.clone(),old).await,Err(hap::server::WriteStatus::InvalidValueInRequest));
    h.feedback.invalidate().unwrap();time.store(101,Ordering::Release);h.feedback.resume(2,101).unwrap();
    reconcile(&h).await;assert_eq!(h.selection().await.stored.value,None);
    levels(&mut h,Some(100),Some(0));reconcile(&h).await;assert_eq!(h.selection().await.stored.value,Some(Value::Integer(1)));
    let mut changed=h.document.scene_switch_groups[0].clone();
    changed.reconciliation.as_mut().unwrap().policy=homesense_config::scene_switches::ReconciliationPolicy::UniqueLightOnPattern;
    let forged_policy=Projection::prepare(&h.actor,h.controller,h.generation.clone(),&changed,&h.document,&h.feedback).await.unwrap().unwrap();
    assert_eq!(h.actor.reconcile_scene_selection(h.controller,h.generation.clone(),forged_policy).await,Err(hap::server::WriteStatus::InvalidValueInRequest),"actor must bind the immutable published policy");
    h.no_output();h.close().await;
}
#[test]
fn collision_policy_is_additive_reviewed_and_requires_a_known_discriminator_profile() {
    let (config,mut rules)=collision_inputs();let parsed=homesense_config::Configuration::parse(&config,ExecutionContext::Simulation).unwrap();
    assert!(Document::parse(&serde_json::to_vec(&rules).unwrap(),&parsed).is_ok());
    crate::configuration_publish::validate_automation(&parsed,&serde_json::to_vec(&rules).unwrap(),ExecutionContext::Simulation).unwrap();
    let mut unsupported:Json=serde_json::from_slice(&config).unwrap();unsupported["gateways"][0]["modules"][0]["profile"]=json!("gt_d8_rl_v2_2_6_2");
    let unsupported=homesense_config::Configuration::parse(&serde_json::to_vec(&unsupported).unwrap(),ExecutionContext::Simulation).unwrap();
    let error=match Document::parse(&serde_json::to_vec(&rules).unwrap(),&unsupported){Ok(_)=>panic!("unreviewed profile accepted"),Err(error)=>error.to_string()};assert!(error.contains("AD8 percentage readback"),"{error}");
    rules["scene_switch_groups"][0]["reconciliation"]["enabled"]=json!(false);assert!(Document::parse(&serde_json::to_vec(&rules).unwrap(),&unsupported).is_ok());
    rules["scene_switch_groups"][0]["reconciliation"]["enabled"]=json!(true);rules["scene_switch_groups"][0]["reconciliation"]["policy"]=json!("unique_light_on_pattern");assert!(Document::parse(&serde_json::to_vec(&rules).unwrap(),&unsupported).is_ok());
    rules["scene_switch_groups"][0]["reconciliation"]["policy"]=json!("power_pattern_then_profile_level");rules["timed_scenes"][1]["steps"][1]["command"]["value"]=json!(100);
    assert!(Document::parse(&serde_json::to_vec(&rules).unwrap(),&unsupported).is_ok(),"unique power vectors need no brightness-profile assumption");
    rules["scene_switch_groups"][0]["reconciliation"]["review_evidence"]=json!("");assert!(Document::parse(&serde_json::to_vec(&rules).unwrap(),&parsed).is_err());
}

#[tokio::test]
async fn collision_refinement_all_off_duplicates_reset_without_level_selection() {
    let (config,mut rules)=collision_inputs();
    for scene in rules["timed_scenes"].as_array_mut().unwrap(){for step in scene["steps"].as_array_mut().unwrap(){step["command"]["value"]=json!(0);}}
    let (mut h,_)=clocked(config,rules).await;levels(&mut h,Some(0),Some(0));reconcile(&h).await;
    assert_eq!(h.selection().await.stored.value,Some(Value::Integer(0)));h.no_output();h.close().await;
}
#[tokio::test]
async fn collision_refinement_actual_level_overrides_recorded_zero_without_making_commands() {
    let (config,rules)=collision_inputs();let (mut h,time)=clocked(config,rules).await;
    completed_zero_setting(&h).await;
    time.store(101,Ordering::Release);levels(&mut h,Some(30),Some(0));reconcile(&h).await;
    assert_eq!(h.selection().await.stored.value,Some(Value::Integer(2)),"recorded dimmer0 cannot replace fresh physical level30 for inference");
    time.store(102,Ordering::Release);levels(&mut h,Some(100),Some(0));reconcile(&h).await;
    assert_eq!(h.selection().await.stored.value,Some(Value::Integer(1)));
    assert_eq!(h.sql().query_row("SELECT count(*) FROM command_journal",[],|r|r.get::<_,i64>(0)).unwrap(),1,"inference itself must not submit light or LED commands");
    h.close().await;
}
