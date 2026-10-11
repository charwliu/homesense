use homesense_import::project_xml;
fn fixture() -> String {
    r#"<project><systemitems><item><id>1</id><type>8</type><name>书房</name><subitems><item><id>210</id><name>灯</name><itemdata><config_data_file>GJ-TECH.CN_Switch_V2.c4z</config_data_file></itemdata><state><![CDATA[<state><properties><property><name>Device ID</name><value>03</value></property><property><name>Channel</name><value>4</value></property><property><name>Activation Key</name><value>DO-NOT-EXPORT</value></property></properties><AdvScene><name>Test scene</name><scene_id>7</scene_id><all_members><AdvSceneMember><device_id>210</device_id></AdvSceneMember></all_members></AdvScene></state>]]></state></item></subitems></item></systemitems><event_mgr><event><deviceid>210</deviceid><eventid>1</eventid><codeitem><id>9</id><device>999</device><type>1</type><enabled>True</enabled><display>DO-NOT-EXPORT</display></codeitem></event></event_mgr></project>"#.into()
}
#[test]
fn imports_disabled_mapping_and_reports_unresolved_code_without_copying_credentials() {
    let report = project_xml(&fixture()).unwrap();
    assert_eq!(report.item_count, 2);
    let device = &report.devices[0];
    assert_eq!(device.stable_id, "control4:210");
    assert_eq!(device.room.as_deref(), Some("书房"));
    assert_eq!((device.address, device.channel), (3, Some(4)));
    assert!(!device.enabled);
    assert_eq!(device.verification, "pending");
    assert_eq!(
        (
            report.scene_count,
            report.event_count,
            report.unsupported_code_nodes
        ),
        (1, 1, 1)
    );
    assert_eq!(report.references.iter().filter(|r| !r.resolved).count(), 1);
    assert!(
        !serde_json::to_string(&report)
            .unwrap()
            .contains("DO-NOT-EXPORT")
    );
}
#[test]
fn rejects_ambiguous_ids_properties_and_invalid_bus_values() {
    for xml in [
        fixture().replace("<id>210</id>", "<id>1</id>"),
        fixture().replace("<value>03</value>", "<value>00</value>"),
        fixture().replace("<value>4</value>", "<value>17</value>"),
        fixture().replace("<value>03</value>", "<value>003</value>"),
        fixture().replace(
            "<properties>",
            "<properties><property><name>Device ID</name><value>04</value></property>",
        ),
    ] {
        assert!(project_xml(&xml).is_err());
    }
    assert!(
        project_xml("<!DOCTYPE project [<!ENTITY x 'x'>]><project><systemitems/></project>")
            .is_err()
    );
}
#[test]
fn malformed_embedded_state_is_explicitly_reported() {
    let xml = fixture().replace("</properties>", "</wrong>");
    let report = project_xml(&xml).unwrap();
    assert!(report.devices.is_empty());
    assert_eq!(report.diagnostics.len(), 1);
}

fn archive(name: &str, xml: &[u8], method: zip::CompressionMethod) -> Vec<u8> {
    use std::io::{Cursor, Write};
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    writer
        .start_file(
            name,
            zip::write::SimpleFileOptions::default().compression_method(method),
        )
        .unwrap();
    writer.write_all(xml).unwrap();
    writer.finish().unwrap().into_inner()
}
#[test]
fn c4p_stored_and_deflated_match_xml_and_record_distinct_source_hashes() {
    let xml = fixture();
    let raw = homesense_import::import_bytes(xml.as_bytes()).unwrap();
    for method in [
        zip::CompressionMethod::Stored,
        zip::CompressionMethod::Deflated,
    ] {
        let bytes = archive("project.xml", xml.as_bytes(), method);
        let report = homesense_import::import_bytes(&bytes).unwrap();
        assert_eq!(report.devices[0].stable_id, raw.devices[0].stable_id);
        let source = report.source.unwrap();
        assert_eq!(source.format, "c4p");
        assert_eq!(source.sha256.len(), 64);
        assert_eq!(
            source.project_xml_sha256,
            raw.source.as_ref().unwrap().sha256
        );
        assert_ne!(source.sha256, source.project_xml_sha256);
    }
}
#[test]
fn c4p_rejects_missing_root_truncation_and_corrupted_member() {
    let xml = fixture();
    assert!(
        homesense_import::import_bytes(&archive(
            "../project.xml",
            xml.as_bytes(),
            zip::CompressionMethod::Stored
        ))
        .is_err()
    );
    let mut bytes = archive(
        "project.xml",
        xml.as_bytes(),
        zip::CompressionMethod::Stored,
    );
    assert!(homesense_import::import_bytes(&bytes[..bytes.len() / 2]).is_err());
    let offset = bytes
        .windows(xml.len())
        .position(|w| w == xml.as_bytes())
        .unwrap();
    bytes[offset + 2] ^= 1;
    assert!(homesense_import::import_bytes(&bytes).is_err());
}

#[test]
fn explicit_profile_plan_produces_valid_disabled_configuration_and_coverage() {
    let report = homesense_import::import_bytes(fixture().as_bytes()).unwrap();
    let plan = serde_json::json!({"source_sha256":report.source.as_ref().unwrap().sha256,"revision":1,"gateway_id":"hb","endpoint":"192.168.0.10:50007","modules":[{"address":"03","profile":"gt_c8_rl_v2_8_2_2","basis":"test fixture manufacturer profile; not field verification"}],"control4_ids":["210"]});
    let draft = homesense_import::configuration::generate(
        &report,
        serde_json::from_value(plan.clone()).unwrap(),
    )
    .unwrap();
    assert_eq!(draft.selected_disabled, vec!["210"]);
    assert!(draft.excluded.is_empty());
    assert_eq!(draft.configuration["devices"][0]["enabled"], false);
    assert_eq!(
        draft.configuration["gateways"][0]["modules"][0]["verification"]["status"],
        "pending"
    );
    homesense_config::Configuration::parse(
        &serde_json::to_vec(&draft.configuration).unwrap(),
        homesense_core::ExecutionContext::Live,
    )
    .unwrap();
    for (pointer, value) in [
        ("/source_sha256", serde_json::json!("wrong")),
        ("/modules/0/profile", serde_json::json!("gt_d8_rl_v2_2_6_2")),
        ("/modules/0/basis", serde_json::json!("")),
        ("/control4_ids", serde_json::json!(["210", "210"])),
        ("/modules/0/address", serde_json::json!("04")),
    ] {
        let mut changed = plan.clone();
        *changed.pointer_mut(pointer).unwrap() = value;
        assert!(
            homesense_import::configuration::generate(
                &report,
                serde_json::from_value(changed).unwrap()
            )
            .is_err()
        );
    }
}

#[test]
fn preserves_program_tree_but_only_recognizes_exact_root_container() {
    let xml = fixture().replace("<codeitem><id>9", "<codeitem><id>0</id><device>0</device><type>1</type><enabled>True</enabled><cmdcond/><subitems><codeitem><id>9").replace("</codeitem></event>", "</codeitem></subitems></codeitem></event>");
    let report = project_xml(&xml).unwrap();
    assert_eq!(report.structural_code_nodes, 1);
    assert_eq!(report.unsupported_code_nodes, 1);
    assert!(!report.programs[0].enabled);
    let root = &report.programs[0].nodes[0];
    assert_eq!(root.kind, "sequence_container");
    assert_eq!(root.target, None);
    assert_eq!(root.children[0].target.as_deref(), Some("999"));
    let changed = xml.replace(
        "<cmdcond/>",
        "<cmdcond><devicecommand><value>DO-NOT-EXPORT</value></devicecommand></cmdcond>",
    );
    let report = project_xml(&changed).unwrap();
    assert_eq!(report.structural_code_nodes, 0);
    assert_eq!(report.unsupported_code_nodes, 2);
    assert_eq!(
        report.programs[0].nodes[0]
            .payload_sha256
            .as_ref()
            .unwrap()
            .len(),
        64
    );
    assert!(
        !serde_json::to_string(&report)
            .unwrap()
            .contains("DO-NOT-EXPORT")
    );
    assert!(project_xml(&xml.replace("<id>9</id>", "<id>0</id>")).is_err());
    assert!(
        project_xml(&xml.replace("<enabled>True</enabled>", "<enabled>maybe</enabled>")).is_err()
    );
}

#[test]
fn preserves_code_nested_inside_conditional_expression() {
    let xml = fixture().replace("<display>DO-NOT-EXPORT</display>", "<cmdcond><deviceconditional><expression><codeitem><id>10</id><device>100001</device><type>6</type><enabled>True</enabled><cmdcond/></codeitem></expression></deviceconditional></cmdcond>");
    let report = project_xml(&xml).unwrap();
    assert_eq!(report.unsupported_code_nodes, 2);
    let expression = &report.programs[0].nodes[0].children[0];
    assert_eq!(expression.id, "10");
    assert_eq!(
        expression.source_container,
        vec!["cmdcond", "deviceconditional", "expression"]
    );
}

#[test]
fn scene_steps_preserve_order_timing_tracking_and_unknown_field_disposition() {
    let xml = fixture().replace("<device_id>210</device_id>", "<device_id>210</device_id><track_lvl>100</track_lvl><ignore_ramp>True</ignore_ramp><all_elements><element><delay>10</delay><rate>1000</rate><level>30</level></element><element><delay>20</delay><rate>0</rate><level>0</level></element></all_elements>");
    let report = project_xml(&xml).unwrap();
    let scene = &report.scenes[0];
    assert_eq!(scene.stable_id, "control4:scene:210:7");
    assert!(!scene.enabled);
    assert!(scene.unhandled_fields.is_empty());
    assert_eq!(scene.members[0].metadata["ignore_ramp"], "True");
    let steps = &scene.members[0].elements;
    assert_eq!(steps.len(), 2);
    assert_eq!(
        (steps[0].source_delay, steps[0].source_rate, steps[0].level),
        (10, 1000, 30)
    );
    assert_eq!(
        (steps[1].source_delay, steps[1].source_rate, steps[1].level),
        (20, 0, 0)
    );
    let extra = xml.replace("</AdvScene>", "<unknown>DO-NOT-EXPORT</unknown></AdvScene>");
    let report = project_xml(&extra).unwrap();
    assert_eq!(report.scenes[0].unhandled_fields, vec!["unknown"]);
    assert!(
        !serde_json::to_string(&report)
            .unwrap()
            .contains("DO-NOT-EXPORT")
    );
    assert!(project_xml(&xml.replace("<level>30</level>", "<level>101</level>")).is_err());
    assert!(project_xml(&xml.replace("<delay>10</delay>", "<delay>-1</delay>")).is_err());
    assert!(
        project_xml(&xml.replace("<rate>1000</rate>", "<rate>1</rate><rate>2</rate>")).is_err()
    );
}

#[test]
fn cli_exports_three_disabled_artifacts_and_refuses_overwrite() {
    use std::{fs, process::Command};
    let root = std::env::temp_dir().join(format!(
        "homesense-import-export-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir(&root).unwrap();
    let xml = fixture().replace("<device>999</device>", "<device>210</device>");
    let report = homesense_import::import_bytes(xml.as_bytes()).unwrap();
    let plan = serde_json::json!({"source_sha256":report.source.as_ref().unwrap().sha256,"revision":1,"gateway_id":"fixture","endpoint":"127.0.0.1:50007","modules":[{"address":"03","profile":"r8i8_din_pro_v2_7_7","basis":"offline fixture"}],"control4_ids":["210"]});
    fs::write(root.join("project.xml"), xml).unwrap();
    fs::write(root.join("plan.json"), serde_json::to_vec(&plan).unwrap()).unwrap();
    let run = || {
        Command::new(env!("CARGO_BIN_EXE_homesense-import"))
            .arg(root.join("project.xml"))
            .arg("--config-plan")
            .arg(root.join("plan.json"))
            .arg("--export-dir")
            .arg(root.join("export"))
            .output()
            .unwrap()
    };
    let result = run();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let names = [
        "configuration.json",
        "coverage-report.json",
        "confirmation-checklist.json",
    ];
    let before: Vec<_> = names
        .iter()
        .map(|n| fs::read(root.join("export").join(n)).unwrap())
        .collect();
    let docs: Vec<serde_json::Value> = before
        .iter()
        .map(|b| serde_json::from_slice(b).unwrap())
        .collect();
    assert_eq!(docs[0]["devices"][0]["enabled"], false);
    assert_eq!(docs[1]["source"]["sha256"], plan["source_sha256"]);
    assert_eq!(docs[2]["source"], docs[1]["source"]);
    assert_eq!(docs[2]["selected_disabled"], serde_json::json!(["210"]));
    assert!(
        before
            .iter()
            .all(|b| !String::from_utf8_lossy(b).contains("DO-NOT-EXPORT"))
    );
    assert!(!run().status.success());
    for (name, bytes) in names.iter().zip(before) {
        assert_eq!(fs::read(root.join("export").join(name)).unwrap(), bytes);
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn room_blockers_keep_cross_room_targets_and_unknown_ownership() {
    let mut report = homesense_import::import_bytes(fixture().as_bytes()).unwrap();
    let view = homesense_import::room::blockers(&report, "书房").unwrap();
    assert_eq!(
        view["programs_pending_translation"][0]["nodes"][0]["target"],
        "999"
    );
    assert_eq!(
        view["unresolved_references_review_required"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    report.programs[0].owner = "event:999:1".into();
    let view = homesense_import::room::blockers(&report, "书房").unwrap();
    assert_eq!(
        view["unassigned_programs_review_required"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    report.programs[0].nodes[0].target = Some("210".into());
    let view = homesense_import::room::blockers(&report, "书房").unwrap();
    assert_eq!(
        view["programs_pending_translation"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(homesense_import::room::blockers(&report, "不存在").is_err());
    assert!(!view.to_string().contains("DO-NOT-EXPORT"));
}
