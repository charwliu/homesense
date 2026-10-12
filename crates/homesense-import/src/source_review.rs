//! Read-only source coverage. Typed-site success is not a semantic approval.
//! Reports contain source identifiers; callers must keep private reports local.
use crate::semantic;
use roxmltree::Node;
use serde::Serialize;

#[derive(Clone, Copy)]
pub struct FadeOptions {
    pub interval_ms: u64,
    pub lateness_ms: u64,
}

#[derive(Debug, Serialize)]
pub struct Compilation {
    pub syntax_compiled: bool,
    pub first_error: Option<String>,
    /// Successful opt-in compilation keeps every generated definition disabled.
    pub disabled_timed_definitions: usize,
}

#[derive(Debug, Serialize)]
pub struct Site {
    pub code_id: String,
    pub source_offset: usize,
    pub kind: &'static str,
    pub operation: Option<String>,
    pub blocker: Option<String>,
    /// Isolated RAMP capability, independent of earlier whole-event blockers.
    pub opt_in_fade: Option<Compilation>,
}

#[derive(Debug, Serialize)]
pub struct Event {
    pub owner: String,
    pub enabled: bool,
    /// Exact empty source structure, not an empty compiler result.
    pub source_empty_noop: bool,
    pub standard: Compilation,
    pub with_fades: Option<Compilation>,
    /// All enabled typed payloads, including descendants of blocked conditions.
    /// Full compilation remains authoritative for container/structural errors.
    pub typed_sites: Vec<Site>,
}

#[derive(Debug, Serialize)]
pub struct Review {
    pub automatic_execution: bool,
    pub syntax_only: bool,
    pub events: Vec<Event>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct LedRequest {
    pub device: String,
    pub on: bool,
}

/// Source request conversion only. Callback meaning, driver behavior and physical
/// bindings remain independent gates; this is never a full event approval.
#[derive(Debug, Serialize)]
pub struct LedRequestProof {
    pub owner: String,
    pub source_request_sequence: Vec<LedRequest>,
    pub ordered_ast_request_equivalent: bool,
    pub semantic_approved: bool,
    pub runtime_rule_enabled: bool,
    pub pending_gates: Vec<&'static str>,
}

/// Independently decode a strict straight-line source subset, then compare the
/// production XML converter's ordered AST. No conditions, disabled branches,
/// scene calls, inferred state or source timing are accepted by this subset.
pub fn direct_led_requests(
    xml: &str,
    devices: &[crate::Device],
) -> Result<Vec<LedRequestProof>, String> {
    use homesense_core::{Command, ModuleKind, rule::Action};
    let document = crate::parse_xml(xml)?;
    let bindings = semantic::candidate_bindings(&document, devices);
    fn white_text(node: Node<'_, '_>) -> bool {
        !node
            .children()
            .any(|n| n.is_text() && n.text().is_some_and(|s| !s.trim().is_empty()))
    }
    fn clean_container(node: Node<'_, '_>) -> bool {
        node.attributes().len() == 0 && white_text(node)
    }
    fn modeled_code(code: Node<'_, '_>) -> bool {
        let mut fields = std::collections::BTreeSet::new();
        clean_container(code)
            && code.children().filter(|n| n.is_element()).all(|n| {
                fields.insert(n.tag_name().name())
                    && matches!(
                        n.tag_name().name(),
                        "id" | "device"
                            | "type"
                            | "enabled"
                            | "display"
                            | "cmdcond"
                            | "subitems"
                            | "creator"
                            | "creatorstate"
                    )
                    && (if matches!(n.tag_name().name(), "cmdcond" | "subitems") {
                        clean_container(n)
                    } else {
                        n.attributes().len() == 0 && !n.children().any(|c| c.is_element())
                    })
            })
    }
    fn decode(
        nodes: &[Node<'_, '_>],
        bindings: &std::collections::BTreeMap<String, semantic::Binding>,
        expected: &mut Vec<LedRequest>,
        depth: usize,
    ) -> Result<(), String> {
        if depth > 64 {
            return Err("LED sequence depth limit".into());
        }
        for &code in nodes {
            if !code.has_tag_name("codeitem")
                || scalar(code, "enabled")? != "True"
                || scalar(code, "type")? != "1"
            {
                return Err("not a straight-line enabled LED sequence".into());
            }
            if !modeled_code(code) {
                return Err("unmodeled LED code fields".into());
            }
            if code.children().any(|n| n.has_tag_name("expression")) {
                return Err("LED source expression is outside this subset".into());
            }
            let payload = unique(code, "cmdcond")?;
            let children = unique(code, "subitems")?;
            if scalar(code, "id")? == "0" && scalar(code, "device")? == "0" && empty(payload) {
                decode(
                    &children
                        .children()
                        .filter(|n| n.is_element())
                        .collect::<Vec<_>>(),
                    bindings,
                    expected,
                    depth + 1,
                )?;
                continue;
            }
            if !empty(children) || payload.children().filter(|n| n.is_element()).count() != 1 {
                return Err("unexpected LED sequence container".into());
            }
            let command = unique(payload, "devicecommand")?;
            if command.attributes().len() != 2
                || command.attribute("owneridtype") != Some("")
                || command.attribute("owneriditem") != Some("-1")
                || command.children().filter(|n| n.is_element()).count() != 2
                || !white_text(command)
            {
                return Err("unreviewed LED request owner/shape".into());
            }
            let on = match scalar(command, "command")? {
                "Turn On" => true,
                "Turn Off" => false,
                _ => return Err("not a direct LED request".into()),
            };
            if unique(command, "command")?.attributes().len() != 0 {
                return Err("unmodeled LED operation attributes".into());
            }
            let params = unique(command, "params")?;
            if !clean_container(params) || params.children().filter(|n| n.is_element()).count() != 1
            {
                return Err("LED parameter count".into());
            }
            let param = unique(params, "param")?;
            if !clean_container(param)
                || param.children().filter(|n| n.is_element()).count() != 2
                || scalar(param, "name")? != "LED Number"
            {
                return Err("LED parameter shape".into());
            }
            let value = unique(param, "value")?;
            if value.attributes().len() != 1
                || !matches!(value.attribute("type"), Some("INTEGER" | "STRING"))
                || value.children().filter(|n| n.is_element()).count() != 1
                || !white_text(value)
            {
                return Err("dynamic LED parameter".into());
            }
            if unique(value, "static")?.attributes().len() != 0
                || unique(param, "name")?.attributes().len() != 0
            {
                return Err("unmodeled scalar attributes".into());
            }
            let selector = scalar(value, "static")?;
            let key: u8 = selector.parse().map_err(|_| "LED key is not an integer")?;
            if !(1..=6).contains(&key) || selector != key.to_string() {
                return Err("LED key is outside the candidate six-key profile".into());
            }
            let binding = bindings
                .get(scalar(code, "device")?)
                .ok_or("missing LED source binding")?;
            if binding.kind != ModuleKind::Keypad {
                return Err("not a keypad source binding".into());
            }
            expected.push(LedRequest {
                device: format!("{}:key:{key}", binding.device.as_str()),
                on,
            });
        }
        Ok(())
    }
    let mut proofs = Vec::new();
    let mut owners = std::collections::BTreeSet::new();
    for event in document
        .root_element()
        .children()
        .filter(|n| n.has_tag_name("event_mgr"))
        .flat_map(|n| n.children().filter(|n| n.has_tag_name("event")))
    {
        let owner = format!(
            "event:{}:{}",
            scalar(event, "deviceid")?,
            scalar(event, "eventid")?
        );
        if !owners.insert(owner.clone()) {
            return Err("duplicate event in LED review".into());
        }
        let roots = event
            .children()
            .filter(|n| n.has_tag_name("codeitem"))
            .collect::<Vec<_>>();
        if roots.is_empty()
            || event
                .descendants()
                .filter(|n| n.has_tag_name("codeitem"))
                .count()
                > 4096
        {
            continue;
        }
        if !clean_container(event)
            || event
                .children()
                .filter(|n| n.is_element())
                .any(|n| !matches!(n.tag_name().name(), "deviceid" | "eventid" | "codeitem"))
        {
            continue;
        }
        let mut expected = Vec::new();
        if decode(&roots, &bindings, &mut expected, 0).is_err() || expected.is_empty() {
            continue;
        }
        let actions = semantic::compile_event(&xml[event.range()], &bindings)?;
        let actual = actions
            .into_iter()
            .map(|n| match n.action {
                Action::Command {
                    device,
                    command: Command::PanelLed(on),
                } => Ok(LedRequest {
                    device: device.as_str().into(),
                    on,
                }),
                _ => Err("LED source converter produced non-LED action".to_string()),
            })
            .collect::<Result<Vec<_>, _>>()?;
        if expected != actual {
            return Err("LED source/AST request sequence differs".into());
        }
        proofs.push(LedRequestProof {
            owner,
            source_request_sequence: expected,
            ordered_ast_request_equivalent: true,
            semantic_approved: false,
            runtime_rule_enabled: false,
            pending_gates: vec![
                "trigger semantics",
                "driver LED behavior",
                "physical binding and six-key profile",
                "ownership policy",
            ],
        });
    }
    Ok(proofs)
}

fn unique<'a, 'i>(node: Node<'a, 'i>, tag: &str) -> Result<Node<'a, 'i>, String> {
    let mut children = node.children().filter(|n| n.has_tag_name(tag));
    let value = children.next().ok_or_else(|| format!("missing {tag}"))?;
    if children.next().is_some() {
        return Err(format!("duplicate {tag}"));
    }
    Ok(value)
}

fn scalar<'a, 'i>(node: Node<'a, 'i>, tag: &str) -> Result<&'a str, String> {
    let child = unique(node, tag)?;
    if child.children().any(|n| n.is_element()) {
        return Err(format!("non-scalar {tag}"));
    }
    child
        .text()
        .map(str::trim)
        .ok_or_else(|| format!("empty {tag}"))
}

fn empty(node: Node<'_, '_>) -> bool {
    node.attributes().len() == 0
        && !node.children().any(|n| {
            n.is_element() || n.is_text() && n.text().is_some_and(|t| !t.trim().is_empty())
        })
}

fn source_empty(event: Node<'_, '_>) -> bool {
    if event
        .children()
        .filter(|n| n.is_element())
        .any(|n| !matches!(n.tag_name().name(), "deviceid" | "eventid" | "codeitem"))
    {
        return false;
    }
    let Ok(code) = unique(event, "codeitem") else {
        return false;
    };
    let mut seen = std::collections::BTreeSet::new();
    if code.attributes().len() != 0
        || code.children().filter(|n| n.is_element()).any(|n| {
            !seen.insert(n.tag_name().name())
                || !matches!(
                    n.tag_name().name(),
                    "id" | "device"
                        | "type"
                        | "enabled"
                        | "cmdcond"
                        | "subitems"
                        | "display"
                        | "creator"
                        | "creatorstate"
                )
                || !matches!(n.tag_name().name(), "cmdcond" | "subitems")
                    && (n.attributes().len() != 0 || n.children().any(|c| c.is_element()))
        })
    {
        return false;
    }
    [
        ("id", "0"),
        ("device", "0"),
        ("type", "1"),
        ("enabled", "True"),
    ]
    .into_iter()
    .all(|(k, v)| scalar(code, k).is_ok_and(|s| s == v))
        && unique(code, "cmdcond").is_ok_and(empty)
        && unique(code, "subitems").is_ok_and(empty)
}

fn compilation(result: Result<semantic::CompiledEvent, String>) -> Compilation {
    match result {
        Ok(value) if value.timed_scenes.values().any(|d| d["enabled"] != false) => Compilation {
            syntax_compiled: false,
            first_error: Some("audit refuses an enabled timed definition".into()),
            disabled_timed_definitions: 0,
        },
        Ok(value) => Compilation {
            syntax_compiled: true,
            first_error: None,
            disabled_timed_definitions: value.timed_scenes.len(),
        },
        Err(error) => Compilation {
            syntax_compiled: false,
            first_error: Some(error),
            disabled_timed_definitions: 0,
        },
    }
}

/// Audit complete event inventory using the importer's existing conservative
/// typed conversion and source-hierarchy candidate bindings. No source payload,
/// display text, fingerprint, runtime configuration or enabled output is emitted.
pub fn project_xml(
    xml: &str,
    devices: &[crate::Device],
    fades: Option<FadeOptions>,
) -> Result<Review, String> {
    let document = crate::parse_xml(xml)?;
    let bindings = semantic::candidate_bindings(&document, devices);
    let mut managers = document
        .root_element()
        .children()
        .filter(|n| n.has_tag_name("event_mgr"));
    let manager = managers.next();
    if managers.next().is_some() {
        return Err("duplicate event_mgr".into());
    }
    let mut events = Vec::new();
    for event in manager
        .into_iter()
        .flat_map(|m| m.children().filter(|n| n.has_tag_name("event")))
    {
        if event
            .descendants()
            .filter(|n| n.has_tag_name("codeitem"))
            .count()
            > 4096
        {
            return Err("source review event exceeds 4096 code nodes".into());
        }
        let owner = format!(
            "event:{}:{}",
            scalar(event, "deviceid")?,
            scalar(event, "eventid")?
        );
        let fragment = &xml[event.range()];
        let standard = compilation(semantic::compile_event(fragment, &bindings).map(|actions| {
            semantic::CompiledEvent {
                actions,
                timed_scenes: Default::default(),
            }
        }));
        let with_fades = fades.map(|options| {
            compilation(semantic::compile_event_with_fades(
                fragment,
                &bindings,
                options.interval_ms,
                options.lateness_ms,
            ))
        });
        let mut typed_sites = Vec::new();
        for code in event.descendants().filter(|n| n.has_tag_name("codeitem")) {
            if !code
                .ancestors()
                .take_while(|n| *n != event)
                .filter(|n| n.has_tag_name("codeitem"))
                .all(|n| scalar(n, "enabled").is_ok_and(|s| s == "True"))
            {
                continue;
            }
            let kind = scalar(code, "type").unwrap_or("");
            if !matches!(kind, "1" | "2") {
                continue;
            }
            if source_empty(event) && code.parent() == Some(event) {
                continue;
            }
            // A nonempty structural root isn't a typed command. Full compilation
            // checks its payload and nested sequence; its descendants are audited.
            if kind == "1"
                && scalar(code, "id") == Ok("0")
                && scalar(code, "device") == Ok("0")
                && unique(code, "cmdcond").is_ok_and(empty)
            {
                continue;
            }
            let payload_tag = if kind == "2" {
                "deviceconditional"
            } else {
                "devicecommand"
            };
            let payload = unique(code, "cmdcond").and_then(|n| unique(n, payload_tag));
            let operation = payload.as_ref().ok().and_then(|n| {
                if n.attribute("owneridtype") == Some("variable") {
                    n.attribute("name").map(str::to_owned)
                } else {
                    scalar(*n, if kind == "2" { "name" } else { "command" })
                        .ok()
                        .map(str::to_owned)
                }
            });
            let opt_in_fade = if kind == "1" && operation.as_deref() == Some("RAMP_TO_LEVEL") {
                fades.map(|options| {
                    compilation(payload.clone().and_then(|payload| {
                        semantic::compile_fade_site(
                            payload,
                            scalar(code, "device")?,
                            &bindings,
                            options.interval_ms,
                            options.lateness_ms,
                        )
                    }))
                })
            } else {
                None
            };
            let result = payload.and_then(|payload| {
                let target = scalar(code, "device")?;
                if kind == "2" {
                    semantic::condition(payload, target, &bindings).map(|_| ())
                } else {
                    semantic::action(payload, target, &bindings).map(|_| ())
                }
            });
            typed_sites.push(Site {
                code_id: scalar(code, "id").unwrap_or("?").into(),
                source_offset: code.range().start,
                kind: if kind == "2" { "condition" } else { "action" },
                operation,
                blocker: result.err(),
                opt_in_fade,
            });
        }
        events.push(Event {
            owner,
            enabled: false,
            source_empty_noop: source_empty(event),
            standard,
            with_fades,
            typed_sites,
        });
    }
    Ok(Review {
        automatic_execution: false,
        syntax_only: true,
        events,
    })
}
