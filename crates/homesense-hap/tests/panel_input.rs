use homesense_core::{DeviceId, ModuleKind, Observation, gesture::Settings};
use homesense_hap::{
    panel_graph::PanelKeyBinding,
    panel_input::{Generation, InputError, PanelInput},
};
use homesense_state::{Change, Cursor, DeviceState, Event, Quality};
fn generation() -> Generation {
    Generation {
        configuration: 1,
        transport: 1,
    }
}
fn input() -> PanelInput {
    PanelInput::new(
        vec![(
            PanelKeyBinding {
                device: DeviceId::new("key").unwrap(),
                key: 2,
                aid: 2,
                event_iid: 10,
            },
            Settings::default(),
        )],
        generation(),
        Cursor {
            boot_id: 1,
            sequence: 5,
        },
        100,
        1000,
    )
    .unwrap()
}
fn event(sequence: u64, at: u64, pressed: bool) -> Event {
    Event {
        cursor: Cursor {
            boot_id: 1,
            sequence,
        },
        device_id: DeviceId::new("key").unwrap(),
        change: Change::Button,
        state: DeviceState {
            kind: ModuleKind::Keypad,
            desired: None,
            observed: Some(Observation::Button { pressed }),
            quality: Quality::Confirmed,
            observed_at_ms: Some(at),
            last_command_id: None,
            pending_command: None,
            version: sequence,
        },
    }
}
#[test]
fn duplicate_batch_is_ignored_but_new_identical_clicks_are_delivered() {
    let mut input = input();
    // Historical down must not become a held key at startup.
    assert!(
        input
            .consume(generation(), &[event(5, 1, true)], 100)
            .unwrap()
            .is_empty()
    );
    let first = [event(6, 110, true), event(7, 160, false)];
    let notifications = input.consume(generation(), &first, 160).unwrap();
    assert_eq!(notifications.len(), 1);
    assert_eq!(notifications[0].value, 0);
    assert!(input.consume(generation(), &first, 170).unwrap().is_empty());
    let second = input
        .consume(
            generation(),
            &[event(8, 210, true), event(9, 260, false)],
            260,
        )
        .unwrap();
    assert_eq!(second, notifications);
}
#[test]
fn stream_loss_generation_change_stale_and_foreign_boot_are_terminal() {
    for (generation, events, now) in [
        (generation(), vec![event(7, 110, true)], 110),
        (
            Generation {
                configuration: 1,
                transport: 2,
            },
            vec![],
            110,
        ),
        (
            Generation {
                configuration: 2,
                transport: 1,
            },
            vec![],
            110,
        ),
        (generation(), vec![event(6, 110, true)], 1110),
        (
            generation(),
            vec![{
                let mut e = event(6, 110, true);
                e.cursor.boot_id = 2;
                e
            }],
            110,
        ),
        (
            generation(),
            vec![
                event(6, 110, true),
                event(7, 160, false),
                event(9, 170, true),
            ],
            170,
        ),
    ] {
        let mut input = input();
        assert_eq!(
            input.consume(generation, &events, now),
            Err(InputError::InvalidStream)
        );
        assert_eq!(input.cursor().sequence, 5);
        assert_eq!(
            input.consume(generation, &[], now),
            Err(InputError::Stopped)
        );
    }
}
#[test]
fn long_press_notifies_once_and_stop_discards_pending_gestures() {
    let mut input = input();
    assert!(
        input
            .consume(generation(), &[event(6, 110, true)], 110)
            .unwrap()
            .is_empty()
    );
    assert_eq!(input.consume(generation(), &[], 710).unwrap()[0].value, 2);
    assert!(input.consume(generation(), &[], 720).unwrap().is_empty());
    assert!(
        input
            .consume(generation(), &[event(7, 730, false)], 730)
            .unwrap()
            .is_empty()
    );
    input.stop();
    assert_eq!(
        input.consume(generation(), &[], 740),
        Err(InputError::Stopped)
    );
}
