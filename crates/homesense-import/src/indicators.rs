//! Source evidence only: lexical co-occurrence does not prove a scene binding.
use roxmltree::{Document, Node};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Led {
    pub panel: String,
    pub key: u8,
}
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Scope {
    /// Includes wrapper tags and ancestor code IDs, keeping Else separate.
    pub path: Vec<String>,
    pub condition_hashes: Vec<String>,
}
#[derive(Debug, Serialize)]
pub struct Action {
    pub event: String,
    pub code: String,
    pub scope: Scope,
    pub source_enabled: bool,
    pub enabled: bool,
    pub operation: String,
    pub scene: Option<String>,
    pub led: Option<Led>,
    pub payload_sha256: String,
    pub diagnostic: Option<String>,
}
#[derive(Debug, Serialize)]
pub struct Candidate {
    pub event: String,
    pub scene: String,
    pub scope: Scope,
    pub activation_codes: Vec<String>,
    pub leds: Vec<Led>,
    pub enabled: bool,
    pub disposition: &'static str,
}
#[derive(Debug, Serialize)]
pub struct Comparison {
    pub scene: String,
    pub entries: usize,
    pub union: Vec<Led>,
    pub intersection: Vec<Led>,
    pub differing_entries: bool,
    pub enabled: bool,
}
#[derive(Debug, Default, Serialize)]
pub struct Inventory {
    pub actions: Vec<Action>,
    pub candidates: Vec<Candidate>,
    pub comparisons: Vec<Comparison>,
}
fn value<'a>(n: Node<'a, '_>, tag: &str) -> Option<&'a str> {
    let mut found = n.children().filter(|n| n.has_tag_name(tag));
    let first = found.next()?;
    if found.next().is_some() {
        return None;
    }
    first.text().map(str::trim)
}
fn parameter(n: Node<'_, '_>, name: &str) -> Option<String> {
    let ps = n.children().find(|n| n.has_tag_name("params"))?;
    let mut ps = ps.children().filter(|n| n.is_element());
    let p = ps.next()?;
    if ps.next().is_some() || value(p, "name")? != name {
        return None;
    }
    let v = p.children().find(|n| n.has_tag_name("value"))?;
    let mut vals = v.children().filter(|n| n.is_element());
    let s = vals.next()?;
    if vals.next().is_some() || !s.has_tag_name("static") {
        return None;
    }
    Some(s.text()?.trim().to_owned())
}
fn scope(code: Node<'_, '_>) -> Scope {
    let mut ancestors = code
        .ancestors()
        .skip(1)
        .take_while(|n| !n.has_tag_name("event"))
        .filter(|n| n.is_element())
        .collect::<Vec<_>>();
    ancestors.reverse();
    let mut path = vec![];
    let mut condition_hashes = vec![];
    for n in ancestors {
        if n.has_tag_name("codeitem") {
            path.push(format!("code:{}", value(n, "id").unwrap_or("?")));
            if value(n, "type") == Some("2")
                && let Some(p) = n.children().find(|n| n.has_tag_name("cmdcond"))
            {
                condition_hashes.push(super::sha256_hex(
                    &n.document().input_text().as_bytes()[p.range()],
                ));
            }
        } else {
            path.push(format!("{}@{}", n.tag_name().name(), n.range().start));
        }
    }
    Scope {
        path,
        condition_hashes,
    }
}
pub(crate) fn inspect(doc: &Document<'_>, keypads: &BTreeSet<String>) -> Inventory {
    let mut inventory = Inventory::default();
    for event in doc.descendants().filter(|n| n.has_tag_name("event")) {
        let owner = format!(
            "event:{}:{}",
            value(event, "deviceid").unwrap_or("?"),
            value(event, "eventid").unwrap_or("?")
        );
        for code in event.descendants().filter(|n| n.has_tag_name("codeitem")) {
            let Some(payload) = code.children().find(|n| n.has_tag_name("cmdcond")) else {
                continue;
            };
            let mut commands = payload
                .children()
                .filter(|n| n.has_tag_name("devicecommand"));
            let Some(command) = commands.next() else {
                continue;
            };
            if commands.next().is_some() {
                continue;
            }
            let Some(operation) = value(command, "command") else {
                continue;
            };
            let target = value(code, "device").unwrap_or("?");
            let scene_action = matches!(operation, "ACTIVATE_SCENE" | "DEACTIVATE_SCENE");
            let led_action =
                keypads.contains(target) && matches!(operation, "Turn On" | "Turn Off");
            if !scene_action && !led_action {
                continue;
            }
            let mut diagnostic = None;
            let scene = if scene_action {
                let s = parameter(command, "SCENE_ID");
                if s.as_deref() != command.attribute("owneriditem")
                    || command.attribute("owneridtype") != Some("agent")
                    || s.is_none()
                {
                    diagnostic = Some("unresolved scene parameter or owner mismatch".into());
                    None
                } else {
                    s
                }
            } else {
                None
            };
            let led = if led_action {
                match parameter(command, "LED Number")
                    .and_then(|s| s.parse::<u8>().ok())
                    .filter(|k| (1..=6).contains(k))
                {
                    Some(key) => Some(Led {
                        panel: target.into(),
                        key,
                    }),
                    None => {
                        diagnostic =
                            Some("unresolved LED parameter or outside six-key profile".into());
                        None
                    }
                }
            } else {
                None
            };
            let source_enabled = code
                .ancestors()
                .take_while(|n| !n.has_tag_name("event"))
                .filter(|n| n.has_tag_name("codeitem"))
                .all(|n| value(n, "enabled") == Some("True"));
            inventory.actions.push(Action {
                event: owner.clone(),
                code: value(code, "id").unwrap_or("?").into(),
                scope: scope(code),
                source_enabled,
                enabled: false,
                operation: operation.into(),
                scene,
                led,
                payload_sha256: super::sha256_hex(&doc.input_text().as_bytes()[payload.range()]),
                diagnostic,
            });
        }
    }
    let mut scopes: BTreeMap<(&str, &Scope), Vec<&Action>> = BTreeMap::new();
    for a in inventory
        .actions
        .iter()
        .filter(|a| a.source_enabled && a.diagnostic.is_none())
    {
        scopes.entry((&a.event, &a.scope)).or_default().push(a);
    }
    for ((event, scope), actions) in scopes {
        let mut scenes: BTreeMap<&str, Vec<String>> = BTreeMap::new();
        let mut on = BTreeSet::new();
        let mut off = BTreeSet::new();
        for action in actions {
            match action.operation.as_str() {
                "ACTIVATE_SCENE" => {
                    if let Some(scene) = &action.scene {
                        scenes.entry(scene).or_default().push(action.code.clone());
                    }
                }
                "Turn On" => {
                    if let Some(led) = &action.led {
                        on.insert(led.clone());
                    }
                }
                "Turn Off" => {
                    if let Some(led) = &action.led {
                        off.insert(led.clone());
                    }
                }
                _ => {}
            }
        }
        let disposition = if scenes.len() > 1 {
            "ambiguous_multiple_scenes_in_scope"
        } else if !on.is_disjoint(&off) {
            "ambiguous_led_writes_in_scope"
        } else {
            "lexical_candidate_requires_binding_and_branch_review"
        };
        for (scene, activation_codes) in scenes {
            inventory.candidates.push(Candidate {
                event: event.into(),
                scene: scene.into(),
                scope: scope.clone(),
                activation_codes,
                // Ambiguous scopes have raw action evidence but no guessed association.
                leds: if disposition == "lexical_candidate_requires_binding_and_branch_review" {
                    on.iter().cloned().collect()
                } else {
                    vec![]
                },
                enabled: false,
                disposition,
            });
        }
    }
    let mut scenes: BTreeMap<&str, Vec<BTreeSet<Led>>> = BTreeMap::new();
    for c in &inventory.candidates {
        if c.disposition == "lexical_candidate_requires_binding_and_branch_review" {
            scenes
                .entry(&c.scene)
                .or_default()
                .push(c.leds.iter().cloned().collect());
        }
    }
    for (scene, sets) in scenes {
        let union = sets
            .iter()
            .flat_map(|s| s.iter().cloned())
            .collect::<BTreeSet<_>>();
        let intersection = union
            .iter()
            .filter(|l| sets.iter().all(|s| s.contains(*l)))
            .cloned()
            .collect::<BTreeSet<_>>();
        inventory.comparisons.push(Comparison {
            scene: scene.into(),
            entries: sets.len(),
            differing_entries: sets.iter().any(|s| s != &sets[0]),
            union: union.into_iter().collect(),
            intersection: intersection.into_iter().collect(),
            enabled: false,
        });
    }
    inventory
}
