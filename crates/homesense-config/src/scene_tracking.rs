//! Explicit actual-feedback activity policies; never a scene-selection variable.
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub scene: String,
    pub enabled: bool,
    /// Keep App activity without allocating a second HomeKit accessory.
    #[serde(default = "default_hap_activity", skip_serializing_if = "is_true")]
    pub hap_activity: bool,
    /// Project actual load state onto the existing action accessory. The
    /// reviewed absolute recipes keep HomeKit On/Off idempotent; the App's
    /// relative room action and physical FullOn/FullOff keys stay independent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hap_action: Option<HapAction>,
    pub aggregation: Aggregation,
    pub unknown_policy: UnknownPolicy,
    pub pending_policy: PendingPolicy,
    pub control_policy: ControlPolicy,
    pub activity_name: String,
    pub max_age_ms: u64,
    pub review_evidence: String,
    pub members: Vec<Member>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HapAction {
    pub on_scene: String,
    pub off_scene: String,
    /// Existing room-wide actions report any illuminated lamp. A mode whose
    /// full reviewed load pattern must match opts in explicitly instead.
    #[serde(default, skip_serializing_if = "is_any_lamp")]
    pub readback: ActionReadback,
}
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ActionReadback {
    #[default]
    AnyLamp,
    AllConditions,
}
fn is_any_lamp(value: &ActionReadback) -> bool {
    *value == ActionReadback::AnyLamp
}
fn default_hap_activity() -> bool {
    true
}
fn is_true(value: &bool) -> bool {
    *value
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Aggregation {
    All,
    Any,
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UnknownPolicy {
    RequireAllKnown,
    DecisiveKnown,
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PendingPolicy {
    BlockScene,
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ControlPolicy {
    SeparateActionAndReadOnlyActivity,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Member {
    pub id: String,
    pub device: String,
    pub predicate: Predicate,
    pub review_evidence: String,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Predicate {
    Power {
        on: bool,
    },
    /// A relay alone does not prove illumination when its shared dimmer is 0.
    PoweredLight {
        dimmer: String,
    },
    LevelRange {
        minimum: u8,
        maximum: u8,
    },
    Ignore {
        reason: String,
    },
}
pub(crate) fn validate(
    policies: &[Policy],
    document: &crate::rule_document::Document,
    config: &crate::Configuration,
) -> Result<(), crate::Error> {
    use homesense_core::{DeviceId, ModuleKind};
    let valid =
        |s: &str| !s.trim().is_empty() && s.len() <= 250 && !s.chars().any(char::is_control);
    if policies.len() > 1024 {
        return Err(crate::invalid("tracking policy capacity exceeded"));
    }
    let mut ids = std::collections::BTreeSet::new();
    let mut hap_recipes = std::collections::BTreeSet::new();
    for p in policies {
        if let Some(action) = &p.hap_action
            && (!p.enabled
                || p.hap_activity
                || p.aggregation
                    != match action.readback {
                        ActionReadback::AnyLamp => Aggregation::Any,
                        ActionReadback::AllConditions => Aggregation::All,
                    }
                || action.on_scene == action.off_scene
                || [&action.on_scene, &action.off_scene].into_iter().any(|id| {
                    id == &p.scene
                        || !document.scene_action_available(id)
                        || !hap_recipes.insert(id.clone())
                        || !document.hidden_scene_entries.contains(id)
                        || document.scene_rooms.get(id) != document.scene_rooms.get(&p.scene)
                        || policies
                            .iter()
                            .any(|other| &other.scene == id && other.hap_action.is_some())
                }))
        {
            return Err(crate::invalid(
                "stateful HAP action requires reviewed same-room hidden absolute recipes and one actual-state provider",
            ));
        }
        let timed = document.timed_scenes.get(&p.scene);
        let immediate = document.immediate_scenes.contains_key(&p.scene);
        if p.activity_name.trim().is_empty()
            || p.activity_name.len() > 64
            || p.activity_name.chars().any(char::is_control)
        {
            return Err(crate::invalid("explicit activity display name required"));
        }
        if !valid(&p.scene)
            || !ids.insert(p.scene.clone())
            || (!immediate && timed.is_none())
            || p.review_evidence.trim().is_empty()
            || p.review_evidence.len() > 8192
            || !(1..=super::RefreshPolicy::MAX_OBSERVATION_AGE_MS).contains(&p.max_age_ms)
            || p.members.is_empty()
            || p.members.len() > 128
        {
            return Err(crate::invalid("invalid or unreviewed tracking policy"));
        }
        if p.enabled
            && (document.disabled_immediate_scenes.contains(&p.scene)
                || timed.is_some_and(|s| !s.enabled)
                || document.scene_switch_groups.iter().any(|g| {
                    g.members
                        .iter()
                        .any(|m| m.scene == p.scene || m.off_scene == p.scene)
                        || g.reset_scenes.contains(&p.scene)
                }))
        {
            return Err(crate::invalid(
                "tracking provider disabled or conflicts with existing scene selection provider",
            ));
        }
        let mut members = std::collections::BTreeSet::new();
        let mut relevant = 0;
        for m in &p.members {
            let id = DeviceId::new(&m.device)?;
            let d = config
                .devices()
                .get(&id)
                .ok_or_else(|| crate::invalid("tracking device missing"))?;
            let module = &config.gateways()[&d.gateway].modules()[&d.address];
            if !valid(&m.id)
                || !members.insert(m.id.clone())
                || m.review_evidence.trim().is_empty()
                || m.review_evidence.len() > 8192
                || !matches!(module.kind(), ModuleKind::Switch | ModuleKind::Dimmer)
                || (p.enabled && config.binding(&id).is_none())
            {
                return Err(crate::invalid(
                    "tracking member must retain reviewed logical light identity",
                ));
            }
            match &m.predicate {
                Predicate::PoweredLight { dimmer } => {
                    let dimmer = DeviceId::new(dimmer)?;
                    if module.kind() != ModuleKind::Switch
                        || config
                            .binding(&dimmer)
                            .is_none_or(|(_, module)| module.kind() != ModuleKind::Dimmer)
                    {
                        return Err(crate::invalid(
                            "powered light tracking requires an enabled supply and dimmer",
                        ));
                    }
                    relevant += 1;
                }
                Predicate::Ignore { reason } if reason.trim().is_empty() || reason.len() > 8192 => {
                    return Err(crate::invalid("tracking ignore reason required"));
                }
                Predicate::Ignore { .. } => {}
                Predicate::LevelRange { minimum, maximum }
                    if module.kind() != ModuleKind::Dimmer
                        || *minimum == 0
                        || minimum > maximum
                        || *maximum > 100 =>
                {
                    return Err(crate::invalid(
                        "tracking level range requires on-light actual brightness 1..100",
                    ));
                }
                _ => {
                    relevant += 1;
                }
            }
        }
        if relevant == 0 {
            return Err(crate::invalid("tracking policy cannot ignore every member"));
        }
        if let Some(action) = &p.hap_action
            && action.readback == ActionReadback::AllConditions
        {
            validate_pattern_action(p, action, document, config)?;
        }
    }
    Ok(())
}

/// A feedback-derived mode needs absolute recipes, rather than its original
/// relative toggle. Initially admit only flat, complete lighting/LED vectors;
/// conditional shared-supply programs require their separate component owner.
fn validate_pattern_action(
    policy: &Policy,
    action: &HapAction,
    document: &crate::rule_document::Document,
    config: &crate::Configuration,
) -> Result<(), crate::Error> {
    use homesense_core::{
        Command, DeviceId, ModuleKind,
        rule::{Action, Node},
    };
    use std::collections::{BTreeMap, BTreeSet};
    let fail = || {
        crate::invalid(
            "pattern HAP action requires complete flat absolute recipes within the canonical source scope",
        )
    };
    fn source_outputs(nodes: &[Node], outputs: &mut BTreeSet<DeviceId>) -> bool {
        for node in nodes {
            match &node.action {
                Action::Command { device, .. } => {
                    outputs.insert(device.clone());
                }
                Action::If {
                    then_actions,
                    else_actions,
                    ..
                } => {
                    if !source_outputs(then_actions, outputs)
                        || !source_outputs(else_actions, outputs)
                    {
                        return false;
                    }
                }
                _ => return false,
            }
        }
        true
    }
    let source = document
        .immediate_scenes
        .get(&policy.scene)
        .ok_or_else(fail)?;
    let mut scope = BTreeSet::new();
    if !source_outputs(&source.activate, &mut scope)
        || source
            .deactivate
            .as_ref()
            .is_some_and(|off| !source_outputs(off, &mut scope))
    {
        return Err(fail());
    }
    let mut recipes = Vec::new();
    for id in [&action.on_scene, &action.off_scene] {
        let recipe = document.immediate_scenes.get(id).ok_or_else(fail)?;
        let mut outputs = BTreeMap::new();
        for node in &recipe.activate {
            let Action::Command { device, command } = &node.action else {
                return Err(fail());
            };
            let (_, module) = config.binding(device).ok_or_else(fail)?;
            if !matches!(
                (module.kind(), command),
                (ModuleKind::Switch, Command::Switch(_))
                    | (
                        ModuleKind::Dimmer,
                        Command::SetLevel {
                            transition: homesense_core::LevelTransition::Immediate,
                            ..
                        }
                    )
                    | (
                        ModuleKind::Keypad,
                        Command::PanelLed(_) | Command::PanelLedOff
                    )
            ) || outputs.insert(device.clone(), command).is_some()
            {
                return Err(fail());
            }
        }
        if outputs.keys().cloned().collect::<BTreeSet<_>>() != scope {
            return Err(fail());
        }
        recipes.push(outputs);
    }
    let on = &recipes[0];
    let off = &recipes[1];
    let mut covered = BTreeSet::new();
    let mut positive = false;
    for member in &policy.members {
        let id = DeviceId::new(&member.device)?;
        covered.insert(id.clone());
        let matches = match &member.predicate {
            Predicate::Power { on: expected } => {
                positive |= *expected;
                matches!(on.get(&id), Some(Command::Switch(value)) if value == expected)
            }
            Predicate::LevelRange { minimum, maximum } => {
                positive = true;
                matches!(on.get(&id), Some(Command::SetLevel{level,..}) if (*minimum..=*maximum).contains(&level.get()))
            }
            Predicate::PoweredLight { dimmer } => {
                positive = true;
                let dimmer = DeviceId::new(dimmer)?;
                covered.insert(dimmer.clone());
                matches!(on.get(&id), Some(Command::Switch(true)))
                    && matches!(on.get(&dimmer), Some(Command::SetLevel{level,..}) if level.get()>0)
            }
            Predicate::Ignore { .. } => false,
        };
        if !matches {
            return Err(fail());
        }
    }
    let lighting = on
        .keys()
        .filter(|id| {
            config
                .binding(id)
                .is_some_and(|(_, m)| m.kind() != ModuleKind::Keypad)
        })
        .cloned()
        .collect::<BTreeSet<_>>();
    if !positive
        || lighting != covered
        || off.values().any(|command| match command {
            Command::Switch(on) => *on,
            Command::SetLevel { level, .. } => level.get() != 0,
            Command::PanelLed(_) | Command::PanelLedOff => false,
            _ => true,
        })
    {
        return Err(fail());
    }
    Ok(())
}

impl crate::rule_document::Document {
    /// A HAP absolute recipe retains its canonical scene's confirmation policy.
    /// This changes proof identity only; admission, commands and caller grants
    /// still use the actual requested recipe and its reviewed output scope.
    pub fn scene_confirmation_id<'a>(&'a self, id: &'a str) -> &'a str {
        self.scene_tracking
            .values()
            .find_map(|policy| {
                policy
                    .hap_action
                    .as_ref()
                    .filter(|action| action.on_scene == id || action.off_scene == id)
                    .map(|_| policy.scene.as_str())
            })
            .unwrap_or(id)
    }
}
