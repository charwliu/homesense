use homesense_core::{DeviceId, ExecutionContext, Observation, Percent};
use homesensed::{
    admission::{Admission, Options},
    feedback::Feedback,
    management_events,
    transport_owner::GatewayRead,
};
use uuid::Uuid;
#[tokio::test]
async fn state_resume_coalesces_and_resets_on_gap_boot_revision_or_scope_change() {
    let root = std::env::temp_dir().join(format!("homesense-events-{}", Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let path = root.join("runtime.sqlite");
    let bytes = include_bytes!("../../../config/simulation.json");
    homesense_storage::Store::open(&path)
        .unwrap()
        .publish_automation(
            None,
            bytes,
            br#"{"schema_version":1,"configuration_revision":1,"variables":[],"rules":[]}"#,
            ExecutionContext::Simulation,
            &[],
        )
        .unwrap();
    let caller = Uuid::new_v4();
    let admission = Admission::open(
        path,
        Options {
            context: ExecutionContext::Simulation,
            ..Default::default()
        },
        move |id| id == caller,
    )
    .await
    .unwrap();
    admission.synchronized(1, 1).await.unwrap();
    let generation = admission.task_generation().await.unwrap();
    let config =
        homesense_config::Configuration::parse(bytes, ExecutionContext::Simulation).unwrap();
    let mut feedback = Feedback::new(
        &config,
        ExecutionContext::Simulation,
        Uuid::parse_str(admission.boot_id()).unwrap().as_u128(),
        1,
    )
    .unwrap();
    let ids = vec![DeviceId::new("simulation:light").unwrap()];
    let initial = management_events::read(
        &admission,
        caller,
        generation.clone(),
        ids.clone(),
        &feedback,
        None,
    )
    .await
    .unwrap();
    assert!(initial.reset);
    assert_eq!(initial.snapshot.devices.len(), 1);
    let idle = management_events::read(
        &admission,
        caller,
        generation.clone(),
        ids.clone(),
        &feedback,
        Some(initial.cursor.clone()),
    )
    .await
    .unwrap();
    assert!(!idle.reset);
    assert!(idle.snapshot.devices.is_empty());
    let apply = |feedback: &mut Feedback, time: u64, value: u8| {
        feedback
            .apply(&GatewayRead {
                gateway_id: "simulation".into(),
                transport_epoch: 1,
                received_at_ms: time,
                result: hb_transport::Receive::Frames {
                    transport_epoch: 1,
                    frames: vec![hb_protocol::RawFrame {
                        address: 0x15,
                        command: 0xd1,
                        value,
                    }],
                },
            })
            .unwrap()
    };
    apply(&mut feedback, 1, 30);
    apply(&mut feedback, 2, 40);
    let changed = management_events::read(
        &admission,
        caller,
        generation.clone(),
        ids.clone(),
        &feedback,
        Some(initial.cursor.clone()),
    )
    .await
    .unwrap();
    assert!(!changed.reset);
    assert_eq!(changed.snapshot.devices.len(), 1);
    assert_eq!(
        changed.snapshot.devices[0].state.observed,
        Some(Observation::Level(Percent::new(40).unwrap()))
    );
    for kind in 0..5 {
        let mut invalid = changed.cursor.clone();
        match kind {
            0 => invalid.boot = Uuid::new_v4().to_string(),
            1 => invalid.revision += 1,
            2 => invalid.sequence = u64::MAX,
            3 => invalid.transport_epoch += 1,
            _ => invalid.scope = "different-scope".into(),
        };
        let reset = management_events::read(
            &admission,
            caller,
            generation.clone(),
            ids.clone(),
            &feedback,
            Some(invalid),
        )
        .await
        .unwrap();
        assert!(reset.reset);
        assert_eq!(reset.snapshot.devices.len(), 1);
    }
    let empty = management_events::read(
        &admission,
        caller,
        generation.clone(),
        vec![],
        &feedback,
        Some(changed.cursor.clone()),
    )
    .await
    .unwrap();
    assert!(empty.reset);
    assert!(empty.snapshot.devices.is_empty());
    for time in 3..300 {
        apply(&mut feedback, time, (time % 100) as u8);
    }
    let gap = management_events::read(
        &admission,
        caller,
        generation.clone(),
        ids.clone(),
        &feedback,
        Some(changed.cursor),
    )
    .await
    .unwrap();
    assert!(gap.reset);
    assert_eq!(gap.snapshot.devices.len(), 1);
    assert!(
        management_events::read(&admission, Uuid::new_v4(), generation, ids, &feedback, None)
            .await
            .is_err()
    );
    admission.shutdown().await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}
