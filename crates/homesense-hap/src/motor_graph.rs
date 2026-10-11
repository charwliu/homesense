//! Offline motor action graph. No position is inferred and no command is sent.
//! The service must verify calibration and install its logical motor handler.
use crate::{
    graph::{information, install, resources},
    identifiers::{AccessoryDefinition, AccessoryKey, MotorOperation},
    storage::IdentityStore,
};
use hap::{Error, Result, accessory::switch::SwitchAccessory, characteristic::HapCharacteristic};
use homesense_core::DeviceId;
use std::collections::BTreeMap;
#[derive(Clone, Debug)]
pub struct MotorSpec {
    pub device: DeviceId,
    pub name: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MotorBinding {
    pub device: DeviceId,
    pub operation: MotorOperation,
    pub aid: u64,
    pub on_iid: u64,
}
pub struct MotorGraph {
    pub accessories: Vec<(MotorBinding, SwitchAccessory)>,
}
fn accessory(aid: u64, name: &str) -> Result<SwitchAccessory> {
    let mut switch = SwitchAccessory::new(aid, information(aid, name))?;
    switch.switch.name = None;
    Ok(switch)
}
impl MotorGraph {
    /// Each selected motor has three separate momentary action identities.
    /// Names and physical addresses never participate in persistent keys.
    pub async fn build(store: &IdentityStore, specs: Vec<MotorSpec>) -> Result<Self> {
        if specs.len() > 1365 {
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
        let mut entries = vec![];
        let mut definitions = vec![];
        for spec in selected.values() {
            for operation in [
                MotorOperation::Open,
                MotorOperation::Close,
                MotorOperation::Stop,
            ] {
                let name = format!("{} {}", spec.name, operation.label());
                let key = AccessoryKey::Motor(spec.device.clone(), operation);
                definitions.push(AccessoryDefinition {
                    key: key.clone(),
                    resources: resources(&accessory(2, &name)?)?,
                });
                entries.push((spec.device.clone(), operation, name, key));
            }
        }
        let ids = store.allocate_identifiers(definitions).await?;
        let mut accessories = vec![];
        for (device, operation, name, key) in entries {
            let ids = &ids[&key];
            let mut switch = accessory(ids.aid, &name)?;
            install(&mut switch, ids)?;
            let binding = MotorBinding {
                device,
                operation,
                aid: ids.aid,
                on_iid: switch.switch.power_state.get_id(),
            };
            accessories.push((binding, switch));
        }
        Ok(Self { accessories })
    }
}
