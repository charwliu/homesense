use hap::server::{WriteBatchHandler, WriteStatus};
use homesense_core::{Command, DeviceId, LevelTransition, Observation, Percent};
use homesense_hap::{
    requests::{Batch, Intent, LightBinding, LightRequests},
    write::LightWriteHandler,
};
use homesense_state::Quality;
use homesensed::simulation::SimulationRuntime;
use std::sync::{Arc, Mutex};
use uuid::Uuid;
const CONFIG: &[u8] = include_bytes!("../../../config/simulation.json");
fn id() -> DeviceId {
    DeviceId::new("simulation:light").unwrap()
}
fn level(n: u8) -> Command {
    Command::SetLevel {
        level: Percent::new(n).unwrap(),
        transition: LevelTransition::Immediate,
    }
}
fn batch(controller: Uuid, command: Command) -> Batch {
    Batch {
        controller,
        intents: vec![Intent {
            device: id(),
            command,
        }],
        subscriptions: vec![],
    }
}

#[test]
fn grouped_hap_write_reaches_configured_bus_and_only_feedback_confirms_state() {
    let controller = Uuid::new_v4();
    let runtime = Arc::new(Mutex::new(
        SimulationRuntime::new(CONFIG, [controller], 8).unwrap(),
    ));
    let owner = runtime.clone();
    let requests = LightRequests::new(vec![LightBinding {
        device: id(),
        aid: 2,
        on_iid: 10,
        brightness_iid: Some(11),
        default_on_level: Some(Percent::new(50).unwrap()),
    }])
    .unwrap();
    let adapter = LightWriteHandler::new(
        requests,
        |_: &DeviceId| None,
        move |batch| {
            owner
                .lock()
                .unwrap()
                .submit(batch, 0)
                .map_err(|_| WriteStatus::ServiceCommunicationFailure)
        },
    );
    adapter.write(controller, br#"{"characteristics":[{"aid":2,"iid":10,"value":true},{"aid":2,"iid":11,"value":30}]}"#).unwrap();
    let mut owner = runtime.lock().unwrap();
    assert_eq!(owner.queued_len(), 1);
    assert_eq!(owner.snapshot().devices[&id()].quality, Quality::Unknown);
    owner.advance(20).unwrap();
    let state = &owner.snapshot().devices[&id()];
    assert_eq!(
        state.observed,
        Some(Observation::Level(Percent::new(30).unwrap()))
    );
    assert_eq!(state.quality, Quality::Confirmed);
    assert_eq!(state.pending_command, None);
    assert_eq!(owner.queued_len(), 0);
}

#[test]
fn rejected_batch_is_atomic_and_does_not_consume_command_ids() {
    let controller = Uuid::new_v4();
    let mut runtime = SimulationRuntime::new(CONFIG, [controller], 8).unwrap();
    let mut invalid = batch(controller, level(30));
    invalid.intents.push(Intent {
        device: DeviceId::new("unknown").unwrap(),
        command: level(40),
    });
    assert!(runtime.submit(invalid, 0).is_err());
    assert_eq!(runtime.queued_len(), 0);
    assert!(runtime.submit(batch(Uuid::new_v4(), level(20)), 0).is_err());
    assert!(
        runtime
            .submit(batch(controller, Command::Query), 0)
            .is_err()
    );
    runtime.submit(batch(controller, level(60)), 0).unwrap();
    runtime.advance(1).unwrap();
    let state = &runtime.snapshot().devices[&id()];
    assert_eq!(state.last_command_id, Some(1));
    assert_eq!(
        state.observed,
        Some(Observation::Level(Percent::new(60).unwrap()))
    );
}

#[test]
fn deadline_and_monotonic_clock_prevent_late_execution() {
    let controller = Uuid::new_v4();
    let mut runtime = SimulationRuntime::new(CONFIG, [controller], 8).unwrap();
    runtime.submit(batch(controller, level(30)), 10).unwrap();
    assert!(runtime.advance(9).is_err());
    runtime.advance(1010).unwrap();
    assert_eq!(runtime.snapshot().devices[&id()].observed, None);
    assert_eq!(runtime.queued_len(), 0);
}
