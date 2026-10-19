//! Per-bus query completeness and recovery, sharing one physical write budget.
use super::*;
use homesense_core::gateway_scope::{Port,ReadRequirement,WireScope};
use std::collections::{BTreeMap,BTreeSet};

pub(super) struct Refresh {
    config:homesense_config::Configuration,
    targets:Vec<crate::physical_queries::Target>,
    pub(super) generation:crate::admission::TaskGeneration,
    pub(super) controller:uuid::Uuid,
    context:homesense_core::ExecutionContext,
    timeout:u64,
    max_age:u64,
    cycle:u64,
    due:Vec<u64>,
    pending:BTreeMap<String,Pending>,
    recovery:BTreeMap<String,Recovery>,
    control_opportunity:BTreeSet<String>,
    pub(super) round:RoundTracker,
    adaptive:Option<crate::adaptive_refresh::Schedule>,
    recent_queries:BTreeMap<(String,u8,u8),u64>,
    strict:BTreeMap<usize,u64>,
    priority:BTreeSet<usize>,
    confirmation_until:BTreeMap<usize,u64>,
    ramp_reads:crate::postwrite_confirmation::RampReads,
}
struct Pending { index:usize,before:Vec<(u8,u64)>,sent:u64,deadline:u64,retried:bool,recovery:bool,strict:bool }
struct Recovery { wire:WireScope,indices:Vec<usize>,next:usize,samples:Vec<crate::gateway_runtime::Sample> }
impl Refresh {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(config:homesense_config::Configuration,targets:Vec<crate::physical_queries::Target>,
        generation:crate::admission::TaskGeneration,controller:uuid::Uuid,context:homesense_core::ExecutionContext,
        timeout:u64,max_age:u64,interval:u64,now:u64)->Result<Self,String> {
        let cycle=interval.checked_mul(targets.len() as u64).ok_or("local refresh cycle overflow")?;
        let adaptive=(max_age==config.refresh_policy().observation_max_age_ms()).then(||crate::adaptive_refresh::Schedule::new(config.refresh_policy()));
        let first_delay=adaptive.as_ref().map_or(0,|s|s.interval());
        let due=(0..targets.len()).map(|i| {
            let offset=interval.checked_mul(i as u64+1).ok_or("local refresh slot overflow")?;
            now.checked_add(first_delay).and_then(|at|at.checked_add(offset)).ok_or("local refresh due overflow")
        }).collect::<Result<Vec<_>,_>>()?;
        let control_opportunity=config.gateways().keys().cloned().collect();
        Ok(Self {config,targets,generation,controller,context,timeout,max_age,cycle,due,pending:BTreeMap::new(),recovery:BTreeMap::new(),control_opportunity,round:RoundTracker::default(),adaptive,recent_queries:BTreeMap::new(),strict:BTreeMap::new(),priority:BTreeSet::new(),confirmation_until:BTreeMap::new(),ramp_reads:Default::default()})
    }
    pub(super) fn configure_consumers(&mut self,requirements:&BTreeMap<homesense_core::DeviceId,u64>) {self.strict=crate::consumer_refresh::target_ages(&self.config,&self.targets,requirements);}
    fn due_at(&self,index:usize,truth:&crate::module_truth::ModuleTruth)->u64 {if self.priority.contains(&index){return 0;}self.strict.get(&index).map_or(self.due[index],|age|self.due[index].min(crate::consumer_refresh::due(&self.targets[index],truth,*age)))}
    pub(super) fn written(&mut self,device:&homesense_core::DeviceId,now:u64,ramped:bool) {
        if self.adaptive.is_none() {return;}
        let Some((binding,module))=self.config.binding(device) else{return;};
        if !matches!(module.kind(),homesense_core::ModuleKind::Switch|homesense_core::ModuleKind::Dimmer) {return;}
        let (gateway,address,channel)=(binding.gateway.clone(),binding.address,binding.channel.get());
        self.ramp_reads.written(&self.config,device,now,ramped);
        self.activity(now);
        for (index,target) in self.targets.iter().enumerate() {if target.gateway==gateway && target.module.address()==address && target.channels.contains(&channel) {self.priority.insert(index);self.confirmation_until.insert(index,now.saturating_add(crate::control_batch::FEEDBACK_TOTAL_MS));self.round.forget(index);self.due[index]=0;}}
    }
    fn activity(&mut self,now:u64) {if let Some(schedule)=&mut self.adaptive {schedule.activity(now);self.round.discard_completion();for due in &mut self.due {*due=(*due).min(now);}}}
    fn solicited(&self,gateway:&str,address:u8,channel:u8,now:u64)->bool {if self.recent_queries.get(&(gateway.into(),address,channel)).is_some_and(|deadline|now<=*deadline){return true;}self.pending.get(gateway).is_some_and(|pending| {let target=&self.targets[pending.index];target.module.address().get()==address && target.channels.contains(&channel)})}
    pub(super) fn pending(&self)->bool {!self.pending.is_empty()}
    pub(super) fn blocked(&self,now:u64,gap:u64,background_led_phase:bool,feedback:&Feedback)->BTreeSet<String> {
        let mut blocked=self.pending.keys().cloned().collect::<BTreeSet<_>>();
        for (i,target) in self.targets.iter().enumerate() {
            if self.ramp_reads.ready(target,now) && !background_led_phase && !self.control_opportunity.contains(&target.gateway) && now.saturating_add(gap)>=self.due[i] && !feedback.query_deferred(&target.gateway,now,self.priority.contains(&i)) {blocked.insert(target.gateway.clone());}
        }
        blocked
    }
    fn fault(&mut self,gateway:&str) {self.ramp_reads.forget_gateway(gateway);self.priority.retain(|i|self.targets[*i].gateway!=gateway);self.confirmation_until.retain(|i,_|self.targets[*i].gateway!=gateway);self.round.reset();self.pending.remove(gateway);self.recovery.remove(gateway);self.control_opportunity.remove(gateway);}
    fn connected(&mut self,wire:WireScope) {
        self.ramp_reads.forget_gateway(wire.gateway());
        self.priority.retain(|i|self.targets[*i].gateway!=wire.gateway());
        self.confirmation_until.retain(|i,_|self.targets[*i].gateway!=wire.gateway());
        self.round.reset();
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
                        ||now.checked_sub(at).is_none_or(|age|age>=config.refresh_policy().observation_max_age_ms()) {return Err("initial full physical feedback required".into());}
                    samples.push(crate::gateway_runtime::Sample {requirement:ReadRequirement {port:Port {address:object.spec.address().get(),channel},before_count:count-1,sent_at_ms:at,deadline_ms:at},count,at});
                }
            }
            initial.push(crate::gateway_runtime::Initial {wire:wire.clone(),samples});
        }
        self.transport.admission().activate_gateways(initial).await.map_err(|e|format!("independent admission activation: {e:?}"))?;
        self.feedback.enable_local_wires(wires.iter().map(|wire|(wire.gateway().into(),wire.epoch())).collect())?;
        self.transport.enable_local_io(connector)
    }
    pub(crate) async fn poll_local_events(&mut self,start_connects:bool)->Result<BTreeSet<String>,String> {
        let events=self.transport.poll_local_io(start_connects).await?;
        let checked=events.reads.iter().map(|read|read.gateway_id.clone()).collect();
        for (wire,audit) in events.faults {
            self.transport.finish_status_read(wire.gateway());
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
            let candidates=match &read.result {
                hb_transport::Receive::Frames {frames,..}=>frames.iter().filter_map(|frame| {
                    let (_,channel,count)=crate::adaptive_refresh::candidate(self.feedback.module_truth(),&read.gateway_id,frame)?;
                    (!self.local_refresh.as_ref().is_some_and(|p|p.solicited(&read.gateway_id,frame.address,channel,read.received_at_ms)) && !self.transport.queued_query_solicited(&read.gateway_id,*frame,read.received_at_ms)).then_some((frame.address,channel,count))
                }).collect::<Vec<_>>(),_=>vec![],
            };

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
            if candidates.iter().any(|(address,channel,before)|self.feedback.module_truth().port_observation_count(&read.gateway_id,*address,*channel).is_some_and(|count|count>*before))
                && let Some(plan)=&mut self.local_refresh {plan.activity(read.received_at_ms);}

        }
        Ok(checked)
    }
    pub(crate) fn take_local_fault_operations(&mut self)->Vec<homesense_storage::gateway_audit::OperationKey> {
        std::mem::take(&mut self.local_fault_operations)
    }
    pub(crate) async fn local_tick(&mut self)->Result<(),String> {
        let checked=self.poll_local_events(true).await?;
        let now=self.transport.now_ms();
        let window=self.local_refresh.as_ref().is_some_and(|plan|!self.background_led_phase
            && plan.targets.iter().enumerate().any(|(i,target)|!plan.pending.contains_key(&target.gateway)
                && !plan.recovery.contains_key(&target.gateway) && plan.due_at(i,self.feedback.module_truth())<=now
                && plan.ramp_reads.ready(target,now) && !self.feedback.query_deferred(&target.gateway,now,plan.priority.contains(&i))));
        let checked=if window {Some(self.local_refresh_receive_window(checked).await?)}else{None};
        let mut plan=self.local_refresh.take().ok_or("local refresh not configured")?;
        let result=self.advance_local_refresh(&mut plan,checked.as_ref()).await;
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
    // Each completed owned read grants only this scheduling turn's query
    // opportunity on that bus. Do not immediately loan the returned stream to
    // another reader: that would starve queries when reads outlast the gap.
    async fn local_refresh_receive_window(&mut self,mut checked:BTreeSet<String>)->Result<BTreeSet<String>,String> {
        let until=tokio::time::Instant::now()+std::time::Duration::from_millis(self.transport.write_interval_ms().max(1));
        self.transport.start_local_reads_except(&checked)?;
        loop {
            checked.extend(self.poll_local_events(false).await?);
            if self.transport.has_local_fault() {return Ok(BTreeSet::new());}
            if self.transport.query_write_slot_ready() && checked.iter().any(|id|self.transport.local_idle_stream(id)) {return Ok(checked);}
            if tokio::time::Instant::now()>=until {return Ok(BTreeSet::new());}
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
    }
    async fn advance_local_refresh(&mut self,plan:&mut Refresh,checked:Option<&BTreeSet<String>>)->Result<(),String> {
        let now=self.transport.now_ms();
        plan.confirmation_until.retain(|index,until| {if now>=*until {plan.priority.remove(index);false}else{true}});
        for (index,deadline) in crate::control_batch::query_requirements(self.transport.admission(),&self.feedback,&plan.config,&plan.targets,plan.controller).await? {
            plan.priority.insert(index);plan.round.forget(index);plan.due[index]=0;
            plan.confirmation_until.entry(index).and_modify(|until|*until=(*until).max(deadline)).or_insert(deadline);
        }
        let completed=plan.pending.iter().filter(|(_,pending)|plan.completed(pending,self.feedback.module_truth(),now))
            .map(|(gateway,_)|gateway.clone()).collect::<Vec<_>>();
        for gateway in completed {
            let pending=plan.pending.remove(&gateway).ok_or("completed local query missing")?;
            self.transport.finish_status_read(&gateway);
            let target=&plan.targets[pending.index];
            let confirming=!pending.recovery && plan.priority.contains(&pending.index)
                && crate::postwrite_confirmation::needed(self.transport.admission(),&self.feedback,&plan.config,target,plan.confirmation_until.get(&pending.index).copied()).await?;
            if !confirming {plan.priority.remove(&pending.index);plan.confirmation_until.remove(&pending.index);}
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
            }else if !pending.strict && !confirming {
                let complete=plan.round.observe(pending.index,&plan.targets,self.feedback.module_truth(),now,plan.max_age,plan.config.refresh_policy().round_budget_ms,&plan.generation);
                if complete && let Some(schedule)=&mut plan.adaptive {
                    let due=now.checked_add(schedule.completed()).ok_or("adaptive refresh time exhausted")?;
                    plan.due.fill(due);self.feedback.set_managed_max_age_ms(schedule.allowance())?;
                }else{plan.due[pending.index]=plan.due[pending.index].checked_add(plan.cycle).ok_or("refresh time exhausted")?.max(now);}

            }
            if !pending.recovery {plan.control_opportunity.insert(gateway);}
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
            if let Some(pending)=plan.pending.get(&gateway) {
                let target=&plan.targets[pending.index];
                let missing=pending.before.iter().filter(|(channel,before)|self.feedback.module_truth().port_observation_count(&gateway,target.module.address().get(),*channel).is_none_or(|count|count<=*before)).map(|(channel,_)|*channel).collect::<Vec<_>>();
                self.feedback.missing_query_ports(&gateway,target.module.address().get(),&missing,now)?;
            }
            if let Some(schedule)=&mut plan.adaptive {schedule.timeout();}
            let wire=self.transport.local_wire(&gateway)?;
            let audit=self.transport.admission().gateway_fault(wire.clone()).await.map_err(|e|format!("local query-timeout audit: {e:?}"))?;
            self.transport.local_record_fault(wire,audit)?;plan.fault(&gateway);
        }
        // Retire the bounded quiet scan only after its outstanding replies or
        // timeouts ended. The next idle scan cannot overlap that old owner.
        if plan.pending.is_empty() && plan.recovery.is_empty()
            && let Some(schedule)=&mut plan.adaptive && schedule.settle_if_quiet(now) {
            plan.round.reset();
            plan.due.fill(now.saturating_add(schedule.interval()));
            self.feedback.set_managed_max_age_ms(schedule.allowance())?;
        }
        if self.background_led_phase && plan.recovery.is_empty() {return Ok(());}
        let mut candidates=vec![];
        for (gateway,pending) in &plan.pending {
            if now>=pending.deadline&&!pending.retried&&self.transport.local_idle_stream(gateway)&&!self.feedback.query_deferred(gateway,now,true) {candidates.push((pending.deadline,pending.index,true,pending.recovery));}
        }
        for (gateway,recovery) in &plan.recovery {
            if !plan.pending.contains_key(gateway)&&recovery.next<recovery.indices.len()&&self.transport.local_idle_stream(gateway)&&!self.feedback.query_deferred(gateway,now,true) {
                candidates.push((now,recovery.indices[recovery.next],false,true));
            }
        }
        for (index,target) in plan.targets.iter().enumerate() {
            if plan.ramp_reads.ready(target,now)&&!plan.pending.contains_key(&target.gateway)&&!plan.recovery.contains_key(&target.gateway)
                &&self.transport.local_connected(&target.gateway)&&!self.transport.local_syncing(&target.gateway)
                &&self.transport.local_idle_stream(&target.gateway)&&!self.feedback.query_deferred(&target.gateway,now,plan.priority.contains(&index))&&now>=plan.due_at(index,self.feedback.module_truth())&&(!plan.round.contains(index)||plan.strict.get(&index).is_some_and(|age|crate::consumer_refresh::due(target,self.feedback.module_truth(),*age)<=now)) {
                candidates.push((plan.due_at(index,self.feedback.module_truth()),index,false,false));
            }
        }
        // A completed reply's turnaround must not hide another independent
        // bus that is already ready for a read.
        candidates.retain(|(_,index,_,_)|!self.transport.query_response_pending(&plan.targets[*index].gateway)
            && checked.is_none_or(|checked|checked.contains(&plan.targets[*index].gateway)));
        candidates.sort_by_key(|candidate|(candidate.0,candidate.1));
        let Some((_,index,retried,recovery))=candidates.first().copied() else{return Ok(());};
        let strict=!recovery && !plan.priority.contains(&index) && plan.pending.get(&plan.targets[index].gateway).map_or_else(||plan.strict.get(&index).is_some_and(|age|crate::consumer_refresh::due(&plan.targets[index],self.feedback.module_truth(),*age)<=now),|p|p.strict);
        let target=&plan.targets[index];
        let before=self.physical_query_counts(&plan.config,target,plan.timeout,plan.max_age)?;
        let timeout=if retried {plan.timeout.min(500)}else{plan.timeout};
        let sent=if retried || recovery {
            self.transport.try_query_status_scope(&target.gateway,&target.module,target.first_channel(),plan.context,timeout,target.whole).await
        }else{
            self.transport.poll_query_status_scope(&target.gateway,&target.module,target.first_channel(),plan.context,timeout,target.whole).await
        };
        match sent {
            Ok(true)=>{},
            Ok(false)=>{plan.control_opportunity.remove(&target.gateway);return Ok(());},
            Err(_) if self.transport.has_local_fault()=>return Ok(()),
            Err(error)=>return Err(error),
        }
        let sent=self.transport.now_ms();
        if !retried&&!recovery&&!strict {plan.round.begin(sent);}
        plan.recent_queries.retain(|_,deadline|*deadline>=sent);
        for channel in &target.channels {plan.recent_queries.insert((target.gateway.clone(),target.module.address().get(),*channel),sent.saturating_add(timeout));}
        plan.pending.insert(target.gateway.clone(),Pending {index,before,sent,deadline:sent.checked_add(timeout).ok_or("local deadline overflow")?,retried,recovery,strict});
        plan.control_opportunity.remove(&target.gateway);
        // Capture generation is unchanged by local retries; it remains a global
        // publication/startup guard and is never replaced with a local epoch.
        if plan.generation.epochs.configuration!=plan.config.revision() {return Err("local query publication scope changed".into());}
        Ok(())
    }
}
