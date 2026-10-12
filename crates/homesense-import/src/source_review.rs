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
