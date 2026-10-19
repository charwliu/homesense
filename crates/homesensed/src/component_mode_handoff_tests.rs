//! Owner-level handoff tests; no network connection or physical command.
use super::*;
use crate::admission::Options;
use homesense_core::{ExecutionContext, rule::ValueType};
use std::path::PathBuf;

struct Fixture {
    path: PathBuf,
    store: Store,
    owner: Owner,
    generation: TaskGeneration,
    config: Configuration,
    controller: Uuid,
}
impl Fixture {
    async fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("component-handoff-{}.sqlite", Uuid::new_v4()));
        let mut store = Store::open(&path).unwrap();
        let bundle = store
            .publish_automation(
                None,
                include_bytes!("../../../config/test/native-app-keypad/devices.json"),
                include_bytes!("../../../config/test/native-app-keypad/automation.json"),
                ExecutionContext::Simulation,
                &[],
            )
            .unwrap();
        drop(store);
        let actor = Admission::open(
            path.clone(),
            Options {
                context: ExecutionContext::Simulation,
                ..Default::default()
            },
            |_| true,
        )
        .await
        .unwrap();
        actor.synchronized(1, 1).await.unwrap();
        let generation = actor.task_generation().await.unwrap();
        actor.shutdown().await.unwrap();
        let mut store = Store::open(&path).unwrap();
        store.ensure_component_mode_audit().unwrap();
        let mut owner = Owner::default();
        owner
            .synchronize(
                &mut store,
                &bundle.configuration,
                bundle.automation.as_ref().unwrap(),
                &generation,
                0,
            )
            .unwrap();
        for name in ["mode", "source-mode"] {
            store
                .define_logical_variable(1, name, ValueType::Integer)
                .unwrap();
            store
                .accept_queue_batch_with_variables(
                    "seed",
                    &[],
                    &[],
                    0,
                    None,
                    &[Update {
                        revision: 1,
                        name: name.into(),
                        expected_version: 0,
                        value: Some(Value::Integer(5)),
                    }],
                )
                .unwrap();
        }
        let mut result = Self {
            path,
            store,
            owner,
            generation,
            config: bundle.configuration,
            controller: Uuid::new_v4(),
        };
        let record = result.record("mode");
        result.owner.confirmed.insert(1, record);
        result
    }
    fn record(&self, name: &str) -> Record {
        let policy: Policy = serde_json::from_value(serde_json::json!({
            "id":format!("old-{name}"),"variable":name,"scenes":["synthetic"],
            "first_value":5,"timeout_ms":100,"max_age_ms":100,
            "review_evidence":"Synthetic old physical owner",
            "postconditions":[{"device":"fixture-light","on":true},
                {"device":"fixture-downlight","on":true}]
        }))
        .unwrap();
        Record {
            view: View {
                id: 1,
                generation: self.generation.clone(),
                policies: vec![policy],
                after: BTreeMap::new(),
                continuing: true,
                confirmation_deadline: None,
            },
            controller: self.controller,
            source: None,
            updates: vec![Update {
                revision: 1,
                name: name.into(),
                expected_version: 1,
                value: Some(Value::Integer(5)),
            }],
            guards: vec![],
            prior_known: vec![],
            tickets: vec![],
            written: BTreeMap::new(),
            manual: BTreeMap::from([(DeviceId::new("fixture-light").unwrap(), 0)]),
            wires: vec![
                homesense_core::gateway_scope::WireScope::from_persisted("bus".into(), 1).unwrap(),
            ],
            deadline: 100,
            observed_ms: Some(0),
            completion_fence: None,
        }
    }
    fn hold(&mut self) {
        self.owner.set_completion_holds(vec![ReadCheck {
            revision: 1,
            name: "mode".into(),
            expected_version: 1,
        }]);
    }
    fn value(&self, name: &str) -> Stored {
        self.store.logical_variable(1, name).unwrap().unwrap()
    }
    fn audit(&mut self, now: u64, changed: bool) {
        let manual =
            BTreeMap::from([(DeviceId::new("fixture-light").unwrap(), u64::from(changed))]);
        self.owner
            .audit(
                &mut self.store,
                Some(&self.generation),
                now,
                &|_| true,
                &manual,
                &|_| true,
            )
            .unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.path.display()));
        }
        let _ = std::fs::remove_file(self.path.with_extension("runtime.lock"));
    }
}

fn observed_feedback(f: &Fixture, at: u64) -> Feedback {
    let mut feedback = Feedback::new(
        &f.config,
        ExecutionContext::Simulation,
        Uuid::parse_str(f.generation.boot_id()).unwrap().as_u128(),
        f.generation.epochs.transport,
    )
    .unwrap();
    feed_both(&mut feedback, at);
    feedback
}
fn feed_both(feedback: &mut Feedback, at: u64) {
    feedback
        .apply(&crate::transport_owner::GatewayRead {
            gateway_id: "bus".into(),
            transport_epoch: 1,
            received_at_ms: at,
            result: hb_transport::Receive::Frames {
                transport_epoch: 1,
                frames: vec![
                    hb_protocol::RawFrame {
                        address: 0x15,
                        command: 0xd1,
                        value: 100,
                    },
                    hb_protocol::RawFrame {
                        address: 0x15,
                        command: 0xd2,
                        value: 100,
                    },
                ],
            },
        })
        .unwrap();
}
fn projection(f: &Fixture) -> homesense_storage::scene_observation::Snapshot {
    let mut snapshot = homesense_storage::scene_observation::Snapshot {
        stored: f.value("mode"),
        component_inferred: false,
        evidence: None,
        evidence_current: false,
        initialization: None,
        physical: None,
    };
    f.owner
        .project_selection(&f.store, "mode", &f.generation, &mut snapshot)
        .unwrap();
    snapshot
}

#[tokio::test]
async fn managed_age_bounds_confirmed_deadline_without_inventing_projection_time() {
    let mut f = Fixture::new().await;
    f.owner.confirmed.get_mut(&1).unwrap().view.policies[0].max_age_ms = 5000;
    f.owner.confirmed.get_mut(&1).unwrap().deadline = 5100;
    let mut feedback = observed_feedback(&f, 100);
    feedback.set_managed_max_age_ms(100).unwrap();
    let view = f.owner.confirmed[&1].view.clone();
    let proof = Proof::prepare(&view, &f.config, &feedback, 110).unwrap();
    assert_eq!(
        proof.valid_until, 200,
        "managed freshness must bound the retained owner too"
    );
    assert!(f.owner.apply(&mut f.store, &f.config, proof, 110).unwrap());
    assert_eq!(f.owner.confirmed[&1].deadline, 200);
    assert_eq!(
        projection(&f).physical.unwrap().observed_ms,
        Some(100),
        "an observation timestamp cannot be recovered by subtracting the declared age from a clamped deadline"
    );
    f.audit(200, false);
    assert_eq!(f.value("mode").value, None);
}

#[tokio::test]
async fn tightening_managed_age_expires_an_already_confirmed_cached_owner_as_unknown() {
    let mut f = Fixture::new().await;
    f.owner.confirmed.get_mut(&1).unwrap().view.policies[0].max_age_ms = 5000;
    f.owner.confirmed.get_mut(&1).unwrap().deadline = 5100;
    let mut feedback = observed_feedback(&f, 100);
    let view = f.owner.confirmed[&1].view.clone();
    let proof = Proof::prepare(&view, &f.config, &feedback, 110).unwrap();
    assert!(f.owner.apply(&mut f.store, &f.config, proof, 110).unwrap());
    assert_eq!(f.owner.confirmed[&1].deadline, 5100);
    feedback.set_managed_max_age_ms(50).unwrap();
    let proof = Proof::prepare(&view, &f.config, &feedback, 160).unwrap();
    assert_eq!(
        proof.matched, None,
        "old observations are unknown, never a false condition"
    );
    assert!(!f.owner.apply(&mut f.store, &f.config, proof, 160).unwrap());
    assert_eq!(
        f.value("mode").value,
        None,
        "do not retain a5second proof after its allowed age becomes50ms"
    );
    assert!(f.owner.confirmed.is_empty());
    feedback.set_managed_max_age_ms(5000).unwrap();
    let old_sample = Proof::prepare(&view, &f.config, &feedback, 170).unwrap();
    assert!(
        !f.owner
            .apply(&mut f.store, &f.config, old_sample, 170)
            .unwrap()
    );
    assert_eq!(
        f.value("mode").value,
        None,
        "loosening the policy cannot resurrect a retired gesture's owner"
    );
}

#[tokio::test]
async fn new_feedback_invalidates_an_older_expiration_proof_before_it_can_clear_state() {
    let mut f = Fixture::new().await;
    f.owner.confirmed.get_mut(&1).unwrap().view.policies[0].max_age_ms = 5000;
    f.owner.confirmed.get_mut(&1).unwrap().deadline = 5100;
    let mut feedback = observed_feedback(&f, 100);
    feedback.set_managed_max_age_ms(50).unwrap();
    let view = f.owner.confirmed[&1].view.clone();
    let expired = Proof::prepare(&view, &f.config, &feedback, 160).unwrap();
    feed_both(&mut feedback, 170);
    assert_eq!(
        f.owner.apply(&mut f.store, &f.config, expired, 170),
        Err(W::ResourceBusy)
    );
    assert_eq!(f.value("mode").value, Some(Value::Integer(5)));
    let current = Proof::prepare(&view, &f.config, &feedback, 171).unwrap();
    assert!(
        f.owner
            .apply(&mut f.store, &f.config, current, 171)
            .unwrap()
    );
    assert_eq!(f.owner.confirmed[&1].deadline, 220);
    assert_eq!(projection(&f).physical.unwrap().observed_ms, Some(170));
}

#[tokio::test]
async fn unknown_with_shorter_age_preserves_a_captured_hold_until_its_release() {
    let mut f = Fixture::new().await;
    f.owner.confirmed.get_mut(&1).unwrap().view.policies[0].max_age_ms = 5000;
    f.owner.confirmed.get_mut(&1).unwrap().deadline = 5100;
    let mut feedback = observed_feedback(&f, 100);
    feedback.set_managed_max_age_ms(50).unwrap();
    let view = f.owner.confirmed[&1].view.clone();
    f.hold();
    let proof = Proof::prepare(&view, &f.config, &feedback, 160).unwrap();
    assert!(!f.owner.apply(&mut f.store, &f.config, proof, 160).unwrap());
    assert_eq!(f.value("mode").value, Some(Value::Integer(5)));
    assert_eq!(
        projection(&f).stored.value,
        None,
        "a held value is not presented as current truth"
    );
    f.owner.set_completion_holds(vec![]);
    let proof = Proof::prepare(&view, &f.config, &feedback, 160).unwrap();
    assert!(!f.owner.apply(&mut f.store, &f.config, proof, 160).unwrap());
    assert_eq!(f.value("mode").value, None);
}

#[tokio::test]
async fn temporarily_unknown_snapshot_shortens_existing_deadline_without_refreshing_it() {
    let mut f = Fixture::new().await;
    f.owner.confirmed.get_mut(&1).unwrap().view.policies[0].max_age_ms = 5000;
    f.owner.confirmed.get_mut(&1).unwrap().deadline = 5100;
    let mut feedback = observed_feedback(&f, 100);
    let view = f.owner.confirmed[&1].view.clone();
    let proof = Proof::prepare(&view, &f.config, &feedback, 110).unwrap();
    assert!(f.owner.apply(&mut f.store, &f.config, proof, 110).unwrap());
    feedback.set_managed_max_age_ms(50).unwrap();
    feedback
        .apply(&crate::transport_owner::GatewayRead {
            gateway_id: "bus".into(),
            transport_epoch: 1,
            received_at_ms: 120,
            result: hb_transport::Receive::Frames {
                transport_epoch: 1,
                frames: vec![hb_protocol::RawFrame {
                    address: 0x15,
                    command: 0xd1,
                    value: 255,
                }],
            },
        })
        .unwrap();
    let proof = Proof::prepare(&view, &f.config, &feedback, 121).unwrap();
    assert_eq!(proof.matched, None);
    assert!(!f.owner.apply(&mut f.store, &f.config, proof, 121).unwrap());
    assert_eq!(f.owner.confirmed[&1].deadline, 150);
    assert_eq!(f.owner.confirmed[&1].observed_ms, Some(100));
    f.audit(150, false);
    assert_eq!(f.value("mode").value, None);
}

#[tokio::test]
async fn changing_managed_policy_rejects_a_prepared_confirmation_with_the_old_allowance() {
    let mut f = Fixture::new().await;
    f.owner.confirmed.get_mut(&1).unwrap().view.policies[0].max_age_ms = 5000;
    f.owner.confirmed.get_mut(&1).unwrap().deadline = 5100;
    let mut feedback = observed_feedback(&f, 100);
    feedback.set_managed_max_age_ms(100).unwrap();
    let view = f.owner.confirmed[&1].view.clone();
    let old = Proof::prepare(&view, &f.config, &feedback, 110).unwrap();
    feedback.set_managed_max_age_ms(50).unwrap();
    assert_eq!(
        f.owner.apply(&mut f.store, &f.config, old, 120),
        Err(W::ResourceBusy)
    );
    assert_eq!(f.owner.confirmed[&1].deadline, 5100);
    let current = Proof::prepare(&view, &f.config, &feedback, 120).unwrap();
    assert!(
        f.owner
            .apply(&mut f.store, &f.config, current, 120)
            .unwrap()
    );
    assert_eq!(f.owner.confirmed[&1].deadline, 150);
    assert_eq!(projection(&f).physical.unwrap().observed_ms, Some(100));
}

#[tokio::test]
async fn a_held_version_is_hidden_without_mutation_until_release_or_handoff() {
    let mut f = Fixture::new().await;
    f.hold();
    f.audit(500, true); // The source gesture invalidates the old Full vector.
    assert_eq!(f.value("mode").value, Some(Value::Integer(5)));
    assert_eq!(f.value("mode").version, 1);
    assert!(f.owner.views(&f.store, &f.config).unwrap().is_empty());
    assert!(
        !f.owner.busy(&["mode".into()].into()),
        "a hold is not an explicit-operation lock"
    );
    let mut snapshot = homesense_storage::scene_observation::Snapshot {
        stored: f.value("mode"),
        component_inferred: false,
        evidence: None,
        evidence_current: false,
        initialization: None,
        physical: None,
    };
    assert!(
        f.owner
            .project_selection(&f.store, "mode", &f.generation, &mut snapshot)
            .unwrap()
    );
    assert!(snapshot.component_inferred);
    assert_eq!(snapshot.stored.value, None);
    assert_eq!(snapshot.stored.version, 1);
    f.owner.set_completion_holds(vec![]);
    f.audit(501, true);
    assert_eq!(f.value("mode").value, None);
    assert_eq!(f.value("mode").version, 2);
}

#[tokio::test]
async fn a_hold_never_overrides_revocation_generation_or_wire_failure() {
    for cause in 0..3 {
        let mut f = Fixture::new().await;
        f.hold();
        let current = (cause != 1).then_some(&f.generation);
        f.owner
            .audit(
                &mut f.store,
                current,
                50,
                &|_| cause != 0,
                &BTreeMap::new(),
                &|_| cause != 2,
            )
            .unwrap();
        assert_eq!(f.value("mode").value, None, "cause {cause}");
        assert!(f.owner.confirmed.is_empty());
    }
}

#[tokio::test]
async fn a_new_explicit_version_detaches_the_old_owner_despite_its_hold() {
    let mut f = Fixture::new().await;
    f.hold();
    f.store
        .accept_queue_batch_with_variables(
            "newer",
            &[],
            &[],
            40,
            None,
            &[Update {
                revision: 1,
                name: "mode".into(),
                expected_version: 1,
                value: Some(Value::Integer(3)),
            }],
        )
        .unwrap();
    f.audit(500, true);
    assert_eq!(f.value("mode").value, Some(Value::Integer(3)));
    assert_eq!(f.value("mode").version, 2);
    assert!(f.owner.confirmed.is_empty());
}

#[tokio::test]
async fn holding_one_component_never_freezes_its_independent_source_owner() {
    let mut f = Fixture::new().await;
    let mut source = f.record("source-mode");
    source.view.id = 2;
    f.owner.confirmed.insert(2, source);
    f.hold();
    f.audit(500, true);
    assert_eq!(f.value("mode").value, Some(Value::Integer(5)));
    assert_eq!(f.value("source-mode").value, None);
    assert_eq!(f.owner.confirmed.len(), 1);
}

#[tokio::test]
async fn old_logical_source_dependency_cannot_erase_a_held_target_before_completion_cas() {
    let mut f = Fixture::new().await;
    f.owner.confirmed.clear();
    f.owner.logical.insert(
        1,
        Logical {
            generation: f.generation.clone(),
            controller: f.controller,
            source: None,
            wires: vec![],
            canonical: homesense_storage::component_modes::Canonical {
                update: Update {
                    revision: 1,
                    name: "mode".into(),
                    expected_version: 1,
                    value: Some(Value::Integer(5)),
                },
                policy: "old-full".into(),
                sources: vec![homesense_storage::component_modes::SourceValue {
                    check: ReadCheck {
                        revision: 1,
                        name: "source-mode".into(),
                        expected_version: 1,
                    },
                    value: Value::Integer(5),
                }],
            },
        },
    );
    f.hold();
    f.store
        .accept_queue_batch_with_variables(
            "source-off",
            &[],
            &[],
            40,
            None,
            &[Update {
                revision: 1,
                name: "source-mode".into(),
                expected_version: 1,
                value: Some(Value::Integer(0)),
            }],
        )
        .unwrap();
    f.audit(50, false);
    assert_eq!(f.value("mode").value, Some(Value::Integer(5)));
    f.owner.set_completion_holds(vec![]);
    f.audit(51, false);
    assert_eq!(f.value("mode").value, None);
}

#[tokio::test]
async fn a_previously_fetched_old_proof_cannot_invalidate_or_extend_a_held_owner() {
    let mut f = Fixture::new().await;
    let view = f.owner.views(&f.store, &f.config).unwrap().pop().unwrap();
    let feedback = Feedback::new(
        &f.config,
        ExecutionContext::Simulation,
        Uuid::parse_str(f.generation.boot_id()).unwrap().as_u128(),
        f.generation.epochs.transport,
    )
    .unwrap();
    let mut proof = Proof::prepare(&view, &f.config, &feedback, 1).unwrap();
    proof.matched = Some(false);
    f.hold();
    assert!(!f.owner.apply(&mut f.store, &f.config, proof, 1).unwrap());
    assert_eq!(f.value("mode").value, Some(Value::Integer(5)));
    assert_eq!(f.owner.confirmed[&1].deadline, 100);
}

#[tokio::test]
async fn startup_resets_all_completion_only_targets_across_storage_batch_boundaries() {
    use serde_json::{Value as Json, json};
    let mut config: Json = serde_json::from_slice(include_bytes!(
        "../../../config/test/native-app-keypad/devices.json"
    ))
    .unwrap();
    let mut rules: Json = serde_json::from_slice(include_bytes!(
        "../../../config/test/native-app-keypad/automation.json"
    ))
    .unwrap();
    let template = config["keypads"][0].clone();
    let module_template = config["gateways"][0]["modules"][0].clone();
    config["keypads"] = json!([]);
    for panel in 0..14 {
        let mut keypad = template.clone();
        let module_id = if panel == 0 {
            "panel-module".into()
        } else {
            let mut module = module_template.clone();
            let id = format!("panel-module-{panel}");
            module["id"] = json!(id);
            module["address"] = json!(format!("{:02X}", 0x50 + panel));
            config["gateways"][0]["modules"]
                .as_array_mut()
                .unwrap()
                .push(module);
            id
        };
        keypad["id"] = json!(format!("panel-{panel}"));
        keypad["module_id"] = json!(module_id);
        for key in 1..=6 {
            keypad["keys"][key - 1]["led_endpoint"]["id"] =
                json!(format!("panel-{panel}-key-{key}"));
            if key < 6 {
                let target = panel * 5 + key - 1;
                rules["variables"].as_array_mut().unwrap().push(json!({
                    "id":format!("mode-{target}"),"kind":"integer","lifetime":"persistent"}));
                keypad["keys"][key - 1]["action"] = template["keys"][0]["action"].clone();
                keypad["keys"][key - 1]["selection_completion"] = json!({
                    "id":format!("effect-{target}"),"after":{"device":"fixture-light","on":false},
                    "when":[{"device":"fixture-downlight","on":true}],
                    "variable":format!("mode-{target}"),"value":3,
                    "indicators":[{"device":format!("panel-{panel}-key-6"),"on":true}],
                    "max_age_ms":5000,"review_evidence":"Synthetic startup capacity regression"});
            }
        }
        config["keypads"].as_array_mut().unwrap().push(keypad);
    }
    let path = std::env::temp_dir().join(format!(
        "completion-reset-capacity-{}.sqlite",
        Uuid::new_v4()
    ));
    let mut store = Store::open(&path).unwrap();
    store
        .publish_automation(
            None,
            &serde_json::to_vec(&config).unwrap(),
            &serde_json::to_vec(&rules).unwrap(),
            ExecutionContext::Simulation,
            &[],
        )
        .unwrap();
    for chunk in (0..70).collect::<Vec<_>>().chunks(64) {
        let updates = chunk
            .iter()
            .map(|i| Update {
                revision: 1,
                name: format!("mode-{i}"),
                expected_version: 0,
                value: Some(Value::Integer(3)),
            })
            .collect::<Vec<_>>();
        store
            .accept_queue_batch_with_variables("old-boot", &[], &[], 0, None, &updates)
            .unwrap();
    }
    drop(store);
    let actor = Admission::open(
        path.clone(),
        Options {
            context: ExecutionContext::Simulation,
            ..Default::default()
        },
        |_| true,
    )
    .await
    .unwrap();
    actor.synchronized(1, 1).await.unwrap();
    actor.task_generation().await.unwrap();
    let store = Store::open(&path).unwrap();
    for i in 0..70 {
        let value = store
            .logical_variable(1, &format!("mode-{i}"))
            .unwrap()
            .unwrap();
        assert_eq!(value.value, None, "mode-{i}");
        assert_eq!(value.version, 2, "mode-{i}");
    }
    assert!(
        actor.dispatch().await.unwrap().is_none(),
        "reset never replays outputs"
    );
    actor.shutdown().await.unwrap();
    drop(store);
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
    let _ = std::fs::remove_file(path.with_extension("runtime.lock"));
}

#[tokio::test]
async fn continuing_mode_uses_adaptive_age_but_pending_confirmation_stays_bounded() {
    let mut f=Fixture::new().await;
    let mut feedback=observed_feedback(&f,10);
    feedback.set_managed_max_age_ms(f.config.refresh_policy().observation_max_age_ms()).unwrap();
    let view=f.owner.views(&f.store,&f.config).unwrap().pop().unwrap();
    let now=300_010;
    let proof=Proof::prepare(&view,&f.config,&feedback,now).unwrap();
    assert_eq!(proof.matched,Some(true));
    assert_eq!(proof.observed_ms,Some(10),"waiting does not renew actual samples");
    assert!(f.owner.apply(&mut f.store,&f.config,proof,now).unwrap());
    let mut pending=view.clone();pending.continuing=false;pending.confirmation_deadline=Some(now+100);
    assert_eq!(Proof::prepare(&pending,&f.config,&feedback,now).unwrap().matched,None);
    let targets=crate::physical_queries::plan(&f.config,ExecutionContext::Simulation).unwrap();
    assert!(view.query_requirements(&f.config,&feedback,&targets,now).is_empty());
    assert!(!pending.query_requirements(&f.config,&feedback,&targets,now).is_empty());
    assert!(pending.query_requirements(&f.config,&feedback,&targets,now+100).is_empty());
    let expires=10+f.config.refresh_policy().observation_max_age_ms();
    let proof=Proof::prepare(&view,&f.config,&feedback,expires).unwrap();
    assert_eq!(proof.matched,None);
    assert!(!f.owner.apply(&mut f.store,&f.config,proof,expires).unwrap());
    assert!(f.owner.confirmed.is_empty(),"the published idle bound still expires ownership");
}
