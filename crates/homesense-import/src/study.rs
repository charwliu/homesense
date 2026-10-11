//! Pure study rule replay. No socket, persistence, or automatic bus writes.
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    SetMode {
        value: u8,
        source: String,
    },
    Scene {
        scene_id: String,
        activate: bool,
        source: String,
    },
    Led {
        key: u8,
        on: bool,
        source: String,
    },
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub key: u8,
    pub mode: u8,
    pub then_actions: Vec<Action>,
    pub else_actions: Vec<Action>,
}
#[derive(Debug, PartialEq)]
pub enum Evaluation {
    Unknown,
    Ignored,
    Planned { next_mode: u8, actions: Vec<Action> },
}
/// State is explicitly supplied by the caller; evaluating never changes it.
/// A shadow observer must not infer EA-5's private variable from lamp levels.
pub fn evaluate(
    rules: &[Rule],
    key: u8,
    released: bool,
    mode: Option<u8>,
) -> Result<Evaluation, String> {
    if rules.len() != 3 {
        return Err("three study rules required".into());
    }
    for (index, rule) in rules.iter().enumerate() {
        if rule.key != [1, 3, 5][index] || rule.mode != (index + 1) as u8 {
            return Err("invalid study rule mapping".into());
        }
        for action in rule.then_actions.iter().chain(&rule.else_actions) {
            let source = match action {
                Action::SetMode { value, source } if *value <= 3 => source,
                Action::Scene {
                    scene_id, source, ..
                } if ["10", "6", "8"].contains(&scene_id.as_str()) => source,
                Action::Led { key, source, .. } if [1, 3, 5].contains(key) => source,
                _ => return Err("action outside study scope".into()),
            };
            if source.is_empty() {
                return Err("source path required".into());
            }
        }
    }
    if !released {
        return Ok(Evaluation::Ignored);
    }
    let Some(rule) = rules.iter().find(|r| r.key == key) else {
        return Ok(Evaluation::Ignored);
    };
    let Some(mode) = mode else {
        return Ok(Evaluation::Unknown);
    };
    if mode > 3 {
        return Err("invalid study mode".into());
    }
    let actions = if mode != rule.mode {
        &rule.then_actions
    } else {
        &rule.else_actions
    };
    let mut next_mode = mode;
    for action in actions {
        if let Action::SetMode { value, .. } = action {
            next_mode = *value;
        }
    }
    Ok(Evaluation::Planned {
        next_mode,
        actions: actions.clone(),
    })
}

#[derive(Debug, Serialize, PartialEq)]
pub struct MemberTarget {
    pub device: String,
    pub level: u8,
    pub source_delay: u64,
    pub source_rate: u64,
    pub source_proxy: String,
}
/// Expand the reviewed study scene while retaining unverified timing units.
/// The returned plan is not transport-ready: timing policy must be selected
/// explicitly by the runtime adapter rather than interpreting C4 rate as a byte.
pub fn targets(
    document: &serde_json::Value,
    scene_id: &str,
    activate: bool,
) -> Result<Vec<MemberTarget>, String> {
    if !["10", "6", "8"].contains(&scene_id) {
        return Err("scene outside study scope".into());
    }
    if !activate && document["deactivate_policy"] != "all_scene_members_off" {
        return Err("deactivation semantics not confirmed".into());
    }
    let scenes = document["scenes"].as_array().ok_or("scenes missing")?;
    let matches: Vec<_> = scenes
        .iter()
        .filter(|s| s["scene_id"] == scene_id)
        .collect();
    if matches.len() != 1 {
        return Err("scene missing or duplicate".into());
    }
    let members = matches[0]["members"].as_array().ok_or("members missing")?;
    if members.len() != 4 {
        return Err("four reviewed study members required".into());
    }
    let expected = [
        ("211", "control4:210"),
        ("279", "control4:278"),
        ("309", "control4:308"),
        ("337", "control4:336"),
    ];
    let mut result = Vec::new();
    for (member, (proxy, device)) in members.iter().zip(expected) {
        if member["device_id"] != proxy || document["proxy_to_device"][proxy] != device {
            return Err("unreviewed proxy mapping or order".into());
        }
        let elements = member["elements"].as_array().ok_or("elements missing")?;
        if elements.len() != 1 {
            return Err("unreviewed multistep scene".into());
        }
        let number = |key: &str| -> Result<u64, String> {
            elements[0][key]
                .as_str()
                .ok_or_else(|| format!("missing {key}"))?
                .parse()
                .map_err(|_| format!("invalid {key}"))
        };
        let level = number("level")?;
        if level > 100 {
            return Err("invalid level".into());
        }
        result.push(MemberTarget {
            device: device.into(),
            level: if activate { level as u8 } else { 0 },
            source_delay: number("delay")?,
            source_rate: number("rate")?,
            source_proxy: proxy.into(),
        });
    }
    Ok(result)
}
