//! A transport phase collects its bounded tail without one actor round trip per frame.
use super::*;

#[tokio::test]
async fn batch_tail_collection_preserves_frame_budget_order_and_independent_phase() {
    let mut r = rules();
    r["rules"] = json!([]);
    let actions = (0..23)
        .map(|n| {
            command(
                &format!("batch/{n}"),
                "relay",
                json!({"kind":"switch","on":n%2==0}),
            )
        })
        .collect::<Vec<_>>();
    r["immediate_scenes"] = json!([
        {"id":"act", "enabled":true, "review_evidence":"reviewed bounded lighting phase", "activate":actions},
        {"id":"other", "enabled":true, "review_evidence":"independent phase", "activate":[
            command("other/marker", "marker", json!({"kind":"switch","on":true}))
        ]}
    ]);
    let mut h = Harness::new(r).await;
    for scene in ["act", "other"] {
        h.request(Action::SceneAction {
            controller: h.controller,
            revision: 1,
            scene: scene.into(),
            execute: true,
        })
        .await
        .unwrap();
    }
    let first = h.actor.dispatch().await.unwrap().unwrap();
    let anchor = first.command.id;
    let mut phase = vec![first];
    let tail = h.actor.extend_write_batch(anchor, 7).await.unwrap();
    assert_eq!(
        tail.len(),
        7,
        "one collection honors the remaining frame budget"
    );
    phase.extend(tail);
    phase.extend(
        h.actor
            .extend_write_batch(anchor, hb_transport::MAX_BATCH_FRAMES)
            .await
            .unwrap(),
    );
    assert_eq!(
        phase.len(),
        23,
        "an independent phase must not enter this batch"
    );
    assert!(
        h.actor
            .extend_write_batch(anchor, hb_transport::MAX_BATCH_FRAMES)
            .await
            .unwrap()
            .is_empty()
    );
    for (n, ticket) in phase.iter().enumerate() {
        assert_eq!(ticket.command.device_id.as_str(), "relay");
        assert_eq!(
            ticket.command.command,
            homesense_core::Command::Switch(n % 2 == 0)
        );
    }
    let ids = phase.iter().map(|ticket| ticket.command.id).collect::<Vec<_>>();
    assert!(h.actor.finish_writes(vec![ids[0], u64::MAX]).await.is_err());
    assert!(h.actor.finish_writes(vec![ids[0], ids[0]]).await.is_err());
    let unchanged = h.actor.command_statuses(h.controller, h.generation.clone(), ids.clone()).await.unwrap();
    assert!(unchanged.iter().all(|status| *status == homesense_storage::journal::Status::Writing),
        "invalid later or duplicate receipt members must not complete a prefix");
    h.actor.finish_writes(ids).await.unwrap();
    let statuses = h.actor.command_statuses(h.controller, h.generation.clone(),
        phase.iter().map(|ticket| ticket.command.id).collect()).await.unwrap();
    assert!(statuses.iter().all(|status| *status == homesense_storage::journal::Status::Written));
    assert_eq!(h.wire().await, [0xae, 3, 2, 0xf1, 0xee]);
    assert_eq!(h.count("command_journal"), 24);
    h.finish().await;
}

#[tokio::test]
async fn complete_module_on_keeps_all_eight_admitted_and_written_channel_records() {
    let mut devices: Json = serde_json::from_slice(&data()).unwrap();
    devices["devices"] = json!([]);
    for channel in 1..=8 {
        devices["devices"].as_array_mut().unwrap().push(json!({
            "id":format!("port-{channel}"),"name":format!("Port {channel}"),
            "module_id":"relay","channel":channel,"enabled":true
        }));
    }
    let mut r = rules();
    r["rules"]=json!([]);r["variables"]=json!([]);
    r["timed_scenes"]=json!([]);
    r["immediate_scenes"]=json!([{
        "id":"act","enabled":true,"review_evidence":"Synthetic eight explicit admitted On outputs",
        "activate":(1..=8).map(|channel|command(&format!("all/{channel}"),&format!("port-{channel}"),json!({"kind":"switch","on":true}))).collect::<Vec<_>>()
    }]);
    r["immediate_scenes"].as_array_mut().unwrap().push(json!({
        "id":"off","enabled":true,"review_evidence":"Synthetic explicit absolute all Off",
        "activate":(1..=8).map(|channel|command(&format!("off/{channel}"),&format!("port-{channel}"),json!({"kind":"switch","on":false}))).collect::<Vec<_>>()
    }));
    let mut h=Harness::from_inputs(serde_json::to_vec(&devices).unwrap(),r).await;
    h.request(Action::SceneAction{controller:h.controller,revision:1,scene:"act".into(),execute:true}).await.unwrap();
    assert_eq!(h.count("command_journal"),8,"wire optimization must retain all individually authorized operations");
    assert_eq!(h.wire().await,[0xae,3,0,0xf1,0xee]);
    let db=rusqlite::Connection::open(h.directory.join("runtime.sqlite")).unwrap();
    let written:i64=db.query_row("SELECT count(*) FROM command_journal WHERE status = 'written'",[],|row|row.get(0)).unwrap();
    assert_eq!(written,8);
    drop(db);
    for _ in 0..2 {
        h.request(Action::SceneAction{controller:h.controller,revision:1,scene:"off".into(),execute:true}).await.unwrap();
        assert_eq!(h.wire().await,[0xae,3,0,0xf2,0xee],"explicit Off never toggles back On");
    }
    assert_eq!(h.count("command_journal"),24);
    h.finish().await;
}

fn household_inputs() -> (Vec<u8>, Json) {
    let mut devices:Json=serde_json::from_slice(&data()).unwrap();
    devices["gateways"][0]["modules"][0]["profile"]=json!("ad8_din_rs485_v2_2_1");
    devices["devices"]=json!([]);
    for dim in [false,true] {for channel in 1..=8 {
        let id=format!("{}-{channel}",if dim{"dim"}else{"port"});
        let mut d=json!({"id":id,"name":"Synthetic output","module_id":if dim{"dim"}else{"relay"},"channel":channel,"enabled":true});
        if dim&&channel==1 {d["supply"]=json!({"module_id":"relay","channel":1});d["default_on_level"]=json!(100);}
        devices["devices"].as_array_mut().unwrap().push(d);
    }}
    let mut r=rules();r["rules"]=json!([]);r["variables"]=json!([]);r["timed_scenes"]=json!([]);
    let definitions=[true,false].map(|on| {
        let actions=[true,false].into_iter().flat_map(|dim|(1..=8).map(move|ch| {
            command(&format!("whole/{on}/{dim}/{ch}"),&format!("{}-{ch}",if dim{"dim"}else{"port"}),
                if dim{level(if on{100}else{0})}else{json!({"kind":"switch","on":on})})
        })).collect::<Vec<_>>();
        json!({"id":if on{"act"}else{"off"},"enabled":true,"review_evidence":"Synthetic household absolute scope","activate":actions})
    });
    r["immediate_scenes"]=json!(definitions);
    r["scene_metadata"]=json!([{"id":"act","name":"全屋打开","scope":"household"},{"id":"off","name":"全屋关闭","scope":"household"}]);
    let modules=[false,true].map(|dim|json!({"module_id":if dim{"dim"}else{"relay"},"devices":(1..=8).map(|ch|format!("{}-{ch}",if dim{"dim"}else{"port"})).collect::<Vec<_>>(),"review_evidence":"Synthetic full eight-port scope"}));
    r["away_off"]=json!({"enabled":true,"review_evidence":"Synthetic same two module scope including fan and combined-supply consumer","active_execution_policy":"cancel_affected_lighting","whole_house_on_scene":"act","whole_house_off_scene":"off","modules":modules});
    (serde_json::to_vec(&devices).unwrap(),r)
}

#[test]
fn household_scene_aliases_reject_partial_relative_or_wrong_direction_recipes() {
    let (devices,r)=household_inputs();let config=Configuration::parse(&devices,ExecutionContext::Simulation).unwrap();
    assert!(Document::parse(&serde_json::to_vec(&r).unwrap(),&config).is_ok());
    let mut ordinary=r.clone();ordinary["away_off"].as_object_mut().unwrap().remove("whole_house_on_scene");ordinary["away_off"].as_object_mut().unwrap().remove("whole_house_off_scene");
    assert!(Document::parse(&serde_json::to_vec(&ordinary).unwrap(),&config).is_err(),"ordinary scene cannot acquire raw shared-supply access");
    for bad in 0..4 {
        let mut r=r.clone();
        match bad {
            0=>{r["immediate_scenes"][0]["activate"].as_array_mut().unwrap().pop();},
            1=>r["immediate_scenes"][0]["activate"][0]["action"]["command"]["on"]=json!(false),
            2=>r["away_off"]["whole_house_off_scene"]=json!("act"),
            _=>r["immediate_scenes"].as_array_mut().unwrap().push(json!({"id":"nested","enabled":true,"review_evidence":"Cannot bypass managed Off","activate":[{"source":"nested/off","action":{"kind":"scene","id":"off","activate":true}}]})),
        }
        assert!(Document::parse(&serde_json::to_vec(&r).unwrap(),&config).is_err(),"case {bad}");
    }
}

#[tokio::test]
async fn household_off_scene_routes_to_the_same_guarded_module_operation_as_away() {
    use crate::web_auth::{Tokens,Grant,Role};
    let (devices,r)=household_inputs();let mut h=Harness::from_inputs(devices,r).await;
    let tokens=Arc::new(Tokens::default());let secret="synthetic-household-scene-secret-0123456789";
    tokens.insert(Uuid::new_v4(),secret,Grant{controller:h.controller,role:Role::Administrator,
        devices:h.configuration.devices().keys().map(|id|id.as_str().to_owned()).collect(),scenes:["act".into(),"off".into()].into()}).unwrap();
    assert!(h.request(Action::SceneAction{controller:h.controller,revision:1,scene:"off".into(),execute:true}).await.is_err(),"unscoped caller cannot bypass managed source checks");
    assert_eq!(h.count("command_journal"),0);
    let limited=Arc::new(Tokens::default());let limited_secret="synthetic-household-limited-0123456789";
    limited.insert(Uuid::new_v4(),limited_secret,Grant{controller:h.controller,role:Role::Member,
        devices:h.configuration.devices().keys().filter(|id|id.as_str()!="port-8").map(|id|id.as_str().to_owned()).collect(),scenes:["act".into()].into()}).unwrap();
    let denied=Action::SceneAction{controller:h.controller,revision:1,scene:"act".into(),execute:true};
    let proof=crate::source_auth::Source::capture(limited.module_off_lease(Some(&format!("Bearer {limited_secret}")),None,None).unwrap(),&denied,None).unwrap();
    let control=h.control.clone();assert!(h.drive(tokio::spawn(async move{control.request_authorized(denied,proof).await})).await.is_err());
    assert_eq!(h.count("command_journal"),0,"missing one output grant rejects the whole opening scene");
    for (scene,expected) in [("act",0xf1),("off",0xf2)] {
        let action=Action::SceneAction{controller:h.controller,revision:1,scene:scene.into(),execute:true};
        let source=crate::source_auth::Source::capture(tokens.module_off_lease(Some(&format!("Bearer {secret}")),None,None).unwrap(),&action,None).unwrap();
        let control=h.control.clone();let reply=h.drive(tokio::spawn(async move{control.request_authorized(action,source).await})).await.unwrap();
        if scene=="off" {assert!(matches!(reply,Reply::AwayAccepted(_)),"full-house shutdown must retain managed retry and priority");}
        let mut wire=h.wire().await;
        assert_eq!(wire,[0xae,0x15,0,if scene=="act"{100}else{0},0xee],"later modules cannot be sent before first-module feedback");
        household_reply(&mut h,0x15,scene=="act",1..=8).await;
        wire.extend(h.wire().await);
        household_reply(&mut h,3,scene=="act",1..=8).await;
        let mut actual=wire.as_chunks::<5>().0.iter().map(|frame|frame.to_vec()).collect::<Vec<_>>();actual.sort();
        let mut wanted=vec![vec![0xae,3,0,expected,0xee],vec![0xae,0x15,0,if scene=="act"{100}else{0},0xee]];wanted.sort();assert_eq!(actual,wanted);
        let before=h.count("command_journal");
        let repeated=Action::SceneAction{controller:h.controller,revision:1,scene:scene.into(),execute:true};
        let source=crate::source_auth::Source::capture(tokens.module_off_lease(Some(&format!("Bearer {secret}")),None,None).unwrap(),&repeated,None).unwrap();
        let control=h.control.clone();
        let repeated=h.drive(tokio::spawn(async move{control.request_authorized(repeated,source).await})).await;
        if scene=="act" {
            assert!(matches!(repeated,Ok(Reply::HouseholdAccepted(crate::household_sequence::Accepted {commands,..})) if commands.is_empty()),"a fully confirmed On may return an audited no-op without a repeat write");
        } else {assert!(repeated.is_err(),"shutdown retrigger remains rate limited");}
        assert_eq!(h.count("command_journal"),before,"throttled repeats must have no durable or wire prefix");
        assert!(h.wire().await.is_empty());
    }
    assert_eq!(h.count("command_journal"),18,"sixteen opening channel records then two existing Away module operations");
    h.finish().await;
}

#[tokio::test]
async fn household_retrigger_bounds_do_not_delay_closing_or_internal_retries() {
    let (devices,r)=household_inputs();let h=Harness::from_inputs(devices,r).await;
    let mut limit=crate::household_scene::Limiter::default();let g=&h.generation;
    assert!(limit.permits(g,true,0,false,false));
    limit.accepted(g.clone(),true,0,vec![1,2]);
    assert_eq!(limit.opening_commands(g),[1,2]);
    assert!(!limit.permits(g,true,4999,false,false));
    assert!(!limit.permits(g,true,5000,false,true),"unfinished writes remain blocked beyond the interval");
    assert!(limit.permits(g,true,5000,false,false));
    assert!(limit.permits(g,false,1,false,true),"closing preempts opening without waiting for cooldown");
    limit.accepted(g.clone(),false,1,vec![]);
    assert!(limit.opening_commands(g).is_empty());
    assert!(!limit.permits(g,false,5000,false,false));
    assert!(!limit.permits(g,false,5001,true,false),"the active managed operation owns its retries");
    assert!(limit.permits(g,false,5001,false,false));
    assert!(!limit.permits(g,true,5000,false,false),"alternating directions cannot reopen immediately after a close");
    assert!(limit.permits(g,true,5001,false,false));
    assert!(!limit.permits(g,true,5001,true,false),"opening must not cancel active Away confirmation");
    let mut next=g.clone();next.epochs.transport+=1;
    assert!(limit.permits(&next,true,2,false,false),"an old epoch cannot throttle a new runtime");
    assert!(limit.opening_commands(&next).is_empty());
    h.finish().await;
}

#[tokio::test]
async fn household_open_skips_only_complete_fresh_modules_at_the_exact_target() {
    for case in 0..5 {
        let (devices,r)=household_inputs();let mut h=Harness::from_inputs(devices,r).await;
        if case==4 {h.feedback.set_managed_max_age_ms(10_000).unwrap();}
        let at=h.actor.monotonic_ms();
        let frames=[(3,case!=1),(0x15,true)].into_iter().flat_map(|(address,on)|(1..=8).map(move|channel|hb_protocol::RawFrame {
            address,command:0xd0+channel,value:if address==3 {if on{0xf1}else{0xf2}}else if case==2&&channel==4{10}else{100},
        })).collect();
        h.feedback.apply(&GatewayRead{gateway_id:"bus".into(),transport_epoch:1,received_at_ms:at,
            result:hb_transport::Receive::Frames{transport_epoch:1,frames}}).unwrap();
        let now=if case>=3 {at+5000}else{h.actor.monotonic_ms()};
        let prepared=crate::household_scene::prepare(&h.configuration,&h.document,h.controller,"act",&h.feedback,now,5000).unwrap();
        let (batch,_)=prepared.compile(&h.configuration,None).unwrap();
        let modules=batch.intents.iter().map(|i|h.configuration.binding(&i.device).unwrap().1.address().get()).collect::<std::collections::BTreeSet<_>>();
        match case {
            0|4=>assert!(batch.intents.is_empty(),"all sixteen raw targets remain confirmed within the configured observation lifetime"),
            1=>{assert_eq!(batch.intents.len(),8);assert_eq!(modules,[3].into());},
            2=>{assert_eq!(batch.intents.len(),8);assert_eq!(modules,[0x15].into(),"10% is On but does not satisfy full brightness");},
            _=>assert_eq!(batch.intents.len(),16,"old values cannot suppress explicit opening"),
        }
        h.finish().await;
    }
}

#[tokio::test]
async fn household_open_noop_rechecks_feedback_and_full_scope_authorization() {
    let (devices,r)=household_inputs();let mut h=Harness::from_inputs(devices,r).await;
    let at=h.actor.monotonic_ms();
    let frames=[3,0x15].into_iter().flat_map(|address|(1..=8).map(move|channel|hb_protocol::RawFrame {
        address,command:0xd0+channel,value:if address==3{0xf1}else{100},
    })).collect();
    h.feedback.apply(&GatewayRead{gateway_id:"bus".into(),transport_epoch:1,received_at_ms:at,
        result:hb_transport::Receive::Frames{transport_epoch:1,frames}}).unwrap();
    let tokens=Arc::new(crate::web_auth::Tokens::default());let secret="synthetic-noop-limited-scope-0123456789";
    tokens.insert(Uuid::new_v4(),secret,crate::web_auth::Grant{controller:h.controller,role:crate::web_auth::Role::Member,
        devices:h.configuration.devices().keys().filter(|id|id.as_str()!="port-8").map(|id|id.as_str().to_owned()).collect(),scenes:["act".into()].into()}).unwrap();
    let action=Action::SceneAction{controller:h.controller,revision:1,scene:"act".into(),execute:true};
    let source=crate::source_auth::Source::capture(tokens.module_off_lease(Some(&format!("Bearer {secret}")),None,None).unwrap(),&action,None).unwrap();
    let denied=crate::household_scene::prepare(&h.configuration,&h.document,h.controller,"act",&h.feedback,h.actor.monotonic_ms(),5000).unwrap();
    assert!(matches!(denied.compile(&h.configuration,Some(&source)),Err(hap::server::WriteStatus::InsufficientPrivileges)),"even a no-op requires the entire reviewed scope");
    let prepared=crate::household_scene::prepare(&h.configuration,&h.document,h.controller,"act",&h.feedback,h.actor.monotonic_ms(),5000).unwrap();
    assert!(h.actor.begin_household(h.generation.clone(),prepared,h.feedback.away_observation(h.actor.monotonic_ms()).unwrap(),h.feedback.last_frames(),h.actor.monotonic_ms()+2000).await.unwrap().commands.is_empty());
    assert_eq!(h.count("command_journal"),0);assert!(h.wire().await.is_empty());
    let prepared=crate::household_scene::prepare(&h.configuration,&h.document,h.controller,"act",&h.feedback,h.actor.monotonic_ms(),5000).unwrap();
    h.feedback.apply(&GatewayRead{gateway_id:"bus".into(),transport_epoch:1,received_at_ms:h.actor.monotonic_ms(),
        result:hb_transport::Receive::Frames{transport_epoch:1,frames:vec![hb_protocol::RawFrame{address:3,command:0xd8,value:0xf2}]}}).unwrap();
    assert!(h.actor.begin_household(h.generation.clone(),prepared,h.feedback.away_observation(h.actor.monotonic_ms()).unwrap(),h.feedback.last_frames(),h.actor.monotonic_ms()+2000).await.is_err(),"changed feedback invalidates an all-noop proof");
    assert_eq!(h.count("command_journal"),0);
    h.finish().await;
}

#[tokio::test]
async fn household_hap_off_is_paired_managed_and_retired_handler_cannot_replay() {
    use hap::storage::Storage;
    let (devices,r)=household_inputs();let mut h=Harness::from_inputs(devices,r).await;
    let mut identity=homesense_hap::storage::IdentityStore::open(h.directory.join("household-identity")).await.unwrap();
    identity.save_config(&hap::Config::default()).await.unwrap();
    let member=h.member;
    let bridge=crate::runtime_scene_hap::RuntimeSceneHap::new_with_admission(&identity,&h.document,
        vec![homesense_hap::scene_graph::SceneSpec{id:"off".into(),name:"全屋关闭".into()}],
        h.control.clone(),h.actor.clone(),h.controller,move|who,_|who==member).await.unwrap();
    identity.save_pairing(&hap::pairing::Pairing::new(h.member,hap::pairing::Permissions::Admin,[7;32])).await.unwrap();
    let scope=h.configuration.devices().keys().map(|id|id.as_str().to_owned()).collect();
    let active=Arc::new(AtomicBool::new(true));
    let old=crate::away_auth::HapLease::new(identity.clone(),member,scope,active.clone()).unwrap();
    assert!(h.actor.submit_away_off(1,crate::away_auth::Authorization::Hap(old.clone()),true).await.is_err(),"new scene must not enable the old hidden Away HAP accessory");
    let scope=h.configuration.devices().keys().map(|id|id.as_str().to_owned()).collect();
    let wrong=crate::away_auth::HapLease::for_household(identity.clone(),member,scope,active,"wrong-scene".into()).unwrap();
    assert!(h.actor.submit_away_off(1,crate::away_auth::Authorization::Hap(wrong),true).await.is_err());
    assert_eq!(h.count("command_journal"),0);
    let binding=&bridge.accessories[0].0;
    let body=serde_json::to_vec(&json!({"characteristics":[{"aid":binding.aid,"iid":binding.on_iid,"value":true}]})).unwrap();
    let handler=bridge.write_handler.clone();let input=body.clone();
    h.drive(tokio::spawn(async move{handler.write_async(member,&input).await})).await.unwrap();
    let mut wire=h.wire().await;assert_eq!(wire,[0xae,0x15,0,0,0xee]);
    household_reply(&mut h,0x15,false,1..=8).await;wire.extend(h.wire().await);
    household_reply(&mut h,3,false,1..=8).await;
    let mut frames=wire.as_chunks::<5>().0.iter().map(|f|f.to_vec()).collect::<Vec<_>>();frames.sort();
    assert_eq!(frames,vec![vec![0xae,3,0,0xf2,0xee],vec![0xae,0x15,0,0,0xee]]);
    assert_eq!(h.count("command_journal"),2);
    let repeated=bridge.write_handler.clone();let input=body.clone();
    assert_eq!(h.drive(tokio::spawn(async move{repeated.write_async(member,&input).await})).await,Err(hap::server::WriteStatus::ResourceBusy));
    assert_eq!(h.count("command_journal"),2);
    assert!(h.wire().await.is_empty());
    let retained=bridge.write_handler.clone();drop(bridge);
    assert_eq!(retained.write_async(member,&body).await,Err(hap::server::WriteStatus::InsufficientPrivileges));
    assert!(h.wire().await.is_empty());
    h.finish().await;
}

async fn household_reply(h:&mut Harness,address:u8,on:bool,channels:std::ops::RangeInclusive<u8>) {
    tokio::time::sleep(Duration::from_millis(1)).await;
    h.feedback.apply(&GatewayRead {gateway_id:"bus".into(),transport_epoch:1,received_at_ms:h.actor.monotonic_ms(),
        result:hb_transport::Receive::Frames {transport_epoch:1,frames:channels.map(|ch|hb_protocol::RawFrame {
            address,command:0xd0+ch,value:if address==0x15 {if on {100}else{0}}else if on {0xf1}else{0xf2}}).collect()}}).unwrap();
    tokio::time::sleep(Duration::from_millis(21)).await;
    if h.actor.away_operations_active() {
        h.actor.tick_away_operations(h.controller,h.generation.clone(),h.feedback.away_observation(h.actor.monotonic_ms()).unwrap()).await.unwrap();
    }else{
        h.actor.tick_household(h.controller,h.generation.clone(),h.feedback.away_observation(h.actor.monotonic_ms()).unwrap(),h.feedback.last_frames()).await.unwrap();
    }
}

#[tokio::test]
async fn whole_house_on_waits_for_all_current_module_feedback_before_admitting_next() {
    let (devices,rules)=household_inputs();let mut h=Harness::from_inputs(devices,rules).await;
    let reply=h.request(Action::SceneAction {controller:h.controller,revision:1,scene:"act".into(),execute:true}).await.unwrap();
    let Reply::HouseholdAccepted(accepted)=reply else{panic!("expected managed module sequence")};
    assert_eq!(accepted.commands.len(),8);
    assert_eq!(h.actor.household_query_scopes(h.controller,h.generation.clone()).await.unwrap(),[("bus".into(),None)].into(),"queries wait for the passive feedback window");
    assert_eq!(h.count("command_journal"),8,"future module has no accepted command or ticking send lease");
    assert_eq!(h.wire().await,[0xae,0x15,0,100,0xee]);
    household_reply(&mut h,0x15,true,1..=7).await;
    assert_eq!(h.count("command_journal"),8);assert!(h.wire().await.is_empty());
    household_reply(&mut h,0x15,false,8..=8).await;
    assert_eq!(h.count("command_journal"),8,"a reply with wrong target must not release next module");
    assert!(h.actor.reserve_module_read("bus".into(),Some(3),h.actor.monotonic_ms()+100).await.is_err(),"an unrelated module cannot steal this bus transaction");
    household_reply(&mut h,0x15,true,8..=8).await;
    assert_eq!(h.count("command_journal"),16);
    assert_eq!(h.wire().await,[0xae,3,0,0xf1,0xee]);
    household_reply(&mut h,3,true,1..=8).await;
    let views=h.actor.household_views(h.controller,h.generation.clone()).await.unwrap();
    assert_eq!(views.len(),1);assert_eq!(views[0].state,"confirmed");assert_eq!(views[0].confirmed_modules,2);
    h.finish().await;
}

#[tokio::test]
async fn whole_house_on_only_current_module_can_acquire_missing_feedback_read() {
    let (devices,rules)=household_inputs();let mut h=Harness::from_inputs(devices,rules).await;
    h.request(Action::SceneAction {controller:h.controller,revision:1,scene:"act".into(),execute:true}).await.unwrap();
    assert_eq!(h.wire().await,[0xae,0x15,0,100,0xee]);
    tokio::time::sleep(Duration::from_millis(120)).await;
    h.actor.tick_household(h.controller,h.generation.clone(),h.feedback.away_observation(h.actor.monotonic_ms()).unwrap(),h.feedback.last_frames()).await.unwrap();
    assert_eq!(h.actor.household_query_scopes(h.controller,h.generation.clone()).await.unwrap(),[("bus".into(),Some(0x15))].into());
    assert!(h.actor.reserve_module_read("bus".into(),Some(3),h.actor.monotonic_ms()+500).await.is_err());
    let permit=h.actor.reserve_module_read("bus".into(),Some(0x15),h.actor.monotonic_ms()+500).await.unwrap();
    household_reply(&mut h,0x15,true,1..=8).await;
    assert_eq!(h.actor.household_query_scopes(h.controller,h.generation.clone()).await.unwrap(),[("bus".into(),None)].into(),"complete feedback blocks another query while the existing reader drains");
    assert!(h.wire().await.is_empty());drop(permit);
    h.actor.tick_household(h.controller,h.generation.clone(),h.feedback.away_observation(h.actor.monotonic_ms()).unwrap(),h.feedback.last_frames()).await.unwrap();
    assert_eq!(h.wire().await,[0xae,3,0,0xf1,0xee]);
    h.finish().await;
}

#[tokio::test]
async fn whole_house_on_missing_port_retries_current_module_with_fresh_ids_and_stops() {
    let (devices,rules)=household_inputs();let mut h=Harness::from_inputs(devices,rules).await;
    h.request(Action::SceneAction {controller:h.controller,revision:1,scene:"act".into(),execute:true}).await.unwrap();
    let mut wire=h.wire().await;
    let until=tokio::time::Instant::now()+Duration::from_secs(4);
    loop {
        household_reply(&mut h,0x15,true,1..=7).await;
        wire.extend(h.wire().await);
        let views=h.actor.household_views(h.controller,h.generation.clone()).await.unwrap();
        let needs=h.actor.away_read_needs(h.controller,h.generation.clone()).await.unwrap();
        assert!(needs.iter().all(|r|r.device.as_str().starts_with("dim-")),"only the active module may be queried");
        if views[0].state=="failed" {assert!(views[0].reason.as_ref().unwrap().contains("NoConfirmation"));break;}
        assert!(tokio::time::Instant::now()<until,"missing-feedback sequence did not terminate");
        tokio::time::sleep(Duration::from_millis(15)).await;
    }
    assert_eq!(wire,[0xae,0x15,0,100,0xee].repeat(3),"retries must not activate the next module");
    assert_eq!(h.count("command_journal"),24,"three attempts each retain eight distinct audit ids");
    let db=rusqlite::Connection::open(h.directory.join("runtime.sqlite")).unwrap();
    let expired:i64=db.query_row("SELECT count(*) FROM command_journal WHERE cancellation_reason='Expired'",[],|r|r.get(0)).unwrap();
    assert_eq!(expired,0,"unstarted modules must not expire while another module is retried");
    drop(db);h.finish().await;
}

#[tokio::test]
async fn whole_house_on_manual_override_cancels_future_modules_without_replay() {
    let (devices,rules)=household_inputs();let mut h=Harness::from_inputs(devices,rules).await;
    h.request(Action::SceneAction {controller:h.controller,revision:1,scene:"act".into(),execute:true}).await.unwrap();
    assert_eq!(h.wire().await,[0xae,0x15,0,100,0xee]);
    h.request(Action::DeviceCommand {controller:h.controller,revision:1,device:homesense_core::DeviceId::new("port-8").unwrap(),action:crate::management_devices::DeviceAction::Power {on:false}}).await.unwrap();
    assert_eq!(h.wire().await,[0xae,3,8,0xf2,0xee]);
    let views=h.actor.household_views(h.controller,h.generation.clone()).await.unwrap();
    assert_eq!(views[0].state,"cancelled");
    household_reply(&mut h,0x15,true,1..=8).await;
    assert!(h.wire().await.is_empty(),"a later reply cannot revive a cancelled sequence");
    assert_eq!(h.count("command_journal"),9);
    h.finish().await;
}

#[tokio::test]
async fn whole_house_on_noop_with_older_pending_off_restores_only_that_module() {
    let (devices,rules)=household_inputs();let mut h=Harness::from_inputs(devices,rules).await;
    household_reply(&mut h,0x15,true,1..=8).await;
    household_reply(&mut h,3,true,1..=8).await;
    h.request(Action::DeviceCommand {controller:h.controller,revision:1,device:homesense_core::DeviceId::new("port-8").unwrap(),action:crate::management_devices::DeviceAction::Power {on:false}}).await.unwrap();
    let Reply::HouseholdAccepted(accepted)=h.request(Action::SceneAction {controller:h.controller,revision:1,scene:"act".into(),execute:true}).await.unwrap() else{panic!()};
    assert_eq!(accepted.commands.len(),8,"an older pending Off invalidates an otherwise matching module's no-op proof");
    assert_eq!(h.wire().await,[0xae,3,0,0xf1,0xee],"cancel pending Off and retain the genuinely settled dimmer module");
    household_reply(&mut h,3,true,1..=8).await;
    assert_eq!(h.actor.household_views(h.controller,h.generation.clone()).await.unwrap().last().unwrap().state,"confirmed");
    h.finish().await;
}

#[tokio::test]
async fn whole_house_on_waiting_for_owned_read_has_no_ticking_send_ticket() {
    let (devices,rules)=household_inputs();let mut h=Harness::from_inputs(devices,rules).await;
    let permit=h.actor.reserve_physical_read("bus".into(),h.actor.monotonic_ms()+3000).await.unwrap();
    let Reply::HouseholdAccepted(accepted)=h.request(Action::SceneAction {controller:h.controller,revision:1,scene:"act".into(),execute:true}).await.unwrap() else{panic!()};
    assert!(accepted.commands.is_empty());assert_eq!(h.count("command_journal"),0);
    tokio::time::sleep(Duration::from_millis(1100)).await;
    h.actor.tick_household(h.controller,h.generation.clone(),h.feedback.away_observation(h.actor.monotonic_ms()).unwrap(),h.feedback.last_frames()).await.unwrap();
    assert_eq!(h.count("command_journal"),0,"waiting for an old read must not exhaust the new module's one-second send lease");
    drop(permit);
    h.actor.tick_household(h.controller,h.generation.clone(),h.feedback.away_observation(h.actor.monotonic_ms()).unwrap(),h.feedback.last_frames()).await.unwrap();
    assert_eq!(h.count("command_journal"),8);
    assert_eq!(h.wire().await,[0xae,0x15,0,100,0xee]);
    h.finish().await;
}

#[tokio::test]
async fn whole_house_on_cannot_delay_stop_or_replay_after_managed_off() {
    use crate::web_auth::{Tokens,Grant,Role};
    let (devices,rules)=household_inputs();
    let mut devices:Json=serde_json::from_slice(&devices).unwrap();
    devices["gateways"][0]["modules"].as_array_mut().unwrap().push(json!({"id":"motor-module","address":"37","profile":"c4_din_pro_v2_7_7","verification":{"status":"simulated"}}));
    devices["devices"].as_array_mut().unwrap().push(json!({"id":"motor","name":"Synthetic motor","module_id":"motor-module","channel":1,"enabled":true,"motor_calibration":{"open_direction":"up","stop_verified":true,"evidence":"synthetic stop","verified_at_unix_ms":1}}));
    let mut h=Harness::from_inputs(serde_json::to_vec(&devices).unwrap(),rules).await;
    h.request(Action::SceneAction {controller:h.controller,revision:1,scene:"act".into(),execute:true}).await.unwrap();
    assert_eq!(h.wire().await,[0xae,0x15,0,100,0xee]);
    h.actor.submit_motor(h.controller,homesense_core::DeviceId::new("motor").unwrap(),homesense_core::MotorAction::Stop).await.unwrap();
    assert_eq!(h.wire().await,[0xae,0x37,1,0xf0,0xee],"urgent STOP may pass the current module reservation without waiting for its feedback");
    let tokens=Arc::new(Tokens::default());let secret="synthetic-household-preempt-secret-0123456789";
    tokens.insert(Uuid::new_v4(),secret,Grant {controller:h.controller,role:Role::Administrator,devices:h.configuration.devices().keys().map(|id|id.as_str().to_owned()).collect(),scenes:["off".into()].into()}).unwrap();
    let action=Action::SceneAction {controller:h.controller,revision:1,scene:"off".into(),execute:true};
    let source=crate::source_auth::Source::capture(tokens.module_off_lease(Some(&format!("Bearer {secret}")),None,None).unwrap(),&action,None).unwrap();
    let control=h.control.clone();let reply=h.drive(tokio::spawn(async move{control.request_authorized(action,source).await})).await.unwrap();
    assert!(matches!(reply,Reply::AwayAccepted(_)));
    let mut first=h.wire().await;assert_eq!(first,[0xae,0x15,0,0,0xee]);
    household_reply(&mut h,0x15,false,1..=8).await;first.extend(h.wire().await);
    household_reply(&mut h,3,false,1..=8).await;
    let mut wire=first.as_chunks::<5>().0.iter().map(|f|f.to_vec()).collect::<Vec<_>>();wire.sort();
    assert_eq!(wire,vec![vec![0xae,3,0,0xf2,0xee],vec![0xae,0x15,0,0,0xee]]);
    household_reply(&mut h,0x15,true,1..=8).await;
    assert_eq!(h.actor.household_views(h.controller,h.generation.clone()).await.unwrap()[0].state,"cancelled");
    assert!(h.wire().await.is_empty(),"late On feedback cannot revive work superseded by managed Off");
    h.finish().await;
}

#[tokio::test]
async fn whole_house_on_rechecks_earlier_confirmed_modules_before_final_success() {
    let (devices,rules)=household_inputs();let mut h=Harness::from_inputs(devices,rules).await;
    h.request(Action::SceneAction {controller:h.controller,revision:1,scene:"act".into(),execute:true}).await.unwrap();
    assert_eq!(h.wire().await,[0xae,0x15,0,100,0xee]);
    household_reply(&mut h,0x15,true,1..=8).await;
    assert_eq!(h.wire().await,[0xae,3,0,0xf1,0xee]);
    household_reply(&mut h,0x15,false,1..=1).await;
    household_reply(&mut h,3,true,1..=8).await;
    let views=h.actor.household_views(h.controller,h.generation.clone()).await.unwrap();
    assert_eq!(views[0].state,"failed","a complete later module cannot mask an earlier target changing");
    assert!(views[0].reason.as_ref().unwrap().contains("target changed"));
    h.finish().await;
}

#[tokio::test]
async fn whole_house_on_status_uses_authorized_scope_even_when_supply_card_is_hidden() {
    let (devices,rules)=household_inputs();let mut h=Harness::from_inputs(devices,rules).await;
    h.request(Action::SceneAction {controller:h.controller,revision:1,scene:"act".into(),execute:true}).await.unwrap();
    let ids=h.configuration.devices().keys().cloned().collect::<Vec<_>>();
    let Reply::Devices(full)=h.request(Action::Devices {controller:h.controller,revision:1,ids:ids.clone()}).await.unwrap() else{panic!()};
    assert!(!full.devices.iter().any(|v|v.id.as_str()=="port-1"),"physical supply remains hidden behind logical light");
    assert_eq!(full.household_operations.len(),1,"hidden display cards must not suppress an authorized operation's push status");
    let Reply::Devices(limited)=h.request(Action::Devices {controller:h.controller,revision:1,ids:ids.into_iter().filter(|id|id.as_str()!="port-8").collect()}).await.unwrap() else{panic!()};
    assert!(limited.household_operations.is_empty(),"partial scope cannot disclose whole-house operations");
    h.finish().await;
}
