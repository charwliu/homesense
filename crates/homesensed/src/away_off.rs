//! Explicit Away module-control lane. No ordinary scene or lamp is folded.
use crate::{
    away_auth::Authorization,
    module_all_off::{self, LightingReview, Plan},
};
use homesense_config::{Configuration, rule_document::Document};
use homesense_core::{DeviceId, ExecutionContext};
use std::collections::BTreeSet;
#[derive(Clone, Debug)]
pub(crate) struct Target {
    pub plan: Plan,
    pub index: usize,
    pub lease: Authorization,
}
impl Target {
    pub(crate) fn frame(
        &self,
        config: &Configuration,
        context: ExecutionContext,
        epoch: u64,
    ) -> Result<(String, hb_transport::PreparedFrame), String> {
        if !self.lease.valid() {
            return Err("Away credential revoked".into());
        }
        self.lease.key_binding(config)?;
        let authorized = authorized_devices(config, self.lease.devices())?;
        self.plan
            .revalidate(config, &authorized, context)
            .map_err(|e| format!("Away mapping changed: {e:?}"))?;
        let target = self
            .plan
            .modules()
            .get(self.index)
            .ok_or("Away module missing")?;
        Ok((
            target.gateway_id().into(),
            hb_transport::PreparedFrame::prepare_module_all_off(target.module(), context, epoch)
                .map_err(|e| e.to_string())?,
        ))
    }
    pub(crate) fn effects(&self, id: u64) -> homesense_storage::module_off::Effects {
        let m = &self.plan.modules()[self.index];
        homesense_storage::module_off::Effects {
            command: id,
            module_id: m.module_id().into(),
            gateway_id: m.gateway_id().into(),
            address: m.module().address().get(),
            profile: m.module().profile().id().into(),
            devices: m
                .devices()
                .iter()
                .enumerate()
                .map(|(n, d)| ((n + 1) as u8, d.as_str().into()))
                .collect(),
        }
    }
}
pub(crate) fn plan(
    config: &Configuration,
    document: &Document,
    lease: Authorization,
    context: ExecutionContext,
) -> Result<Vec<Target>, String> {
    if !lease.valid() {
        return Err("Away credential revoked".into());
    }
    lease.key_binding(config)?;
    let policy = document
        .away_off
        .as_ref()
        .filter(|p| p.enabled)
        .ok_or("Away operation not enabled")?;
    if lease.origin() == homesense_core::queue::Origin::Hap && !policy.hap_expose && !lease.permits_household_hap(policy) {
        return Err("HAP Away exposure not enabled".into());
    }
    if policy.review_evidence.trim().is_empty() || policy.modules.len() > 128 {
        return Err("Away review required".into());
    }
    let reviews = policy
        .modules
        .iter()
        .map(|m| {
            LightingReview::new(
                m.module_id.clone(),
                m.devices
                    .iter()
                    .map(|id| DeviceId::new(id.clone()))
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|e| e.to_string())?,
                m.review_evidence.clone(),
            )
            .map_err(|e| format!("Away review: {e:?}"))
        })
        .collect::<Result<Vec<_>, String>>()?;
    let authorized = authorized_devices(config, lease.devices())?;
    let plan = module_all_off::plan(config, &reviews, &authorized, context)
        .map_err(|e| format!("Away scope: {e:?}"))?;
    Ok((0..plan.modules().len())
        .map(|index| Target {
            plan: plan.clone(),
            index,
            lease: lease.clone(),
        })
        .collect())
}
/// Expand logical lamp scope to its hidden supply only when every logical
/// owner is enabled and authorized. Recomputed against the current mapping
/// both at admission and immediately before each module write.
pub(crate) fn authorized_devices(
    config: &Configuration,
    scope: &BTreeSet<String>,
) -> Result<BTreeSet<DeviceId>, String> {
    let original = scope
        .iter()
        .map(|id| DeviceId::new(id.clone()))
        .collect::<Result<BTreeSet<_>, _>>()
        .map_err(|e| e.to_string())?;
    let supplies = config
        .devices()
        .values()
        .filter_map(|device| device.supply_relay.clone())
        .collect::<BTreeSet<_>>();
    // A raw grant for a hidden supply must never bypass logical-owner scope.
    let mut authorized = original
        .difference(&supplies)
        .cloned()
        .collect::<BTreeSet<_>>();
    for supply in supplies {
        if config
            .devices()
            .iter()
            .filter(|(_, d)| d.supply_relay.as_ref() == Some(&supply))
            .all(|(owner, _)| original.contains(owner) && config.binding(owner).is_some())
        {
            authorized.insert(supply);
        }
    }
    Ok(authorized)
}
pub(crate) fn receipt_device(id: u64) -> DeviceId {
    DeviceId::new(format!("module-off-ticket:{id}")).expect("module receipt identity")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn supply_scope_comes_only_from_enabled_logical_owners() {
        let mut devices = Vec::new();
        for (channel, suffix) in [(1, "a"), (2, "b")] {
            devices.push(serde_json::json!({"id":format!("supply:{suffix}"),"name":"Hidden supply","module_id":"relay","channel":channel,"enabled":true}));
            devices.push(serde_json::json!({"id":format!("lamp:{suffix}"),"name":"Synthetic light","module_id":"dimmer","channel":channel,"enabled":true,"default_on_level":75,"supply":{"module_id":"relay","channel":channel}}));
        }
        let mut value = serde_json::json!({"schema_version":2,"revision":1,"gateways":[{"id":"bus","endpoint":"127.0.0.1:1","modules":[{"id":"relay","address":"27","profile":"r8i8_din_pro_v2_7_7","verification":{"status":"simulated"}},{"id":"dimmer","address":"38","profile":"ad8_din_rs485_v2_2_1","verification":{"status":"simulated"}}]}],"devices":devices});
        let config = Configuration::parse(
            &serde_json::to_vec(&value).unwrap(),
            ExecutionContext::Simulation,
        )
        .unwrap();
        let supply = DeviceId::new("supply:a").unwrap();
        let all = BTreeSet::from(["lamp:a".into(), "lamp:b".into()]);
        assert!(authorized_devices(&config, &all).unwrap().contains(&supply));
        assert!(
            !authorized_devices(&config, &BTreeSet::from(["supply:a".into()]))
                .unwrap()
                .contains(&supply)
        );
        assert!(
            !authorized_devices(
                &config,
                &BTreeSet::from(["supply:a".into(), "lamp:b".into()])
            )
            .unwrap()
            .contains(&supply)
        );
        value["devices"][0]["enabled"] = false.into();
        value["devices"][1]["enabled"] = false.into();
        let disabled = Configuration::parse(
            &serde_json::to_vec(&value).unwrap(),
            ExecutionContext::Simulation,
        )
        .unwrap();
        assert!(
            !authorized_devices(&disabled, &all)
                .unwrap()
                .contains(&supply)
        );
    }
}

/// Internal projection prerequisites, never a client-supplied write scope.
#[derive(Debug,Clone,PartialEq,Eq)]
pub(crate) struct CatalogScope {required: BTreeSet<String>,ready:bool}
impl CatalogScope {
    pub(crate) fn permitted(&self,grant:&crate::web_auth::Grant)->bool {
        self.ready && grant.role!=crate::web_auth::Role::Viewer && self.required.iter().all(|id|grant.may_control_device(id))
    }
}
pub(crate) fn catalog_scope(config:&Configuration,document:&Document,gateways:&[crate::gateway_runtime::View])->Option<CatalogScope> {
    let policy=document.away_off.as_ref().filter(|p|p.enabled)?;
    let mut required=BTreeSet::new();let mut target_gateways=BTreeSet::new();
    for id in policy.modules.iter().flat_map(|module|&module.devices) {
        let id=DeviceId::new(id).ok()?;let (binding,_)=config.binding(&id)?;
        target_gateways.insert(binding.gateway.clone());
        let owners=config.devices().iter().filter(|(_,d)|d.enabled && d.supply_relay.as_ref()==Some(&id)).map(|(id,_)|id.as_str().to_owned()).collect::<Vec<_>>();
        if owners.is_empty(){required.insert(id.as_str().to_owned());}else{required.extend(owners);}
    }
    let ready=gateways.is_empty() || target_gateways.iter().all(|gateway|gateways.iter().any(|view|&view.gateway==gateway && view.state=="ready"));
    Some(CatalogScope{required,ready})
}

#[cfg(test)]
#[path="keypad_away_tests.rs"]
mod keypad_tests;
