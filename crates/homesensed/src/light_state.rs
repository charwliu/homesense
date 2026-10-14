//! Publication policy; raw Feedback remains a physical measurement.
//! Confirmed steady lights use physical feedback. Historical setpoints are
//! diagnostic intent, not permanent display overrides.
use homesense_core::{DeviceId, ModuleKind, Observation, Percent};
use std::collections::BTreeMap;

pub fn project(
    configuration: &homesense_config::Configuration,
    physical: &homesense_state::Snapshot,
    recorded: &BTreeMap<DeviceId, Percent>,
) -> Result<homesense_state::Snapshot, &'static str> {
    // Kept for API compatibility: old persistent records do not prove an
    // active local command. Restore brightness is handled separately while Off.
    let _ = recorded;
    crate::combined_light::project(configuration, physical)
}

/// Trusted publisher overlay from the actor's bounded, lifecycle-checked targets.
pub(crate) fn project_pending(
    configuration: &homesense_config::Configuration,
    physical: &homesense_state::Snapshot,
    targets: &BTreeMap<DeviceId, Percent>,
) -> Result<homesense_state::Snapshot, &'static str> {
    let mut logical = project(configuration, physical, &BTreeMap::new())?;
    for (id, level) in targets {
        let Some(state) = logical.devices.get_mut(id) else {
            continue;
        };
        if state.kind != ModuleKind::Dimmer || state.observed.is_none() {
            continue;
        }
        let powered = configuration
            .devices()
            .get(id)
            .and_then(|d| d.supply_relay.as_ref())
            .and_then(|relay| physical.devices.get(relay))
            .and_then(|s| s.observed);
        state.observed = Some(Observation::Level(
            if powered == Some(Observation::Switch(false)) {
                Percent::new(0).unwrap()
            } else {
                *level
            },
        ));
    }
    Ok(logical)
}
