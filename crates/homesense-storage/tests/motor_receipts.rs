use homesense_core::{
    Command, DeviceId, MotorMotion,
    queue::{Epochs, Origin, QueuedCommand},
};
use homesense_storage::{Store, journal::Status};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
static IDS: AtomicU64 = AtomicU64::new(0);
struct Database(PathBuf);
impl Drop for Database {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.0.display()));
        }
    }
}
fn fixture() -> (Database, Store, QueuedCommand) {
    let db = Database(std::env::temp_dir().join(format!(
        "motor-receipt-{}-{}.sqlite",
        std::process::id(),
        IDS.fetch_add(1, Ordering::Relaxed)
    )));
    let mut store = Store::open(&db.0).unwrap();
    store.publish(None, 1, b"synthetic", &[]).unwrap();
    let command = QueuedCommand {
        id: 1,
        device_id: DeviceId::new("curtain").unwrap(),
        command: Command::Motor(MotorMotion::Up),
        origin: Origin::Hap,
        epochs: Epochs {
            transport: 1,
            configuration: 1,
            control: 1,
        },
        deadline_ms: 10000,
        run_id: None,
    };
    store
        .accept_commands("boot1", std::slice::from_ref(&command), 100)
        .unwrap();
    (db, store, command)
}
#[test]
fn timestamp_is_written_only_and_observed_update_cannot_replace_it() {
    let (_db, mut store, c) = fixture();
    let read = |store: &Store| {
        store
            .motor_receipt("boot1", 1, &c.device_id, c.command, c.epochs)
            .unwrap()
            .unwrap()
    };
    assert_eq!(read(&store).written_at_ms, None);
    store
        .transition_command("boot1", 1, Status::Accepted, Status::Writing, 900)
        .unwrap();
    assert_eq!(read(&store).written_at_ms, None);
    store
        .transition_command("boot1", 1, Status::Writing, Status::Written, 1000)
        .unwrap();
    assert_eq!(read(&store).written_at_ms, Some(1000));
    store
        .transition_command("boot1", 1, Status::Written, Status::Observed, 2000)
        .unwrap();
    let row = read(&store);
    assert_eq!(row.status, Status::Observed);
    assert_eq!(row.written_at_ms, None);
}
#[test]
fn boot_device_motion_and_every_epoch_are_receipt_ownership_conditions() {
    let (_db, store, c) = fixture();
    assert!(
        store
            .motor_receipt("boot2", 1, &c.device_id, c.command, c.epochs)
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .motor_receipt(
                "boot1",
                1,
                &DeviceId::new("other").unwrap(),
                c.command,
                c.epochs
            )
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .motor_receipt(
                "boot1",
                1,
                &c.device_id,
                Command::Motor(MotorMotion::Down),
                c.epochs
            )
            .unwrap()
            .is_none()
    );
    for epochs in [
        Epochs {
            transport: 2,
            ..c.epochs
        },
        Epochs {
            configuration: 2,
            ..c.epochs
        },
        Epochs {
            control: 2,
            ..c.epochs
        },
    ] {
        assert!(
            store
                .motor_receipt("boot1", 1, &c.device_id, c.command, epochs)
                .unwrap()
                .is_none()
        );
    }
    assert!(
        store
            .motor_receipt("boot1", 1, &c.device_id, Command::Switch(true), c.epochs)
            .is_err()
    );
    assert_eq!(
        store.command_status("boot1", 1).unwrap(),
        Some(Status::Accepted)
    );
}
#[test]
fn uncertain_write_never_yields_a_timing_anchor() {
    let (_db, mut store, c) = fixture();
    store
        .transition_command("boot1", 1, Status::Accepted, Status::Writing, 900)
        .unwrap();
    store
        .transition_command("boot1", 1, Status::Writing, Status::Uncertain, 1000)
        .unwrap();
    let row = store
        .motor_receipt("boot1", 1, &c.device_id, c.command, c.epochs)
        .unwrap()
        .unwrap();
    assert_eq!(row.status, Status::Uncertain);
    assert_eq!(row.written_at_ms, None);
}
