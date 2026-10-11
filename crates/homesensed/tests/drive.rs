use homesense_core::{Command, DeviceId, ExecutionContext, LevelTransition, Percent};
use homesense_hap::{
    requests::{Batch, Intent},
    storage::IdentityStore,
};
use homesense_storage::{Store, journal::Status};
use homesensed::{
    admission::{Admission, Options},
    feedback::Feedback,
    observed_transport::ObservedTransport,
    runtime_hap::RuntimeHap,
    transport_owner::TransportOwner,
};
use std::{
    io::{self, Read, Write},
    sync::{Arc, Mutex},
    time::Duration,
};
use uuid::Uuid;
struct Stream {
    input: io::Cursor<Vec<u8>>,
    output: Arc<Mutex<Vec<u8>>>,
}
impl Read for Stream {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.input.read(bytes)
    }
}
impl Write for Stream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.output.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
async fn exercise(stop_first: bool, cancel: bool) {
    use hap::storage::Storage;
    let root = std::env::temp_dir().join(format!("homesense-drive-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let database = root.join("runtime.sqlite");
    let document = include_bytes!("../../../config/simulation-home.json");
    let config =
        homesense_config::Configuration::parse(document, ExecutionContext::Simulation).unwrap();
    let mut store = Store::open(&database).unwrap();
    store
        .publish_configuration(None, document, ExecutionContext::Simulation, &[])
        .unwrap();
    let controller = Uuid::new_v4();
    let admission = Admission::open(
        database,
        Options {
            context: ExecutionContext::Simulation,
            ..Default::default()
        },
        move |id| id == controller,
    )
    .await
    .unwrap();
    let boot = admission.boot_id().to_owned();
    let feedback = Feedback::new(
        &config,
        ExecutionContext::Simulation,
        Uuid::parse_str(&boot).unwrap().as_u128(),
        1,
    )
    .unwrap();
    let mut identity = IdentityStore::open(root.join("identity")).await.unwrap();
    identity.save_config(&hap::Config::default()).await.unwrap();
    let mut graph = RuntimeHap::new(
        &config,
        feedback.snapshot(),
        &identity,
        admission.clone(),
        5000,
    )
    .await
    .unwrap();
    let binding = graph.lights[0].0.clone();
    let requests = homesense_hap::requests::LightRequests::new(vec![binding.clone()]).unwrap();
    let on = hap::serde_json::to_vec(&hap::serde_json::json!({"characteristics":[{"aid":binding.aid,"iid":binding.on_iid,"value":true}]})).unwrap();
    let default_batch = requests.normalize(controller, &on, |_| None).unwrap();
    assert_eq!(
        default_batch.intents[0].command,
        Command::SetLevel {
            level: Percent::new(100).unwrap(),
            transition: LevelTransition::Immediate
        }
    );
    admission
        .submit(Batch {
            controller,
            intents: vec![Intent {
                device: DeviceId::new("simulation:light").unwrap(),
                command: Command::SetLevel {
                    level: Percent::new(30).unwrap(),
                    transition: LevelTransition::Immediate,
                },
            }],
            subscriptions: vec![],
        })
        .await
        .unwrap();
    let output = Arc::new(Mutex::new(Vec::new()));
    let stream = Stream {
        input: io::Cursor::new(vec![0xAE, 0x15, 0xD1, 27, 0xEE]),
        output: output.clone(),
    };
    let mut owner = ObservedTransport::new(
        TransportOwner::new(
            admission.clone(),
            [(
                "simulation".into(),
                hb_transport::Connection::for_simulation(stream, 1),
            )]
            .into(),
        ),
        feedback,
    );
    let (_stop, receiver) = tokio::sync::watch::channel(stop_first);
    let result = if cancel {
        let light = graph.lights[0].1.clone();
        let ready = async {
            loop {
                let value = light
                    .lock()
                    .await
                    .get_mut_service(hap::HapType::Lightbulb)
                    .unwrap()
                    .get_mut_characteristic(hap::HapType::Brightness)
                    .unwrap()
                    .get_value()
                    .await;
                if value.is_ok_and(|value| value == hap::serde_json::json!(27)) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        };
        tokio::select! {
            outcome = graph.drive(&mut owner, Duration::from_secs(1), receiver) => panic!("unexpected exit: {outcome:?}"),
            ready = tokio::time::timeout(Duration::from_secs(1), ready) => { ready.unwrap(); }
        }
        Err("cancelled".into())
    } else {
        graph
            .drive(&mut owner, Duration::from_millis(1), receiver)
            .await
    };
    if stop_first {
        assert!(result.is_ok(), "{result:?}");
        assert!(output.lock().unwrap().is_empty());
        assert_eq!(
            store.command_status(&boot, 1).unwrap(),
            Some(Status::Accepted)
        );
    } else {
        assert!(
            result
                .unwrap_err()
                .contains(if cancel { "cancelled" } else { "disconnected" })
        );
        assert_eq!(*output.lock().unwrap(), vec![0xAE, 0x15, 0x01, 30, 0xEE]);
        assert_eq!(
            store.command_status(&boot, 1).unwrap(),
            Some(Status::Written)
        );
        let snapshot = owner.snapshot();
        let state = &snapshot.devices[&DeviceId::new("simulation:light").unwrap()];
        assert_eq!(state.quality, homesense_state::Quality::Stale);
        assert!(state.observed.is_some());
    }
    assert!(admission.synchronized(1, 1).await.is_err());
    if cancel {
        admission.shutdown().await.unwrap();
    }
    let mut light = graph.lights[0].1.lock().await;
    assert!(
        light
            .get_mut_service(hap::HapType::Lightbulb)
            .unwrap()
            .get_mut_characteristic(hap::HapType::Brightness)
            .unwrap()
            .get_value()
            .await
            .is_err()
    );
    drop(light);
    drop(graph);
    drop(identity);
    drop(store);
    std::fs::remove_dir_all(root).unwrap();
}
#[tokio::test]
async fn drive_sends_observes_and_invalidates_on_eof() {
    exercise(false, false).await;
}
#[tokio::test]
async fn stopped_drive_never_dispatches_accepted_command() {
    exercise(true, false).await;
}

#[tokio::test]
async fn cancelled_drive_invalidates_confirmed_reads_and_stops_admission() {
    exercise(false, true).await;
}
