use homesense_core::{
    Command, DeviceId,
    queue::{Epochs, Origin, QueuedCommand},
    rule::{Value, ValueType},
};
use homesense_storage::{
    Error, Store, journal::Status, keypad_completion::Commit, variables::Update,
};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture {
    store: Store,
    path: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        Self::source(1, Command::Switch(false))
    }
    fn source(id: u64, action: Command) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "key-completion-store-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("runtime.sqlite");
        let mut store = Store::open(&path).unwrap();
        store
            .publish(None, 1, b"synthetic configuration", &[])
            .unwrap();
        store
            .define_logical_variable(1, "mode", ValueType::Integer)
            .unwrap();
        store.ensure_keypad_completion_audit().unwrap();
        store
            .accept_commands("boot", &[command(id, "source", action, 50)], 1)
            .unwrap();
        store
            .transition_command("boot", id, Status::Accepted, Status::Writing, 2)
            .unwrap();
        store
            .transition_command("boot", id, Status::Writing, Status::Written, 3)
            .unwrap();
        Self { store, path }
    }
    fn sql(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(&self.path).unwrap()
    }
    fn count(&self, table: &str) -> i64 {
        self.sql()
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap()
    }
    fn no_commit(&self) {
        let variable = self.store.logical_variable(1, "mode").unwrap().unwrap();
        assert_eq!(variable.version, 0);
        assert_eq!(variable.value, None);
        assert_eq!(self.store.command_status("boot", 10).unwrap(), None);
        assert!(
            self.store
                .component_mode_audit(1, "mode", 1)
                .unwrap()
                .is_none()
        );
        assert_eq!(self.count("keypad_completion_audit"), 0);
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(self.path.parent().unwrap());
    }
}
fn id(value: &str) -> DeviceId {
    DeviceId::new(value).unwrap()
}
fn epochs() -> Epochs {
    Epochs {
        configuration: 1,
        transport: 1,
        control: 1,
    }
}
fn command(n: u64, device: &str, action: Command, deadline_ms: u64) -> QueuedCommand {
    QueuedCommand {
        id: n,
        device_id: id(device),
        command: action,
        origin: Origin::Rule,
        epochs: epochs(),
        deadline_ms,
        run_id: None,
    }
}
fn led(n: u64) -> QueuedCommand {
    command(n, "linked-led", Command::PanelLed(true), 1_100)
}
fn completion() -> Commit {
    Commit {
        update: Update {
            revision: 1,
            name: "mode".into(),
            expected_version: 0,
            value: Some(Value::Integer(3)),
        },
        policy: "source-off-conditional".into(),
        sequence: 7,
        endpoint: id("origin-key"),
        request_id: "gesture-1".into(),
        epochs: epochs(),
        command_fence: 1,
        written: BTreeMap::from([(1, 3)]),
        observations: BTreeMap::from([(id("source"), 10), (id("condition"), 10)]),
        after_device: id("source"),
        after_written: 3,
        original_deadline_ms: 1_500,
        send_before_ms: 1_100,
    }
}

#[test]
fn component_cas_source_provenance_consumed_gesture_and_led_queue_commit_together() {
    let mut f = Fixture::new();
    let proof = completion();
    f.store
        .accept_keypad_completion("boot", &[led(10)], &[], 1_000, None, "controller", &proof)
        .unwrap();
    let value = f.store.logical_variable(1, "mode").unwrap().unwrap();
    assert_eq!(value.version, 1);
    assert_eq!(value.value, Some(Value::Integer(3)));
    assert_eq!(
        f.store.command_status("boot", 10).unwrap(),
        Some(Status::Accepted),
        "acceptance is not physical LED delivery"
    );
    let audit = f.store.component_mode_audit(1, "mode", 1).unwrap().unwrap();
    assert_eq!(audit.policy, proof.policy);
    assert_eq!(audit.provenance, "local_written_with_feedback_inference");
    assert_eq!(audit.controller, "controller");
    assert_eq!(f.count("keypad_completion_audit"), 1);
    assert_eq!(
        f.count("component_mode_source"),
        0,
        "source-Off is not a continuing logical dependency of the linked component"
    );
    let reopened = Store::open(&f.path).unwrap();
    assert_eq!(
        reopened.command_status("boot", 10).unwrap(),
        Some(Status::Accepted)
    );
    assert_eq!(
        reopened
            .logical_variable(1, "mode")
            .unwrap()
            .unwrap()
            .version,
        1
    );
}

#[test]
fn a_late_audit_sql_failure_rolls_back_the_earlier_variable_and_queue_inserts() {
    let mut f = Fixture::new();
    f.sql().execute_batch("CREATE TRIGGER fail_completion BEFORE INSERT ON keypad_completion_audit BEGIN SELECT RAISE(ABORT,'synthetic failure'); END;").unwrap();
    assert!(
        f.store
            .accept_keypad_completion(
                "boot",
                &[led(10)],
                &[],
                1_000,
                None,
                "controller",
                &completion()
            )
            .is_err()
    );
    f.no_commit();
    f.sql()
        .execute_batch("DROP TRIGGER fail_completion")
        .unwrap();
    f.store
        .accept_keypad_completion(
            "boot",
            &[led(10)],
            &[],
            1_000,
            None,
            "controller",
            &completion(),
        )
        .unwrap();
    assert_eq!(f.count("keypad_completion_audit"), 1);
}

#[test]
fn a_newer_variable_version_wins_without_accepting_any_led_or_provenance() {
    let mut f = Fixture::new();
    f.store
        .accept_queue_batch_with_variables(
            "other",
            &[],
            &[],
            10,
            None,
            &[Update {
                revision: 1,
                name: "mode".into(),
                expected_version: 0,
                value: Some(Value::Integer(4)),
            }],
        )
        .unwrap();
    assert!(matches!(
        f.store.accept_keypad_completion(
            "boot",
            &[led(10)],
            &[],
            1_000,
            None,
            "controller",
            &completion()
        ),
        Err(Error::Conflict)
    ));
    assert_eq!(
        f.store.logical_variable(1, "mode").unwrap().unwrap().value,
        Some(Value::Integer(4))
    );
    assert_eq!(f.store.command_status("boot", 10).unwrap(), None);
    assert_eq!(f.count("keypad_completion_audit"), 0);
    assert!(
        f.store
            .component_mode_audit(1, "mode", 2)
            .unwrap()
            .is_none()
    );
}

#[test]
fn a_consumed_origin_cannot_complete_again_with_a_fresh_variable_version_or_new_led_ids() {
    let mut f = Fixture::new();
    let mut proof = completion();
    f.store
        .accept_keypad_completion("boot", &[led(10)], &[], 1_000, None, "controller", &proof)
        .unwrap();
    proof.update.expected_version = 1;
    assert!(matches!(
        f.store.accept_keypad_completion(
            "boot",
            &[led(11)],
            &[],
            1_001,
            None,
            "controller",
            &proof
        ),
        Err(Error::Conflict)
    ));
    assert_eq!(
        f.store
            .logical_variable(1, "mode")
            .unwrap()
            .unwrap()
            .version,
        1
    );
    assert_eq!(f.store.command_status("boot", 11).unwrap(), None);
    assert_eq!(f.count("keypad_completion_audit"), 1);
    assert!(
        f.store
            .component_mode_audit(1, "mode", 2)
            .unwrap()
            .is_none()
    );
}

#[test]
fn invalid_or_expired_evidence_cannot_partially_accept_a_completion() {
    for case in 0..22 {
        let mut f = Fixture::new();
        let mut proof = completion();
        let mut output = vec![led(10)];
        match case {
            0 => proof.written.clear(),
            1 => proof.written = BTreeMap::from([(99, 3)]),
            2 => proof.written = BTreeMap::from([(1, 2)]),
            3 => proof.command_fence = 2,
            4 => {
                proof.observations.insert(id("source"), 3);
            }
            5 => {
                proof.observations.insert(id("condition"), 1_001);
            }
            6 => proof.original_deadline_ms = 1_099,
            7 => proof.send_before_ms = 3_001,
            8 => proof.send_before_ms = 1_000,
            9 => proof.endpoint = id("linked-led"),
            10 => proof.request_id = "bad request id".into(),
            11 => proof.policy.clear(),
            12 => proof.update.value = None,
            13 => proof.epochs.transport = 2,
            14 => proof.after_device = id("condition"),
            15 => proof.after_written = 2,
            16 => proof.sequence = 0,
            17 => output[0].command = Command::Switch(false),
            18 => output[0].deadline_ms = 1_101,
            19 => output.push(led(11)),
            20 => proof.observations.clear(),
            _ => proof.update.revision = 2,
        }
        assert!(
            f.store
                .accept_keypad_completion("boot", &output, &[], 1_000, None, "controller", &proof)
                .is_err(),
            "case {case}"
        );
        f.no_commit();
    }
}

#[test]
fn each_source_ticket_needs_a_complete_real_write_and_queries_or_leds_are_not_actuators() {
    for action in [Command::Query, Command::PanelLed(false)] {
        let mut f = Fixture::source(1, action);
        assert!(
            f.store
                .accept_keypad_completion(
                    "boot",
                    &[led(10)],
                    &[],
                    1_000,
                    None,
                    "controller",
                    &completion()
                )
                .is_err()
        );
        f.no_commit();
    }
    let mut f = Fixture::new();
    let mut proof = completion();
    proof.written.insert(2, 4);
    f.store
        .accept_commands(
            "boot",
            &[command(2, "source-key", Command::PanelLed(false), 50)],
            1,
        )
        .unwrap();
    assert!(
        f.store
            .accept_keypad_completion("boot", &[led(10)], &[], 1_000, None, "controller", &proof)
            .is_err()
    );
    f.no_commit();
    f.store
        .transition_command("boot", 2, Status::Accepted, Status::Writing, 2)
        .unwrap();
    f.store
        .transition_command("boot", 2, Status::Writing, Status::Written, 4)
        .unwrap();
    f.store
        .accept_keypad_completion("boot", &[led(10)], &[], 1_000, None, "controller", &proof)
        .unwrap();
}

#[test]
fn a_newer_automated_control_of_any_observed_dependency_blocks_old_completion() {
    for device in ["source", "condition"] {
        let mut f = Fixture::new();
        f.store
            .accept_commands(
                "boot",
                &[command(2, device, Command::Switch(true), 2_000)],
                100,
            )
            .unwrap();
        assert!(matches!(
            f.store.accept_keypad_completion(
                "boot",
                &[led(10)],
                &[],
                1_000,
                None,
                "controller",
                &completion()
            ),
            Err(Error::Conflict)
        ));
        f.no_commit();
    }
    for (device, action) in [
        ("condition", Command::Query),
        ("unrelated", Command::Switch(true)),
    ] {
        let mut f = Fixture::new();
        f.store
            .accept_commands("boot", &[command(2, device, action, 2_000)], 100)
            .unwrap();
        f.store
            .accept_keypad_completion(
                "boot",
                &[led(10)],
                &[],
                1_000,
                None,
                "controller",
                &completion(),
            )
            .unwrap();
    }
}

#[test]
fn whole_module_off_is_an_override_even_for_a_nonrepresentative_channel() {
    let mut f = Fixture::new();
    let devices = (1..=8)
        .map(|channel| {
            (
                channel,
                if channel == 8 {
                    "condition".to_owned()
                } else {
                    format!("port-{channel}")
                },
            )
        })
        .collect();
    let effects = homesense_storage::module_off::Effects {
        command: 2,
        module_id: "relay".into(),
        gateway_id: "bus".into(),
        address: 3,
        profile: "r8i8_din_pro_v2_7_7".into(),
        devices,
    };
    f.store
        .accept_module_off_commands(
            "boot",
            &[command(2, "port-1", Command::ModuleAllOff, 2_000)],
            &[],
            100,
            &[effects],
        )
        .unwrap();
    assert!(matches!(
        f.store.accept_keypad_completion(
            "boot",
            &[led(10)],
            &[],
            1_000,
            None,
            "controller",
            &completion()
        ),
        Err(Error::Conflict)
    ));
    f.no_commit();
}

#[test]
fn a_control_before_the_captured_command_fence_does_not_become_a_new_override() {
    let mut f = Fixture::source(3, Command::Switch(false));
    f.store
        .accept_commands(
            "boot",
            &[command(1, "condition", Command::Switch(true), 50)],
            0,
        )
        .unwrap();
    f.store
        .transition_command("boot", 1, Status::Accepted, Status::Writing, 1)
        .unwrap();
    f.store
        .transition_command("boot", 1, Status::Writing, Status::Written, 2)
        .unwrap();
    let mut proof = completion();
    proof.command_fence = 2;
    proof.written = BTreeMap::from([(3, 3)]);
    f.store
        .accept_keypad_completion("boot", &[led(10)], &[], 1_000, None, "controller", &proof)
        .unwrap();
}

#[test]
fn a_previous_observed_condition_is_settled_at_its_observation_boundary_without_borrowing_its_write()
 {
    let mut f = Fixture::source(3, Command::Switch(false));
    f.store
        .accept_commands(
            "boot",
            &[command(1, "condition", Command::Switch(true), 50)],
            0,
        )
        .unwrap();
    f.store
        .transition_command("boot", 1, Status::Accepted, Status::Writing, 1)
        .unwrap();
    f.store
        .transition_command("boot", 1, Status::Writing, Status::Written, 2)
        .unwrap();
    f.store
        .transition_command("boot", 1, Status::Written, Status::Observed, 10)
        .unwrap();
    let mut proof = completion();
    proof.command_fence = 2;
    proof.written = BTreeMap::from([(3, 3)]);
    assert!(
        f.store
            .keypad_completion_feedback_settled(
                "boot",
                &id("condition"),
                Some(10),
                1,
                2,
                &proof.written
            )
            .unwrap()
    );
    f.store
        .accept_keypad_completion("boot", &[led(10)], &[], 1_000, None, "controller", &proof)
        .unwrap();
    let tickets: String = f
        .sql()
        .query_row("SELECT tickets FROM keypad_completion_audit", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(
        tickets, "3",
        "the old condition command is not this gesture's receipt"
    );
}

#[test]
fn live_deadline_check_rolls_back_queue_cas_and_all_audits_after_database_work() {
    use std::cell::Cell;
    for commit_now in [999, 1_100, 1_500] {
        let mut f = Fixture::new();
        let called = Cell::new(0);
        let clock = || {
            called.set(called.get() + 1);
            commit_now
        };
        assert!(matches!(
            f.store.accept_keypad_completion_before(
                "boot",
                &[led(10)],
                &[],
                1_000,
                None,
                "controller",
                &completion(),
                &clock
            ),
            Err(Error::Conflict)
        ));
        assert_eq!(called.get(), 1);
        f.no_commit();
    }
    let mut f = Fixture::new();
    f.store
        .accept_keypad_completion_before(
            "boot",
            &[led(10)],
            &[],
            1_000,
            None,
            "controller",
            &completion(),
            &|| 1_099,
        )
        .unwrap();
    assert_eq!(
        f.store.logical_variable(1, "mode").unwrap().unwrap().value,
        Some(Value::Integer(3))
    );
}

#[test]
fn a_pre_capture_pending_module_off_blocks_nonrepresentative_conditions_in_the_atomic_transaction()
{
    let mut f = Fixture::source(3, Command::Switch(false));
    let effects = homesense_storage::module_off::Effects {
        command: 1,
        module_id: "relay".into(),
        gateway_id: "bus".into(),
        address: 3,
        profile: "r8i8_din_pro_v2_7_7".into(),
        devices: (1..=8)
            .map(|n| {
                (
                    n,
                    if n == 8 {
                        "condition".into()
                    } else {
                        format!("port-{n}")
                    },
                )
            })
            .collect(),
    };
    f.store
        .accept_module_off_commands(
            "boot",
            &[command(1, "port-1", Command::ModuleAllOff, 50)],
            &[],
            0,
            &[effects],
        )
        .unwrap();
    let mut proof = completion();
    proof.command_fence = 2;
    proof.written = BTreeMap::from([(3, 3)]);
    assert!(
        !f.store
            .keypad_completion_feedback_settled(
                "boot",
                &id("condition"),
                Some(10),
                1,
                2,
                &proof.written
            )
            .unwrap()
    );
    assert!(matches!(
        f.store.accept_keypad_completion(
            "boot",
            &[led(10)],
            &[],
            1_000,
            None,
            "controller",
            &proof
        ),
        Err(Error::Conflict)
    ));
    f.no_commit();
}
