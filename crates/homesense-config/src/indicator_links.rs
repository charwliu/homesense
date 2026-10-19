//! Explicit reviewed off links; one load/scene may fan out to many LEDs.
use crate::{Configuration, Error, invalid, scene_switches::Group};
use homesense_core::{DeviceId, ModuleKind, PanelLedMode};
use serde::Deserialize;
use std::collections::BTreeSet;
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Link {
    pub id: String,
    #[serde(default)]
    pub enabled: bool,
    pub review_evidence: Option<String>,
    #[serde(deserialize_with = "device_ids")]
    pub lights: Vec<DeviceId>,
    /// Reviewed subset required Off; remaining lights must be On. Physical
    /// policies retain at least one On input and cannot add an unlisted read.
    #[serde(default, deserialize_with = "device_ids")]
    pub lights_off: Vec<DeviceId>,
    #[serde(deserialize_with = "device_ids")]
    pub leds: Vec<DeviceId>,
    pub scene: Option<String>,
    #[serde(default)]
    pub power_policy: PowerPolicy,
}
/// Opt-in physical reconciliation. Clearing a mode never infers its identity.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PowerPolicy {
    #[default]
    LegacyOff,
    FollowAllOn,
    ClearIfAnyOff,
}
pub(crate) fn validate(
    links: &[Link],
    config: &Configuration,
    groups: &[Group],
    scenes: &std::collections::BTreeMap<String, crate::rule_document::TimedScene>,
) -> Result<(), Error> {
    if links.len() > 128 {
        return Err(invalid("indicator link capacity exceeded"));
    }
    let mut ids = BTreeSet::new();
    let mut owners = BTreeSet::new();
    for link in links {
        if link.id.trim().is_empty()
            || link.id.len() > 250
            || link.id.chars().any(char::is_control)
            || !ids.insert(&link.id)
            || link.lights.is_empty()
            || link.lights.len() > 64
            || link.leds.is_empty()
            || link.leds.len() > 64
            || link.lights.iter().collect::<BTreeSet<_>>().len() != link.lights.len()
            || link.lights_off.iter().collect::<BTreeSet<_>>().len() != link.lights_off.len()
            || link.lights_off.len() >= link.lights.len()
            || link.lights_off.iter().any(|id| !link.lights.contains(id))
            || (!link.lights_off.is_empty() && link.power_policy == PowerPolicy::LegacyOff)
            || link.leds.iter().collect::<BTreeSet<_>>().len() != link.leds.len()
        {
            return Err(invalid("invalid indicator link"));
        }
        if !link.enabled {
            continue;
        }
        if link.power_policy != PowerPolicy::LegacyOff && link.scene.is_some() {
            return Err(invalid("physical power indicator cannot also own scene selection"));
        }
        if !link
            .review_evidence
            .as_ref()
            .is_some_and(|s| !s.trim().is_empty() && s.len() <= 8192)
        {
            return Err(invalid("enabled indicator link needs review evidence"));
        }
        for id in &link.lights {
            if !config
                .binding(id)
                .is_some_and(|(_, m)| matches!(m.kind(), ModuleKind::Switch | ModuleKind::Dimmer))
                || config
                    .devices()
                    .values()
                    .any(|d| d.enabled && d.supply_relay.as_ref() == Some(id))
            {
                return Err(invalid("indicator links require logical light bindings"));
            }
        }
        for id in &link.leds {
            let (_, module) = config
                .binding(id)
                .ok_or_else(|| invalid("indicator LED binding unavailable"))?;
            if module.kind() != ModuleKind::Keypad || !owners.insert(id) {
                return Err(invalid("ambiguous indicator LED owner"));
            }
            // The device parser validates channel/profile; LED ownership
            // additionally requires explicitly configured manual mode.
            if module.led_mode() != PanelLedMode::Manual {
                return Err(invalid("indicator requires explicit manual LED mode"));
            }
        }
        if let Some(scene) = &link.scene {
            if !groups
                .iter()
                .any(|g| g.members.iter().any(|m| &m.scene == scene))
            {
                return Err(invalid(
                    "indicator scene must be a reviewed selection member",
                ));
            }
            let scene = scenes
                .get(scene)
                .ok_or_else(|| invalid("indicator scene missing"))?;
            let mut scope = BTreeSet::new();
            for step in &scene.steps {
                match step.command {
                    homesense_core::Command::Switch(_)
                    | homesense_core::Command::SetLevel { .. } => {
                        scope.insert(&step.device);
                    }
                    homesense_core::Command::PanelLed(_) => {}
                    _ => return Err(invalid("indicator scene has unknown final light scope")),
                }
            }
            if scope != link.lights.iter().collect() {
                return Err(invalid(
                    "indicator link must cover the complete scene light scope",
                ));
            }
        }
    }
    Ok(())
}

fn device_ids<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<DeviceId>, D::Error> {
    Vec::<String>::deserialize(deserializer)?
        .into_iter()
        .map(|id| DeviceId::new(id).map_err(serde::de::Error::custom))
        .collect()
}
