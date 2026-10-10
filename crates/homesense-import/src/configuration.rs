//! Explicit profile selection produces a disabled, validator-checked draft.
use crate::Report;
use homesense_core::{BusAddress, ExecutionContext, ProtocolProfile};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    net::SocketAddr,
};
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    pub source_sha256: String,
    pub revision: u64,
    pub gateway_id: String,
    pub endpoint: SocketAddr,
    pub modules: Vec<Module>,
    pub control4_ids: Vec<String>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Module {
    pub address: String,
    pub profile: String,
    pub basis: String,
}
#[derive(Serialize)]
pub struct Draft {
    pub source_sha256: String,
    pub configuration: Value,
    pub selected_disabled: Vec<String>,
    pub excluded: Vec<String>,
    pub profile_basis: Vec<Module>,
}
pub fn generate(report: &Report, plan: Plan) -> Result<Draft, String> {
    if plan.source_sha256
        != report
            .source
            .as_ref()
            .ok_or("source fingerprint required")?
            .sha256
    {
        return Err("plan source fingerprint does not match imported input".into());
    }
    if !report.diagnostics.is_empty() {
        return Err("resolve import diagnostics before generating configuration".into());
    }
    if plan.control4_ids.is_empty()
        || plan.control4_ids.len() > 4096
        || plan.modules.is_empty()
        || plan.modules.len() > 255
    {
        return Err("invalid selection size".into());
    }
    let mut modules = BTreeMap::new();
    let profiles = [
        ProtocolProfile::GtC8Rl,
        ProtocolProfile::GtE4Rl,
        ProtocolProfile::GtD8Rl,
        ProtocolProfile::GtL8Rl,
        ProtocolProfile::GtM4Rl,
    ];
    for module in &plan.modules {
        let address = BusAddress::from_hex(&module.address)
            .map_err(|e| e.to_string())?
            .get();
        let profile = profiles
            .iter()
            .copied()
            .find(|p| p.id() == module.profile)
            .ok_or("unknown or unsupported profile")?;
        if module.basis.trim().is_empty()
            || module.basis.len() > 1024
            || modules.insert(address, profile).is_some()
        {
            return Err("missing profile basis or duplicate address".into());
        }
    }
    let mut selected = BTreeSet::new();
    let mut used = BTreeSet::new();
    let mut devices = Vec::new();
    for id in &plan.control4_ids {
        if !selected.insert(id.clone()) {
            return Err("duplicate selected Control4 ID".into());
        }
        let device = report
            .devices
            .iter()
            .find(|d| &d.control4_id == id)
            .ok_or_else(|| format!("unknown Control4 ID {id}"))?;
        let profile = modules
            .get(&device.address)
            .ok_or_else(|| format!("missing profile for {:02X}", device.address))?;
        let kind = match profile.kind() {
            homesense_core::ModuleKind::Switch => "switch",
            homesense_core::ModuleKind::Dimmer => "dimmer",
            homesense_core::ModuleKind::Motor => "motor",
            _ => return Err("unsupported device kind".into()),
        };
        if kind != device.kind {
            return Err(format!("profile category conflicts with item {id}"));
        }
        let channel = device
            .channel
            .ok_or("keypad layout requires a separate mapping")?;
        used.insert(device.address);
        devices.push(json!({"id":device.stable_id,"name":device.name,"gateway":plan.gateway_id,"address":format!("{:02X}",device.address),"channel":channel,"enabled":false}));
    }
    if modules.keys().any(|a| !used.contains(a)) {
        return Err("plan contains unused module mappings".into());
    }
    let modules: Vec<_> = modules.into_iter().map(|(address, profile)| json!({"address":format!("{address:02X}"),"profile":profile.id(),"verification":{"status":"pending"}})).collect();
    let configuration = json!({"schema_version":1,"revision":plan.revision,"manual_degraded_enabled":false,"gateways":[{"id":plan.gateway_id,"endpoint":plan.endpoint.to_string(),"modules":modules}],"devices":devices});
    homesense_config::Configuration::parse(
        &serde_json::to_vec(&configuration).map_err(|e| e.to_string())?,
        ExecutionContext::Live,
    )
    .map_err(|e| e.to_string())?;
    let excluded = report
        .devices
        .iter()
        .filter(|d| !selected.contains(&d.control4_id))
        .map(|d| d.control4_id.clone())
        .collect();
    Ok(Draft {
        source_sha256: plan.source_sha256,
        configuration,
        selected_disabled: selected.into_iter().collect(),
        excluded,
        profile_basis: plan.modules,
    })
}
