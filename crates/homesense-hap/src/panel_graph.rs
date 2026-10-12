//! Offline, opt-in TS6 panel key graph. No device writes or event replay.
//! The service owner must validate the physical layout and route live gestures.
use crate::{
    graph::{information, install, resources},
    identifiers::{AccessoryDefinition, AccessoryKey},
    storage::IdentityStore,
};
use hap::{
    Error, Result, accessory::stateless_programmable_switch::StatelessProgrammableSwitchAccessory,
    characteristic::HapCharacteristic,
};
use homesense_core::{DeviceId, gesture::Event};
use std::collections::BTreeMap;

#[derive(Clone, Debug)]
pub struct PanelKeySpec {
    pub device: DeviceId,
    /// One-based key number, validated against the actual panel by the owner.
    pub key: u8,
    pub name: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PanelKeyBinding {
    pub device: DeviceId,
    pub key: u8,
    pub aid: u64,
    pub event_iid: u64,
}
pub struct PanelGraph {
    pub keys: BTreeMap<(DeviceId, u8), StatelessProgrammableSwitchAccessory>,
    pub bindings: Vec<PanelKeyBinding>,
}

/// Convert completed gestures only. A press/release pair is not two clicks,
/// and ending/cancelling a hold must not emit another long-press notification.
pub fn switch_event(event: Event) -> Option<u8> {
    match event {
        Event::Click => Some(0),
        Event::DoubleClick => Some(1),
        Event::LongStart => Some(2),
        Event::Pressed | Event::Released | Event::LongEnd | Event::Cancelled => None,
    }
}

fn accessory(aid: u64, name: &str) -> Result<StatelessProgrammableSwitchAccessory> {
    let mut accessory = StatelessProgrammableSwitchAccessory::new(aid, information(aid, name))?;
    accessory.stateless_programmable_switch.name = None;
    accessory.stateless_programmable_switch.label_index = None;
    Ok(accessory)
}

impl PanelGraph {
    /// Each selected key owns a stable accessory. Unselected keys, LEDs and
    /// internal variables are absent. Empty selection allocates nothing.
    pub async fn build(store: &IdentityStore, specs: Vec<PanelKeySpec>) -> Result<Self> {
        if specs.len() > 4095 {
            return Err(Error::Storage);
        }
        let mut sorted = BTreeMap::new();
        for spec in specs {
            if !(1..=6).contains(&spec.key)
                || spec.device.as_str().chars().any(char::is_control)
                || spec.name.trim().is_empty()
                || spec.name.len() > 64
                || spec.name.chars().any(char::is_control)
                || sorted
                    .insert((spec.device.clone(), spec.key), spec)
                    .is_some()
            {
                return Err(Error::Storage);
            }
        }
        let mut keys = BTreeMap::new();
        let mut bindings = Vec::new();
        if sorted.is_empty() {
            return Ok(Self { keys, bindings });
        }
        let definitions = sorted
            .values()
            .map(|spec| {
                Ok(AccessoryDefinition {
                    key: AccessoryKey::PanelKey(spec.device.clone(), spec.key),
                    resources: resources(&accessory(2, &spec.name)?)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let identifiers = store.allocate_identifiers(definitions).await?;
        for ((device, key), spec) in sorted {
            let ids = &identifiers[&AccessoryKey::PanelKey(device.clone(), key)];
            let mut button = accessory(ids.aid, &spec.name)?;
            install(&mut button, ids)?;
            bindings.push(PanelKeyBinding {
                device: device.clone(),
                key,
                aid: ids.aid,
                event_iid: button
                    .stateless_programmable_switch
                    .programmable_switch_event
                    .get_id(),
            });
            keys.insert((device, key), button);
        }
        Ok(Self { keys, bindings })
    }
}
