//! Dedicated absolute full-module On proof; never exposed as a raw client batch.
use crate::{feedback::{Feedback, RuleLease}, automation_control::Action};
use homesense_config::{Configuration, rule_document::Document};
use homesense_core::{Command, rule::Action as RuleAction};
use homesense_hap::requests::{Batch, Intent};
use uuid::Uuid;
pub(crate) const RETRIGGER_INTERVAL_MS:u64=5_000;
/// Actor-owned admission throttle shared by App, HAP and wall Away entries.
/// It never delays/replays a request or applies to an operation's own retries.
#[derive(Default)]
pub(crate) struct Limiter {
    last:Option<(crate::admission::TaskGeneration,bool,u64)>,
    opening:Option<(crate::admission::TaskGeneration,Vec<u64>)>,
}
impl Limiter {
    #[cfg(test)]
    pub(crate) fn opening_commands(&self,generation:&crate::admission::TaskGeneration)->&[u64] {
        self.opening.as_ref().filter(|(g,_)|g==generation).map_or(&[],|(_,ids)|ids.as_slice())
    }
    pub(crate) fn permits(&self,generation:&crate::admission::TaskGeneration,on:bool,now:u64,away_active:bool,opening_pending:bool)->bool {
        if away_active || on && opening_pending {return false;}
        // Closing may preempt opening immediately. Reopening waits after either
        // direction, so alternating buttons cannot defeat the ordinary cooldown.
        !self.last.as_ref().is_some_and(|(g,previous,at)|g==generation && (on || !*previous)
            && now.checked_sub(*at).is_none_or(|age|age<RETRIGGER_INTERVAL_MS))
    }
    pub(crate) fn accepted(&mut self,generation:crate::admission::TaskGeneration,on:bool,now:u64,commands:Vec<u64>) {
        self.last=Some((generation.clone(),on,now));
        if on {self.opening=Some((generation,commands));} else {self.opening=None;}
    }
}

pub(crate) struct Prepared {
    controller: Uuid,
    revision: u64,
    scene: String,
    scope: Vec<homesense_core::DeviceId>,
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
        (u8::from(module.kind()==homesense_core::ModuleKind::Switch),device.gateway.clone(),module.address().get(),device.channel.get())
    });
    let scope=physical.iter().map(|intent|intent.device.clone()).collect();
    let (_,_,mut lease)=feedback.native_scene_capture(&[],now,max_age)?;
    let sample_age=feedback.continuing_max_age_ms(max_age);
    let mut skip=std::collections::BTreeSet::new();
    // Omit only a complete module whose eight raw physical samples already
    // match the absolute target. A cached pending target is never a sample.
    for group in physical.chunks(8) {
        if group.len()!=8 {return Err("household module is incomplete".into());}
        let (first,first_module)=config.binding(&group[0].device).ok_or("household binding missing")?;
        if !group.iter().all(|intent|config.binding(&intent.device).is_some_and(|(device,module)| {
            device.gateway==first.gateway && module.address()==first_module.address() && feedback.module_truth().received_port_state(&device.gateway,module.address().get(),device.channel.get())
                .is_some_and(|state|state.quality==homesense_state::Quality::Confirmed
                    && state.observed_at_ms.and_then(|at|now.checked_sub(at)).is_some_and(|age|age<feedback.effective_max_age_ms(sample_age))
                    && state.observed.is_some_and(|actual|intent.command.target_matches(actual)))
        })) {continue;}
        let ids=group.iter().map(|intent|intent.device.clone()).collect::<Vec<_>>();
        if let Ok((_,_,proof))=feedback.native_scene_capture(&ids,now,sample_age) {
            lease=lease.merge_physical(&proof)?;
            skip.extend(ids);
        }
    }
    physical.retain(|intent|!skip.contains(&intent.device));
    Ok(Prepared{controller,revision:document.revision,scene:scene.into(),scope,physical,lease})
}
impl Prepared {
    pub(crate) fn scope(&self)->Vec<homesense_core::DeviceId> {self.scope.clone()}
    pub(crate) fn same_observation(&self,proof:&crate::feedback::RuleLease,now:u64)->bool {self.lease.same_capture(proof,now)}
    pub(crate) fn compile(self,config:&Configuration,source:Option<&crate::source_auth::Source>)
        ->Result<(Batch,RuleLease),hap::server::WriteStatus> {
        use hap::server::WriteStatus;
        if config.revision()!=self.revision {return Err(WriteStatus::InvalidValueInRequest);}
        if let Some(source)=source {
            let original=Action::SceneAction{controller:self.controller,revision:self.revision,scene:self.scene,execute:true};
            if !source.matches(&original)||self.scope.iter().any(|id|!source.permits_device(id.as_str())) {
                return Err(WriteStatus::InsufficientPrivileges);
            }
        }
        Ok((Batch{controller:self.controller,intents:self.physical,subscriptions:vec![]},self.lease))
    }
}

/// A raw cached On is not stable while an older producer still owns a command.
/// Restore only complete reviewed modules; never create a new output scope.
pub(crate) fn restore_unsettled(config:&Configuration,scope:&[homesense_core::DeviceId],batch:&mut Batch,
    mut settled:impl FnMut(&homesense_core::DeviceId)->Result<bool,hap::server::WriteStatus>)->Result<(),hap::server::WriteStatus> {
    use hap::server::WriteStatus as W;
    for group in scope.chunks(8) {
        if group.len()!=8 {return Err(W::InvalidValueInRequest);}
        if group.iter().all(|id|batch.intents.iter().any(|i|&i.device==id)){continue;}
        if group.iter().any(|id|batch.intents.iter().any(|i|&i.device==id)){return Err(W::InvalidValueInRequest);}
        let mut stable=true;for id in group {stable &= settled(id)?;}
        if stable {continue;}
        for id in group {
            let (_,module)=config.binding(id).ok_or(W::InvalidValueInRequest)?;
            let command=match module.kind() {
                homesense_core::ModuleKind::Switch=>Command::Switch(true),
                homesense_core::ModuleKind::Dimmer=>Command::SetLevel {level:homesense_core::Percent::new(100).unwrap(),transition:homesense_core::LevelTransition::Immediate},
                _=>return Err(W::InvalidValueInRequest),
            };
            batch.intents.push(Intent {device:id.clone(),command});
        }
    }
    batch.intents.sort_by_key(|intent| {
        let (device,module)=config.binding(&intent.device).expect("reviewed household binding");
        (u8::from(module.kind()==homesense_core::ModuleKind::Switch),device.gateway.clone(),module.address().get(),device.channel.get())
    });
    Ok(())
}
