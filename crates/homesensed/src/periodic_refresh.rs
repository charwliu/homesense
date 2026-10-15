//! Periodic reads keep the bus serialized while the runtime continues to pump
//! requests and publish actual feedback. Startup/recovery reads remain strict.
use super::*;
pub(super) struct OutputRefresh {
    configuration: homesense_config::Configuration,
    targets: Vec<crate::physical_queries::Target>,
    controller: uuid::Uuid,
    generation: crate::admission::TaskGeneration,
    context: homesense_core::ExecutionContext,
    timeout_ms: u64,
    max_age_ms: u64,
    interval_ms: u64,
    next: usize,
    due: u64,
    pending: Option<Pending>,
    control_opportunity: bool,
}
struct Pending {
    index: usize,
    before: Vec<(u8, u64)>,
    deadline: u64,
    retried: bool,
}
impl OutputRefresh {
    pub(super) fn awaiting_response(&self) -> bool {
        self.pending.is_some()
    }
    pub(super) fn defer_control(&self, now: u64, gap: u64) -> bool {
        self.awaiting_response()
            || (!self.control_opportunity && now.saturating_add(gap) >= self.due)
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
    fn oldest(&self, truth: &crate::module_truth::ModuleTruth) -> usize {
        (0..self.targets.len())
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
                    u8::from(available),
                    oldest,
                    (*index + self.targets.len() - self.next) % self.targets.len(),
                )
            })
            .expect("nonempty checked physical query plan")
    }
}
impl<S: Read + Write + Send + 'static> ObservedTransport<S> {
    /// Consume at most one bounded read before a control attempt. In
    /// particular, an already-buffered reply must not cost an empty dispatch
    /// iteration and let accepted controls accumulate behind completed reads.
    pub(crate) async fn receive_pending_output(&mut self) -> Result<Option<GatewayRead>, String> {
        let gateway = self.output_refresh.as_ref().and_then(|plan| {
            plan.pending
                .as_ref()
                .map(|pending| plan.targets[pending.index].gateway.clone())
        });
        let Some(gateway) = gateway else {
            return Ok(None);
        };
        let read = self.receive(&gateway).await?;
        if let Some(plan) = &mut self.output_refresh
            && plan.pending.as_ref().is_some_and(|pending| {
                plan.complete(
                    pending,
                    self.feedback.module_truth(),
                    self.transport.now_ms(),
                )
            })
        {
            plan.pending = None;
            plan.control_opportunity = true;
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
        let interval_ms = crate::physical_queries::interval(
            &targets,
            self.transport.write_interval_ms(),
            max_age_ms,
        )?;
        if self.transport.local_mode() {
            self.local_refresh=Some(local_refresh::Refresh::new(configuration.clone(),targets,generation,context,timeout_ms,max_age_ms,interval_ms,self.transport.now_ms())?);
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
            due: self.transport.now_ms().saturating_add(interval_ms),
            pending: None,
            control_opportunity: true,
        });
        Ok(())
    }
    /// At most one status read waits on the bus. A response updates state only
    /// through receive(); accepting/sending a query never refreshes timestamps.
    pub(crate) async fn refresh_outputs_if_due(&mut self) -> Result<(), (String, String)> {
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
        if let Some(pending) = &plan.pending {
            if plan.complete(pending, self.feedback.module_truth(), now) {
                plan.pending = None;
                plan.control_opportunity = true;
                // Leave one control-dispatch opportunity between reads even
                // when a slow response has consumed the next polling slot.
                return Ok(());
            }
            if now < pending.deadline {
                return Ok(());
            }
        } else if now < plan.due {
            return Ok(());
        }
        if !self
            .transport
            .idle_for_refresh(plan.controller, plan.generation.clone())
            .await
        {
            if let Some(pending) = &plan.pending {
                return Err((
                    plan.targets[pending.index].gateway.clone(),
                    "output query admission unavailable".into(),
                ));
            }
            return Ok(());
        }
        let retry = plan.pending.is_some();
        let index = plan
            .pending
            .as_ref()
            .map(|p| p.index)
            .unwrap_or_else(|| plan.oldest(self.feedback.module_truth()));
        let target = &plan.targets[index];
        let gateway = target.gateway.clone();
        if let Some(pending)=plan.pending.as_ref().filter(|pending|pending.retried) {
            let missing=pending.before.iter().filter_map(|(channel,count)|self.feedback.module_truth().port_observation_count(&target.gateway,target.module.address().get(),*channel).is_none_or(|current|current<=*count).then_some(*channel)).collect::<Vec<_>>();
            log::warn!("physical_query_timeout phase=periodic gateway={:?} address={:02X} whole={} required={:?} missing={:?} transport_epoch={} configuration_revision={} timeout_ms={} retry=true",target.gateway,target.module.address().get(),target.whole,target.channels,missing,plan.generation.epochs.transport,plan.configuration.revision(),plan.timeout_ms);
            return Err((gateway, "physical recovery feedback timeout".into()));
        }
        let timeout = if retry {
            plan.timeout_ms.min(500)
        } else {
            plan.timeout_ms
        };
        let before = self
            .physical_query_counts(&plan.configuration, target, timeout, plan.max_age_ms)
            .map_err(|error| (gateway.clone(), error))?;
        self.transport
            .query_status_scope(
                &target.gateway,
                &target.module,
                target.first_channel(),
                plan.context,
                timeout,
                target.whole,
            )
            .await
            .map_err(|error| (gateway.clone(), error))?;
        let deadline = self
            .transport
            .now_ms()
            .checked_add(timeout)
            .ok_or_else(|| (gateway, "query deadline overflow".into()))?;
        plan.pending = Some(Pending {
            index,
            before,
            deadline,
            retried: retry,
        });
        if !retry {
            plan.next = (index + 1) % plan.targets.len();
            plan.due = plan
                .due
                .saturating_add(plan.interval_ms)
                .max(self.transport.now_ms());
        }
        Ok(())
    }
}
