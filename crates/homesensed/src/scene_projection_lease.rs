//! Dependency-scoped validity for read-only scene projections. Command admission
//! continues to use the strict global RuleLease and its original mutation fence.
use super::{Feedback, RuleLease};
use crate::module_truth::ModuleTruth;
use homesense_config::Configuration;
use homesense_core::{DeviceId, ModuleKind, Observation};
use homesense_state::{DeviceState, Quality};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, MutexGuard},
};

#[derive(PartialEq, Eq)]
struct Signature {
    kind: ModuleKind,
    observed: Option<Observation>,
    quality: Quality,
    invalid: bool,
    never_observed: bool,
}
impl Signature {
    fn same_power(&self, other: &Self) -> bool {
        let power = |sample: Option<Observation>| match sample {
            Some(Observation::Switch(on)) => Some(on),
            Some(Observation::Level(level)) => Some(level.get() > 0),
            _ => None,
        };
        self.kind == other.kind
            && self.invalid == other.invalid
            && self.never_observed == other.never_observed
            && power(self.observed) == power(other.observed)
    }
    fn new(truth: &ModuleTruth, id: &DeviceId, state: &DeviceState) -> Self {
        Self {
            kind: state.kind,
            observed: state.observed,
            quality: state.quality,
            invalid: truth.invalid_alias(id),
            never_observed: truth.observation_count(id) == Some(0),
        }
    }
}
struct Entry {
    signature: Signature,
    version: u64,
    power_version: u64,
    semantic_version: u64,
    observed_ms: Option<u64>,
}
pub(super) struct Versions {
    boot: u128,
    revision: u64,
    epoch: u64,
    active: bool,
    entries: BTreeMap<DeviceId, Entry>,
    // An unbound port needs no cache bookkeeping; a bound port updates only its
    // alias. Avoid scanning every logical device for each polling response.
    aliases: BTreeMap<(String, u8, u8), DeviceId>,
}
impl Versions {
    pub(super) fn new(truth: &ModuleTruth, config: &Configuration, boot: u128, epoch: u64) -> Self {
        let entries = truth
            .snapshot()
            .devices
            .into_iter()
            .map(|(id, state)| {
                let entry = Entry {
                    signature: Signature::new(truth, &id, &state),
                    version: 0,
                    power_version: 0,
                    semantic_version: 0,
                    observed_ms: state.observed_at_ms,
                };
                (id, entry)
            })
            .collect::<BTreeMap<_, _>>();
        let aliases = entries
            .keys()
            .filter_map(|id| {
                config.binding(id).map(|(binding, _)| {
                    (
                        (
                            binding.gateway.clone(),
                            binding.address.get(),
                            binding.channel.get(),
                        ),
                        id.clone(),
                    )
                })
            })
            .collect();
        Self {
            boot,
            revision: config.revision(),
            epoch,
            active: true,
            entries,
            aliases,
        }
    }
    fn update(
        entry: &mut Entry,
        signature: Signature,
        observed_ms: Option<u64>,
    ) -> Result<(), String> {
        if entry.signature != signature {
            // Read-cache validity remains strict below. LED effect identity is
            // separate: aging/reconfirmation alone is not a new physical cause.
            if entry.signature.kind!=signature.kind || entry.signature.observed!=signature.observed
                || entry.signature.invalid!=signature.invalid || entry.signature.never_observed!=signature.never_observed {
                entry.semantic_version=entry.semantic_version.checked_add(1).ok_or("indicator semantic version exhausted")?;
            }
            if !entry.signature.same_power(&signature) {
                entry.power_version = entry
                    .power_version
                    .checked_add(1)
                    .ok_or("indicator power version exhausted")?;
            }
            entry.version = entry
                .version
                .checked_add(1)
                .ok_or("projection dependency version exhausted")?;
            entry.signature = signature;
        }
        // Equal-value replies do not revoke a proof or extend its captured
        // lifetime. Still reject a current sample from the future at read time.
        entry.observed_ms = observed_ms;
        Ok(())
    }
    pub(super) fn refresh(
        &mut self,
        truth: &ModuleTruth,
        epoch: u64,
        active: bool,
    ) -> Result<(), String> {
        self.active = false;
        self.epoch = epoch;
        for (id, entry) in &mut self.entries {
            let state = truth
                .alias_state(id)
                .ok_or("projection read mapping changed")?;
            Self::update(
                entry,
                Signature::new(truth, id, &state),
                state.observed_at_ms,
            )?;
        }
        self.active = active;
        Ok(())
    }
    pub(super) fn refresh_port(
        &mut self,
        truth: &ModuleTruth,
        gateway: &str,
        address: u8,
        channel: u8,
    ) -> Result<(), String> {
        let Some(id) = self.aliases.get(&(gateway.to_owned(), address, channel)) else {
            return Ok(());
        };
        let state = truth
            .alias_state(id)
            .ok_or("projection read mapping changed")?;
        let entry = self
            .entries
            .get_mut(id)
            .ok_or("projection read mapping changed")?;
        if let Err(error) = Self::update(
            entry,
            Signature::new(truth, id, &state),
            state.observed_at_ms,
        ) {
            self.active = false;
            return Err(error);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Fingerprint {
    boot: u128,
    revision: u64,
    epoch: u64,
    dependencies: BTreeMap<DeviceId, Option<u64>>,
}
pub(crate) struct ProjectionLease {
    rule: RuleLease,
    versions: Arc<Mutex<Versions>>,
    expected: BTreeMap<DeviceId, Option<u64>>,
    power_expected: BTreeMap<DeviceId, Option<u64>>,
    semantic_expected: BTreeMap<DeviceId, Option<u64>>,
}
impl ProjectionLease {
    pub(crate) fn fingerprint(&self, level_sensitive: bool) -> Fingerprint {
        Fingerprint {
            boot: self.rule.boot,
            revision: self.rule.revision,
            epoch: self.rule.epoch,
            dependencies: if level_sensitive {
                self.semantic_expected.clone()
            } else {
                self.power_expected.clone()
            },
        }
    }

    pub(super) fn capture(feedback: &Feedback, lease: &RuleLease) -> Result<Self, String> {
        let _guard = lease.lock(
            feedback.boot,
            feedback.revision,
            feedback.epoch,
            lease.captured_ms,
        )?;
        let versions = feedback
            .projection_versions
            .lock()
            .map_err(|_| "projection dependency lease poisoned")?;
        let power_expected = lease
            .observations
            .keys()
            .map(|id| {
                (
                    id.clone(),
                    versions.entries.get(id).map(|entry| entry.power_version),
                )
            })
            .collect();
        let semantic_expected=lease.observations.keys().map(|id|(
            id.clone(),versions.entries.get(id).map(|entry|entry.semantic_version)
        )).collect();
        let expected = lease
            .observations
            .keys()
            .map(|id| {
                (
                    id.clone(),
                    versions.entries.get(id).map(|entry| entry.version),
                )
            })
            .collect();
        Ok(Self {
            rule: lease.clone(),
            versions: feedback.projection_versions.clone(),
            expected,
            power_expected,
            semantic_expected,
        })
    }
    pub(crate) fn lock(
        &self,
        boot: u128,
        revision: u64,
        epoch: u64,
        now: u64,
    ) -> Result<MutexGuard<'_, u64>, String> {
        // Physical mutations and admission serialize with the same global lock;
        // only this read-only cache ignores unrelated global version increments.
        let guard = self
            .rule
            .version
            .lock()
            .map_err(|_| "feedback lease poisoned")?;
        let versions = self
            .versions
            .lock()
            .map_err(|_| "projection dependency lease poisoned")?;
        if !versions.active
            || versions.boot != boot
            || versions.revision != revision
            || versions.epoch != epoch
            || self.rule.boot != boot
            || self.rule.revision != revision
            || self.rule.epoch != epoch
            || now < self.rule.captured_ms
            // A read-only projection expires with its captured physical
            // sample, not the shorter deadline for submitting an action.
            || self.rule.sample_expires_ms.is_some_and(|at| now >= at)
            || self.expected.iter().any(|(id, expected)| {
                let current = versions.entries.get(id);
                current.map(|entry| entry.version) != *expected
                    || current.is_some_and(|entry| entry.observed_ms.is_some_and(|at| at > now))
            })
        {
            return Err("projection dependency expired or changed".into());
        }
        Ok(guard)
    }
}
