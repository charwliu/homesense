//! Reviewed logical light products over shared supply and dimmer registers.
//! Logical IDs reuse existing catalog identities; physical bindings stay unique.
use crate::{Configuration, Error, invalid};
use homesense_core::{DeviceId, ModuleKind, Percent};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Member {
    #[serde(deserialize_with = "device_id")]
    pub supply: DeviceId,
    #[serde(deserialize_with = "device_id")]
    pub dimmer: DeviceId,
}
fn device_id<'de, D: serde::Deserializer<'de>>(d: D) -> Result<DeviceId, D::Error> {
    let value = String::deserialize(d)?;
    crate::identifier(&value).map_err(serde::de::Error::custom)?;
    DeviceId::new(value).map_err(serde::de::Error::custom)
}
fn device_ids<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<DeviceId>, D::Error> {
    Vec::<String>::deserialize(d)?
        .into_iter()
        .map(|v| {
            crate::identifier(&v).map_err(serde::de::Error::custom)?;
            DeviceId::new(v).map_err(serde::de::Error::custom)
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Light {
    #[serde(deserialize_with = "device_id")]
    pub id: DeviceId,
    pub name: String,
    pub members: Vec<Member>,
    pub default_on_level: u8,
    #[serde(default, deserialize_with = "device_ids")]
    pub excludes_on: Vec<DeviceId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Group {
    pub id: String,
    pub lights: Vec<Light>,
    pub review_evidence: String,
    /// Reviewed wiring may couple lamps when multiple products share a column.
    /// Always prefer preserving peers; expose actual feedback for unavoidable coupling.
    #[serde(default)]
    pub allow_shared_side_effects: bool,
}
impl Group {
    pub fn resources(&self) -> BTreeSet<&DeviceId> {
        self.lights
            .iter()
            .flat_map(|l| l.members.iter().flat_map(|m| [&m.supply, &m.dimmer]))
            .collect()
    }
    pub fn light(&self, id: &DeviceId) -> Option<&Light> {
        self.lights.iter().find(|l| &l.id == id)
    }
}

pub(super) fn resolve(
    inputs: Vec<Group>,
    config: &Configuration,
) -> Result<BTreeMap<String, Group>, Error> {
    if inputs.len() > 32 || !inputs.is_empty() && config.schema_version() != 2 {
        return Err(invalid("composed lights require bounded schema2 groups"));
    }
    let mut groups = BTreeMap::new();
    let mut used = BTreeSet::new();
    for group in inputs {
        crate::identifier(&group.id)?;
        if group.lights.is_empty()
            || group.lights.len() > 8
            || group.review_evidence.trim().is_empty()
            || group.review_evidence.len() > 8192
        {
            return Err(invalid("composed light review or size invalid"));
        }
        let ids = group.lights.iter().map(|l| &l.id).collect::<BTreeSet<_>>();
        if ids.len() != group.lights.len() {
            return Err(invalid("duplicate composed light identity"));
        }
        let mut edges = BTreeSet::new();
        let mut supplies = BTreeSet::new();
        let mut dimmers = BTreeSet::new();
        let mut location = None;
        for light in &group.lights {
            if light.name.trim().is_empty()
                || light.name.chars().count() > 100
                || light.name.chars().any(char::is_control)
                || light.members.is_empty()
                || light.members.len() > 4
                || light.default_on_level == 0
                || Percent::new(light.default_on_level).is_err()
                || light.excludes_on.len()
                    != light.excludes_on.iter().collect::<BTreeSet<_>>().len()
                || light
                    .excludes_on
                    .iter()
                    .any(|id| id == &light.id || !ids.contains(id))
            {
                return Err(invalid(
                    "invalid composed light members, default or exclusions",
                ));
            }
            // A grouped night light may span both bedside supplies, with one
            // shared brightness. Different dimmers cannot invent one percent.
            if light
                .members
                .iter()
                .any(|m| m.dimmer != light.members[0].dimmer)
            {
                return Err(invalid("one composed light requires one physical dimmer"));
            }
            for member in &light.members {
                if !edges.insert((&member.supply, &member.dimmer)) {
                    return Err(invalid("duplicate composed light product"));
                }
                supplies.insert(&member.supply);
                dimmers.insert(&member.dimmer);
                for (id, kind) in [
                    (&member.supply, ModuleKind::Switch),
                    (&member.dimmer, ModuleKind::Dimmer),
                ] {
                    let (d, m) = config
                        .binding(id)
                        .ok_or_else(|| invalid("composed light resource disabled or missing"))?;
                    if m.kind() != kind
                        || d.supply_relay.is_some()
                        || config
                            .devices()
                            .values()
                            .any(|other| other.supply_relay.as_ref() == Some(id))
                    {
                        return Err(invalid(
                            "composed light resources conflict with physical or exclusive supply bindings",
                        ));
                    }
                    let here = (d.gateway.clone(), d.room.clone());
                    if location.as_ref().is_some_and(|old| old != &here) {
                        return Err(invalid("composed lights require one gateway and room"));
                    }
                    location.get_or_insert(here);
                }
            }
        }
        if supplies.len() > 8 || dimmers.len() > 4 || edges.len() > 24 {
            return Err(invalid("composed light matrix exceeds planning bound"));
        }
        let resources = group.resources();
        if ids.iter().any(|id| !resources.contains(id)) {
            return Err(invalid(
                "composed light identity must retain an existing resource identity",
            ));
        }
        for id in &resources {
            if !used.insert((*id).clone()) {
                return Err(invalid("overlapping composed light groups"));
            }
        }
        for shared in config.shared_lights().values() {
            let shared_resources = shared.devices().into_iter().collect::<BTreeSet<_>>();
            if !resources.is_disjoint(&shared_resources) && resources != shared_resources {
                return Err(invalid(
                    "composed group must cover an overlapping shared matrix completely",
                ));
            }
        }
        if groups.insert(group.id.clone(), group).is_some() {
            return Err(invalid("duplicate composed light group"));
        }
    }
    Ok(groups)
}
