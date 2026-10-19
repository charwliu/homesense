//! Read-only publication impact. This is not a reservation or permission to publish.
use homesense_config::Configuration;
#[derive(Debug, PartialEq, Eq, serde::Serialize)]
pub struct Run {
    pub run: u64,
    pub audit_id: String,
    pub definition: Option<String>,
    pub status: String,
}
#[derive(Debug, PartialEq, Eq, serde::Serialize)]
pub struct Preview {
    pub boot: String,
    pub active_revision: u64,
    pub candidate_revision: u64,
    pub connection_rebuild_required: bool,
    pub interrupted_runs: Vec<Run>,
    pub device_changes: Vec<DeviceChange>,
    pub invalidates_old_rules_and_timers: bool,
}
pub fn connection_rebuild_required(current: &Configuration, candidate: &Configuration) -> bool {
    current.gateways().len() != candidate.gateways().len()
        || current.gateways().iter().any(|(id, gateway)| {
            candidate.gateways().get(id).is_none_or(|next| {
                gateway.endpoint != next.endpoint || gateway.modules() != next.modules()
            })
        })
}

#[derive(Debug, PartialEq, Eq, serde::Serialize)]
pub struct DeviceChange {
    pub device: String,
    pub changes: Vec<&'static str>,
}
/// Compare configured devices, including disabled entries. No state mutation.
pub fn device_changes(current:&Configuration,candidate:&Configuration)->Vec<DeviceChange> {
    let ids=current.devices().keys().chain(candidate.devices().keys()).collect::<std::collections::BTreeSet<_>>();
    ids.into_iter().filter_map(|id| {
        let mut changes=Vec::new();
        match (current.devices().get(id),candidate.devices().get(id)) {
            (None,Some(_))=>changes.push("added"),
            (Some(_),None)=>changes.push("removed"),
            (Some(old),Some(new))=>{
                if old.name!=new.name || old.room!=new.room {changes.push("metadata");}
                if old.enabled!=new.enabled {changes.push(if new.enabled {"enabled"} else {"disabled"});}
                if old.gateway!=new.gateway || old.address!=new.address || old.channel!=new.channel {changes.push("binding");}
                if old.module_id!=new.module_id{changes.push("module_reference");}
                if old.supply_relay!=new.supply_relay || old.supply_channel!=new.supply_channel {changes.push("supply_relay");}
                if current.shared_resource(id)!=candidate.shared_resource(id) {changes.push("shared_light");}
                if old.default_on_level!=new.default_on_level {changes.push("default_on_level");}
                let calibration=|d:&homesense_config::Device| d.motor_calibration.as_ref().map(|c|
                    (c.motion(homesense_core::MotorAction::Open),c.evidence().evidence().to_owned(),c.evidence().verified_at_unix_ms()));
                if calibration(old)!=calibration(new) {changes.push("motor_calibration");}
                if old.curtain_state!=new.curtain_state {changes.push("curtain_state");}
                let gateway_changed=match (current.gateways().get(&old.gateway),candidate.gateways().get(&new.gateway)) {
                    (Some(a),Some(b))=>a.endpoint!=b.endpoint || a.modules().get(&old.address)!=b.modules().get(&new.address),
                    _=>true,
                };
                if gateway_changed {changes.push("gateway_or_module");}
            },
            _=>{},
        }
        (!changes.is_empty()).then(||DeviceChange{device:id.as_str().to_owned(),changes})
    }).collect()
}
