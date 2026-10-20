//! Read-only membership of the lights a scene can command. Conditions alone
//! do not make a light a member; metadata never grants access or queues I/O.
use homesense_config::{Configuration, rule_document::Document};
use homesense_core::{DeviceId, ModuleKind, rule::{Action, Node}};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub(crate) struct Member {
    pub id: String,
    pub name: String,
}

/// Accepted revisions are immutable. Cache only membership, never feedback,
/// availability or grants, so frequent WSS reads cannot rewalk every program.
#[derive(Default)]
pub(crate) struct Cache {
    revision: Option<u64>,
    entries: BTreeMap<String, Vec<Member>>,
}
impl Cache {
    pub(crate) fn read(&mut self, config: &Configuration, document: &Document, scene: &str) -> Result<Vec<Member>, String> {
        if document.revision != config.revision() { return Err("scene light mapping revision mismatch".into()); }
        if self.revision != Some(config.revision()) {
            self.entries.clear();
            self.revision = Some(config.revision());
        }
        if let std::collections::btree_map::Entry::Vacant(entry) = self.entries.entry(scene.to_owned()) {
            entry.insert(members(config, document, scene)?);
        }
        Ok(self.entries[scene].clone())
    }
}

pub(crate) fn members(config: &Configuration, document: &Document, scene: &str) -> Result<Vec<Member>, String> {
    struct Collector<'a> {
        document: &'a Document,
        seen: BTreeSet<String>,
        outputs: BTreeSet<DeviceId>,
        remaining: usize,
    }
    impl Collector<'_> {
        fn nodes(&mut self, nodes: &[Node]) -> Result<(), String> {
            for node in nodes {
                self.remaining = self.remaining.checked_sub(1).ok_or("scene light membership budget exceeded")?;
                match &node.action {
                    Action::Command { device, .. } => { self.outputs.insert(device.clone()); }
                    Action::If { then_actions, else_actions, .. } => {
                        self.nodes(then_actions)?;
                        self.nodes(else_actions)?;
                    }
                    Action::Scene { id, .. } => self.scene(id)?,
                    Action::Set { .. } | Action::Motor { .. }
                    | Action::RequireQuiescence { .. } | Action::SceneActivation { .. } => {}
                }
            }
            Ok(())
        }
        fn scene(&mut self, id: &str) -> Result<(), String> {
            if !self.seen.insert(id.to_owned()) { return Ok(()); }
            if self.document.scene_execution_kind(id).is_none() { return Err("scene light definition unavailable".into()); }
            if let Some(scene) = self.document.immediate_scenes.get(id) {
                self.nodes(&scene.activate)?;
                if let Some(nodes) = &scene.deactivate { self.nodes(nodes)?; }
            } else if let Some(scene) = self.document.timed_scenes.get(id) {
                self.outputs.extend(scene.steps.iter().map(|step| step.device.clone()));
            }
            if let Some(off) = self.document.scene_deactivations.get(id).filter(|off| off.enabled) {
                self.scene(&off.target)?;
            }
            // Include this member's explicit shutdown scope, rather than all
            // peer modes merely sharing the same selection variable.
            for group in &self.document.scene_switch_groups {
                for member in &group.members {
                    if member.scene == id { self.scene(&member.off_scene)?; }
                }
            }
            Ok(())
        }
    }
    let mut collector = Collector { document, seen: BTreeSet::new(), outputs: BTreeSet::new(), remaining: 32768 };
    collector.scene(scene)?;
    let mut ids = BTreeSet::new();
    for id in collector.outputs {
        if let Some(group) = config.composed_resource(&id) {
            ids.extend(group.lights.iter().filter(|light|light.members.iter()
                .any(|member|member.supply==id || member.dimmer==id)).map(|light|light.id.clone()));
            continue;
        }
        let supplied: Vec<_> = config.devices().iter()
            .filter(|(_, device)| device.enabled && device.supply_relay.as_ref() == Some(&id))
            .map(|(id, _)| id.clone()).collect();
        if supplied.is_empty() { ids.insert(id); } else { ids.extend(supplied); }
    }
    Ok(ids.into_iter().filter_map(|id| {
        let (device, module) = config.binding(&id)?;
        matches!(module.kind(), ModuleKind::Switch | ModuleKind::Dimmer)
            .then(|| Member { id: id.as_str().to_owned(), name: config.composed_light(&id)
                .map_or_else(||device.name.clone(),|(_,light)|light.name.clone()) })
    }).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use homesense_core::ExecutionContext;
    use serde_json::{Value, json};

    fn config() -> Configuration {
        config_at(1)
    }
    fn config_at(revision: u64) -> Configuration {
        Configuration::parse(&serde_json::to_vec(&json!({
            "schema_version":2,"revision":revision,
            "gateways":[{"id":"bus","endpoint":"127.0.0.1:1","modules":[
                {"id":"relay","address":"03","profile":"r8i8_din_pro_v2_7_7","verification":{"status":"simulated"}},
                {"id":"dimmer","address":"11","profile":"ad8_din_rs485_v2_2_1","verification":{"status":"simulated"}},
                {"id":"panel","address":"41","profile":"ts6_86_rs485_v2_6_5_6key","led_mode":"manual","verification":{"status":"simulated"}},
                {"id":"motor","address":"21","profile":"c4_din_pro_v2_7_7","verification":{"status":"simulated"}}
            ]}],
            "devices":[
                {"id":"supply","name":"Supply","module_id":"relay","channel":1,"enabled":true},
                {"id":"a","name":"Combined A","module_id":"dimmer","channel":1,"enabled":true,"default_on_level":100,"supply":{"module_id":"relay","channel":1}},
                {"id":"b","name":"B","module_id":"dimmer","channel":2,"enabled":true},
                {"id":"c","name":"Condition only","module_id":"dimmer","channel":3,"enabled":true},
                {"id":"led","name":"LED","module_id":"panel","channel":1,"enabled":true},
                {"id":"curtain","name":"Curtain","module_id":"motor","channel":1,"enabled":true}
            ]
        })).unwrap(), ExecutionContext::Simulation).unwrap()
    }
    fn command(id: &str, command: Value) -> Value {
        json!({"source":"synthetic member","action":{"kind":"command","device":id,"command":command}})
    }
    fn level(id: &str) -> Value { command(id, json!({"kind":"level","value":100,"ramp":false})) }
    fn document(config: &Configuration, scenes: Value, extra: Value) -> Document {
        let mut value = json!({"schema_version":1,"configuration_revision":config.revision(),"variables":[],"rules":[],"immediate_scenes":scenes});
        value.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        Document::parse(&serde_json::to_vec(&value).unwrap(), config).unwrap()
    }
    fn ids(config: &Configuration, document: &Document, id: &str) -> Vec<String> {
        members(config, document, id).unwrap().into_iter().map(|m| m.id).collect()
    }

    #[test]
    fn follows_both_branches_nested_scenes_and_explicit_off_without_condition_or_nonlight_members() {
        let config = config();
        let doc = document(&config, json!([
            {"id":"root","enabled":true,"review_evidence":"synthetic","activate":[
                {"source":"conditional","action":{"kind":"if","condition":{"kind":"equal","left":{"kind":"device_on","device":"c"},"right":{"kind":"bool","value":true}},"then_actions":[level("a")],"else_actions":[{"source":"nested","action":{"kind":"scene","id":"nested","activate":true}}]}},
                command("led",json!({"kind":"panel_led","on":true})),
                command("curtain",json!({"kind":"motor_stop"}))
            ],"deactivate":[level("a")]},
            {"id":"nested","enabled":true,"review_evidence":"synthetic","activate":[level("b")]}
        ]), json!({}));
        assert_eq!(ids(&config, &doc, "root"), ["a", "b"]);
        assert_eq!(members(&config,&doc,"root").unwrap()[0].name,"Combined A");
        assert!(members(&config,&doc,"missing").is_err());
    }

    #[test]
    fn native_off_scope_is_included_without_unrelated_peer_modes_and_supply_is_deduplicated() {
        let config = config();
        let scene = |id: &str, targets: &[&str]| json!({"id":id,"enabled":true,"review_evidence":"synthetic","max_lateness_ms":1000,"steps":targets.iter().map(|id|json!({"source":"synthetic","device":id,"offset_ms":0,"command":{"kind":"level","value":100,"ramp":false}})).collect::<Vec<_>>()});
        let doc = document(&config, json!([]), json!({"timed_scenes":[scene("root",&["a","a"]),scene("off",&["b"]),scene("peer",&["c"])],"variables":[{"id":"mode","kind":"integer","lifetime":"persistent"}],
            "scene_switch_groups":[{"id":"group","variable":"mode","review_evidence":"synthetic","members":[
                {"scene":"root","value":1,"off_scene":"off"},{"scene":"peer","value":2,"off_scene":"off"}]}]}));
        assert_eq!(ids(&config,&doc,"root"),["a","b"]);
        assert_eq!(ids(&config,&doc,"peer"),["b","c"]);
    }

    #[test]
    fn immutable_membership_cache_returns_owned_values_and_rebuilds_on_revision_change() {
        let first = config();
        let next = config_at(2);
        let scene = |id| json!([{"id":"root","enabled":true,"review_evidence":"synthetic","activate":[level(id)]}]);
        let first_doc = document(&first, scene("a"), json!({}));
        let next_doc = document(&next, scene("b"), json!({}));
        let mut cache = Cache::default();
        let mut old = cache.read(&first, &first_doc, "root").unwrap();
        old[0].name = "caller copy".into();
        assert_eq!(cache.read(&first, &first_doc, "root").unwrap()[0].name, "Combined A");
        assert!(cache.read(&next, &first_doc, "root").is_err());
        assert_eq!(cache.read(&next, &next_doc, "root").unwrap()[0].id, "b");
        assert_eq!(old[0].id, "a");
        assert_eq!(cache.read(&first, &first_doc, "root").unwrap()[0].id, "a");
    }
}
