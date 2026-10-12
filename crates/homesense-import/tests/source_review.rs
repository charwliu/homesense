use homesense_import::{
    Device,
    source_review::{self, FadeOptions},
};

fn code(id: &str, kind: &str, device: &str, payload: &str, children: &str) -> String {
    format!(
        "<codeitem><id>{id}</id><device>{device}</device><type>{kind}</type><enabled>True</enabled><display>PRIVATE-DISPLAY</display><cmdcond>{payload}</cmdcond><subitems>{children}</subitems></codeitem>"
    )
}

fn event(id: u8, body: &str) -> String {
    format!("<event><deviceid>10</deviceid><eventid>{id}</eventid>{body}</event>")
}

fn project(events: &str) -> String {
    format!(
        "<project><systemitems><item><id>77</id><subitems><item><id>78</id></item></subitems></item></systemitems><event_mgr>{events}</event_mgr></project>"
    )
}

fn devices(kind: &str) -> Vec<Device> {
    vec![Device {
        control4_id: "77".into(),
        stable_id: "simulation:device".into(),
        name: "PRIVATE-NAME".into(),
        room: None,
        kind: kind.into(),
        address: 1,
        channel: Some(1),
        enabled: false,
        verification: "pending",
    }]
}

fn condition(name: &str) -> String {
    format!(
        "<deviceconditional owneridtype=\"\" owneriditem=\"-1\"><name>{name}</name></deviceconditional>"
    )
}

fn command(name: &str, params: &str) -> String {
    format!(
        "<devicecommand owneridtype=\"\" owneriditem=\"-1\"><command>{name}</command><params>{params}</params></devicecommand>"
    )
}

#[test]
fn straight_led_source_requests_match_production_ast_order_and_preserve_pending_gates() {
    let request = |on, key| {
        code(
            &format!("{key}"),
            "1",
            "78",
            &command(
                if on { "Turn On" } else { "Turn Off" },
                &format!(
                    "<param><name>LED Number</name><value type=\"INTEGER\"><static>{key}</static></value></param>"
                ),
            ),
            "",
        )
    };
    let xml = project(&event(
        1,
        &code("0", "1", "0", "", &(request(true, 2) + &request(false, 6))),
    ));
    let proofs = source_review::direct_led_requests(&xml, &devices("keypad")).unwrap();
    assert_eq!(proofs.len(), 1);
    assert_eq!(
        proofs[0].source_request_sequence,
        vec![
            source_review::LedRequest {
                device: "simulation:device:key:2".into(),
                on: true
            },
            source_review::LedRequest {
                device: "simulation:device:key:6".into(),
                on: false
            }
        ]
    );
    assert!(proofs[0].ordered_ast_request_equivalent);
    assert!(!proofs[0].semantic_approved);
    assert!(!proofs[0].runtime_rule_enabled);
    assert!(proofs[0].pending_gates.contains(&"trigger semantics"));
    assert!(proofs[0].pending_gates.contains(&"driver LED behavior"));
}

#[test]
fn source_led_subset_does_not_promote_disabled_branches_conditions_or_scene_calls() {
    let led = code(
        "1",
        "1",
        "78",
        &command(
            "Turn On",
            "<param><name>LED Number</name><value type=\"INTEGER\"><static>1</static></value></param>",
        ),
        "",
    );
    for body in [
        led.replace("True", "False"),
        code("2", "2", "78", &condition("IF_STOPPED"), &led),
        code("3", "1", "78", &command("ACTIVATE_SCENE", ""), ""),
    ] {
        let xml = project(&event(1, &code("0", "1", "0", "", &body)));
        assert!(
            source_review::direct_led_requests(&xml, &devices("keypad"))
                .unwrap()
                .is_empty()
        );
    }
}

#[test]
fn led_source_proof_rejects_extra_fields_dynamic_values_and_wrong_profiles() {
    let led = code(
        "1",
        "1",
        "78",
        &command(
            "Turn Off",
            "<param><name>LED Number</name><value type=\"INTEGER\"><static>3</static></value></param>",
        ),
        "",
    );
    for body in [
        led.replace("</codeitem>", "<opaque>unreviewed</opaque></codeitem>"),
        led.replace("<static>3</static>", "<variable>3</variable>"),
        led.replace(
            "owneriditem=\"-1\"",
            "owneriditem=\"-1\" opaque=\"unreviewed\"",
        ),
        led.replace("INTEGER", "BOOLEAN"),
    ] {
        let xml = project(&event(1, &code("0", "1", "0", "", &body)));
        assert!(
            source_review::direct_led_requests(&xml, &devices("keypad"))
                .unwrap()
                .is_empty()
        );
    }
    let xml = project(&event(1, &code("0", "1", "0", "", &led)));
    assert!(
        source_review::direct_led_requests(&xml, &devices("switch"))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn ledger_request_proof_accepts_source_string_selectors_and_rejects_mixed_containers() {
    let led = code(
        "1",
        "1",
        "78",
        &command(
            "Turn On",
            "<param><name>LED Number</name><value type=\"STRING\"><static>3</static></value></param>",
        ),
        "",
    );
    let root = code("0", "1", "0", "", &led);
    assert_eq!(
        source_review::direct_led_requests(&project(&event(1, &root)), &devices("keypad"))
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        source_review::direct_led_requests(&project(&event(1, &led)), &devices("keypad"))
            .unwrap()
            .len(),
        1
    );
    for source in [
        root.replace("<subitems>", "<subitems opaque=\"unreviewed\">"),
        root.replace("<cmdcond>", "<cmdcond opaque=\"unreviewed\">"),
        root.replace("<subitems>", "<subitems>unmodeled-text"),
        root.replace("<cmdcond>", "<cmdcond>unmodeled-text"),
        root.replace("<params>", "<params>unmodeled-text"),
        root.replace(
            "<static>3</static>",
            "<static opaque=\"unreviewed\">3</static>",
        ),
    ] {
        assert!(
            source_review::direct_led_requests(&project(&event(1, &source)), &devices("keypad"))
                .unwrap()
                .is_empty()
        );
    }
}

#[test]
fn strict_source_proof_rejects_each_isolated_root_or_leaf_container_boundary() {
    let leaf = code(
        "1",
        "1",
        "78",
        &command(
            "Turn Off",
            "<param><name>LED Number</name><value type=\"INTEGER\"><static>3</static></value></param>",
        ),
        "",
    );
    let xml = project(&event(1, &code("0", "1", "0", "", &leaf)));
    assert_eq!(
        source_review::direct_led_requests(&xml, &devices("keypad"))
            .unwrap()
            .len(),
        1
    );
    for input in [
        xml.replacen(
            "<cmdcond></cmdcond><subitems>",
            "<cmdcond></cmdcond><subitems opaque=\"unmodeled\">",
            1,
        ),
        xml.replace(
            "<cmdcond><devicecommand",
            "<cmdcond opaque=\"unmodeled\"><devicecommand",
        ),
        xml.replacen(
            "<cmdcond></cmdcond><subitems>",
            "<cmdcond></cmdcond><subitems>unmodeled",
            1,
        ),
        xml.replace(
            "<cmdcond><devicecommand",
            "<cmdcond>unmodeled<devicecommand",
        ),
    ] {
        assert!(
            source_review::direct_led_requests(&input, &devices("keypad"))
                .unwrap()
                .is_empty()
        );
    }
}

#[test]
fn only_exact_empty_root_is_disposed_and_every_event_is_retained() {
    let empty = code("0", "1", "0", " \n", " \n");
    let disabled = code("9", "1", "78", &command("UNKNOWN", ""), "").replace("True", "False");
    let comment = code("9", "5", "0", "", "");
    let variants = [
        empty.clone(),
        empty.replace("True", "False"),
        code("0", "1", "0", "", &disabled),
        code("0", "1", "0", "", &comment),
        code("9", "1", "0", "", ""),
        code("0", "1", "0", "SECRET", ""),
        code("0", "1", "0", "", "<unknown/>"),
        empty.replace("</codeitem>", "<expression/></codeitem>"),
        empty.replace(
            "<display>PRIVATE-DISPLAY</display>",
            "<display><unknown/></display>",
        ),
        empty.replace("</codeitem>", "<enabled>True</enabled></codeitem>"),
    ];
    let xml = project(
        &variants
            .iter()
            .enumerate()
            .map(|(i, v)| event(i as u8, v))
            .collect::<String>(),
    );
    let audit = source_review::project_xml(&xml, &[], None).unwrap();
    assert_eq!(audit.events.len(), variants.len());
    assert!(audit.events[0].source_empty_noop);
    assert!(audit.events[0].standard.syntax_compiled);
    assert!(audit.events.iter().skip(1).all(|e| !e.source_empty_noop));
    // Disabled unknown code and comments compile to no actions, but aren't empty source.
    assert!(audit.events[2].standard.syntax_compiled);
    assert!(audit.events[3].standard.syntax_compiled);
    assert!(!audit.automatic_execution);
    assert!(audit.events.iter().all(|e| !e.enabled));
    let json = serde_json::to_string(&audit).unwrap();
    assert!(!json.contains("PRIVATE-DISPLAY"));
    assert!(!json.contains("SECRET"));
}

#[test]
fn blocked_conditions_do_not_hide_nested_motor_actions() {
    let open = command(
        "SET_LEVEL_TARGET",
        "<param><name>LEVEL_TARGET_NAME</name><value><static>Open</static></value></param>",
    );
    let first = code(
        "1",
        "2",
        "78",
        &condition("IF_STOPPED"),
        &code("2", "1", "78", &open, ""),
    );
    let second = code(
        "3",
        "2",
        "78",
        &condition("IF_MOVING"),
        &code("4", "1", "78", &command("STOP", ""), ""),
    );
    let audit = source_review::project_xml(
        &project(&event(1, &code("0", "1", "0", "", &(first + &second)))),
        &devices("motor"),
        None,
    )
    .unwrap();
    let e = &audit.events[0];
    assert_eq!(
        e.standard.first_error.as_deref(),
        Some("unsupported device-state condition IF_STOPPED")
    );
    assert_eq!(
        e.typed_sites
            .iter()
            .map(|s| (
                s.operation.as_deref().unwrap(),
                s.blocker.as_deref().unwrap()
            ))
            .collect::<Vec<_>>(),
        vec![
            (
                "IF_STOPPED",
                "unsupported device-state condition IF_STOPPED"
            ),
            (
                "SET_LEVEL_TARGET",
                "unsupported typed command SET_LEVEL_TARGET"
            ),
            ("IF_MOVING", "unsupported device-state condition IF_MOVING"),
            ("STOP", "unsupported typed command STOP")
        ]
    );
    assert!(
        e.typed_sites
            .windows(2)
            .all(|s| s[0].source_offset < s[1].source_offset)
    );
}

#[test]
fn disabled_ancestors_suppress_sites_and_structure_failures_remain_visible() {
    let unknown = code("2", "1", "78", &command("UNKNOWN", ""), "");
    let disabled = code("1", "2", "78", &condition("UNKNOWN"), &unknown)
        .replace("<enabled>True</enabled>", "<enabled>False</enabled>");
    let root = code("0", "1", "0", "", &disabled);
    let malformed = code(
        "3",
        "1",
        "78",
        &command("ON", ""),
        &code("4", "5", "0", "", ""),
    );
    let audit = source_review::project_xml(
        &project(&(event(1, &root) + &event(2, &malformed))),
        &devices("switch"),
        None,
    )
    .unwrap();
    assert!(audit.events[0].typed_sites.is_empty());
    assert!(!audit.events[0].source_empty_noop);
    assert!(audit.events[0].standard.syntax_compiled);
    assert_eq!(audit.events[1].typed_sites.len(), 1);
    assert!(audit.events[1].typed_sites[0].blocker.is_none());
    assert_eq!(
        audit.events[1].standard.first_error.as_deref(),
        Some("action with nested executable children")
    );
}

#[test]
fn opt_in_fade_capability_is_separate_disabled_and_retains_standard_blocker() {
    let fade = command(
        "RAMP_TO_LEVEL",
        "<param><name>TIME</name><value type=\"INTEGER\"><static>1000</static></value></param><param><name>LEVEL</name><value type=\"INTEGER\"><static>0</static></value></param>",
    );
    let xml = project(&event(1, &code("1", "1", "78", &fade, "")));
    let audit = source_review::project_xml(
        &xml,
        &devices("dimmer"),
        Some(FadeOptions {
            interval_ms: 100,
            lateness_ms: 100,
        }),
    )
    .unwrap();
    let e = &audit.events[0];
    assert_eq!(
        e.standard.first_error.as_deref(),
        Some("unsupported typed command RAMP_TO_LEVEL")
    );
    assert!(e.typed_sites[0].blocker.is_some());
    let opt = e.with_fades.as_ref().unwrap();
    assert!(opt.syntax_compiled);
    assert_eq!(opt.disabled_timed_definitions, 1);
    assert!(!e.enabled);
    assert_eq!(
        e.typed_sites[0]
            .opt_in_fade
            .as_ref()
            .unwrap()
            .disabled_timed_definitions,
        1
    );
    let bad = source_review::project_xml(
        &xml.replace("<static>1000</static>", "<variable>1000</variable>"),
        &devices("dimmer"),
        Some(FadeOptions {
            interval_ms: 100,
            lateness_ms: 100,
        }),
    )
    .unwrap();
    assert!(!bad.events[0].with_fades.as_ref().unwrap().syntax_compiled);
    assert_eq!(
        bad.events[0]
            .with_fades
            .as_ref()
            .unwrap()
            .disabled_timed_definitions,
        0
    );
}

#[test]
fn isolated_fade_audit_is_not_hidden_by_a_preceding_blocked_condition() {
    let fade = command(
        "RAMP_TO_LEVEL",
        "<param><name>TIME</name><value type=\"INTEGER\"><static>1000</static></value></param><param><name>LEVEL</name><value type=\"INTEGER\"><static>0</static></value></param>",
    );
    let xml = project(&event(
        1,
        &code(
            "1",
            "2",
            "78",
            &condition("UNKNOWN"),
            &code("2", "1", "78", &fade, ""),
        ),
    ));
    let audit = source_review::project_xml(
        &xml,
        &devices("dimmer"),
        Some(FadeOptions {
            interval_ms: 100,
            lateness_ms: 100,
        }),
    )
    .unwrap();
    let e = &audit.events[0];
    assert!(!e.with_fades.as_ref().unwrap().syntax_compiled);
    assert_eq!(e.typed_sites.len(), 2);
    assert!(
        e.typed_sites[1]
            .opt_in_fade
            .as_ref()
            .unwrap()
            .syntax_compiled
    );
    assert_eq!(
        e.typed_sites[1]
            .opt_in_fade
            .as_ref()
            .unwrap()
            .disabled_timed_definitions,
        1
    );
}

#[test]
fn all_compound_condition_operands_are_individually_audited() {
    let mut c = code("1", "2", "78", &condition("LOAD_ON"), "");
    c = c.replace(
        "</codeitem>",
        &(format!(
            "<expression>{}{}</expression></codeitem>",
            code("2", "6", "0", "", "").replace("PRIVATE-DISPLAY", "AND"),
            code("3", "2", "78", &condition("UNKNOWN"), "")
        )),
    );
    let audit =
        source_review::project_xml(&project(&event(1, &c)), &devices("switch"), None).unwrap();
    assert_eq!(audit.events[0].typed_sites.len(), 2);
    assert!(audit.events[0].typed_sites[0].blocker.is_none());
    assert_eq!(
        audit.events[0].typed_sites[1].operation.as_deref(),
        Some("UNKNOWN")
    );
    assert!(!audit.events[0].standard.syntax_compiled);
}

#[test]
fn duplicate_manager_and_missing_event_identity_are_rejected() {
    assert!(
        source_review::project_xml("<project><event_mgr/><event_mgr/></project>", &[], None)
            .is_err()
    );
    assert!(
        source_review::project_xml(
            "<project><event_mgr><event><eventid>1</eventid></event></event_mgr></project>",
            &[],
            None
        )
        .is_err()
    );
    assert!(
        source_review::project_xml("<project><event_mgr/></project>", &[], None)
            .unwrap()
            .events
            .is_empty()
    );
}
