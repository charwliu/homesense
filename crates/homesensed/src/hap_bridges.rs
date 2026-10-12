//! Prepare explicit independent identities; no listeners or gateway writes.
use crate::{hap_bridge_plan::Plan, hap_endpoint::Settings};
use std::{collections::BTreeSet, path::Path};
pub struct PreparedBridge {
    pub id: String,
    pub name: String,
    pub settings: Settings,
    pub identity: homesense_hap::storage::IdentityStore,
    pub lights: BTreeSet<homesense_core::DeviceId>,
}
fn private_directory(path: &Path) -> Result<(), String> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_dir() => Ok(()),
        Ok(_) => Err("HAP bridge directory must be a real directory".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut builder = std::fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            builder.create(path).map_err(|e| e.to_string())
        }
        Err(error) => Err(error.to_string()),
    }
}
/// Validate the complete plan before creating any bridge identity. Existing
/// identities are retained; endpoint changes require a separate migration.
pub async fn prepare(
    bytes: &[u8],
    config: &homesense_config::Configuration,
    document: &homesense_config::rule_document::Document,
    base: &Path,
) -> Result<Vec<PreparedBridge>, String> {
    let plan = Plan::parse(bytes, config, document)?;
    private_directory(base)?;
    private_directory(&base.join("hap-bridges"))?;
    let mut prepared = Vec::new();
    for bridge in &plan.bridges {
        let directory = plan.state_directory(base, &bridge.id)?;
        private_directory(&directory)?;
        let settings = Settings {
            address: bridge.address,
            scenes: bridge.scenes.clone(),
            motors: bridge.motors.clone(),
        };
        let identity = settings.identity_named(&directory, &bridge.name).await?;
        prepared.push(PreparedBridge {
            id: bridge.id.clone(),
            name: bridge.name.clone(),
            settings,
            identity,
            lights: bridge.lights.iter().cloned().collect(),
        });
    }
    Ok(prepared)
}
#[cfg(test)]
mod tests {
    use super::*;
    use hap::storage::Storage;
    #[tokio::test]
    async fn identities_are_separate_stable_on_rename_and_invalid_plans_are_inert() {
        let root = std::env::temp_dir().join(format!(
            "homesense-bridge-identities-{}",
            uuid::Uuid::new_v4()
        ));
        let mut raw: serde_json::Value =
            serde_json::from_slice(include_bytes!("../../../config/simulation.json")).unwrap();
        let mut second = raw["devices"][0].clone();
        second["id"] = "second".into();
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
        let mut plan = serde_json::json!({"schema_version":1,"bridges":[{"id":"f1","name":"First floor","address":"127.0.0.1:51826","lights":["simulation:light"],"scenes":[],"motors":[]},{"id":"f2","name":"Second floor","address":"127.0.0.1:51827","lights":["second"],"scenes":[],"motors":[]}]});
        assert!(prepare(b"{}", &config, &doc, &root).await.is_err());
        assert!(!root.exists());
        let mut prepared = prepare(&serde_json::to_vec(&plan).unwrap(), &config, &doc, &root)
            .await
            .unwrap();
        let before = prepared[0].identity.load_config().await.unwrap();
        let other = prepared[1].identity.load_config().await.unwrap();
        assert_ne!(before.device_id, other.device_id);
        assert_ne!(
            before.device_ed25519_keypair.to_bytes(),
            other.device_ed25519_keypair.to_bytes()
        );
        let controller = uuid::Uuid::new_v4();
        prepared[0]
            .identity
            .save_pairing(&hap::pairing::Pairing::new(
                controller,
                hap::pairing::Permissions::Admin,
                [9; 32],
            ))
            .await
            .unwrap();
        drop(prepared);
        plan["bridges"][0]["name"] = "Renamed floor".into();
        let prepared = prepare(&serde_json::to_vec(&plan).unwrap(), &config, &doc, &root)
            .await
            .unwrap();
        let after = prepared[0].identity.load_config().await.unwrap();
        assert_eq!(after.name, "Renamed floor");
        assert_eq!(before.device_id, after.device_id);
        assert_eq!(
            before.device_ed25519_keypair.to_bytes(),
            after.device_ed25519_keypair.to_bytes()
        );
        assert!(prepared[0].identity.load_pairing(&controller).await.is_ok());
        assert!(
            prepared[1]
                .identity
                .load_pairing(&controller)
                .await
                .is_err()
        );
        let legacy = prepared[0]
            .settings
            .identity(&root.join("hap-bridges/f1"))
            .await
            .unwrap();
        assert_eq!(legacy.load_config().await.unwrap().name, "Renamed floor");
        drop(legacy);
        assert!(!root.join("identity").exists());
        drop(prepared);
        #[cfg(unix)]
        {
            let linked = root.join("linked");
            std::os::unix::fs::symlink(root.join("hap-bridges"), &linked).unwrap();
            assert!(private_directory(&linked).is_err());
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(root.join("hap-bridges/f1"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
