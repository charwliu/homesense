//! Explicit, reviewed latching scene groups. Zero means no local selection;
//! unknown remains unknown unless an explicit physical initialization policy is enabled.
use crate::{Error, invalid, rule_document::TimedScene};
use homesense_core::{Command, DeviceId, ModuleKind, rule::{Lifetime, ValueType, Variable}};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Member {
    pub scene: String,
    pub value: i64,
    pub off_scene: String,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Group {
    pub id: String,
    pub variable: String,
    pub review_evidence: String,
    pub members: Vec<Member>,
    /// Reviewed source values that select no member without rewriting history.
    #[serde(default)]
    pub unselected_values:Vec<i64>,
    #[serde(default)]
    pub reset_scenes: Vec<String>,
    #[serde(default)]
    pub initialization: Option<Initialization>,
    #[serde(default)]
    pub reconciliation: Option<Reconciliation>,
}
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReconciliationPolicy { UniqueLightOnPattern, PowerPatternThenProfileLevel }
#[derive(Debug, Default, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OmittedTargets { #[default] Off, Preserve }
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reconciliation {
    #[serde(default)]
    pub enabled: bool,
    pub policy: ReconciliationPolicy,
    pub max_age_ms: u64,
    pub review_evidence: String,
    /// Opt-in only: preserve each source scene's actuator scope. Observation
    /// proofs still require the complete reviewed union of all member lights.
    #[serde(default)]
    pub omitted_targets: OmittedTargets,
}
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InitializationPolicy { UniqueLightTargets }
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Initialization {
    #[serde(default)]
    pub enabled: bool,
    pub policy: InitializationPolicy,
    pub max_age_ms: u64,
    pub review_evidence: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LightTarget { Switch(bool), Level(u8) }
impl LightTarget {
    pub fn is_off(self) -> bool { matches!(self, Self::Switch(false) | Self::Level(0)) }
}
pub type LightTargets = BTreeMap<i64, BTreeMap<DeviceId, LightTarget>>;
impl Group {
    pub fn pattern_scope(&self) -> homesense_core::scene_pattern::PatternScope {
        if self.reconciliation.as_ref().is_some_and(|p| p.enabled && p.omitted_targets == OmittedTargets::Preserve) {
            homesense_core::scene_pattern::PatternScope::PreserveOmissions
        } else { homesense_core::scene_pattern::PatternScope::Complete }
    }
    /// Absolute logical targets for relative action planning. Source omissions
    /// default to Off within the union; an explicit reviewed policy preserves them.
    pub fn absolute_light_targets(&self, scenes:&BTreeMap<String,TimedScene>)->Result<BTreeMap<i64,BTreeMap<DeviceId,Command>>,Error> {
        let mut result=BTreeMap::new();let mut off=BTreeMap::new();
        for member in &self.members {
            let mut targets=BTreeMap::new();let mut steps=scenes.get(&member.scene).ok_or_else(||invalid("relative scene missing"))?.steps.iter().enumerate().collect::<Vec<_>>();
            steps.sort_by_key(|(index,step)|(step.delay_ms,*index));
            for (_,step) in steps {
                let negative=match step.command {
                    Command::Switch(_)=>Command::Switch(false),
                    Command::SetLevel{..}=>Command::SetLevel{level:homesense_core::Percent::new(0).unwrap(),transition:homesense_core::LevelTransition::Immediate},
                    Command::PanelLed(_)=>continue,
                    _=>return Err(invalid("relative scene needs absolute light targets")),
                };
                if off.insert(step.device.clone(),negative).is_some_and(|previous|previous!=negative) {return Err(invalid("relative light type mismatch"));}
                targets.insert(step.device.clone(),step.command);
            }
            result.insert(member.value,targets);
        }
        if off.is_empty() || off.len()>128 {return Err(invalid("relative light capacity invalid"));}
        if self.pattern_scope() == homesense_core::scene_pattern::PatternScope::PreserveOmissions && result.values().any(BTreeMap::is_empty) {
            return Err(invalid("preserved scene scope needs light targets in every member"));
        }
        if self.pattern_scope() == homesense_core::scene_pattern::PatternScope::Complete {
            for targets in result.values_mut(){for (id,command) in &off {targets.entry(id.clone()).or_insert(*command);}}
        }
        Ok(result)
    }
    /// Reviewed member targets. Omissions default to Off, or stay absent under
    /// the opt-in policy. Missing physical observations always remain unknown.
    pub fn light_on_patterns(&self, scenes: &BTreeMap<String, TimedScene>) -> Result<homesense_core::scene_pattern::MemberPatterns, Error> {
        let mut patterns = BTreeMap::new();
        let mut union = BTreeSet::new();
        for member in &self.members {
            let mut targets = BTreeMap::new();
            let mut steps = scenes.get(&member.scene).ok_or_else(|| invalid("reconciliation scene missing"))?.steps.iter().enumerate().collect::<Vec<_>>();
            steps.sort_by_key(|(index, step)| (step.delay_ms, *index));
            for (_, step) in steps {
                let power = match step.command {
                    Command::Switch(on) => on.into(),
                    Command::SetLevel { level, .. } => level.into(),
                    Command::PanelLed(_) => continue,
                    _ => return Err(invalid("reconciliation needs absolute light targets")),
                };
                targets.insert(step.device.clone(), power);
            }
            union.extend(targets.keys().cloned());
            patterns.insert(member.value, targets);
        }
        if union.is_empty() || union.len() > 128 { return Err(invalid("reconciliation light capacity invalid")); }
        if self.pattern_scope() == homesense_core::scene_pattern::PatternScope::Complete {
            for targets in patterns.values_mut() {
                for id in &union { targets.entry(id.clone()).or_insert(homesense_core::scene_pattern::LightPower::Off); }
            }
        }
        Ok(patterns)
    }
    /// Final logical dimmer targets under the reviewed omission policy. These
    /// are percentages, not raw Control4 source/rate units.
    pub fn profile_level_patterns(&self,scenes:&BTreeMap<String,TimedScene>)->Result<homesense_core::scene_pattern::MemberLevels,Error> {
        Ok(self.absolute_light_targets(scenes)?.into_iter().map(|(value,targets)|(value,targets.into_iter().filter_map(|(id,command)|match command {Command::SetLevel{level,..}=>Some((id,level)),_=>None}).collect())).collect())
    }
    /// Exact final targets in source time/order. Missing targets are not Off.
    pub fn light_targets(&self, scenes: &BTreeMap<String, TimedScene>) -> Result<LightTargets, Error> {
        let mut result = BTreeMap::new();
        for member in &self.members {
            let mut targets = BTreeMap::new();
            let mut steps = scenes.get(&member.scene).ok_or_else(|| invalid("initialization scene missing"))?.steps.iter().enumerate().collect::<Vec<_>>();
            steps.sort_by_key(|(index, step)| (step.delay_ms, *index));
            for (_, step) in steps {
                let value = match step.command {
                    Command::Switch(on) => LightTarget::Switch(on),
                    Command::SetLevel { level, .. } => LightTarget::Level(level.get()),
                    Command::PanelLed(_) => continue,
                    _ => return Err(invalid("initialization needs absolute light targets")),
                };
                targets.insert(step.device.clone(), value);
            }
            if targets.is_empty() || targets.len() > 128 { return Err(invalid("initialization light capacity invalid")); }
            result.insert(member.value, targets);
        }
        let first = result.values().next().ok_or_else(|| invalid("initialization members missing"))?;
        if result.values().any(|targets| targets.keys().ne(first.keys()) || targets.iter().any(|(id, target)| std::mem::discriminant(target) != std::mem::discriminant(&first[id]))) {
            return Err(invalid("initialization requires complete consistent light vectors"));
        }
        Ok(result)
    }
}
pub(crate) fn validate(
    groups: &[Group],
    variables: &BTreeMap<String, Variable>,
    scenes: &BTreeMap<String, TimedScene>,
    config: &crate::Configuration,
) -> Result<(), Error> {
    let identifier =
        |s: &str| !s.trim().is_empty() && s.len() <= 250 && !s.chars().any(char::is_control);
    if groups.len() > 32 {
        return Err(invalid("scene switch group capacity exceeded"));
    }
    let mut ids = BTreeSet::new();
    let mut vars = BTreeSet::new();
    let mut members = BTreeSet::new();
    let mut off = BTreeSet::new();
    for group in groups {
        if !identifier(&group.id)
            || !identifier(&group.variable)
            || !ids.insert(&group.id)
            || !vars.insert(&group.variable)
            || group.review_evidence.trim().is_empty()
            || group.review_evidence.len() > 8192
            || !(1..=64).contains(&group.members.len())
            || group.reset_scenes.len() > 64
        {
            return Err(invalid("invalid or unreviewed scene switch group"));
        }
        if !variables.get(&group.variable).is_some_and(|v| {
            v.lifetime == Lifetime::Persistent && v.value_type == ValueType::Integer
        }) {
            return Err(invalid(
                "scene switch selection requires a persistent integer variable",
            ));
        }
        let reviewed = |id: &str| {
            identifier(id)
                && scenes.get(id).is_some_and(|s| {
                    s.enabled
                        && s.review_evidence
                            .as_ref()
                            .is_some_and(|v| !v.trim().is_empty())
                })
        };
        let mut values = BTreeSet::new();
        for member in &group.members {
            if member.value <= 0
                || !values.insert(member.value)
                || !members.insert(&member.scene)
                || !reviewed(&member.scene)
                || !reviewed(&member.off_scene)
            {
                return Err(invalid("invalid scene switch member or shutdown scene"));
            }
            off.insert(&member.off_scene);
        }
        let unselected=group.unselected_values.iter().copied().collect::<BTreeSet<_>>();
        if unselected.len()!=group.unselected_values.len()||unselected.len()>64||unselected.iter().any(|v|*v<=0||values.contains(v)){return Err(invalid("invalid unselected scene source values"));}
        let mut resets = BTreeSet::new();
        for id in &group.reset_scenes {
            if !reviewed(id) || !resets.insert(id) {
                return Err(invalid("invalid scene switch reset scene"));
            }
            off.insert(id);
        }
        if let Some(policy) = &group.initialization {
            if !(1..=super::RefreshPolicy::MAX_OBSERVATION_AGE_MS).contains(&policy.max_age_ms) || policy.review_evidence.len() > 8192 || (policy.enabled && policy.review_evidence.trim().is_empty()) {
                return Err(invalid("invalid or unreviewed scene initialization"));
            }
            if policy.enabled {
                for targets in group.light_targets(scenes)?.values() {
                    for (id, target) in targets {
                        let (_, module) = config.binding(id).ok_or_else(|| invalid("initialization light disabled"))?;
                        if !matches!((target,module.kind()), (LightTarget::Switch(_),ModuleKind::Switch) | (LightTarget::Level(_),ModuleKind::Dimmer)) {
                            return Err(invalid("initialization target is not an absolute supported light value"));
                        }
                    }
                }
            }
        }
        if let Some(policy) = &group.reconciliation {
            if !(1..=super::RefreshPolicy::MAX_OBSERVATION_AGE_MS).contains(&policy.max_age_ms) || policy.review_evidence.len()>8192 || (policy.enabled && policy.review_evidence.trim().is_empty()) {
                return Err(invalid("invalid or unreviewed scene reconciliation"));
            }
            if policy.enabled {
                if group.initialization.as_ref().is_some_and(|p|p.enabled) { return Err(invalid("initialization and reconciliation policies conflict")); }
                for targets in group.light_on_patterns(scenes)?.values() {
                    for id in targets.keys() {
                        let (_, module) = config.binding(id).ok_or_else(|| invalid("reconciliation light disabled"))?;
                        if !matches!(module.kind(),ModuleKind::Switch|ModuleKind::Dimmer) { return Err(invalid("reconciliation needs queryable lights")); }
                    }
                }
                if policy.policy==ReconciliationPolicy::PowerPatternThenProfileLevel {
                    let powers=group.light_on_patterns(scenes)?;
                    let levels=group.profile_level_patterns(scenes)?;
                    if group.pattern_scope()==homesense_core::scene_pattern::PatternScope::PreserveOmissions {
                        // Sparse candidates can overlap only for some complete
                        // union observations. Require an established level domain
                        // for every possible explicit discriminator, not just a
                        // partially sampled member vector.
                        for id in levels.values().flat_map(|v|v.keys()) {
                            let (_,module)=config.binding(id).ok_or_else(||invalid("brightness discriminator disabled"))?;
                            if module.profile()!=homesense_core::ProtocolProfile::Ad8Din {return Err(invalid("brightness discriminator requires established AD8 percentage readback"));}
                        }
                    }
                    for pattern in powers.values() {
                        let actual=pattern.iter().map(|(id,power)|(id.clone(),Some(*power))).collect();
                        if let Some(reads)=homesense_core::scene_pattern::brightness_reads_for_collision_scoped(&powers,&levels,&actual,group.pattern_scope()) {
                            for id in reads {
                                let (_,module)=config.binding(&id).ok_or_else(||invalid("brightness discriminator disabled"))?;
                                // This opt-in starts with the established AD8 0..100
                                // readback domain. Other profiles require their own
                                // reviewed conversion; never assume a tolerance.
                                if module.profile()!=homesense_core::ProtocolProfile::Ad8Din {return Err(invalid("brightness discriminator requires established AD8 percentage readback"));}
                            }
                        }
                    }
                }
                for member in &group.members {
                    for step in &scenes[&member.scene].steps {
                        if matches!(step.command,Command::PanelLed(_)) { continue; }
                        let (_, module)=config.binding(&step.device).ok_or_else(||invalid("reconciliation target disabled"))?;
                        if !matches!((step.command,module.kind()),(Command::Switch(_),ModuleKind::Switch)|(Command::SetLevel{..},ModuleKind::Dimmer)) { return Err(invalid("reconciliation target type mismatch")); }
                    }
                }
            }
        }
    }
    if members.len() > 512 || members.iter().any(|id| off.contains(id)) {
        return Err(invalid("ambiguous scene switch action identity"));
    }
    Ok(())
}
