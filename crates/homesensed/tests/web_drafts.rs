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

#[test]
fn archive_releases_capacity_without_recycling_ids() {
    let root=std::env::temp_dir().join(format!("draft-archive-{}",Uuid::new_v4()));std::fs::create_dir(&root).unwrap();let path=root.join("drafts.sqlite");
    let mut store=Drafts::open(&path).unwrap();let owner=Uuid::new_v4();let foreign=Uuid::new_v4();
    let mut devices:serde_json::Value=serde_json::from_slice(include_bytes!("../../../config/simulation.json")).unwrap();devices["revision"]=2.into();
    let mut draft=Draft{id:Uuid::new_v4(),expected_revision:1,revision:2,devices,automation:serde_json::json!({"schema_version":1,"configuration_revision":2,"variables":[],"rules":[]})};
    let first=draft.id;
    for _ in 0..128 {store.save(owner,&draft,ExecutionContext::Simulation).unwrap();draft.id=Uuid::new_v4();}
    assert!(store.save(owner,&draft,ExecutionContext::Simulation).is_err());
    assert!(!store.archive(foreign,first).unwrap());assert_eq!(store.list(owner).unwrap().len(),128);
    assert!(store.archive(owner,first).unwrap());assert!(store.archive(owner,first).unwrap());assert!(store.get(owner,first).unwrap().is_none());
    store.save(owner,&draft,ExecutionContext::Simulation).unwrap();draft.id=first;
    assert!(store.save(owner,&draft,ExecutionContext::Simulation).is_err());drop(store);
    let mut store=Drafts::open(&path).unwrap();assert_eq!(store.list(owner).unwrap().len(),128);assert!(store.get(owner,first).unwrap().is_none());assert!(store.save(owner,&draft,ExecutionContext::Simulation).is_err());
    let db=rusqlite::Connection::open(&path).unwrap();let total:i64=db.query_row("SELECT count(*) FROM configuration_draft",[],|r|r.get(0)).unwrap();assert_eq!(total,129);
    drop(db);drop(store);std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn metadata_edit_derives_draft_preserving_bindings_and_large_numbers() {
    use homesensed::web_drafts::DeviceMetadataEdit;
    let root=std::env::temp_dir().join(format!("metadata-edit-{}",Uuid::new_v4()));std::fs::create_dir(&root).unwrap();let mut store=Drafts::open(&root.join("drafts.sqlite")).unwrap();let owner=Uuid::new_v4();
    let mut devices:serde_json::Value=serde_json::from_slice(include_bytes!("../../../config/simulation.json")).unwrap();devices["revision"]=9007199254740993u64.into();devices["rooms"]=serde_json::json!([{"id":"study","name":"书房"}]);
    let draft=Draft{id:Uuid::new_v4(),expected_revision:1,revision:9007199254740993,devices,automation:serde_json::json!({"schema_version":1,"configuration_revision":9007199254740993u64,"variables":[],"rules":[]})};store.save(owner,&draft,ExecutionContext::Simulation).unwrap();
    let make=|room:Option<&str>|DeviceMetadataEdit{id:Uuid::new_v4(),expected_revision:"1".into(),revision:"9007199254740994".into(),device:"simulation:light".into(),name:"书房灯".into(),room:room.map(str::to_owned)};
    assert!(store.edit_device(Uuid::new_v4(),draft.id,make(Some("study")),ExecutionContext::Simulation).is_err());
    assert!(store.edit_device(owner,draft.id,make(Some("missing")),ExecutionContext::Simulation).is_err());
    let id=store.edit_device(owner,draft.id,make(Some("study")),ExecutionContext::Simulation).unwrap();let new=store.get(owner,id).unwrap().unwrap();
    let mut expected=draft.devices.clone();expected["revision"]=9007199254740994u64.into();expected["devices"][0]["name"]="书房灯".into();expected["devices"][0]["room"]="study".into();assert_eq!(new.devices,expected);
    assert_eq!(store.get(owner,draft.id).unwrap(),Some(draft));assert_eq!(new.automation["configuration_revision"],9007199254740994u64);
    let id2=store.edit_device(owner,id,make(None),ExecutionContext::Simulation).unwrap();assert!(store.get(owner,id2).unwrap().unwrap().devices["devices"][0].get("room").is_none());
    drop(store);std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn room_edits_preserve_membership_and_original_draft() {
    use homesensed::web_drafts::RoomEdit;
    let root=std::env::temp_dir().join(format!("room-edit-{}",Uuid::new_v4()));std::fs::create_dir(&root).unwrap();let mut store=Drafts::open(&root.join("drafts.sqlite")).unwrap();let owner=Uuid::new_v4();
    let mut devices:serde_json::Value=serde_json::from_slice(include_bytes!("../../../config/simulation.json")).unwrap();devices["revision"]=2.into();
    let draft=Draft{id:Uuid::new_v4(),expected_revision:1,revision:2,devices,automation:serde_json::json!({"schema_version":1,"configuration_revision":2,"variables":[],"rules":[]})};store.save(owner,&draft,ExecutionContext::Simulation).unwrap();
    let make=|name:&str|RoomEdit{id:Uuid::new_v4(),expected_revision:"1".into(),revision:"9007199254740993".into(),room:homesense_config::Room{id:"study".into(),name:name.into(),floor:Some("二层".into())},remove:false};
    assert!(store.edit_room(Uuid::new_v4(),draft.id,make("书房"),ExecutionContext::Simulation).is_err());assert!(store.edit_room(owner,draft.id,make(""),ExecutionContext::Simulation).is_err());
    let id=store.edit_room(owner,draft.id,make("书房"),ExecutionContext::Simulation).unwrap();let updated=store.get(owner,id).unwrap().unwrap();assert_eq!(updated.devices["devices"],draft.devices["devices"]);assert_eq!(updated.devices["revision"],9007199254740993u64);
    let next=store.edit_room(owner,id,make("工作室"),ExecutionContext::Simulation).unwrap();let updated=store.get(owner,next).unwrap().unwrap();assert_eq!(updated.devices["rooms"].as_array().unwrap().len(),1);assert_eq!(updated.devices["rooms"][0]["name"],"工作室");assert_eq!(store.get(owner,draft.id).unwrap(),Some(draft));
    let mut remove=make("ignored");remove.remove=true;
    let removed=store.edit_room(owner,next,remove,ExecutionContext::Simulation).unwrap();assert!(store.get(owner,removed).unwrap().unwrap().devices["rooms"].as_array().unwrap().is_empty());
    let mut referenced=store.get(owner,next).unwrap().unwrap();referenced.id=Uuid::new_v4();referenced.devices["devices"][0]["room"]="study".into();store.save(owner,&referenced,ExecutionContext::Simulation).unwrap();
    let mut remove=make("ignored");remove.remove=true;let refused=remove.id;assert!(store.edit_room(owner,referenced.id,remove,ExecutionContext::Simulation).is_err());assert!(store.get(owner,refused).unwrap().is_none());assert_eq!(store.get(owner,referenced.id).unwrap(),Some(referenced));
    drop(store);std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn binding_edits_preserve_source_and_invalidate_changed_motor_calibration() {
    use homesensed::web_drafts::DeviceBindingEdit;
    let root=std::env::temp_dir().join(format!("binding-draft-{}",Uuid::new_v4()));std::fs::create_dir(&root).unwrap();
    let mut store=Drafts::open(&root.join("drafts.sqlite")).unwrap();let principal=Uuid::new_v4();
    let draft=Draft{id:Uuid::new_v4(),expected_revision:1,revision:2,
        devices:serde_json::json!({"schema_version":1,"revision":2,"gateways":[{"id":"test","endpoint":"127.0.0.1:50007","modules":[{"address":"22","profile":"c4_din_pro_v2_7_7","verification":{"status":"simulated"}}]}],"devices":[{"id":"motor","name":"Motor fixture","gateway":"test","address":"22","channel":2,"enabled":true,"motor_calibration":{"open_direction":"up","stop_verified":true,"evidence":"offline fixture","verified_at_unix_ms":1}}]}),
        automation:serde_json::json!({"schema_version":1,"configuration_revision":2,"variables":[],"rules":[],"timed_scenes":[]})};
    store.save(principal,&draft,ExecutionContext::Simulation).unwrap();
    let edit=|channel|DeviceBindingEdit{id:Uuid::new_v4(),expected_revision:"1".into(),revision:"3".into(),device:"motor".into(),gateway:"test".into(),address:"22".into(),channel,enabled:true};
    let same=store.edit_binding(principal,draft.id,edit(2),ExecutionContext::Simulation).unwrap();
    assert!(store.get(principal,same).unwrap().unwrap().devices["devices"][0].get("motor_calibration").is_some());
    let changed=store.edit_binding(principal,draft.id,edit(3),ExecutionContext::Simulation).unwrap();
    let changed=store.get(principal,changed).unwrap().unwrap();
    assert_eq!(changed.devices["devices"][0]["channel"],3);
    assert!(changed.devices["devices"][0].get("motor_calibration").is_none());
    assert_eq!(changed.devices["devices"][0]["name"],"Motor fixture");
    assert_eq!(store.get(principal,draft.id).unwrap().unwrap(),draft);
    let bad=edit(5);let candidate=bad.id;
    assert!(store.edit_binding(principal,draft.id,bad,ExecutionContext::Simulation).is_err());
    assert!(store.get(principal,candidate).unwrap().is_none());
    assert!(store.edit_binding(Uuid::new_v4(),draft.id,edit(3),ExecutionContext::Simulation).is_err());
    let mut bad=edit(3);bad.gateway="unknown".into();assert!(store.edit_binding(principal,draft.id,bad,ExecutionContext::Simulation).is_err());
    let mut disabled=edit(2);disabled.enabled=false;
    let id=store.edit_binding(principal,draft.id,disabled,ExecutionContext::Simulation).unwrap();
    assert_eq!(store.get(principal,id).unwrap().unwrap().devices["devices"][0]["enabled"],false);
    drop(store);std::fs::remove_dir_all(root).unwrap();
}
