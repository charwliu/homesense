//! Native relative phases and the real indicator synchronizer share one actor,
//! canonical feedback and transport owner. All bytes stay in this memory bus.
use super::*;
use crate::{
    indicator_sync::Synchronizer, scene_scheduler::SceneScheduler, transport_owner::TransportOwner,
};
use homesense_hap::requests::{Batch, Intent};
use std::collections::{BTreeMap, VecDeque};

#[derive(Default)]
struct Bus {
    input: VecDeque<u8>,
    frames: Vec<[u8; 5]>,
    leds: BTreeMap<(u8, u8), bool>,
}
struct MemoryWire(Arc<Mutex<Bus>>);
impl Read for MemoryWire {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        let mut bus = self.0.lock().unwrap();
        if bus.input.is_empty() {
            return Err(std::io::ErrorKind::WouldBlock.into());
        }
        let count = out.len().min(bus.input.len());
        for byte in &mut out[..count] {
            *byte = bus.input.pop_front().unwrap();
        }
        Ok(count)
    }
}
impl Write for MemoryWire {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let(frames,tail)=bytes.as_chunks::<5>();assert!(tail.is_empty());
        for &frame in frames{
        assert_eq!((frame[0], frame[4]), (0xae, 0xee));
        let mut bus = self.0.lock().unwrap();
        if [0x41, 0x42].contains(&frame[1]) {
            assert!([0xf1, 0xf2].contains(&frame[3]));
            bus.leds.insert((frame[1], frame[2]), frame[3] == 0xf1);
        }
        bus.frames.push(frame);
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn fixture() -> (Vec<u8>, Json) {
    let (config, mut rules) = inputs_native(false);
    let mut config: Json = serde_json::from_slice(&config).unwrap();
    config["schema_version"] = json!(2);
    let roles = [
        "dimmer-module",
        "supply-module",
        "primary-panel",
        "secondary-panel",
    ];
    assert_eq!(
        config["gateways"][0]["modules"].as_array().unwrap().len(),
        roles.len()
    );
    for (module, role) in config["gateways"][0]["modules"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .zip(roles)
    {
        module["id"] = json!(role);
    }
    for device in config["devices"].as_array_mut().unwrap() {
        device["module_id"] = json!(match device["id"].as_str().unwrap() {
            "light" | "down" => "dimmer-module",
            "switch" => "supply-module",
            "key1" | "key3" | "key5" => "primary-panel",
            "other-led" => "secondary-panel",
            _ => panic!("unreviewed fixture role"),
        });
        device.as_object_mut().unwrap().remove("gateway");
        device.as_object_mut().unwrap().remove("address");
    }
    rules["indicator_links"] = json!([
        {"id":"first-fanout","enabled":true,"review_evidence":"synthetic multi-panel link","scene":"first","lights":["light","down"],"leds":["key1","other-led"]},
        {"id":"independent-switch","enabled":true,"review_evidence":"synthetic unrelated light","lights":["switch"],"leds":["key5"]}
    ]);
    (serde_json::to_vec(&config).unwrap(), rules)
}
async fn setup() -> (
    Harness,
    TransportOwner<MemoryWire>,
    Arc<Mutex<Bus>>,
    SceneScheduler,
) {
    setup_with_supply(false).await
}
async fn setup_with_supply(
    combined: bool,
) -> (
    Harness,
    TransportOwner<MemoryWire>,
    Arc<Mutex<Bus>>,
    SceneScheduler,
) {
    let (config, rules) = fixture();
    let config = if combined {
        let mut value: Json = serde_json::from_slice(&config).unwrap();
        // The visible independent switch stays on channel 1. Channel 2 is an
        // explicit hidden power link, referenced only by stable module role.
        value["devices"][0]["supply"] = json!({"module_id":"supply-module","channel":2});
        value["devices"][0]["default_on_level"] = json!(100);
        value["devices"].as_array_mut().unwrap().push(json!({
            "id":"power-port","name":"Synthetic hidden power port",
            "module_id":"supply-module","channel":2,"enabled":true
        }));
        serde_json::to_vec(&value).unwrap()
    } else {
        config
    };
    let h = harness(config, rules).await;
    let bus = Arc::new(Mutex::new(Bus::default()));
    let owner = TransportOwner::new(
        h.actor.clone(),
        [(
            "bus".into(),
            hb_transport::Connection::for_simulation(
                MemoryWire(bus.clone()),
                h.generation.epochs.transport,
            ),
        )]
        .into(),
    );
    let scenes =
        SceneScheduler::new(h.actor.clone(), h.controller, 8, h.actor.monotonic_ms()).unwrap();
    // A known local history does not authorize guessing current physical state.
    Store::open(h.directory.join("runtime.sqlite"))
        .unwrap()
        .accept_queue_batch_with_variables(
            "synthetic-history",
            &[],
            &[],
            0,
            None,
            &[homesense_storage::variables::Update {
                revision: 1,
                name: "selection".into(),
                expected_version: 0,
                value: Some(homesense_core::rule::Value::Integer(1)),
            }],
        )
        .unwrap();
    h.actor
        .submit_rule_update(
            h.generation.clone(),
            Batch {
                controller: h.controller,
                intents: ["key1", "other-led", "key5"]
                    .map(|name| Intent {
                        device: DeviceId::new(name).unwrap(),
                        command: Command::PanelLed(true),
                    })
                    .into(),
                subscriptions: vec![],
            },
            vec![],
        )
        .await
        .unwrap();
    (h, owner, bus, scenes)
}

#[tokio::test]
async fn combined_supply_on_queries_then_dimmer_and_external_supply_off_clear_only_linked_leds() {
    combined_joint(true).await;
}
#[tokio::test]
async fn combined_supply_off_skips_dimmer_and_waits_for_complete_feedback_before_led_clear() {
    combined_joint(false).await;
}
async fn combined_joint(powered: bool) {
    let (mut h, mut owner, bus, scenes) = setup_with_supply(true).await;
    drain_owner(&h, &mut owner).await;
    Store::open(h.directory.join("runtime.sqlite"))
        .unwrap()
        .remember_light_levels(
            1,
            &[(DeviceId::new("light").unwrap(), Percent::new(60).unwrap())],
        )
        .unwrap();
    let begin = bus.lock().unwrap().frames.len();
    receive(
        &mut h,
        &mut owner,
        &bus,
        &[(0x15, 0xd2, 100), (3, 0xd1, 0xf1)],
    )
    .await;
    reconcile(&h, &scenes).await;
    let mut manager = Manager::default();
    let mut indicators = Synchronizer::default();
    start(&h, &mut manager, "first", "combined-joint").await;
    drain_owner(&h, &mut owner).await;
    let initial = bus.lock().unwrap().frames[begin..].to_vec();
    assert!(initial.contains(&[0xae, 3, 0xa2, 0xf1, 0xee]));
    assert!(
        !initial.iter().any(|f| f[1] == 0x15 && f[2] == 1),
        "no dimmer target before relay observation"
    );
    manager.tick(&h.actor, &h.document, &h.feedback).await;
    drain_owner(&h, &mut owner).await;
    let relay_query = bus.lock().unwrap().frames[begin..].to_vec();
    assert!(relay_query.contains(&[0xae, 3, 0xa2, 0xf2, 0xee]));
    let waiting = bus.lock().unwrap().frames.len();
    for _ in 0..3 {
        manager.tick(&h.actor, &h.document, &h.feedback).await;
        sync(&h, &scenes, &mut indicators).await;
        drain_owner(&h, &mut owner).await;
    }
    assert_eq!(
        start(&h, &mut manager, "first", "combined-joint")
            .await
            .status,
        "pending"
    );
    assert_eq!(
        bus.lock().unwrap().frames.len(),
        waiting,
        "waiting for relay does not replay Toggle or clear LEDs"
    );
    receive(
        &mut h,
        &mut owner,
        &bus,
        &[(3, 0xd2, if powered { 0xf1 } else { 0xf2 })],
    )
    .await;
    manager.tick(&h.actor, &h.document, &h.feedback).await;
    drain_owner(&h, &mut owner).await;
    let conditional = bus.lock().unwrap().frames[waiting..].to_vec();
    assert_eq!(
        conditional,
        if powered {
            vec![[0xae, 0x15, 1, 100, 0xee]]
        } else {
            vec![]
        }
    );
    sync(&h, &scenes, &mut indicators).await;
    drain_owner(&h, &mut owner).await;
    assert!(
        led_frames(&bus, begin).is_empty(),
        "relay result alone does not settle the composite light"
    );
    manager.tick(&h.actor, &h.document, &h.feedback).await;
    drain_owner(&h, &mut owner).await;
    let final_queries = bus.lock().unwrap().frames[waiting + conditional.len()..].to_vec();
    assert_eq!(final_queries.len(), 3);
    assert!(
        final_queries
            .iter()
            .all(|f| f[3] == 0xf2 && [0xa1, 0xa2].contains(&f[2]))
    );
    // Relay and dimmer are fresh, but the scene's unused downlight has no final
    // query result yet. Intermediate zero must not clear either panel.
    receive(
        &mut h,
        &mut owner,
        &bus,
        &[
            (0x15, 0xd1, 0),
            (3, 0xd2, if powered { 0xf1 } else { 0xf2 }),
        ],
    )
    .await;
    manager.tick(&h.actor, &h.document, &h.feedback).await;
    reconcile(&h, &scenes).await;
    assert_eq!(
        row(&h, "combined-joint").await.status,
        homesense_storage::native_scene::Status::Pending
    );
    assert!(h.selection().await.physical.unwrap().pending_relative);
    for _ in 0..3 {
        sync(&h, &scenes, &mut indicators).await;
        drain_owner(&h, &mut owner).await;
    }
    assert!(led_frames(&bus, begin).is_empty());
    let actual = if powered { 100 } else { 60 };
    receive(
        &mut h,
        &mut owner,
        &bus,
        &[
            (0x15, 0xd1, actual),
            (0x15, 0xd2, 0),
            (3, 0xd2, if powered { 0xf1 } else { 0xf2 }),
        ],
    )
    .await;
    manager.tick(&h.actor, &h.document, &h.feedback).await;
    reconcile(&h, &scenes).await;
    assert_eq!(
        row(&h, "combined-joint").await.status,
        homesense_storage::native_scene::Status::Observed
    );
    assert_eq!(
        row(&h, "combined-joint").await.selected,
        Some(if powered { 1 } else { 0 })
    );
    sync(&h, &scenes, &mut indicators).await;
    drain_owner(&h, &mut owner).await;
    if powered {
        assert_eq!(led_frames(&bus, begin),vec![[0xae,0x41,1,0xf1,0xee],[0xae,0x42,1,0xf1,0xee]],"completed selected scene reasserts its reviewed LEDs");
        receive(&mut h, &mut owner, &bus, &[(3, 0xd2, 0xf2)]).await;
        reconcile(&h, &scenes).await;
        sync(&h, &scenes, &mut indicators).await;
        drain_owner(&h, &mut owner).await;
    } else {
        assert_eq!(
            Store::open(h.directory.join("runtime.sqlite"))
                .unwrap()
                .light_restore_levels(1)
                .unwrap()[&DeviceId::new("light").unwrap()]
                .get(),
            60
        );
    }
    assert_eq!(
        h.selection().await.stored.value,
        Some(homesense_core::rule::Value::Integer(0))
    );
    let settled = bus.lock().unwrap().frames.len();
    for _ in 0..3 {
        manager.tick(&h.actor, &h.document, &h.feedback).await;
        sync(&h, &scenes, &mut indicators).await;
        drain_owner(&h, &mut owner).await;
    }
    assert_eq!(
        start(&h, &mut manager, "first", "combined-joint")
            .await
            .status,
        "observed"
    );
    assert_eq!(bus.lock().unwrap().frames.len(), settled);
    let mut leds = led_frames(&bus, begin);
    leds.sort();
    assert_eq!(leds,if powered {
        vec![[0xae,0x41,1,0xf1,0xee],[0xae,0x41,1,0xf2,0xee],[0xae,0x42,1,0xf1,0xee],[0xae,0x42,1,0xf2,0xee]]
    } else {vec![[0xae,0x41,1,0xf2,0xee],[0xae,0x42,1,0xf2,0xee]]});
    assert!(bus.lock().unwrap().leds[&(0x41, 5)]);
    let frames = bus.lock().unwrap().frames[begin..].to_vec();
    assert_eq!(
        frames
            .iter()
            .filter(|f| **f == [0xae, 3, 0xa2, 0xf1, 0xee])
            .count(),
        1
    );
    assert!(
        !frames.iter().any(|f| f[1] == 3 && f[2] == 1),
        "independent channel 1 not actuated"
    );
    assert_eq!(
        h.feedback.snapshot().devices[&DeviceId::new("switch").unwrap()].observed,
        Some(homesense_core::Observation::Switch(true))
    );
    let module = h.feedback.module_truth();
    assert_eq!(
        module.located_state("bus", 3, 2).unwrap().observed,
        Some(homesense_core::Observation::Switch(false))
    );
    trace(
        &bus,
        if powered {
            "combined-on-then-supply-off"
        } else {
            "combined-supply-off"
        },
    );
    drop(owner);
    h.close().await;
}
async fn drain_owner(h: &Harness, owner: &mut TransportOwner<MemoryWire>) {
    for _ in 0..100 {
        if let Some(outcome) = owner.step().await.unwrap() {
            outcome.result.unwrap();
        }
        let d = h
            .actor
            .diagnostics(h.controller, h.generation.clone())
            .await
            .unwrap();
        if d.queued == 0 && !d.in_flight {
            return;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    panic!("real memory transport owner did not drain");
}
async fn receive(
    h: &mut Harness,
    owner: &mut TransportOwner<MemoryWire>,
    bus: &Arc<Mutex<Bus>>,
    samples: &[(u8, u8, u8)],
) {
    tokio::time::sleep(Duration::from_millis(3)).await;
    bus.lock().unwrap().input.extend(
        samples
            .iter()
            .flat_map(|&(address, command, value)| [0xae, address, command, value, 0xee]),
    );
    let read = owner.receive("bus").await.unwrap();
    h.feedback.apply(&read).unwrap();
}
async fn sync(h: &Harness, scenes: &SceneScheduler, sync: &mut Synchronizer) {
    sync.tick(
        &h.actor,
        h.controller,
        &h.document,
        scenes,
        &h.feedback,
        5000,
    )
    .await
    .unwrap();
}
async fn reconcile(h: &Harness, scenes: &SceneScheduler) {
    crate::scene_reconciliation::tick(&h.actor, h.controller, &h.document, scenes, &h.feedback)
        .await
        .unwrap();
}
fn led_frames(bus: &Arc<Mutex<Bus>>, after: usize) -> Vec<[u8; 5]> {
    bus.lock().unwrap().frames[after..]
        .iter()
        .filter(|f| [0x41, 0x42].contains(&f[1]))
        .copied()
        .collect()
}
fn trace(bus: &Arc<Mutex<Bus>>, name: &str) {
    let frames = bus.lock().unwrap().frames.clone();
    println!(
        "JOINT_TRACE {name} {}",
        serde_json::to_string(&frames).unwrap()
    );
}

#[tokio::test]
async fn captured_indicator_plan_is_rejected_if_native_pending_begins_before_commit() {
    use homesense_core::rule::{Action, Budget, Condition, Expr, Node, Value};
    let (mut h, mut owner, bus, scenes) = setup().await;
    drain_owner(&h, &mut owner).await;
    let begin = bus.lock().unwrap().frames.len();
    receive(
        &mut h,
        &mut owner,
        &bus,
        &[(0x15, 0xd1, 0), (3, 0xd1, 0xf1)],
    )
    .await;
    reconcile(&h, &scenes).await;
    let nodes = vec![Node {
        source: "synthetic captured off".into(),
        action: Action::If {
            condition: Condition::Equal(
                Expr::DeviceOn(DeviceId::new("light").unwrap()),
                Expr::Literal(Value::Bool(false)),
            ),
            then_actions: ["key1", "other-led"]
                .map(|name| Node {
                    source: "synthetic reviewed fanout".into(),
                    action: Action::Command {
                        device: DeviceId::new(name).unwrap(),
                        command: Command::PanelLed(false),
                    },
                })
                .into(),
            else_actions: vec![],
        },
    }];
    let mut lease = None;
    let mut prepared = crate::rule_runtime::PreparedRule::prepare_indicator(
        &h.actor,
        h.controller,
        h.generation.clone(),
        vec!["selection".into()],
        |variables, selections, policy| {
            assert!(
                !selections["selection"]
                    .physical
                    .as_ref()
                    .unwrap()
                    .pending_relative
            );
            let (plan, read) = h.feedback.plan_indicator_rule(
                &nodes,
                variables,
                Budget::default(),
                h.actor.monotonic_ms(),
                5000,
                &BTreeMap::new(),
                policy,
            )?;
            assert_eq!(plan.commands.len(), 2);
            lease = Some(read);
            Ok(plan)
        },
    )
    .await
    .unwrap_or_else(|_| panic!("captured indicator plan must be valid before native start"));
    prepared.attach_feedback(lease);
    let mut manager = Manager::default();
    start(&h, &mut manager, "first", "snapshot-race").await;
    assert!(h.selection().await.physical.unwrap().pending_relative);
    // The actor recalculates physical selection (including the pending marker)
    // and compares the complete read snapshot before accepting any LED output.
    assert_eq!(
        prepared.commit_status(&h.actor).await,
        Err(hap::server::WriteStatus::InvalidValueInRequest)
    );
    drain_owner(&h, &mut owner).await;
    assert!(led_frames(&bus, begin).is_empty());
    assert_eq!(
        Store::open(h.directory.join("runtime.sqlite"))
            .unwrap()
            .logical_variable(1, "selection")
            .unwrap()
            .unwrap()
            .value,
        Some(Value::Integer(1))
    );
    trace(&bus, "rejected-captured-plan");
    drop(owner);
    h.close().await;
}

#[tokio::test]
async fn pending_relative_defers_only_its_group_not_independent_led_reconciliation() {
    let (mut h, mut owner, bus, scenes) = setup().await;
    drain_owner(&h, &mut owner).await;
    let begin = bus.lock().unwrap().frames.len();
    receive(
        &mut h,
        &mut owner,
        &bus,
        &[(0x15, 0xd2, 100), (3, 0xd1, 0xf1)],
    )
    .await;
    reconcile(&h, &scenes).await;
    let mut manager = Manager::default();
    let mut indicators = Synchronizer::default();
    start(&h, &mut manager, "first", "independent-joint").await;
    drain_owner(&h, &mut owner).await;
    manager.tick(&h.actor, &h.document, &h.feedback).await;
    drain_owner(&h, &mut owner).await;
    receive(
        &mut h,
        &mut owner,
        &bus,
        &[(0x15, 0xd1, 0), (3, 0xd1, 0xf2)],
    )
    .await;
    manager.tick(&h.actor, &h.document, &h.feedback).await;
    reconcile(&h, &scenes).await;
    assert!(h.selection().await.physical.unwrap().pending_relative);
    for _ in 0..3 {
        sync(&h, &scenes, &mut indicators).await;
        drain_owner(&h, &mut owner).await;
    }
    assert_eq!(led_frames(&bus, begin), vec![[0xae, 0x41, 5, 0xf2, 0xee]]);
    {
        let state = bus.lock().unwrap();
        assert!(state.leds[&(0x41, 1)] && state.leds[&(0x42, 1)]);
        assert!(!state.leds[&(0x41, 5)]);
    }
    trace(&bus, "independent-off-while-pending");
    drop(owner);
    h.close().await;
}

#[tokio::test]
async fn pending_relative_partial_off_does_not_clear_multipanel_leds_or_history() {
    let (mut h, mut owner, bus, scenes) = setup().await;
    drain_owner(&h, &mut owner).await;
    let begin = bus.lock().unwrap().frames.len();
    receive(
        &mut h,
        &mut owner,
        &bus,
        &[(0x15, 0xd2, 100), (3, 0xd1, 0xf1)],
    )
    .await;
    reconcile(&h, &scenes).await;
    assert!(h.selection().await.stored.value.is_none());
    let mut manager = Manager::default();
    let mut indicators = Synchronizer::default();
    start(&h, &mut manager, "first", "partial-joint").await;
    drain_owner(&h, &mut owner).await;
    sync(&h, &scenes, &mut indicators).await;
    drain_owner(&h, &mut owner).await;
    manager.tick(&h.actor, &h.document, &h.feedback).await;
    drain_owner(&h, &mut owner).await;
    // Only one post-query output sample exists. The final native phase remains
    // pending even though this fresh intermediate sample is temporarily Off.
    receive(&mut h, &mut owner, &bus, &[(0x15, 0xd1, 0)]).await;
    manager.tick(&h.actor, &h.document, &h.feedback).await;
    reconcile(&h, &scenes).await;
    assert_eq!(
        row(&h, "partial-joint").await.status,
        homesense_storage::native_scene::Status::Pending
    );
    assert!(h.selection().await.physical.unwrap().pending_relative);
    sync(&h, &scenes, &mut indicators).await;
    drain_owner(&h, &mut owner).await;
    trace(&bus, "partial-pending");
    assert!(
        led_frames(&bus, begin).is_empty(),
        "pending partial Off must not clear either panel"
    );
    assert_eq!(
        Store::open(h.directory.join("runtime.sqlite"))
            .unwrap()
            .logical_variable(1, "selection")
            .unwrap()
            .unwrap()
            .value,
        Some(homesense_core::rule::Value::Integer(1))
    );
    receive(
        &mut h,
        &mut owner,
        &bus,
        &[(0x15, 0xd1, 30), (0x15, 0xd2, 0)],
    )
    .await;
    manager.tick(&h.actor, &h.document, &h.feedback).await;
    reconcile(&h, &scenes).await;
    assert_eq!(row(&h, "partial-joint").await.selected, Some(1));
    assert_eq!(
        h.selection().await.stored.value,
        Some(homesense_core::rule::Value::Integer(1))
    );
    sync(&h, &scenes, &mut indicators).await;
    drain_owner(&h, &mut owner).await;
    assert_eq!(led_frames(&bus, begin),vec![[0xae,0x41,1,0xf1,0xee],[0xae,0x42,1,0xf1,0xee]],"only the complete selected pattern permits positive LEDs");
    let frames = bus.lock().unwrap().frames[begin..].to_vec();
    assert_eq!(
        frames
            .iter()
            .filter(|f| **f == [0xae, 0x15, 0xa1, 0xf1, 0xee])
            .count(),
        1
    );
    assert!(
        frames.contains(&[0xae, 0x15, 2, 0, 0xee]),
        "source-unused dimmer explicitly Off"
    );
    assert_eq!(
        h.feedback.snapshot().devices[&DeviceId::new("down").unwrap()].observed,
        Some(homesense_core::Observation::Level(Percent::new(0).unwrap()))
    );
    drop(owner);
    h.close().await;
}

#[tokio::test]
async fn observed_relative_then_unknown_stale_and_external_off_clear_only_linked_leds_once() {
    let (mut h, mut owner, bus, scenes) = setup().await;
    drain_owner(&h, &mut owner).await;
    let begin = bus.lock().unwrap().frames.len();
    receive(
        &mut h,
        &mut owner,
        &bus,
        &[(0x15, 0xd2, 100), (3, 0xd1, 0xf1)],
    )
    .await;
    reconcile(&h, &scenes).await;
    let mut manager = Manager::default();
    let mut indicators = Synchronizer::default();
    start(&h, &mut manager, "first", "off-joint").await;
    drain_owner(&h, &mut owner).await;
    manager.tick(&h.actor, &h.document, &h.feedback).await;
    drain_owner(&h, &mut owner).await;
    receive(
        &mut h,
        &mut owner,
        &bus,
        &[(0x15, 0xd1, 30), (0x15, 0xd2, 0)],
    )
    .await;
    manager.tick(&h.actor, &h.document, &h.feedback).await;
    reconcile(&h, &scenes).await;
    assert_eq!(
        row(&h, "off-joint").await.status,
        homesense_storage::native_scene::Status::Observed
    );
    sync(&h, &scenes, &mut indicators).await;
    drain_owner(&h, &mut owner).await;
    assert_eq!(led_frames(&bus, begin),vec![[0xae,0x41,1,0xf1,0xee],[0xae,0x42,1,0xf1,0xee]],"an intentionally Off downlight preserves positive scene indication");
    let stable = bus.lock().unwrap().frames.len();
    receive(&mut h, &mut owner, &bus, &[(0x15, 0xd1, 101)]).await;
    assert_eq!(
        h.feedback.snapshot().devices[&DeviceId::new("light").unwrap()].quality,
        homesense_state::Quality::Unknown
    );
    for _ in 0..3 {
        manager.tick(&h.actor, &h.document, &h.feedback).await;
        reconcile(&h, &scenes).await;
        sync(&h, &scenes, &mut indicators).await;
        drain_owner(&h, &mut owner).await;
    }
    tokio::time::sleep(Duration::from_millis(3)).await;
    h.feedback.expire(h.actor.monotonic_ms(), 1).unwrap();
    for _ in 0..3 {
        manager.tick(&h.actor, &h.document, &h.feedback).await;
        reconcile(&h, &scenes).await;
        sync(&h, &scenes, &mut indicators).await;
        drain_owner(&h, &mut owner).await;
    }
    assert_eq!(
        start(&h, &mut manager, "first", "off-joint").await.status,
        "observed"
    );
    drain_owner(&h, &mut owner).await;
    assert_eq!(
        bus.lock().unwrap().frames.len(),
        stable,
        "unknown/stale ticks must emit nothing"
    );
    receive(
        &mut h,
        &mut owner,
        &bus,
        &[(0x15, 0xd1, 0), (0x15, 0xd2, 0), (3, 0xd1, 0xf1)],
    )
    .await;
    reconcile(&h, &scenes).await;
    sync(&h, &scenes, &mut indicators).await;
    drain_owner(&h, &mut owner).await;
    assert_eq!(
        h.selection().await.stored.value,
        Some(homesense_core::rule::Value::Integer(0))
    );
    for _ in 0..3 {
        manager.tick(&h.actor, &h.document, &h.feedback).await;
        reconcile(&h, &scenes).await;
        sync(&h, &scenes, &mut indicators).await;
        drain_owner(&h, &mut owner).await;
    }
    let mut leds = led_frames(&bus, stable);
    leds.sort();
    assert_eq!(
        leds,
        vec![[0xae, 0x41, 1, 0xf2, 0xee], [0xae, 0x42, 1, 0xf2, 0xee]]
    );
    assert!(
        bus.lock().unwrap().leds[&(0x41, 5)],
        "unrelated LED remains red"
    );
    assert_eq!(
        h.feedback.snapshot().devices[&DeviceId::new("switch").unwrap()].observed,
        Some(homesense_core::Observation::Switch(true))
    );
    let frames = bus.lock().unwrap().frames[begin..].to_vec();
    assert!(
        !frames.iter().any(|f| f[1] == 3),
        "unrelated light has no writes"
    );
    assert_eq!(
        frames
            .iter()
            .filter(|f| **f == [0xae, 0x15, 0xa1, 0xf1, 0xee])
            .count(),
        1
    );
    trace(&bus, "observed-off");
    drop(owner);
    h.close().await;
}
