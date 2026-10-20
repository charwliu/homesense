//! One native HAP window covering per selected calibrated motor. This only
//! constructs the graph; runtime command and position ownership stay separate.
use crate::{
    graph::{information, install, resources},
    identifiers::{AccessoryDefinition, AccessoryKey},
    motor_graph::MotorSpec,
    storage::IdentityStore,
};
use hap::{
    Error, Result, accessory::window_covering::WindowCoveringAccessory,
    characteristic::HapCharacteristic,
};
use homesense_core::DeviceId;
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowCoveringBinding {
    pub device: DeviceId,
    pub aid: u64,
    pub current_iid: u64,
    pub state_iid: u64,
    pub target_iid: u64,
    pub hold_iid: u64,
}

pub struct WindowCoveringGraph {
    pub accessories: Vec<(WindowCoveringBinding, WindowCoveringAccessory)>,
}

fn accessory(aid: u64, name: &str) -> Result<WindowCoveringAccessory> {
    let mut covering = WindowCoveringAccessory::new(aid, information(aid, name))?;
    covering.window_covering.name = None;
    covering.window_covering.current_horizontal_tilt_angle = None;
    covering.window_covering.target_horizontal_tilt_angle = None;
    covering.window_covering.current_vertical_tilt_angle = None;
    covering.window_covering.target_vertical_tilt_angle = None;
    covering.window_covering.obstruction_detected = None;
    Ok(covering)
}

impl WindowCoveringGraph {
    pub async fn build(store: &IdentityStore, specs: Vec<MotorSpec>) -> Result<Self> {
        if specs.len() > 4095 {
            return Err(Error::Storage);
        }
        let mut selected = BTreeMap::new();
        for spec in specs {
            if spec.name.trim().is_empty()
                || spec.name.len() > 58
                || spec.name.chars().any(char::is_control)
                || selected.insert(spec.device.clone(), spec).is_some()
            {
                return Err(Error::Storage);
            }
        }
        if selected.is_empty() {
            return Ok(Self {
                accessories: vec![],
            });
        }
        let definitions = selected
            .values()
            .map(|spec| {
                Ok(AccessoryDefinition {
                    key: AccessoryKey::WindowCovering(spec.device.clone()),
                    resources: resources(&accessory(2, &spec.name)?)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let ids = store.allocate_identifiers(definitions).await?;
        let mut accessories = Vec::with_capacity(selected.len());
        for spec in selected.values() {
            let key = AccessoryKey::WindowCovering(spec.device.clone());
            let ids = &ids[&key];
            let mut covering = accessory(ids.aid, &spec.name)?;
            install(&mut covering, ids)?;
            let service = &covering.window_covering;
            let binding = WindowCoveringBinding {
                device: spec.device.clone(),
                aid: ids.aid,
                current_iid: service.current_position.get_id(),
                state_iid: service.position_state.get_id(),
                target_iid: service.target_position.get_id(),
                hold_iid: service
                    .hold_position
                    .as_ref()
                    .ok_or(Error::Storage)?
                    .get_id(),
            };
            accessories.push((binding, covering));
        }
        Ok(Self { accessories })
    }
}
