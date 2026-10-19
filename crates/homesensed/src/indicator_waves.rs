//! Finite unadmitted LED metadata; each wave prepares current proofs afresh.
use crate::{admission::{Admission,TaskGeneration},feedback::Feedback,indicator_sync::{Evaluation,Synchronizer},scene_scheduler::SceneScheduler};
use homesense_config::rule_document::Document;
use homesense_core::{DeviceId,Observation,Percent};
use homesense_state::Quality;
use hap::server::WriteStatus;
use std::collections::BTreeMap;
use uuid::Uuid;
type PhysicalSelections=BTreeMap<String,(Option<homesense_core::rule::Value>,bool,bool)>;
#[derive(Clone,Debug,PartialEq,Eq)]
struct Signal {
    lights:Vec<(DeviceId,Option<Observation>,Quality)>,
    variables:BTreeMap<String,homesense_storage::variables::Stored>,
    recorded:BTreeMap<DeviceId,Percent>,
    pending:BTreeMap<DeviceId,Percent>,
    physical:PhysicalSelections,
}
#[derive(Clone)]
struct Target {link:usize,id:DeviceId}
struct Cycle {
    generation:TaskGeneration,signal:Signal,deadline:u64,targets:Vec<Target>,cursor:usize,
    expected:BTreeMap<DeviceId,bool>,unresolved:usize,not_required:usize,waves:usize,admitted:usize,
}
struct Pending {targets:Vec<(Target,Evaluation)>,complete:Option<bool>}
#[derive(Clone,Debug,PartialEq,Eq)]
pub(crate) enum Kind {Synced,Changed,Unresolved,Aborted}
#[derive(Clone,Debug)]
pub(crate) struct Outcome {pub kind:Kind,pub targets:usize,pub admitted:usize,pub waves:usize,pub unresolved:usize}
pub(crate) struct Wave {pub batches:Vec<Vec<u64>>,pub deadline:u64}
#[derive(Default)]
pub(crate) struct Waves {
    sync:Synchronizer,cycle:Option<Cycle>,pending:Option<Pending>,last_signal:Option<Signal>,
    last_sequence:Option<u64>,last_version:Option<u64>,next_probe:u64,
    pub(crate) outcome:Option<Outcome>,
}
impl Waves {
    pub(crate) fn phase_ended(&mut self,all_written:bool) {if let Some(pending)=&mut self.pending {pending.complete=Some(all_written);}}
    fn abort(&mut self) {
        if let Some(c)=self.cycle.take(){log::debug!("LED cycle aborted targets={} visited={} admitted={} waves={} unresolved={}",c.targets.len(),c.cursor,c.admitted,c.waves,c.unresolved);self.outcome=Some(Outcome {kind:Kind::Aborted,targets:c.targets.len(),admitted:c.admitted,waves:c.waves,unresolved:c.unresolved+c.targets.len().saturating_sub(c.cursor)});self.last_signal=Some(c.signal);}
        self.pending=None;
    }
    async fn signal(admission:&Admission,controller:Uuid,generation:&TaskGeneration,document:&Document,feedback:&Feedback)->Result<Signal,WriteStatus> {
        let snapshot=feedback.snapshot();let mut ids=document.indicator_links.iter().filter(|l|l.enabled).flat_map(|l|l.lights.iter().cloned()).collect::<std::collections::BTreeSet<_>>();
        // A combined light's retained dimmer level does not change when its
        // supply relay turns off. Cache every physical input used by planning.
        let supplies=ids.iter().filter_map(|id|feedback.supply_relay(id).cloned()).collect::<Vec<_>>();
        ids.extend(supplies);
        let lights=ids.iter().filter_map(|id|snapshot.devices.get(id).map(|s|(id.clone(),s.observed,s.quality))).collect();
        let names:Vec<_>=document.scene_switch_groups.iter().map(|g|g.variable.clone()).collect::<std::collections::BTreeSet<_>>().into_iter().collect();
        let variables=admission.variable_snapshot(controller,generation.clone(),names.clone()).await?;
        // Native completion changes pending/selection without necessarily
        // changing a lamp value or the persisted source variable. Do not
        // include observation timestamps: identical replies are not new edges.
        let physical=admission.scene_selection_snapshot(controller,generation.clone(),names).await?.into_iter().filter_map(|(name,state)| {
            state.physical.map(|p|(name,(state.stored.value,p.pending_relative,p.observed_ms.is_some())))
        }).collect();
        let recorded=admission.recorded_dimmer_levels(controller,generation.clone()).await?;
        // A wave deferred by a local transition must be reconsidered when the
        // transition settles or expires, even if physical values stayed equal.
        let mut pending=admission.projection_levels(generation.epochs.configuration,snapshot.cursor.boot_id).await?;
        pending.retain(|id,_|ids.contains(id)&&snapshot.devices.get(id).is_some_and(|state|state.kind==homesense_core::ModuleKind::Dimmer));
        Ok(Signal {lights,variables,recorded,pending,physical})
    }
    fn affected_links(before:&Signal,after:&Signal,document:&Document,feedback:&Feedback)->std::collections::BTreeSet<usize> {
        let old=before.lights.iter().map(|(id,value,quality)|(id,(value,quality))).collect::<BTreeMap<_,_>>();
        let new=after.lights.iter().map(|(id,value,quality)|(id,(value,quality))).collect::<BTreeMap<_,_>>();
        let changed=old.keys().chain(new.keys()).filter(|id|old.get(*id)!=new.get(*id)).map(|id|(*id).clone())
            .chain(before.pending.keys().chain(after.pending.keys()).filter(|id|before.pending.get(*id)!=after.pending.get(*id)).cloned()).collect::<std::collections::BTreeSet<_>>();
        document.indicator_links.iter().enumerate().filter_map(|(index,link)| {
            if !link.enabled {return None;}
            let group=link.scene.as_ref().and_then(|scene|document.scene_switch_groups.iter().find(|g|g.members.iter().any(|m|&m.scene==scene)));
            let mut inputs=link.lights.iter().cloned().collect::<std::collections::BTreeSet<_>>();
            // Physical selection can depend on another member's light, not just
            // this link's named lights. Keep the complete reviewed group scope.
            if let Some(group)=group {for member in &group.members {if let Some(scene)=document.timed_scenes.get(&member.scene) {inputs.extend(scene.steps.iter().map(|s|s.device.clone()));}}}
            let supplies=inputs.iter().filter_map(|id|feedback.supply_relay(id).cloned()).collect::<Vec<_>>();inputs.extend(supplies);
            (inputs.iter().any(|id|changed.contains(id)) || group.is_some_and(|g|before.variables.get(&g.variable)!=after.variables.get(&g.variable)||before.physical.get(&g.variable)!=after.physical.get(&g.variable))).then_some(index)
        }).collect()
    }
    fn selection_color(target:&Target,signal:&Signal,document:&Document)->Option<bool> {
        let scene=document.indicator_links[target.link].scene.as_ref()?;
        let group=document.scene_switch_groups.iter().find(|group|group.members.iter().any(|member|&member.scene==scene))?;
        let member=group.members.iter().find(|member|&member.scene==scene)?;
        let value=if let Some((value,pending,observed))=signal.physical.get(&group.variable) {
            if *pending || !*observed {return None;}value.as_ref()?
        }else{signal.variables.get(&group.variable)?.value.as_ref()?};
        match value {homesense_core::rule::Value::Integer(value)=>Some(*value==member.value),_=>None}
    }
    async fn preferred(admission:&Admission,controller:Uuid,generation:&TaskGeneration,targets:&[Target],signal:&Signal,document:&Document,affected:&std::collections::BTreeSet<usize>)->Result<std::collections::BTreeSet<DeviceId>,WriteStatus> {
        // Ordering only: a projected color never authorizes a write. Each
        // selected target still gets a fresh Synchronizer proof at admission.
        let mut preferred=targets.iter().filter(|target|affected.contains(&target.link)
            && document.indicator_links[target.link].power_policy!=homesense_config::indicator_links::PowerPolicy::LegacyOff)
            .map(|target|target.id.clone()).collect::<std::collections::BTreeSet<_>>();
        let candidates=targets.iter().filter(|target|affected.contains(&target.link)).filter_map(|target|Self::selection_color(target,signal,document).map(|color|(target.id.clone(),color))).collect::<BTreeMap<_,_>>();
        if candidates.is_empty(){return Ok(preferred);}
        let outputs=Self::fences(admission,controller,generation,&candidates.keys().cloned().collect::<Vec<_>>()).await?;
        preferred.extend(candidates.into_iter().filter_map(|(id,color)|(!Self::matches(outputs.get(&id),color)).then_some(id)));
        Ok(preferred)
    }
    fn reprioritize(cycle:&mut Cycle,signal:Signal,affected:&std::collections::BTreeSet<usize>,preferred:&std::collections::BTreeSet<DeviceId>) {
        // Retain the original hard deadline and every unvisited target. At most
        // half a full wave jumps the queue, so hot inputs cannot starve the rest.
        let rotated=cycle.targets[cycle.cursor..].iter().chain(&cycle.targets[..cycle.cursor]).cloned().collect::<Vec<_>>();
        let mut urgent=vec![];
        for (index,target) in rotated.iter().enumerate(){if urgent.len()<8 && affected.contains(&target.link) && preferred.contains(&target.id){urgent.push(index);}}
        for (index,target) in rotated.iter().enumerate(){if urgent.len()<8 && affected.contains(&target.link) && !urgent.contains(&index){urgent.push(index);}}
        cycle.targets=urgent.iter().map(|index|rotated[*index].clone()).chain(rotated.iter().enumerate().filter(|(index,_)|!urgent.contains(index)).map(|(_,target)|target.clone())).collect();cycle.cursor=0;cycle.signal=signal;
    }
    async fn fences(admission:&Admission,controller:Uuid,generation:&TaskGeneration,ids:&[DeviceId])->Result<crate::indicator_outputs::Fence,WriteStatus> {
        let mut result=BTreeMap::new();for chunk in ids.chunks(128) {result.extend(admission.indicator_outputs(controller,generation.clone(),chunk.to_vec(),vec![]).await?.fence);}Ok(result)
    }
    fn matches(output:Option<&crate::indicator_outputs::Output>,color:bool)->bool {
        output.is_some_and(|value|value.written.as_ref().is_some_and(|w|w.color==Some(color))
            && value.requested.as_ref().is_none_or(|r|matches!(r.status.as_str(),"written"|"observed")))
    }
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn advance(&mut self,admission:&Admission,controller:Uuid,document:&Document,scenes:&SceneScheduler,feedback:&Feedback,max_age:u64,gap_ms:u64,qualified_round:bool,query_pending:bool)->Result<Option<Wave>,String> {
        if !document.indicator_links.iter().any(|l|l.enabled) {return Ok(None);}
        let now=admission.monotonic_ms();
        if !admission.source_valid(){self.abort();return Ok(None);}
        if self.cycle.as_ref().is_some_and(|c|now>=c.deadline){self.abort();return Ok(None);}
        let version=feedback.snapshot().state_version;
        if self.cycle.is_none() && !qualified_round && self.last_version==Some(version) && now<self.next_probe {return Ok(None);}
        let generation=match admission.task_generation().await {Ok(value)=>value,Err(WriteStatus::ResourceBusy)=>return Ok(None),Err(_)=>{self.abort();return Ok(None);}};
        if generation.epochs.configuration!=document.revision || self.cycle.as_ref().is_some_and(|c|c.generation!=generation){self.abort();return Ok(None);}
        if let Some(pending)=self.pending.take() {
            let Some(all_written)=pending.complete else {self.pending=Some(pending);return Ok(None);};
            let ids=pending.targets.iter().map(|(target,_)|target.id.clone()).collect::<Vec<_>>();
            let fences=match Self::fences(admission,controller,&generation,&ids).await {Ok(value)=>value,Err(WriteStatus::ResourceBusy)=>{self.pending=Some(Pending {complete:Some(all_written),..pending});return Ok(None);},Err(_)=>{self.abort();return Ok(None);}};
            if let Some(cycle)=&mut self.cycle {
                if !all_written {cycle.unresolved+=1;}
                for (target,evaluation) in pending.targets {
                    match evaluation {
                        Evaluation::NotRequired=>cycle.not_required+=1,
                        Evaluation::Unknown=>cycle.unresolved+=1,
                        Evaluation::Desired(color)=>{cycle.expected.insert(target.id.clone(),color);if !Self::matches(fences.get(&target.id),color){cycle.unresolved+=1;}}
                    }
                }
            }
            // One complete ordinary runtime/read/control quantum must precede
            // the next preparation, even if no periodic query is currently due.
            return Ok(None);
        }
        if query_pending || !scenes.publication_impact().is_empty(){return Ok(None);}
        let state=match admission.diagnostics(controller,generation.clone()).await {Ok(value)=>value,Err(WriteStatus::ResourceBusy)=>return Ok(None),Err(_)=>{self.abort();return Ok(None);}};
        if !state.ready(){self.abort();return Ok(None);}
        if state.queued!=0 || state.in_flight{return Ok(None);}
        let signal=match Self::signal(admission,controller,&generation,document,feedback).await {Ok(value)=>value,Err(WriteStatus::ResourceBusy)=>return Ok(None),Err(_)=>{self.abort();return Ok(None);}};
        let sequence=match admission.indicator_sequence(controller,generation.clone()).await {Ok(value)=>value,Err(WriteStatus::ResourceBusy)=>return Ok(None),Err(_)=>{self.abort();return Ok(None);}};
        self.last_version=Some(version);self.next_probe=now.saturating_add(250);
        if let Some(cycle)=&mut self.cycle && cycle.signal!=signal && cycle.cursor<cycle.targets.len() {
            let affected=Self::affected_links(&cycle.signal,&signal,document,feedback);
            self.outcome=Some(Outcome {kind:Kind::Changed,targets:cycle.targets.len(),admitted:cycle.admitted,waves:cycle.waves,unresolved:cycle.unresolved});
            if affected.is_empty() {cycle.signal=signal.clone();}else{
                let preferred=match Self::preferred(admission,controller,&generation,&cycle.targets,&signal,document,&affected).await{Ok(value)=>value,Err(WriteStatus::ResourceBusy)=>return Ok(None),Err(_)=>{self.abort();return Ok(None);}};
                Self::reprioritize(cycle,signal.clone(),&affected,&preferred);
            }
        }
        if self.cycle.as_ref().is_some_and(|c|c.cursor==c.targets.len()) {
            let cycle=self.cycle.take().unwrap();let ids=cycle.expected.keys().cloned().collect::<Vec<_>>();
            let fences=Self::fences(admission,controller,&generation,&ids).await.map_err(|e|format!("LED cycle verification: {e:?}"))?;
            let mismatch=cycle.expected.iter().any(|(id,color)|!Self::matches(fences.get(id),*color));
            let changed=signal!=cycle.signal;
            let kind=if changed {Kind::Changed}else if cycle.unresolved!=0 || mismatch {Kind::Unresolved}else{Kind::Synced};
            self.outcome=Some(Outcome {kind,targets:cycle.targets.len(),admitted:cycle.admitted,waves:cycle.waves,unresolved:cycle.unresolved+usize::from(mismatch)});
            if let Some(outcome)=&self.outcome {log::debug!("LED cycle {:?} targets={} admitted={} waves={} unresolved={}",outcome.kind,outcome.targets,outcome.admitted,outcome.waves,outcome.unresolved);}
            self.last_signal=Some(if changed {cycle.signal}else{signal});
            if !mismatch || cycle.unresolved!=0 {self.last_sequence=Some(sequence);}
            return Ok(None);
        }
        if self.cycle.is_none() {
            if !qualified_round && self.last_signal.as_ref()==Some(&signal) && self.last_sequence==Some(sequence) {return Ok(None);}
            let mut targets=document.indicator_links.iter().enumerate().filter(|(_,l)|l.enabled).flat_map(|(link,l)|l.leds.iter().cloned().map(move|id|Target {link,id})).collect::<Vec<_>>();
            // Initialize independent power indicators before the legacy scene
            // fanout. Unchanged OFF inputs otherwise never reprioritize these
            // endpoints, leaving their first confirmed color at the tail.
            // This changes ordering only; admission still proves every write.
            targets.sort_by_key(|target|document.indicator_links[target.link].power_policy==homesense_config::indicator_links::PowerPolicy::LegacyOff);
            if targets.is_empty(){return Ok(None);}
            let snapshot=feedback.snapshot();
            let horizon=signal.lights.iter().filter_map(|(id,_,quality)|(*quality==Quality::Confirmed).then(||snapshot.devices.get(id).and_then(|s|s.observed_at_ms)).flatten()).min().map(|at|at.saturating_add(max_age)).unwrap_or(now);
            let available=horizon.saturating_sub(now);let width=((available.min(2000)/2)/gap_ms.max(1)).min(16) as usize;
            if width==0 {self.last_signal=Some(signal);self.last_sequence=Some(sequence);return Ok(None);}
            let bound=(targets.len().div_ceil(width) as u64).saturating_mul(2000+5500+100);
            let mut cycle=Cycle {generation:generation.clone(),signal,deadline:horizon.min(now.saturating_add(bound)),targets,cursor:0,expected:BTreeMap::new(),unresolved:0,not_required:0,waves:0,admitted:0};
            // A native completion can arrive after the previous scan finished.
            // Apply the same priority at a fresh cycle, not only mid-scan.
            if let Some(previous)=&self.last_signal {
                let affected=Self::affected_links(previous,&cycle.signal,document,feedback);
                if !affected.is_empty(){
                    let preferred=match Self::preferred(admission,controller,&generation,&cycle.targets,&cycle.signal,document,&affected).await{Ok(value)=>value,Err(WriteStatus::ResourceBusy)=>return Ok(None),Err(_)=>{self.abort();return Ok(None);}};
                    let signal=cycle.signal.clone();Self::reprioritize(&mut cycle,signal,&affected,&preferred);
                }
            }
            self.cycle=Some(cycle);
        }
        // Metadata/owner reads above can be slow on the full catalog. Start
        // this delivery slot from the current clock, while retaining the
        // original cycle horizon and every command's proof/deadline checks.
        let now=admission.monotonic_ms();
        let cycle=self.cycle.as_ref().unwrap();let available=cycle.deadline.saturating_sub(now);
        // Retire only unadmitted metadata when the finite cycle cannot fund a
        // complete delivery slot. A shortened slot can elapse during the next
        // ordinary control quantum and cancel newly accepted LEDs well before
        // their own TTL. A later cycle must capture fresh proofs; no accepted
        // command or original observation horizon is renewed or replayed.
        if available<4000 {self.abort();return Ok(None);}
        let allowance=2000;
        let width=((allowance.min(2000)/2)/gap_ms.max(1)).min(16) as usize;
        if width==0 {self.abort();return Ok(None);}
        let targets=cycle.targets[cycle.cursor..cycle.targets.len().min(cycle.cursor+width)].to_vec();
        let phase_deadline=now.saturating_add(allowance.min(2000));
        let mut subset=document.clone();subset.indicator_links=targets.iter().map(|target|{let mut link=document.indicator_links[target.link].clone();link.leds=vec![target.id.clone()];link}).collect();
        let visited=self.sync.tick_wave(admission,controller,&subset,scenes,feedback,max_age,phase_deadline).await?;
        log::debug!("LED wave planned targets={} visited={} remaining_ms={}",targets.len(),visited,phase_deadline.saturating_sub(admission.monotonic_ms()));
        let batches=self.sync.take_emitted_batches();
        if visited==0 {return Ok(None);}
        let mut evaluations=vec![];for target in targets.into_iter().take(visited){let evaluation=self.sync.evaluation(&document.indicator_links[target.link].id,&target.id);evaluations.push((target,evaluation));}
        let count=batches.iter().map(Vec::len).sum::<usize>();if count>16 || count>visited {return Err("LED wave exceeded admitted target bound".into());}
        let cycle=self.cycle.as_mut().unwrap();cycle.cursor+=visited;cycle.waves+=1;cycle.admitted+=count;
        self.pending=Some(Pending {targets:evaluations,complete:batches.is_empty().then_some(true)});
        if batches.is_empty(){Ok(None)}else{Ok(Some(Wave {batches,deadline:phase_deadline}))}
    }
}
#[cfg(test)]
#[path="indicator_waves_tests.rs"]
mod tests;
