//! One explicit immutable migration; generated IDs are saved, never regenerated
//! from physical addresses on parse, restart, replacement or reconnect.
use crate::web_drafts::Draft;
use homesense_core::{BusAddress, ExecutionContext};
use std::collections::BTreeMap;
use uuid::Uuid;
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Edit {
    pub id: Uuid,
    pub expected_revision: String,
    pub revision: String,
}
#[derive(Debug, serde::Serialize)]
pub struct Assignment {
    pub module_id: String,
    pub gateway: String,
    pub address: String,
    pub profile: String,
}
#[derive(Debug, serde::Serialize)]
pub struct Impact {
    pub draft: Uuid,
    pub modules: Vec<Assignment>,
    pub active_configuration_changed: bool,
}
pub fn prepare(
    mut draft: Draft,
    edit: Edit,
    context: ExecutionContext,
) -> Result<(Draft, Impact), String> {
    let old = homesense_config::Configuration::parse(
        &serde_json::to_vec(&draft.devices).map_err(|e| e.to_string())?,
        context,
    )
    .map_err(|e| e.to_string())?;
    if old.schema_version() != 1
        || edit.id == draft.id
        || edit.expected_revision != draft.expected_revision.to_string()
    {
        return Err("immutable legacy source required".into());
    }
    let revision = edit
        .revision
        .parse::<u64>()
        .map_err(|_| "invalid revision")?;
    if revision.to_string() != edit.revision
        || revision < draft.revision
        || revision <= draft.expected_revision
    {
        return Err("invalid revision".into());
    }
    let mut map = BTreeMap::new();
    let mut assignments = vec![];
    for gateway in draft.devices["gateways"]
        .as_array_mut()
        .ok_or("gateways missing")?
    {
        let gateway_id = gateway["id"]
            .as_str()
            .ok_or("gateway ID missing")?
            .to_owned();
        for module in gateway["modules"].as_array_mut().ok_or("modules missing")? {
            let address =
                BusAddress::from_hex(module["address"].as_str().ok_or("address missing")?)
                    .map_err(|e| e.to_string())?;
            let module_id = format!("module:{}", Uuid::new_v4());
            map.insert((gateway_id.clone(), address), module_id.clone());
            assignments.push(Assignment {
                module_id: module_id.clone(),
                gateway: gateway_id.clone(),
                address: address.to_string(),
                profile: module["profile"].as_str().ok_or("profile missing")?.into(),
            });
            module["id"] = module_id.into();
        }
    }
    for device in draft.devices["devices"]
        .as_array_mut()
        .ok_or("devices missing")?
    {
        let key = (
            device["gateway"]
                .as_str()
                .ok_or("gateway missing")?
                .to_owned(),
            BusAddress::from_hex(device["address"].as_str().ok_or("address missing")?)
                .map_err(|e| e.to_string())?,
        );
        let module_id = map.get(&key).ok_or("module mapping missing")?.clone();
        let object = device.as_object_mut().ok_or("device missing")?;
        object.remove("gateway");
        object.remove("address");
        object.insert("module_id".into(), module_id.into());
        if let Some(relay) = object.remove("supply_relay") {
            let relay = old
                .devices()
                .get(
                    &homesense_core::DeviceId::new(
                        relay.as_str().ok_or("invalid supply reference")?,
                    )
                    .map_err(|e| e.to_string())?,
                )
                .ok_or("supply binding missing")?;
            let module_id = map
                .get(&(relay.gateway.clone(), relay.address))
                .ok_or("supply module missing")?;
            object.insert(
                "supply".into(),
                serde_json::json!({"module_id":module_id,"channel":relay.channel.get()}),
            );
        }
    }
    draft.id = edit.id;
    draft.revision = revision;
    draft.devices["schema_version"] = 2.into();
    draft.devices["revision"] = revision.into();
    draft.automation["configuration_revision"] = revision.into();
    let config = homesense_config::Configuration::parse(
        &serde_json::to_vec(&draft.devices).map_err(|e| e.to_string())?,
        context,
    )
    .map_err(|e| e.to_string())?;
    crate::configuration_publish::validate_automation(
        &config,
        &serde_json::to_vec(&draft.automation).map_err(|e| e.to_string())?,
        context,
    )
    .map_err(|e| e.to_string())?;
    Ok((
        draft,
        Impact {
            draft: edit.id,
            modules: assignments,
            active_configuration_changed: false,
        },
    ))
}
