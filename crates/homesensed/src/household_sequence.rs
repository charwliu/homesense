//! Admission-owned whole-house On sequencing. Future modules stay outside the
//! command queue; retrying creates new, independently audited command IDs.
use crate::{admission::TaskGeneration, away_execution::Observation, source_auth::Source};
use hap::server::WriteStatus as W;
use homesense_config::Configuration;
use homesense_core::{DeviceId, Command, module_sequence as core,
    queue::{QueuedCommand, Origin, CancelReason}, arbiter::{Arbiter, BatchCommitError}, admission::Access};
use homesense_hap::requests::Batch;
use homesense_storage::{Store, journal::Status};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Accepted { pub operation: String, pub commands: Vec<u64> }
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct View {
    pub operation: String,
    pub revision: String,
    pub state: String,
    pub confirmed_modules: usize,
    pub current_module: Option<String>,
    pub reason: Option<String>,
}
pub(crate) struct Job {
    pub generation: TaskGeneration,
    pub controller: Uuid,
    pub source: Option<Source>,
    pub wires: Vec<homesense_core::gateway_scope::WireScope>,
    pub manual: BTreeMap<DeviceId, u64>,
    engine: core::Sequence,
    operation: String,
    ids: BTreeSet<u64>,
    current_ids: BTreeSet<u64>,
    before_counts: BTreeMap<DeviceId,u64>,
    written: BTreeMap<u64, u64>,
    modules: BTreeMap<String, (String, u8)>,
    reads: BTreeSet<DeviceId>,
    last_view: Option<View>,
    required:BTreeMap<DeviceId,Command>,
    applied:BTreeSet<DeviceId>,
    failure:Option<String>,
    retired_queue:bool,
    skipped_modules:usize,
}
impl Job {
    #[allow(clippy::too_many_arguments)]
    pub fn new(config: &Configuration, batch: Batch, generation: TaskGeneration,
               source: Option<Source>, wires: Vec<homesense_core::gateway_scope::WireScope>,
               manual: &BTreeMap<DeviceId,u64>, scope: Vec<DeviceId>, serial: u64, now: u64) -> Result<Self, W> {
        let mut groups = Vec::<core::Module>::new();
        let mut modules = BTreeMap::new();
        for chunk in batch.intents.chunks(8) {
            if chunk.len()!=8 { return Err(W::InvalidValueInRequest); }
            let (first,module) = config.binding(&chunk[0].device).ok_or(W::InvalidValueInRequest)?;
            let mut channels = BTreeSet::new();
            let id = format!("{}/{:02X}",first.gateway,module.address().get());
            for intent in chunk {
                let (binding,spec) = config.binding(&intent.device).ok_or(W::InvalidValueInRequest)?;
                if binding.gateway!=first.gateway || spec.address()!=module.address() || !channels.insert(binding.channel.get()) {
                    return Err(W::InvalidValueInRequest);
                }
            }
            if channels!=(1..=8).collect() {return Err(W::InvalidValueInRequest);}
            groups.push(core::Module {id:id.clone(),targets:chunk.iter().map(|i|(i.device.clone(),i.command)).collect()});
            modules.insert(id,(first.gateway.clone(),module.address().get()));
        }
        let engine=core::Sequence::new(groups.clone(),now).map_err(|_|W::InvalidValueInRequest)?;
        let required=scope.iter().map(|id| {
            let (_,module)=config.binding(id).ok_or(W::InvalidValueInRequest)?;
            let command=if module.kind()==homesense_core::ModuleKind::Dimmer {Command::SetLevel {level:homesense_core::Percent::new(100).unwrap(),transition:homesense_core::LevelTransition::Immediate}}else{Command::Switch(true)};
            Ok((id.clone(),command))
        }).collect::<Result<BTreeMap<_,_>,W>>()?;
        let queued=groups.iter().flat_map(|m|m.targets.keys().cloned()).collect::<BTreeSet<_>>();
        let applied=required.keys().filter(|id|!queued.contains(*id)).cloned().collect();
        let skipped_modules=scope.len()/8-groups.len();
        let manual=scope.into_iter().map(|id|{let version=manual.get(&id).copied().unwrap_or(0);(id,version)}).collect();
        Ok(Self {generation:generation.clone(),controller:batch.controller,source,wires,manual,engine,
            operation:format!("{}-on-{serial}",generation.boot_id()), ids:Default::default(), current_ids:Default::default(),before_counts:Default::default(),
            written:Default::default(),modules,reads:Default::default(),last_view:None,required,applied,failure:None,retired_queue:false,skipped_modules})
    }
    pub fn active(&self)->bool {self.engine.end().is_none()}
    pub fn gateway(&self)->Option<&str> {self.engine.current().map(|m|self.modules[&m.id].0.as_str())}
    pub fn operation(&self)->String {self.operation.clone()}
    pub fn view(&self)->View {
        let end=self.engine.end();
        View {operation:self.operation.clone(),revision:self.generation.epochs.configuration.to_string(),
            state:if self.failure.is_some(){"failed".into()}else{match end {None=>"pending",Some(core::End::Confirmed)=>"confirmed",Some(core::End::Cancelled)=>"cancelled",Some(_)=>"failed"}.into()},
            confirmed_modules:self.skipped_modules+self.engine.confirmed_modules(),current_module:self.engine.current().map(|m|m.id.clone()),
            reason:self.failure.clone().or_else(||end.filter(|v|!matches!(v,core::End::Confirmed)).map(|v|format!("{v:?}")))}
    }
    fn report(&mut self) {
        let view=self.view();
        if self.last_view.as_ref()!=Some(&view) {
            if view.state=="failed" {log::error!("household_on_failed operation={} module={:?} reason={:?}",view.operation,view.current_module,view.reason);}
            self.last_view=Some(view);
        }
    }
    pub fn reservation(&self,gateway:&str)->bool {
        self.active() && self.engine.current().and_then(|m|self.modules.get(&m.id)).is_some_and(|(g,_)|g==gateway)
    }
    pub fn allows(&self,gateway:&str,address:u8,command:Command,id:u64)->bool {
        if !self.reservation(gateway) || command==Command::ModuleAllOff || matches!(command,Command::Motor(homesense_core::MotorMotion::Stop)) {return true;}
        self.current_ids.contains(&id) || command==Command::Query && self.engine.current().is_some_and(|m|
            self.modules[&m.id].1==address && !self.reads.is_empty())
    }
    pub fn read_allowed(&self,gateway:&str,address:Option<u8>)->bool {
        !self.reservation(gateway) || self.engine.current().is_some_and(|m|
            address==Some(self.modules[&m.id].1) && !self.reads.is_empty())
    }
    pub fn read_needs(&self)->Vec<crate::away_execution::ReadNeed> {
        if !self.active() {return vec![];}
        let after=self.current_ids.iter().filter_map(|id|self.written.get(id)).copied().max();
        self.reads.iter().filter_map(|id|after.map(|after_ms|crate::away_execution::ReadNeed {
            device:id.clone(),before_count:self.before_counts.get(id).copied().unwrap_or(0),after_ms,deadline_ms:after_ms.saturating_add(core::RESPONSE_MS).min(self.engine.deadline_ms())})).collect()
    }
    pub fn written(&mut self,id:u64,at:u64) {if self.ids.contains(&id) {self.written.insert(id,at);}}
    pub fn retire(&mut self,now:u64)->Result<(),W> {
        self.engine.poll(now,false,false,&BTreeMap::new(),&BTreeMap::new()).map_err(|_|W::ServiceCommunicationFailure)?;
        self.reads.clear(); self.report(); Ok(())
    }
    pub fn valid(&self,generation:Option<&TaskGeneration>,manual:&BTreeMap<DeviceId,u64>,authorize:&impl Fn(Uuid)->bool,
                 gateways:Option<&crate::gateway_runtime::State>)->bool {
        generation==Some(&self.generation) && authorize(self.controller)
            && self.source.as_ref().is_none_or(|s|s.valid_for(self.controller,self.generation.epochs.configuration))
            && self.manual.iter().all(|(id,v)|manual.get(id).copied().unwrap_or(0)==*v)
            && self.wires.iter().all(|w|gateways.is_some_and(|g|g.table.ready(w)))
    }
}
#[derive(Default)]
pub(crate) struct Owner { pub jobs:VecDeque<Job>, serial:u64, latest_noop:Option<View> }
impl Owner {
    pub fn query_scopes(&self)->BTreeMap<String,Option<u8>> {
        self.jobs.iter().filter(|j|j.active()).filter_map(|j|j.engine.current().map(|m| {
            let (gateway,address)=&j.modules[&m.id];
            (gateway.clone(),(!j.reads.is_empty()).then_some(*address))
        })).collect()
    }
    pub fn next_serial(&mut self)->Result<u64,W> {
        while self.jobs.len()>=64 {
            let Some(index)=self.jobs.iter().position(|j|!j.active()) else{return Err(W::ResourceBusy)};
            self.jobs.remove(index);
        }
        self.latest_noop=None;
        self.serial=self.serial.checked_add(1).ok_or(W::ResourceBusy)?;
        Ok(self.serial)
    }
    pub fn active(&self)->bool {self.jobs.iter().any(Job::active)}
    pub fn allows(&self,gateway:&str,address:u8,command:Command,id:u64)->bool {self.jobs.iter().all(|j|j.allows(gateway,address,command,id))}
    pub fn read_allowed(&self,gateway:&str,address:Option<u8>)->bool {self.jobs.iter().all(|j|j.read_allowed(gateway,address))}
    pub fn written(&mut self,id:u64,at:u64) {for j in &mut self.jobs {j.written(id,at);}}
    pub fn read_needs(&self)->Vec<crate::away_execution::ReadNeed> {self.jobs.iter().flat_map(Job::read_needs).collect()}
    pub fn views(&self)->Vec<View> {self.jobs.iter().map(Job::view).chain(self.latest_noop.clone()).collect()}
    pub fn record_noop(&mut self,generation:&TaskGeneration,modules:usize,serial:u64)->Accepted {
        let operation=format!("{}-on-{serial}",generation.boot_id());
        self.latest_noop=Some(View {operation:operation.clone(),revision:generation.epochs.configuration.to_string(),state:"confirmed".into(),confirmed_modules:modules,current_module:None,reason:None});
        Accepted {operation,commands:vec![]}
    }
    pub fn audit(&mut self,context:&mut Context<'_>,generation:Option<&TaskGeneration>,authorize:&impl Fn(Uuid)->bool)->Result<(),W> {
        for job in &mut self.jobs {
            if job.active() && context.now>=job.engine.deadline_ms() {
                job.engine.poll(context.now,true,false,&BTreeMap::new(),&BTreeMap::new()).map_err(|_|W::ServiceCommunicationFailure)?;job.reads.clear();job.report();
            }
            if job.active() && !job.valid(generation,context.manual,authorize,context.gateways) {job.retire(context.now)?;}
            if !job.active() && !job.retired_queue {
                let cancelled=context.arbiter.cancel_queued_ids_committed(&job.ids,CancelReason::RunCancelled,
                    |rows|context.store.cancel_queued(context.boot,rows,context.now)).map_err(|_|W::ServiceCommunicationFailure)?;
                for row in cancelled {context.principals.remove(&row.command.id);}
                job.retired_queue=true;
            }
        }
        Ok(())
    }
}
pub(crate) struct Context<'a> {
    pub config:&'a Configuration,
    pub store:&'a mut Store,
    pub arbiter:&'a mut Arbiter,
    pub gateways:Option<&'a crate::gateway_runtime::State>,
    pub boot:&'a str,
    pub next_id:&'a mut u64,
    pub principals:&'a mut BTreeMap<u64,Uuid>,
    pub sources:&'a mut BTreeMap<u64,Source>,
    pub manual:&'a mut BTreeMap<DeviceId,u64>,
    pub batches:&'a mut crate::control_batch::Owner,
    pub now:u64,
}
impl Context<'_> {
    pub fn claim_scope(&mut self,scope:&[DeviceId],source:Option<&Source>)->Result<u64,W> {
        let fence=*self.next_id;
        let next=fence.checked_add(1).ok_or(W::ServiceCommunicationFailure)?;
        let affected=scope.iter().cloned().collect::<BTreeSet<_>>();
        let cancelled=crate::source_auth::commit(source,||self.arbiter.cancel_queued_devices_committed(&affected,
            |rows|self.store.cancel_queued(self.boot,rows,self.now))).ok_or(W::InsufficientPrivileges)?.map_err(|_|W::ServiceCommunicationFailure)?;
        for row in cancelled {self.principals.remove(&row.command.id);}
        *self.next_id=next;
        for device in scope {self.manual.insert(device.clone(),fence);}
        Ok(fence)
    }
    pub fn advance(&mut self,job:&mut Job,observation:&Observation,bus_quiet:bool)->Result<Vec<u64>,W> {
        let deliveries=job.current_ids.iter().map(|id| {
            let state=match self.store.command_status(self.boot,*id) {
                Ok(Some(Status::Accepted))=>core::Delivery::Accepted,
                Ok(Some(Status::Writing))=>core::Delivery::Writing,
                Ok(Some(Status::Written|Status::Observed))=>match job.written.get(id) {Some(at)=>core::Delivery::Written {at_ms:*at},None=>return Err(W::ServiceCommunicationFailure)},
                Ok(_)=>core::Delivery::Retired,Err(_)=>return Err(W::ServiceCommunicationFailure),
            };Ok((*id,state))
        }).collect::<Result<BTreeMap<_,_>,_>>()?;
        let samples:BTreeMap<_,_>=observation.proof.snapshot().devices.iter().map(|(id,state)|(id.clone(),core::Sample {
            observation:state.observed,confirmed:state.quality==homesense_state::Quality::Confirmed,
            count:observation.counts.get(id).copied().unwrap_or(0),received_ms:state.observed_at_ms.unwrap_or(0)})).collect();
        // Stop requesting a port once its actual post-write target arrived,
        // even while an already-owned query drains its reply/turnaround. Old
        // refresh priorities must not start another query and hide the quiet
        // slot required to advance the module sequence.
        if !job.current_ids.is_empty() && job.current_ids.iter().all(|id|job.written.contains_key(id))
            && let Some(after)=job.current_ids.iter().map(|id|job.written[id]).max()
            && let Some(module)=job.engine.current() {
            job.reads.retain(|id|samples.get(id).is_none_or(|sample|!sample.confirmed
                || sample.count<=job.before_counts.get(id).copied().unwrap_or(0)
                || sample.received_ms<after || sample.received_ms>after.saturating_add(core::RESPONSE_MS)
                || sample.observation.is_none_or(|actual|!module.targets[id].target_matches(actual))));
        }
        if job.applied.iter().any(|id|samples.get(id).is_none_or(|s:&core::Sample|!s.confirmed||s.observation.is_none_or(|o|!job.required[id].target_matches(o)))) {
            job.failure=Some("previously confirmed module target changed".into());job.retire(self.now)?;return Ok(vec![]);
        }
        let before=job.engine.confirmed_modules();
        let current=job.engine.current().map(|m|m.targets.keys().cloned().collect::<Vec<_>>()).unwrap_or_default();
        let decision=job.engine.poll(self.now,true,bus_quiet,&deliveries,&samples).map_err(|_|W::ServiceCommunicationFailure)?;
        if job.engine.confirmed_modules()>before {job.applied.extend(current);}
        match decision {
            core::Decision::Send(attempt)=>{
                job.reads.clear();
                let module=job.engine.current().ok_or(W::ServiceCommunicationFailure)?;
                let end=self.next_id.checked_add(8).ok_or(W::ServiceCommunicationFailure)?;
                let commands=module.targets.iter().enumerate().map(|(n,(device,command))|(QueuedCommand {
                    id:*self.next_id+n as u64,device_id:device.clone(),command:*command,origin:Origin::Hap,
                    epochs:job.generation.epochs,deadline_ms:attempt.send_before_ms,run_id:None},
                    Access {authorized:true,device_exists:true,device_verified:true,existing_direct_manual:false})).collect::<Vec<_>>();
                let tickets=commands.iter().map(|(c,_)|c.clone()).collect::<Vec<_>>();
                let scopes=self.gateways.map(|g|g.scope(self.config,&tickets,&BTreeMap::new(),&job.wires,vec![])).transpose()?;
                let accepted=crate::source_auth::commit(job.source.as_ref(),||self.arbiter.submit_batch_committed(commands,self.now,|commands,cancelled| {
                    match &scopes {Some(scopes)=>self.store.accept_queue_batch_scoped(self.boot,commands,cancelled,self.now,None,&[],None,None,&[],&[],None,&[],scopes),
                        None=>self.store.accept_queue_batch(self.boot,commands,cancelled,self.now)}
                })).ok_or(W::InsufficientPrivileges)?;
                match accepted {
                    Ok(cancelled)=>{for c in cancelled {self.principals.remove(&c.command.id);}},
                    Err(BatchCommitError::Admission(_))=>{job.engine.defer(attempt.proposal,self.now).map_err(|_|W::ServiceCommunicationFailure)?;return Ok(vec![]);},
                    Err(BatchCommitError::Persistence(_))=>return Err(W::ServiceCommunicationFailure),
                }
                let ids=(*self.next_id..end).collect::<BTreeSet<_>>();
                let before_counts:BTreeMap<_,_>=module.targets.keys().map(|id|(id.clone(),observation.counts.get(id).copied().unwrap_or(0))).collect();
                for command in &tickets {
                    self.principals.insert(command.id,job.controller);
                    if let Some(source)=&job.source {self.sources.insert(command.id,source.clone());}
                    self.manual.insert(command.device_id.clone(),*self.next_id);
                    job.manual.insert(command.device_id.clone(),*self.next_id);
                }
                self.batches.accepted(self.config,&tickets,job.generation.clone(),job.controller,job.source.clone(),vec![],self.manual);
                job.before_counts=before_counts.clone();
                job.engine.accepted(attempt.proposal,core::Receipt {commands:ids.clone(),before_counts},self.now).map_err(|_|W::ServiceCommunicationFailure)?;
                job.ids.extend(ids.iter().copied());job.current_ids=ids.clone();*self.next_id=end;
                job.report();Ok(ids.into_iter().collect())
            },
            core::Decision::Read {missing,..}=>{job.reads=missing;Ok(vec![])},
            core::Decision::Finished(_)=>{job.reads.clear();job.report();Ok(vec![])},
            core::Decision::Wait=>Ok(vec![]),
        }
    }
}
