//! Unpublished scene action accessories. Selection and runtime authorization
//! belong to the service owner; construction never runs a scene or opens a port.
use crate::{
    graph::{information, install, resources},
    identifiers::{AccessoryDefinition, AccessoryKey},
    storage::IdentityStore,
};
use hap::{Error, Result, accessory::switch::SwitchAccessory, characteristic::HapCharacteristic};
use std::collections::BTreeMap;

#[derive(Clone, Debug)]
pub struct SceneSpec {
    pub id: String,
    pub name: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SceneBinding {
    pub scene: String,
    pub aid: u64,
    pub on_iid: u64,
}
pub struct SceneGraph {
    pub scenes: BTreeMap<String, SwitchAccessory>,
    pub bindings: Vec<SceneBinding>,
}
fn accessory(aid: u64, name: &str) -> Result<SwitchAccessory> {
    let mut accessory = SwitchAccessory::new(aid, information(aid, name))?;
    accessory.switch.name = None;
    Ok(accessory)
}
impl SceneGraph {
    /// Uses the same identity store as the light graph. Removed scene IDs remain
    /// reserved; rename, reorder and re-add do not change AID/IID assignments.
    pub async fn build(store: &IdentityStore, specs: Vec<SceneSpec>) -> Result<Self> {
        Self::build_role(store, specs, false).await
    }
    async fn build_role(
        store: &IdentityStore,
        specs: Vec<SceneSpec>,
        activity: bool,
    ) -> Result<Self> {
        let key = |id: String| {
            if activity {
                AccessoryKey::SceneActivity(id)
            } else {
                AccessoryKey::Scene(id)
            }
        };
        if specs.len() > 4095 {
            return Err(Error::Storage);
        }
        let mut sorted = BTreeMap::new();
        for spec in specs {
            if spec.id.trim().is_empty()
                || spec.id.len() > 250
                || spec.id.chars().any(char::is_control)
                || spec.name.trim().is_empty()
                || spec.name.len() > 64
                || spec.name.chars().any(char::is_control)
                || sorted.insert(spec.id.clone(), spec).is_some()
            {
                return Err(Error::Storage);
            }
        }
        let mut scenes = BTreeMap::new();
        let mut bindings = Vec::new();
        if sorted.is_empty() {
            return Ok(Self { scenes, bindings });
        }
        let definitions = sorted
            .values()
            .map(|spec| {
                Ok(AccessoryDefinition {
                    key: key(spec.id.clone()),
                    resources: resources(&accessory(2, &spec.name)?)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let identifiers = store.allocate_identifiers(definitions).await?;
        for (id, spec) in sorted {
            let ids = &identifiers[&key(id.clone())];
            let mut switch = accessory(ids.aid, &spec.name)?;
            install(&mut switch, ids)?;
            if activity {
                switch.switch.power_state.set_perms(vec![
                    hap::characteristic::Perm::PairedRead,
                    hap::characteristic::Perm::Events,
                ]);
            }
            bindings.push(SceneBinding {
                scene: id.clone(),
                aid: ids.aid,
                on_iid: switch.switch.power_state.get_id(),
            });
            scenes.insert(id, switch);
        }
        Ok(Self { scenes, bindings })
    }
}

/// Role-typed read-only activity addresses, authorized by the canonical scene.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SceneActivityBinding {
    pub scene: String,
    pub aid: u64,
    pub on_iid: u64,
}
pub struct SceneActivityGraph {
    pub activities: BTreeMap<String, SwitchAccessory>,
    pub bindings: Vec<SceneActivityBinding>,
}
impl SceneActivityGraph {
    pub async fn build(store: &IdentityStore, specs: Vec<SceneSpec>) -> Result<Self> {
        let graph = SceneGraph::build_role(store, specs, true).await?;
        Ok(Self {
            activities: graph.scenes,
            bindings: graph
                .bindings
                .into_iter()
                .map(|b| SceneActivityBinding {
                    scene: b.scene,
                    aid: b.aid,
                    on_iid: b.on_iid,
                })
                .collect(),
        })
    }
}
