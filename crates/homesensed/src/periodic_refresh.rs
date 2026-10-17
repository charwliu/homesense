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
    recent_queries:std::collections::BTreeMap<(String,u8,u8),u64>,
    strict:std::collections::BTreeMap<usize,u64>,
}
struct Pending {
    index: usize,
    before: Vec<(u8, u64)>,
    deadline: u64,
    retried: bool,
    strict: bool,
}
impl OutputRefresh {
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

    pub(super) fn awaiting_response(&self) -> bool {
        self.pending.is_some()
    }
    pub(super) fn defer_control(&self, now: u64, gap: u64,feedback:&Feedback) -> bool {
        self.awaiting_response()
            || (!self.control_opportunity && now.saturating_add(gap) >= self.due
                && self.targets.iter().enumerate().any(|(i,target)|self.ramp_reads.ready(target,now) && !feedback.query_deferred(&target.gateway,now,self.priority.contains(&i))))
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
            prefer_postwrite: true,
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
        if self.background_led_phase {return Ok(());}
        let now=self.transport.now_ms();
        let window=self.output_refresh.as_ref().is_some_and(|plan|plan.pending.is_none()
            && (now>=plan.due || !plan.priority.is_empty() || plan.strict.iter().any(|(i,age)|
                crate::consumer_refresh::due(&plan.targets[*i],self.feedback.module_truth(),*age)<=now))
            && plan.targets.iter().enumerate().any(|(i,target)|plan.ramp_reads.ready(target,now)
                && !self.feedback.query_deferred(&target.gateway,now,plan.priority.contains(&i))));
        if window && !self.refresh_slot_receive_window().await? {return Ok(());}
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
        plan.confirmation_until.retain(|index,until| {if now>=*until {plan.priority.remove(index);false}else{true}});
        for (index,deadline) in crate::control_batch::query_requirements(self.transport.admission(),&self.feedback,&plan.configuration,&plan.targets,plan.controller).await.map_err(|e|("feedback-proof".into(),e))? {
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
        let strict_due=plan.strict.iter().filter(|(i,_)|plan.ramp_reads.ready(&plan.targets[**i],now) && !self.feedback.query_deferred(&plan.targets[**i].gateway,now,false)).filter_map(|(i,age)|{let due=crate::consumer_refresh::due(&plan.targets[*i],self.feedback.module_truth(),*age);(due<=now).then_some((due,*i))}).min();
        // Changed outputs need genuine post-write reads even when another
        // consumer is permanently overdue. Alternate the two classes so that
        // neither feedback confirmation nor strict consumers can starve.
        let priority = plan.priority.iter().copied().find(|i|plan.ramp_reads.ready(&plan.targets[*i],now) && !self.feedback.query_deferred(&plan.targets[*i].gateway,now,true));
        let postwrite = priority.is_some() && (plan.prefer_postwrite || strict_due.is_none());
        let strict=plan.pending.as_ref().map_or(!postwrite && strict_due.is_some(),|p|p.strict);
        let Some(index)=plan.pending.as_ref().map(|p|p.index)
            .or_else(||if postwrite {priority} else {strict_due.map(|(_,i)|i)})
            .or_else(||plan.oldest(self.feedback.module_truth(),|i|plan.ramp_reads.ready(&plan.targets[i],now) && !self.feedback.query_deferred(&plan.targets[i].gateway,now,false))) else {
                plan.control_opportunity=true;return Ok(());
            };
        let target = &plan.targets[index];
        let gateway = target.gateway.clone();
        if let Some(pending)=plan.pending.as_ref().filter(|pending|pending.retried) {
            let missing=pending.before.iter().filter_map(|(channel,count)|self.feedback.module_truth().port_observation_count(&target.gateway,target.module.address().get(),*channel).is_none_or(|current|current<=*count).then_some(*channel)).collect::<Vec<_>>();
            log::warn!("physical_query_timeout phase=periodic gateway={:?} address={:02X} whole={} required={:?} missing={:?} transport_epoch={} configuration_revision={} timeout_ms={} retry=true",target.gateway,target.module.address().get(),target.whole,target.channels,missing,plan.generation.epochs.transport,plan.configuration.revision(),plan.timeout_ms);
            return Err((gateway, "physical recovery feedback timeout".into()));
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
        plan.pending = Some(Pending {
            index,
            before,
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
