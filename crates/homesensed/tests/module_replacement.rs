use homesense_core::ExecutionContext;
use homesensed::{
    module_replace::{Edit, prepare},
    web_drafts::{Draft, Drafts},
};
use serde_json::{Value, json};
use uuid::Uuid;
fn legacy() -> Draft {
    Draft {
        id: Uuid::new_v4(),
        expected_revision: 1,
        revision: 2,
        devices: json!({"schema_version":1,"revision":2,"rooms":[{"id":"lab","name":"Synthetic lab"}],"gateways":[{"id":"bus","endpoint":"127.0.0.1:50007","modules":[
 {"address":"03","profile":"r8i8_din_pro_v2_7_7","verification":{"status":"simulated"}},
 {"address":"15","profile":"ad8_din_rs485_v2_2_1","verification":{"status":"simulated"}},
 {"address":"41","profile":"ts6_86_rs485_v2_6_5_6key","verification":{"status":"simulated"},"led_mode":"manual"},
 {"address":"22","profile":"c4_din_pro_v2_7_7","verification":{"status":"simulated"}}]}],"devices":[
 {"id":"supply","name":"Synthetic supply","gateway":"bus","address":"03","channel":1,"enabled":true},
 {"id":"unused-switch","name":"Unused switch","gateway":"bus","address":"03","channel":8,"enabled":false},
 {"id":"main","name":"Main fixture","room":"lab","gateway":"bus","address":"15","channel":1,"enabled":true,"supply_relay":"supply","default_on_level":50},
 {"id":"key1","name":"Synthetic key one","gateway":"bus","address":"41","channel":1,"enabled":true},
 {"id":"key6","name":"Synthetic key six","gateway":"bus","address":"41","channel":6,"enabled":false},
 {"id":"motor","name":"Synthetic curtain","gateway":"bus","address":"22","channel":2,"enabled":true,"motor_calibration":{"open_direction":"up","stop_verified":true,"evidence":"synthetic only","verified_at_unix_ms":1}}]}),
        automation: json!({"schema_version":1,"configuration_revision":2,"variables":[],"rules":[],"timed_scenes":[{"id":"light-scene","enabled":true,"review_evidence":"synthetic","max_lateness_ms":1000,"steps":[{"source":"synthetic source","offset_ms":0,"device":"main","command":{"kind":"level","value":30,"ramp":false}}]}]}),
    }
}
fn original() -> Draft {
    let mut draft = legacy();
    draft.devices["schema_version"] = 2.into();
    let pairs = [
        ("03", "module-supply"),
        ("15", "module-dimmer"),
        ("41", "module-panel"),
        ("22", "module-curtain"),
    ];
    for module in draft.devices["gateways"][0]["modules"]
        .as_array_mut()
        .unwrap()
    {
        module["id"] = pairs
            .iter()
            .find(|(address, _)| module["address"] == *address)
            .unwrap()
            .1
            .into();
    }
    for device in draft.devices["devices"].as_array_mut().unwrap() {
        let id = pairs
            .iter()
            .find(|(address, _)| device["address"] == *address)
            .unwrap()
            .1;
        let object = device.as_object_mut().unwrap();
        object.remove("gateway");
        object.remove("address");
        object.insert("module_id".into(), id.into());
        if object.remove("supply_relay").is_some() {
            object.insert(
                "supply".into(),
                json!({"module_id":"module-supply","channel":1}),
            );
        }
    }
    draft
}
fn edit(address: &str, profile: &str) -> Edit {
    serde_json::from_value(json!({"id":Uuid::new_v4(),"expected_revision":"1","revision":"3","module_id":match address{"03"=>"module-supply","15"=>"module-dimmer","41"=>"module-panel","22"=>"module-curtain",_=>"unknown"},"new_address":"61","profile":profile,"verification":{"status":"simulated"}})).unwrap()
}
fn fresh_original(draft: &Draft) -> Draft {
    Draft {
        id: draft.id,
        expected_revision: draft.expected_revision,
        revision: draft.revision,
        devices: draft.devices.clone(),
        automation: draft.automation.clone(),
    }
}
#[test]
fn replacement_changes_every_binding_atomically_and_preserves_ids_metadata_and_source_refs() {
    for (address, profile, affected) in [
        ("03", "r8i8_din_pro_v2_7_7", vec!["supply", "unused-switch"]),
        ("41", "ts6_86_rs485_v2_6_5_6key", vec!["key1", "key6"]),
    ] {
        let source = original();
        let before = source.devices.clone();
        let before_rules = source.automation.clone();
        let (candidate, impact) =
            prepare(source, edit(address, profile), ExecutionContext::Simulation).unwrap();
        assert!(!impact.active_configuration_changed);
        assert!(!impact.hardware_address_changed);
        assert_eq!(impact.devices, affected);
        for (old, new) in before["devices"]
            .as_array()
            .unwrap()
            .iter()
            .zip(candidate.devices["devices"].as_array().unwrap())
        {
            assert_eq!(old, new);
        }
        let mut expected = before_rules;
        expected["configuration_revision"] = 3.into();
        assert_eq!(candidate.automation, expected);
        if address == "03" {
            assert_eq!(impact.related_logical_lights, vec!["main"]);
            assert_eq!(impact.scenes, vec!["light-scene"]);
        }
        assert_eq!(
            candidate.devices["devices"][5]["motor_calibration"]["evidence"],
            "synthetic only"
        );
    }
}
#[test]
fn motor_module_replacement_clears_old_address_calibration_and_requires_review() {
    let (candidate, impact) = prepare(
        original(),
        edit("22", "c4_din_pro_v2_7_7"),
        ExecutionContext::Simulation,
    )
    .unwrap();
    assert_eq!(impact.recalibration_required, vec!["motor"]);
    assert!(
        candidate.devices["devices"][5]
            .get("motor_calibration")
            .is_none()
    );
    let config = homesense_config::Configuration::parse(
        &serde_json::to_vec(&candidate.devices).unwrap(),
        ExecutionContext::Simulation,
    )
    .unwrap();
    assert!(
        config
            .motor_command(
                &homesense_core::DeviceId::new("motor").unwrap(),
                homesense_core::MotorAction::Open
            )
            .is_err()
    );
    assert!(
        homesensed::configuration_preview::connection_rebuild_required(
            &homesense_config::Configuration::parse(
                &serde_json::to_vec(&original().devices).unwrap(),
                ExecutionContext::Simulation
            )
            .unwrap(),
            &config
        )
    );
}
#[test]
fn collisions_broadcast_missing_models_capacity_and_stale_source_are_rejected() {
    for case in 0..8 {
        let mut request = edit("15", "ad8_din_rs485_v2_2_1");
        match case {
            0 => request.new_address = "03".into(),
            1 => request.new_address = "00".into(),
            2 => request.new_address = "061".into(),
            3 => request.module_id = "missing".into(),
            4 => request.profile = "c4_din_pro_v2_7_7".into(),
            5 => request.profile = "gt_e4_rl_v3_2_2_2".into(),
            6 => request.expected_revision = "2".into(),
            7 => {
                request.verification = serde_json::from_value(json!({"status":"pending"})).unwrap()
            }
            _ => unreachable!(),
        };
        assert!(
            prepare(original(), request, ExecutionContext::Simulation).is_err(),
            "case {case}"
        );
    }
}
#[test]
fn replacement_draft_is_private_immutable_and_insert_failure_preserves_source() {
    let path = std::env::temp_dir().join(format!("module-replace-{}.sqlite", Uuid::new_v4()));
    let mut store = Drafts::open(&path).unwrap();
    let owner = Uuid::new_v4();
    let source = original();
    store
        .save(owner, &source, ExecutionContext::Simulation)
        .unwrap();
    assert!(
        store
            .replace_module(
                Uuid::new_v4(),
                source.id,
                edit("41", "ts6_86_rs485_v2_6_5_6key"),
                ExecutionContext::Simulation
            )
            .is_err()
    );
    let sql = rusqlite::Connection::open(&path).unwrap();
    sql.execute_batch("CREATE TRIGGER fail_new BEFORE INSERT ON configuration_draft BEGIN SELECT RAISE(ABORT,'synthetic'); END;").unwrap();
    let edit = edit("41", "ts6_86_rs485_v2_6_5_6key");
    let id = edit.id;
    assert!(
        store
            .replace_module(owner, source.id, edit, ExecutionContext::Simulation)
            .is_err()
    );
    assert!(store.get(owner, id).unwrap().is_none());
    assert_eq!(store.get(owner, source.id).unwrap().unwrap(), source);
    sql.execute_batch("DROP TRIGGER fail_new;").unwrap();
    let impact = store
        .replace_module(
            owner,
            source.id,
            crate::edit("41", "ts6_86_rs485_v2_6_5_6key"),
            ExecutionContext::Simulation,
        )
        .unwrap();
    assert_eq!(store.get(owner, source.id).unwrap().unwrap(), source);
    assert!(store.get(owner, impact.draft).unwrap().is_some());
    drop(sql);
    drop(store);
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}
#[test]
fn verification_timestamps_preserve_u64_values_without_json_float_rounding() {
    let mut input:Value=serde_json::to_value(json!({"id":Uuid::new_v4(),"expected_revision":"1","revision":"3","module_id":"module-dimmer","new_address":"61","profile":"ad8_din_rs485_v2_2_1","verification":{"status":"verified","evidence":"synthetic profile approval","verified_at_unix_ms":"9007199254740993"}})).unwrap();
    let (candidate, _) = prepare(
        fresh_original(&original()),
        serde_json::from_value(input.clone()).unwrap(),
        ExecutionContext::Simulation,
    )
    .unwrap();
    assert_eq!(
        candidate.devices["gateways"][0]["modules"][1]["verification"]["verified_at_unix_ms"]
            .as_u64(),
        Some(9007199254740993)
    );
    for time in ["0", "01", "18446744073709551616"] {
        input["id"] = Uuid::new_v4().to_string().into();
        input["verification"]["verified_at_unix_ms"] = time.into();
        assert!(
            prepare(
                original(),
                serde_json::from_value(input.clone()).unwrap(),
                ExecutionContext::Simulation
            )
            .is_err()
        );
    }
}

#[test]
fn legacy_migration_persists_independent_module_ids_and_preserves_physical_plan() {
    let source = legacy();
    let before = source.devices.clone();
    let (candidate, impact) = homesensed::module_migration::prepare(
        source,
        homesensed::module_migration::Edit {
            id: Uuid::new_v4(),
            expected_revision: "1".into(),
            revision: "3".into(),
        },
        ExecutionContext::Simulation,
    )
    .unwrap();
    assert_eq!(candidate.devices["schema_version"], 2);
    assert_eq!(impact.modules.len(), 4);
    let old = homesense_config::Configuration::parse(
        &serde_json::to_vec(&before).unwrap(),
        ExecutionContext::Simulation,
    )
    .unwrap();
    let config = homesense_config::Configuration::parse(
        &serde_json::to_vec(&candidate.devices).unwrap(),
        ExecutionContext::Simulation,
    )
    .unwrap();
    assert_eq!(config.module_registry().len(), 4);
    for (id, device) in old.devices() {
        let current = &config.devices()[id];
        assert_eq!(device.gateway, current.gateway);
        assert_eq!(device.address, current.address);
        assert_eq!(device.channel, current.channel);
        assert_eq!(device.supply_relay, current.supply_relay);
        assert!(current.module_id.is_some());
    }
    for device in candidate.devices["devices"].as_array().unwrap() {
        assert!(device.get("gateway").is_none());
        assert!(device.get("address").is_none());
        assert!(device.get("supply_relay").is_none());
    }
    let root = std::env::temp_dir().join(format!("persist-modules-{}.sqlite", Uuid::new_v4()));
    let principal = Uuid::new_v4();
    let mut drafts = Drafts::open(&root).unwrap();
    drafts
        .save(principal, &candidate, ExecutionContext::Simulation)
        .unwrap();
    drop(drafts);
    let drafts = Drafts::open(&root).unwrap();
    let stored = drafts.get(principal, candidate.id).unwrap().unwrap();
    assert_eq!(stored.devices, candidate.devices);
    let reloaded = homesense_config::Configuration::parse(
        &serde_json::to_vec(&stored.devices).unwrap(),
        ExecutionContext::Simulation,
    )
    .unwrap();
    assert_eq!(config.module_registry(), reloaded.module_registry());
    assert!(
        homesensed::module_migration::prepare(
            stored,
            homesensed::module_migration::Edit {
                id: Uuid::new_v4(),
                expected_revision: "1".into(),
                revision: "4".into()
            },
            ExecutionContext::Simulation
        )
        .is_err()
    );
    drop(drafts);
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", root.display()));
    }
}
#[test]
fn schema2_publication_cannot_downgrade_to_legacy_and_discard_module_identity() {
    let root = std::env::temp_dir().join(format!("modules-publication-{}.sqlite", Uuid::new_v4()));
    let mut store = homesense_storage::Store::open(&root).unwrap();
    let source = original();
    store
        .publish_automation(
            None,
            &serde_json::to_vec(&source.devices).unwrap(),
            &serde_json::to_vec(&source.automation).unwrap(),
            ExecutionContext::Simulation,
            &[],
        )
        .unwrap();
    let mut old = legacy();
    old.devices["revision"] = 3.into();
    old.automation["configuration_revision"] = 3.into();
    assert!(
        store
            .publish_automation(
                Some(2),
                &serde_json::to_vec(&old.devices).unwrap(),
                &serde_json::to_vec(&old.automation).unwrap(),
                ExecutionContext::Simulation,
                &[]
            )
            .is_err()
    );
    assert_eq!(
        store
            .active_bundle(ExecutionContext::Simulation)
            .unwrap()
            .unwrap()
            .configuration
            .module_registry()
            .len(),
        4
    );
    assert_eq!(store.active().unwrap().unwrap().revision, 2);
    drop(store);
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", root.display()));
    }
}

#[test]
fn module_calibration_is_bound_to_exact_stable_module_channel() {
    let path = std::env::temp_dir().join(format!("module-calibration-{}.sqlite", Uuid::new_v4()));
    let owner = Uuid::new_v4();
    let source = original();
    let mut drafts = Drafts::open(&path).unwrap();
    drafts
        .save(owner, &source, ExecutionContext::Simulation)
        .unwrap();
    let make = |module: &str| homesensed::web_drafts::ModuleCalibrationEdit {
        id: Uuid::new_v4(),
        expected_revision: "1".into(),
        revision: "3".into(),
        device: "motor".into(),
        module_id: module.into(),
        channel: 2,
        calibration: Some(homesensed::web_drafts::MotorCalibrationRecord {
            open_direction: "down".into(),
            stop_verified: true,
            evidence: "synthetic calibration only".into(),
            verified_at_unix_ms: "9007199254740993".into(),
        }),
    };
    let bad = make("module-panel");
    let rejected = bad.id;
    assert!(
        drafts
            .edit_module_calibration(owner, source.id, bad, ExecutionContext::Simulation)
            .is_err()
    );
    assert!(drafts.get(owner, rejected).unwrap().is_none());
    let id = drafts
        .edit_module_calibration(
            owner,
            source.id,
            make("module-curtain"),
            ExecutionContext::Simulation,
        )
        .unwrap();
    let result = drafts.get(owner, id).unwrap().unwrap();
    assert_eq!(
        result.devices["devices"][5]["motor_calibration"]["verified_at_unix_ms"].as_u64(),
        Some(9007199254740993)
    );
    assert_eq!(result.devices["devices"][5]["module_id"], "module-curtain");
    assert_eq!(drafts.get(owner, source.id).unwrap().unwrap(), source);
    drop(drafts);
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}
