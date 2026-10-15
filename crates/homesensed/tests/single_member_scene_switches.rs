//! Independent manual scene toggles over an in-memory wire; no field endpoints.
use homesense_core::ExecutionContext;
use homesensed::{
    admission::{Admission, Options},
    automation_control::{Action, Control, Reply},
    automation_runtime::AutomationRuntime,
    feedback::Feedback,
    observed_transport::ObservedTransport,
    transport_owner::TransportOwner,
};
use serde_json::json;
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    sync::{Arc, Mutex},
    time::Duration,
};
use uuid::Uuid;

fn fixtures() -> (Vec<u8>, Vec<u8>) {
    let devices = json!({"schema_version":1,"revision":1,
    "gateways":[{"id":"bus","endpoint":"127.0.0.1:50007","modules":[
        {"address":"15","profile":"gt_d8_rl_v2_2_6_2","verification":{"status":"simulated"}}
    ]}],"devices":[
        {"id":"courtyard-light","name":"Courtyard","gateway":"bus","address":"15","channel":1,"enabled":true},
        {"id":"home-light","name":"Passage","gateway":"bus","address":"15","channel":2,"enabled":true}
    ]});
    let mut variables = vec![];
    let mut scenes = vec![];
    let mut groups = vec![];
    for (name, light) in [("courtyard", "courtyard-light"), ("home", "home-light")] {
        let variable = format!("{name}-selection");
        let off = format!("{name}-off");
        variables.push(json!({"id":variable,"kind":"integer","lifetime":"persistent"}));
        for (id, level) in [(name.to_owned(), 100), (off.clone(), 0)] {
            scenes.push(json!({"id":id,"enabled":true,"review_evidence":"synthetic independent manual scene",
                "max_lateness_ms":1000,"steps":[{"source":"synthetic","offset_ms":0,"device":light,
                    "command":{"kind":"level","value":level,"ramp":false}}]}));
        }
        groups.push(
            json!({"id":name,"variable":variable,"review_evidence":"synthetic independent toggle",
            "members":[{"scene":name,"value":1,"off_scene":off}],"reset_scenes":[off]}),
        );
    }
    let rules = json!({"schema_version":1,"configuration_revision":1,"variables":variables,
        "rules":[],"timed_scenes":scenes,"scene_switch_groups":groups});
    (
        serde_json::to_vec(&devices).unwrap(),
        serde_json::to_vec(&rules).unwrap(),
    )
}
struct Wire(Arc<Mutex<Vec<[u8; 5]>>>);
impl Read for Wire {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        Err(std::io::ErrorKind::WouldBlock.into())
    }
}
impl Write for Wire {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().push(bytes.try_into().unwrap());
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
async fn states(control: &Control, controller: Uuid) -> BTreeMap<String, Option<bool>> {
    let Reply::SceneSwitchStates(states) = control
        .request(Action::SceneSwitchStates {
            controller,
            revision: 1,
        })
        .await
        .unwrap()
    else {
        panic!("scene state reply expected")
    };
    states
}
async fn write(
    control: &Control,
    controller: Uuid,
    scenes: Vec<(&str, bool)>,
) -> Result<Reply, String> {
    control
        .request(Action::SceneWrites {
            controller,
            revision: 1,
            scenes: scenes
                .into_iter()
                .map(|(name, on)| (name.into(), Some(on)))
                .collect(),
        })
        .await
}

#[tokio::test]
async fn independent_singletons_execute_on_off_without_exclusion_or_unknown_guessing() {
    let root = std::env::temp_dir().join(format!("singleton-scenes-{}", Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let path = root.join("runtime.sqlite");
    let (devices, rules) = fixtures();
    let config =
        homesense_config::Configuration::parse(&devices, ExecutionContext::Simulation).unwrap();
    homesense_storage::Store::open(&path)
        .unwrap()
        .publish_automation(None, &devices, &rules, ExecutionContext::Simulation, &[])
        .unwrap();
    let controller = Uuid::new_v4();
    let actor = Admission::open(
        path.clone(),
        Options {
            context: ExecutionContext::Simulation,
            ..Default::default()
        },
        move |id| id == controller,
    )
    .await
    .unwrap();
    let mut runtime = AutomationRuntime::load(
        actor.clone(),
        controller,
        1_800_000_000_000,
        1000,
        16,
        Default::default(),
    )
    .await
    .unwrap();
    let control = runtime.control();
    let frames = Arc::new(Mutex::new(vec![]));
    let feedback = Feedback::new(
        &config,
        ExecutionContext::Simulation,
        Uuid::parse_str(actor.boot_id()).unwrap().as_u128(),
        1,
    )
    .unwrap();
    let mut owner = ObservedTransport::new(
        TransportOwner::new(
            actor.clone(),
            [(
                "bus".into(),
                hb_transport::Connection::for_simulation(Wire(frames.clone()), 1),
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
    let initial = states(&control, controller).await;
    assert_eq!(initial["courtyard"], None);
    assert_eq!(initial["home"], None);
    assert!(
        write(&control, controller, vec![("courtyard", false)])
            .await
            .is_err()
    );
    assert!(frames.lock().unwrap().is_empty());
    for (commands, expected) in [
        (vec![("courtyard", true)], [Some(true), None]),
        (vec![("home", true)], [Some(true), Some(true)]),
        (vec![("courtyard", false)], [Some(false), Some(true)]),
        (vec![("home", false)], [Some(false), Some(false)]),
        (
            vec![("courtyard", true), ("home", true)],
            [Some(true), Some(true)],
        ),
    ] {
        assert!(matches!(
            write(&control, controller, commands).await.unwrap(),
            Reply::ScenesAccepted(_)
        ));
        let actual = states(&control, controller).await;
        assert_eq!([actual["courtyard"], actual["home"]], expected);
        tokio::time::sleep(Duration::from_millis(60)).await;
    }
    tokio::time::timeout(Duration::from_secs(2), async {
        while frames.lock().unwrap().len() < 6 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let expected = [
        [0xae, 0x15, 0x01, 100, 0xee],
        [0xae, 0x15, 0x02, 100, 0xee],
        [0xae, 0x15, 0x01, 0, 0xee],
        [0xae, 0x15, 0x02, 0, 0xee],
        [0xae, 0x15, 0x01, 100, 0xee],
        [0xae, 0x15, 0x02, 100, 0xee],
    ];
    assert_eq!(*frames.lock().unwrap(), expected);
    let repeated = write(&control, controller, vec![("courtyard", true)])
        .await
        .unwrap();
    assert!(matches!(repeated, Reply::ScenesAccepted(runs) if runs.is_empty()));
    stop.send(true).unwrap();
    drive.await.unwrap().unwrap();
    actor.shutdown().await.unwrap();
    let db = rusqlite::Connection::open(&path).unwrap();
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM command_journal WHERE status='written'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        6
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM command_journal", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        6
    );
    drop(db);
    std::fs::remove_dir_all(root).unwrap();
}
