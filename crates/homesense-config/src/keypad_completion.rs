//! Reviewed, key-specific effects after a real actuator write and feedback.
//! This declares no new actuator command, query, credential or deadline.
use crate::{
    Configuration, Error, component_modes::{FirstValue, Postcondition},
    invalid, keypad::{KeyAction, Keypad}, rule_document::Document,
};
use homesense_core::{DeviceId, ModuleKind, PanelLedMode, rule::{Lifetime, ValueType}};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Indicator {
    pub device: String,
    pub on: bool,
}

/// `after` must belong to the source selection's physical recipe and must be
/// confirmed after that gesture's successful write. `when` is the complete
/// fresh, settled condition, not an assumed false value for missing feedback.
/// Variable versions and source authority must be retained before admission.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SelectionCompletion {
    pub id: String,
    pub after: Postcondition,
    pub when: Vec<Postcondition>,
    pub variable: String,
    pub value: FirstValue,
    pub indicators: Vec<Indicator>,
    pub max_age_ms: u64,
    pub review_evidence: String,
}

fn light(config: &Configuration, condition: &Postcondition) -> Result<DeviceId, Error> {
    let id = DeviceId::new(&condition.device)?;
    let (_, module) = config.binding(&id)
        .filter(|(device, _)| device.enabled)
        .ok_or_else(|| invalid("key completion light binding unavailable"))?;
    if !matches!(module.kind(), ModuleKind::Switch | ModuleKind::Dimmer)
        || condition.level.is_some_and(|level| module.kind() != ModuleKind::Dimmer
            || level > 100 || (level > 0) != condition.on)
    {
        return Err(invalid("key completion requires a consistent lighting observation"));
    }
    Ok(id)
}

/// Complete read and indicator authority. A supplying relay is always part of
/// the same physical read, even if a member can see the logical dimmer alone.
pub fn devices(config: &Configuration, effect: &SelectionCompletion) -> Result<BTreeSet<DeviceId>, Error> {
    let mut ids = BTreeSet::new();
    for condition in std::iter::once(&effect.after).chain(&effect.when) {
        ids.insert(light(config, condition)?);
    }
    for indicator in &effect.indicators {
        ids.insert(DeviceId::new(&indicator.device)?);
    }
    let supplies = ids.iter().filter_map(|id| config.devices().get(id)
        .and_then(|device| device.supply_relay.clone())).collect::<Vec<_>>();
    ids.extend(supplies);
    Ok(ids)
}

fn effects(panel: &Keypad) -> impl Iterator<Item = (&crate::keypad::Key, &SelectionCompletion)> {
    panel.keys.values().filter_map(|key| key.selection_completion.as_ref().map(|effect| (key, effect)))
}

pub(crate) fn validate_bindings(config: &Configuration) -> Result<(), Error> {
    let mut names = BTreeSet::new();
    let mut count = 0;
    for panel in config.keypads().values() {
        for (key, effect) in effects(panel) {
            count += 1;
            crate::identifier(&effect.id)?;
            crate::identifier(&effect.variable)?;
            if !names.insert(&effect.id) || count > 256
                || !matches!(key.action, Some(KeyAction::SceneToggle { .. }))
                || effect.when.is_empty() || effect.when.len() > 16
                || effect.indicators.is_empty() || effect.indicators.len() > 16
                || effect.max_age_ms == 0
                || effect.max_age_ms > config.refresh_policy().observation_max_age_ms()
                || effect.review_evidence.trim().is_empty() || effect.review_evidence.len() > 4096
            {
                return Err(invalid("invalid or excessive key selection completion"));
            }
            let mut reads = BTreeSet::from([light(config, &effect.after)?]);
            for condition in &effect.when {
                if !reads.insert(light(config, condition)?) {
                    return Err(invalid("duplicate key completion observation"));
                }
            }
            let mut leds = BTreeSet::new();
            for indicator in &effect.indicators {
                let id = DeviceId::new(&indicator.device)?;
                let (_, module) = config.binding(&id)
                    .filter(|(device, _)| device.enabled)
                    .ok_or_else(|| invalid("key completion indicator missing"))?;
                if !leds.insert(id.clone()) || id.as_str() == key.led_endpoint.id
                    || module.kind() != ModuleKind::Keypad
                    || module.led_mode() != PanelLedMode::Manual
                    || !config.keypads().values().any(|p| p.enabled
                        && p.keys.values().any(|k| k.led_endpoint.enabled && k.led_endpoint.id == id.as_str()))
                {
                    return Err(invalid("key completion requires distinct enabled linked manual indicators"));
                }
            }
        }
    }
    Ok(())
}

pub(crate) fn validate_document(config: &Configuration, document: &Document) -> Result<(), Error> {
    for panel in config.keypads().values() {
        for (key, effect) in effects(panel) {
            let variable = document.variables.get(&effect.variable)
                .ok_or_else(|| invalid("key completion variable missing"))?;
            if variable.lifetime != Lifetime::Persistent
                || !matches!((variable.value_type, &effect.value),
                    (ValueType::Bool, FirstValue::Bool(_)) | (ValueType::Integer, FirstValue::Integer(_)))
            {
                return Err(invalid("key completion requires a typed persistent variable"));
            }
            let Some(KeyAction::SceneToggle { scene, .. }) = &key.action else {
                return Err(invalid("key completion selection action missing"));
            };
            let group = document.scene_switch_groups.iter().find(|group|
                group.members.iter().any(|member| &member.scene == scene))
                .ok_or_else(|| invalid("key completion selection group missing"))?;
            let member = group.members.iter().find(|member| &member.scene == scene).unwrap();
            let targets = group.absolute_light_targets(&document.timed_scenes)?;
            let trigger = DeviceId::new(&effect.after.device)?;
            if !targets.get(&member.value).is_some_and(|targets|
                targets.contains_key(&trigger))
            {
                return Err(invalid("key completion must follow a light in its own selection recipe"));
            }
            // A completion cannot recursively invoke an event rule or replace
            // the source selection's own version fence.
            if effect.variable == group.variable || document.rules.iter().any(|rule|
                matches!(&rule.trigger, crate::rule_document::Trigger::VariableChanged { variable } if variable == &effect.variable))
            {
                return Err(invalid("key completion cannot target its own selection or an event variable"));
            }
        }
    }
    Ok(())
}
