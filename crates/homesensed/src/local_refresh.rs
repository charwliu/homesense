//! Per-bus query completeness and recovery, sharing one physical write budget.
use super::*;
use homesense_core::gateway_scope::{Port,ReadRequirement,WireScope};
use std::collections::{BTreeMap,BTreeSet};

pub(super) struct Refresh {
    config:homesense_config::Configuration,
    targets:Vec<crate::physical_queries::Target>,
    generation:crate::admission::TaskGeneration,
    context:homesense_core::ExecutionContext,
    timeout:u64,
    max_age:u64,
    cycle:u64,
    due:Vec<u64>,
    pending:BTreeMap<String,Pending>,
    recovery:BTreeMap<String,Recovery>,
    control_opportunity:BTreeSet<String>,
}
struct Pending { index:usize,before:Vec<(u8,u64)>,sent:u64,deadline:u64,retried:bool,recovery:bool }
struct Recovery { wire:WireScope,indices:Vec<usize>,next:usize,samples:Vec<crate::gateway_runtime::Sample> }
impl Refresh {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(config:homesense_config::Configuration,targets:Vec<crate::physical_queries::Target>,
        generation:crate::admission::TaskGeneration,context:homesense_core::ExecutionContext,
        timeout:u64,max_age:u64,interval:u64,now:u64)->Result<Self,String> {
        let cycle=interval.checked_mul(targets.len() as u64).ok_or("local refresh cycle overflow")?;
        let due=(0..targets.len()).map(|i|now.checked_add(interval.checked_mul(i as u64+1).ok_or("local refresh slot overflow")?).ok_or("local refresh due overflow")).collect::<Result<Vec<_>,_>>()?;
        let control_opportunity=config.gateways().keys().cloned().collect();
        Ok(Self {config,targets,generation,context,timeout,max_age,cycle,due,pending:BTreeMap::new(),recovery:BTreeMap::new(),control_opportunity})
    }
    pub(super) fn pending(&self)->bool {!self.pending.is_empty()}
    pub(super) fn blocked(&self,now:u64,gap:u64)->BTreeSet<String> {
        let mut blocked=self.pending.keys().cloned().collect::<BTreeSet<_>>();
        for (i,target) in self.targets.iter().enumerate() {
            if !self.control_opportunity.contains(&target.gateway) && now.saturating_add(gap)>=self.due[i] {blocked.insert(target.gateway.clone());}
        }
        blocked
    }
    fn fault(&mut self,gateway:&str) {self.pending.remove(gateway);self.recovery.remove(gateway);self.control_opportunity.remove(gateway);}
    fn connected(&mut self,wire:WireScope) {
        self.pending.remove(wire.gateway());
        let indices=self.targets.iter().enumerate().filter(|(_,target)|target.gateway==wire.gateway()).map(|(i,_)|i).collect();
        self.recovery.insert(wire.gateway().into(),Recovery {wire,indices,next:0,samples:vec![]});
    }
    fn completed(&self,pending:&Pending,truth:&crate::module_truth::ModuleTruth,now:u64)->bool {
        let target=&self.targets[pending.index];
        pending.before.iter().all(|(channel,before)| {
            truth.port_observation_count(&target.gateway,target.module.address().get(),*channel).is_some_and(|n|n>*before)
                &&truth.received_port_state(&target.gateway,target.module.address().get(),*channel).is_some_and(|state|
                    state.quality==Quality::Confirmed &&state.observed.is_some()
                    &&state.observed_at_ms.is_some_and(|at|at>=pending.sent &&at<=pending.deadline
                        &&now.checked_sub(at).is_some_and(|age|age<self.max_age)))
        })
    }
}
impl<S:Read+Write+Send+'static> ObservedTransport<S> {
    pub(crate) fn independent(&self)->bool {self.transport.local_mode()}
    /// Existing initial all-bus query policy has already completed. Re-check
    /// every registered queryable physical port before enabling target-local IO.
    pub(crate) async fn enable_independent(&mut self,config:&homesense_config::Configuration,
        connector:crate::transport_owner::Connector<S>)->Result<(),String> {
        if config.gateways().len()<2 || self.independent() {return Ok(());}
        if self.unavailable || self.pending_recovery.is_some() || self.projection_failed
            ||config.revision()!=self.feedback.configuration_revision() {return Err("independent activation before full initial synchronization".into());}
        let now=self.transport.now_ms();
        let wires=self.transport.connection_wires()?;
        let mut initial=vec![];
        for wire in &wires {
            let mut samples=vec![];
            for object in self.feedback.module_truth().module_objects().iter().filter(|m|m.gateway==wire.gateway()
                &&matches!(m.spec.kind(),homesense_core::ModuleKind::Switch|homesense_core::ModuleKind::Dimmer)) {
                for channel in 1..=object.spec.channels() {
                    let state=self.feedback.module_truth().received_port_state(wire.gateway(),object.spec.address().get(),channel).ok_or("initial canonical port missing")?;
                    let at=state.observed_at_ms.ok_or("initial actual observation missing")?;
                    let count=self.feedback.module_truth().port_observation_count(wire.gateway(),object.spec.address().get(),channel).ok_or("initial actual count missing")?;
                    if state.quality!=Quality::Confirmed ||state.observed.is_none() ||count==0
                        ||now.checked_sub(at).is_none_or(|age|age>=5000) {return Err("initial full physical feedback required".into());}
                    samples.push(crate::gateway_runtime::Sample {requirement:ReadRequirement {port:Port {address:object.spec.address().get(),channel},before_count:count-1,sent_at_ms:at,deadline_ms:at},count,at});
                }
            }
            initial.push(crate::gateway_runtime::Initial {wire:wire.clone(),samples});
        }
        self.transport.admission().activate_gateways(initial).await.map_err(|e|format!("independent admission activation: {e:?}"))?;
        self.feedback.enable_local_wires(wires.iter().map(|wire|(wire.gateway().into(),wire.epoch())).collect())?;
        self.transport.enable_local_io(connector)
    }
    pub(crate) async fn poll_local_events(&mut self,start_connects:bool)->Result<(),String> {
        let events=self.transport.poll_local_io(start_connects).await?;
        for (wire,audit) in events.faults {
            self.feedback.invalidate_gateway(wire.gateway())?;
            if let Some(plan)=&mut self.local_refresh {plan.fault(wire.gateway());}
            self.local_fault_operations.extend(audit.operations);
        }
        for wire in events.connected {
            self.feedback.attach_gateway_wire(wire.gateway(),wire.epoch())?;
            self.local_refresh.as_mut().ok_or("local refresh missing")?.connected(wire);
        }
        let read_fences=events.read_fences;
        for read in events.reads {
            // HAP's coalesced publisher may skip a reached local target followed
            // by external feedback in the same receive batch. Settle the bounded
            // target from genuine per-frame evidence before later samples; this
            // does not shorten protection for intermediate ramp values.
            let targets=if matches!(&read.result,hb_transport::Receive::Frames {frames,..} if !frames.is_empty()) {
                self.transport.admission().projection_levels(self.feedback.configuration_revision(),self.feedback.snapshot().cursor.boot_id)
                    .await.map_err(|e|format!("local target projection: {e:?}"))?
            } else {BTreeMap::new()};
            if !targets.is_empty() && let hb_transport::Receive::Frames {transport_epoch,frames}=&read.result {
                if frames.len()>103 {return Err("invalid feedback batch".into());}
                for frame in frames {
                    let single=crate::transport_owner::GatewayRead {gateway_id:read.gateway_id.clone(),transport_epoch:read.transport_epoch,received_at_ms:read.received_at_ms,
                        result:hb_transport::Receive::Frames {transport_epoch:*transport_epoch,frames:vec![*frame]}};
                    if self.feedback.apply_at(&single,self.transport.now_ms())?>0 {
                        let config=&self.local_refresh.as_ref().ok_or("local refresh missing")?.config;
                        let channel=frame.command.checked_sub(0xd0);
                        let received=config.devices().keys().filter(|id|config.binding(id).is_some_and(|(device,module)|device.gateway==read.gateway_id && module.address().get()==frame.address && Some(device.channel.get())==channel)).cloned().collect();
                        let wire=WireScope::from_persisted(read.gateway_id.clone(),read.transport_epoch).map_err(|e|e.to_string())?;
                        self.transport.admission().received_projection(wire,read_fences.get(&read.gateway_id).cloned().unwrap_or_default(),received,read.received_at_ms,self.feedback.snapshot())
                            .await.map_err(|e|format!("settled local target: {e:?}"))?;
                    }
                }
            }else {self.feedback.apply_at(&read,self.transport.now_ms())?;}
        }
        Ok(())
    }
    pub(crate) fn take_local_fault_operations(&mut self)->Vec<homesense_storage::gateway_audit::OperationKey> {
        std::mem::take(&mut self.local_fault_operations)
    }
    pub(crate) async fn local_tick(&mut self)->Result<(),String> {
        self.poll_local_events(true).await?;
        let mut plan=self.local_refresh.take().ok_or("local refresh not configured")?;
        let result=self.advance_local_refresh(&mut plan).await;
        self.local_refresh=Some(plan);
        result?;
        if self.transport.has_local_fault() {self.poll_local_events(false).await?;}
        Ok(())
    }
    pub(crate) fn start_local_reads(&mut self)->Result<(),String> {self.transport.start_local_reads()}
    pub(crate) async fn settle_local_handoff(&mut self)->Result<(),String> {
        while self.transport.local_jobs_pending() {
            self.poll_local_events(false).await?;
            if self.transport.local_jobs_pending() {tokio::time::sleep(std::time::Duration::from_millis(1)).await;}
        }
        Ok(())
    }
    async fn advance_local_refresh(&mut self,plan:&mut Refresh)->Result<(),String> {
        let now=self.transport.now_ms();
        let completed=plan.pending.iter().filter(|(_,pending)|plan.completed(pending,self.feedback.module_truth(),now))
            .map(|(gateway,_)|gateway.clone()).collect::<Vec<_>>();
        for gateway in completed {
            let pending=plan.pending.remove(&gateway).ok_or("completed local query missing")?;
            let target=&plan.targets[pending.index];
            if pending.recovery {
                let recovery=plan.recovery.get_mut(&gateway).ok_or("local recovery owner missing")?;
                for (channel,before) in pending.before {
                    let state=self.feedback.module_truth().received_port_state(&gateway,target.module.address().get(),channel).ok_or("recovery port missing")?;
                    recovery.samples.push(crate::gateway_runtime::Sample {requirement:ReadRequirement {
                        port:Port {address:target.module.address().get(),channel},before_count:before,
                        sent_at_ms:pending.sent,deadline_ms:pending.deadline,
                    },count:self.feedback.module_truth().port_observation_count(&gateway,target.module.address().get(),channel).ok_or("recovery count missing")?,at:state.observed_at_ms.ok_or("recovery actual time missing")?});
                }
                recovery.next+=1;
            }else{
                plan.due[pending.index]=plan.due[pending.index].checked_add(plan.cycle).ok_or("refresh time exhausted")?.max(now);
                plan.control_opportunity.insert(gateway);
            }
        }
        let synchronized=plan.recovery.iter().filter(|(_,recovery)|recovery.next==recovery.indices.len()).map(|(id,_)|id.clone()).collect::<Vec<_>>();
        for gateway in synchronized {
            let recovery=plan.recovery.remove(&gateway).ok_or("completed recovery missing")?;
            if recovery.samples.iter().any(|sample|now.checked_sub(sample.at).is_none_or(|age|age>=plan.max_age)) {
                let audit=self.transport.admission().gateway_fault(recovery.wire.clone()).await.map_err(|e|format!("late recovery audit: {e:?}"))?;
                self.transport.local_record_fault(recovery.wire,audit)?;continue;
            }
            if self.transport.admission().gateway_ready(recovery.wire.clone(),recovery.samples).await.map_err(|e|format!("complete local recovery: {e:?}"))? {
                self.transport.local_ready(&recovery.wire)?;
                self.feedback.gateway_ready(&gateway)?;
                plan.control_opportunity.insert(gateway.clone());
                for (i,target) in plan.targets.iter().enumerate() {if target.gateway==gateway {plan.due[i]=now.checked_add(plan.cycle).ok_or("recovered refresh time overflow")?;}}
            }
        }
        // Expired local queries get the original single shorter retry. The
        // failed bus's backoff is not a sleep in the central runtime.
        let expired=plan.pending.iter().filter(|(_,pending)|now>=pending.deadline&&pending.retried)
            .map(|(gateway,_)|gateway.clone()).collect::<Vec<_>>();
        for gateway in expired {
            let wire=self.transport.local_wire(&gateway)?;
            let audit=self.transport.admission().gateway_fault(wire.clone()).await.map_err(|e|format!("local query-timeout audit: {e:?}"))?;
            self.transport.local_record_fault(wire,audit)?;plan.fault(&gateway);
        }
        let mut candidates=vec![];
        for (gateway,pending) in &plan.pending {
            if now>=pending.deadline&&!pending.retried&&self.transport.local_idle_stream(gateway) {candidates.push((pending.deadline,pending.index,true,pending.recovery));}
        }
        for (gateway,recovery) in &plan.recovery {
            if !plan.pending.contains_key(gateway)&&recovery.next<recovery.indices.len()&&self.transport.local_idle_stream(gateway) {
                candidates.push((now,recovery.indices[recovery.next],false,true));
            }
        }
        for (index,target) in plan.targets.iter().enumerate() {
            if !plan.pending.contains_key(&target.gateway)&&!plan.recovery.contains_key(&target.gateway)
                &&self.transport.local_connected(&target.gateway)&&!self.transport.local_syncing(&target.gateway)
                &&self.transport.local_idle_stream(&target.gateway)&&now>=plan.due[index] {
                candidates.push((plan.due[index],index,false,false));
            }
        }
        candidates.sort_by_key(|candidate|(candidate.0,candidate.1));
        let Some((_,index,retried,recovery))=candidates.first().copied() else{return Ok(());};
        let target=&plan.targets[index];
        let before=self.physical_query_counts(&plan.config,target,plan.timeout,plan.max_age)?;
        let timeout=if retried {plan.timeout.min(500)}else{plan.timeout};
        match self.transport.query_status_scope(&target.gateway,&target.module,target.first_channel(),plan.context,timeout,target.whole).await {
            Ok(())=>{},
            Err(_) if self.transport.has_local_fault()=>return Ok(()),
            Err(error)=>return Err(error),
        }
        let sent=self.transport.now_ms();
        plan.pending.insert(target.gateway.clone(),Pending {index,before,sent,deadline:sent.checked_add(timeout).ok_or("local deadline overflow")?,retried,recovery});
        plan.control_opportunity.remove(&target.gateway);
        // Capture generation is unchanged by local retries; it remains a global
        // publication/startup guard and is never replaced with a local epoch.
        if plan.generation.epochs.configuration!=plan.config.revision() {return Err("local query publication scope changed".into());}
        Ok(())
    }
}
