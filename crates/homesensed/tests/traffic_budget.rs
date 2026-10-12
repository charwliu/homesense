//! Shared software-fade allocation must leave ordinary controls admissible.
use homesense_core::{Command, DeviceId, ExecutionContext, LevelTransition, Percent};
use homesense_hap::requests::{Batch, Intent};
use homesense_storage::Store;
use homesensed::{
    admission::{Admission, Options},
    feedback::Feedback,
    software_fade::SoftwareFade,
    transport_owner::GatewayRead,
};
#[tokio::test]
async fn fade_headroom_rejection_does_not_block_manual_control_and_releases_on_cancel() {
    let path =
        std::env::temp_dir().join(format!("homesense-traffic-{}.sqlite", uuid::Uuid::new_v4()));
    let mut raw: serde_json::Value =
        serde_json::from_slice(include_bytes!("../../../config/simulation.json")).unwrap();
    let mut second = raw["devices"][0].clone();
    second["id"] = "second".into();
    second["channel"] = 2.into();
    raw["devices"].as_array_mut().unwrap().push(second);
    let devices = serde_json::to_vec(&raw).unwrap();
    Store::open(&path)
        .unwrap()
        .publish_configuration(None, &devices, ExecutionContext::Simulation, &[])
        .unwrap();
    let controller = uuid::Uuid::new_v4();
    let admission = Admission::open(
        path.clone(),
        Options {
            context: ExecutionContext::Simulation,
            write_interval: std::time::Duration::from_millis(200),
            ..Default::default()
        },
        move |id| id == controller,
    )
    .await
    .unwrap();
    admission.synchronized(1, 1).await.unwrap();
    let config =
        homesense_config::Configuration::parse(&devices, ExecutionContext::Simulation).unwrap();
    let mut feedback = Feedback::new(
        &config,
        ExecutionContext::Simulation,
        uuid::Uuid::parse_str(admission.boot_id())
            .unwrap()
            .as_u128(),
        1,
    )
    .unwrap();
    feedback
        .apply(&GatewayRead {
            gateway_id: "simulation".into(),
            transport_epoch: 1,
            received_at_ms: 0,
            result: hb_transport::Receive::Frames {
                transport_epoch: 1,
                frames: vec![
                    hb_protocol::RawFrame {
                        address: 0x15,
                        command: 0xd1,
                        value: 0,
                    },
                    hb_protocol::RawFrame {
                        address: 0x15,
                        command: 0xd2,
                        value: 0,
                    },
                ],
            },
        })
        .unwrap();
    let first = DeviceId::new("simulation:light").unwrap();
    let second = DeviceId::new("second").unwrap();
    let target = Percent::new(50).unwrap();
    // The writer can do five frames/s. Three are held for ordinary traffic,
    // so a three-frame/s fade fails before any output is admitted.
    assert!(
        SoftwareFade::start(
            admission.clone(),
            controller,
            first.clone(),
            target,
            1000,
            400,
            100,
            3,
            &feedback,
            5000
        )
        .await
        .is_err()
    );
    let mut fade = SoftwareFade::start(
        admission.clone(),
        controller,
        first.clone(),
        target,
        1000,
        500,
        100,
        2,
        &feedback,
        5000,
    )
    .await
    .unwrap();
    assert!(
        SoftwareFade::start(
            admission.clone(),
            controller,
            second.clone(),
            target,
            1000,
            500,
            100,
            2,
            &feedback,
            5000
        )
        .await
        .is_err()
    );
    let command = Command::SetLevel {
        level: Percent::new(70).unwrap(),
        transition: LevelTransition::Immediate,
    };
    admission
        .submit_for_revision(
            1,
            Batch {
                controller,
                intents: vec![Intent {
                    device: second.clone(),
                    command,
                }],
                subscriptions: vec![],
            },
        )
        .await
        .unwrap();
    let ticket = admission.dispatch().await.unwrap().unwrap();
    assert_eq!(ticket.command.device_id, second);
    assert_eq!(ticket.command.command, command);
    admission
        .finish_write(ticket.command.id, true)
        .await
        .unwrap();
    assert!(admission.task_generation().await.is_ok());
    fade.cancel().await.unwrap();
    let mut replacement = SoftwareFade::start(
        admission.clone(),
        controller,
        first,
        target,
        1000,
        500,
        100,
        2,
        &feedback,
        5000,
    )
    .await
    .unwrap();
    replacement.cancel().await.unwrap();
    assert!(admission.dispatch().await.unwrap().is_none());
    admission.shutdown().await.unwrap();
    drop(feedback);
    std::fs::remove_file(path).unwrap();
}
