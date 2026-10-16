//! Explicitly driven timeline -> durable rule admission adapter. No subscriptions
//! or sockets are started by construction. Transport completion is reported by
//! the existing owner, never inferred from successful admission.
use crate::admission::{Admission, TaskGeneration};
use homesense_core::{
    DeviceId,
    scene::{Status, Step, Timeline},
};
use homesense_hap::requests::{Batch, Intent};
use uuid::Uuid;
pub struct SceneRuntime {
    admission: Admission,
    controller: Uuid,
    generation: TaskGeneration,
    manual_fence: u64,
    causal_deadline: Option<u64>,
    timeline: Timeline,
    targets: crate::scene_activity::Targets,
    audit_id: String,
    definition: Option<String>,
    result_taken: bool,
    audit_version: u64,
    audit_status: homesense_storage::scene_run::Status,
    audit_failed: bool,
    cancellation_audited: std::collections::BTreeMap<DeviceId,bool>,
    cancel_requests: std::collections::BTreeMap<DeviceId,bool>,
    stop_reason: Option<homesense_storage::scene_run::CancellationReason>,
    expiries_audited: std::collections::BTreeSet<usize>,
    admitted: Vec<u64>,
    pending: Option<Pending>,
    fade: Option<OwnedFade>,
    pub(crate) configured_fades: std::collections::BTreeMap<usize,homesense_config::rule_document::FadeTiming>,
}
struct OwnedFade {
    index: usize,
    device: DeviceId,
    driver: crate::software_fade::SoftwareFade,
    stopping: bool,
}
struct PhaseMember{index:usize,device:DeviceId,ids:Vec<u64>}
struct Pending {
    members:Vec<PhaseMember>,
    device: DeviceId,
    index: usize,
    run: u64,
    ids: Vec<u64>,
    written: usize,
}
impl SceneRuntime {
    pub(crate) fn controller(&self) -> Uuid { self.controller }
    pub(crate) fn source_authorization(&self)->Option<crate::source_auth::Source>{self.admission.source()}
    pub(crate) fn source_valid(&self)->bool{self.admission.source_valid()}
    /// A durable terminal with no owned output is safe to deliver as a result.
    pub(crate) fn settled_terminal(&self) -> bool {
        !self.audit_failed && !self.has_pending() && self.audit_status.terminal()
    }

    pub(crate) fn take_result(&mut self) -> Option<(String, String, Status)> {
        if self.audit_failed
            || self.result_taken
            || self.has_pending()
            || matches!(
                self.audit_status,
                homesense_storage::scene_run::Status::Pending
                    | homesense_storage::scene_run::Status::Running
            )
        {
            return None;
        }
        let definition = self.definition.clone()?;
        self.result_taken = true;
        Some((definition, self.audit_id.clone(), self.status()))
    }
    pub(crate) fn configuration_revision(&self) -> u64 {
        self.generation.epochs.configuration
    }
    pub async fn new(
        admission: Admission,
        controller: Uuid,
        steps: Vec<Step>,
        now: u64,
        max_lateness: u64,
    ) -> Result<Self, String> {
        Self::new_named(admission, controller, steps, now, max_lateness, None).await
    }
    pub(crate) async fn new_named(
        admission: Admission,
        controller: Uuid,
        steps: Vec<Step>,
        now: u64,
        max_lateness: u64,
        definition: Option<String>,
    ) -> Result<Self, String> {
        let generation = admission
            .task_generation()
            .await
            .map_err(|e| format!("{e:?}"))?;
        let gateway_targets=steps.iter().map(|step|step.device.clone()).collect();
        let targets = crate::scene_activity::Targets::new(&steps);
        let timeline = Timeline::new(steps, generation.epochs, now, max_lateness)
            .map_err(|e| format!("{e:?}"))?;
        let manual_fence = admission
            .manual_fence()
            .await
            .map_err(|e| format!("{e:?}"))?;
        let audit_id = Uuid::new_v4().to_string();
        admission
            .scene_update(
                controller,
                generation.clone(),
                crate::admission::SceneUpdate::BeginScoped {
                    run: audit_id.clone(),
                    definition: definition.clone(),
                    targets:gateway_targets,
                },
            )
            .await
            .map_err(|e| format!("scene receipt: {e:?}"))?;
        Ok(Self {
            audit_id,
            definition,
            result_taken: false,
            audit_version: 0,
            audit_status: homesense_storage::scene_run::Status::Pending,
            audit_failed: false,
            cancellation_audited: Default::default(),
            cancel_requests: Default::default(),
            stop_reason: None,
            expiries_audited: Default::default(),
            manual_fence,
            causal_deadline: None,
            admission,
            controller,
            generation,
            timeline,
            targets,
            admitted: vec![],
            pending: None,
            fade: None,
            configured_fades: Default::default(),
        })
    }
    /// Install only a freshly accepted, single-use capability. This never reads
    /// executable work from history and cannot recreate a token after restart.
    pub async fn from_accepted(admission:Admission, accepted:crate::admission::AcceptedScene)->Result<Self,String> {
        let admission=admission.with_source(accepted.source_authorization.clone());
        let current=admission.task_generation().await.map_err(|e|format!("{e:?}"))?;
        if current!=accepted.generation {return Err("accepted scene generation expired".into());}
        let bundle=admission.automation_bundle(accepted.controller,current.clone()).await.map_err(|e|format!("{e:?}"))?;
        let document=bundle.automation.ok_or("accepted automation missing")?;
        let definition=document.timed_scenes.get(&accepted.start.definition).ok_or("accepted scene missing")?;
        if !definition.enabled {return Err("accepted scene disabled".into());}
        let mut steps=definition.steps.clone();
        for step in &mut steps {
            step.source=format!("{} -> {}",accepted.start.source,step.source);
            if step.source.len()>8192 {return Err("accepted scene source too long".into());}
        }
        let targets=crate::scene_activity::Targets::new(&steps);
        let timeline=Timeline::new(steps,current.epochs,accepted.started_ms,definition.max_lateness_ms).map_err(|e|format!("{e:?}"))?;
        Ok(Self{admission,controller:accepted.controller,generation:current,manual_fence:accepted.manual_fence,causal_deadline:accepted.causal_deadline,timeline,targets,
            audit_id:accepted.start.run,definition:Some(accepted.start.definition),result_taken:false,audit_version:0,
            audit_status:homesense_storage::scene_run::Status::Pending,audit_failed:false,cancellation_audited:Default::default(),cancel_requests:Default::default(),stop_reason:None,expiries_audited:Default::default(),admitted:vec![],pending:None,fade:None,configured_fades:definition.fades.clone()})
    }
    /// Transfer the original scene fences to a delayed fade. This does not
    /// start the fade or resolve the timeline step; the scheduler owns both.
    pub fn bind_fade(&self, index: usize, fade: &mut crate::software_fade::SoftwareFade) -> Result<(), String> {
        if self.audit_failed || self.audit_status.terminal() {
            return Err("scene no longer accepts fade work".into());
        }
        if !matches!(self.timeline.states().get(index), Some(homesense_core::scene::State::Waiting | homesense_core::scene::State::Offered)) {
            return Err("scene step no longer accepts fade work".into());
        }
        let step=self.timeline.step(index).ok_or("unknown fade scene step")?;
        fade.inherit_scene(&self.generation,self.controller,self.manual_fence,self.causal_deadline,step,self.audit_id.clone(),index)
    }
    /// Install a prepared fade for the next due step. The runtime owns polling,
    /// cancellation and the step result after this succeeds.
    pub async fn install_due_fade(&mut self, mut fade: crate::software_fade::SoftwareFade, now: u64) -> Result<(), String> {
        if self.has_pending() || self.audit_failed {
            fade.cancel().await?;
            return Err("scene already has pending work".into());
        }
        let due=match self.timeline.next(now,self.generation.epochs) {
            Ok(Some(due))=>due,
            result=>{
                fade.cancel().await?;
                self.checkpoint().await?;
                return Err(match result {Err(error)=>format!("{error:?}"),_=>"no scene step due".into()});
            }
        };
        if let Err(error) = self.audit_expiries().await {
            fade.cancel().await?;
            return Err(error);
        }
        if let Err(error)=self.bind_fade(due.index,&mut fade) {
            self.timeline.resolve(due.index,false).map_err(|e|format!("{e:?}"))?;
            fade.cancel().await?;
            self.checkpoint().await?;
            return Err(error);
        }
        self.fade=Some(OwnedFade{index:due.index,device:due.step.device,driver:fade,stopping:false});
        self.checkpoint().await
    }
    async fn reconcile_fade(&mut self) -> Result<(), String> {
        let Some(fade)=self.fade.as_mut() else {return Ok(());};
        let result=if fade.stopping {fade.driver.cancel().await.map(|()|true)} else {fade.driver.tick().await};
        match result {
            Ok(false)=>return Ok(()),
            Ok(true) if !fade.stopping => self.timeline.resolve(fade.index,true),
            Ok(true) => if fade.driver.output_possible() { self.timeline.resolve_partial(fade.index) } else {self.timeline.cancel_offered(fade.index)},
            Err(error) => {
                fade.driver.cancel().await?;
                if fade.driver.manually_cancelled() {
                    self.timeline.cancel_device(&fade.device);
                } else {
                    self.timeline.cancel();
                }
                if fade.driver.output_possible() { self.timeline.resolve_partial(fade.index) }
                else if fade.driver.manually_cancelled() {self.timeline.cancel_offered(fade.index)}
                else {self.timeline.resolve(fade.index,false)}
                .map_err(|e|format!("{e:?}"))?;
                let manual_device=fade.driver.manually_cancelled().then(||fade.device.clone());
                self.fade=None;
                if let Some(device)=manual_device
                    && self.cancellation_audited.get(&device)!=Some(&true) {
                    self.persist_checkpoint(homesense_storage::scene_run::Status::Running,Some(homesense_storage::scene_run::Cancellation{device:device.clone(),reason:homesense_storage::scene_run::CancellationReason::ManualOverride})).await?;
                    self.cancellation_audited.insert(device,true);
                }
                self.checkpoint().await?;
                return Err(error);
            }
        }.map_err(|e|format!("{e:?}"))?;
        self.fade=None;
        Ok(())
    }
    pub(crate) fn due_fade(&self, now:u64) -> Option<(Step,homesense_config::rule_document::FadeTiming)> {
        if self.has_pending() {return None;}
        let (index,step)=self.timeline.due_step(now)?;
        self.configured_fades.get(&index).copied().map(|timing|(step.clone(),timing))
    }
    /// Caller uses the same boot-relative monotonic clock for construction and polling.
    pub async fn poll(&mut self, now: u64) -> Result<Option<Vec<u64>>, String> {
        if self.audit_failed {
            return Err("scene audit unavailable".into());
        }
        let result = self.poll_inner(now).await;
        self.checkpoint().await?;
        result
    }
    async fn poll_inner(&mut self, now: u64) -> Result<Option<Vec<u64>>, String> {
        self.reconcile().await?;
        if self.has_pending() {
            return Err("scene action still in flight".into());
        }
        let current = self
            .admission
            .task_generation()
            .await
            .map_err(|e| format!("{e:?}"))?;
        if current != self.generation {
            self.timeline.cancel();
            return Err("scene generation changed".into());
        }
        if self.due_fade(now).is_some() {
            return Err("configured fade requires feedback preparation".into());
        }
        // Cancel before offering a step: no command has entered admission yet.
        // Admission still checks the fence atomically for a concurrent override.
        if let Some((_, step)) = self.timeline.due_step(now) {
            let device = step.device.clone();
            if self.admission.manual_superseded(device.clone(), self.manual_fence)
                .await.map_err(|e| format!("scene manual fence: {e:?}"))?
            {
                self.cancel_device(&device).await?;
                return Ok(None);
            }
        }
        let Some(due) = self
            .timeline
            .next(now, current.epochs)
            .map_err(|e| format!("{e:?}"))?
        else {
            return Ok(None);
        };
        self.audit_expiries().await?;
        let mut due_steps=vec![due.clone()];
        let additional=self.timeline.extend_due_phase(due.index,|index,step|!self.configured_fades.contains_key(&index)&&matches!(step.command,homesense_core::Command::Switch(_)|homesense_core::Command::SetLevel{..}|homesense_core::Command::PanelLed(_)|homesense_core::Command::PanelLedOff));
        match additional{Ok(additional)=>due_steps.extend(additional),Err(error)=>{self.timeline.resolve(due.index,false).map_err(|e|format!("{e:?}"))?;self.timeline.cancel();return Err(format!("phase offer: {error:?}"));}}
        let device=due.step.device.clone();
        let batch=Batch{controller:self.controller,intents:due_steps.iter().map(|due|Intent{device:due.step.device.clone(),command:due.step.command}).collect(),subscriptions:vec![]};
        let links=due_steps.iter().map(|due|homesense_storage::scene_run::StepLink{sample:None,run:self.audit_id.clone(),index:due.index,source:due.step.source.clone()}).collect::<Vec<_>>();
        let deadline=self.causal_deadline; // retain the original admission TTL and any causal fence
        let accepted=if due_steps.len()>1 {self.admission.submit_scene_phase(self.generation.clone(),batch,self.manual_fence,links,deadline).await}
        else{self.admission.submit_scene_step(self.generation.clone(),batch,self.manual_fence,links[0].clone(),deadline).await.map(|ids|(ids,vec![]))};
        match accepted {
            Ok((ids,rows))=>{
                let members=if due_steps.len()==1{vec![PhaseMember{index:due.index,device:device.clone(),ids:ids.clone()}]}else{due_steps.iter().map(|due|PhaseMember{index:due.index,device:due.step.device.clone(),ids:rows.iter().filter(|row|row.step==due.index).map(|row|row.command_id).collect()}).collect()};
                self.admitted.push(ids[0]);self.pending=Some(Pending{members,device,index:due.index,run:ids[0],ids:ids.clone(),written:0});Ok(Some(ids))
            }
            Err(crate::admission::SubmissionFailure::ManualSuperseded)=>{
                // This distinct actor result proves zero accepted commands and
                // zero variable updates. Retain unaffected peers, with their
                // original due/deadline; only superseded devices lose timers.
                for due in &due_steps{
                    if self.admission.manual_superseded(due.step.device.clone(),self.manual_fence).await.map_err(|e|format!("phase manual fence: {e:?}"))?{
                        self.timeline.cancel_offered(due.index).map_err(|e|format!("{e:?}"))?;
                        self.cancellation_audited.entry(due.step.device.clone()).or_insert(false);self.cancel_device(&due.step.device).await?;
                    }else{self.timeline.return_unadmitted(due.index).map_err(|e|format!("{e:?}"))?;}
                }
                Ok(None)
            }
            Err(crate::admission::SubmissionFailure::Status(
                hap::server::WriteStatus::InvalidValueInRequest
                | hap::server::WriteStatus::InsufficientPrivileges
                | hap::server::WriteStatus::ResourceDoesNotExist,
            )) => {
                // Rejected admission produced no command. End only this run,
                // preserve any earlier written output as Partial, and durably
                // checkpoint via poll(). Storage/generation/transport failures
                // still propagate and gate the service.
                log::warn!("scene step rejected run={} step={}; remaining steps cancelled", self.audit_id, due.index);
                for due in &due_steps{self.timeline.resolve(due.index,false).map_err(|e|format!("{e:?}"))?;}
                self.timeline.cancel();
                Ok(None)
            }
            Err(e) => {
                for due in &due_steps{self.timeline.resolve(due.index,false).map_err(|e|format!("{e:?}"))?;}
                Err(format!("scene admission: {e:?}"))
            }
        }
    }
    /// Resolve overwritten/cancelled output even when no transport callback will
    /// arrive. Durable Writing remains pending; it is never treated as success.
    pub async fn reconcile(&mut self) -> Result<(), String> {
        use homesense_storage::journal::Status as Audit;
        self.reconcile_fade().await?;
        let Some(pending) = &self.pending else {
            return self.checkpoint().await;
        };
        let states = self
            .admission
            .command_statuses(
                self.controller,
                self.generation.clone(),
                pending.ids.clone(),
            )
            .await
            .map_err(|e| format!("scene audit: {e:?}"))?;
        if states
            .iter()
            .any(|s| matches!(s, Audit::Accepted | Audit::Writing))
        {
            return Ok(());
        }
        if pending.members.len()>1 {
            for member in &pending.members{
                let samples=member.ids.iter().map(|id|states[pending.ids.iter().position(|candidate|candidate==id).expect("phase receipt member")]).collect::<Vec<_>>();
                let count=samples.iter().filter(|s|matches!(s,Audit::Written|Audit::Observed)).count();
                let failed=samples.iter().any(|s|matches!(s,Audit::Failed|Audit::Uncertain));
                if failed{self.timeline.cancel();}else if samples.contains(&Audit::Cancelled){self.timeline.cancel_device(&member.device);}
                let result=if count==samples.len(){self.timeline.resolve(member.index,true)}else if count>0||samples.contains(&Audit::Uncertain){self.timeline.resolve_partial(member.index)}else if failed{self.timeline.resolve(member.index,false)}else{self.timeline.cancel_offered(member.index)};
                result.map_err(|e|format!("phase resolve: {e:?}"))?;
            }
        }else{
            let written=pending.written+states.iter().filter(|s|matches!(s,Audit::Written|Audit::Observed)).count();
            let failed=states.iter().any(|s|matches!(s,Audit::Failed|Audit::Uncertain));
            if failed{self.timeline.cancel();}else if states.contains(&Audit::Cancelled){self.timeline.cancel_device(&pending.device);}
            let result=if written==pending.written+states.len(){self.timeline.resolve(pending.index,true)}else if written>0||states.contains(&Audit::Uncertain){self.timeline.resolve_partial(pending.index)}else if failed{self.timeline.resolve(pending.index,false)}else{self.timeline.cancel_offered(pending.index)};
            result.map_err(|e|format!("{e:?}"))?;
        }
        self.admitted.retain(|run| *run != pending.run);
        self.pending = None;
        self.checkpoint().await
    }
    /// Report each durable TransportOwner result. Failed output is never retried.
    pub async fn written(&mut self, id: u64, success: bool) -> Result<(), String> {
        if self.pending.as_ref().is_some_and(|pending|pending.members.len()>1){return self.reconcile().await;}
        let pending = self.pending.as_mut().ok_or("no scene action pending")?;
        if pending.ids.first() != Some(&id) {
            return Err("unexpected scene completion".into());
        }
        if !success {
            self.timeline.cancel();
            // Admission::finish_write(false), called by TransportOwner before
            // this notification, durably cancels all accepted output and gates
            // the failed transport. A second cancellation would be rejected by
            // that gate; never use this notification before finish_write.
            if pending.written > 0 {
                self.timeline.resolve_partial(pending.index)
            } else {
                self.timeline.resolve(pending.index, false)
            }
            .map_err(|e| format!("{e:?}"))?;
            self.admitted.retain(|run| *run != pending.run);
            self.pending = None;
            return self.checkpoint().await;
        }
        pending.ids.remove(0);
        pending.written += 1;
        if pending.ids.is_empty() {
            self.timeline
                .resolve(pending.index, true)
                .map_err(|e| format!("{e:?}"))?;
            self.admitted.retain(|run| *run != pending.run);
            self.pending = None;
        }
        self.checkpoint().await
    }
    pub fn cancel_device_timers(&mut self, device: &DeviceId) -> usize {
        let fading=self.fade.as_ref().is_some_and(|fade| &fade.device==device);
        if let Some(fade)=&mut self.fade && &fade.device==device {fade.stopping=true;}
        let count=self.timeline.cancel_device(device);
        if count>0 || fading {self.cancellation_audited.entry(device.clone()).or_insert(false);}
        count
    }
    /// Call after an accepted manual override or before a replacement scene is
    /// started. Invalidate future work before awaiting durable cancellation.
    /// A Writing command cannot be recalled and remains pending until settled.
    pub async fn cancel_device(&mut self, device: &DeviceId) -> Result<usize, String> {
        self.cancel_device_with_reason(device,homesense_storage::scene_run::CancellationReason::ManualOverride).await
    }
    pub(crate) async fn cancel_device_with_reason(&mut self, device: &DeviceId, reason: homesense_storage::scene_run::CancellationReason) -> Result<usize,String> {
        let mut count = self.cancel_device_timers(device);
        if self.pending.as_ref().is_some_and(|pending|pending.members.iter().any(|member|&member.device==device)) {
            self.cancellation_audited.entry(device.clone()).or_insert(false);
        }
        if self.cancellation_audited.get(device)==Some(&false) {
            self.persist_checkpoint(homesense_storage::scene_run::Status::Running,Some(homesense_storage::scene_run::Cancellation{device:device.clone(),reason})).await?;
            self.cancellation_audited.insert(device.clone(),true);
        }
        if self.fade.as_ref().is_some_and(|f|f.stopping) {self.reconcile_fade().await?;}
        if let Some(pending) = &self.pending
            && pending.members.iter().any(|member|&member.device==device)
        {
            count += self
                .admission
                .cancel_scene_members(self.controller,self.generation.clone(),pending.run,pending.members.iter().filter(|m|&m.device==device).flat_map(|m|m.ids.iter().copied()).collect())
                .await
                .map_err(|e| format!("scene device cancellation: {e:?}"))?;
            self.reconcile().await?;
        }
        self.checkpoint().await?;
        Ok(count)
    }
    pub(crate) async fn cancel_revoked_source(&mut self)->Result<(),String> {
        self.cancel_with_reason(homesense_storage::scene_run::CancellationReason::CancelRequested).await?;
        self.checkpoint().await
    }
    pub async fn cancel(&mut self) -> Result<usize, String> {
        self.cancel_with_reason(homesense_storage::scene_run::CancellationReason::CancelRequested).await
    }
    pub(crate) async fn cancel_with_reason(&mut self, reason: homesense_storage::scene_run::CancellationReason) -> Result<usize,String> {
        let reason=*self.stop_reason.get_or_insert(reason);
        // Invalidate volatile timers even if durable cancellation fails; caller
        // may retry cancellation, but no further timed action can be admitted.
        let mut count = self.cancel_timers();
        let devices: Vec<_> = self.cancel_requests.iter()
            .filter(|(_, saved)| !**saved).map(|(device, _)| device.clone()).collect();
        for device in devices {
            self.persist_checkpoint(homesense_storage::scene_run::Status::Running,
                Some(homesense_storage::scene_run::Cancellation {
                    device: device.clone(),
                    reason,
                })).await?;
            self.cancel_requests.insert(device, true);
        }
        // A transport failure may already have made every in-flight command terminal.
        self.reconcile().await?;
        for run in &self.admitted {
            let cancelled = self
                .admission
                .cancel_scene_run(self.controller, self.generation.clone(), *run)
                .await
                .map_err(|e| format!("{e:?}"))?;
            count += cancelled;
            if let Some(pending) = &self.pending
                && pending.run == *run
                && cancelled == pending.ids.len()
            {
                for member in &pending.members{
                    if pending.written>0{self.timeline.resolve_partial(member.index)}else{self.timeline.cancel_offered(member.index)}.map_err(|e|format!("{e:?}"))?;
                }
                self.pending = None;
            }
        }
        self.reconcile().await?;
        Ok(count)
    }
    pub(crate) fn cancel_timers(&mut self) -> usize {
        // Retain requests across the scheduler's synchronous timer barrier.
        // Offered work may already be writing; this records intent, not recall.
        for (index, state) in self.timeline.states().iter().enumerate() {
            if matches!(state, homesense_core::scene::State::Waiting | homesense_core::scene::State::Offered) {
                let device = self.timeline.step(index).expect("timeline step").device.clone();
                self.cancel_requests.entry(device).or_insert(false);
            }
        }
        if let Some(fade)=&mut self.fade {fade.stopping=true;}
        self.timeline.cancel()
    }
    pub(crate) fn fade_reservation(&self) -> Option<(u64,u64)> {
        self.fade.as_ref().map(|fade|fade.driver.reservation())
    }
    pub(crate) fn has_pending(&self) -> bool {
        self.pending.is_some() || self.fade.is_some()
    }
    /// Read-only projection; successful writes are not observed target evidence.
    pub async fn activity(
        &self,
        feedback: &crate::feedback::Feedback,
        max_age: u64,
    ) -> Result<crate::scene_activity::SceneActivity, String> {
        self.targets.project(&self.admission,self.controller,&self.generation,feedback,max_age).await
    }

    pub(crate) fn definition(&self) -> Option<&str> { self.definition.as_deref() }
    pub fn audit_id(&self) -> &str {
        &self.audit_id
    }
    async fn audit_expiries(&mut self) -> Result<(), String> {
        if self.audit_failed { return Err("scene audit unavailable".into()); }
        let expired: Vec<_> = self.timeline.states().iter().enumerate()
            .filter(|(index,state)| **state == homesense_core::scene::State::Expired && !self.expiries_audited.contains(index))
            .map(|(index,_)| {
                let step=self.timeline.step(index).expect("timeline step");
                homesense_storage::scene_run::Expiry{index,device:step.device.clone(),source:step.source.clone()}
            }).collect();
        for expiry in expired {
            let index=expiry.index;
            self.audit_failed=true;
            self.audit_version=self.admission.scene_update(self.controller,self.generation.clone(),
                crate::admission::SceneUpdate::Expiry{run:self.audit_id.clone(),expected:self.audit_version,expiry})
                .await.map_err(|e|format!("scene expiry checkpoint: {e:?}"))?;
            self.audit_status=homesense_storage::scene_run::Status::Running;
            self.expiries_audited.insert(index);
            self.audit_failed=false;
        }
        Ok(())
    }
    async fn checkpoint(&mut self) -> Result<(), String> {
        self.audit_expiries().await?;
        use homesense_storage::scene_run::Status as Audit;
        if self.audit_failed {
            return Err("scene audit unavailable".into());
        }
        let status = if self.has_pending() {
            Audit::Running
        } else {
            match self.timeline.status() {
                Status::Pending if self.audit_status == Audit::Running => Audit::Running,
                Status::Pending => Audit::Pending,
                Status::Running => Audit::Running,
                Status::CommandsWritten => Audit::CommandsWritten,
                Status::Partial => Audit::Partial,
                Status::Cancelled => Audit::Cancelled,
                Status::Failed => Audit::Failed,
            }
        };
        self.persist_checkpoint(status,None).await
    }
    async fn persist_checkpoint(&mut self, status: homesense_storage::scene_run::Status, cancellation: Option<homesense_storage::scene_run::Cancellation>) -> Result<(),String> {
        if self.audit_failed {return Err("scene audit unavailable".into());}
        if status == self.audit_status && cancellation.is_none() {
            return Ok(());
        }
        // Cancellation of this await leaves the run gated, never resubmitted.
        self.audit_failed = true;
        let version = self
            .admission
            .scene_update(
                self.controller,
                self.generation.clone(),
                crate::admission::SceneUpdate::Checkpoint {
                    run: self.audit_id.clone(),
                    expected: self.audit_version,
                    status,
                    cancellation,
                },
            )
            .await
            .map_err(|e| format!("scene checkpoint: {e:?}"))?;
        self.audit_version = version;
        self.audit_status = status;
        self.audit_failed = false;
        Ok(())
    }
    pub fn status(&self) -> Status {
        self.timeline.status()
    }
}
