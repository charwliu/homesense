//! Native logical lights over a reviewed, bounded relay/dimmer matrix.
//! Scene programs keep their physical semantics. No guessed wiring is admitted.
use crate::{
    admission::{Admission, TaskGeneration},
    feedback::{Feedback, RuleLease},
    management_devices::DeviceAction,
};
use homesense_config::{
    Configuration,
    composed_light::{Group, Light, Member},
};
use homesense_core::{Command, DeviceId, LevelTransition, ModuleKind, Observation, Percent};
use homesense_hap::requests::{Batch, Intent};
use homesense_state::{Quality, Snapshot};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

#[cfg(test)]
mod tests;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Matrix {
    powers: BTreeMap<DeviceId, bool>,
    levels: BTreeMap<DeviceId, Percent>,
}
impl Matrix {
    fn lit(&self, m: &Member) -> bool {
        self.powers[&m.supply] && self.levels[&m.dimmer].get() > 0
    }
}

pub(crate) fn control_error(
    config: &Configuration,
    id: &DeviceId,
    physical: &Snapshot,
) -> Option<&'static str> {
    let (group, light) = config.composed_light(id)?;
    let mut observed = Matrix {
        powers: BTreeMap::new(),
        levels: BTreeMap::new(),
    };
    for resource in group.resources() {
        let Some(state) = physical
            .devices
            .get(resource)
            .filter(|s| s.quality == Quality::Confirmed)
        else {
            return Some("shared_feedback_unavailable");
        };
        match state.observed {
            Some(Observation::Switch(on)) => {
                observed.powers.insert(resource.clone(), on);
            }
            Some(Observation::Level(level)) => {
                observed.levels.insert(resource.clone(), level);
            }
            _ => return Some("shared_feedback_unavailable"),
        }
    }
    let level = if light.members.iter().any(|m| observed.lit(m)) {
        Percent::new(0).unwrap()
    } else {
        Percent::new(light.default_on_level).unwrap()
    };
    plan(group, light, &observed, level)
        .err()
        .map(|_| "shared_channel_in_use")
}

/// Preserve every other physical lamp product, except reviewed exclusions.
/// Shared dimmers necessarily share brightness. Impossible preservation is an
/// error, never a best-effort partial write. At most 8 relays x 4 dimmers.
fn plan(
    group: &Group,
    target: &Light,
    before: &Matrix,
    level: Percent,
) -> Result<Vec<Intent>, &'static str> {
    let rows = before.powers.keys().cloned().collect::<Vec<_>>();
    let columns = before.levels.keys().cloned().collect::<Vec<_>>();
    let target_columns = target
        .members
        .iter()
        .map(|m| &m.dimmer)
        .collect::<BTreeSet<_>>();
    let mut best: Option<(usize, Matrix)> = None;
    for row_bits in 0..(1usize << rows.len()) {
        for column_bits in 0..(1usize << columns.len()) {
            let candidate = Matrix {
                powers: rows
                    .iter()
                    .enumerate()
                    .map(|(i, id)| (id.clone(), row_bits & (1 << i) != 0))
                    .collect(),
                levels: columns
                    .iter()
                    .enumerate()
                    .map(|(i, id)| {
                        let value = if level.get() > 0 && target_columns.contains(id) {
                            level
                        } else if column_bits & (1 << i) != 0 {
                            before.levels[id]
                        } else {
                            Percent::new(0).unwrap()
                        };
                        (id.clone(), value)
                    })
                    .collect(),
            };
            let valid = group.lights.iter().all(|light| {
                if light.id == target.id {
                    return light
                        .members
                        .iter()
                        .all(|m| candidate.lit(m) == (level.get() > 0));
                }
                if level.get() > 0 && target.excludes_on.contains(&light.id) {
                    return light.members.iter().all(|m| !candidate.lit(m));
                }
                light.members.iter().all(|member| {
                    (candidate.lit(member) == before.lit(member)
                        || group.allow_shared_side_effects
                            && level.get() > 0
                            && !before.lit(member))
                        && (!before.lit(member)
                            || target_columns.contains(&member.dimmer)
                            || candidate.levels[&member.dimmer] == before.levels[&member.dimmer])
                })
            });
            if !valid {
                continue;
            }
            let collateral = group
                .lights
                .iter()
                .filter(|l| {
                    l.id != target.id && !(level.get() > 0 && target.excludes_on.contains(&l.id))
                })
                .flat_map(|l| l.members.iter())
                .filter(|m| before.lit(m) != candidate.lit(m))
                .count();
            let score = collateral * 100
                + rows
                    .iter()
                    .filter(|id| before.powers[*id] != candidate.powers[*id])
                    .count()
                + columns
                    .iter()
                    .filter(|id| before.levels[*id] != candidate.levels[*id])
                    .count()
                + candidate.powers.values().filter(|on| **on).count()
                + columns
                    .iter()
                    .filter(|id| {
                        candidate.levels[*id].get() > 0
                            && !group.lights.iter().any(|l| {
                                l.members
                                    .iter()
                                    .any(|m| &m.dimmer == *id && candidate.lit(m))
                            })
                    })
                    .count()
                    * 8;
            if best.as_ref().is_none_or(|(old, _)| score < *old) {
                best = Some((score, candidate));
            }
        }
    }
    let after = best
        .ok_or("requested logical lights cannot coexist on the reviewed wiring")?
        .1;
    let mut writes = Vec::new();
    // Remove conflicting power before changing a shared column. Energize rows last.
    for id in &rows {
        if before.powers[id] && !after.powers[id] {
            writes.push(Intent {
                device: id.clone(),
                command: Command::Switch(false),
            });
        }
    }
    for id in &columns {
        if before.levels[id] != after.levels[id] || level.get() > 0 && target_columns.contains(id) {
            writes.push(Intent {
                device: id.clone(),
                command: Command::SetLevel {
                    level: after.levels[id],
                    transition: LevelTransition::Immediate,
                },
            });
        }
    }
    for id in &rows {
        if !before.powers[id] && after.powers[id] {
            writes.push(Intent {
                device: id.clone(),
                command: Command::Switch(true),
            });
        }
    }
    if writes.is_empty() {
        let member = &target.members[0];
        let (device, command) = if !after.powers[&member.supply] {
            (member.supply.clone(), Command::Switch(false))
        } else {
            (
                member.dimmer.clone(),
                Command::SetLevel {
                    level: after.levels[&member.dimmer],
                    transition: LevelTransition::Immediate,
                },
            )
        };
        writes.push(Intent { device, command });
    }
    Ok(writes)
}

pub(crate) struct Prepared {
    controller: Uuid,
    revision: u64,
    device: DeviceId,
    action: DeviceAction,
    group: Group,
    observed: Matrix,
    level: Percent,
    lease: RuleLease,
}
impl Prepared {
    pub(crate) fn compile(
        self,
        config: &Configuration,
        source: Option<&crate::source_auth::Source>,
    ) -> Result<(Batch, RuleLease), hap::server::WriteStatus> {
        use hap::server::WriteStatus;
        let (group, light) = config
            .composed_light(&self.device)
            .ok_or(WriteStatus::InvalidValueInRequest)?;
        if self.revision != config.revision() || group != &self.group {
            return Err(WriteStatus::InvalidValueInRequest);
        }
        if let Some(source) = source {
            let action = crate::automation_control::Action::DeviceCommand {
                controller: self.controller,
                revision: self.revision,
                device: self.device.clone(),
                action: self.action,
            };
            if !source.matches(&action)
                || group
                    .resources()
                    .iter()
                    .any(|id| !source.permits_device(id.as_str()))
            {
                return Err(WriteStatus::InsufficientPrivileges);
            }
        }
        let intents = plan(group, light, &self.observed, self.level)
            .map_err(|_| WriteStatus::ResourceBusy)?;
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

pub(crate) async fn prepare(
    admission: &Admission,
    controller: Uuid,
    generation: TaskGeneration,
    id: &DeviceId,
    action: &DeviceAction,
    feedback: &Feedback,
    max_age_ms: u64,
) -> Result<Option<Prepared>, String> {
    let bundle = admission
        .automation_bundle(controller, generation)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let Some((group, light)) = bundle.configuration.composed_light(id) else {
        return Ok(None);
    };
    let saved = admission
        .remember_levels(
            bundle.configuration.revision(),
            feedback.snapshot(),
            max_age_ms,
        )
        .await
        .map_err(|e| format!("{e:?}"))?;
    let level = match action {
        DeviceAction::Power { on: false } => Percent::new(0).unwrap(),
        DeviceAction::Power { on: true } => saved
            .get(id)
            .copied()
            .filter(|n| n.get() > 0)
            .unwrap_or(Percent::new(light.default_on_level).unwrap()),
        DeviceAction::Brightness { level } => Percent::new(*level).map_err(|e| e.to_string())?,
        _ => return Err("composed lights accept power or brightness only".into()),
    };
    let ids = group.resources().into_iter().cloned().collect::<Vec<_>>();
    let (powers, levels, lease) =
        feedback.native_scene_capture(&ids, admission.monotonic_ms(), max_age_ms)?;
    let mut observed = Matrix {
        powers: BTreeMap::new(),
        levels: BTreeMap::new(),
    };
    for resource in &ids {
        let (_, module) = bundle
            .configuration
            .binding(resource)
            .ok_or("composed resource missing")?;
        match module.kind() {
            ModuleKind::Switch => {
                observed.powers.insert(
                    resource.clone(),
                    powers
                        .get(resource)
                        .copied()
                        .flatten()
                        .ok_or("composed supply feedback unavailable")?,
                );
            }
            ModuleKind::Dimmer => {
                observed.levels.insert(
                    resource.clone(),
                    *levels
                        .get(resource)
                        .ok_or("composed dimmer feedback unavailable")?,
                );
            }
            _ => return Err("invalid composed resource".into()),
        }
    }
    // Reject an impossible preservation request before admission as well.
    plan(group, light, &observed, level).map_err(str::to_owned)?;
    Ok(Some(Prepared {
        controller,
        revision: bundle.configuration.revision(),
        device: id.clone(),
        action: action.clone(),
        group: group.clone(),
        observed,
        level,
        lease,
    }))
}

pub(crate) fn project(
    config: &Configuration,
    physical: &Snapshot,
    logical: &mut Snapshot,
) -> Result<(), &'static str> {
    for group in config.composed_lights().values() {
        for light in &group.lights {
            let components = config.logical_components(&light.id);
            let states = components
                .iter()
                .map(|id| {
                    physical
                        .devices
                        .get(*id)
                        .ok_or("composed resource state missing")
                })
                .collect::<Result<Vec<_>, _>>()?;
            let state = logical
                .devices
                .get_mut(&light.id)
                .ok_or("composed logical state missing")?;
            state.kind = ModuleKind::Dimmer;
            state.version = states
                .iter()
                .map(|s| s.version)
                .max()
                .ok_or("empty composed state")?;
            state.observed_at_ms = states
                .iter()
                .map(|s| s.observed_at_ms)
                .collect::<Option<Vec<_>>>()
                .and_then(|v| v.into_iter().min());
            state.quality = if states.iter().any(|s| s.quality == Quality::Unknown) {
                Quality::Unknown
            } else if states.iter().any(|s| s.quality != Quality::Confirmed) {
                Quality::Stale
            } else {
                Quality::Confirmed
            };
            state.desired = None;
            state.pending_command = None;
            state.last_command_id = None;
            let mut observed = Some(Percent::new(0).unwrap());
            for member in &light.members {
                match (
                    physical.devices[&member.supply].observed,
                    physical.devices[&member.dimmer].observed,
                ) {
                    (Some(Observation::Switch(on)), Some(Observation::Level(level))) => {
                        if on
                            && let Some(current) = observed
                            && level.get() > current.get()
                        {
                            observed = Some(level);
                        }
                    }
                    _ => {
                        observed = None;
                        state.quality = Quality::Unknown;
                    }
                }
            }
            state.observed = observed.map(Observation::Level);
        }
    }
    Ok(())
}
