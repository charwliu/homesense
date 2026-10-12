//! Publication policy; raw Feedback remains a physical measurement.
//! Switches use measured state. Dimmers retain the last successfully written
//! HomeSense setting. Before a local setting exists, use measured bootstrap.
use homesense_core::{DeviceId, ModuleKind, Observation, Percent};
use std::collections::BTreeMap;

pub fn project(
    configuration: &homesense_config::Configuration,
    physical: &homesense_state::Snapshot,
    recorded: &BTreeMap<DeviceId, Percent>,
) -> Result<homesense_state::Snapshot, &'static str> {
    let mut logical = crate::combined_light::project(configuration, physical)?;
    for (id, level) in recorded {
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
            .map(|relay| physical.devices.get(relay).and_then(|s| s.observed));
        // Separate power relays are switches: their actual off state wins.
        state.observed = Some(Observation::Level(
            if powered == Some(Some(Observation::Switch(false))) {
                Percent::new(0).unwrap()
            } else {
                *level
            },
        ));
    }
    Ok(logical)
}
