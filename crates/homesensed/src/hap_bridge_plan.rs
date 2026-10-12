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
    #[serde(deserialize_with = "device_ids")]
    pub lights: Vec<DeviceId>,
    pub scenes: Vec<String>,
    #[serde(deserialize_with = "device_ids")]
    pub motors: Vec<DeviceId>,
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
            if bridge.lights.is_empty() && bridge.scenes.is_empty() && bridge.motors.is_empty() {
                return Err("empty HAP bridge publication".into());
            }
            let settings = crate::hap_endpoint::Settings {
                address: bridge.address,
                scenes: bridge.scenes.clone(),
                motors: bridge.motors.clone(),
            };
            settings.validate(config, document)?;
            for id in &bridge.lights {
                if supplies.contains(id)
                    || !config.binding(id).is_some_and(|(_, module)| {
                        matches!(module.kind(), ModuleKind::Switch | ModuleKind::Dimmer)
                    })
                {
                    return Err("HAP light selection is not an enabled logical light".into());
                }
            }
            for id in bridge.lights.iter().chain(&bridge.motors) {
                if !devices.insert(id) {
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
