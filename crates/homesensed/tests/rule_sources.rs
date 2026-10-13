//! Source receipts are exercised without sockets, identities or a wire owner.
use homesense_core::{
    Command, DeviceId, ExecutionContext, LevelTransition, Percent,
    rule::{Action, Node},
};
use homesense_storage::Store;
use homesensed::{
    admission::{Admission, Options},
    event_rules::{Binding, EventRules, Trigger},
};
use uuid::Uuid;

struct Database(std::path::PathBuf);
impl Database {
    fn new() -> Self {
        let directory =
            std::env::temp_dir().join(format!("homesense-runtime-sources-{}", Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        Self(directory)
    }
    fn path(&self) -> std::path::PathBuf {
        self.0.join("runtime.sqlite")
    }
}
impl Drop for Database {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn configuration(combined: bool) -> Vec<u8> {
    let mut value = serde_json::json!({"schema_version":2,"revision":1,
        "gateways":[{"id":"synthetic-bus","endpoint":"127.0.0.1:50007","modules":[
            {"id":"fixed-relay","address":"03","profile":"r8i8_din_pro_v2_7_7","verification":{"status":"simulated"}},
            {"id":"fixed-dimmer","address":"12","profile":"ad8_din_rs485_v2_2_1","verification":{"status":"simulated"}},
            {"id":"fixed-panel","address":"41","profile":"ts6_86_rs485_v2_6_5_6key","led_mode":"manual","verification":{"status":"simulated"}}]}],
        "devices":[{"id":"first","name":"First synthetic load","module_id":"fixed-relay","channel":1,"enabled":true},
            {"id":"second","name":"Second synthetic load","module_id":"fixed-relay","channel":2,"enabled":true},
            {"id":"third","name":"Third synthetic load","module_id":"fixed-relay","channel":3,"enabled":true},
            {"id":"panel-key","name":"Synthetic panel key","module_id":"fixed-panel","channel":1,"enabled":true}]});
    if combined {
        value["devices"].as_array_mut().unwrap().push(serde_json::json!({"id":"combined","name":"Synthetic combined light","module_id":"fixed-dimmer","channel":4,"enabled":true,"default_on_level":100,"supply":{"module_id":"fixed-relay","channel":1}}));
    }
    serde_json::to_vec(&value).unwrap()
}
fn node(source: &str, device: &str, command: Command) -> Node {
    Node {
        source: source.into(),
        action: Action::Command {
            device: DeviceId::new(device).unwrap(),
            command,
        },
    }
}
async fn actor(db: &Database, controller: Uuid, combined: bool) -> Admission {
    Store::open(db.path())
        .unwrap()
        .publish_configuration(
            None,
            &configuration(combined),
            ExecutionContext::Simulation,
            &[],
        )
        .unwrap();
    open_actor(db, controller).await
}
async fn open_actor(db: &Database, controller: Uuid) -> Admission {
    let admission = Admission::open(
        db.path(),
        Options {
            context: ExecutionContext::Simulation,
            ..Options::default()
        },
        move |id| id == controller,
    )
    .await
    .unwrap();
    admission.synchronized(1, 1).await.unwrap();
    admission
}
async fn event(db: &Database, combined: bool, actions: Vec<Node>) -> (String, u128, Vec<u64>) {
    let controller = Uuid::new_v4();
    let admission = actor(db, controller, combined).await;
    let mut router = EventRules::new(
        admission.clone(),
        controller,
        vec![Binding {
            id: "synthetic-rule".into(),
            trigger: Trigger::StartupSynchronized,
            variables: vec![],
            actions,
        }],
        Default::default(),
        Default::default(),
    )
    .await
    .unwrap();
    let report = router.dispatch(Trigger::StartupSynchronized, None).await;
    assert!(report.failure.is_none(), "{:?}", report.failure);
    assert_eq!(report.completed.len(), 1);
    let boot = admission.boot_id().to_owned();
    let ids = report.completed[0].commands.clone();
    admission.shutdown().await.unwrap();
    (boot, report.correlation_id, ids)
}
#[tokio::test]
async fn two_same_kind_independent_channels_keep_exact_node_labels_after_reopen() {
    let db = Database::new();
    let labels = ["event:1/then:first", "event:1/then:second"];
    let (boot, correlation, ids) = event(
        &db,
        false,
        vec![
            node(labels[0], "first", Command::Switch(true)),
            node(labels[1], "second", Command::Switch(true)),
        ],
    )
    .await;
    let mut store = Store::open(db.path()).unwrap();
    let page = store.rule_chain(&boot, correlation, 0, 64).unwrap();
    let commands = &page.records[0].commands;
    assert_eq!(commands.iter().map(|c| c.id).collect::<Vec<_>>(), ids);
    assert_eq!(
        commands
            .iter()
            .map(|c| (c.device.as_str(), c.source.as_deref()))
            .collect::<Vec<_>>(),
        [("first", Some(labels[0])), ("second", Some(labels[1]))]
    );
}
#[tokio::test]
async fn combined_expansion_copies_source_to_each_output_without_shifting_next_node() {
    let db = Database::new();
    let labels = ["event:1/combined-node", "event:1/independent-node"];
    let (boot, correlation, ids) = event(
        &db,
        true,
        vec![
            node(
                labels[0],
                "combined",
                Command::SetLevel {
                    level: Percent::new(40).unwrap(),
                    transition: LevelTransition::Immediate,
                },
            ),
            node(labels[1], "second", Command::Switch(true)),
        ],
    )
    .await;
    let mut store = Store::open(db.path()).unwrap();
    let page = store.rule_chain(&boot, correlation, 0, 64).unwrap();
    let commands = &page.records[0].commands;
    assert_eq!(ids.len(), 3);
    assert_eq!(commands.iter().map(|c| c.id).collect::<Vec<_>>(), ids);
    assert_eq!(
        commands
            .iter()
            .map(|c| (c.device.as_str(), c.source.as_deref()))
            .collect::<Vec<_>>(),
        [
            ("combined", Some(labels[0])),
            ("first", Some(labels[0])),
            ("second", Some(labels[1]))
        ]
    );
}
#[tokio::test]
async fn panel_click_retains_audited_source_labels() {
    use homesense_core::gesture::{Event, Settings};
    use homesensed::panel_rules::{Binding as PanelBinding, Key, Mode, PanelRules};
    let db = Database::new();
    let controller = Uuid::new_v4();
    let admission = actor(&db, controller, false).await;
    let key = DeviceId::new("panel-key").unwrap();
    let mut router = PanelRules::new(
        admission.clone(),
        controller,
        Mode::Execute,
        vec![Key {
            device: key.clone(),
            settings: Settings::default(),
            bindings: vec![PanelBinding {
                id: "synthetic-click".into(),
                event: Event::Click,
                variables: vec![],
                actions: vec![
                    node("panel:click/node:load", "first", Command::Switch(true)),
                    node("panel:click/node:led", "panel-key", Command::PanelLed(true)),
                ],
            }],
        }],
        Default::default(),
        0,
    )
    .await
    .unwrap();
    assert!(router.edge(&key, true, 0).await.unwrap().is_empty());
    let fired = router.edge(&key, false, 50).await.unwrap();
    assert_eq!(fired.len(), 1);
    let correlation = fired[0].cause.as_ref().unwrap().correlation_id();
    let boot = admission.boot_id().to_owned();
    admission.shutdown().await.unwrap();
    let mut store = Store::open(db.path()).unwrap();
    let page = store.rule_chain(&boot, correlation, 0, 64).unwrap();
    assert_eq!(
        page.records[0]
            .commands
            .iter()
            .map(|c| c.source.as_deref())
            .collect::<Vec<_>>(),
        [Some("panel:click/node:load"), Some("panel:click/node:led")]
    );
}
#[tokio::test]
async fn mixed_deferred_rule_retains_immediate_and_existing_call_member_sources() {
    use homesense_core::rule_chain::Chain;
    use homesensed::scene_scheduler::SceneScheduler;
    let db = Database::new();
    let controller = Uuid::new_v4();
    let call = "event:1/node:scene";
    let member = "scene:one/member:0";
    let immediate = "event:1/node:immediate";
    let document = serde_json::json!({"schema_version":1,"configuration_revision":1,"variables":[],
        "rules":[{"id":"mixed-rule","enabled":true,"review_evidence":"synthetic offline test","source":"rule:source","trigger":{"kind":"startup_synchronized"},
            "actions":[{"source":immediate,"action":{"kind":"command","device":"third","command":{"kind":"switch","on":true}}},
                {"source":call,"action":{"kind":"scene","id":"timed","activate":true}}]}],
        "timed_scenes":[{"id":"timed","enabled":true,"review_evidence":"synthetic offline test","max_lateness_ms":1000,
            "steps":[{"source":member,"offset_ms":0,"device":"second","command":{"kind":"switch","on":true}}]}]});
    Store::open(db.path())
        .unwrap()
        .publish_automation(
            None,
            &configuration(false),
            &serde_json::to_vec(&document).unwrap(),
            ExecutionContext::Simulation,
            &[],
        )
        .unwrap();
    let admission = open_actor(&db, controller).await;
    let mut scheduler = SceneScheduler::new(admission.clone(), controller, 1, 0).unwrap();
    let mut chain = Chain::new(77, 0, 60000, Default::default(), 32).unwrap();
    let parent = chain.root();
    let outcome = homesensed::deferred_rule::execute(
        &admission,
        controller,
        "mixed-rule",
        &mut chain,
        &parent,
        &mut scheduler,
        None,
    )
    .await
    .unwrap();
    assert_eq!(outcome.command_ids.len(), 1);
    assert_eq!(outcome.scene_runs.len(), 1);
    let ticket = admission.dispatch().await.unwrap().unwrap();
    assert_eq!(ticket.command.id, outcome.command_ids[0]);
    admission
        .finish_write(ticket.command.id, true)
        .await
        .unwrap();
    let boot = admission.boot_id().to_owned();
    let store = Store::open(db.path()).unwrap();
    let page = store
        .query_scene(
            1,
            &homesense_storage::scene_run::Query::Latest {
                definition: "timed".into(),
            },
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        store.scene_cause(&boot, &page.run).unwrap().unwrap().source,
        call
    );
    drop(store);
    scheduler.tick(page.record.started_ms + 100).await.unwrap();
    let ticket = admission.dispatch().await.unwrap().unwrap();
    admission
        .finish_write(ticket.command.id, true)
        .await
        .unwrap();
    drop(scheduler);
    admission.shutdown().await.unwrap();
    let mut reopened = Store::open(db.path()).unwrap();
    let audit = reopened.rule_chain(&boot, 77, 0, 64).unwrap();
    assert_eq!(
        audit.records[0].commands[0].source.as_deref(),
        Some(immediate)
    );
    let delayed = reopened.scene_commands(&boot, &page.run, 0, 128).unwrap();
    assert_eq!(delayed.len(), 1);
    assert_eq!(delayed[0].source, format!("{call} -> {member}"));
}
