//! Reviewed synthetic rule nodes only. No real bus, sockets or device identities.
#[path = "support/staged_feedback.rs"]
mod staged_feedback;
use homesense_core::{
    Command, DeviceId, ExecutionContext, LevelTransition, Percent,
    rule::{Action, Expr, Node, Value, ValueType},
    scene_expand::Definition,
};
use homesense_hap::requests::{Batch, Intent};
use homesense_storage::{Store, journal::Status};
use homesensed::{
    admission::{Admission, Options},
    event_rules::{Binding, EventRules, Report, Trigger},
    transport_owner::TransportOwner,
};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use uuid::Uuid;

struct Database(std::path::PathBuf);
impl Database {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("homesense-rule-order-{}", Uuid::new_v4()));
        std::fs::create_dir(&p).unwrap();
        Self(p)
    }
    fn path(&self) -> std::path::PathBuf {
        self.0.join("runtime.sqlite")
    }
    fn store(&self) -> Store {
        Store::open(self.path()).unwrap()
    }
}
impl Drop for Database {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn config() -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({"schema_version":2,"revision":1,
    "gateways":[{"id":"bus","endpoint":"127.0.0.1:50007","modules":[
        {"id":"relay-module","address":"03","profile":"r8i8_din_pro_v2_7_7","verification":{"status":"simulated"}},
        {"id":"dimmer-module","address":"12","profile":"ad8_din_rs485_v2_2_1","verification":{"status":"simulated"}},
        {"id":"panel-module","address":"41","profile":"ts6_86_rs485_v2_6_5_6key","led_mode":"manual","verification":{"status":"simulated"}}]}],
    "devices":[{"id":"supply","name":"Synthetic supply","module_id":"relay-module","channel":1,"enabled":true},
        {"id":"independent","name":"Independent synthetic load","module_id":"relay-module","channel":2,"enabled":true},
        {"id":"dimmer","name":"Synthetic standalone dimmer","module_id":"dimmer-module","channel":1,"enabled":true},
        {"id":"combo","name":"Synthetic combined light","module_id":"dimmer-module","channel":4,"enabled":true,"default_on_level":100,"supply":{"module_id":"relay-module","channel":1}},
        {"id":"key","name":"Synthetic panel key","module_id":"panel-module","channel":1,"enabled":true}]})).unwrap()
}
async fn open(db: &Database, controller: Uuid, capacity: usize) -> Admission {
    let a = Admission::open(
        db.path(),
        Options {
            context: ExecutionContext::Simulation,
            command_capacity: capacity,
            ..Options::default()
        },
        move |id| id == controller,
    )
    .await
    .unwrap();
    a.synchronized(1, 1).await.unwrap();
    a
}
async fn actor(db: &Database, controller: Uuid, capacity: usize) -> Admission {
    db.store()
        .publish_configuration(None, &config(), ExecutionContext::Simulation, &[])
        .unwrap();
    open(db, controller, capacity).await
}
fn node(label: &str, device: &str, command: Command) -> Node {
    Node {
        source: label.into(),
        action: Action::Command {
            device: DeviceId::new(device).unwrap(),
            command,
        },
    }
}
fn level(n: u8) -> Command {
    Command::SetLevel {
        level: Percent::new(n).unwrap(),
        transition: LevelTransition::Immediate,
    }
}
async fn event(
    a: &Admission,
    id: Uuid,
    actions: Vec<Node>,
    scenes: BTreeMap<String, Definition>,
    variables: Vec<String>,
) -> Report {
    let mut r = EventRules::new(
        a.clone(),
        id,
        vec![Binding {
            id: "synthetic-rule".into(),
            trigger: Trigger::StartupSynchronized,
            variables,
            actions,
        }],
        scenes,
        Default::default(),
    )
    .await
    .unwrap();
    r.dispatch(Trigger::StartupSynchronized, None).await
}
type Bytes = Arc<Mutex<Vec<u8>>>;
struct Wire {
    bytes: Bytes,
    fail_after: Option<usize>,
    writes: usize,
}
impl std::io::Read for Wire {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        Err(std::io::ErrorKind::WouldBlock.into())
    }
}
impl std::io::Write for Wire {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        if self.fail_after.is_some_and(|n|self.writes>=n){return Err(std::io::ErrorKind::BrokenPipe.into());}
        let n=self.fail_after.map_or(b.len(),|limit|b.len().min(limit-self.writes));
        self.writes+=n;self.bytes.lock().unwrap().extend_from_slice(&b[..n]);Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn wire(a: &Admission, fail_after: Option<usize>) -> (TransportOwner<staged_feedback::Echo<Wire>>, Bytes) {
    let bytes = Arc::new(Mutex::new(vec![]));
    (
        TransportOwner::new(
            a.clone(),
            [(
                "bus".into(),
                hb_transport::Connection::for_simulation(
                    staged_feedback::Echo::new(Wire {
                        bytes: bytes.clone(),
                        fail_after,
                        writes: 0,
                    }),
                    1,
                ),
            )]
            .into(),
        ),
        bytes,
    )
}
async fn emit(a: &Admission, controller:Uuid, ids: &[u64]) -> Vec<u8> {
    let (mut owner, bytes) = wire(a, None);
    let mut replies=staged_feedback::Replies::new(a,&homesense_config::Configuration::parse(&config(),ExecutionContext::Simulation).unwrap());
    for id in ids {
        let output = owner.step_with_frame_budget(1).await.unwrap().unwrap_or_else(||panic!("expected next frame after bytes {:?}",bytes.lock().unwrap()));
        assert_eq!(output.command_id, *id);
        output.result.unwrap();
        replies.received(a,controller,&mut owner,"bus").await;
    }
    assert!(owner.step_with_frame_budget(1).await.unwrap().is_none());
    bytes.lock().unwrap().clone()
}
fn verify(db: &Database, boot: &str, report: &Report, expected: &[(&str, &str, Command, Status)]) {
    let mut store = db.store();
    let page = store
        .rule_chain(boot, report.correlation_id, 0, 64)
        .unwrap();
    assert_eq!(page.records.len(), 1);
    let commands = &page.records[0].commands;
    assert_eq!(commands.len(), expected.len());
    assert_eq!(
        commands.iter().map(|c| c.id).collect::<Vec<_>>(),
        report.completed[0].commands
    );
    let sql = rusqlite::Connection::open(db.path()).unwrap();
    for (row, (device, label, command, status)) in commands.iter().zip(expected) {
        assert_eq!(row.device, *device);
        assert_eq!(row.source.as_deref(), Some(*label));
        assert_eq!(&row.status, status);
        // The durable requested intent must agree with each source's own node.
        let intent: Vec<u8> = sql
            .query_row(
                "SELECT intent FROM command_journal WHERE boot_id=?1 AND command_id=?2",
                rusqlite::params![boot, i64::try_from(row.id).unwrap()],
                |r| r.get(0),
            )
            .unwrap();
        let (tag, value) = match command {
            Command::PanelLed(on) => (6, u8::from(*on)),
            Command::Switch(on) => (1, u8::from(*on)),
            Command::SetLevel {
                level,
                transition: LevelTransition::Immediate,
            } => (2, level.get()),
            _ => panic!("fixture requires explicitly normalized command"),
        };
        assert_eq!(intent, [1, tag, value, 0]);
        assert_eq!(
            sql.query_row(
                "SELECT count(*) FROM command_journal WHERE boot_id=?1 AND command_id=?2",
                rusqlite::params![boot, i64::try_from(row.id).unwrap()],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
    }
}
#[tokio::test]
async fn same_key_led_nodes_include_off_off_and_off_on_without_deduplication() {
    for sequence in [[true, false], [false, false], [false, true], [true, true]] {
        let db = Database::new();
        let id = Uuid::new_v4();
        let a = actor(&db, id, 8).await;
        let boot = a.boot_id().to_owned();
        let report = event(
            &a,
            id,
            vec![
                node("first-node", "key", Command::PanelLed(sequence[0])),
                node("second-node", "key", Command::PanelLed(sequence[1])),
            ],
            BTreeMap::new(),
            vec![],
        )
        .await;
        assert!(report.failure.is_none(), "{:?}", report.failure);
        assert_eq!(report.completed[0].commands.len(), 2);
        let frames = emit(&a, id, &report.completed[0].commands).await;
        assert_eq!(
            frames,
            sequence
                .into_iter()
                .flat_map(|on| [0xae, 0x41, 1, if on { 0xf1 } else { 0xf2 }, 0xee])
                .collect::<Vec<_>>()
        );
        a.shutdown().await.unwrap();
        verify(
            &db,
            &boot,
            &report,
            &[
                (
                    "key",
                    "first-node",
                    Command::PanelLed(sequence[0]),
                    Status::Written,
                ),
                (
                    "key",
                    "second-node",
                    Command::PanelLed(sequence[1]),
                    Status::Written,
                ),
            ],
        );
    }
}
#[tokio::test]
async fn repeated_dimmer_zero_nodes_and_endpoint_changes_remain_distinct() {
    for sequence in [[0, 0], [100, 0], [0, 100]] {
        let db = Database::new();
        let id = Uuid::new_v4();
        let a = actor(&db, id, 8).await;
        let boot = a.boot_id().to_owned();
        let report = event(
            &a,
            id,
            vec![
                node("first-level", "dimmer", level(sequence[0])),
                node("second-level", "dimmer", level(sequence[1])),
            ],
            BTreeMap::new(),
            vec![],
        )
        .await;
        assert!(report.failure.is_none(), "{:?}", report.failure);
        assert_eq!(
            emit(&a, id, &report.completed[0].commands).await,
            sequence
                .into_iter()
                .flat_map(|n| [0xae, 0x12, 1, n, 0xee])
                .collect::<Vec<_>>()
        );
        a.shutdown().await.unwrap();
        verify(
            &db,
            &boot,
            &report,
            &[
                ("dimmer", "first-level", level(sequence[0]), Status::Written),
                (
                    "dimmer",
                    "second-level",
                    level(sequence[1]),
                    Status::Written,
                ),
            ],
        );
    }
}
#[tokio::test]
async fn two_immediate_scene_calls_with_overlapping_targets_keep_call_member_sources() {
    let db = Database::new();
    let id = Uuid::new_v4();
    let a = actor(&db, id, 8).await;
    let boot = a.boot_id().to_owned();
    let calls = [
        Node {
            source: "call-one".into(),
            action: Action::Scene {
                id: "one".into(),
                activate: true,
            },
        },
        Node {
            source: "call-two".into(),
            action: Action::Scene {
                id: "two".into(),
                activate: true,
            },
        },
    ];
    let scenes = [
        (
            "one".into(),
            Definition {
                activate: vec![node("member-one", "dimmer", level(0))],
                deactivate: None,
            },
        ),
        (
            "two".into(),
            Definition {
                activate: vec![node("member-two", "dimmer", level(0))],
                deactivate: None,
            },
        ),
    ]
    .into();
    let report = event(&a, id, calls.into(), scenes, vec![]).await;
    assert!(report.failure.is_none(), "{:?}", report.failure);
    assert_eq!(
        emit(&a, id, &report.completed[0].commands).await,
        [0xae, 0x12, 1, 0, 0xee, 0xae, 0x12, 1, 0, 0xee]
    );
    a.shutdown().await.unwrap();
    verify(
        &db,
        &boot,
        &report,
        &[
            (
                "dimmer",
                "call-one -> member-one",
                level(0),
                Status::Written,
            ),
            (
                "dimmer",
                "call-two -> member-two",
                level(0),
                Status::Written,
            ),
        ],
    );
}
#[tokio::test]
async fn repeated_combined_nodes_preserve_each_expanded_source_and_wire_order() {
    for off in [false, true] {
        let db = Database::new();
        let id = Uuid::new_v4();
        let a = actor(&db, id, 8).await;
        let boot = a.boot_id().to_owned();
        let report = event(
            &a,
            id,
            vec![
                node("combo-first", "combo", level(100)),
                node(
                    "combo-second",
                    "combo",
                    if off {
                        Command::Switch(false)
                    } else {
                        level(100)
                    },
                ),
                node("unrelated", "independent", Command::Switch(false)),
            ],
            BTreeMap::new(),
            vec![],
        )
        .await;
        assert!(report.failure.is_none(), "{:?}", report.failure);
        let mut expected = vec![
            ("combo", "combo-first", level(100), Status::Written),
            (
                "supply",
                "combo-first",
                Command::Switch(true),
                Status::Written,
            ),
        ];
        let mut frames = vec![0xae, 0x12, 4, 100, 0xee, 0xae, 3, 1, 0xf1, 0xee];
        if off {
            expected.push((
                "supply",
                "combo-second",
                Command::Switch(false),
                Status::Written,
            ));
            frames.extend([0xae, 3, 1, 0xf2, 0xee]);
        } else {
            expected.extend([
                ("combo", "combo-second", level(100), Status::Written),
                (
                    "supply",
                    "combo-second",
                    Command::Switch(true),
                    Status::Written,
                ),
            ]);
            frames.extend([0xae, 0x12, 4, 100, 0xee, 0xae, 3, 1, 0xf1, 0xee]);
        }
        expected.push((
            "independent",
            "unrelated",
            Command::Switch(false),
            Status::Written,
        ));
        frames.extend([0xae, 3, 2, 0xf2, 0xee]);
        assert_eq!(emit(&a, id, &report.completed[0].commands).await, frames);
        a.shutdown().await.unwrap();
        verify(&db, &boot, &report, &expected);
    }
}
#[tokio::test]
async fn external_duplicate_and_audited_hidden_supply_controls_remain_rejected() {
    let db = Database::new();
    let id = Uuid::new_v4();
    let a = actor(&db, id, 8).await;
    let intents = (0..2)
        .map(|_| Intent {
            device: DeviceId::new("dimmer").unwrap(),
            command: level(0),
        })
        .collect();
    assert_eq!(
        a.submit(Batch {
            controller: id,
            intents,
            subscriptions: vec![]
        })
        .await
        .unwrap_err(),
        hap::server::WriteStatus::InvalidValueInRequest
    );
    let report = event(
        &a,
        id,
        vec![
            node("valid-first", "key", Command::PanelLed(true)),
            node("hidden-second", "supply", Command::Switch(false)),
        ],
        BTreeMap::new(),
        vec![],
    )
    .await;
    assert!(report.failure.is_some());
    assert!(report.completed.is_empty());
    assert!(a.task_generation().await.is_ok());
    assert!(a.dispatch().await.unwrap().is_none());
    let sql = rusqlite::Connection::open(db.path()).unwrap();
    assert_eq!(
        sql.query_row("SELECT count(*) FROM command_journal", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    a.shutdown().await.unwrap();
}
#[tokio::test]
async fn invalid_later_node_and_expanded_capacity_rejection_accept_nothing() {
    for mode in ["invalid", "capacity", "expanded-limit"] {
        let db = Database::new();
        let id = Uuid::new_v4();
        let a = actor(&db, id, if mode == "capacity" { 1 } else { 128 }).await;
        let actions = match mode {
            "invalid" => vec![
                node("valid-first", "key", Command::PanelLed(true)),
                node("invalid-second", "dimmer", Command::PanelLed(false)),
            ],
            "capacity" => vec![
                node("first", "dimmer", level(0)),
                node("second", "dimmer", level(0)),
            ],
            _ => {
                let mut nodes = (0..64)
                    .map(|i| node(&format!("combo-{i}"), "combo", level(100)))
                    .collect::<Vec<_>>();
                nodes.push(node("last", "key", Command::PanelLed(true)));
                nodes
            }
        };
        let report = event(&a, id, actions, BTreeMap::new(), vec![]).await;
        assert!(report.failure.is_some());
        assert!(report.completed.is_empty());
        assert!(a.task_generation().await.is_ok());
        assert!(a.dispatch().await.unwrap().is_none());
        let sql = rusqlite::Connection::open(db.path()).unwrap();
        for table in ["command_journal", "rule_admission", "rule_command"] {
            assert_eq!(
                sql.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r
                    .get::<_, i64>(0))
                    .unwrap(),
                0
            );
        }
        a.shutdown().await.unwrap();
    }
}
#[tokio::test]
async fn repeated_node_source_insert_failure_rolls_back_variables_and_closes_gate() {
    let db = Database::new();
    let id = Uuid::new_v4();
    db.store()
        .publish_configuration(None, &config(), ExecutionContext::Simulation, &[])
        .unwrap();
    db.store()
        .define_logical_variable(1, "mode", ValueType::Integer)
        .unwrap();
    let a = open(&db, id, 8).await;
    let sql = rusqlite::Connection::open(db.path()).unwrap();
    sql.execute_batch("CREATE TRIGGER fail_source BEFORE INSERT ON rule_command WHEN NEW.source='second' BEGIN SELECT RAISE(ABORT,'synthetic'); END;").unwrap();
    let report = event(
        &a,
        id,
        vec![
            Node {
                source: "variable".into(),
                action: Action::Set {
                    variable: "mode".into(),
                    value: Expr::Literal(Value::Integer(1)),
                },
            },
            node("first", "key", Command::PanelLed(true)),
            node("second", "key", Command::PanelLed(false)),
        ],
        BTreeMap::new(),
        vec!["mode".into()],
    )
    .await;
    assert!(report.failure.is_some());
    assert!(report.completed.is_empty());
    assert!(a.task_generation().await.is_err());
    for table in ["command_journal", "rule_admission", "rule_command"] {
        assert_eq!(
            sql.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    let mode = db.store().logical_variable(1, "mode").unwrap().unwrap();
    assert!(mode.value.is_none());
    assert_eq!(mode.version, 0);
    a.shutdown().await.unwrap();
}
#[tokio::test]
async fn cancellation_and_manual_override_remove_all_pending_repeat_nodes_without_replay() {
    for manual in [false, true] {
        let db = Database::new();
        let id = Uuid::new_v4();
        let a = actor(&db, id, 8).await;
        let boot = a.boot_id().to_owned();
        let report = event(
            &a,
            id,
            vec![
                node("first", "dimmer", level(0)),
                node("second", "dimmer", level(0)),
            ],
            BTreeMap::new(),
            vec![],
        )
        .await;
        assert!(report.failure.is_none());
        if manual {
            let ids = a
                .submit(Batch {
                    controller: id,
                    intents: vec![Intent {
                        device: DeviceId::new("dimmer").unwrap(),
                        command: level(100),
                    }],
                    subscriptions: vec![],
                })
                .await
                .unwrap();
            assert_eq!(emit(&a, id, &ids).await, [0xae, 0x12, 1, 100, 0xee]);
        } else {
            let generation = a.task_generation().await.unwrap();
            assert_eq!(
                a.cancel_scene_run(id, generation, report.completed[0].commands[0])
                    .await
                    .unwrap(),
                2
            );
            assert!(a.dispatch().await.unwrap().is_none());
        }
        a.shutdown().await.unwrap();
        verify(
            &db,
            &boot,
            &report,
            &[
                ("dimmer", "first", level(0), Status::Cancelled),
                ("dimmer", "second", level(0), Status::Cancelled),
            ],
        );
        let reopened = open(&db, id, 8).await;
        assert!(reopened.dispatch().await.unwrap().is_none());
        reopened.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn partial_wire_failure_preserves_first_written_node_and_never_replays_tail() {
    for budget in [5usize,7]{
    let db = Database::new();
    let id = Uuid::new_v4();
    let a = actor(&db, id, 8).await;
    let boot = a.boot_id().to_owned();
    let report = event(
        &a,
        id,
        vec![
            node("first", "key", Command::PanelLed(true)),
            node("second", "key", Command::PanelLed(false)),
            node("third", "key", Command::PanelLed(true)),
        ],
        BTreeMap::new(),
        vec![],
    )
    .await;
    assert!(report.failure.is_none());
    assert_eq!(report.completed[0].commands.len(), 3);
    let (mut owner, bytes) = wire(&a, Some(budget));
    assert!(owner.step_with_frame_budget(1).await.unwrap().unwrap().result.is_ok());
    assert!(owner.step_with_frame_budget(1).await.unwrap().unwrap().result.is_err());
    assert!(owner.step_with_frame_budget(1).await.is_err());
    assert_eq!(*bytes.lock().unwrap(),[0xae,0x41,1,0xf1,0xee,0xae,0x41][..budget]);
    assert!(a.task_generation().await.is_err());
    a.shutdown().await.unwrap();
    let mut store = db.store();
    let page = store
        .rule_chain(&boot, report.correlation_id, 0, 64)
        .unwrap();
    assert_eq!(
        page.records[0]
            .commands
            .iter()
            .map(|c| c.source.as_deref())
            .collect::<Vec<_>>(),
        [Some("first"), Some("second"), Some("third")]
    );
    assert_eq!(page.records[0].commands[0].status, Status::Written);
    assert_eq!(page.records[0].commands[1].status, if budget==5{Status::Failed}else{Status::Uncertain});
    assert_eq!(page.records[0].commands[2].status, Status::Cancelled);
    drop(store);
    let recovered = open(&db, id, 8).await;
    assert!(recovered.dispatch().await.unwrap().is_none());
    recovered.shutdown().await.unwrap();
    }
}
