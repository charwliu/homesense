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
    let catalog=store.list(owner).unwrap();assert_eq!(catalog.len(),1);assert_eq!(catalog[0].id,draft.id);assert_eq!(catalog[0].revision,2);
    assert!(store.list(Uuid::new_v4()).unwrap().is_empty());
    let other=Uuid::new_v4();
    let foreign=Draft{id:Uuid::new_v4(),expected_revision:1,revision:2,devices:draft.devices.clone(),automation:draft.automation.clone()};
    store.save(other,&foreign,ExecutionContext::Simulation).unwrap();
    assert_eq!(store.list(owner).unwrap(),catalog);assert_eq!(store.list(other).unwrap()[0].id,foreign.id);
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

#[test]
fn publication_preview_distinguishes_names_from_connection_changes() {
    let mut source: serde_json::Value =
        serde_json::from_slice(include_bytes!("../../../config/simulation.json")).unwrap();
    let parse = |value: &serde_json::Value| {
        homesense_config::Configuration::parse(
            &serde_json::to_vec(value).unwrap(),
            ExecutionContext::Simulation,
        )
        .unwrap()
    };
    let old = parse(&source);
    source["revision"] = 2.into();
    source["devices"][0]["name"] = "Renamed".into();
    assert!(!homesensed::configuration_preview::connection_rebuild_required(&old, &parse(&source)));
    source["gateways"][0]["endpoint"] = "127.0.0.1:50008".into();
    assert!(homesensed::configuration_preview::connection_rebuild_required(&old, &parse(&source)));
}
