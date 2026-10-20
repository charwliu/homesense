//! Shared-resource manual controls. Physical programs retain their original
//! register semantics; a manual pair intent requires a fresh physical lease.
use crate::{
    admission::{Admission, TaskGeneration},
    feedback::{Feedback, RuleLease},
    management_devices::DeviceAction,
};
use homesense_config::{Configuration, shared_light::Group};
use homesense_core::{Command, DeviceId, Observation, Percent, shared_light::Observed};
use homesense_hap::requests::{Batch, Intent};
use homesense_state::{Quality, Snapshot};
use uuid::Uuid;
#[cfg(test)]
#[path = "shared_light_tests.rs"]
mod tests;

pub(crate) enum Prepared {
    Master(Box<MasterPrepared>),
    Composed(Box<crate::composed_light::Prepared>),
}
impl Prepared {
    pub(crate) fn compile(self, configuration:&Configuration, source:Option<&crate::source_auth::Source>)
        -> Result<(Batch,RuleLease),hap::server::WriteStatus> {
        match self {Self::Master(p)=>p.compile(configuration,source), Self::Composed(p)=>p.compile(configuration,source)}
    }
}

pub(crate) struct MasterPrepared {
    controller: Uuid,
    revision: u64,
    device: DeviceId,
    action: DeviceAction,
    group: Group,
    observed: Observed,
    level: Percent,
    lease: RuleLease,
}

/// A raw recipe can supersede logical intent without having a new logical
/// target itself. Reading/bedside-only writes do not affect the common pair.
pub(crate) fn physical_target_invalidations(
    configuration: &Configuration,
    intents: &[Intent],
) -> std::collections::BTreeSet<DeviceId> {
    let mut result = std::collections::BTreeSet::new();
    for intent in intents {
        if matches!(intent.command, Command::Query) {
            continue;
        }
        if let Some(group) = configuration.composed_resource(&intent.device) {
            result.extend(group.lights.iter().map(|light|light.id.clone()));
        }
        let Some(group) = configuration.shared_resource(&intent.device) else {
            continue;
        };
        if intent.device == group.common_supply {
            result.insert(group.primary.clone());
            result.insert(group.secondary.clone());
        } else if group.light(&intent.device).is_some() {
            result.insert(intent.device.clone());
        }
    }
    result
}

impl MasterPrepared {
    pub(crate) fn compile(
        self,
        configuration: &Configuration,
        source: Option<&crate::source_auth::Source>,
    ) -> Result<(Batch, RuleLease), hap::server::WriteStatus> {
        use hap::server::WriteStatus;
        let Some(group) = configuration.shared_light(&self.device) else {
            return Err(WriteStatus::InvalidValueInRequest);
        };
        if self.revision != configuration.revision() || group != &self.group {
            return Err(WriteStatus::InvalidValueInRequest);
        }
        if let Some(source) = source {
            let original = crate::automation_control::Action::DeviceCommand {
                controller: self.controller,
                revision: self.revision,
                device: self.device.clone(),
                action: self.action,
            };
            if !source.matches(&original)
                || group
                    .devices()
                    .iter()
                    .any(|id| !source.permits_device(id.as_str()))
            {
                return Err(WriteStatus::InsufficientPrivileges);
            }
        }
        let light = group
            .light(&self.device)
            .ok_or(WriteStatus::InvalidValueInRequest)?;
        let plan = homesense_core::shared_light::plan(self.observed, light, self.level);
        let intents = plan
            .writes
            .into_iter()
            .map(|(output, command)| Intent {
                device: group.output(output).clone(),
                command,
            })
            .collect();
        Ok((
            Batch {
                controller: self.controller,
                intents,
                subscriptions: vec![],
            },
            self.lease,
        ))
    }
}

/// Only the native manual device endpoint uses this path. Scene/rule actions
/// are reviewed physical recipes and must not be expanded a second time.
pub(crate) async fn prepare(
    admission: &Admission,
    controller: Uuid,
    generation: TaskGeneration,
    id: &DeviceId,
    action: &DeviceAction,
    feedback: &Feedback,
    max_age_ms: u64,
) -> Result<Option<Prepared>, String> {
    if let Some(prepared) = crate::composed_light::prepare(admission,controller,generation.clone(),id,action,feedback,max_age_ms).await? {
        return Ok(Some(Prepared::Composed(Box::new(prepared))));
    }
    let bundle = admission
        .automation_bundle(controller, generation)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let Some(group) = bundle.configuration.shared_light(id) else {
        return Ok(None);
    };
    let (_, module) = bundle
        .configuration
        .binding(id)
        .ok_or("shared light disabled")?;
    if module.kind() != homesense_core::ModuleKind::Dimmer {
        return Err("shared light requires a dimmer target".into());
    }
    let levels = admission
        .remember_levels(
            bundle.configuration.revision(),
            feedback.snapshot(),
            max_age_ms,
        )
        .await
        .map_err(|e| format!("{e:?}"))?;
    let level = match action {
        DeviceAction::Power { on: false } => Percent::new(0).unwrap(),
        DeviceAction::Power { on: true } => levels
            .get(id)
            .copied()
            .filter(|n| n.get() != 0)
            .or(bundle
                .configuration
                .binding(id)
                .and_then(|(d, _)| d.default_on_level))
            .ok_or("shared light restore missing")?,
        DeviceAction::Brightness { level } => Percent::new(*level).map_err(|e| e.to_string())?,
        _ => return Err("shared light accepts power or brightness only".into()),
    };
    let ids = group.devices().into_iter().cloned().collect::<Vec<_>>();
    let (powers, levels, lease) =
        feedback.native_scene_capture(&ids, admission.monotonic_ms(), max_age_ms)?;
    let power = |id: &DeviceId| {
        powers
            .get(id)
            .copied()
            .flatten()
            .ok_or("shared supply feedback unavailable")
    };
    let dimmer = |id: &DeviceId| {
        levels
            .get(id)
            .copied()
            .ok_or("shared dimmer feedback unavailable")
    };
    let observed = Observed {
        common_supply: power(&group.common_supply)?,
        left_supply: power(&group.left_supply)?,
        right_supply: power(&group.right_supply)?,
        primary_level: dimmer(&group.primary)?,
        secondary_level: dimmer(&group.secondary)?,
        reading_level: dimmer(&group.reading)?,
    };
    Ok(Some(Prepared::Master(Box::new(MasterPrepared {
        controller,
        revision: bundle.configuration.revision(),
        device: id.clone(),
        action: action.clone(),
        group: group.clone(),
        observed,
        level,
        lease,
    }))))
}

/// Project only the common pair. Original scene predicates still consume the
/// physical matrix; they cannot mistake a reading/night column for the main.
pub(crate) fn project(
    configuration: &Configuration,
    physical: &Snapshot,
    logical: &mut Snapshot,
) -> Result<(), &'static str> {
    for group in configuration.shared_lights().values() {
        let supply = physical
            .devices
            .get(&group.common_supply)
            .ok_or("shared supply state missing")?;
        for id in [&group.primary, &group.secondary] {
            let dimmer = physical
                .devices
                .get(id)
                .ok_or("shared dimmer state missing")?;
            let view = logical
                .devices
                .get_mut(id)
                .ok_or("shared logical state missing")?;
            view.observed_at_ms = supply
                .observed_at_ms
                .zip(dimmer.observed_at_ms)
                .map(|(a, b)| a.min(b));
            view.version = supply.version.max(dimmer.version);
            view.desired = None;
            view.pending_command = None;
            view.last_command_id = None;
            view.quality = match (supply.quality, dimmer.quality) {
                (Quality::Confirmed, Quality::Confirmed) => Quality::Confirmed,
                (Quality::Unknown, _) | (_, Quality::Unknown) => Quality::Unknown,
                _ => Quality::Stale,
            };
            view.observed = match (supply.observed, dimmer.observed) {
                (Some(Observation::Switch(on)), Some(Observation::Level(level))) => {
                    Some(Observation::Level(if on {
                        level
                    } else {
                        Percent::new(0).unwrap()
                    }))
                }
                _ => {
                    view.quality = Quality::Unknown;
                    None
                }
            };
        }
    }
    Ok(())
}
