//! Periodic reads keep the bus serialized while the runtime continues to pump
//! requests and publish actual feedback. Startup/recovery reads remain strict.
use super::*;
pub(super) struct OutputRefresh {
    configuration: homesense_config::Configuration,
    targets: Vec<crate::physical_queries::Target>,
    pub(super) controller: uuid::Uuid,
    pub(super) generation: crate::admission::TaskGeneration,
    context: homesense_core::ExecutionContext,
    timeout_ms: u64,
    max_age_ms: u64,
    interval_ms: u64,
    next: usize,
    due: u64,
    pending: Option<Pending>,
    control_opportunity: bool,
    pub(super) round:RoundTracker,
    adaptive:Option<crate::adaptive_refresh::Schedule>,
    priority:std::collections::BTreeSet<usize>,
    confirmation_until:std::collections::BTreeMap<usize,u64>,
    ramp_reads:crate::postwrite_confirmation::RampReads,
    prefer_postwrite: bool,
    away_critical: bool,
    recent_queries:std::collections::BTreeMap<(String,u8,u8),u64>,
    strict:std::collections::BTreeMap<usize,u64>,
    query_backoff:std::collections::BTreeMap<(String,u8),u64>,
    target_backoff:std::collections::BTreeMap<usize,u64>,
    failed_ports:std::collections::BTreeMap<(String,u8,u8),u64>,
    away_writes:std::collections::BTreeMap<usize,(u64,Vec<(u8,u64)>)>,
}
struct Pending {
    index: usize,
    before: Vec<(u8, u64)>,
    original_before: Vec<(u8,u64)>,
    deadline: u64,
    retried: bool,
    strict: bool,
}
impl OutputRefresh {
    pub(super) fn away_written(&mut self,gateway:&str,devices:&[homesense_core::DeviceId],truth:&crate::module_truth::ModuleTruth,at:u64) {
        for(index,target)in self.targets.iter().enumerate().filter(|(_,target)|target.gateway==gateway) {
            let owned=target.channels.iter().all(|channel|devices.iter().any(|id|self.configuration.binding(id).is_some_and(|(binding,module)|
                binding.gateway==gateway && module.address()==target.module.address() && binding.channel.get()==*channel)));
            if owned {self.away_writes.insert(index,(at,target.channels.iter().map(|channel|
                (*channel,truth.port_observation_count(gateway,target.module.address().get(),*channel).unwrap_or(0))).collect()));}
        }
    }
    fn away_sample(&self,index:usize,feedback:&Feedback,now:u64,require_fresh:bool,require_on:bool)->bool {
        let Some((written,before))=self.away_writes.get(&index)else{return false;};let target=&self.targets[index];
        before.iter().all(|(channel,count)|feedback.module_truth().port_observation_count(&target.gateway,target.module.address().get(),*channel).is_some_and(|current|current>*count)
            && feedback.module_truth().located_state(&target.gateway,target.module.address().get(),*channel).is_some_and(|state|
                state.quality==Quality::Confirmed && state.observed.is_some_and(|observation|match observation {
                    homesense_core::Observation::Switch(on)=>!require_on||on,
                    homesense_core::Observation::Level(level)=>!require_on||level.get()>0,
                    _=>false,
                }) && state.observed_at_ms.is_some_and(|at|at>*written && at<=now && (!require_fresh||now-at<crate::control_batch::FEEDBACK_SAMPLE_MS))))
    }
    fn satisfy_away_reads(&mut self,feedback:&Feedback,now:u64,required:&std::collections::BTreeMap<usize,u64>)->Option<u64> {
        if !self.away_critical {self.away_writes.clear();return None;}
        let settled=self.priority.iter().copied().filter(|index|!required.contains_key(index)
            && self.pending.as_ref().is_none_or(|pending|pending.index!=*index)
            && self.away_sample(*index,feedback,now,true,false)).collect::<Vec<_>>();
        let mut allowance=None;
        for index in settled {
            self.round.begin(now);
            if let Some(bound)=self.observed_round(index,feedback.module_truth(),now) {allowance=Some(bound);}
        }
        allowance
    }
    fn gateway_outputs_missing(&mut self,gateway:&str,address:u8,missing:&[(u8,u64)],truth:&crate::module_truth::ModuleTruth)->bool {
        for (channel,count) in missing {self.failed_ports.insert((gateway.into(),address,*channel),*count);}
        self.failed_ports.retain(|(bus,address,channel),count|
            truth.port_observation_count(bus,*address,*channel).is_none_or(|current|current<=*count));
        let targets=self.targets.iter().filter(|t|t.gateway==gateway).collect::<Vec<_>>();
        !targets.is_empty() && targets.iter().all(|target|target.channels.iter().all(|channel|
            self.failed_ports.contains_key(&(gateway.into(),target.module.address().get(),*channel))))
    }
    fn eligible(&self,index:usize,feedback:&Feedback,now:u64,postwrite:bool)->bool {
        let target=&self.targets[index];
        self.query_backoff.get(&(target.gateway.clone(),target.module.address().get())).is_none_or(|until|now>=*until)
            && self.target_backoff.get(&index).is_none_or(|until|now>=*until)
            && self.ramp_reads.ready(target,now) && !feedback.query_deferred(&target.gateway,now,postwrite)
    }
    fn backoff_missing(&mut self,index:usize,feedback:&Feedback,now:u64) {
        let target=&self.targets[index];
        // Real post-write replies prove this module is partly reachable, even
        // when they have aged beyond the Off-confirmation bound. Back off only
        // the failed query, so another channel can supply a new actual sample.
        // A completely silent module still yields its whole query scope.
        let partial=self.away_critical && self.targets.iter().enumerate().any(|(i,other)|
            other.gateway==target.gateway && other.module.address()==target.module.address()
                && self.away_sample(i,feedback,now,false,false));
        if partial {self.target_backoff.insert(index,now.saturating_add(5_000));}
        else {self.query_backoff.insert((target.gateway.clone(),target.module.address().get()),now.saturating_add(5_000));}
    }
    pub(super) fn activity(&mut self,now:u64) {
        if let Some(schedule)=&mut self.adaptive {schedule.activity();self.round.discard_completion();self.due=self.due.min(now);}
    }
    pub(super) fn written(&mut self,device:&homesense_core::DeviceId,now:u64,ramped:bool) {
        if self.adaptive.is_none() {return;}
        let Some((binding,module))=self.configuration.binding(device) else{return;};
        if !matches!(module.kind(),homesense_core::ModuleKind::Switch|homesense_core::ModuleKind::Dimmer) {return;}
        let (gateway,address,channel)=(binding.gateway.clone(),binding.address,binding.channel.get());
        self.ramp_reads.written(&self.configuration,device,now,ramped);
        self.activity(now);
        for (index,target) in self.targets.iter().enumerate() {
            if target.gateway==gateway && target.module.address()==address && target.channels.contains(&channel) {
                self.priority.insert(index);self.confirmation_until.insert(index,now.saturating_add(crate::control_batch::FEEDBACK_TOTAL_MS));self.round.forget(index);
            }
        }
    }
    pub(super) fn solicited(&self,gateway:&str,address:u8,channel:u8,now:u64)->bool {
        if self.recent_queries.get(&(gateway.into(),address,channel)).is_some_and(|deadline|now<=*deadline) {return true;}

        self.pending.as_ref().is_some_and(|p| {let t=&self.targets[p.index];t.gateway==gateway && t.module.address().get()==address && t.channels.contains(&channel)})
    }
    fn observed_round(&mut self,index:usize,truth:&crate::module_truth::ModuleTruth,now:u64)->Option<u64> {
        self.priority.remove(&index);self.confirmation_until.remove(&index);
        if self.round.observe(index,&self.targets,truth,now,self.max_age_ms,self.configuration.refresh_policy().round_budget_ms,&self.generation)
            && let Some(schedule)=&mut self.adaptive {
            self.due=now.saturating_add(schedule.completed());return Some(schedule.allowance());
        }
        None
    }

    fn next_query(&self, feedback:&Feedback, now:u64)->Option<(usize,bool,bool)> {
        // Away already refreshes this exact port at a bound of at most5s.
        // Coalesce a duplicate5s strict request; tighter consumers retain
        // their independent alternating turn and original expiry.
        let strict_due=self.strict.iter().filter(|(i,age)|!self.away_critical || **age<crate::control_batch::FEEDBACK_SAMPLE_MS || !self.away_sample(**i,feedback,now,false,true)).filter(|(i,age)|!self.away_critical || !self.priority.contains(*i) || **age<crate::control_batch::FEEDBACK_SAMPLE_MS).filter(|(i,_)|self.eligible(**i,feedback,now,false)).filter_map(|(i,age)|{let due=crate::consumer_refresh::due(&self.targets[*i],feedback.module_truth(),*age);(due<=now).then_some((due,*i))}).min();
        // Changed outputs need genuine post-write reads even when another
        // consumer is permanently overdue. Alternate the two classes so that
        // neither feedback confirmation nor strict consumers can starve.
        // A refreshed low index may become due again before a large cohort
        // completes. Visit the oldest physical witness rather than allowing
        // that index to monopolize every post-write slot.
        let priority = self.priority.iter().copied()
            .filter(|i|self.eligible(*i,feedback,now,true))
            .min_by_key(|i|self.targets[*i].channels.iter().map(|channel|
                feedback.module_truth().located_state(&self.targets[*i].gateway,self.targets[*i].module.address().get(),*channel)
                    .filter(|state|state.quality==Quality::Confirmed && state.observed.is_some())
                    .and_then(|state|state.observed_at_ms.filter(|at|*at<=now)).unwrap_or(0)
            ).min().unwrap_or(0));
        let postwrite = priority.is_some() && (self.prefer_postwrite || strict_due.is_none());
        let strict=self.pending.as_ref().map_or(!postwrite && strict_due.is_some(),|p|p.strict);
        let index=self.pending.as_ref().map(|p|p.index)
            .or_else(||if postwrite {priority} else {strict_due.map(|(_,i)|i)})
            .or_else(||self.oldest(feedback.module_truth(),|i|self.eligible(i,feedback,now,false)))?;
        Some((index,postwrite,strict))
    }
    pub(super) fn awaiting_response(&self) -> bool {
        self.pending.is_some()
    }
    pub(super) fn defer_control(&self, now: u64, gap: u64,feedback:&Feedback) -> bool {
        self.awaiting_response()
            || (!self.control_opportunity && now.saturating_add(gap) >= self.due
                && self.targets.iter().enumerate().any(|(i,_)|self.eligible(i,feedback,now,self.priority.contains(&i))))
    }
    pub(super) fn control_checked(&mut self) {
        self.control_opportunity = false;
    }
    fn complete(
        &self,
        pending: &Pending,
        truth: &crate::module_truth::ModuleTruth,
        now: u64,
    ) -> bool {
        let target = &self.targets[pending.index];
        target.fresh(truth, now, self.max_age_ms)
            && pending.before.iter().all(|(channel, count)| {
                truth
                    .port_observation_count(
                        &target.gateway,
                        target.module.address().get(),
                        *channel,
                    )
                    .is_some_and(|current| current > *count)
                    && truth
                        .located_state(&target.gateway, target.module.address().get(), *channel)
                        .and_then(|state| state.observed_at_ms)
                        .is_some_and(|at| at <= pending.deadline)
            })
    }
    fn oldest(&self, truth: &crate::module_truth::ModuleTruth, eligible: impl Fn(usize)->bool) -> Option<usize> {
        (0..self.targets.len())
            .filter(|index|!self.round.contains(*index) && eligible(*index))
            .min_by_key(|index| {
                let target = &self.targets[*index];
                let mut available = true;
                let oldest = target
                    .channels
                    .iter()
                    .map(|channel| {
                        let state = truth.located_state(
                            &target.gateway,
                            target.module.address().get(),
                            *channel,
                        );
                        if state.as_ref().is_none_or(|s| {
                            s.quality != Quality::Confirmed
                                || s.observed.is_none()
                                || s.observed_at_ms.is_none()
                        }) {
                            available = false;
                        }
                        state.and_then(|s| s.observed_at_ms).unwrap_or(0)
                    })
                    .min()
                    .unwrap_or(0);
                (
                    u8::from(!self.priority.contains(index)),
                    u8::from(available),
                    oldest,
                    (*index + self.targets.len() - self.next) % self.targets.len(),
                )
            })
    }
}

impl<S: Read + Write + Send + 'static> ObservedTransport<S> {
    /// Consume at most one bounded read before a control attempt. In
    /// particular, an already-buffered reply must not cost an empty dispatch
    /// iteration and let accepted controls accumulate behind completed reads.
    pub(crate) async fn receive_pending_output(&mut self) -> Result<Option<GatewayRead>, String> {
        if self.transport.local_mode() {return Ok(None);}
        let gateway = self.output_refresh.as_ref().and_then(|plan| {
            plan.pending
                .as_ref()
                .map(|pending| plan.targets[pending.index].gateway.clone())
        }).or_else(||self.transport.queued_query_gateway());
        let Some(gateway) = gateway else {
            return Ok(None);
        };
        let read = self.receive(&gateway).await?;
        if let Some(plan)=&mut self.output_refresh
            && let Some(pending)=&plan.pending && plan.complete(pending,self.feedback.module_truth(),self.transport.now_ms()) {
                let index=pending.index;
                let confirming=plan.priority.contains(&index) && crate::postwrite_confirmation::needed(self.transport.admission(),&self.feedback,&plan.configuration,&plan.targets[index],plan.confirmation_until.get(&index).copied()).await?;
                if !confirming {plan.priority.remove(&index);plan.confirmation_until.remove(&index);}
                if !confirming && !pending.strict && let Some(bound)=plan.observed_round(index,self.feedback.module_truth(),self.transport.now_ms()) {self.feedback.set_managed_max_age_ms(bound)?;}
                self.transport.finish_status_read(&gateway);
                plan.pending=None;plan.control_opportunity=true;
        }
        Ok(Some(read))
    }
    pub(crate) fn output_query_pending(&self) -> bool {
        if self.transport.local_mode() {return self.local_refresh.as_ref().is_some_and(local_refresh::Refresh::pending);}
        self.output_refresh
            .as_ref()
            .is_some_and(OutputRefresh::awaiting_response)
    }
    pub(crate) fn configure_output_refresh(
        &mut self,
        configuration: &homesense_config::Configuration,
        controller: uuid::Uuid,
        generation: crate::admission::TaskGeneration,
        context: homesense_core::ExecutionContext,
        timeout_ms: u64,
        max_age_ms: u64,
    ) -> Result<(), String> {
        if configuration.revision() != self.feedback.configuration_revision()
            || generation.epochs.configuration != configuration.revision()
            || generation.epochs.transport != self.feedback.transport_epoch()
            || self.pending_recovery.is_some()
            || self.output_query_pending()
            || self.unavailable
            || timeout_ms == 0
            || timeout_ms > 5000
            || max_age_ms == 0
        {
            return Err("invalid output refresh generation or bounds".into());
        }
        self.feedback
            .module_truth()
            .project_logical(configuration)?;
        let targets = crate::physical_queries::plan(configuration, context)?;
        let interval_ms = crate::physical_queries::policy_interval(
            &targets,
            self.transport.write_interval_ms(),
            max_age_ms,
            configuration.refresh_policy(),
        )?;
        let adaptive=(max_age_ms==configuration.refresh_policy().observation_max_age_ms()).then(||crate::adaptive_refresh::Schedule::new(configuration.refresh_policy()));
        if let Some(schedule)=&adaptive {self.feedback.set_managed_max_age_ms(schedule.allowance())?;}
        let first_delay=adaptive.as_ref().map_or(interval_ms,|schedule|schedule.interval());
        let mut round=RoundTracker::default();
        // The startup/recovery owner already proved every registered port.
        if targets.iter().all(|target|target.fresh(self.feedback.module_truth(),self.transport.now_ms(),max_age_ms)) {
            round.begin(self.transport.now_ms());
            for index in 0..targets.len() {round.observe(index,&targets,self.feedback.module_truth(),self.transport.now_ms(),max_age_ms,configuration.refresh_policy().round_budget_ms,&generation);}
        }
        if self.transport.local_mode() {
            self.local_refresh=Some(local_refresh::Refresh::new(configuration.clone(),targets,generation,controller,context,timeout_ms,max_age_ms,interval_ms,self.transport.now_ms())?);
            if let Some(plan)=&mut self.local_refresh {plan.round=round;}
            self.output_refresh=None;return Ok(());
        }
        self.output_refresh = Some(OutputRefresh {
            configuration: configuration.clone(),
            targets,
            controller,
            generation,
            context,
            timeout_ms,
            max_age_ms,
            interval_ms,
            next: 0,
            due: self.transport.now_ms().saturating_add(first_delay),
            pending: None,
            control_opportunity: true,
            round,
            adaptive,priority:Default::default(),confirmation_until:Default::default(),ramp_reads:Default::default(),recent_queries:Default::default(),strict:Default::default(),
            query_backoff:Default::default(),target_backoff:Default::default(),failed_ports:Default::default(),away_writes:Default::default(),prefer_postwrite: true,
            away_critical: false,
        });
        Ok(())
    }
    pub(crate) fn configure_consumer_refresh(&mut self,requirements:&std::collections::BTreeMap<homesense_core::DeviceId,u64>)->Result<(),String>{
        if let Some(plan)=&mut self.output_refresh {plan.strict=crate::consumer_refresh::target_ages(&plan.configuration,&plan.targets,requirements);}
        if let Some(plan)=&mut self.local_refresh {plan.configure_consumers(requirements);}
        Ok(())
    }
    /// At most one status read waits on the bus. A response updates state only
    /// through receive(); accepting/sending a query never refreshes timestamps.
    pub(crate) async fn refresh_outputs_if_due(&mut self) -> Result<(), (String, String)> {
        let away_critical=self.transport.admission().away_operations_active();
        if let Some(plan)=&mut self.output_refresh {plan.away_critical=away_critical;}
        if self.background_led_phase {return Ok(());}
        let now=self.transport.now_ms();
        let window=self.output_refresh.as_ref().is_some_and(|plan|plan.pending.is_none()
            && (now>=plan.due || !plan.priority.is_empty() || plan.strict.iter().any(|(i,age)|
                crate::consumer_refresh::due(&plan.targets[*i],self.feedback.module_truth(),*age)<=now))
            && plan.targets.iter().enumerate().any(|(i,_)|plan.eligible(i,&self.feedback,now,plan.priority.contains(&i))));
        if window && let Some((gateway,controller,generation))=self.output_refresh.as_ref().and_then(|plan|plan.next_query(&self.feedback,now).map(|(i,_,_)|(plan.targets[i].gateway.clone(),plan.controller,plan.generation.clone())))
            // Drain only for a query that may actually be admitted. An idle
            // receive must not consume an already accepted control's lease.
            && self.transport.idle_for_refresh(controller,generation).await
            && !self.refresh_slot_receive_window(&gateway).await? {return Ok(());}
        let Some(mut plan) = self.output_refresh.take() else {
            return Ok(());
        };
        let result = self.advance_output_refresh(&mut plan).await;
        if result.is_ok() {
            self.output_refresh = Some(plan);
        } else {
            self.invalidate();
        }
        result
    }
    async fn advance_output_refresh(
        &mut self,
        plan: &mut OutputRefresh,
    ) -> Result<(), (String, String)> {
        let now = self.transport.now_ms();
        plan.query_backoff.retain(|_,until|now<*until);
        plan.confirmation_until.retain(|index,until| {if now>=*until {plan.priority.remove(index);false}else{true}});
        let required=crate::control_batch::query_requirements(self.transport.admission(),&self.feedback,&plan.configuration,&plan.targets,plan.controller).await.map_err(|e|("feedback-proof".into(),e))?;
        if let Some(bound)=plan.satisfy_away_reads(&self.feedback,now,&required) {
            self.feedback.set_managed_max_age_ms(bound).map_err(|error|("feedback-proof".into(),error))?;
        }
        for (index,deadline) in required {
            plan.priority.insert(index);plan.round.forget(index);
            plan.confirmation_until.entry(index).and_modify(|until|*until=(*until).max(deadline)).or_insert(deadline);
        }
        if let Some(pending) = &plan.pending {
            if plan.complete(pending, self.feedback.module_truth(), now) {
                let index=pending.index;
                let confirming=plan.priority.contains(&index) && crate::postwrite_confirmation::needed(self.transport.admission(),&self.feedback,&plan.configuration,&plan.targets[index],plan.confirmation_until.get(&index).copied()).await.map_err(|e|(plan.targets[index].gateway.clone(),e))?;
                if !confirming {plan.priority.remove(&index);plan.confirmation_until.remove(&index);}
                if !confirming && !pending.strict && let Some(bound)=plan.observed_round(index,self.feedback.module_truth(),now) {self.feedback.set_managed_max_age_ms(bound).map_err(|e|(plan.targets[index].gateway.clone(),e))?;}
                self.transport.finish_status_read(&plan.targets[index].gateway);
                plan.pending = None;
                plan.control_opportunity = true;
                // Leave one control-dispatch opportunity between reads even
                // when a slow response has consumed the next polling slot.
                return Ok(());
            }
            if now < pending.deadline {
                return Ok(());
            }
        } else if now < plan.due && plan.priority.is_empty() && !plan.strict.iter().any(|(i,age)|crate::consumer_refresh::due(&plan.targets[*i],self.feedback.module_truth(),*age)<=now) {
            return Ok(());
        }
        let retry = plan.pending.is_some();
        let readable=if retry {self.transport.can_retry_read(plan.controller,plan.generation.clone()).await}
            else {self.transport.idle_for_refresh(plan.controller,plan.generation.clone()).await};
        if !readable {
            if let Some(pending) = &plan.pending {
                return Err((
                    plan.targets[pending.index].gateway.clone(),
                    "output query admission unavailable".into(),
                ));
            }
            plan.control_opportunity=true;
            return Ok(());
        }
        let Some((index,postwrite,strict))=plan.next_query(&self.feedback,now) else {
            plan.control_opportunity=true;return Ok(());
        };
        let target = &plan.targets[index];
        let gateway = target.gateway.clone();
        if let Some(pending)=plan.pending.as_ref().filter(|pending|pending.retried) {
            let missing_counts=pending.original_before.iter().filter(|(channel,count)|self.feedback.module_truth().port_observation_count(&target.gateway,target.module.address().get(),*channel).is_none_or(|current|current<=*count)).copied().collect::<Vec<_>>();
            let missing=missing_counts.iter().map(|(channel,_)|*channel).collect::<Vec<_>>();
            log::warn!("physical_query_timeout phase=periodic gateway={:?} address={:02X} whole={} required={:?} missing={:?} transport_epoch={} configuration_revision={} timeout_ms={} retry=true",target.gateway,target.module.address().get(),target.whole,target.channels,missing,plan.generation.epochs.transport,plan.configuration.revision(),plan.timeout_ms);
            // Both bounded receive attempts ended without transport failure.
            // Do not reconnect the whole bus or abort other modules' Away
            // confirmation. Missing observations remain explicitly unknown.
            self.feedback.missing_query_ports(&gateway,target.module.address().get(),&missing,now)
                .map_err(|error|(gateway.clone(),error))?;
            let address=target.module.address().get();
            if plan.gateway_outputs_missing(&gateway,address,&missing_counts,self.feedback.module_truth()) {
                return Err((gateway,"all registered output feedback unavailable".into()));
            }
            plan.backoff_missing(index,&self.feedback,now);
            plan.round.forget(index);
            self.transport.finish_status_read(&gateway);
            plan.pending=None;plan.control_opportunity=true;plan.due=now;
            return Ok(());
        }
        // Already-issued reads keep their original timeout/fault accounting.
        // A retry skips the five-minute quiet period, but still lets F2 arrive.
        if self.feedback.query_deferred(&gateway,now,retry || postwrite) {
            plan.control_opportunity=true;return Ok(());
        }
        let timeout = if retry {
            plan.timeout_ms.min(500)
        } else {
            plan.timeout_ms
        };
        let before = self
            .physical_query_counts(&plan.configuration, target, timeout, plan.max_age_ms)
            .map_err(|error| (gateway.clone(), error))?;
        let sent=if retry {self.transport.try_query_status_scope(
                &target.gateway, &target.module, target.first_channel(),
                plan.context, timeout, target.whole).await
            } else {self.transport.poll_query_status_scope(
                &target.gateway,
                &target.module,
                target.first_channel(),
                plan.context,
                timeout,
                target.whole,
            )
            .await}
            .map_err(|error| (gateway.clone(), error))?;
        // Keep the query's turn while its write slot matures. Giving another
        // normal control a turn here lets continuous controls consume every
        // slot and starves feedback. STOP/eligible LED work still has priority.
        if !sent {plan.control_opportunity=false;return Ok(());}
        if !retry {plan.prefer_postwrite = !postwrite;}
        let deadline = self
            .transport
            .now_ms()
            .checked_add(timeout)
            .ok_or_else(|| (gateway, "query deadline overflow".into()))?;
        plan.recent_queries.retain(|_,old|*old>=self.transport.now_ms());
        for channel in &target.channels {plan.recent_queries.insert((target.gateway.clone(),target.module.address().get(),*channel),deadline);}
        let original_before=plan.pending.as_ref().map_or_else(||before.clone(),|pending|pending.original_before.clone());
        plan.pending = Some(Pending {
            index,
            before,
            original_before,
            deadline,
            retried: retry,
            strict,
        });
        if !retry && !strict {
            plan.round.begin(self.transport.now_ms());
            plan.next = (index + 1) % plan.targets.len();
            plan.due = plan
                .due
                .saturating_add(plan.interval_ms)
                .max(self.transport.now_ms());
        }
        Ok(())
    }
}

#[cfg(test)]
mod priority_tests {
    use super::*;
    use homesense_core::ExecutionContext;

    #[tokio::test]
    async fn persistent_postwrite_priority_refreshes_every_port_and_keeps_response_and_class_ownership() {
        let bytes=serde_json::to_vec(&serde_json::json!({
            "schema_version":2,"revision":1,
            "gateways":[{"id":"bus","endpoint":"127.0.0.1:1","modules":[
                {"id":"relay","address":"03","profile":"r8i8_din_pro_v2_7_7","verification":{"status":"simulated"}}
            ]}],"devices":[{"id":"load","name":"Synthetic load","module_id":"relay","channel":1,"enabled":true}]
        })).unwrap();
        let config=homesense_config::Configuration::parse(&bytes,ExecutionContext::Simulation).unwrap();
        let root=std::env::temp_dir().join(format!("priority-oldest-{}",uuid::Uuid::new_v4()));std::fs::create_dir(&root).unwrap();
        let path=root.join("state.sqlite");homesense_storage::Store::open(&path).unwrap().publish(None,1,&bytes,&[]).unwrap();
        let actor=crate::admission::Admission::open(path,crate::admission::Options{context:ExecutionContext::Simulation,..Default::default()},|_|true).await.unwrap();
        actor.synchronized(1,1).await.unwrap();let generation=actor.task_generation().await.unwrap();
        let boot=uuid::Uuid::parse_str(actor.boot_id()).unwrap().as_u128();
        let mut feedback=Feedback::new(&config,ExecutionContext::Simulation,boot,1).unwrap();
        let targets=crate::physical_queries::plan(&config,ExecutionContext::Simulation).unwrap();assert_eq!(targets.len(),8);
        let mut refresh=OutputRefresh{
            configuration:config,targets,controller:uuid::Uuid::new_v4(),generation,context:ExecutionContext::Simulation,
            timeout_ms:500,max_age_ms:5000,interval_ms:20,next:0,due:0,pending:None,control_opportunity:false,
            round:Default::default(),adaptive:None,priority:(0..8).collect(),confirmation_until:Default::default(),
            ramp_reads:Default::default(),prefer_postwrite:true,away_critical:false,recent_queries:Default::default(),strict:Default::default(),query_backoff:Default::default(),target_backoff:Default::default(),failed_ports:Default::default(),away_writes:Default::default(),
        };
        for expected in 0..8 {
            let at=100+expected as u64;
            assert_eq!(refresh.next_query(&feedback,at),Some((expected,true,false)),"a still-priority fresh first port cannot starve an unobserved later port");
            feedback.apply_at(&GatewayRead{
                gateway_id:"bus".into(),transport_epoch:1,received_at_ms:at,
                result:hb_transport::Receive::Frames{transport_epoch:1,frames:vec![hb_protocol::RawFrame{
                    address:3,command:0xd1+expected as u8,value:if expected==0{0xf1}else{0xf2},
                }]},
            },at).unwrap();
        }
        assert_eq!(refresh.next_query(&feedback,1000),Some((0,true,false)));
        refresh.pending=Some(Pending{index:3,before:vec![],original_before:vec![],deadline:1500,retried:false,strict:false});
        assert_eq!(refresh.next_query(&feedback,1000).unwrap().0,3,"an owned response is never rotated away");
        refresh.pending=None;refresh.strict.insert(7,1);refresh.prefer_postwrite=false;
        assert_eq!(refresh.next_query(&feedback,1000),Some((7,false,true)),"strict consumers keep their alternating turn");
        refresh.prefer_postwrite=true;assert_eq!(refresh.next_query(&feedback,1000),Some((0,true,false)));
        refresh.away_critical=true;refresh.prefer_postwrite=false;refresh.strict.insert(7,5000);
        assert_eq!(refresh.next_query(&feedback,3000),Some((0,true,false)),"the same5s witness must not consume two classes of read slots during Away");
        refresh.strict.insert(7,1000);
        assert_eq!(refresh.next_query(&feedback,3000),Some((7,false,true)),"a stricter consumer cannot borrow Away's broader allowance");
        refresh.query_backoff.insert(("bus".into(),3),5000);
        assert_eq!(refresh.next_query(&feedback,3000),None,"a missing module yields both priority and strict read slots");
        refresh.pending=Some(Pending{index:3,before:vec![],original_before:vec![],deadline:4500,retried:false,strict:false});
        assert_eq!(refresh.next_query(&feedback,3000).unwrap().0,3,"backoff cannot replace an already-owned reply");
        refresh.pending=None;
        assert!(refresh.next_query(&feedback,5000).is_some(),"backoff has a fixed end and cannot disable future recovery reads");
        assert!(!refresh.gateway_outputs_missing("bus",3,&[(1,1)],feedback.module_truth()));
        assert!(refresh.gateway_outputs_missing("bus",3,&(1..=8).map(|c|(c,1)).collect::<Vec<_>>(),feedback.module_truth()),"only a fully exhausted registered output scope enters gateway recovery");
        feedback.apply_at(&GatewayRead {gateway_id:"bus".into(),transport_epoch:1,received_at_ms:5001,result:hb_transport::Receive::Frames{transport_epoch:1,frames:vec![hb_protocol::RawFrame{address:3,command:0xd1,value:0xf2}]}},5001).unwrap();
        assert!(!refresh.gateway_outputs_missing("bus",3,&[(8,1)],feedback.module_truth()),"one genuine recovered output invalidates the whole-gateway silence classification");
        refresh.priority=[0].into();refresh.strict.clear();refresh.query_backoff.clear();
        let load=homesense_core::DeviceId::new("load").unwrap();
        refresh.away_written("bus",std::slice::from_ref(&load),feedback.module_truth(),6000);
        refresh.backoff_missing(7,&feedback,6001);
        assert!(!refresh.eligible(0,&feedback,6001,false),"without a real reply, the silent module yields its whole scope");
        refresh.query_backoff.clear();
        let _=refresh.satisfy_away_reads(&feedback,6001,&Default::default());
        assert!(refresh.priority.contains(&0),"an old or merely written value cannot clear post-write read priority");
        let receive=|at,value|GatewayRead {gateway_id:"bus".into(),transport_epoch:1,received_at_ms:at,result:hb_transport::Receive::Frames{transport_epoch:1,frames:vec![hb_protocol::RawFrame{address:3,command:0xd1,value}]}};
        feedback.apply_at(&receive(6002,0xf1),6002).unwrap();
        refresh.backoff_missing(7,&feedback,6003);
        assert!(refresh.eligible(0,&feedback,6003,false),"a missing sibling must not block a reachable port");
        assert!(!refresh.eligible(7,&feedback,6003,false));
        assert!(refresh.eligible(7,&feedback,11003,false),"per-query backoff also has a fixed end");
        refresh.target_backoff.clear();
        let _=refresh.satisfy_away_reads(&feedback,6002,&Default::default());
        assert!(!refresh.priority.contains(&0),"a real unsolicited failure settles the read, never the Off operation");
        refresh.strict.insert(0,5000);refresh.prefer_postwrite=false;
        assert_ne!(refresh.next_query(&feedback,9000).unwrap().0,0,"same-bound background reads do not keep querying a known Away failure");
        refresh.strict.insert(0,1000);
        assert_eq!(refresh.next_query(&feedback,9000),Some((0,false,true)),"tighter explicit consumers retain their read opportunity");
        refresh.away_written("bus",&[load],feedback.module_truth(),10000);
        assert!(!refresh.away_sample(0,&feedback,10001,false,true),"a new attempt cannot inherit the preceding attempt's On reply");
        refresh.priority.insert(0);feedback.apply_at(&receive(10002,0xf2),10002).unwrap();
        let _=refresh.satisfy_away_reads(&feedback,10002,&[(0,15000)].into());
        assert!(refresh.priority.contains(&0),"an independent current proof need is never discarded");
        let _=refresh.satisfy_away_reads(&feedback,10002,&Default::default());
        assert!(!refresh.priority.contains(&0));
        refresh.strict.insert(0,5000);
        assert_eq!(refresh.next_query(&feedback,13000),Some((0,false,true)),"healthy Off witnesses still expire at the original bound");
        actor.shutdown().await.unwrap();std::fs::remove_dir_all(root).unwrap();
    }
}
