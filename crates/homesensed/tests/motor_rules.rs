//! Synthetic reviewed predicates, durable actor, memory bus; no sockets or field configuration.
use homesense_core::{
    DeviceId, ExecutionContext, MotorAction,
    curtain_state::Provenance,
    rule::{Node, Truth, Value},
};
use homesense_storage::Store;
use homesensed::{
    admission::{Admission, Options},
    feedback::Feedback,
    rule_runtime::PreparedRule,
    transport_owner::TransportOwner,
};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use uuid::Uuid;
struct Wire(Arc<Mutex<Vec<Vec<u8>>>>);
impl std::io::Read for Wire {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        Err(std::io::ErrorKind::WouldBlock.into())
    }
}
impl std::io::Write for Wire {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().push(b.to_vec());
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn config(revision: u64, direction: &str) -> serde_json::Value {
    serde_json::json!({"schema_version":2,"revision":revision,"gateways":[{"id":"bus","endpoint":"127.0.0.1:1","modules":[{"id":"motor-module","address":"37","profile":"c4_din_pro_v2_7_7","verification":{"status":"simulated"}},{"id":"relay-module","address":"25","profile":"r8i8_din_pro_v2_7_7","verification":{"status":"simulated"}}]}],"devices":[{"id":"motor","name":"Synthetic motor","module_id":"motor-module","channel":3,"enabled":true,"motor_calibration":{"open_direction":direction,"stop_verified":true,"evidence":"synthetic calibration","verified_at_unix_ms":1},"curtain_state":{"open_travel_ms":1000,"close_travel_ms":1000,"evidence":"synthetic model review","reviewed_at_unix_ms":1}},{"id":"relay","name":"Synthetic relay","module_id":"relay-module","channel":1,"enabled":true}]})
}
fn rules(revision: u64) -> serde_json::Value {
    let guarded = |state: &str| serde_json::json!({"id":state,"enabled":true,"source":"synthetic","review_evidence":"synthetic complete rule review","trigger":{"kind":"startup_synchronized"},"actions":[{"source":format!("{state}-guard"),"action":{"kind":"if","condition":{"kind":"motor_state","device":"motor","state":state,"evidence_policy":"reviewed_command_estimate","max_age_ms":5000,"mapping_review":"synthetic source/target review","evidence_review":"only this synthetic predicate may use this synthetic model"},"then_actions":[{"source":"then-mark","action":{"kind":"set","variable":"mark","value":{"kind":"integer","value":1}}},{"source":"then-output","action":{"kind":"command","device":"relay","command":{"kind":"switch","on":true}}}],"else_actions":[{"source":"else-mark","action":{"kind":"set","variable":"mark","value":{"kind":"integer","value":2}}},{"source":"else-output","action":{"kind":"command","device":"relay","command":{"kind":"switch","on":false}}}]}}]});
    serde_json::json!({"schema_version":1,"configuration_revision":revision,"variables":[{"id":"mark","kind":"integer","lifetime":"persistent"}],"rules":[guarded("moving"),guarded("stopped"),{"id":"ordered","enabled":true,"source":"synthetic","review_evidence":"synthetic ordering review","trigger":{"kind":"startup_synchronized"},"actions":[{"source":"source-open","action":{"kind":"motor","device":"motor","action":"open","mapping_review":"synthetic mapping"}},{"source":"source-stop","action":{"kind":"motor","device":"motor","action":"stop","mapping_review":"synthetic mapping"}},{"source":"source-close","action":{"kind":"motor","device":"motor","action":"close","mapping_review":"synthetic mapping"}}]}]})
}
struct Fixture {
    root: std::path::PathBuf,
    a: Uuid,
    b: Uuid,
    authorized: Arc<AtomicBool>,
    actor: Admission,
    feedback: Feedback,
    wire: TransportOwner<Wire>,
    bytes: Arc<Mutex<Vec<Vec<u8>>>>,
    document: homesense_config::rule_document::Document,
}
impl Fixture {
    async fn new() -> Self {
        Self::with_options(5000, false).await
    }
    async fn with_options(age: u64, own_open: bool) -> Self {
        let root = std::env::temp_dir().join(format!("motor-rules-{}", Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let config_bytes = serde_json::to_vec(&config(1, "down")).unwrap();
        let mut rule_value = rules(1);
        for rule in rule_value["rules"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .take(2)
        {
            rule["actions"][0]["action"]["condition"]["max_age_ms"] = age.into();
        }
        if own_open {
            rule_value["rules"][1]["actions"][0]["action"]["then_actions"].as_array_mut().unwrap().insert(0,serde_json::json!({"source":"own-open","action":{"kind":"motor","device":"motor","action":"open","mapping_review":"synthetic mapping"}}));
        }
        let rule_bytes = serde_json::to_vec(&rule_value).unwrap();
        Store::open(root.join("runtime.sqlite"))
            .unwrap()
            .publish_automation(
                None,
                &config_bytes,
                &rule_bytes,
                ExecutionContext::Simulation,
                &[],
            )
            .unwrap();
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let authorized = Arc::new(AtomicBool::new(true));
        let auth = authorized.clone();
        let actor = Admission::open(
            root.join("runtime.sqlite"),
            Options {
                context: ExecutionContext::Simulation,
                ..Default::default()
            },
            move |id| (id == a && auth.load(Ordering::SeqCst)) || id == b,
        )
        .await
        .unwrap();
        actor.synchronized(1, 1).await.unwrap();
        let configuration =
            homesense_config::Configuration::parse(&config_bytes, ExecutionContext::Simulation)
                .unwrap();
        let document =
            homesense_config::rule_document::Document::parse(&rule_bytes, &configuration).unwrap();
        let feedback = Feedback::new(
            &configuration,
            ExecutionContext::Simulation,
            Uuid::parse_str(actor.boot_id()).unwrap().as_u128(),
            1,
        )
        .unwrap();
        let bytes = Arc::new(Mutex::new(vec![]));
        let wire = TransportOwner::new(
            actor.clone(),
            [(
                "bus".into(),
                hb_transport::Connection::for_simulation(Wire(bytes.clone()), 1),
            )]
            .into(),
        );
        Self {
            root,
            a,
            b,
            authorized,
            actor,
            feedback,
            wire,
            bytes,
            document,
        }
    }
    fn nodes(&self, state: &str) -> Vec<Node> {
        self.document
            .rules
            .iter()
            .find(|r| r.id == state)
            .unwrap()
            .actions
            .clone()
    }
    async fn plan(&self, controller: Uuid, state: &str) -> PreparedRule {
        PreparedRule::prepare_with_feedback(
            &self.actor,
            controller,
            vec!["mark".into()],
            &self.nodes(state),
            Default::default(),
            &self.feedback,
            5000,
        )
        .await
        .unwrap()
    }
    async fn mark(&self) -> Option<Value> {
        self.actor
            .variable_snapshot(
                self.b,
                self.actor.task_generation().await.unwrap(),
                vec!["mark".into()],
            )
            .await
            .unwrap()["mark"]
            .value
            .clone()
    }
    async fn accepted(&self, controller: Uuid, action: MotorAction) -> u64 {
        self.actor
            .submit_motor(controller, DeviceId::new("motor").unwrap(), action)
            .await
            .unwrap()[0]
    }
    async fn write_one(&mut self, id: u64) {
        let result = self.wire.step().await.unwrap().unwrap();
        assert_eq!(result.command_id, id);
        result.result.unwrap();
    }
    async fn manual(&mut self, controller: Uuid, action: MotorAction) {
        let id = self.accepted(controller, action).await;
        self.write_one(id).await;
    }
    async fn finish(self) {
        self.actor.shutdown().await.unwrap();
        std::fs::remove_dir_all(self.root).unwrap();
    }
}
#[tokio::test]
async fn unknown_and_pending_skip_both_branches_written_is_inferred_and_stop_has_valid_else() {
    let mut f = Fixture::new().await;
    for state in ["moving", "stopped"] {
        let p = f.plan(f.a, state).await;
        assert_eq!(p.motor_reads[0].truth, Truth::Unknown);
        assert_eq!(p.unknown_branches, vec![format!("{state}-guard")]);
        assert!(p.commit(&f.actor).await.unwrap().is_empty());
    }
    assert_eq!(f.mark().await, None);
    assert!(f.bytes.lock().unwrap().is_empty());
    let id = f.accepted(f.a, MotorAction::Open).await;
    assert_eq!(
        f.plan(f.a, "moving").await.motor_reads[0].truth,
        Truth::Unknown
    );
    f.write_one(id).await;
    let p = f.plan(f.a, "moving").await;
    assert_eq!(p.motor_reads[0].truth, Truth::True);
    assert_eq!(p.motor_reads[0].provenance, Some(Provenance::Inferred));
    assert!(
        f.feedback
            .snapshot()
            .devices
            .values()
            .all(|s| s.observed.is_none())
    );
    let ids = p.commit(&f.actor).await.unwrap();
    assert_eq!(f.mark().await, Some(Value::Integer(1)));
    f.write_one(ids[0]).await;
    f.manual(f.a, MotorAction::Stop).await;
    let p = f.plan(f.a, "moving").await;
    assert_eq!(p.motor_reads[0].truth, Truth::False);
    let ids = p.commit(&f.actor).await.unwrap();
    assert_eq!(f.mark().await, Some(Value::Integer(2)));
    f.write_one(ids[0]).await;
    assert_eq!(
        f.plan(f.a, "stopped").await.motor_reads[0].truth,
        Truth::True
    );
    f.finish().await;
}
#[tokio::test]
async fn admission_rechecks_pending_written_and_natural_endpoint_before_variables_or_queue() {
    let mut f = Fixture::new().await;
    f.manual(f.a, MotorAction::Open).await;
    let p = f.plan(f.b, "moving").await;
    let id = f.accepted(f.a, MotorAction::Open).await;
    assert!(p.commit(&f.actor).await.is_err());
    assert_eq!(f.mark().await, None);
    f.write_one(id).await;
    let p = f.plan(f.b, "moving").await;
    f.manual(f.a, MotorAction::Open).await;
    assert!(
        p.commit(&f.actor).await.is_err(),
        "same motion with a new Written is different evidence"
    );
    assert_eq!(f.mark().await, None);
    let p = f.plan(f.b, "moving").await;
    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert!(
        p.commit(&f.actor).await.is_err(),
        "natural terminal transition invalidates the old Moving plan"
    );
    assert_eq!(f.mark().await, None);
    assert!(f.actor.dispatch().await.unwrap().is_none());
    let p = f.plan(f.b, "stopped").await;
    assert_eq!(p.motor_reads[0].truth, Truth::True);
    let ids = p.commit(&f.actor).await.unwrap();
    f.write_one(ids[0]).await;
    f.finish().await;
}
#[tokio::test]
async fn revoked_source_cannot_lend_estimates_but_another_subject_can_write_stop() {
    let mut f = Fixture::new().await;
    f.manual(f.a, MotorAction::Open).await;
    let p = f.plan(f.b, "moving").await;
    f.authorized.store(false, Ordering::SeqCst);
    assert!(p.commit(&f.actor).await.is_err());
    assert_eq!(f.mark().await, None);
    assert_eq!(
        f.plan(f.b, "moving").await.motor_reads[0].truth,
        Truth::Unknown
    );
    assert!(
        PreparedRule::prepare_with_feedback(
            &f.actor,
            f.a,
            vec!["mark".into()],
            &f.nodes("moving"),
            Default::default(),
            &f.feedback,
            5000
        )
        .await
        .is_err()
    );
    f.manual(f.b, MotorAction::Stop).await;
    assert_eq!(
        f.plan(f.b, "stopped").await.motor_reads[0].truth,
        Truth::True
    );
    assert_eq!(f.bytes.lock().unwrap().len(), 2);
    f.finish().await;
}
#[tokio::test]
async fn transport_recovery_and_restart_are_unknown_and_do_not_replay_saved_motion() {
    let mut f = Fixture::new().await;
    f.manual(f.a, MotorAction::Open).await;
    let p = f.plan(f.a, "moving").await;
    f.actor.transport_failed().await.unwrap();
    assert!(p.commit(&f.actor).await.is_err());
    f.actor.recover_synchronized(2, 1).await.unwrap();
    assert_eq!(
        f.plan(f.b, "moving").await.motor_reads[0].truth,
        Truth::Unknown
    );
    assert!(f.actor.dispatch().await.unwrap().is_none());
    assert_eq!(f.bytes.lock().unwrap().len(), 1);
    f.actor.clone().shutdown().await.unwrap();
    let b = f.b;
    let next = Admission::open(
        f.root.join("runtime.sqlite"),
        Options {
            context: ExecutionContext::Simulation,
            ..Default::default()
        },
        move |id| id == b,
    )
    .await
    .unwrap();
    next.synchronized(1, 1).await.unwrap();
    let p = PreparedRule::prepare_with_feedback(
        &next,
        b,
        vec!["mark".into()],
        &f.nodes("stopped"),
        Default::default(),
        &f.feedback,
        5000,
    )
    .await
    .unwrap();
    assert_eq!(p.motor_reads[0].truth, Truth::Unknown);
    assert!(next.dispatch().await.unwrap().is_none());
    next.shutdown().await.unwrap();
    std::fs::remove_dir_all(f.root).unwrap();
}
#[tokio::test]
async fn configuration_publication_invalidates_old_evidence_and_changes_only_calibrated_direction()
{
    let mut f = Fixture::new().await;
    f.manual(f.a, MotorAction::Open).await;
    let p = f.plan(f.a, "moving").await;
    f.actor
        .begin_automation_publication(
            serde_json::to_vec(&config(2, "up")).unwrap(),
            serde_json::to_vec(&rules(2)).unwrap(),
            1,
            Duration::from_secs(2),
        )
        .await
        .unwrap();
    assert!(p.commit(&f.actor).await.is_err());
    while let Some(out) = f.wire.step().await.unwrap() {
        out.result.unwrap();
    }
    assert_eq!(f.actor.commit_publication().await.unwrap(), 2);
    f.actor.synchronized(1, 2).await.unwrap();
    assert_eq!(
        f.plan(f.b, "moving").await.motor_reads[0].truth,
        Truth::Unknown
    );
    f.manual(f.b, MotorAction::Open).await;
    assert_eq!(
        f.bytes.lock().unwrap().last().unwrap(),
        &vec![0xae, 0x37, 3, 0xf1, 0xee]
    );
    f.finish().await;
}
#[tokio::test]
async fn event_and_deferred_dispatch_preserve_guard_evidence_sources_and_calibrated_node_order() {
    use homesensed::{
        event_rules::{Binding, EventRules, Trigger},
        scene_scheduler::SceneScheduler,
    };
    let mut f = Fixture::new().await;
    f.manual(f.a, MotorAction::Stop).await;
    let mut events = EventRules::new(
        f.actor.clone(),
        f.a,
        vec![Binding {
            id: "stopped".into(),
            trigger: Trigger::StartupSynchronized,
            variables: vec!["mark".into()],
            actions: f.nodes("stopped"),
        }],
        Default::default(),
        Default::default(),
    )
    .await
    .unwrap();
    let report = events
        .dispatch(Trigger::StartupSynchronized, Some((&f.feedback, 5000)))
        .await;
    assert!(report.failure.is_none(), "{:?}", report.failure);
    assert_eq!(report.completed[0].motor_reads[0].truth, Truth::True);
    f.write_one(report.completed[0].commands[0]).await;
    let mut scheduler = SceneScheduler::new(f.actor.clone(), f.a, 1, 0).unwrap();
    let mut chain =
        homesense_core::rule_chain::Chain::new(77, 0, 60000, Default::default(), 32).unwrap();
    let root = chain.root();
    let outcome = homesensed::deferred_rule::execute(
        &f.actor,
        f.a,
        "ordered",
        &mut chain,
        &root,
        &mut scheduler,
        Some((&f.feedback, 5000)),
    )
    .await
    .unwrap();
    assert_eq!(outcome.command_ids.len(), 3);
    // Explicit Stop retains its place within this reviewed rule batch.
    while let Some(out) = f.wire.step().await.unwrap() {
        out.result.unwrap();
    }
    let mut store = Store::open(f.root.join("runtime.sqlite")).unwrap();
    let page = store.rule_chain(f.actor.boot_id(), 77, 0, 64).unwrap();
    let commands = &page.records[0].commands;
    assert_eq!(
        commands
            .iter()
            .map(|c| c.source.as_deref())
            .collect::<Vec<_>>(),
        vec![
            Some("source-open"),
            Some("source-stop"),
            Some("source-close")
        ]
    );
    let bytes = f.bytes.lock().unwrap().clone();
    assert_eq!(
        &bytes[2..],
        &[
            vec![0xae, 0x37, 3, 0xf2, 0xee],
            vec![0xae, 0x37, 3, 0xf0, 0xee],
            vec![0xae, 0x37, 3, 0xf1, 0xee]
        ]
    );
    drop(store);
    f.finish().await;
}

#[tokio::test]
async fn queued_guard_expiry_cancels_without_wire_and_fresh_guard_still_dispatches() {
    for expired in [true, false] {
        let mut f = Fixture::with_options(100, false).await;
        f.manual(f.a, MotorAction::Stop).await;
        let ids = f.plan(f.a, "stopped").await.commit(&f.actor).await.unwrap();
        assert_eq!(ids.len(), 1);
        if expired {
            tokio::time::sleep(Duration::from_millis(200)).await;
            assert!(f.actor.dispatch().await.unwrap().is_none());
            assert_eq!(
                Store::open(f.root.join("runtime.sqlite"))
                    .unwrap()
                    .command_status(f.actor.boot_id(), ids[0])
                    .unwrap(),
                Some(homesense_storage::journal::Status::Cancelled)
            );
            assert_eq!(f.bytes.lock().unwrap().len(), 1);
        } else {
            f.write_one(ids[0]).await;
            assert_eq!(f.bytes.lock().unwrap().len(), 2);
        }
        f.finish().await;
    }
}
#[tokio::test]
async fn queued_guard_is_invalidated_by_foreign_acceptance_or_written_change() {
    for finish_foreign in [false, true] {
        let mut f = Fixture::new().await;
        f.manual(f.a, MotorAction::Stop).await;
        let ids = f.plan(f.a, "stopped").await.commit(&f.actor).await.unwrap();
        let other = f.accepted(f.b, MotorAction::Open).await;
        let ticket = f.actor.dispatch().await.unwrap().unwrap();
        assert_eq!(
            ticket.command.id, other,
            "old guarded relay must be cancelled before foreign manual motor dispatch"
        );
        f.actor.finish_write(other, finish_foreign).await.unwrap();
        if finish_foreign {
            assert!(f.actor.dispatch().await.unwrap().is_none());
        } else {
            assert!(f.actor.dispatch().await.is_err());
        }
        assert_eq!(
            Store::open(f.root.join("runtime.sqlite"))
                .unwrap()
                .command_status(f.actor.boot_id(), ids[0])
                .unwrap(),
            Some(homesense_storage::journal::Status::Cancelled)
        );
        assert_eq!(f.bytes.lock().unwrap().len(), 1);
        f.finish().await;
    }
}
#[tokio::test]
async fn guarded_own_motor_pending_and_written_preserve_the_fresh_remainder() {
    let mut f = Fixture::with_options(5000, true).await;
    f.manual(f.a, MotorAction::Stop).await;
    let ids = f.plan(f.a, "stopped").await.commit(&f.actor).await.unwrap();
    assert_eq!(ids.len(), 2);
    for id in ids {
        f.write_one(id).await;
    }
    assert_eq!(
        &f.bytes.lock().unwrap()[1..],
        &[
            vec![0xae, 0x37, 3, 0xf2, 0xee],
            vec![0xae, 0x25, 1, 0xf1, 0xee]
        ]
    );
    assert!(f.actor.dispatch().await.unwrap().is_none());
    f.finish().await;
}
#[tokio::test]
async fn foreign_change_during_own_write_cannot_be_absorbed_into_remaining_guard() {
    let mut f = Fixture::with_options(5000, true).await;
    f.manual(f.a, MotorAction::Stop).await;
    let ids = f.plan(f.a, "stopped").await.commit(&f.actor).await.unwrap();
    let ticket = f.actor.dispatch().await.unwrap().unwrap();
    assert_eq!(ticket.command.id, ids[0]);
    let other = f.accepted(f.b, MotorAction::Open).await;
    f.actor.finish_write(ids[0], true).await.unwrap();
    f.write_one(other).await;
    assert!(f.actor.dispatch().await.unwrap().is_none());
    assert_eq!(
        Store::open(f.root.join("runtime.sqlite"))
            .unwrap()
            .command_status(f.actor.boot_id(), ids[1])
            .unwrap(),
        Some(homesense_storage::journal::Status::Cancelled)
    );
    f.finish().await;
}
