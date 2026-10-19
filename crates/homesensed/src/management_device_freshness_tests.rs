use super::*;
use crate::{admission::Options, transport_owner::GatewayRead};
use homesense_core::{ExecutionContext, Observation, Percent};
use homesense_state::Quality;
use homesense_storage::Store;
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

#[tokio::test]
async fn management_only_snapshot_retires_exact_post_write_target_without_hap_or_indicator_sampler() {
    use homesense_core::{Command, LevelTransition};
    use homesense_hap::requests::{Batch, Intent};
    static TIME: AtomicU64 = AtomicU64::new(0);
    let mut fixture = Fixture::new(false, &TIME).await;
    TIME.store(10, Ordering::Release);
    fixture.admission.submit(Batch {
        controller: fixture.controller,
        intents: vec![Intent { device: fixture.id.clone(), command: Command::SetLevel {
            level: Percent::new(40).unwrap(), transition: LevelTransition::Immediate,
        }}], subscriptions: vec![],
    }).await.unwrap();
    // A matching sample cannot acknowledge Accepted or Writing commands.
    fixture.observe(0x15, 40, 10);
    assert!(fixture.view(5000).await.devices[0].recorded);
    TIME.store(11, Ordering::Release);
    let ticket = fixture.admission.dispatch().await.unwrap().unwrap();
    fixture.observe(0x15, 40, 11);
    assert!(fixture.view(5000).await.devices[0].recorded);
    TIME.store(12, Ordering::Release);
    fixture.admission.finish_write(ticket.command.id, true).await.unwrap();
    // A fresh intermediate ramp value is physical truth, not target completion.
    TIME.store(13, Ordering::Release);
    fixture.observe(0x15, 35, 13);
    let intermediate = fixture.view(5000).await;
    assert!(intermediate.devices[0].recorded);
    assert_eq!(intermediate.devices[0].state.observed, Some(Observation::Level(Percent::new(35).unwrap())));
    assert_eq!(intermediate.devices[0].reported, Some(Observation::Level(Percent::new(40).unwrap())));
    TIME.store(14, Ordering::Release);
    fixture.observe(0x15, 40, 14);
    let complete = fixture.view(5000).await;
    assert_eq!(complete.devices[0].state.quality, Quality::Confirmed);
    assert_eq!(complete.devices[0].state.observed, Some(Observation::Level(Percent::new(40).unwrap())));
    assert_eq!(complete.devices[0].reported, complete.devices[0].state.observed);
    assert!(!complete.devices[0].recorded, "management snapshot must retire a reached target without an auxiliary sampler");
    // Retiring the target must expose a later external physical value immediately.
    TIME.store(15, Ordering::Release);
    fixture.observe(0x15, 25, 15);
    let external = fixture.view(5000).await;
    assert!(!external.devices[0].recorded);
    assert_eq!(external.devices[0].reported, Some(Observation::Level(Percent::new(25).unwrap())));
    fixture.admission.shutdown().await.unwrap();
    for suffix in ["", "-wal", "-shm"] { let _ = std::fs::remove_file(format!("{}{suffix}", fixture.path.display())); }
    let _ = std::fs::remove_file(fixture.path.with_extension("runtime.lock"));
}

struct Fixture {
    path: PathBuf,
    admission: Admission,
    controller: Uuid,
    generation: TaskGeneration,
    feedback: Feedback,
    id: DeviceId,
}
impl Fixture {
    async fn new(combined: bool, time: &'static AtomicU64) -> Self {
        Self::with_policy(combined,time,homesense_config::RefreshPolicy {idle_interval_ms:1000,active_interval_ms:100,backoff_ms:[200,500],quiet_rounds_per_step:2,round_budget_ms:1000,observation_grace_ms:3000}).await
    }
    async fn with_policy(combined:bool,time:&'static AtomicU64,policy:homesense_config::RefreshPolicy)->Self {
        time.store(0, Ordering::Release);
        let path = std::env::temp_dir().join(format!("device-read-age-{}.sqlite", Uuid::new_v4()));
        let mut document = serde_json::json!({"schema_version":2,"revision":1,"gateways":[{"id":"fixture","endpoint":"127.0.0.1:1","modules":[{"id":"dimmer","address":"15","profile":"ad8_din_rs485_v2_2_1","verification":{"status":"simulated"}}]}],"devices":[{"id":"light","name":"Synthetic light","module_id":"dimmer","channel":1,"enabled":true}]});
        if combined {
            document["gateways"][0]["modules"].as_array_mut().unwrap().push(serde_json::json!({"id":"switch","address":"03","profile":"r8i8_din_pro_v2_7_7","verification":{"status":"simulated"}}));
            document["devices"][0]["supply"] = serde_json::json!({"module_id":"switch","channel":1});
            document["devices"][0]["default_on_level"] = 100.into();
            document["devices"].as_array_mut().unwrap().push(serde_json::json!({"id":"supply","name":"Synthetic supply","module_id":"switch","channel":1,"enabled":true}));
        }
        // Preserve the original 5s boundary checks with an explicit tight policy.
        document["refresh_policy"]=serde_json::to_value(policy).unwrap();
        let bytes = serde_json::to_vec(&document).unwrap();
        Store::open(&path)
            .unwrap()
            .publish_configuration(None, &bytes, ExecutionContext::Simulation, &[])
            .unwrap();
        let configuration =
            homesense_config::Configuration::parse(&bytes, ExecutionContext::Simulation).unwrap();
        let controller = Uuid::new_v4();
        let admission = Admission::open_with_test_time(
            path.clone(),
            Options {
                context: ExecutionContext::Simulation,
                ..Default::default()
            },
            move |id| id == controller,
            time,
        )
        .await
        .unwrap();
        admission.synchronized(1, 1).await.unwrap();
        let generation = admission.task_generation().await.unwrap();
        let feedback = Feedback::new(
            &configuration,
            ExecutionContext::Simulation,
            Uuid::parse_str(admission.boot_id()).unwrap().as_u128(),
            1,
        )
        .unwrap();
        let mut fixture = Self {
            path,
            admission,
            controller,
            generation,
            feedback,
            id: DeviceId::new("light").unwrap(),
        };
        fixture.observe(0x15, 37, 0);
        if combined {
            fixture.observe(3, 0xF1, 0);
        }
        fixture
    }
    fn observe(&mut self, address: u8, value: u8, at: u64) {
        self.feedback
            .apply(&GatewayRead {
                gateway_id: "fixture".into(),
                transport_epoch: 1,
                received_at_ms: at,
                result: hb_transport::Receive::Frames {
                    transport_epoch: 1,
                    frames: vec![hb_protocol::RawFrame {
                        address,
                        command: 0xD1,
                        value,
                    }],
                },
            })
            .unwrap();
    }
    async fn view(&self, age: u64) -> DeviceSnapshot {
        snapshot_with_max_age(
            &self.admission,
            self.controller,
            self.generation.clone(),
            vec![self.id.clone()],
            &self.feedback,
            age,
        )
        .await
        .unwrap()
    }
    async fn close(self) {
        self.admission.shutdown().await.unwrap();
        let sql = rusqlite::Connection::open(&self.path).unwrap();
        assert_eq!(
            sql.query_row("SELECT count(*) FROM command_journal", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        drop(sql);
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.path.display()));
        }
        let _ = std::fs::remove_file(self.path.with_extension("runtime.lock"));
    }
}

#[tokio::test]
async fn default_device_read_expires_at_boundary_without_mutating_feedback() {
    static TIME: AtomicU64 = AtomicU64::new(0);
    let fixture = Fixture::new(false, &TIME).await;
    let original = fixture.feedback.snapshot();
    TIME.store(4999, Ordering::Release);
    assert_eq!(
        fixture.view(5000).await.devices[0].state.quality,
        Quality::Confirmed
    );
    TIME.store(5000, Ordering::Release);
    let state = snapshot(
        &fixture.admission,
        fixture.controller,
        fixture.generation.clone(),
        vec![fixture.id.clone()],
        &fixture.feedback,
    )
    .await
    .unwrap();
    assert_eq!(state.devices[0].state.quality, Quality::Stale);
    assert_eq!(
        state.devices[0].state.observed,
        Some(Observation::Level(Percent::new(37).unwrap()))
    );
    assert_eq!(state.devices[0].state.observed_at_ms, Some(0));
    assert_eq!(fixture.feedback.snapshot(), original);
    let modules = crate::management_modules::snapshot(
        &fixture.admission,
        fixture.controller,
        fixture.generation.clone(),
        &fixture.feedback,
        5000,
        0,
        12,
    )
    .await
    .unwrap();
    assert_eq!(modules.modules[0].slots[0].quality, "stale");
    fixture.close().await;
}

#[tokio::test]
async fn read_respects_shorter_policy_and_rejects_invalid_or_future_age() {
    static TIME: AtomicU64 = AtomicU64::new(0);
    let mut fixture = Fixture::new(false, &TIME).await;
    TIME.store(1233, Ordering::Release);
    assert_eq!(
        fixture.view(1234).await.devices[0].state.quality,
        Quality::Confirmed
    );
    TIME.store(1234, Ordering::Release);
    assert_eq!(
        fixture.view(1234).await.devices[0].state.quality,
        Quality::Stale
    );
    assert_eq!(
        fixture.view(5000).await.devices[0].state.quality,
        Quality::Confirmed
    );
    for bound in [0, 5001] {
        assert!(
            snapshot_with_max_age(
                &fixture.admission,
                fixture.controller,
                fixture.generation.clone(),
                vec![fixture.id.clone()],
                &fixture.feedback,
                bound
            )
            .await
            .is_err()
        );
    }
    fixture.observe(0x15, 42, 2000);
    assert_eq!(
        fixture.view(5000).await.devices[0].state.quality,
        Quality::Stale
    );
    fixture.close().await;
}

#[tokio::test]
async fn fresh_dimmer_does_not_hide_expired_supply_and_readback_restores_quality() {
    static TIME: AtomicU64 = AtomicU64::new(0);
    let mut fixture = Fixture::new(true, &TIME).await;
    TIME.store(4900, Ordering::Release);
    fixture.observe(0x15, 41, 4900);
    TIME.store(5000, Ordering::Release);
    let original = fixture.feedback.snapshot();
    let view = fixture.view(5000).await;
    assert_eq!(view.devices.len(), 1);
    assert_eq!(view.devices[0].state.quality, Quality::Stale);
    assert_eq!(view.devices[0].state.observed_at_ms, Some(0));
    assert_eq!(fixture.feedback.snapshot(), original);
    fixture.observe(3, 0xF1, 5000);
    let view = fixture.view(5000).await;
    assert_eq!(view.devices[0].state.quality, Quality::Confirmed);
    assert_eq!(
        view.devices[0].state.observed,
        Some(Observation::Level(Percent::new(41).unwrap()))
    );
    fixture.close().await;
}

#[tokio::test]
async fn three_hour_idle_retains_last_observation_and_still_expires_after_the_policy_margin() {
    static TIME:AtomicU64=AtomicU64::new(0);
    let policy=homesense_config::RefreshPolicy::default();
    let mut fixture=Fixture::with_policy(false,&TIME,policy).await;
    fixture.feedback.set_managed_max_age_ms(policy.observation_max_age_ms()).unwrap();
    fixture.observe(0x15,30,0);
    TIME.store(policy.idle_interval_ms,Ordering::Release);
    let view=snapshot(&fixture.admission,fixture.controller,fixture.generation.clone(),vec![fixture.id.clone()],&fixture.feedback).await.unwrap();
    assert_eq!(view.devices[0].state.quality,Quality::Confirmed);
    assert_eq!(view.devices[0].state.observed_at_ms,Some(0),"never restamp cached feedback");
    TIME.store(policy.observation_max_age_ms(),Ordering::Release);
    let view=snapshot(&fixture.admission,fixture.controller,fixture.generation.clone(),vec![fixture.id.clone()],&fixture.feedback).await.unwrap();
    assert_eq!(view.devices[0].state.quality,Quality::Stale);
    assert_eq!(fixture.feedback.snapshot().devices[&fixture.id].observed_at_ms,Some(0));
    fixture.close().await;
}


#[tokio::test]
async fn relay_feedback_authority_is_not_a_dimmer_target_even_after_written_off() {
    use homesense_core::{Command, LevelTransition};
    use homesense_hap::requests::{Batch, Intent};
    static TIME: AtomicU64 = AtomicU64::new(10);
    let path = std::env::temp_dir().join(format!("authority-kind-{}.sqlite", Uuid::new_v4()));
    let bytes = serde_json::to_vec(&serde_json::json!({
        "schema_version":2,"revision":1,
        "gateways":[{"id":"fixture","endpoint":"127.0.0.1:1","modules":[
            {"id":"relay-module","address":"03","profile":"r8i8_din_pro_v2_7_7","verification":{"status":"simulated"}},
            {"id":"dimmer-module","address":"15","profile":"ad8_din_rs485_v2_2_1","verification":{"status":"simulated"}}
        ]}],
        "devices":[
            {"id":"relay","name":"Synthetic relay","module_id":"relay-module","channel":1,"enabled":true},
            {"id":"dimmer","name":"Synthetic dimmer","module_id":"dimmer-module","channel":1,"enabled":true}
        ]
    })).unwrap();
    Store::open(&path).unwrap().publish_configuration(None,&bytes,ExecutionContext::Simulation,&[]).unwrap();
    let configuration=homesense_config::Configuration::parse(&bytes,ExecutionContext::Simulation).unwrap();
    let controller=Uuid::new_v4();
    let admission=Admission::open_with_test_time(path.clone(),Options {context:ExecutionContext::Simulation,..Default::default()},move |id|id==controller,&TIME).await.unwrap();
    admission.synchronized(1,1).await.unwrap();
    let generation=admission.task_generation().await.unwrap();
    let mut feedback=Feedback::new(&configuration,ExecutionContext::Simulation,Uuid::parse_str(admission.boot_id()).unwrap().as_u128(),1).unwrap();
    let relay=DeviceId::new("relay").unwrap();let dimmer=DeviceId::new("dimmer").unwrap();
    let sample=|feedback:&mut Feedback,at,value|feedback.apply(&GatewayRead {
        gateway_id:"fixture".into(),transport_epoch:1,received_at_ms:at,
        result:hb_transport::Receive::Frames {transport_epoch:1,frames:vec![
            hb_protocol::RawFrame {address:3,command:0xd1,value},
            hb_protocol::RawFrame {address:0x15,command:0xd1,value:25}
        ]}
    }).unwrap();
    sample(&mut feedback,10,0xf1);
    admission.submit(Batch {controller,intents:vec![
        Intent {device:relay.clone(),command:Command::Switch(false)},
        Intent {device:dimmer.clone(),command:Command::SetLevel {level:Percent::new(60).unwrap(),transition:LevelTransition::Immediate}}
    ],subscriptions:vec![]}).await.unwrap();
    let grant=crate::web_auth::Grant {controller,role:crate::web_auth::Role::Administrator,devices:["relay".into(),"dimmer".into()].into(),scenes:Default::default()};
    for after_written in [false,true] {
        if after_written {
            while let Some(ticket)=admission.dispatch().await.unwrap() {admission.finish_write(ticket.command.id,true).await.unwrap();}
            TIME.store(20,Ordering::Release);sample(&mut feedback,20,0xf2);
        }
        // Both actor targets intentionally exist: only the dimmer actually uses
        // that display overlay. Do not remove the relay's internal policy entry.
        let targets=admission.projection_levels(1,feedback.snapshot().cursor.boot_id).await.unwrap();
        assert!(targets.contains_key(&relay) && targets.contains_key(&dimmer));
        let raw=feedback.snapshot();
        let view=snapshot(&admission,controller,generation.clone(),vec![relay.clone(),dimmer.clone()],&feedback).await.unwrap();
        let relay_view=view.devices.iter().find(|v|v.id==relay).unwrap();
        assert!(!relay_view.recorded,"relay authority must remain physical feedback, after_written={after_written}");
        let relay_json=crate::web_api::device_json(relay_view,&grant);
        assert_eq!(relay_json["state_authority"],"feedback");
        assert_eq!(relay_json["observed"]["on"],!after_written);
        assert_eq!(relay_json["reported"],relay_json["observed"]);
        assert_eq!(relay_json["quality"],"Confirmed");
        let dimmer_view=view.devices.iter().find(|v|v.id==dimmer).unwrap();
        assert!(dimmer_view.recorded);
        let dimmer_json=crate::web_api::device_json(dimmer_view,&grant);
        assert_eq!(dimmer_json["state_authority"],"pending_target");
        assert_eq!(dimmer_json["observed"]["level"],25);
        assert_eq!(dimmer_json["reported"]["level"],60);
        assert_eq!(feedback.snapshot(),raw,"view must not alter physical observations");
    }
    let prior=crate::management_events::read_scoped(&admission,controller,generation.clone(),vec![relay.clone(),dimmer.clone()],&feedback,None,"projection-test").await.unwrap();
    let target_fingerprint=crate::web_events::light_projection(&prior.snapshot);
    TIME.store(16010,Ordering::Release);
    let settled=crate::management_events::read_scoped(&admission,controller,generation.clone(),vec![relay.clone(),dimmer.clone()],&feedback,Some(prior.cursor.clone()),"projection-test").await.unwrap();
    assert_eq!(settled.cursor,prior.cursor,"software target expiry must not fabricate a physical cursor");
    assert!(!settled.reset);
    let dimmer_view=settled.snapshot.devices.iter().find(|view|view.id==dimmer).expect("event delta must retain dimmer whose overlay cleared without a physical event");
    assert!(!dimmer_view.recorded);
    assert_eq!(dimmer_view.reported,Some(Observation::Level(Percent::new(25).unwrap())));
    let settled_fingerprint=crate::web_events::light_projection(&settled.snapshot);
    assert_ne!(settled_fingerprint,target_fingerprint,"WSS must publish target/authority convergence even at the same cursor");
    TIME.store(16011,Ordering::Release);sample(&mut feedback,16011,0xf2);
    let quiet=crate::management_events::read_scoped(&admission,controller,generation,vec![relay,dimmer],&feedback,Some(settled.cursor),"projection-test").await.unwrap();
    assert_eq!(crate::web_events::light_projection(&quiet.snapshot),settled_fingerprint,"unchanged repeated readback timestamps must not flood WSS");
    admission.shutdown().await.unwrap();
    for suffix in ["","-wal","-shm"] {let _=std::fs::remove_file(format!("{}{suffix}",path.display()));}
    let _=std::fs::remove_file(path.with_extension("runtime.lock"));
}

#[tokio::test]
async fn same_value_dimmer_confirmation_advances_projection_without_a_physical_cursor() {
    use homesense_core::{Command, LevelTransition, gateway_scope::WireScope};
    use homesense_hap::requests::{Batch, Intent};
    static TIME: AtomicU64 = AtomicU64::new(0);
    let mut fixture=Fixture::new(false,&TIME).await;
    let wire=WireScope::from_persisted("fixture".into(),1).unwrap();
    // Use the actual local-wire owner so received_projection can verify the
    // Written command's wire and the post-write receive witness.
    fixture.admission.activate_gateways(vec![crate::gateway_runtime::Initial {
        wire:wire.clone(),samples:(1..=8).map(|channel|crate::gateway_runtime::Sample {
            requirement:homesense_core::gateway_scope::ReadRequirement {
                port:homesense_core::gateway_scope::Port {address:0x15,channel},before_count:0,sent_at_ms:0,deadline_ms:100
            },count:1,at:0
        }).collect()
    }]).await.unwrap();
    fixture.feedback.enable_local_wires([("fixture".into(),1)].into()).unwrap();
    TIME.store(10,Ordering::Release);
    fixture.admission.submit(Batch {controller:fixture.controller,intents:vec![Intent {
        device:fixture.id.clone(),command:Command::SetLevel {level:Percent::new(37).unwrap(),transition:LevelTransition::Immediate}
    }],subscriptions:vec![]}).await.unwrap();
    let ticket=fixture.admission.dispatch().await.unwrap().unwrap();
    fixture.admission.finish_write(ticket.command.id,true).await.unwrap();
    let prior=crate::management_events::read_scoped(&fixture.admission,fixture.controller,fixture.generation.clone(),vec![fixture.id.clone()],&fixture.feedback,None,"projection-test").await.unwrap();
    assert!(prior.snapshot.devices[0].recorded);
    assert_eq!(prior.snapshot.devices[0].reported,Some(Observation::Level(Percent::new(37).unwrap())));
    let pending=crate::web_events::light_projection(&prior.snapshot);
    TIME.store(20,Ordering::Release);fixture.observe(0x15,37,20);
    fixture.admission.received_projection(wire,vec![ticket.command.id],vec![fixture.id.clone()],20,fixture.feedback.snapshot()).await.unwrap();
    let confirmed=crate::management_events::read_scoped(&fixture.admission,fixture.controller,fixture.generation.clone(),vec![fixture.id.clone()],&fixture.feedback,Some(prior.cursor.clone()),"projection-test").await.unwrap();
    assert_eq!(confirmed.cursor,prior.cursor,"same value feedback does not fabricate a physical event");
    assert_eq!(confirmed.snapshot.devices.len(),1,"cleared overlay must remain in the bounded delta view");
    assert!(!confirmed.snapshot.devices[0].recorded);
    assert_eq!(confirmed.snapshot.devices[0].reported,prior.snapshot.devices[0].reported);
    let settled=crate::web_events::light_projection(&confirmed.snapshot);
    assert_ne!(pending,settled,"the exact WSS send fingerprint must change when only authority settles");
    TIME.store(21,Ordering::Release);fixture.observe(0x15,37,21);
    let quiet=crate::management_events::read_scoped(&fixture.admission,fixture.controller,fixture.generation,vec![fixture.id],&fixture.feedback,Some(confirmed.cursor),"projection-test").await.unwrap();
    assert_eq!(crate::web_events::light_projection(&quiet.snapshot),settled,"quiet same-value readbacks must not cause repeated sends");
    fixture.admission.shutdown().await.unwrap();
    for suffix in ["","-wal","-shm"] {let _=std::fs::remove_file(format!("{}{suffix}",fixture.path.display()));}
    let _=std::fs::remove_file(fixture.path.with_extension("runtime.lock"));
}

#[tokio::test]
async fn repeated_query_failures_are_visible_in_scoped_snapshot_and_recovery_clears_them() {
    static TIME: AtomicU64=AtomicU64::new(0);
    let mut f=Fixture::new(false,&TIME).await;TIME.store(1,Ordering::Release);f.observe(0x15,40,1);
    for at in [10,30] {TIME.store(at,Ordering::Release);f.feedback.missing_query_ports("fixture",0x15,&[1],at).unwrap();}
    assert!(f.view(5000).await.devices[0].communication_error.is_none());
    TIME.store(70,Ordering::Release);f.feedback.missing_query_ports("fixture",0x15,&[1],70).unwrap();
    let snapshot=f.view(5000).await;
    assert_eq!(snapshot.devices[0].communication_error.as_ref().unwrap().consecutive_cycles,3);
    assert_eq!(snapshot.devices[0].state.quality,Quality::Unknown);
    TIME.store(80,Ordering::Release);f.observe(0x15,40,80);
    assert!(f.view(5000).await.devices[0].communication_error.is_none());f.close().await;
}
