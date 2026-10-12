use homesense_import::project_xml;
fn code(id: &str, target: &str, payload: &str, children: &str, enabled: bool) -> String {
    format!(
        "<codeitem><id>{id}</id><device>{target}</device><type>1</type><enabled>{}</enabled><cmdcond>{payload}</cmdcond><subitems>{children}</subitems></codeitem>",
        if enabled { "True" } else { "False" }
    )
}
fn scene(id: &str, scene: &str) -> String {
    code(
        id,
        "3",
        &format!(
            "<devicecommand owneridtype='agent' owneriditem='{scene}'><command>ACTIVATE_SCENE</command><params><param><name>SCENE_ID</name><value><static>{scene}</static></value></param></params></devicecommand>"
        ),
        "",
        true,
    )
}
fn led(id: &str, panel: &str, key: u8, on: bool) -> String {
    code(
        id,
        panel,
        &format!(
            "<devicecommand><command>Turn {}</command><params><param><name>LED Number</name><value><static>{key}</static></value></param></params></devicecommand>",
            if on { "On" } else { "Off" }
        ),
        "",
        true,
    )
}
fn xml(events: &str) -> String {
    let items = ["20", "21"].into_iter().enumerate().map(|(i,id)|format!("<item><id>{id}</id><name>Panel</name><itemdata><config_data_file>GJ-TECH.CN_Keypad.c4z</config_data_file></itemdata><state><![CDATA[<state><property><name>Device ID</name><value>{:02X}</value></property></state>]]></state></item>",i+1)).collect::<String>();
    format!("<project><systemitems>{items}</systemitems><event_mgr>{events}</event_mgr></project>")
}
fn event(id: &str, children: &str) -> String {
    format!(
        "<event><deviceid>20</deviceid><eventid>{id}</eventid>{}</event>",
        code("0", "0", "", children, true)
    )
}
#[test]
fn keeps_multi_panel_candidates_and_entry_differences_disabled() {
    let a = event(
        "1",
        &(scene("1", "7")
            + &led("2", "20", 1, true)
            + &led("3", "21", 3, true)
            + &led("4", "20", 5, false)),
    );
    let b = event("2", &(scene("1", "7") + &led("2", "20", 1, true)));
    let r = project_xml(&xml(&(a + &b))).unwrap().indicators;
    assert_eq!(r.actions.len(), 6);
    assert_eq!(r.candidates.len(), 2);
    assert_eq!(r.candidates[0].leds.len(), 2);
    assert_eq!(r.comparisons[0].union.len(), 2);
    assert_eq!(r.comparisons[0].intersection.len(), 1);
    assert!(r.comparisons[0].differing_entries);
    assert!(r.actions.iter().all(|a| !a.enabled));
    assert!(r.candidates.iter().all(|a| !a.enabled));
    assert!(r.comparisons.iter().all(|a| !a.enabled));
}
#[test]
fn branch_containers_and_disabled_ancestors_never_merge() {
    let then = scene("2", "7") + &led("3", "20", 1, true);
    let otherwise = led("4", "21", 2, true);
    let branch = code(
        "1",
        "0",
        "",
        &format!("<then>{then}</then><else>{otherwise}</else>"),
        true,
    );
    let disabled = code(
        "5",
        "0",
        "",
        &(scene("6", "9") + &led("7", "21", 3, true)),
        false,
    );
    let r = project_xml(&xml(&event("1", &(branch + &disabled))))
        .unwrap()
        .indicators;
    assert_eq!(r.candidates.len(), 1);
    assert_eq!(r.candidates[0].leds.len(), 1);
    assert!(
        r.candidates[0]
            .scope
            .path
            .iter()
            .any(|p| p.starts_with("then@"))
    );
    assert!(
        r.actions
            .iter()
            .filter(|a| a.code == "6" || a.code == "7")
            .all(|a| !a.source_enabled)
    );
}
#[test]
fn dynamic_and_owner_mismatch_are_visible_not_bindings() {
    let dynamic = led("2", "20", 1, true).replace("<static>1</static>", "<variable>99</variable>");
    let mismatch = scene("3", "9").replace("owneriditem='9'", "owneriditem='10'");
    let r = project_xml(&xml(&event("1", &(scene("1", "7") + &dynamic + &mismatch))))
        .unwrap()
        .indicators;
    assert_eq!(
        r.actions.iter().filter(|a| a.diagnostic.is_some()).count(),
        2
    );
    assert!(r.candidates[0].leds.is_empty());
    assert_eq!(r.comparisons[0].union.len(), 0);
    assert!(r.actions.iter().all(|a| a.payload_sha256.len() == 64));
}
#[test]
fn multiple_scenes_or_conflicting_led_writes_remain_ambiguous() {
    let a = event(
        "1",
        &(scene("1", "7") + &scene("2", "8") + &led("3", "20", 1, true)),
    );
    let b = event(
        "2",
        &(scene("1", "9") + &led("2", "20", 1, true) + &led("3", "20", 1, false)),
    );
    let r = project_xml(&xml(&(a + &b))).unwrap().indicators;
    assert_eq!(r.candidates.len(), 3);
    assert_eq!(
        r.candidates
            .iter()
            .filter(|c| c.disposition == "ambiguous_multiple_scenes_in_scope")
            .count(),
        2
    );
    assert_eq!(
        r.candidates
            .iter()
            .filter(|c| c.disposition == "ambiguous_led_writes_in_scope")
            .count(),
        1
    );
    assert!(r.comparisons.is_empty());
}

#[test]
fn duplicate_wrapper_names_and_condition_evidence_remain_distinct() {
    let children = format!(
        "<then>{}</then><then>{}</then>",
        scene("2", "7") + &led("3", "20", 1, true),
        led("4", "21", 2, true)
    );
    let branch = code("1", "0", "<deviceconditional owneridtype='variable' owneriditem='99' name='='><param name='value' type='int'>1</param></deviceconditional>", &children, true)
        .replacen("<type>1</type>", "<type>2</type>", 1)
        .replace("<cmdcond>", "<display>PRIVATE-DISPLAY</display><cmdcond>");
    let r = project_xml(&xml(&event("1", &branch))).unwrap().indicators;
    assert_eq!(r.candidates.len(), 1);
    assert_eq!(r.candidates[0].leds.len(), 1);
    assert_eq!(r.candidates[0].scope.condition_hashes.len(), 1);
    assert_eq!(r.candidates[0].scope.condition_hashes[0].len(), 64);
    assert!(
        !serde_json::to_string(&r)
            .unwrap()
            .contains("PRIVATE-DISPLAY")
    );
}
