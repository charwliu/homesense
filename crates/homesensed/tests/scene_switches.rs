use homesense_config::rule_document::Document;
use homesense_core::{
    ExecutionContext,
    rule::{Lifetime, Value, ValueType},
};
use homesense_storage::variables::Stored;
use homesensed::{
    automation_control::{Action, Reply},
    scene_switches,
};
use serde_json::json;
use std::{collections::BTreeMap, time::Duration};

fn fixture() -> serde_json::Value {
    let scenes=[("reading",100),("daily",60),("eco",17),("off",0)].map(|(id,level)|json!({"id":id,"enabled":true,"review_evidence":"synthetic reviewed scene","max_lateness_ms":1000,"steps":[{"source":"synthetic","offset_ms":60000,"device":"simulation:light","command":{"kind":"level","value":level,"ramp":false}}]}));
    json!({"schema_version":1,"configuration_revision":1,"variables":[{"id":"selection","kind":"integer","lifetime":"persistent"}],"rules":[],
        "timed_scenes":scenes,
        "scene_switch_groups":[{"id":"room","variable":"selection","review_evidence":"synthetic reviewed toggle and exclusion","members":[{"scene":"reading","value":1,"off_scene":"off"},{"scene":"daily","value":2,"off_scene":"off"},{"scene":"eco","value":3,"off_scene":"off"}],"reset_scenes":["off"]}]})
}
fn parse(input: &serde_json::Value) -> Result<Document, homesense_config::Error> {
    let config = homesense_config::Configuration::parse(
        include_bytes!("../../../config/simulation.json"),
        ExecutionContext::Simulation,
    )
    .unwrap();
    Document::parse(&serde_json::to_vec(input).unwrap(), &config)
}
fn snapshot(value: Option<i64>) -> BTreeMap<String, Stored> {
    [(
        "selection".into(),
        Stored {
            lifetime: Lifetime::Persistent,
            kind: ValueType::Integer,
            value: value.map(Value::Integer),
            version: 7,
        },
    )]
    .into()
}
fn write(id: &str, on: Option<bool>) -> (String, Option<bool>) {
    (id.into(), on)
}

#[test]
fn reviewed_group_validation_rejects_ambiguous_or_incomplete_bindings() {
    let base = fixture();
    assert!(parse(&base).is_ok());
    let mut cases = vec![];
    let mut v = base.clone();
    v["variables"][0]["lifetime"] = "temporary".into();
    cases.push(v);
    let mut v = base.clone();
    v["scene_switch_groups"][0]["members"][1]["value"] = 1.into();
    cases.push(v);
    let mut v = base.clone();
    v["scene_switch_groups"][0]["members"][1]["off_scene"] = "missing".into();
    cases.push(v);
    let mut v = base.clone();
    v["scene_switch_groups"][0]["members"][1]["off_scene"] = "reading".into();
    cases.push(v);
    let mut v = base.clone();
    v["scene_switch_groups"][0]["review_evidence"] = "".into();
    cases.push(v);
    let mut v = base.clone();
    v["timed_scenes"][1]["enabled"] = false.into();
    cases.push(v);
    let mut v = base.clone();
    v["scene_switch_groups"][0]["toggle_implicitly"] = true.into();
    cases.push(v);
    let mut v = base.clone();
    let group = v["scene_switch_groups"][0].clone();
    v["scene_switch_groups"].as_array_mut().unwrap().push(group);
    cases.push(v);
    for v in cases {
        assert!(parse(&v).is_err(), "accepted invalid group: {v}");
    }
}

#[test]
fn set_on_off_exclusion_and_subscription_planning_preserve_local_mode_semantics() {
    let doc = parse(&fixture()).unwrap();
    let plan =
        scene_switches::plan(&doc, &[write("reading", Some(true))], &snapshot(None)).unwrap();
    assert_eq!(plan.definitions, ["reading"]);
    assert_eq!(plan.updates[0].value, Some(Value::Integer(1)));
    assert_eq!(plan.updates[0].expected_version, 7);
    assert!(scene_switches::plan(&doc, &[write("reading", Some(false))], &snapshot(None)).is_err());
    let plan =
        scene_switches::plan(&doc, &[write("reading", Some(false))], &snapshot(Some(1))).unwrap();
    assert_eq!(plan.definitions, ["off"]);
    assert_eq!(plan.updates[0].value, Some(Value::Integer(0)));
    for on in [Some(false), None] {
        let plan = scene_switches::plan(&doc, &[write("reading", on)], &snapshot(Some(2))).unwrap();
        assert!(plan.definitions.is_empty() && plan.updates.is_empty());
    }
    let plan = scene_switches::plan(
        &doc,
        &[write("daily", Some(false)), write("eco", Some(true))],
        &snapshot(Some(2)),
    )
    .unwrap();
    assert_eq!(plan.definitions, ["eco"]);
    assert_eq!(plan.updates[0].value, Some(Value::Integer(3)));
    let plan = scene_switches::plan(&doc, &[write("eco", Some(true))], &snapshot(Some(3))).unwrap();
    assert!(plan.definitions.is_empty() && plan.updates.is_empty());
    assert!(
        scene_switches::plan(
            &doc,
            &[write("daily", Some(true)), write("eco", Some(true))],
            &snapshot(Some(1))
        )
        .is_err()
    );
    assert!(
        scene_switches::plan(
            &doc,
            &[write("off", Some(true)), write("eco", Some(true))],
            &snapshot(Some(1))
        )
        .is_err()
    );
    let plan = scene_switches::plan(&doc, &[write("off", Some(true))], &snapshot(None)).unwrap();
    assert_eq!(plan.definitions, ["off"]);
    assert_eq!(plan.updates[0].value, Some(Value::Integer(0)));
    let states = scene_switches::states(&doc, &snapshot(Some(2))).unwrap();
    assert_eq!(states["daily"], Some(true));
    assert_eq!(states["reading"], Some(false));
    assert_eq!(states["eco"], Some(false));
    assert!(scene_switches::states(&doc, &snapshot(Some(9))).is_err());
    assert!(
        scene_switches::states(&doc, &snapshot(None))
            .unwrap()
            .values()
            .all(Option::is_none)
    );
}

#[test]
fn scene_selection_and_manual_receipts_commit_together_or_roll_back_together() {
    use homesense_storage::{Store, scene_run::ManualStart, variables::Update};
    let root = std::env::temp_dir().join(format!("switch-cas-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let path = root.join("runtime.sqlite");
    let mut store = Store::open(&path).unwrap();
    store
        .publish_automation(
            None,
            include_bytes!("../../../config/simulation.json"),
            &serde_json::to_vec(&fixture()).unwrap(),
            ExecutionContext::Simulation,
            &[],
        )
        .unwrap();
    store
        .ensure_logical_variables(1, &[("selection".into(), ValueType::Integer)])
        .unwrap();
    let update = |version, value| Update {
        revision: 1,
        name: "selection".into(),
        expected_version: version,
        value: Some(Value::Integer(value)),
    };
    let start = |run: &str, scene: &str| ManualStart {
        run: run.into(),
        definition: scene.into(),
    };
    store
        .begin_manual_scene_batch_with_variables(
            "boot",
            1,
            &[start("first", "reading")],
            1,
            &[update(0, 1)],
        )
        .unwrap();
    assert!(
        store
            .begin_manual_scene_batch_with_variables(
                "boot",
                1,
                &[start("stale", "eco")],
                2,
                &[update(0, 3)]
            )
            .is_err()
    );
    assert!(store.scene_run("boot", "stale").unwrap().is_none());
    let sql = rusqlite::Connection::open(&path).unwrap();
    sql.execute_batch("CREATE TRIGGER reject_switch_scene BEFORE INSERT ON scene_run WHEN NEW.run_id='failed' BEGIN SELECT RAISE(ABORT,'synthetic scene write failure'); END;").unwrap();
    assert!(
        store
            .begin_manual_scene_batch_with_variables(
                "boot",
                1,
                &[start("failed", "eco")],
                2,
                &[update(1, 3)]
            )
            .is_err()
    );
    assert_eq!(
        store
            .logical_variable(1, "selection")
            .unwrap()
            .unwrap()
            .value,
        Some(Value::Integer(1))
    );
    assert!(store.scene_run("boot", "failed").unwrap().is_none());
    store
        .begin_manual_scene_batch_with_variables(
            "boot",
            1,
            &[start("next", "eco")],
            3,
            &[update(1, 3)],
        )
        .unwrap();
    drop(store);
    let store = Store::open(&path).unwrap();
    assert_eq!(
        store
            .logical_variable(1, "selection")
            .unwrap()
            .unwrap()
            .value,
        Some(Value::Integer(3))
    );
    assert_eq!(
        sql.query_row("SELECT count(*) FROM command_journal", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    drop(sql);
    drop(store);
    std::fs::remove_dir_all(root).unwrap();
}

struct Idle;
impl std::io::Read for Idle {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        Err(std::io::ErrorKind::WouldBlock.into())
    }
}
impl std::io::Write for Idle {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        panic!("delayed scenes must not write during this test: {bytes:?}")
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn runtime_accepts_exclusive_scene_writes_and_restores_selection_without_replaying_output() {
    use homesensed::{
        admission::{Admission, Options},
        automation_runtime::AutomationRuntime,
        feedback::Feedback,
        observed_transport::ObservedTransport,
        transport_owner::TransportOwner,
    };
    let root = std::env::temp_dir().join(format!("switch-runtime-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let path = root.join("runtime.sqlite");
    let rules = serde_json::to_vec(&fixture()).unwrap();
    homesense_storage::Store::open(&path)
        .unwrap()
        .publish_automation(
            None,
            include_bytes!("../../../config/simulation.json"),
            &rules,
            ExecutionContext::Simulation,
            &[],
        )
        .unwrap();
    let caller = uuid::Uuid::new_v4();
    for restarting in [false, true] {
        let a = Admission::open(
            path.clone(),
            Options {
                context: ExecutionContext::Simulation,
                ..Default::default()
            },
            move |id| id == caller,
        )
        .await
        .unwrap();
        a.synchronized(1, 1).await.unwrap();
        let mut runtime = AutomationRuntime::load(
            a.clone(),
            caller,
            1_800_000_000_000,
            1000,
            16,
            Default::default(),
        )
        .await
        .unwrap();
        let control = runtime.control();
        let config = homesense_config::Configuration::parse(
            include_bytes!("../../../config/simulation.json"),
            ExecutionContext::Simulation,
        )
        .unwrap();
        let feedback = Feedback::new(
            &config,
            ExecutionContext::Simulation,
            uuid::Uuid::parse_str(a.boot_id()).unwrap().as_u128(),
            1,
        )
        .unwrap();
        let mut owner = ObservedTransport::new(
            TransportOwner::new(
                a.clone(),
                [(
                    "simulation".into(),
                    hb_transport::Connection::for_simulation(Idle, 1),
                )]
                .into(),
            ),
            feedback,
        );
        let (stop, receive) = tokio::sync::watch::channel(false);
        let drive = tokio::spawn(async move {
            runtime
                .drive(&mut owner, Duration::from_millis(5), 5000, receive, || {
                    Ok(1_800_000_000_000)
                })
                .await
        });
        if !restarting {
            let write = |scene: &str, on: Option<bool>| Action::SceneWrites {
                controller: caller,
                revision: 1,
                scenes: vec![(scene.into(), on)],
            };
            assert!(
                control
                    .request(write("reading", Some(false)))
                    .await
                    .is_err()
            );
            for (id, on, runs) in [
                ("reading", Some(true), 1),
                ("reading", None, 0),
                ("reading", Some(false), 1),
                ("daily", Some(true), 1),
                ("eco", Some(true), 1),
                ("eco", Some(true), 0),
            ] {
                let Reply::ScenesAccepted(accepted) = control.request(write(id, on)).await.unwrap()
                else {
                    panic!("wrong write reply")
                };
                assert_eq!(accepted.len(), runs);
            }
            assert!(
                control
                    .request(Action::SceneWrites {
                        controller: caller,
                        revision: 1,
                        scenes: vec![("daily".into(), Some(true)), ("eco".into(), Some(true))]
                    })
                    .await
                    .is_err()
            );
        }
        let Reply::SceneSwitchStates(states) = control
            .request(Action::SceneSwitchStates {
                controller: caller,
                revision: 1,
            })
            .await
            .unwrap()
        else {
            panic!("wrong states reply")
        };
        assert_eq!(states["eco"], Some(true));
        assert_eq!(states["daily"], Some(false));
        assert_eq!(states["reading"], Some(false));
        assert!(
            control
                .request(Action::SceneSwitchStates {
                    controller: uuid::Uuid::new_v4(),
                    revision: 1
                })
                .await
                .is_err()
        );
        assert!(
            control
                .request(Action::SceneSwitchStates {
                    controller: caller,
                    revision: 2
                })
                .await
                .is_err()
        );
        stop.send(true).unwrap();
        drive.await.unwrap().unwrap();
        a.shutdown().await.unwrap();
    }
    let sql = rusqlite::Connection::open(path).unwrap();
    assert_eq!(
        sql.query_row("SELECT count(*) FROM command_journal", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    drop(sql);
    std::fs::remove_dir_all(root).unwrap();
}
