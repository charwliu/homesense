//! Room-placement metadata from installed HAP bindings, never identity files.
//! AID-derived serials are meaningful only within an explicitly selected bridge.
use crate::{hap_bridge_group::Status, unified_hap::UnifiedHap};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind { Light, Fan, Scene, Curtain }

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Accessory {
    pub kind: Kind,
    pub id: String,
    pub aid: u64,
    pub serial_number: String,
    pub room_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Bridge {
    pub id: String,
    pub name: String,
    pub available: bool,
    pub accessories: Vec<Accessory>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Publication {
    pub boot: String,
    pub revision: String,
    pub generation: String,
    pub bridges: Vec<Bridge>,
}

impl Publication {
    pub(crate) fn new(boot: String, revision: u64, bridges: Vec<Bridge>) -> Self {
        Self { boot, revision:revision.to_string(), generation:uuid::Uuid::new_v4().to_string(), bridges }
    }
    /// Lifecycle status may only remove availability. This metadata cannot turn
    /// an uninstalled bridge into a published bridge or grant control scopes.
    pub(crate) fn current(&self, boot: &str, revision: u64, statuses: &BTreeMap<String,Status>) -> Option<Self> {
        if self.boot != boot || self.revision != revision.to_string() { return None; }
        let mut view=self.clone();
        for bridge in &mut view.bridges {
            bridge.available &= statuses.get(&bridge.id)==Some(&Status::Listening);
        }
        Some(view)
    }
}

pub(crate) fn installed_accessories(
    graph: &UnifiedHap,
    config: &homesense_config::Configuration,
    document: &homesense_config::rule_document::Document,
) -> Result<Vec<Accessory>,String> {
    let mut rows=vec![];
    for (binding,_) in &graph.lights.lights {
        let device=config.devices().get(&binding.device).ok_or("published light missing")?;
        rows.push(Accessory {kind:if graph.lights.fan_ids.contains(&binding.device){Kind::Fan}else{Kind::Light},id:binding.device.as_str().into(),aid:binding.aid,
            serial_number:format!("HS-{}",binding.aid),room_id:device.room.clone()});
    }
    if let Some(scenes)=&graph.scenes {
        for (binding,_) in &scenes.accessories {
            rows.push(Accessory {kind:Kind::Scene,id:binding.scene.clone(),aid:binding.aid,
                serial_number:format!("HS-{}",binding.aid),room_id:document.scene_rooms.get(&binding.scene).map(|room|room.id.clone())});
        }
    }
    if let Some(curtains)=&graph.window_coverings {
        for (binding,_) in &curtains.accessories {
            let device=config.devices().get(&binding.device).ok_or("published curtain missing")?;
            rows.push(Accessory {kind:Kind::Curtain,id:binding.device.as_str().into(),aid:binding.aid,
                serial_number:format!("HS-{}",binding.aid),room_id:device.room.clone()});
        }
    }
    let mut aids=BTreeSet::new();
    if rows.iter().any(|row|row.aid<=1 || !aids.insert(row.aid)) {return Err("invalid installed HAP placement bindings".into());}
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bridge_status_and_runtime_generation_fail_closed_without_reallocating_identity() {
        let row=Accessory {kind:Kind::Light,id:"lamp".into(),aid:2,serial_number:"HS-2".into(),room_id:Some("room".into())};
        let declaration=Publication::new("boot".into(),1,vec![Bridge{id:"one".into(),name:"Bridge".into(),available:true,accessories:vec![row.clone()]},Bridge{id:"two".into(),name:"Bridge".into(),available:true,accessories:vec![row]}]);
        let view=declaration.current("boot",1,&BTreeMap::from([("one".into(),Status::Listening),("two".into(),Status::Degraded)])).unwrap();
        assert!(view.bridges[0].available);assert!(!view.bridges[1].available);
        assert_eq!(view.bridges[0].accessories[0].serial_number,view.bridges[1].accessories[0].serial_number);
        assert!(declaration.current("new-boot",1,&BTreeMap::new()).is_none());
        assert!(declaration.current("boot",2,&BTreeMap::new()).is_none());
        assert!(declaration.current("boot",1,&BTreeMap::new()).unwrap().bridges.iter().all(|b|!b.available));
        let next=Publication::new("boot".into(),1,declaration.bridges.clone());
        assert_ne!(declaration.generation,next.generation);
        assert_eq!(declaration.bridges,next.bridges);
        let json=serde_json::to_value(view).unwrap();
        assert_eq!(json["bridges"][0]["accessories"][0]["room_id"],"room");
        assert!(json.get("pin").is_none());
    }
}
