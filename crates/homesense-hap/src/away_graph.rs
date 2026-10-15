//! A separately identified momentary Away operation, never an imported scene.
use crate::{
    graph::{information, install, resources},
    identifiers::{AccessoryDefinition, AccessoryKey},
    scene_graph::SceneBinding,
    storage::IdentityStore,
};
use hap::{Result, accessory::switch::SwitchAccessory, characteristic::HapCharacteristic};
fn accessory(aid: u64) -> Result<SwitchAccessory> {
    let mut switch = SwitchAccessory::new(aid, information(aid, "离家全部关灯"))?;
    switch.switch.name = None;
    Ok(switch)
}
pub async fn build(store: &IdentityStore) -> Result<(SceneBinding, SwitchAccessory)> {
    let ids = store
        .allocate_identifiers(vec![AccessoryDefinition {
            key: AccessoryKey::AwayOff,
            resources: resources(&accessory(2)?)?,
        }])
        .await?;
    let ids = &ids[&AccessoryKey::AwayOff];
    let mut switch = accessory(ids.aid)?;
    install(&mut switch, ids)?;
    Ok((
        SceneBinding {
            scene: "away-off".into(),
            aid: ids.aid,
            on_iid: switch.switch.power_state.get_id(),
        },
        switch,
    ))
}
