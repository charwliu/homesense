#[path = "../../test-support/cli.rs"]
mod test_cli;
#[path = "support/reviewed_scene.rs"]
mod fixture;
use fixture::*;
use homesense_config::rule_document::Document;
use homesense_import::reviewed_scenes::*;
use serde_json::json;
fn convert_doc(xml: &str) -> Bundle {
    convert(xml, &config(false), &review(xml)).unwrap()
}
#[test]
fn exact_target_members_not_source_off_or_union_and_every_source_field_retained() {
    let (a, b) = topology();
    let xml = project(&a, &b, "", false);
    let bundle = convert_doc(&xml);
    assert!(
        bundle
            .scenes
            .iter()
            .all(|s| s.status == "converted_disabled")
    );
    assert!(!bundle.automatic_execution && !bundle.enabled);
    let d = &bundle.document;
    let source = d["immediate_scenes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == "control4:scene:7:50")
        .unwrap();
    assert_eq!(source["activate"].as_array().unwrap().len(), 4);
    let off = source["deactivate"].as_array().unwrap();
    assert_eq!(off.len(), 36);
    assert_eq!(
        off.iter()
            .map(|s| s["action"]["device"].as_str().unwrap().to_string())
            .collect::<Vec<_>>(),
        (1004..1040)
            .map(|i| format!("control4:{i}"))
            .collect::<Vec<_>>()
    );
    for (i, step) in off.iter().enumerate() {
        assert_eq!(
            step["action"]["command"],
            json!({"kind":"switch","on":false})
        );
        assert!(
            step["source"]
                .as_str()
                .unwrap()
                .contains(&format!("control4:scene:7:11/member:{i}/element:0"))
        );
    }
    let parsed = Document::parse(&serde_json::to_vec(d).unwrap(), &config(false)).unwrap();
    assert!(parsed.rules.is_empty());
    assert_eq!(parsed.disabled_immediate_scenes.len(), 3);
    assert!(parsed.linked_rules_with_deferred().unwrap().is_empty());
    for s in &bundle.scenes {
        assert_eq!(
            s.source.fields.len(),
            s.review.as_ref().unwrap().fields.len()
        );
        assert!(s.source.raw_xml.contains("<is_active>True</is_active>"));
    }
}
#[test]
fn qualified_calls_keep_distinct_agents_even_same_id_and_display_with_outer_condition_and_disabled_chain()
 {
    let (a, b) = topology();
    let event = guarded_event(&format!(
        "{}{}{}",
        call(3, 7, 50, false, true),
        call(4, 8, 50, true, true),
        call(5, 99, 50, true, false)
    ));
    let xml = project(&a, &b, &event, false);
    let cfg = config(false);
    let review = review(&xml);
    let bundle = convert(&xml, &cfg, &review).unwrap();
    let linked = link_event(&xml, &event, &cfg, &review, &bundle).unwrap();
    let actions = &linked["actions"];
    assert_eq!(actions[1]["action"]["kind"], "if");
    assert_eq!(actions[1]["action"]["condition"]["left"]["id"], "7");
    let calls = actions[1]["action"]["then_actions"].as_array().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0]["action"]["id"], "control4:scene:7:50");
    assert_eq!(calls[1]["action"]["id"], "control4:scene:8:50");
    assert_eq!(calls[0]["source"], "device:900/event:2/code:2/code:3");
    assert_eq!(linked["source_event_xml"], event);
    assert_eq!(
        linked["scene_call_links"]["device:900/event:2/code:2/code:5"],
        "control4:scene:99:50"
    );
    let mut doc = bundle.document.clone();
    doc["variables"] = json!([{"id":"7","kind":"integer","lifetime":"persistent"}]);
    doc["rules"] = json!([document_rule(actions.clone(), true)]);
    let parsed = Document::parse(&serde_json::to_vec(&doc).unwrap(), &cfg).unwrap();
    assert!(
        parsed.linked_rules_with_deferred().is_err(),
        "enabling caller alone must not enable imported scene"
    );
    for s in doc["immediate_scenes"].as_array_mut().unwrap() {
        s["enabled"] = true.into();
    }
    let parsed = Document::parse(&serde_json::to_vec(&doc).unwrap(), &cfg).unwrap();
    let linked = parsed.linked_rules_with_deferred().unwrap();
    let homesense_core::rule::Action::If { then_actions, .. } = &linked[0].actions[1].action else {
        panic!()
    };
    assert_eq!(then_actions.len(), 37);
    assert!(
        then_actions[0].source.starts_with(
            "device:900/event:2/code:2/code:3 -> control4:scene:7:11/member:0/element:0"
        )
    );
    assert!(
        link_event(
            &xml,
            &event.replace("<device>7</device>", "<device>8</device>"),
            &cfg,
            &review,
            &bundle
        )
        .is_err()
    );
}
#[test]
fn missing_unknown_stale_and_unsupported_policy_hold_entire_scene_and_dependents() {
    let (a, b) = topology();
    let xml = project(&a, &b, "", false);
    let cfg = config(false);
    let check = |r: &Review| {
        let bundle = convert(&xml, &cfg, r).unwrap();
        assert_eq!(
            bundle.scenes.iter().filter(|s| s.status == "held").count(),
            2
        );
        assert_eq!(
            bundle.document["immediate_scenes"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    };
    let mut r = review(&xml);
    r.scenes.retain(|s| s.scene != "control4:scene:7:11");
    check(&r);
    for mutation in 0..5 {
        let mut r = review(&xml);
        let target = r
            .scenes
            .iter_mut()
            .find(|s| s.scene == "control4:scene:7:11")
            .unwrap();
        match mutation {
            0 => {
                target.fields.remove("AdvScene/hold_rate_up[0]");
            }
            1 => target.source_sha256 = "wrong".into(),
            2 => {
                target
                    .fields
                    .get_mut("AdvScene/is_active[0]")
                    .unwrap()
                    .expected = "False".into()
            }
            3 => {
                target
                    .fields
                    .get_mut("AdvScene/track_mode[0]")
                    .unwrap()
                    .disposition = Disposition::Level
            }
            _ => target.nonoperative_tracking_tokens.clear(),
        };
        check(&r);
    }
    let mut r = review(&xml);
    r.bindings.retain(|b| b.source_device != "2004");
    check(&r);
    let mut r = review(&xml);
    r.scenes
        .iter_mut()
        .find(|s| s.scene == "control4:scene:7:50")
        .unwrap()
        .deactivation = Deactivation::Unavailable {
        raw_value: "11".into(),
    };
    assert_eq!(
        convert(&xml, &cfg, &r)
            .unwrap()
            .scenes
            .iter()
            .filter(|s| s.status == "held")
            .count(),
        1
    );
    for (from, to) in [
        (
            "<hold_rate_up>0</hold_rate_up>",
            "<hold_rate_up>5000</hold_rate_up>",
        ),
        ("<full_on>False</full_on>", "<full_on>True</full_on>"),
        ("<track_lvl>100</track_lvl>", "<unknown>100</unknown>"),
        (
            "<name>Same synthetic display</name>",
            "<name color=\"x\">Same synthetic display</name>",
        ),
    ] {
        let altered = xml.replace(from, to);
        let bundle = convert_doc(&altered);
        assert!(bundle.scenes.iter().all(|s| s.status == "held"));
        assert!(
            bundle.document["immediate_scenes"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
    let mut r = review(&xml);
    r.project_xml_sha256 = "stale".into();
    assert!(convert(&xml, &cfg, &r).is_err());
    let mut r = review(&xml);
    r.configuration_revision = 2;
    assert!(convert(&xml, &cfg, &r).is_err());
    let mut r = review(&xml);
    r.bindings[0].logical_device = "control4:1001".into();
    assert!(convert(&xml, &cfg, &r).is_err());
}
#[test]
fn explicit_timing_units_anchors_and_backend_without_deferred_deactivation_approximation() {
    let a = scene(1, "unavailable", &[(2000, 20, 1, 1), (2000, 40, 2, 1)]);
    let xml = project(&a, "", "", true);
    let cfg = config(true);
    let mut r = review(&xml);
    r.scenes[0].delay_unit = Unit::Seconds;
    r.scenes[0].rate_duration_unit = Unit::Seconds;
    r.scenes[0].delay_anchor = Anchor::PreviousElementInMember;
    r.scenes[0].rate_backend = RateBackend::SoftwareFade {
        interval_ms: 100,
        lateness_ms: 1000,
    };
    // Separate members restart the cumulative per-member anchor.
    let bundle = convert(&xml, &cfg, &r).unwrap();
    let steps = bundle.document["timed_scenes"][0]["steps"]
        .as_array()
        .unwrap();
    assert_eq!(steps[0]["offset_ms"], 1000);
    assert_eq!(steps[1]["offset_ms"], 2000);
    assert_eq!(steps[0]["fade"]["duration_ms"], 1000);
    r.scenes[0].rate_backend = RateBackend::UnsupportedHardwareRamp;
    assert_eq!(convert(&xml, &cfg, &r).unwrap().scenes[0].status, "held");
    let a = format!(
        "{}{}",
        scene(1, "2", &[(2000, 20, 1, 0)]),
        scene(2, "unavailable", &[(2000, 0, 0, 0)])
    );
    let xml = project(&a, "", "", true);
    let mut r = review(&xml);
    r.scenes[0].delay_unit = Unit::Seconds;
    let bundle = convert(&xml, &cfg, &r).unwrap();
    assert_eq!(
        bundle.scenes.iter().filter(|s| s.status == "held").count(),
        1
    );
    assert!(
        bundle
            .scenes
            .iter()
            .find(|s| s.source.scene_id == "1")
            .unwrap()
            .blockers[0]
            .contains("deferred deactivation")
    );
    let mut wrong = bundle;
    wrong.document["rules"] = json!([{"unexpected":true}]);
    assert!(link_event(&xml, "<event/>", &cfg, &r, &wrong).is_err());
}
#[test]
fn no_contract_is_inventory_only_not_runtime_approval() {
    let (a, b) = topology();
    let xml = project(&a, &b, "", false);
    let mut r = review(&xml);
    r.scenes.clear();
    r.bindings.clear();
    let bundle = convert(&xml, &config(false), &r).unwrap();
    assert!(bundle.scenes.iter().all(|s| s.status == "held"));
    assert_eq!(bundle.document["immediate_scenes"], json!([]));
    assert_eq!(bundle.document["timed_scenes"], json!([]));
}
#[test]
fn multi_element_ties_cumulative_anchor_and_backend_capability_are_explicit() {
    let a = scene(1, "no-target", &[(2000, 20, 1, 1)]).replace(
        "</all_elements>",
        "<element><delay>2</delay><rate>1</rate><level>40</level></element></all_elements>",
    );
    let xml = project(&a, "", "", true);
    let cfg = config(true);
    let mut r = review(&xml);
    r.scenes[0].delay_unit = Unit::Seconds;
    r.scenes[0].rate_duration_unit = Unit::Seconds;
    r.scenes[0].rate_backend = RateBackend::SoftwareFade {
        interval_ms: 100,
        lateness_ms: 1000,
    };
    r.scenes[0].delay_anchor = Anchor::PreviousElementInMember;
    let cumulative = convert(&xml, &cfg, &r).unwrap();
    assert_eq!(
        cumulative.document["timed_scenes"][0]["steps"][1]["offset_ms"],
        3000
    );
    r.scenes[0].delay_anchor = Anchor::SceneStart;
    let absolute = convert(&xml, &cfg, &r).unwrap();
    assert_eq!(
        absolute.document["timed_scenes"][0]["steps"][1]["offset_ms"],
        2000
    );
    r.scenes[0].delay_unit = Unit::Minutes;
    let minute = convert(&xml, &cfg, &r).unwrap();
    assert_eq!(
        minute.document["timed_scenes"][0]["steps"][1]["offset_ms"],
        120000
    );
    let cfg_bytes = String::from_utf8(config_bytes(true))
        .unwrap()
        .replace("gt_d8_rl_v2_2_6_2", "ad8_din_rs485_v2_2_1");
    let ad8 = homesense_config::Configuration::parse(
        cfg_bytes.as_bytes(),
        homesense_core::ExecutionContext::Simulation,
    )
    .unwrap();
    assert_eq!(convert(&xml, &ad8, &r).unwrap().scenes[0].status, "held");
    let immediate = project(&scene(1, "unresolved", &[(2000, 50, 0, 0)]), "", "", true);
    assert_eq!(
        convert(&immediate, &ad8, &review(&immediate))
            .unwrap()
            .scenes[0]
            .status,
        "held"
    );
    let rgb = project(
        &scene(1, "unresolved", &[(2000, 0, 0, 0)]).replace(
            "<toggle_id>",
            "<top_active_color>#0000ff</top_active_color><toggle_id>",
        ),
        "",
        "",
        true,
    );
    let held = convert(&rgb, &cfg, &review(&rgb)).unwrap();
    assert_eq!(held.scenes[0].status, "held");
    assert!(held.scenes[0].blockers[0].contains("top_active_color"));
}
#[test]
fn missing_actual_call_target_agent_holds_whole_event_without_fallback() {
    let (a, b) = topology();
    let event = event(&format!(
        "{}{}",
        call(3, 7, 50, true, true),
        call(4, 99, 50, true, true)
    ));
    let xml = project(&a, &b, &event, false);
    let cfg = config(false);
    let r = review(&xml);
    let bundle = convert(&xml, &cfg, &r).unwrap();
    let error = link_event(&xml, &event, &cfg, &r, &bundle).unwrap_err();
    assert!(error.contains("control4:scene:99:50"));
    let event = event.replace("<static>50</static>", "<static>11</static>");
    let xml = project(&a, &b, &event, false);
    let r = review(&xml);
    let bundle = convert(&xml, &cfg, &r).unwrap();
    assert!(
        link_event(&xml, &event, &cfg, &r, &bundle).is_err(),
        "owneriditem mismatch cannot qualify a different SCENE_ID"
    );
}
#[test]
fn offline_cli_exports_complete_disabled_bundle_and_verified_qualified_event() {
    let (a, b) = topology();
    let event = guarded_event(&call(3, 7, 50, false, true));
    let xml = project(&a, &b, &event, false);
    let review = review(&xml);
    let dir = std::env::temp_dir().join(format!("reviewed-scene-cli-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (name, bytes) in [
        ("project.xml", xml.as_bytes().to_vec()),
        ("config.json", config_bytes(false)),
        ("review.json", serde_json::to_vec_pretty(&review).unwrap()),
        ("event.xml", event.as_bytes().to_vec()),
    ] {
        std::fs::write(dir.join(name), bytes).unwrap();
    }
    let cli = test_cli::binary(env!("CARGO_BIN_EXE_reviewed-scene-draft"));
    let args = [
        dir.join("project.xml"),
        dir.join("config.json"),
        dir.join("review.json"),
    ];
    let output = std::process::Command::new(&cli)
        .args(&args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let bundle: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(bundle["enabled"], false);
    assert_eq!(bundle["document"]["rules"], json!([]));
    assert_eq!(bundle["scenes"].as_array().unwrap().len(), 3);
    let linked = std::process::Command::new(&cli)
        .args(&args)
        .arg(dir.join("event.xml"))
        .output()
        .unwrap();
    assert!(
        linked.status.success(),
        "{}",
        String::from_utf8_lossy(&linked.stderr)
    );
    let draft: serde_json::Value = serde_json::from_slice(&linked.stdout).unwrap();
    assert_eq!(
        draft["actions"][1]["action"]["then_actions"][0]["action"]["id"],
        "control4:scene:7:50"
    );
    assert_eq!(draft["enabled"], false);
    if let Ok(artifacts) = std::env::var("REVIEWED_SCENE_ARTIFACT_ROOT") {
        let destination = std::path::Path::new(&artifacts).join("synthetic");
        std::fs::create_dir_all(&destination).unwrap();
        for name in ["project.xml", "config.json", "review.json", "event.xml"] {
            std::fs::copy(dir.join(name), destination.join(name)).unwrap();
        }
        std::fs::write(destination.join("bundle.json"), &output.stdout).unwrap();
        std::fs::write(destination.join("event-draft.json"), &linked.stdout).unwrap();
    }
    let mut stale = serde_json::to_value(review).unwrap();
    stale["project_xml_sha256"] = "stale".into();
    std::fs::write(dir.join("review.json"), serde_json::to_vec(&stale).unwrap()).unwrap();
    let rejected = std::process::Command::new(&cli)
        .args(&args)
        .output()
        .unwrap();
    assert!(!rejected.status.success());
    assert!(rejected.stdout.is_empty());
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn mixed_owned_supply_and_dimmer_aliases_hold_entire_scene_without_dropping_members() {
    let a = format!(
        "{}{}{}",
        scene(100, "none", &[(2000, 40, 0, 0), (2001, 100, 0, 0)]),
        scene(101, "100", &[(2002, 0, 0, 0)]),
        scene(102, "none", &[(2000, 40, 0, 0)])
    );
    let xml = project(&a, "", "", true);
    let cfg = combined_config();
    assert!(
        convert(&xml, &cfg, &review(&xml))
            .unwrap_err()
            .contains("binding behavior review"),
        "combined planning must require explicit binding behavior approval"
    );
    let bundle = convert(&xml, &cfg, &combined_review(&xml)).unwrap();
    let held = bundle
        .scenes
        .iter()
        .find(|s| s.source.scene_id == "100")
        .unwrap();
    assert_eq!(held.status, "held");
    assert_eq!(
        held.source.inventory["members"].as_array().unwrap().len(),
        2
    );
    assert!(held.blockers[0].contains("owned-supply/alias conflict"));
    assert!(held.blockers[0].contains("control4:1001"));
    assert!(held.blockers[0].contains("control4:1000"));
    assert_eq!(
        bundle
            .scenes
            .iter()
            .find(|s| s.source.scene_id == "101")
            .unwrap()
            .status,
        "held"
    );
    if let Ok(artifacts) = std::env::var("REVIEWED_SCENE_ARTIFACT_ROOT") {
        let destination = std::path::Path::new(&artifacts).join("synthetic");
        std::fs::create_dir_all(&destination).unwrap();
        std::fs::write(
            destination.join("mixed-owned-supply-draft.json"),
            serde_json::to_vec_pretty(&bundle).unwrap(),
        )
        .unwrap();
        std::fs::write(
            destination.join("combined-config.json"),
            combined_config_bytes(),
        )
        .unwrap();
    }
    let valid = bundle.document["immediate_scenes"].as_array().unwrap();
    assert_eq!(valid.len(), 1);
    assert_eq!(valid[0]["id"], "control4:scene:7:102");
    assert_eq!(valid[0]["activate"].as_array().unwrap().len(), 1);
    assert_eq!(valid[0]["activate"][0]["action"]["device"], "control4:1000");
    assert_eq!(
        valid[0]["activate"][0]["action"]["command"],
        json!({"kind":"level","value":40,"ramp":false})
    );
}
#[test]
fn reviewed_tracking_contract_retains_raw_codes_and_requires_every_explicit_member_policy() {
    use homesense_config::scene_tracking::{Aggregation, PendingPolicy, Predicate, UnknownPolicy};
    let a = scene(50100, "none", &[(2000, 100, 10, 0), (2001, 0, 10, 0)])
        .replacen("synthetic-no-tracking", "0", 1)
        .replace("synthetic-no-tracking", "4");
    let xml = project(&a, "", "", true);
    let cfg = config(true);
    let mut r = review(&xml);
    r.scenes[0].tracking = Some(TrackingContract {
        aggregation: Aggregation::All,
        unknown_policy: UnknownPolicy::RequireAllKnown,
        pending_policy: PendingPolicy::BlockScene,
        control_policy:
            homesense_config::scene_tracking::ControlPolicy::SeparateActionAndReadOnlyActivity,
        activity_name: "Synthetic activity".into(),
        max_age_ms: 5000,
        review_evidence: "explicit synthetic map; no original raw code interpretation inferred"
            .into(),
        members: vec![
            TrackingMember {
                source_member_index: 0,
                predicate: Predicate::LevelRange {
                    minimum: 20,
                    maximum: 60,
                },
                review_evidence: "explicit powered brightness predicate".into(),
            },
            TrackingMember {
                source_member_index: 1,
                predicate: Predicate::Ignore {
                    reason: "explicit tracking exclusion only; control member retained".into(),
                },
                review_evidence: "explicit ignored member".into(),
            },
        ],
    });
    for (path, field) in &mut r.scenes[0].fields {
        if path.ends_with("track_mode[0]") || path.ends_with("track_lvl[0]") {
            field.disposition = Disposition::Tracking;
        }
    }
    let bundle = convert(&xml, &cfg, &r).unwrap();
    assert_eq!(bundle.scenes[0].status, "converted_disabled");
    assert_eq!(bundle.document["scene_tracking"][0]["enabled"], false);
    assert_eq!(
        bundle.document["scene_tracking"][0]["members"][0]["predicate"]["minimum"],
        20
    );
    assert_eq!(
        bundle.document["timed_scenes"][0]["steps"]
            .as_array()
            .unwrap()
            .len(),
        2,
        "ignore never deletes source control actions"
    );
    assert_eq!(
        bundle.scenes[0].source.inventory["metadata"]["track_mode"],
        "0"
    );
    assert_eq!(
        bundle.scenes[0].source.inventory["members"][0]["metadata"]["track_mode"],
        "4"
    );
    assert_eq!(
        bundle.document["scene_metadata"][0]["name"],
        "Same synthetic display"
    );
    let mut missing = r.clone();
    missing.scenes[0].tracking.as_mut().unwrap().members.pop();
    assert_eq!(
        convert(&xml, &cfg, &missing).unwrap().scenes[0].status,
        "held"
    );
    let mut reordered = r.clone();
    reordered.scenes[0]
        .tracking
        .as_mut()
        .unwrap()
        .members
        .reverse();
    assert_eq!(
        convert(&xml, &cfg, &reordered).unwrap().scenes[0].status,
        "held"
    );
    let mut absent = r.clone();
    absent.scenes[0].tracking = None;
    assert_eq!(
        convert(&xml, &cfg, &absent).unwrap().scenes[0].status,
        "held"
    );
    let mut raw = r.clone();
    raw.scenes[0]
        .fields
        .get_mut("AdvScene/track_mode[0]")
        .unwrap()
        .expected = "4".into();
    assert_eq!(convert(&xml, &cfg, &raw).unwrap().scenes[0].status, "held");
    let altered = xml.replace(
        "<hold_rate_up>0</hold_rate_up>",
        "<hold_rate_up>5000</hold_rate_up>",
    );
    let mut unrelated = review(&altered);
    unrelated.scenes[0].tracking = r.scenes[0].tracking.clone();
    for (path, field) in &mut unrelated.scenes[0].fields {
        if path.ends_with("track_mode[0]") || path.ends_with("track_lvl[0]") {
            field.disposition = Disposition::Tracking;
        }
    }
    assert_eq!(
        convert(&altered, &cfg, &unrelated).unwrap().scenes[0].status,
        "held",
        "tracking approval never discards hold semantics"
    );
}
#[test]
fn explicit_deactivation_runtime_is_required_and_target_timing_never_approximated() {
 let source=(2000..2004).map(|id|(id,100,3,0)).collect::<Vec<_>>();let target=(2004..2040).map(|id|(id,0,2,0)).collect::<Vec<_>>();let xml=project(&format!("{}{}",scene(50,"11",&source),scene(11,"none",&target)),"","",false);let cfg=config(false);let mut r=review(&xml);
 assert_eq!(convert(&xml,&cfg,&r).unwrap().scenes.iter().find(|s|s.source.scene_id=="50").unwrap().status,"held");
 for c in &mut r.scenes{c.delay_unit=Unit::Seconds;}
 r.scenes.iter_mut().find(|c|c.scene=="control4:scene:7:50").unwrap().deactivation_runtime=Some(DeactivationRuntime{active_execution_policy:homesense_config::rule_document::DeactivationPolicy::RequireQuiescentRuntime,review_evidence:"synthetic separately reviewed quiescence scope".into()});
 let b=convert(&xml,&cfg,&r).unwrap();assert!(b.scenes.iter().all(|s|s.status=="converted_disabled"));assert_eq!(b.document["scene_deactivations"][0]["target"],"control4:scene:7:11");assert_eq!(b.document["scene_deactivations"][0]["enabled"],false);
 let target=b.document["timed_scenes"].as_array().unwrap().iter().find(|s|s["id"]=="control4:scene:7:11").unwrap();assert_eq!(target["steps"].as_array().unwrap().len(),36);assert!(target["steps"].as_array().unwrap().iter().all(|s|s["offset_ms"]==2000));
 r.scenes.iter_mut().find(|c|c.scene=="control4:scene:7:11").unwrap().fields.get_mut("AdvScene/all_members[0]/AdvSceneMember[0]/all_elements[0]/element[0]/level[0]").unwrap().expected="99".into();let b=convert(&xml,&cfg,&r).unwrap();assert!(b.scenes.iter().all(|s|s.status=="held"));assert!(b.document["timed_scenes"].as_array().unwrap().is_empty());
}
#[test]
fn target_fade_uses_its_reviewed_backend_and_unsupported_target_holds_dependent_source() {
 let xml=project(&format!("{}{}",scene(50,"11",&[(2001,100,0,0)]),scene(11,"none",&[(2000,30,10,300)])),"","",true);let cfg=config(true);let mut r=review(&xml);
 r.scenes.iter_mut().find(|c|c.scene=="control4:scene:7:50").unwrap().deactivation_runtime=Some(DeactivationRuntime{active_execution_policy:homesense_config::rule_document::DeactivationPolicy::RequireQuiescentRuntime,review_evidence:"synthetic explicit quiescence".into()});r.scenes.iter_mut().find(|c|c.scene=="control4:scene:7:11").unwrap().rate_backend=RateBackend::SoftwareFade{interval_ms:100,lateness_ms:200};let b=convert(&xml,&cfg,&r).unwrap();assert!(b.scenes.iter().all(|s|s.status=="converted_disabled"));assert_eq!(b.document["immediate_scenes"][0]["activate"][0]["action"]["device"],"control4:1001");assert_eq!(b.document["timed_scenes"][0]["steps"][0]["device"],"control4:1000");assert_eq!(b.document["timed_scenes"][0]["steps"][0]["fade"],json!({"duration_ms":300,"interval_ms":100,"lateness_ms":200}));assert_eq!(b.document["timed_scenes"][0]["steps"][0]["offset_ms"],10);
 r.scenes.iter_mut().find(|c|c.scene=="control4:scene:7:11").unwrap().rate_backend=RateBackend::UnsupportedHardwareRamp;let b=convert(&xml,&cfg,&r).unwrap();assert!(b.scenes.iter().all(|s|s.status=="held"));assert!(b.document["immediate_scenes"].as_array().unwrap().is_empty());assert!(b.document["timed_scenes"].as_array().unwrap().is_empty());
}
