//! Configuration-only module replacement. No hardware address or parameter write.
use crate::web_drafts::Draft;
use homesense_core::{BusAddress, ExecutionContext, ModuleKind};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;
use uuid::Uuid;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Edit {
    pub id: Uuid,
    pub expected_revision: String,
    pub revision: String,
    pub module_id: String,
    pub new_address: String,
    pub profile: String,
    pub verification: VerificationEdit,
}
#[derive(Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum VerificationEdit {
    Pending,
    Simulated,
    Verified {
        evidence: String,
        verified_at_unix_ms: String,
    },
}
impl VerificationEdit {
    fn document(self) -> Result<Value, String> {
        Ok(match self {
            Self::Pending => serde_json::json!({"status":"pending"}),
            Self::Simulated => serde_json::json!({"status":"simulated"}),
            Self::Verified {
                evidence,
                verified_at_unix_ms,
            } => {
                let time = verified_at_unix_ms
                    .parse::<u64>()
                    .map_err(|_| "invalid verification time")?;
                if time == 0 || time.to_string() != verified_at_unix_ms {
                    return Err("invalid verification time".into());
                }
                serde_json::json!({"status":"verified","evidence":evidence,"verified_at_unix_ms":time})
            }
        })
    }
}
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct Impact {
    pub draft: Uuid,
    pub module_id: String,
    pub gateway: String,
    pub old_address: String,
    pub new_address: String,
    pub old_profile: String,
    pub new_profile: String,
    pub devices: Vec<String>,
    pub related_logical_lights: Vec<String>,
    pub scenes: Vec<String>,
    pub recalibration_required: Vec<String>,
    pub hardware_address_changed: bool,
    pub active_configuration_changed: bool,
}
pub fn prepare(
    mut draft: Draft,
    edit: Edit,
    context: ExecutionContext,
) -> Result<(Draft, Impact), String> {
    if edit.id == draft.id || edit.expected_revision != draft.expected_revision.to_string() {
        return Err("source draft or base revision changed".into());
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
    let new_address = BusAddress::from_hex(&edit.new_address).map_err(|e| e.to_string())?;
    let old = homesense_config::Configuration::parse(
        &serde_json::to_vec(&draft.devices).map_err(|e| e.to_string())?,
        context,
    )
    .map_err(|e| e.to_string())?;
    if old.schema_version() != 2 {
        return Err("persistent module ID migration required".into());
    }
    let (location, old_module) = old
        .module(&edit.module_id)
        .ok_or("stable module not found")?;
    let gateway_id = location.gateway.clone();
    let old_address = location.address;
    if old_address == new_address {
        return Err("new module address required".into());
    }
    if old.gateways()[&gateway_id]
        .modules()
        .contains_key(&new_address)
    {
        return Err("module address conflict".into());
    }
    let ids: BTreeSet<String> = old
        .devices()
        .iter()
        .filter(|(_, d)| d.module_id.as_deref() == Some(edit.module_id.as_str()))
        .map(|(id, _)| id.as_str().to_owned())
        .collect();
    let related: BTreeSet<String> = old
        .devices()
        .iter()
        .filter(|(_, d)| {
            d.supply_relay
                .as_ref()
                .is_some_and(|id| ids.contains(id.as_str()))
        })
        .map(|(id, _)| id.as_str().to_owned())
        .collect();
    let gateways = draft.devices["gateways"]
        .as_array_mut()
        .ok_or("gateways missing")?;
    let module = gateways
        .iter_mut()
        .find(|g| g["id"] == gateway_id)
        .and_then(|g| g["modules"].as_array_mut())
        .and_then(|ms| ms.iter_mut().find(|m| m["id"] == edit.module_id))
        .ok_or("module missing")?;
    module["address"] = new_address.to_string().into();
    module["profile"] = edit.profile.clone().into();
    module["verification"] = edit.verification.document()?;
    // LED mode belongs to a panel capability; retaining it is allowed only after full parse below.
    let mut recalibration_required = vec![];
    for d in draft.devices["devices"]
        .as_array_mut()
        .ok_or("devices missing")?
    {
        if d["id"].as_str().is_some_and(|id| ids.contains(id)) {
            if old_module.kind() == ModuleKind::Motor {
                recalibration_required
                    .push(d["id"].as_str().ok_or("device id missing")?.to_owned());
            }
            d.as_object_mut()
                .ok_or("invalid device")?
                .remove("motor_calibration");
        }
    }
    draft.id = edit.id;
    draft.revision = revision;
    draft.devices["revision"] = revision.into();
    draft.automation["configuration_revision"] = revision.into();
    let candidate = homesense_config::Configuration::parse(
        &serde_json::to_vec(&draft.devices).map_err(|e| e.to_string())?,
        context,
    )
    .map_err(|e| e.to_string())?;
    let new_module = &candidate.gateways()[&gateway_id].modules()[&new_address];
    if new_module.kind() != old_module.kind()
        || new_module.channels() < old_module.channels()
        || new_module.channels() == 0
    {
        return Err("incompatible module kind or channel capacity".into());
    }
    let rules = crate::configuration_publish::validate_automation(
        &candidate,
        &serde_json::to_vec(&draft.automation).map_err(|e| e.to_string())?,
        context,
    )
    .map_err(|e| e.to_string())?;
    let affected = ids.union(&related).collect::<BTreeSet<_>>();
    let scenes = rules
        .timed_scenes
        .iter()
        .filter(|(_, scene)| {
            scene
                .steps
                .iter()
                .any(|step| affected.contains(&step.device.as_str().to_owned()))
        })
        .map(|(id, _)| id.clone())
        .collect();
    let impact = Impact {
        draft: draft.id,
        module_id: edit.module_id,
        gateway: gateway_id,
        old_address: old_address.to_string(),
        new_address: new_address.to_string(),
        old_profile: old_module.profile().id().into(),
        new_profile: new_module.profile().id().into(),
        devices: ids.into_iter().collect(),
        related_logical_lights: related.into_iter().collect(),
        scenes,
        recalibration_required,
        hardware_address_changed: false,
        active_configuration_changed: false,
    };
    Ok((draft, impact))
}
