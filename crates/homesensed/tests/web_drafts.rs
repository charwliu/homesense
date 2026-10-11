use homesense_core::ExecutionContext;
use homesensed::web_drafts::{Draft, Drafts};
use uuid::Uuid;
#[test]
fn validated_drafts_are_immutable_private_and_survive_restart() {
    let root = std::env::temp_dir().join(format!("homesense-drafts-{}", Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let path = root.join("drafts.sqlite");
    let owner = Uuid::new_v4();
    let mut devices: serde_json::Value =
        serde_json::from_slice(include_bytes!("../../../config/simulation.json")).unwrap();
    devices["revision"] = 2.into();
    let mut draft = Draft {
        id: Uuid::new_v4(),
        expected_revision: 1,
        revision: 2,
        devices,
        automation: serde_json::json!({"schema_version":1,"configuration_revision":2,"variables":[],"rules":[],"timed_scenes":[]}),
    };
    let mut store = Drafts::open(&path).unwrap();
    store
        .save(owner, &draft, ExecutionContext::Simulation)
        .unwrap();
    store
        .save(owner, &draft, ExecutionContext::Simulation)
        .unwrap();
    assert!(
        store
            .save(Uuid::new_v4(), &draft, ExecutionContext::Simulation)
            .is_err()
    );
    assert!(store.get(Uuid::new_v4(), draft.id).unwrap().is_none());
    assert!(store.save(owner, &draft, ExecutionContext::Live).is_err());
    drop(store);
    let mut store = Drafts::open(&path).unwrap();
    assert_eq!(
        store.get(owner, draft.id).unwrap(),
        Some(Draft {
            id: draft.id,
            expected_revision: 1,
            revision: 2,
            devices: draft.devices.clone(),
            automation: draft.automation.clone()
        })
    );
    draft.devices["devices"][0]["name"] = "changed".into();
    assert!(
        store
            .save(owner, &draft, ExecutionContext::Simulation)
            .is_err()
    );
    draft.id = Uuid::new_v4();
    draft.automation["configuration_revision"] = 3.into();
    assert!(
        store
            .save(owner, &draft, ExecutionContext::Simulation)
            .is_err()
    );
    assert!(store.get(owner, draft.id).unwrap().is_none());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o077,
            0
        );
    }
    drop(store);
    std::fs::remove_dir_all(root).unwrap();
}
