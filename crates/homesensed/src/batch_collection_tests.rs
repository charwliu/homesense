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
        let wire=h.wire().await;let mut actual=wire.as_chunks::<5>().0.iter().map(|frame|frame.to_vec()).collect::<Vec<_>>();actual.sort();
        let mut wanted=vec![vec![0xae,3,0,expected,0xee],vec![0xae,0x15,0,if scene=="act"{100}else{0},0xee]];wanted.sort();assert_eq!(actual,wanted);
    }
    assert_eq!(h.count("command_journal"),18,"sixteen opening channel records then two existing Away module operations");
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
    let wire=h.wire().await;let mut frames=wire.as_chunks::<5>().0.iter().map(|f|f.to_vec()).collect::<Vec<_>>();frames.sort();
    assert_eq!(frames,vec![vec![0xae,3,0,0xf2,0xee],vec![0xae,0x15,0,0,0xee]]);
    assert_eq!(h.count("command_journal"),2);
    let retained=bridge.write_handler.clone();drop(bridge);
    assert_eq!(retained.write_async(member,&body).await,Err(hap::server::WriteStatus::InsufficientPrivileges));
    assert!(h.wire().await.is_empty());
    h.finish().await;
}
