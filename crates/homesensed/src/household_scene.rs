//! Dedicated absolute full-module On proof; never exposed as a raw client batch.
use crate::{feedback::{Feedback, RuleLease}, automation_control::Action};
use homesense_config::{Configuration, rule_document::Document};
use homesense_core::{Command, rule::Action as RuleAction};
use homesense_hap::requests::{Batch, Intent};
use uuid::Uuid;
pub(crate) struct Prepared {
    controller: Uuid,
    revision: u64,
    scene: String,
    physical: Vec<Intent>,
    lease: RuleLease,
}
pub(crate) fn prepare(config:&Configuration, document:&Document, controller:Uuid, scene:&str,
    feedback:&Feedback, now:u64, max_age:u64)->Result<Prepared,String> {
    if config.revision()!=document.revision || document.whole_house_on_scene()!=Some(scene) {
        return Err("household On alias changed".into());
    }
    let definition=document.immediate_scenes.get(scene).ok_or("household recipe missing")?;
    let mut physical=definition.activate.iter().map(|node|match &node.action {
        RuleAction::Command{device,command} if (matches!(command,Command::Switch(true))||matches!(command,Command::SetLevel{level,transition:homesense_core::LevelTransition::Immediate} if level.get()==100))=>Ok(Intent{device:device.clone(),command:*command}),
        _=>Err("household recipe is not absolute".to_owned()),
    }).collect::<Result<Vec<_>,_>>()?;
    if physical.is_empty()||physical.len()>128 {return Err("household output bound invalid".into());}
    let expected=document.away_off.as_ref().ok_or("household module review missing")?.modules.iter().flat_map(|m|m.devices.iter().map(String::as_str)).collect::<std::collections::BTreeSet<_>>();
    let actual=physical.iter().map(|i|i.device.as_str()).collect::<std::collections::BTreeSet<_>>();
    if actual!=expected || actual.len()!=physical.len() {return Err("household physical scope changed".into());}
    physical.sort_by_key(|intent| {
        let (device,module)=config.binding(&intent.device).expect("validated household binding");
        (u8::from(module.kind()==homesense_core::ModuleKind::Switch),device.module_id.clone(),device.channel.get())
    });
    // Absolute full-module targets read no prior brightness or relay value.
    // Retain the same active boot/epoch lease without inventing observations.
    let (_,_,lease)=feedback.native_scene_capture(&[],now,max_age)?;
    Ok(Prepared{controller,revision:document.revision,scene:scene.into(),physical,lease})
}
impl Prepared {
    pub(crate) fn compile(self,config:&Configuration,source:Option<&crate::source_auth::Source>)
        ->Result<(Batch,RuleLease),hap::server::WriteStatus> {
        use hap::server::WriteStatus;
        if config.revision()!=self.revision {return Err(WriteStatus::InvalidValueInRequest);}
        if let Some(source)=source {
            let original=Action::SceneAction{controller:self.controller,revision:self.revision,scene:self.scene,execute:true};
            if !source.matches(&original)||self.physical.iter().any(|intent|!source.permits_device(intent.device.as_str())) {
                return Err(WriteStatus::InsufficientPrivileges);
            }
        }
        Ok((Batch{controller:self.controller,intents:self.physical,subscriptions:vec![]},self.lease))
    }
}
