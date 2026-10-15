use homesense_core::{Command, LevelTransition, Percent};
use homesensed::combined_light::{Output, observed, plan};
fn percent(n: u8) -> Percent {
    Percent::new(n).unwrap()
}
#[test]
fn restore_prepares_dimmer_before_energizing_supply() {
    assert_eq!(
        plan(Command::Switch(true), Some(percent(30)), Some(percent(100))).unwrap(),
        vec![
            (
                Output::Dimmer,
                Command::SetLevel {
                    level: percent(30),
                    transition: LevelTransition::Ramp
                }
            ),
            (Output::Relay, Command::Switch(true))
        ]
    );
    assert_eq!(
        plan(Command::Switch(true), None, Some(percent(100))).unwrap()[0],
        (
            Output::Dimmer,
            Command::SetLevel {
                level: percent(100),
                transition: LevelTransition::Immediate
            }
        )
    );
    assert!(plan(Command::Switch(true), Some(percent(0)), None).is_err());
    assert!(plan(Command::Switch(true), None, Some(percent(0))).is_err());
}
#[test]
fn off_preserves_physical_dimmer_and_never_energizes_relay() {
    for target in [
        Command::Switch(false),
        Command::SetLevel {
            level: percent(0),
            transition: LevelTransition::Immediate,
        },
    ] {
        assert_eq!(
            plan(target, None, None).unwrap(),
            vec![(Output::Relay, Command::Switch(false))]
        );
    }
    assert!(plan(Command::Query, None, None).is_err());
    assert!(plan(Command::PanelLed(true), None, None).is_err());
}
#[test]
fn external_feedback_and_loss_of_either_component_are_reflected() {
    assert_eq!(
        observed(Some(true), Some(percent(0))),
        Some((false, percent(0)))
    );
    assert_eq!(
        observed(Some(false), Some(percent(30))),
        Some((false, percent(30)))
    );
    assert_eq!(
        observed(Some(true), Some(percent(30))),
        Some((true, percent(30)))
    );
    assert_eq!(observed(None, Some(percent(0))), None);
    assert_eq!(observed(Some(false), None), None);
}

#[tokio::test]
async fn projection_hides_supply_and_uses_the_older_feedback_without_mutating_physical_state() {
    use homesense_core::{DeviceId, ExecutionContext, ModuleKind, Observation};
    use homesense_state::{Cursor, DeviceState, Quality, Snapshot};
    let document = br#"{
      "schema_version":1,"revision":1,
      "gateways":[{"id":"bus","endpoint":"127.0.0.1:50007","modules":[
        {"address":"12","profile":"ad8_din_rs485_v2_2_1","verification":{"status":"simulated"}},
        {"address":"03","profile":"r8i8_din_pro_v2_7_7","verification":{"status":"simulated"}}]}],
      "devices":[
        {"id":"light","name":"Light","gateway":"bus","address":"12","channel":4,"enabled":true,"default_on_level":100,"supply_relay":"relay"},
        {"id":"relay","name":"Relay","gateway":"bus","address":"03","channel":4,"enabled":true}]}
      "#;
    let config =
        homesense_config::Configuration::parse(document, ExecutionContext::Simulation).unwrap();
    let light = DeviceId::new("light").unwrap();
    let relay = DeviceId::new("relay").unwrap();
    let state = |kind, observed, at| DeviceState {
        kind,
        observed: Some(observed),
        quality: Quality::Confirmed,
        observed_at_ms: Some(at),
        desired: None,
        last_command_id: Some(9),
        pending_command: Some(9),
        version: at,
    };
    let mut physical = Snapshot {
        cursor: Cursor {
            boot_id: 1,
            sequence: 3,
        },
        state_version: 3,
        devices: [
            (
                light.clone(),
                state(ModuleKind::Dimmer, Observation::Level(percent(30)), 200),
            ),
            (
                relay.clone(),
                state(ModuleKind::Switch, Observation::Switch(false), 100),
            ),
        ]
        .into(),
    };
    let projected = homesensed::combined_light::project(&config, &physical).unwrap();
    assert_eq!(projected.devices.len(), 1);
    assert_eq!(
        projected.devices[&light].observed,
        Some(Observation::Level(percent(0)))
    );
    assert_eq!(projected.devices[&light].observed_at_ms, Some(100));
    assert_eq!(projected.devices[&light].pending_command, None);
    assert_eq!(
        physical.devices[&light].observed,
        Some(Observation::Level(percent(30)))
    );
    physical.devices.get_mut(&relay).unwrap().quality = Quality::Stale;
    assert_eq!(
        homesensed::combined_light::project(&config, &physical)
            .unwrap()
            .devices[&light]
            .quality,
        Quality::Stale
    );
    let directory =
        std::env::temp_dir().join(format!("homesense-combined-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&directory).unwrap();
    let database = directory.join("runtime.sqlite");
    let mut store = homesense_storage::Store::open(&database).unwrap();
    store
        .publish_configuration(None, document, ExecutionContext::Simulation, &[])
        .unwrap();
    store
        .remember_light_levels(1, &[(light.clone(), percent(30))])
        .unwrap();
    drop(store);
    let actor = homesensed::admission::Admission::open(
        database,
        homesensed::admission::Options {
            context: ExecutionContext::Simulation,
            ..Default::default()
        },
        |_| true,
    )
    .await
    .unwrap();
    let mut identity = homesense_hap::storage::IdentityStore::open(directory.join("identity"))
        .await
        .unwrap();
    physical.cursor.boot_id = uuid::Uuid::parse_str(actor.boot_id()).unwrap().as_u128();
    use hap::storage::Storage;
    identity.save_config(&hap::Config::default()).await.unwrap();
    let graph = homesensed::runtime_hap::RuntimeHap::new(
        &config,
        physical.clone(),
        &identity,
        actor.clone(),
        5000,
    )
    .await
    .unwrap();
    assert_eq!(graph.lights.len(), 1);
    assert_eq!(graph.lights[0].0.device, light);
    assert!(graph.lights[0].0.brightness_iid.is_some());
    let first_binding = graph.lights[0].0.clone();
    drop(graph);
    for state in physical.devices.values_mut() {
        state.quality = Quality::Confirmed;
        state.observed_at_ms = Some(0);
    }
    // The supply is off, and the module's 100 must not replace saved 30.
    physical.devices.get_mut(&light).unwrap().observed = Some(Observation::Level(percent(100)));
    let graph = homesensed::runtime_hap::RuntimeHap::new(
        &config,
        physical.clone(),
        &identity,
        actor.clone(),
        5000,
    )
    .await
    .unwrap();
    assert_eq!(graph.lights[0].0.aid, first_binding.aid);
    assert_eq!(
        graph.lights[0].0.brightness_iid,
        first_binding.brightness_iid
    );
    {
        let mut accessory = graph.lights[0].1.lock().await;
        let service = accessory.get_mut_service(hap::HapType::Lightbulb).unwrap();
        assert_eq!(
            service
                .get_mut_characteristic(hap::HapType::PowerState)
                .unwrap()
                .get_value()
                .await
                .unwrap(),
            hap::serde_json::json!(false)
        );
        assert_eq!(
            service
                .get_mut_characteristic(hap::HapType::Brightness)
                .unwrap()
                .get_value()
                .await
                .unwrap(),
            hap::serde_json::json!(30)
        );
    }
    let binding = &graph.lights[0].0;
    for (iid, value, expected) in [
        (binding.on_iid, hap::serde_json::json!(true), 30),
        (
            binding.brightness_iid.unwrap(),
            hap::serde_json::json!(100),
            100,
        ),
    ] {
        let body = hap::serde_json::json!({"characteristics":[{"aid":binding.aid,"iid":iid,"value":value}]}).to_string();
        graph
            .write_handler
            .write_async(uuid::Uuid::new_v4(), body.as_bytes())
            .await
            .unwrap();
        let ticket = actor.dispatch().await.unwrap().unwrap();
        assert!(
            matches!(ticket.command.command, Command::SetLevel { level, .. } if level == percent(expected))
        );
        actor.finish_write(ticket.command.id, true).await.unwrap();
        let ticket = actor.dispatch().await.unwrap().unwrap();
        assert_eq!(ticket.command.command, Command::Switch(true));
        actor.finish_write(ticket.command.id, true).await.unwrap();
    }
    drop(graph);
    actor.shutdown().await.unwrap();
    drop(identity);
    std::fs::remove_dir_all(directory).unwrap();
    physical.devices.remove(&relay);
    assert!(homesensed::combined_light::project(&config, &physical).is_err());
}
