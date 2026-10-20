//! Explicit multi-bridge publication plan. Parsing never creates identities or listeners.
use homesense_core::{DeviceId, ModuleKind};
use serde::Deserialize;
use std::{
    collections::BTreeSet,
    net::SocketAddr,
    path::{Path, PathBuf},
};
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    pub schema_version: u32,
    pub bridges: Vec<Bridge>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bridge {
    pub id: String,
    pub name: String,
    pub address: SocketAddr,
    #[serde(default, deserialize_with = "device_ids")]
    pub lights: Vec<DeviceId>,
    /// Publish every visible logical light, including reviewed composed lights.
    #[serde(default)]
    pub all_lights: bool,
    #[serde(default)]
    pub scenes: Vec<String>,
    #[serde(default, deserialize_with = "device_ids")]
    pub motors: Vec<DeviceId>,
    #[serde(default, deserialize_with = "device_ids")]
    pub panels: Vec<DeviceId>,
}
pub fn all_light_ids(config: &homesense_config::Configuration) -> BTreeSet<DeviceId> {
    let supplies: BTreeSet<_> = config.devices().values().filter(|d| d.enabled)
        .filter_map(|d| d.supply_relay.as_ref()).collect();
    config.devices().iter().filter_map(|(id, device)| {
        let (_, module) = config.binding(id)?;
        if !matches!(module.kind(), ModuleKind::Switch | ModuleKind::Dimmer)
            || supplies.contains(id) { return None; }
        // A reviewed logical light reuses a physical identity. Hiding that raw
        // channel must not hide the logical card that the App publishes.
        if config.composed_light(id).is_some() { return Some(id.clone()); }
        if !device.catalog_visible { return None; }
        (config.composed_resource(id).is_none()).then(|| id.clone())
    }).collect()
}
impl Bridge {
    pub fn selected_lights(&self, config: &homesense_config::Configuration) -> BTreeSet<DeviceId> {
        if self.all_lights { all_light_ids(config) } else { self.lights.iter().cloned().collect() }
    }
}
fn device_ids<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<DeviceId>, D::Error> {
    Vec::<String>::deserialize(deserializer)?
        .into_iter()
        .map(|value| {
            DeviceId::new(value).map_err(|error| serde::de::Error::custom(format!("{error:?}")))
        })
        .collect()
}
/// Resolve explicit logical keys against the active, verified configuration.
/// Key numbers and display names come from the binding, never caller guesses.
/// Panel publication never infers keys from a module or exposes an entire panel.
pub fn panel_specs(
    selected: &[DeviceId],
    config: &homesense_config::Configuration,
) -> Result<Vec<homesense_hap::panel_graph::PanelKeySpec>, String> {
    if selected.len() > 256 {
        return Err("HAP panel selection exceeds 256 keys".into());
    }
    let mut devices = BTreeSet::new();
    let mut physical = BTreeSet::new();
    let mut result = Vec::new();
    for id in selected {
        let (device, module) = config
            .binding(id)
            .ok_or("HAP panel key missing or disabled")?;
        if module.profile() != homesense_core::ProtocolProfile::Ts686Rs485SixKey {
            return Err("HAP panel requires a confirmed TS6 six-key profile".into());
        }
        if !devices.insert(id)
            || !physical.insert((&device.gateway, device.address, device.channel.get()))
        {
            return Err("duplicate HAP panel key".into());
        }
        if device.name.trim().is_empty()
            || device.name.len() > 64
            || device.name.chars().any(char::is_control)
        {
            return Err("invalid HAP panel key name".into());
        }
        result.push(homesense_hap::panel_graph::PanelKeySpec {
            device: id.clone(),
            key: device.channel.get(),
            name: device.name.clone(),
        });
    }
    Ok(result)
}

pub(crate) fn validate_panel_ownership(selected:&[DeviceId],document:&homesense_config::rule_document::Document)->Result<(),String> {
    let selected:BTreeSet<_>=selected.iter().map(|id|id.as_str()).collect();
    if document.rules.iter().any(|rule|rule.enabled && matches!(&rule.trigger,homesense_config::rule_document::Trigger::Panel{device,..} if selected.contains(device.as_str())))
        || document.holds.as_ref().is_some_and(|holds|holds.bindings.iter().any(|hold|hold.enabled && selected.contains(hold.key.as_str()))) {
        return Err("HAP panel key already executes a Rust rule or hold action".into());
    }
    Ok(())
}

impl Plan {
    pub fn parse(
        bytes: &[u8],
        config: &homesense_config::Configuration,
        document: &homesense_config::rule_document::Document,
    ) -> Result<Self, String> {
        if document.revision != config.revision() {
            return Err("HAP plan configuration revision mismatch".into());
        }
        if bytes.len() > 65536 {
            return Err("HAP bridge plan exceeds 64 KiB".into());
        }
        let plan: Self = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        if plan.schema_version != 1 || plan.bridges.is_empty() || plan.bridges.len() > 16 {
            return Err("HAP bridge plan requires schema 1 and 1..16 bridges".into());
        }
        let mut ids = BTreeSet::new();
        let mut addresses = BTreeSet::new();
        let mut devices = BTreeSet::new();
        let mut scenes = BTreeSet::new();
        let supplies: BTreeSet<_> = config
            .devices()
            .values()
            .filter(|device| device.enabled)
            .filter_map(|device| device.supply_relay.as_ref())
            .collect();
        for bridge in &plan.bridges {
            if bridge.id.is_empty()
                || bridge.id.len() > 48
                || !bridge
                    .id
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
            {
                return Err("invalid HAP bridge id".into());
            }
            if bridge.name.trim().is_empty()
                || bridge.name.len() > 64
                || bridge.name.chars().any(char::is_control)
            {
                return Err("invalid HAP bridge name".into());
            }
            if !ids.insert(&bridge.id) || !addresses.insert(bridge.address) {
                return Err("duplicate HAP bridge id or endpoint".into());
            }
            if bridge.lights.is_empty() && !bridge.all_lights && bridge.scenes.is_empty() && bridge.motors.is_empty() && bridge.panels.is_empty() {
                return Err("empty HAP bridge publication".into());
            }
            if bridge.all_lights && !bridge.lights.is_empty() {
                return Err("HAP all_lights cannot be combined with explicit lights".into());
            }
            if bridge.lights.iter().collect::<BTreeSet<_>>().len() != bridge.lights.len() {
                return Err("duplicate HAP light selection".into());
            }
            let settings = crate::hap_endpoint::Settings {
                address: bridge.address,
                scenes: bridge.scenes.clone(),
                motors: bridge.motors.clone(),
            };
            settings.validate(config, document)?;
            let selected_lights = bridge.selected_lights(config);
            if bridge.all_lights && selected_lights.is_empty() {
                return Err("HAP all_lights found no visible logical lights".into());
            }
            if selected_lights.len() > 4095 { return Err("too many HAP lights".into()); }
            for id in &selected_lights {
                if supplies.contains(id)
                    || !config.binding(id).is_some_and(|(_, module)| {
                        matches!(module.kind(), ModuleKind::Switch | ModuleKind::Dimmer)
                    })
                    || config.composed_resource(id).is_some() && config.composed_light(id).is_none()
                {
                    return Err("HAP light selection is not an enabled logical light".into());
                }
                let name = config.composed_light(id).map_or_else(|| &config.devices()[id].name, |(_, light)| &light.name);
                if name.trim().is_empty() || name.len() > 64 || name.chars().any(char::is_control) {
                    return Err("invalid HAP light name".into());
                }
            }
            panel_specs(&bridge.panels, config)?;
            validate_panel_ownership(&bridge.panels,document)?;
            for id in selected_lights.iter().chain(&bridge.motors).chain(&bridge.panels) {
                if !devices.insert(id.clone()) {
                    return Err("device published by multiple HAP bridges".into());
                }
            }
            for id in &bridge.scenes {
                if !scenes.insert(id) {
                    return Err("scene published by multiple HAP bridges".into());
                }
            }
        }
        Ok(plan)
    }
    /// Validate before use; bridge IDs are safe single path components.
    pub fn state_directory(&self, base: &Path, id: &str) -> Result<PathBuf, String> {
        if id.is_empty()
            || id.len() > 48
            || !id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
            || !self.bridges.iter().any(|bridge| bridge.id == id)
        {
            return Err("unknown HAP bridge".into());
        }
        Ok(base.join("hap-bridges").join(id))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_lights_selects_composed_and_ordinary_without_raw_supply() {
        let raw=serde_json::json!({"schema_version":2,"revision":1,
            "rooms":[{"id":"room","name":"Room"}],
            "gateways":[{"id":"bus","endpoint":"127.0.0.1:1","modules":[
                {"id":"relay","address":"31","profile":"r8i8_din_pro_v2_7_7","verification":{"status":"simulated"}},
                {"id":"dimmer","address":"32","profile":"ad8_din_rs485_v2_2_1","verification":{"status":"simulated"}}]}],
            "devices":[
                {"id":"supply","name":"Raw supply","room":"room","module_id":"relay","channel":1,"enabled":true},
                {"id":"composed","name":"Raw dimmer","room":"room","module_id":"dimmer","channel":1,"enabled":true,"default_on_level":100},
                {"id":"ordinary","name":"Ordinary","room":"room","module_id":"dimmer","channel":2,"enabled":true,"default_on_level":100}],
            "composed_lights":[{"id":"matrix","review_evidence":"synthetic reviewed wiring","lights":[
                {"id":"composed","name":"Reviewed lamp","members":[{"supply":"supply","dimmer":"composed"}],"default_on_level":70}]}]});
        let config=homesense_config::Configuration::parse(&serde_json::to_vec(&raw).unwrap(),homesense_core::ExecutionContext::Simulation).unwrap();
        let doc=homesense_config::rule_document::Document::parse(br#"{"schema_version":1,"configuration_revision":1,"variables":[],"rules":[]}"#,&config).unwrap();
        let selected=all_light_ids(&config);
        assert_eq!(selected.iter().map(DeviceId::as_str).collect::<Vec<_>>(),vec!["composed","ordinary"]);
        let mut hidden=raw.clone();
        hidden["devices"][1]["catalog_visible"]=false.into();
        let hidden=homesense_config::Configuration::parse(&serde_json::to_vec(&hidden).unwrap(),homesense_core::ExecutionContext::Simulation).unwrap();
        assert_eq!(all_light_ids(&hidden).iter().map(DeviceId::as_str).collect::<Vec<_>>(),vec!["composed","ordinary"]);
        let mut raw_hidden_ordinary=raw.clone();
        raw_hidden_ordinary["devices"][2]["catalog_visible"]=false.into();
        let raw_hidden_ordinary=homesense_config::Configuration::parse(&serde_json::to_vec(&raw_hidden_ordinary).unwrap(),homesense_core::ExecutionContext::Simulation).unwrap();
        assert_eq!(all_light_ids(&raw_hidden_ordinary).iter().map(DeviceId::as_str).collect::<Vec<_>>(),vec!["composed"]);
        let plan=serde_json::json!({"schema_version":1,"bridges":[{"id":"home","name":"Home","address":"127.0.0.1:51826","all_lights":true}]});
        assert_eq!(Plan::parse(&serde_json::to_vec(&plan).unwrap(),&config,&doc).unwrap().bridges[0].selected_lights(&config),selected);
        let mut invalid=plan;
        invalid["bridges"][0]["all_lights"]=false.into();
        invalid["bridges"][0]["lights"]=serde_json::json!(["supply"]);
        assert!(Plan::parse(&serde_json::to_vec(&invalid).unwrap(),&config,&doc).is_err());
    }
    #[test]
    fn panel_selection_uses_verified_logical_bindings_and_rejects_other_devices() {
        let mut raw: serde_json::Value =
            serde_json::from_slice(include_bytes!("../../../config/simulation.json")).unwrap();
        raw["gateways"][0]["modules"].as_array_mut().unwrap().push(serde_json::json!({"address":"41","profile":"ts6_86_rs485_v2_6_5_6key","verification":{"status":"simulated"}}));
        for (id, channel, enabled) in [
            ("key-two", 2, true),
            ("key-six", 6, true),
            ("disabled", 1, false),
        ] {
            raw["devices"].as_array_mut().unwrap().push(serde_json::json!({"id":id,"name":id,"gateway":"simulation","address":"41","channel":channel,"enabled":enabled}));
        }
        let config = homesense_config::Configuration::parse(
            &serde_json::to_vec(&raw).unwrap(),
            homesense_core::ExecutionContext::Simulation,
        )
        .unwrap();
        let id = |v: &str| DeviceId::new(v).unwrap();
        let specs = panel_specs(&[id("key-six"), id("key-two")], &config).unwrap();
        assert_eq!(specs.iter().map(|s| s.key).collect::<Vec<_>>(), vec![6, 2]);
        assert_eq!(specs[0].name, "key-six");
        let plan=serde_json::json!({"schema_version":1,"bridges":[{"id":"panel","name":"Panel","address":"127.0.0.1:51826","lights":[],"scenes":[],"motors":[],"panels":["key-two"]}]});
        let document=homesense_config::rule_document::Document::parse(br#"{"schema_version":1,"configuration_revision":1,"variables":[],"rules":[]}"#,&config).unwrap();
        assert!(Plan::parse(&serde_json::to_vec(&plan).unwrap(),&config,&document).is_ok());
        let mut duplicate=plan.clone();
        let mut second=plan["bridges"][0].clone();second["id"]="other".into();second["address"]="127.0.0.1:51827".into();duplicate["bridges"].as_array_mut().unwrap().push(second);
        assert!(Plan::parse(&serde_json::to_vec(&duplicate).unwrap(),&config,&document).unwrap_err().contains("multiple"));

        let mut document=document;
        document.rules.push(homesense_config::rule_document::Rule {id:"active-panel".into(),enabled:true,review_evidence:Some("synthetic".into()),source:"synthetic".into(),trigger:homesense_config::rule_document::Trigger::Panel{device:"key-two".into(),event:homesense_config::rule_document::PanelEvent::Click},actions:vec![]});
        assert!(Plan::parse(&serde_json::to_vec(&plan).unwrap(),&config,&document).unwrap_err().contains("already executes"));
        document.rules[0].enabled=false;
        assert!(Plan::parse(&serde_json::to_vec(&plan).unwrap(),&config,&document).is_ok());
        document.holds=Some(homesense_config::rule_document::Holds{interval_ms:100,increment:10,bindings:vec![homesense_config::rule_document::Hold{key:"key-two".into(),device:"simulation:light".into(),up:true,enabled:true,review_evidence:Some("synthetic".into()),release:homesense_config::rule_document::HoldRelease::CancelPendingOnly}]});
        assert!(Plan::parse(&serde_json::to_vec(&plan).unwrap(),&config,&document).unwrap_err().contains("already executes"));
        document.holds.as_mut().unwrap().bindings[0].enabled=false;
        assert!(Plan::parse(&serde_json::to_vec(&plan).unwrap(),&config,&document).is_ok());

        assert!(panel_specs(&[], &config).unwrap().is_empty());
        for selection in [
            vec![id("missing")],
            vec![id("disabled")],
            vec![id("simulation:light")],
            vec![id("key-two"), id("key-two")],
            vec![id("key-two"); 257],
        ] {
            assert!(panel_specs(&selection, &config).is_err());
        }
    }
    #[test]
    fn disjoint_bridges_have_stable_separate_directories() {
        let mut raw: serde_json::Value =
            serde_json::from_slice(include_bytes!("../../../config/simulation.json")).unwrap();
        let mut second = raw["devices"][0].clone();
        second["id"] = "simulation:second".into();
        second["channel"] = 2.into();
        raw["devices"].as_array_mut().unwrap().push(second);
        let config = homesense_config::Configuration::parse(
            &serde_json::to_vec(&raw).unwrap(),
            homesense_core::ExecutionContext::Simulation,
        )
        .unwrap();
        let doc = homesense_config::rule_document::Document::parse(
            br#"{"schema_version":1,"configuration_revision":1,"variables":[],"rules":[]}"#,
            &config,
        )
        .unwrap();
        let mut raw = serde_json::json!({"schema_version":1,"bridges":[
            {"id":"f1","name":"First","address":"127.0.0.1:51826","lights":["simulation:light"],"scenes":[],"motors":[]},
            {"id":"f2","name":"Second","address":"127.0.0.1:51827","lights":["simulation:second"],"scenes":[],"motors":[]}]});
        let parse =
            |raw: &serde_json::Value| Plan::parse(&serde_json::to_vec(raw).unwrap(), &config, &doc);
        let first = parse(&raw).unwrap();
        let path = first.state_directory(Path::new("root"), "f1").unwrap();
        assert_ne!(
            path,
            first.state_directory(Path::new("root"), "f2").unwrap()
        );
        raw["bridges"].as_array_mut().unwrap().reverse();
        raw["bridges"][1]["name"] = "Renamed".into();
        assert_eq!(
            parse(&raw)
                .unwrap()
                .state_directory(Path::new("root"), "f1")
                .unwrap(),
            path
        );
        let mut bad = raw.clone();
        bad["bridges"][1]["id"] = bad["bridges"][0]["id"].clone();
        assert!(parse(&bad).is_err());
        let mut bad = raw.clone();
        bad["bridges"][1]["address"] = bad["bridges"][0]["address"].clone();
        assert!(parse(&bad).is_err());
        let mut bad = raw.clone();
        bad["bridges"][0]["lights"] = serde_json::json!(["simulation:second", "simulation:second"]);
        assert!(parse(&bad).is_err());
    }
    #[test]
    fn explicit_selection_rejects_duplicate_ownership_and_unsafe_identity_paths() {
        let config = homesense_config::Configuration::parse(
            include_bytes!("../../../config/simulation.json"),
            homesense_core::ExecutionContext::Simulation,
        )
        .unwrap();
        let doc = homesense_config::rule_document::Document::parse(
            br#"{"schema_version":1,"configuration_revision":1,"variables":[],"rules":[]}"#,
            &config,
        )
        .unwrap();
        let base = serde_json::json!({"schema_version":1,"bridges":[{"id":"floor-1","name":"Floor 1","address":"127.0.0.1:51826","lights":["simulation:light"],"scenes":[],"motors":[]}]});
        let parse = |value: &serde_json::Value| {
            Plan::parse(&serde_json::to_vec(value).unwrap(), &config, &doc)
        };
        let plan = parse(&base).unwrap();
        assert_eq!(
            plan.state_directory(Path::new("root"), "floor-1").unwrap(),
            Path::new("root/hap-bridges/floor-1")
        );
        assert!(plan.state_directory(Path::new("root"), "missing").is_err());
        for id in ["../identity", "/tmp/a", "UPPER", ""] {
            let mut bad = base.clone();
            bad["bridges"][0]["id"] = id.into();
            assert!(parse(&bad).is_err());
        }
        let mut duplicate = base.clone();
        let mut second = base["bridges"][0].clone();
        second["id"] = "floor-2".into();
        second["address"] = "127.0.0.1:51827".into();
        duplicate["bridges"].as_array_mut().unwrap().push(second);
        assert!(parse(&duplicate).unwrap_err().contains("multiple"));
        for address in ["0.0.0.0:51826", "8.8.8.8:51826", "127.0.0.1:0"] {
            let mut bad = base.clone();
            bad["bridges"][0]["address"] = address.into();
            assert!(parse(&bad).is_err());
        }
        let mut bad = base.clone();
        bad["bridges"][0]["lights"] = serde_json::json!(["missing"]);
        assert!(parse(&bad).is_err());
        let mut bad = base.clone();
        bad["bridges"][0]["typo"] = true.into();
        assert!(parse(&bad).is_err());
        assert!(Plan::parse(&vec![b' '; 65537], &config, &doc).is_err());
    }
}
