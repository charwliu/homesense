//! Physical scene truth and its reviewed LED links; no external key observer.
use super::*;
use crate::{
    indicator_sync::Synchronizer, scene_scheduler::SceneScheduler, transport_owner::TransportOwner,
};
use homesense_core::Command;
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    sync::{Arc, Mutex},
};

fn linked() -> (Vec<u8>, Json) {
    let (config, mut rules) = reconciled();
    let mut config: Json = serde_json::from_slice(&config).unwrap();
    config["schema_version"] = json!(2);
    let roles = [
        "dimmer-role",
        "supply-role",
        "panel-role",
        "other-panel-role",
    ];
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
            "light" | "down" => "dimmer-role",
            "switch" => "supply-role",
            "other-led" => "other-panel-role",
            _ => "panel-role",
        });
        device.as_object_mut().unwrap().remove("gateway");
        device.as_object_mut().unwrap().remove("address");
    }
    rules["scene_observation"]["enabled"] = json!(false);
    rules["indicator_links"] = json!([
        {"id":"reading","enabled":true,"review_evidence":"synthetic linked LEDs","scene":"first","lights":["light","down"],"leds":["key1","other-led"]},
        {"id":"daily","enabled":true,"review_evidence":"synthetic linked LEDs","scene":"second","lights":["light","down"],"leds":["key3"]},
        {"id":"eco","enabled":true,"review_evidence":"synthetic linked LEDs","scene":"third","lights":["light","down"],"leds":["key5"]}
    ]);
    (serde_json::to_vec(&config).unwrap(), rules)
}
struct Wire(Arc<Mutex<Vec<[u8; 5]>>>);
impl Read for Wire {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        Err(std::io::ErrorKind::WouldBlock.into())
    }
}
impl Write for Wire {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().push(b.try_into().unwrap());
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
async fn drain_leds(h: &Harness) -> BTreeMap<(u8, u8), bool> {
    let bytes = Arc::new(Mutex::new(vec![]));
    let mut owner = TransportOwner::new(
        h.actor.clone(),
        [(
            "bus".into(),
            hb_transport::Connection::for_simulation(Wire(bytes.clone()), 1),
        )]
        .into(),
    );
    let mut output = BTreeMap::new();
    while let Some(out) = owner.step().await.unwrap() {
        out.result.unwrap();
    }
    for frame in bytes.lock().unwrap().iter() {
        assert!([0x41, 0x42].contains(&frame[1]));
        assert!([0xf1, 0xf2].contains(&frame[3]));
        output.insert((frame[1], frame[2]), frame[3] == 0xf1);
    }
    output
}
async fn sync(h: &Harness, s: &mut Synchronizer) {
    let scenes =
        SceneScheduler::new(h.actor.clone(), h.controller, 8, h.actor.monotonic_ms()).unwrap();
    s.tick(
        &h.actor,
        h.controller,
        &h.document,
        &scenes,
        &h.feedback,
        5000,
    )
    .await
    .unwrap();
}
#[tokio::test]
async fn startup_reading_and_daily_to_eco_physical_edges_set_all_reviewed_leds() {
    let (config, rules) = linked();
    let mut h = harness(config, rules).await;
    let mut s = Synchronizer::default();
    assert!(!h.document.scene_observation.as_ref().unwrap().enabled);
    history(&h, 2);
    let mut leds = BTreeMap::new();
    for (main, down, selected) in [
        (60, 0, 1),
        (100, 100, 2),
        (0, 100, 3),
        (0, 17, 3),
        (0, 0, 0),
        (60, 0, 1),
    ] {
        levels(&mut h, Some(main), Some(down));
        reconcile(&h).await;
        assert_eq!(
            h.selection().await.stored.value,
            Some(Value::Integer(selected))
        );
        sync(&h, &mut s).await;
        let actual = drain_leds(&h).await;
        leds.extend(actual.clone());
        // The same On/Off pattern at another brightness has no new LED edge.
        if main == 0 && down == 17 {
            assert!(actual.is_empty());
        } else {
            assert_eq!(leds.get(&(0x41, 1)), Some(&(selected == 1)));
            assert_eq!(leds.get(&(0x42, 1)), Some(&(selected == 1)));
            assert_eq!(leds.get(&(0x41, 3)), Some(&(selected == 2)));
            assert_eq!(leds.get(&(0x41, 5)), Some(&(selected == 3)));
        }
        assert_eq!(
            h.sql()
                .query_row(
                    "SELECT value FROM logical_variable WHERE name='selection'",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            2,
            "physical LED synchronization must not rewrite historical intent"
        );
        sync(&h, &mut s).await;
        assert!(drain_leds(&h).await.is_empty());
    }
    assert_eq!(
        h.sql()
            .query_row(
                "SELECT count(*) FROM command_journal WHERE device_id IN ('light','down','switch')",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    h.close().await;
}

#[tokio::test]
async fn unknown_partial_stale_and_owned_pending_never_authorize_positive_led_output() {
    for mode in ["unknown", "partial", "stale", "pending", "pending-off"] {
        let (config, rules) = linked();
        let mut h = harness(config, rules).await;
        let mut s = Synchronizer::default();
        match mode {
            "unknown" => levels(&mut h, None, None),
            "partial" => levels(&mut h, Some(60), None),
            "pending-off" => levels(&mut h, Some(0), Some(0)),
            _ => levels(&mut h, Some(60), Some(0)),
        }
        reconcile(&h).await;
        if mode == "stale" {
            tokio::time::sleep(Duration::from_millis(3)).await;
            h.feedback.expire(h.actor.monotonic_ms(), 1).unwrap();
        }
        if mode.starts_with("pending") {
            h.actor
                .submit(homesense_hap::requests::Batch {
                    controller: h.controller,
                    intents: vec![homesense_hap::requests::Intent {
                        device: homesense_core::DeviceId::new("light").unwrap(),
                        command: Command::SetLevel {
                            level: homesense_core::Percent::new(100).unwrap(),
                            transition: homesense_core::LevelTransition::Immediate,
                        },
                    }],
                    subscriptions: vec![],
                })
                .await
                .unwrap();
        }
        sync(&h, &mut s).await;
        assert_eq!(h.sql().query_row("SELECT count(*) FROM command_journal WHERE device_id IN ('key1','key3','key5','other-led')",[],|r|r.get::<_,i64>(0)).unwrap(),0,"mode={mode}");
        h.close().await;
    }
}
#[tokio::test]
async fn ordinary_light_links_use_current_feedback_not_historical_zero_or_nonzero() {
    for (actual, history, off) in [(0, 100, true), (17, 0, false)] {
        let (config, mut rules) = linked();
        rules["indicator_links"] = json!([{"id":"plain-light","enabled":true,"review_evidence":"synthetic light-only scope","lights":["light"],"leds":["key1"]}]);
        let mut h = harness(config, rules).await;
        let mut store = Store::open(h.directory.join("runtime.sqlite")).unwrap();
        let command = homesense_core::queue::QueuedCommand {
            id: 1,
            device_id: homesense_core::DeviceId::new("light").unwrap(),
            command: Command::SetLevel {
                level: homesense_core::Percent::new(history).unwrap(),
                transition: homesense_core::LevelTransition::Immediate,
            },
            origin: homesense_core::queue::Origin::Hap,
            epochs: h.generation.epochs,
            deadline_ms: 1000,
            run_id: None,
        };
        store
            .accept_commands("synthetic-history", &[command], 0)
            .unwrap();
        store
            .begin_command_write("synthetic-history", 1, 0)
            .unwrap();
        store
            .transition_command(
                "synthetic-history",
                1,
                homesense_storage::journal::Status::Writing,
                homesense_storage::journal::Status::Written,
                1,
            )
            .unwrap();
        levels(&mut h, Some(actual), Some(0));
        let mut s = Synchronizer::default();
        sync(&h, &mut s).await;
        let frames = drain_leds(&h).await;
        assert_eq!(frames.get(&(0x41, 1)), off.then_some(&false));
        assert!(
            !frames.contains_key(&(0x41, 3))
                && !frames.contains_key(&(0x41, 5))
                && !frames.contains_key(&(0x42, 1))
        );
        h.close().await;
    }
}
#[tokio::test]
async fn positive_plan_rejects_changed_physical_selection_without_touching_leds_or_history() {
    let (config, rules) = linked();
    let mut h = harness(config, rules).await;
    history(&h, 2);
    levels(&mut h, Some(60), Some(0));
    reconcile(&h).await;
    let mut lease = None;
    let mut plan = crate::rule_runtime::PreparedRule::prepare_indicator(
        &h.actor,
        h.controller,
        h.generation.clone(),
        vec!["selection".into()],
        |variables, _, policy| {
            let nodes = vec![homesense_core::rule::Node {
                source: "snapshot-positive".into(),
                action: homesense_core::rule::Action::If {
                    condition: homesense_core::rule::Condition::All(vec![
                        homesense_core::rule::Condition::Equal(
                            homesense_core::rule::Expr::DeviceOn(
                                homesense_core::DeviceId::new("light").unwrap(),
                            ),
                            homesense_core::rule::Expr::Literal(Value::Bool(true)),
                        ),
                        homesense_core::rule::Condition::Equal(
                            homesense_core::rule::Expr::DeviceOn(
                                homesense_core::DeviceId::new("down").unwrap(),
                            ),
                            homesense_core::rule::Expr::Literal(Value::Bool(false)),
                        ),
                    ]),
                    then_actions: vec![homesense_core::rule::Node {
                        source: "snapshot-positive-led".into(),
                        action: homesense_core::rule::Action::Command {
                            device: homesense_core::DeviceId::new("key1").unwrap(),
                            command: Command::PanelLed(true),
                        },
                    }],
                    else_actions: vec![],
                },
            }];
            let (plan, read) = h.feedback.plan_indicator_rule(
                &nodes,
                variables,
                Default::default(),
                h.actor.monotonic_ms(),
                5000,
                &BTreeMap::new(),
                policy,
            )?;
            lease = Some(read);
            Ok(plan)
        },
    )
    .await
    .unwrap_or_else(|_| panic!("positive snapshot"));
    plan.attach_feedback(lease);
    levels(&mut h, Some(0), Some(17));
    reconcile(&h).await;
    assert_eq!(
        plan.commit_status(&h.actor).await,
        Err(hap::server::WriteStatus::InvalidValueInRequest)
    );
    assert!(drain_leds(&h).await.is_empty());
    assert_eq!(
        h.sql()
            .query_row(
                "SELECT value FROM logical_variable WHERE name='selection'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        2
    );
    h.close().await;
}

#[tokio::test]
async fn collision_brightness_switches_every_reviewed_led_without_dropping_physical_selection() {
    let (config,mut rules)=linked();let mut config:Json=serde_json::from_slice(&config).unwrap();
    config["gateways"][0]["modules"][0]["profile"]=json!("ad8_din_rs485_v2_2_1");
    rules["scene_switch_groups"][0]["reconciliation"]["policy"]=json!("power_pattern_then_profile_level");
    rules["timed_scenes"][1]["steps"][0]["command"]["value"]=json!(30);
    rules["timed_scenes"][1]["steps"][1]["command"]["value"]=json!(0);
    for scene in rules["timed_scenes"].as_array_mut().unwrap(){for step in scene["steps"].as_array_mut().unwrap(){step["command"]["ramp"]=json!(true);}}
    let parsed=homesense_config::Configuration::parse(&serde_json::to_vec(&config).unwrap(),ExecutionContext::Simulation).unwrap();
    crate::configuration_publish::validate_automation(&parsed,&serde_json::to_vec(&rules).unwrap(),ExecutionContext::Simulation).unwrap();
    let mut h=harness(serde_json::to_vec(&config).unwrap(),rules).await;history(&h,2);
    let mut indicators=Synchronizer::default();
    levels(&mut h,Some(100),Some(0));reconcile(&h).await;sync(&h,&mut indicators).await;
    assert_eq!(h.selection().await.stored.value,Some(Value::Integer(1)),"accepted LED-only writes cannot drop inferred scene");
    assert_eq!(drain_leds(&h).await,[((0x41,1),true),((0x42,1),true),((0x41,3),false),((0x41,5),false)].into());
    assert_eq!(h.selection().await.stored.value,Some(Value::Integer(1)));
    tokio::time::sleep(Duration::from_millis(2)).await;
    levels(&mut h,Some(30),Some(0));reconcile(&h).await;sync(&h,&mut indicators).await;
    assert_eq!(h.selection().await.stored.value,Some(Value::Integer(2)));
    assert_eq!(drain_leds(&h).await,[((0x41,1),false),((0x42,1),false),((0x41,3),true)].into());
    assert_eq!(h.selection().await.stored.value,Some(Value::Integer(2)));
    tokio::time::sleep(Duration::from_millis(2)).await;
    levels(&mut h,Some(0),Some(0));reconcile(&h).await;sync(&h,&mut indicators).await;
    assert_eq!(drain_leds(&h).await,[((0x41,3),false)].into());
    assert_eq!(h.selection().await.stored.value,Some(Value::Integer(0)));
    assert_eq!(h.sql().query_row("SELECT value FROM logical_variable WHERE name='selection'",[],|r|r.get::<_,i64>(0)).unwrap(),2,"physical inference and LEDs never rewrite historical intent");
    assert_eq!(h.sql().query_row("SELECT count(*) FROM command_journal WHERE device_id IN ('light','down','switch')",[],|r|r.get::<_,i64>(0)).unwrap(),0,"no lamp/supply commands may be synthesized");
    for table in ["scene_run","rule_admission"]{assert_eq!(h.sql().query_row(&format!("SELECT count(*) FROM {table}"),[],|r|r.get::<_,i64>(0)).unwrap(),0);}
    h.close().await;
}

#[path = "indicator_cross_writer_tests.rs"]
mod cross_writer;

#[path="indicator_output_fence_tests.rs"]
mod output_fence;
