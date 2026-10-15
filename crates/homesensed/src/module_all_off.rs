//! Pure planning for an explicitly requested Away operation; never a scene
//! optimizer. No admission, persistence, queue reservation or transport I/O.
use homesense_config::Configuration;
use homesense_core::{DeviceId, ExecutionContext, ModuleSpec};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    EmptyRequest,
    StableModuleIdsRequired,
    InvalidReview,
    DuplicateModule,
    UnknownModule,
    UnsupportedProfile,
    UnverifiedModule,
    IncompleteCoverage,
    InvalidLightingBinding,
    Unauthorized,
    ConfigurationChanged,
}

/// Trusted operator-reviewed policy, not a payload supplied by an API client.
/// Every physical output must be positively classified as a lighting output;
/// unknown, unbound or non-lighting ports cannot be omitted to widen scope.
#[derive(Clone, Debug)]
pub struct LightingReview {
    module_id: String,
    devices: BTreeSet<DeviceId>,
    evidence: String,
}
impl LightingReview {
    pub fn new(module_id: String, devices: Vec<DeviceId>, evidence: String) -> Result<Self, Error> {
        if module_id.trim().is_empty() || evidence.trim().is_empty() || evidence.len() > 4096 {
            return Err(Error::InvalidReview);
        }
        let count = devices.len();
        let devices: BTreeSet<_> = devices.into_iter().collect();
        if devices.len() != count || devices.is_empty() {
            return Err(Error::InvalidReview);
        }
        Ok(Self {
            module_id,
            devices,
            evidence,
        })
    }
    pub fn evidence(&self) -> &str {
        &self.evidence
    }
}

#[derive(Clone, Debug)]
pub struct ModuleOff {
    module_id: String,
    gateway_id: String,
    module: ModuleSpec,
    devices: Vec<DeviceId>,
}
impl ModuleOff {
    pub fn module_id(&self) -> &str {
        &self.module_id
    }
    pub fn gateway_id(&self) -> &str {
        &self.gateway_id
    }
    pub fn module(&self) -> &ModuleSpec {
        &self.module
    }
    pub fn devices(&self) -> &[DeviceId] {
        &self.devices
    }
}

/// A revision-bound proposal, not a dispatch capability. A future runtime
/// adapter must atomically lock all affected resources, durably accept one
/// physical module action and its per-device effects, then revalidate before
/// Writing. Written must never substitute for actual channel feedback.
#[derive(Clone, Debug)]
pub struct Plan {
    revision: u64,
    modules: Vec<ModuleOff>,
    reviews: Vec<LightingReview>,
}
impl Plan {
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn modules(&self) -> &[ModuleOff] {
        &self.modules
    }
    pub fn revalidate(
        &self,
        config: &Configuration,
        authorized: &BTreeSet<DeviceId>,
        context: ExecutionContext,
    ) -> Result<(), Error> {
        if config.revision() != self.revision {
            return Err(Error::ConfigurationChanged);
        }
        let next = plan(config, &self.reviews, authorized, context)?;
        if next.modules.len() != self.modules.len()
            || next.modules.iter().zip(&self.modules).any(|(a, b)| {
                a.module_id != b.module_id
                    || a.gateway_id != b.gateway_id
                    || a.module != b.module
                    || a.devices != b.devices
            })
        {
            return Err(Error::ConfigurationChanged);
        }
        Ok(())
    }
}

/// Only the explicit Away entry should call this. Ordinary HAP Off, timed
/// scenes and source programs retain their original node order and semantics.
/// The authenticated adapter supplies authorized devices and trusted reviews;
/// this helper cannot authorize I/O and does not mutate selection or feedback.
pub fn plan(
    config: &Configuration,
    reviews: &[LightingReview],
    authorized: &BTreeSet<DeviceId>,
    context: ExecutionContext,
) -> Result<Plan, Error> {
    if reviews.is_empty() {
        return Err(Error::EmptyRequest);
    }
    if config.schema_version() != 2 {
        return Err(Error::StableModuleIdsRequired);
    }
    let mut modules = BTreeMap::new();
    for review in reviews {
        if modules.contains_key(&review.module_id) {
            return Err(Error::DuplicateModule);
        }
        let (location, module) = config
            .module(&review.module_id)
            .ok_or(Error::UnknownModule)?;
        if !hb_protocol::supports_module_all_off(module.profile()) {
            return Err(Error::UnsupportedProfile);
        }
        module
            .validate_context(context)
            .map_err(|_| Error::UnverifiedModule)?;
        if review.devices.len() != usize::from(module.channels()) {
            return Err(Error::IncompleteCoverage);
        }
        let mut ports = BTreeMap::new();
        for id in &review.devices {
            let (binding, bound_module) =
                config.binding(id).ok_or(Error::InvalidLightingBinding)?;
            if binding.module_id.as_deref() != Some(review.module_id.as_str())
                || binding.gateway != location.gateway
                || binding.address != location.address
                || bound_module != module
            {
                return Err(Error::InvalidLightingBinding);
            }
            if !authorized.contains(id) {
                return Err(Error::Unauthorized);
            }
            if ports.insert(binding.channel.get(), id.clone()).is_some() {
                return Err(Error::IncompleteCoverage);
            }
        }
        if ports.keys().copied().ne(1..=module.channels()) {
            return Err(Error::IncompleteCoverage);
        }
        // Reject even a disabled or alternate logical binding outside the
        // reviewed set: whole-module Off still physically affects that port.
        if config.logical_devices().iter().any(|(id, b)| {
            b.module_id.as_deref() == Some(review.module_id.as_str())
                && !review.devices.contains(id)
        }) {
            return Err(Error::InvalidLightingBinding);
        }
        modules.insert(
            review.module_id.clone(),
            ModuleOff {
                module_id: review.module_id.clone(),
                gateway_id: location.gateway.clone(),
                module: module.clone(),
                devices: ports.into_values().collect(),
            },
        );
    }
    Ok(Plan {
        revision: config.revision(),
        modules: modules.into_values().collect(),
        reviews: reviews.to_vec(),
    })
}
