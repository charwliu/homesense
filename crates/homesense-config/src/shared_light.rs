//! Reviewed shared physical resources. Register identities remain unique; this
//! does not relax the exclusive supply rule for ordinary combined lights.
use crate::{Configuration, Device, Error, invalid};
use homesense_core::{DeviceId, ModuleKind};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Input {
    id: String,
    common_supply: String,
    left_supply: String,
    right_supply: String,
    primary: String,
    secondary: String,
    reading: String,
    review_evidence: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub id: String,
    pub common_supply: DeviceId,
    pub left_supply: DeviceId,
    pub right_supply: DeviceId,
    pub primary: DeviceId,
    pub secondary: DeviceId,
    pub reading: DeviceId,
    pub review_evidence: String,
}

impl Group {
    pub fn devices(&self) -> [&DeviceId; 6] {
        [
            &self.common_supply,
            &self.left_supply,
            &self.right_supply,
            &self.primary,
            &self.secondary,
            &self.reading,
        ]
    }

    pub fn light(&self, id: &DeviceId) -> Option<homesense_core::shared_light::Light> {
        use homesense_core::shared_light::Light;
        if id == &self.primary {
            Some(Light::Primary)
        } else if id == &self.secondary {
            Some(Light::Secondary)
        } else {
            None
        }
    }

    pub fn output(&self, output: homesense_core::shared_light::Output) -> &DeviceId {
        use homesense_core::shared_light::Output;
        match output {
            Output::CommonSupply => &self.common_supply,
            Output::LeftSupply => &self.left_supply,
            Output::RightSupply => &self.right_supply,
            Output::PrimaryLevel => &self.primary,
            Output::SecondaryLevel => &self.secondary,
        }
    }
}

pub(super) fn resolve(
    inputs: Vec<Input>,
    configuration: &Configuration,
) -> Result<BTreeMap<String, Group>, Error> {
    if inputs.len() > 64 || (!inputs.is_empty() && configuration.schema_version() != 2) {
        return Err(invalid("shared lights require bounded schema 2 groups"));
    }
    let mut groups = BTreeMap::new();
    let mut used = BTreeSet::new();
    for input in inputs {
        super::identifier(&input.id)?;
        if input.review_evidence.trim().is_empty() || input.review_evidence.len() > 8192 {
            return Err(invalid(
                "shared lights require bounded wiring and behavior review",
            ));
        }
        let group = Group {
            id: input.id,
            common_supply: DeviceId::new(input.common_supply)?,
            left_supply: DeviceId::new(input.left_supply)?,
            right_supply: DeviceId::new(input.right_supply)?,
            primary: DeviceId::new(input.primary)?,
            secondary: DeviceId::new(input.secondary)?,
            reading: DeviceId::new(input.reading)?,
            review_evidence: input.review_evidence,
        };
        let mut first: Option<&Device> = None;
        for (index, id) in group.devices().into_iter().enumerate() {
            let (device, module) = configuration
                .binding(id)
                .ok_or_else(|| invalid("shared light requires six enabled physical bindings"))?;
            if !used.insert(id.clone())
                || device.supply_relay.is_some()
                || configuration
                    .devices()
                    .values()
                    .any(|d| d.supply_relay.as_ref() == Some(id))
                || module.kind()
                    != if index < 3 {
                        ModuleKind::Switch
                    } else {
                        ModuleKind::Dimmer
                    }
                || first.is_some_and(|first| {
                    first.gateway != device.gateway || first.room != device.room
                })
                || index >= 3 && device.default_on_level.is_none()
            {
                return Err(invalid(
                    "shared light bindings must be distinct, same gateway/room, and outside exclusive combined lights",
                ));
            }
            first.get_or_insert(device);
        }
        if groups.insert(group.id.clone(), group).is_some() {
            return Err(invalid("duplicate shared light group"));
        }
    }
    Ok(groups)
}
