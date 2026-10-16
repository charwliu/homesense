//! Explicit consumers constrain only their physical ports, never the whole
//! default idle round. A requested read cannot renew its own observation time.
use homesense_config::{Configuration,rule_document::Document};
use homesense_core::DeviceId;
use std::collections::BTreeMap;
pub(crate) fn requirements(config:&Configuration,document:&Document)->Result<BTreeMap<DeviceId,u64>,String>{
    let configured=config.refresh_policy().observation_max_age_ms();let mut ages=BTreeMap::<DeviceId,u64>::new();
    let mut add=|id:DeviceId,age:u64|{let age=age.min(configured);if age<configured{ages.entry(id).and_modify(|old|*old=(*old).min(age)).or_insert(age);}};
    for group in &document.scene_switch_groups {
        if let Some(policy)=group.reconciliation.as_ref().filter(|p|p.enabled) {for id in group.light_on_patterns(&document.timed_scenes).map_err(|e|e.to_string())?.values().flat_map(|v|v.keys()){add(id.clone(),policy.max_age_ms);}}
        if let Some(policy)=group.initialization.as_ref().filter(|p|p.enabled) {for id in group.light_targets(&document.timed_scenes).map_err(|e|e.to_string())?.values().flat_map(|v|v.keys()){add(id.clone(),policy.max_age_ms);}}
    }
    for policy in document.scene_tracking.values().filter(|p|p.enabled) {for m in &policy.members {if !matches!(m.predicate,homesense_config::scene_tracking::Predicate::Ignore{..}){add(DeviceId::new(&m.device).map_err(|e|e.to_string())?,policy.max_age_ms);}}}
    // Indicator links and enabled manual feedback use the published runtime
    // bound unless a reviewed policy above sets a stricter age for that port.
    for link in document.indicator_links.iter().filter(|l|l.enabled){for id in &link.lights{add(id.clone(),configured);}}
    for id in document.immediate_scenes.keys().filter(|id|document.scene_action_available(id)) {for target in homesense_config::keypad::scene_execute_devices(document,config,id).map_err(|e|e.to_string())? {add(target,configured);}}
    for policy in &document.component_mode_confirmations {for condition in &policy.postconditions{add(DeviceId::new(&condition.device).map_err(|e|e.to_string())?,policy.max_age_ms);}}
    let supplies=ages.iter().filter_map(|(id,age)|config.devices().get(id).and_then(|d|d.supply_relay.clone()).map(|id|(id,*age))).collect::<Vec<_>>();for (id,age) in supplies{ages.entry(id).and_modify(|old|*old=(*old).min(age)).or_insert(age);}
    Ok(ages)
}
pub(crate) fn target_ages(config:&Configuration,targets:&[crate::physical_queries::Target],requirements:&BTreeMap<DeviceId,u64>)->BTreeMap<usize,u64>{
    let mut result=BTreeMap::new();for (id,age) in requirements {if let Some((binding,_))=config.binding(id){for (index,target) in targets.iter().enumerate(){if target.gateway==binding.gateway && target.module.address()==binding.address && target.channels.contains(&binding.channel.get()){result.entry(index).and_modify(|old:&mut u64|*old=(*old).min(*age)).or_insert(*age);}}}}result
}
pub(crate) fn due(target:&crate::physical_queries::Target,truth:&crate::module_truth::ModuleTruth,age:u64)->u64{
    target.channels.iter().filter_map(|ch|truth.located_state(&target.gateway,target.module.address().get(),*ch).and_then(|s|s.observed_at_ms)).min().map_or(0,|at|at.saturating_add((age/2).max(1)))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reviewed_reconciliation_initialization_and_tracking_bounds_remain_scoped(){
        let bytes=include_bytes!("../../../config/test/native-app-keypad-away/devices.template.json");let config=Configuration::parse(bytes,homesense_core::ExecutionContext::Simulation).unwrap();let mut value:serde_json::Value=serde_json::from_slice(include_bytes!("../../../config/test/native-app-keypad-away/automation.json")).unwrap();
        for kind in ["reconciliation","initialization"] {
            let mut document=value.clone();document["scene_switch_groups"][0].as_object_mut().unwrap().remove("reconciliation");document["scene_switch_groups"][0][kind]=if kind=="reconciliation"{serde_json::json!({"enabled":true,"policy":"unique_light_on_pattern","max_age_ms":5000,"review_evidence":"synthetic"})}else{serde_json::json!({"enabled":true,"policy":"unique_light_targets","max_age_ms":5000,"review_evidence":"synthetic"})};
            let doc=Document::parse(&serde_json::to_vec(&document).unwrap(),&config).unwrap();let ids=requirements(&config,&doc).unwrap();assert_eq!(ids.len(),1);assert_eq!(ids[&DeviceId::new("relay-a:1").unwrap()],5000);
        }
        let mut tracking=value.clone();tracking["scene_switch_groups"][0]["reconciliation"]["enabled"]=false.into();tracking["timed_scenes"].as_array_mut().unwrap().push(serde_json::json!({"id":"track-only","enabled":true,"review_evidence":"synthetic","max_lateness_ms":1000,"steps":[{"source":"synthetic","offset_ms":0,"device":"relay-b:1","command":{"kind":"switch","on":true}}]}));tracking["scene_tracking"]=serde_json::json!([{"scene":"track-only","enabled":true,"aggregation":"all","unknown_policy":"require_all_known","pending_policy":"block_scene","control_policy":"separate_action_and_read_only_activity","activity_name":"synthetic","max_age_ms":7000,"review_evidence":"synthetic","members":[{"id":"one","device":"relay-b:1","predicate":{"kind":"power","on":true},"review_evidence":"synthetic"}]}]);let doc=Document::parse(&serde_json::to_vec(&tracking).unwrap(),&config).unwrap();let ages=requirements(&config,&doc).unwrap();assert_eq!(ages.len(),1);assert_eq!(ages[&DeviceId::new("relay-b:1").unwrap()],7000);
        value["scene_switch_groups"]=serde_json::json!([]);let doc=Document::parse(&serde_json::to_vec(&value).unwrap(),&config);assert!(doc.is_err(),"existing scene_toggle still requires selection group");
    }
}
