//! Explicit automation loop over already-established bounded streams. It opens
//! no connections, changes no control ownership, and never replays on restart.
use crate::{
    admission::Admission, observed_transport::ObservedTransport, panel_rules::PanelRules,
    scene_scheduler::SceneScheduler,
};
use std::{
    io::{Read, Write},
    time::Duration,
};
#[path="indicator_waves.rs"]
mod indicator_waves;
#[path="background_led_phase.rs"]
mod background_led_phase;
struct Guard<'a, S: Read + Write + Send + 'static> {
    admission: &'a Admission,
    owner: &'a mut ObservedTransport<S>,
    panels: &'a mut PanelRules,
    scenes: &'a mut SceneScheduler,
    calendar: Option<&'a mut crate::event_rules::ScheduledRules>,
    publication_paused: bool,
}
impl<S: Read + Write + Send + 'static> Drop for Guard<'_, S> {
    fn drop(&mut self) {
        if let Some(calendar) = &mut self.calendar {
            calendar.stop();
        }
        self.panels.disconnect();
        self.scenes.stop_now();
        if !self.publication_paused {
            self.admission.stop_now();
        }
        self.owner.invalidate();
    }
}
pub async fn drive<S: Read + Write + Send + 'static>(
    admission: &Admission,
    owner: &mut ObservedTransport<S>,
    panels: &mut PanelRules,
    scenes: &mut SceneScheduler,
    cadence: Duration,
    max_age_ms: u64,
    stop: tokio::sync::watch::Receiver<bool>,
) -> Result<(), String> {
    drive_with_calendar(
        admission,
        owner,
        panels,
        scenes,
        None,
        cadence,
        max_age_ms,
        stop,
        || {
            let elapsed = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|e| e.to_string())?;
            i64::try_from(elapsed.as_millis()).map_err(|e| e.to_string())
        },
    )
    .await
    .map(|_| ())
}
/// The wall clock is injectable for simulation. Delays and gestures continue to
/// use Admission's monotonic clock. Only explicitly supplied schedules run.
// Borrowed runtime owners and clocks remain explicit at this assembly boundary.
#[allow(clippy::too_many_arguments)]
pub async fn drive_with_calendar<S: Read + Write + Send + 'static>(
    admission: &Admission,
    owner: &mut ObservedTransport<S>,
    panels: &mut PanelRules,
    scenes: &mut SceneScheduler,
    calendar: Option<&mut crate::event_rules::ScheduledRules>,
    cadence: Duration,
    max_age_ms: u64,
    stop: tokio::sync::watch::Receiver<bool>,
    wall_clock: impl FnMut() -> Result<i64, String>,
) -> Result<(), String> {
    drive_controlled(
        admission, owner, panels, scenes, calendar, cadence, max_age_ms, stop, wall_clock, None, None,
    )
    .await
    .map(|_| ())
}
#[allow(clippy::too_many_arguments)]
pub(crate) async fn drive_controlled<S: Read + Write + Send + 'static>(
    admission: &Admission,
    owner: &mut ObservedTransport<S>,
    panels: &mut PanelRules,
    scenes: &mut SceneScheduler,
    calendar: Option<&mut crate::event_rules::ScheduledRules>,
    cadence: Duration,
    max_age_ms: u64,
    stop: tokio::sync::watch::Receiver<bool>,
    wall_clock: impl FnMut() -> Result<i64, String>,
    control: Option<(
        &mut crate::automation_control::Receiver,
        &homesense_config::rule_document::Document,
    )>,
    causal: Option<&mut crate::deferred_router::Router>,
) -> Result<bool, String> {
    match drive_classified(admission, owner, panels, scenes, calendar, cadence, max_age_ms, stop, wall_clock, control, causal).await? {
        Exit::Stopped => Ok(false),
        Exit::PublicationPaused => Ok(true),
        Exit::Disconnected { gateway } => Err(format!("gateway disconnected: {gateway}")),
    }
}
/// A transport disconnection is distinct from rule, storage and cleanup errors.
/// This classification does not reopen the stopped admission owner.
#[derive(Debug, PartialEq, Eq)]
pub enum Exit {
    Stopped,
    PublicationPaused,
    Disconnected { gateway: String },
}
#[allow(clippy::too_many_arguments)]
pub(crate) async fn drive_classified<S: Read + Write + Send + 'static>(
    admission: &Admission,
    owner: &mut ObservedTransport<S>,
    panels: &mut PanelRules,
    scenes: &mut SceneScheduler,
    calendar: Option<&mut crate::event_rules::ScheduledRules>,
    cadence: Duration,
    max_age_ms: u64,
    stop: tokio::sync::watch::Receiver<bool>,
    wall_clock: impl FnMut() -> Result<i64, String>,
    control: Option<(
        &mut crate::automation_control::Receiver,
        &homesense_config::rule_document::Document,
    )>,
    causal: Option<&mut crate::deferred_router::Router>,
) -> Result<Exit, String> {
    drive_with_handoff(admission,owner,panels,scenes,calendar,cadence,max_age_ms,stop,wall_clock,control,causal,false).await
}
#[allow(clippy::too_many_arguments)]
pub(crate) async fn drive_with_handoff<S: Read + Write + Send + 'static>(
    admission: &Admission,
    owner: &mut ObservedTransport<S>,
    panels: &mut PanelRules,
    scenes: &mut SceneScheduler,
    calendar: Option<&mut crate::event_rules::ScheduledRules>,
    cadence: Duration,
    max_age_ms: u64,
    stop: tokio::sync::watch::Receiver<bool>,
    wall_clock: impl FnMut() -> Result<i64, String>,
    control: Option<(
        &mut crate::automation_control::Receiver,
        &homesense_config::rule_document::Document,
    )>,
    causal: Option<&mut crate::deferred_router::Router>,
    allow_handoff: bool,
) -> Result<Exit, String> {
    drive_with_projection(admission,owner,panels,scenes,calendar,cadence,max_age_ms,stop,wall_clock,control,causal,allow_handoff,None).await
}
#[allow(clippy::too_many_arguments)]
pub(crate) async fn drive_with_projection<S: Read + Write + Send + 'static>(
    admission: &Admission,
    owner: &mut ObservedTransport<S>,
    panels: &mut PanelRules,
    scenes: &mut SceneScheduler,
    calendar: Option<&mut crate::event_rules::ScheduledRules>,
    cadence: Duration,
    max_age_ms: u64,
    stop: tokio::sync::watch::Receiver<bool>,
    wall_clock: impl FnMut() -> Result<i64, String>,
    control: Option<(
        &mut crate::automation_control::Receiver,
        &homesense_config::rule_document::Document,
    )>,
    causal: Option<&mut crate::deferred_router::Router>,
    allow_handoff: bool,
    projection: Option<tokio::sync::watch::Sender<homesense_state::Snapshot>>,
) -> Result<Exit, String> {
    drive_with_panel_feeds(admission,owner,panels,scenes,calendar,cadence,max_age_ms,stop,wall_clock,control,causal,allow_handoff,projection,vec![],None).await
}
async fn acquire_quantum(
    gate: Option<&std::sync::Arc<tokio::sync::Mutex<()>>>,
    stop: &mut tokio::sync::watch::Receiver<bool>,
) -> Option<tokio::sync::OwnedMutexGuard<()>> {
    let gate=gate?;
    tokio::select! {
        permit=gate.clone().lock_owned()=>Some(permit),
        _=stop.wait_for(|stopped|*stopped)=>None,
    }
}
#[allow(clippy::too_many_arguments)]
pub(crate) async fn drive_with_panel_feeds<S: Read + Write + Send + 'static>(
    admission: &Admission,
    owner: &mut ObservedTransport<S>,
    panels: &mut PanelRules,
    scenes: &mut SceneScheduler,
    calendar: Option<&mut crate::event_rules::ScheduledRules>,
    cadence: Duration,
    max_age_ms: u64,
    mut stop: tokio::sync::watch::Receiver<bool>,
    mut wall_clock: impl FnMut() -> Result<i64, String>,
    control: Option<(
        &mut crate::automation_control::Receiver,
        &homesense_config::rule_document::Document,
    )>,
    mut causal: Option<&mut crate::deferred_router::Router>,
    allow_handoff: bool,
    projection: Option<tokio::sync::watch::Sender<homesense_state::Snapshot>>,
    panel_feeds:Vec<crate::runtime_panel_hap::Feed>,
    handoff_gate:Option<std::sync::Arc<tokio::sync::Mutex<()>>>,
) -> Result<Exit, String> {
    struct Close<'a>(
        Option<(
            &'a mut crate::automation_control::Receiver,
            &'a homesense_config::rule_document::Document,
        )>,
    );
    impl Drop for Close<'_> {
        fn drop(&mut self) {
            if let Some((receiver, _)) = &mut self.0 {
                receiver.close();
            }
        }
    }
    let mut control = Close(control);
    if cadence.is_zero()
        || cadence > Duration::from_secs(1)
        || max_age_ms == 0
        || !owner.belongs_to(admission)
        || !scenes.matches_admission(admission)
        || calendar
            .as_ref()
            .is_some_and(|c| !c.matches_admission(admission))
    {
        return Err("invalid automation drive scope or cadence".into());
    }
    struct PanelFeeds(Vec<crate::runtime_panel_hap::Feed>);
    impl Drop for PanelFeeds {
        fn drop(&mut self) { for feed in &self.0 {feed.invalidate();} }
    }
    let mut panel_feeds=PanelFeeds(panel_feeds);
    let mut panel_cursor=owner.snapshot().cursor;
    let panel_generation=homesense_hap::panel_input::Generation {configuration:owner.feedback().configuration_revision(),transport:owner.feedback().transport_epoch()};
    // Keep the lease through error cleanup; declare it before Guard so an
    // abandoned future closes Admission before releasing the runtime quantum.
    let mut quantum = None;
    let mut guard = Guard {
        admission,
        owner,
        panels,
        scenes,
        calendar,
        publication_paused: false,
    };
    let mut disconnected = None;
    let mut transport_failure=false;
    // Heap-own the central loop to keep nested drive wrappers within the
    // caller's default stack, without changing guard or cancellation ownership.
    let result = Box::pin(async {
        let gateways = guard.owner.gateway_ids();
        if gateways.is_empty() {
            return Err("automation drive has no gateways".into());
        }
        // A trusted publication coordinator enters its barrier only between
        // complete runtime quanta, never while a causal owner awaits Admission.
        // Failed/cancelled work retains the existing fail-closed guards.
        quantum = acquire_quantum(handoff_gate.as_ref(), &mut stop).await;
        if *stop.borrow() || stop.has_changed().is_err() { return Ok(()); }
        if let Some((receiver,document))=&mut control.0 {receiver.bind_selection_reader(admission,document,stop.clone()).await?;}
        if let Some(router)=&mut causal {router.enable_feedback_candidates().await?;}
        guard.panels.bind_feedback(guard.owner.feedback())?;
        if causal.is_some() && let Some(calendar)=&mut guard.calendar {
            calendar.bind_feedback(guard.owner.feedback())?;
        }
        let mut indicators=indicator_waves::Waves::default();
        let mut component_modes=crate::component_modes::Consumer::default();
        let mut control_batches=crate::control_batch::Consumer::default();
        let mut background_led_phase:Option<background_led_phase::Phase>=None;
        let mut scene_observations=crate::scene_observation_runtime::Consumer::default();
        let mut next = 0;
        loop {
            let quantum_started=std::time::Instant::now();
            if *stop.borrow() || stop.has_changed().is_err() {
                return Ok(());
            }
            let max_age_ms=guard.owner.feedback().effective_max_age_ms(max_age_ms);
            if let Some(phase)=&background_led_phase
                && let Some(all_written)=phase.completion(admission,control.0.as_ref().map(|(receiver,_)|receiver.controller())).await {
                    indicators.phase_ended(all_written);
                    background_led_phase=None;guard.owner.pause_refresh_for_background_leds(false);
            }
            if guard.owner.independent() {
                guard.owner.local_tick().await?;
                let operations=guard.owner.take_local_fault_operations();
                guard.scenes.cancel_gateway_operations(&operations).await?;
            }else{
            let had_pending_read = guard.owner.output_query_pending();
            let pending_read = match guard.owner.receive_pending_output().await {
                Ok(read) => read,
                Err(error) => {
                    if let Some(gateway) = guard.owner.audited_receive_failure_gateway() {
                        log::warn!("audited gateway receive failed gateway={gateway}: {error}");
                        transport_failure = true;
                        disconnected = Some(gateway.to_owned());
                        return Ok(());
                    }
                    return Err(error);
                }
            };
            if let Some(read) = pending_read
                && matches!(read.result, hb_transport::Receive::Disconnected { .. })
            {
                transport_failure = true;
                disconnected = Some(read.gateway_id);
                return Ok(());
            }
            if had_pending_read && !guard.owner.output_query_pending() {
                // The previous quantum ends with this response. Offer its
                // publication boundary before starting any new runtime work
                // or query, even if every following polling slot is overdue.
                drop(quantum.take());
                quantum = acquire_quantum(handoff_gate.as_ref(), &mut stop).await;
                if *stop.borrow() || stop.has_changed().is_err() {
                    return Ok(());
                }
            }
            }
            let before_control_ms=quantum_started.elapsed().as_millis();
            if let Some((receiver, document)) = &mut control.0 {
                scene_observations.tick(admission,receiver.controller(),document,guard.owner.feedback()).await?;
                control_batches.tick(admission,receiver.controller(),guard.owner.feedback()).await?;
                component_modes.tick(admission,receiver.controller(),document,guard.owner.feedback()).await?;
                receiver.poll_causal(admission, document, guard.scenes, guard.owner.feedback(), max_age_ms, guard.calendar.as_deref(),causal.as_deref_mut()).await;
            }
            let after_control_ms=quantum_started.elapsed().as_millis();
            let now = admission.monotonic_ms();
            if let Some(calendar) = &mut guard.calendar {
                if let Some(router) = &mut causal {
                    calendar.startup_causal(Some((guard.owner.feedback(),max_age_ms)),router,guard.scenes).await?;
                    calendar.poll_causal(wall_clock()?,Some((guard.owner.feedback(),max_age_ms)),router,guard.scenes).await?;
                    calendar.consume_feedback_causal(guard.owner.feedback(),max_age_ms,router,guard.scenes).await?;
                } else {
                for report in calendar
                    .poll(wall_clock()?, Some((guard.owner.feedback(), max_age_ms)))
                    .await?
                {
                    for completed in &report.completed {
                        log::info!(
                            "calendar correlation={} rule={} cause={} commands={:?}",
                            report.correlation_id,
                            completed.rule,
                            completed.causation_id,
                            completed.commands
                        );
                    }
                    if let Some(error) = report.failure {
                        log::warn!(
                            "calendar correlation={} failed: {error}",
                            report.correlation_id
                        );
                    }
                }
            }
            }
            if let Some(router)=&mut causal {
                guard.panels.consume_feedback_causal(guard.owner.feedback(),now,max_age_ms,router,guard.scenes).await.map_err(|e|e.to_string())?;
            } else {
                guard.panels.consume_feedback(guard.owner.feedback(),now,max_age_ms).await.map_err(|e|e.to_string())?;
            }
            guard.panels.tick_holds(now).await?;
            let results = if let Some(router) = &mut causal {
                let (_, unowned) = router.poll(guard.scenes, Some((guard.owner.feedback(), max_age_ms))).await?;
                unowned
            } else {
                for run in guard.scenes.tick_with_feedback(now,Some((guard.owner.feedback(),max_age_ms))).await? {
                    if let Err(error) = run.result {
                        log::warn!("scene run={} failed: {error}", run.run);
                    }
                }
                if guard.calendar.is_some() { guard.scenes.take_results() } else { Vec::new() }
            };
            if let Some(calendar) = &mut guard.calendar {
                for result in results {
                    if let Some(router) = &mut causal {
                        calendar.scene_result_causal(result,Some((guard.owner.feedback(),max_age_ms)),router,guard.scenes).await?;
                        continue;
                    }
                    let report = calendar
                        .scene_result(result, Some((guard.owner.feedback(), max_age_ms)))
                        .await;
                    if let Some(error) = report.failure {
                        log::warn!("scene result rule failed: {error}");
                    }
                }
            }
            let burst_started=tokio::time::Instant::now();let mut written_this_quantum=0;
            // Bounded accepted-work progress. Each dispatch still rechecks STOP,
            // revocation, generation and the original deadline in Admission.
            for _ in 0..4 {
                if written_this_quantum>=hb_transport::MAX_BATCH_FRAMES || *stop.borrow() || background_led_phase.as_ref().is_some_and(|phase|admission.monotonic_ms()>=phase.deadline()) {break;}
                let Some(outcome)=guard.owner.step_after_refresh_read().await? else{break;};
                match &outcome.result {
                    Err(error) if guard.owner.independent() && crate::transport_owner::network_send_failure(error)=>{
                        log::warn!("local actuator write failed without replay command={}: {error:?}",outcome.command_id);
                    },
                    Err(error)=>{transport_failure=true;return Err(format!("automation write: {error:?}"));},
                    Ok(())=>{if let Some(router)=&mut causal {for (id,_) in &outcome.completed{router.observe_written(*id).await?;}}},
                }
                written_this_quantum+=outcome.physical_frames;
                if burst_started.elapsed()>=std::time::Duration::from_millis(100) {break;}
            }
            if *stop.borrow() {
                return Ok(());
            }
            let protected_after_burst=if written_this_quantum>0 {
                if let Some((receiver,_))=&control.0 {guard.owner.protected_controls(receiver.controller()).await?}else{false}
            }else{false};
            let defer_idle_receive=written_this_quantum>0 && protected_after_burst && !guard.owner.output_query_pending();
            if !guard.owner.independent() && !defer_idle_receive {
            let read = match guard.owner.receive(&gateways[next]).await {
                Ok(read) => read,
                Err(error) => {
                    if let Some(gateway) = guard.owner.audited_receive_failure_gateway() {
                        log::warn!("audited gateway receive failed gateway={gateway}: {error}");
                        transport_failure = true;
                        disconnected = Some(gateway.to_owned());
                        return Ok(());
                    }
                    return Err(error);
                }
            };
            if matches!(read.result, hb_transport::Receive::Disconnected { .. }) {
                transport_failure=true;
                disconnected = Some(gateways[next].clone());
                return Ok(());
            }
            if let Err((gateway,error))=guard.owner.refresh_outputs_if_due().await {
                log::warn!("periodic output query failed gateway={gateway}: {error}");
                transport_failure=true;
                disconnected=Some(gateway);
                return Ok(());
            }
            }else if guard.owner.independent(){
                guard.owner.start_local_reads()?;
            }
            guard.owner.expire(admission.monotonic_ms(), max_age_ms)?;
            if let Some((receiver,document))=&control.0 {
                scene_observations.tick(admission,receiver.controller(),document,guard.owner.feedback()).await?;
                crate::scene_initialization::tick(admission,receiver.controller(),document,guard.scenes,guard.owner.feedback()).await?;
                crate::scene_reconciliation::tick(admission,receiver.controller(),document,guard.scenes,guard.owner.feedback()).await?;
                let round=guard.owner.take_completed_refresh_round();
                if background_led_phase.is_none() {
                    let qualified=round.as_ref().is_some_and(|round|round.generation.epochs.configuration==document.revision && round.completed_ms>=round.started_ms && admission.monotonic_ms().saturating_sub(round.started_ms)<round.max_age_ms);
                    if let Some(wave)=indicators.advance(admission,receiver.controller(),document,guard.scenes,guard.owner.feedback(),max_age_ms,guard.owner.write_interval_ms(),qualified,guard.owner.output_query_pending()).await? {
                        let generation=admission.task_generation().await.map_err(|e|format!("LED wave generation: {e:?}"))?;
                        background_led_phase=Some(background_led_phase::Phase::new(generation,wave.batches,wave.deadline));
                        guard.owner.pause_refresh_for_background_leds(true);
                    }
                }
            }
            if let Some(projection)=&projection {projection.send_replace(guard.owner.snapshot());}
            if !panel_feeds.0.is_empty() {
                match guard.owner.feedback().events_since(panel_cursor) {
                    Ok(events)=>{
                        if let Some(last)=events.last() {panel_cursor=last.cursor;}
                        let now=admission.monotonic_ms();
                        for feed in &panel_feeds.0 {feed.publish(panel_generation,&events,now);}
                    }
                    Err(error)=>{
                        log::error!("HAP panel feed lost: {error}");
                        for feed in &panel_feeds.0 {feed.invalidate();}
                        panel_feeds.0.clear();
                    }
                }
            }
            if quantum_started.elapsed()>=std::time::Duration::from_millis(100) {
                log::warn!("automation_quantum_lag total_ms={} before_control_ms={before_control_ms} control_ms={} after_control_ms={} written_frames={written_this_quantum} pending_read={}",quantum_started.elapsed().as_millis(),after_control_ms.saturating_sub(before_control_ms),quantum_started.elapsed().as_millis().saturating_sub(after_control_ms),guard.owner.output_query_pending());
            }
            next = (next + 1) % gateways.len();
            // Preserve the existing publication boundary: an outstanding read
            // belongs to this generation until it completes or faults. Keep
            // pumping controls/feedback without publishing a new mapping mid-read.
            if !guard.owner.output_query_pending() {
                drop(quantum.take());
            }
            tokio::select! {
                _=tokio::time::sleep(cadence)=>{},
                changed=stop.changed()=>{if changed.is_err() || *stop.borrow() {return Ok(());}}
            }
            if quantum.is_none() {
                quantum = acquire_quantum(handoff_gate.as_ref(), &mut stop).await;
            }
        }
    })
    .await;
    // Retire bounded owned readers/connectors before a global mapping handoff.
    // Actual future cancellation still takes the existing fail-closed Drop.
    if guard.owner.independent() && result.is_ok() {
        guard.owner.settle_local_handoff().await?;
    }
    let publication_barrier=admission.publication_in_progress().await.unwrap_or(false);
    let reason=if transport_failure {homesense_storage::scene_run::CancellationReason::TransportFailure}
        else if publication_barrier {homesense_storage::scene_run::CancellationReason::PublicationBarrier}
        else {homesense_storage::scene_run::CancellationReason::ServiceStopped};
    let mut cleanup_errors = Vec::new();
    if let Some(router) = &mut causal
        && let Err(error) = router.close_with_reason(guard.scenes,reason).await {
        cleanup_errors.push(format!("causal shutdown: {error}"));
    }
    for (id, result) in guard.scenes.stop_with_reason(reason).await {
        if let Err(error) = result {
            cleanup_errors.push(format!("scene shutdown run={id}: {error}"));
        }
    }
    // Only an acknowledged active barrier permits a soft pause. Cancellation
    // of this future or an unavailable coordinator keeps the fail-closed Drop.
    if admission.publication_in_progress().await.unwrap_or(false) {
        for error in &cleanup_errors {
            log::warn!("publication coordinator retains cleanup responsibility: {error}");
        }
        guard.publication_paused = true;
        Ok(Exit::PublicationPaused)
    } else {
        if !cleanup_errors.is_empty() {
            let cleanup = cleanup_errors.join("; ");
            return Err(match result {
                Ok(()) => cleanup,
                Err(error) => format!("{error}; {cleanup}"),
            });
        }
        if allow_handoff && disconnected.is_some() && result.is_ok() {
            guard.publication_paused = true; // Supervisor owns subsequent recovery.
        }
        result.map(|()| match disconnected {
            Some(gateway) => Exit::Disconnected { gateway },
            None => Exit::Stopped,
        })
    }
}

#[cfg(test)]
#[path = "publication_barrier_probe.rs"]
mod publication_barrier_probe;
