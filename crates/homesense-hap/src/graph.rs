//! Offline construction of the bridge's basic light graph. No listener, pairing,
//! callbacks or physical transport is started by this module.
use crate::{
    identifiers::{AccessoryDefinition, AccessoryIds, AccessoryKey},
    requests::LightBinding,
    storage::IdentityStore,
};
use hap::{
    Error, HapType, Result,
    accessory::{
        AccessoryInformation, HapAccessory, bridge::BridgeAccessory, lightbulb::LightbulbAccessory,
        fan::FanAccessory,
    },
    characteristic::HapCharacteristic,
};
use homesense_core::{DeviceId, Percent};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone)]
pub struct LightSpec {
    pub device: DeviceId,
    pub name: String,
    pub dimmable: bool,
    pub default_on_level: Option<Percent>,
}
pub struct LightGraph {
    pub bridge: BridgeAccessory,
    pub lights: BTreeMap<DeviceId, LightbulbAccessory>,
    pub fans: BTreeMap<DeviceId, FanAccessory>,
    pub bindings: Vec<LightBinding>,
}
pub(crate) fn information(aid: u64, name: &str) -> AccessoryInformation {
    AccessoryInformation {
        manufacturer: "HomeSense".into(),
        // HMAccessory.model is public on supported iOS versions; the serial
        // characteristic is no longer exposed to ordinary HomeKit clients.
        // Keep AID/IID/serial and pairing identity unchanged while providing a
        // bridge-scoped key that the native room-placement client can read.
        model: if aid==1 {"HomeSense".into()}else{format!("HomeSense-AID:{aid}")},
        name: name.into(),
        serial_number: format!("HS-{aid}"),
        firmware_revision: Some(env!("CARGO_PKG_VERSION").into()),
        ..Default::default()
    }
}
fn light(aid: u64, spec: &LightSpec) -> Result<LightbulbAccessory> {
    let mut light = LightbulbAccessory::new(aid, information(aid, &spec.name))?;
    // Only expose capabilities represented by our command model.
    light.lightbulb.hue = None;
    light.lightbulb.saturation = None;
    light.lightbulb.color_temperature = None;
    light.lightbulb.name = None;
    if !spec.dimmable {
        light.lightbulb.brightness = None;
    }
    Ok(light)
}
fn fan(aid: u64, spec: &LightSpec) -> Result<FanAccessory> {
    let mut fan = FanAccessory::new(aid, information(aid, &spec.name))?;
    // Relay-driven exhaust fans expose On only, never speed or direction.
    fan.fan.name = None;
    fan.fan.rotation_direction = None;
    fan.fan.rotation_speed = None;
    Ok(fan)
}
fn type_key(kind: HapType) -> Result<String> {
    // The wire UUID, never Debug output or generated enumeration order.
    serde_json::to_value(kind)?
        .as_str()
        .map(str::to_owned)
        .ok_or(Error::Storage)
}
pub(crate) fn resources(accessory: &dyn HapAccessory) -> Result<Vec<String>> {
    let mut result = Vec::new();
    for service in accessory.get_services() {
        let key = format!("service:{}", type_key(service.get_type())?);
        result.push(key.clone());
        for characteristic in service.get_characteristics() {
            result.push(format!(
                "{key}/characteristic:{}",
                type_key(characteristic.get_type())?
            ));
        }
    }
    Ok(result)
}
pub(crate) fn install(accessory: &mut dyn HapAccessory, ids: &AccessoryIds) -> Result<()> {
    if accessory.get_id() != ids.aid {
        return Err(Error::Storage);
    }
    let mut used = BTreeSet::new();
    for service in accessory.get_mut_services() {
        let key = format!("service:{}", type_key(service.get_type())?);
        let id = *ids.instances.get(&key).ok_or(Error::Storage)?;
        if !used.insert(id) {
            return Err(Error::Storage);
        }
        service.set_id(id);
        // These basic services have no linked services. Do not retain constructor IDs.
        service.set_linked_services(Vec::new());
        for characteristic in service.get_mut_characteristics() {
            let key = format!(
                "{key}/characteristic:{}",
                type_key(characteristic.get_type())?
            );
            let id = *ids.instances.get(&key).ok_or(Error::Storage)?;
            if !used.insert(id) {
                return Err(Error::Storage);
            }
            characteristic.set_id(id);
        }
    }
    Ok(())
}
impl LightGraph {
    /// Allocate identifiers and construct an unpublished graph. Runtime must add
    /// snapshot/intent callbacks and coordinate configuration-number publication.
    pub async fn build(
        store: &IdentityStore,
        bridge_name: &str,
        specs: Vec<LightSpec>,
    ) -> Result<Self> {
        Self::build_with_fans(store, bridge_name, specs, BTreeSet::new()).await
    }
    pub async fn build_with_fans(
        store: &IdentityStore,
        bridge_name: &str,
        specs: Vec<LightSpec>,
        fan_ids: BTreeSet<DeviceId>,
    ) -> Result<Self> {
        let valid_name = |name: &str| {
            !name.trim().is_empty() && name.len() <= 64 && !name.chars().any(char::is_control)
        };
        if !valid_name(bridge_name) || specs.len() > 4095 {
            return Err(Error::Storage);
        }
        let mut sorted = BTreeMap::new();
        for spec in specs {
            if !valid_name(&spec.name)
                || spec.default_on_level.is_some_and(|v| v.get() == 0)
                || (!spec.dimmable && spec.default_on_level.is_some())
                || sorted.insert(spec.device.clone(), spec).is_some()
            {
                return Err(Error::Storage);
            }
        }
        if fan_ids.iter().any(|id| sorted.get(id).is_none_or(|s|s.dimmable || s.default_on_level.is_some())) {
            return Err(Error::Storage);
        }
        let mut bridge = BridgeAccessory::new(1, information(1, bridge_name))?;
        let mut definitions = vec![AccessoryDefinition {
            key: AccessoryKey::Bridge,
            resources: resources(&bridge)?,
        }];
        for spec in sorted.values() {
            definitions.push(AccessoryDefinition {
                key: AccessoryKey::Device(spec.device.clone()),
                resources: if fan_ids.contains(&spec.device) {resources(&fan(2, spec)?)?} else {resources(&light(2, spec)?)?},
            });
        }
        let ids = store.allocate_identifiers(definitions).await?;
        install(&mut bridge, &ids[&AccessoryKey::Bridge])?;
        let mut lights = BTreeMap::new();
        let mut fans = BTreeMap::new();
        let mut bindings = Vec::new();
        for (device, spec) in sorted {
            let ids = &ids[&AccessoryKey::Device(device.clone())];
            if fan_ids.contains(&device) {
                let mut accessory = fan(ids.aid, &spec)?;
                install(&mut accessory, ids)?;
                bindings.push(LightBinding {device:device.clone(),aid:ids.aid,
                    on_iid:accessory.fan.power_state.get_id(),brightness_iid:None,default_on_level:None});
                fans.insert(device,accessory);
                continue;
            }
            let mut accessory = light(ids.aid, &spec)?;
            install(&mut accessory, ids)?;
            bindings.push(LightBinding {
                device: device.clone(),
                aid: ids.aid,
                on_iid: accessory.lightbulb.power_state.get_id(),
                brightness_iid: accessory.lightbulb.brightness.as_ref().map(|v| v.get_id()),
                default_on_level: spec.default_on_level,
            });
            lights.insert(device, accessory);
        }
        Ok(Self {
            bridge,
            lights,
            fans,
            bindings,
        })
    }
}
