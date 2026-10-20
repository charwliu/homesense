//! One bounded manual HAP intent over multiple logical resource owners.
//! Preparation never writes. Admission revalidates the complete feedback lease
//! and commits every physical command together, or none of them.
use crate::{
    admission::{Admission, TaskGeneration},
    feedback::{Feedback, RuleLease},
};
use homesense_config::Configuration;
use homesense_core::{Command, DeviceId, LevelTransition, ModuleKind, Percent};
use homesense_hap::requests::{Batch, Intent};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

/// Client light combinations are deliberately small; large absolute operations use scenes.
pub(crate) const MAX_JOINT_LIGHTS: usize = 10;

#[derive(Debug)]
pub(crate) enum Error {
    Invalid(String),
    Unavailable(String),
}
impl Error {
    pub(crate) fn status(&self) -> hap::server::WriteStatus {
        match self {
            Self::Invalid(_) => hap::server::WriteStatus::InvalidValueInRequest,
            Self::Unavailable(_) => hap::server::WriteStatus::ServiceCommunicationFailure,
        }
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(reason) | Self::Unavailable(reason) => f.write_str(reason),
        }
    }
}
impl From<&str> for Error {
    fn from(reason: &str) -> Self {
        Self::Invalid(reason.into())
    }
}
impl From<String> for Error {
    fn from(reason: String) -> Self {
        Self::Unavailable(reason)
    }
}

pub(crate) struct Prepared {
    controller: Uuid,
    revision: u64,
    physical: Vec<Intent>,
    lease: RuleLease,
}
impl Prepared {
    pub(crate) fn compile(
        self,
        configuration: &Configuration,
        source: Option<&crate::source_auth::Source>,
    ) -> Result<(Batch, RuleLease), hap::server::WriteStatus> {
        // This entry is internal to the paired HAP write path. A native source
        // cannot substitute an unreviewed joint action for its captured action.
        if source.is_some() {
            return Err(hap::server::WriteStatus::InsufficientPrivileges);
        }
        if configuration.revision() != self.revision {
            return Err(hap::server::WriteStatus::InvalidValueInRequest);
        }
        Ok((
            Batch {
                controller: self.controller,
                intents: self.physical,
                subscriptions: vec![],
            },
            self.lease,
        ))
    }
}
fn percent(command: Command) -> Result<Percent, Error> {
    match command {
        Command::SetLevel {
            level,
            transition: LevelTransition::Immediate,
        } => Ok(level),
        _ => Err("joint logical light requires normalized immediate brightness".into()),
    }
}
fn merge(physical: &mut BTreeMap<DeviceId, Command>, intents: Vec<Intent>) -> Result<(), Error> {
    for intent in intents {
        if physical
            .insert(intent.device, intent.command)
            .is_some_and(|prior| prior != intent.command)
        {
            return Err("joint light requests conflict on one physical output".into());
        }
    }
    Ok(())
}

pub(crate) async fn prepare(
    admission: &Admission,
    controller: Uuid,
    generation: TaskGeneration,
    revision: u64,
    intents: Vec<Intent>,
    feedback: &Feedback,
    max_age_ms: u64,
) -> Result<Prepared, Error> {
    if intents.is_empty() || intents.len() > MAX_JOINT_LIGHTS {
        return Err("joint light request bound invalid".into());
    }
    let bundle = admission
        .automation_bundle(controller, generation)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let config = &bundle.configuration;
    if config.revision() != revision {
        return Err("joint light revision changed".into());
    }
    let mut requested = BTreeSet::new();
    let mut resources = BTreeSet::new();
    let mut composed = BTreeMap::<String, BTreeMap<DeviceId, Percent>>::new();
    let mut legacy = BTreeMap::<String, BTreeMap<DeviceId, Percent>>::new();
    let mut ordinary = vec![];
    for intent in intents {
        if !requested.insert(intent.device.clone()) {
            return Err("duplicate joint logical target".into());
        }
        let (device, module) = config
            .binding(&intent.device)
            .ok_or("joint light missing or disabled")?;
        if let Some((group, _)) = config.composed_light(&intent.device) {
            composed
                .entry(group.id.clone())
                .or_default()
                .insert(intent.device, percent(intent.command)?);
            resources.extend(group.resources().into_iter().cloned());
        } else if let Some(group) = config.shared_light(&intent.device) {
            legacy
                .entry(group.id.clone())
                .or_default()
                .insert(intent.device, percent(intent.command)?);
            resources.extend(group.devices().into_iter().cloned());
        } else {
            if !matches!(module.kind(), ModuleKind::Switch | ModuleKind::Dimmer)
                || config.composed_resource(&intent.device).is_some()
                || config.shared_resource(&intent.device).is_some()
                || config
                    .devices()
                    .values()
                    .any(|d| d.enabled && d.supply_relay.as_ref() == Some(&intent.device))
            {
                return Err("joint raw target is not an independent light".into());
            }
            match (module.kind(), intent.command) {
                (ModuleKind::Switch, Command::Switch(_)) => {}
                (
                    ModuleKind::Dimmer,
                    Command::SetLevel {
                        transition: LevelTransition::Immediate,
                        ..
                    },
                ) => {}
                _ => return Err("invalid joint raw light command".into()),
            }
            resources.insert(intent.device.clone());
            if let Some(supply) = &device.supply_relay {
                resources.insert(supply.clone());
            }
            ordinary.push(intent);
        }
    }
    let (powers, levels, lease) = feedback.native_scene_capture(
        &resources.into_iter().collect::<Vec<_>>(),
        admission.monotonic_ms(),
        max_age_ms,
    )?;
    let powers = powers
        .into_iter()
        .filter_map(|(id, on)| on.map(|on| (id, on)))
        .collect::<BTreeMap<_, _>>();
    let mut physical = BTreeMap::new();
    for (id, targets) in composed {
        let group = config
            .composed_lights()
            .get(&id)
            .ok_or("joint composed group disappeared")?;
        merge(
            &mut physical,
            crate::composed_light::plan_joint(group, &targets, &powers, &levels)
                .map_err(Error::from)?,
        )?;
    }
    for (id, targets) in legacy {
        let group = config
            .shared_lights()
            .get(&id)
            .ok_or("joint shared group disappeared")?;
        let power = |id: &DeviceId| {
            powers
                .get(id)
                .copied()
                .ok_or("joint shared supply unavailable")
        };
        let level = |id: &DeviceId| {
            levels
                .get(id)
                .copied()
                .ok_or("joint shared dimmer unavailable")
        };
        let mut observed = homesense_core::shared_light::Observed {
            common_supply: power(&group.common_supply)?,
            left_supply: power(&group.left_supply)?,
            right_supply: power(&group.right_supply)?,
            primary_level: level(&group.primary)?,
            secondary_level: level(&group.secondary)?,
            reading_level: level(&group.reading)?,
        };
        // Both requested common lights are constrained together. The primary
        // retains the reviewed bedside-exit rule; the secondary preserves it.
        let mut writes = BTreeMap::new();
        for target in [&group.primary, &group.secondary] {
            if let Some(level) = targets.get(target) {
                let plan = homesense_core::shared_light::plan(
                    observed,
                    group.light(target).ok_or("invalid shared target")?,
                    *level,
                );
                observed = plan.expected;
                for (output, command) in plan.writes {
                    writes.insert(group.output(output).clone(), command);
                }
            }
        }
        merge(
            &mut physical,
            writes
                .into_iter()
                .map(|(device, command)| Intent { device, command })
                .collect(),
        )?;
    }
    for intent in ordinary {
        let (device, module) = config
            .binding(&intent.device)
            .ok_or("joint light disappeared")?;
        let writes = if let Some(supply) = &device.supply_relay {
            crate::combined_light::plan(intent.command, None, device.default_on_level)?
                .into_iter()
                .map(|(output, command)| Intent {
                    device: match output {
                        crate::combined_light::Output::Relay => supply.clone(),
                        crate::combined_light::Output::Dimmer => intent.device.clone(),
                    },
                    command,
                })
                .collect()
        } else {
            vec![Intent {
                device: intent.device,
                command: crate::light_command::plan(module, intent.command)?,
            }]
        };
        merge(&mut physical, writes)?;
    }
    if physical.is_empty() || physical.len() > 128 {
        return Err("joint expanded light bound invalid".into());
    }
    // Remove power first, set immediate levels, then energize supplies. This
    // ordering never treats a queue commit as a physical bus transaction.
    let mut physical = physical
        .into_iter()
        .map(|(device, command)| Intent { device, command })
        .collect::<Vec<_>>();
    physical.sort_by_key(|intent| match intent.command {
        Command::Switch(false) => 0,
        Command::SetLevel { .. } => 1,
        Command::Switch(true) => 2,
        _ => 3,
    });
    Ok(Prepared {
        controller,
        revision,
        physical,
        lease,
    })
}
