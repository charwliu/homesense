//! Bounded, explicitly driven scene scheduler. Construction does not enable live
//! automation. The service owns tick cadence and transport dispatch separately.
use crate::{admission::Admission, scene_runtime::SceneRuntime};
use homesense_core::{
    DeviceId,
    scene::{Status, Step},
};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;
use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};

pub struct SceneScheduler {
    admission: Admission,
    identity: Uuid,
    controller: Uuid,
    runs: BTreeMap<u64, SceneRuntime>,
    next: u64,
    now: u64,
    capacity: usize,
    reserved: Arc<AtomicUsize>,
    stopped: bool,
    results: Vec<SceneResult>,
}
/// Capacity only, never authorization or a durable acceptance. Dropping a token
/// releases unused slots, including when a surrounding future is cancelled.
pub struct Reservation {
    owner: Arc<AtomicUsize>,
    remaining: usize,
}
impl Reservation {
    pub fn remaining(&self) -> usize {self.remaining}
}
impl Drop for Reservation {
    fn drop(&mut self) {self.owner.fetch_sub(self.remaining, Ordering::SeqCst);}
}
#[derive(Debug)]
pub struct SceneResult {
    pub run: u64,
    pub definition: String,
    pub audit_id: String,
    pub status: Status,
}
#[derive(Debug)]
pub struct Tick {
    pub run: u64,
    pub result: Result<Vec<u64>, String>,
    pub status: Status,
}
pub(crate) struct SceneSchedulerSourceScope<'a> {owner:&'a mut SceneScheduler,previous:Admission}
impl std::ops::Deref for SceneSchedulerSourceScope<'_> {type Target=SceneScheduler;fn deref(&self)->&Self::Target{self.owner}}
impl std::ops::DerefMut for SceneSchedulerSourceScope<'_> {fn deref_mut(&mut self)->&mut Self::Target{self.owner}}
impl Drop for SceneSchedulerSourceScope<'_> {fn drop(&mut self){self.owner.admission=self.previous.clone();}}
impl SceneScheduler {
    pub(crate) fn source_scope(&mut self,source:Option<crate::source_auth::Source>)->SceneSchedulerSourceScope<'_> {
        let previous=self.admission.clone();self.admission=self.admission.with_source(source);
        SceneSchedulerSourceScope{owner:self,previous}
    }

    pub(crate) fn publication_impact(&self) -> Vec<crate::configuration_preview::Run> {
        self.runs.iter().filter(|(_,run)|!run.settled_terminal()).map(|(id,run)|crate::configuration_preview::Run {
            run:*id, audit_id:run.audit_id().to_owned(),definition:run.definition().map(str::to_owned),status:format!("{:?}",run.status()),
        }).collect()
    }

    pub fn owned_by(&self,run:u64,controller:Uuid)->bool {self.runs.get(&run).is_some_and(|r|r.controller()==controller)}
    pub fn new(
        admission: Admission,
        controller: Uuid,
        capacity: usize,
        now: u64,
    ) -> Result<Self, String> {
        if capacity == 0 || capacity > 64 {
            return Err("scene capacity must be 1..64".into());
        }
        Ok(Self {
            admission,
            identity: Uuid::new_v4(),
            controller,
            runs: BTreeMap::new(),
            next: 1,
            now,
            capacity,
            reserved: Arc::new(AtomicUsize::new(0)),
            stopped: false,
            results: vec![],
        })
    }
    /// Reserve before committing rule variables. Other starts must respect these
    /// slots; this API alone creates no scene record or physical command.
    pub fn reserve_slots(&mut self, count:usize) -> Result<Reservation,String> {
        if !self.stopped && count > 0 && count <= self.capacity {
            self.reclaim_delivered_slots(count);
        }
        if self.stopped || count==0 || count>self.capacity || self.runs.len()+self.results.len()+self.reserved.load(Ordering::SeqCst)>self.capacity-count {
            return Err("scene reservation capacity unavailable".into());
        }
        self.reserved.fetch_add(count,Ordering::SeqCst);
        Ok(Reservation{owner:self.reserved.clone(),remaining:count})
    }
    /// A delivered terminal is a bounded history cache, not live execution.
    /// Keep it queryable until a new admission actually needs its slot. Never
    /// evict an active run, an undelivered result, or pending physical output.
    fn reclaim_delivered_slots(&mut self, count: usize) {
        let occupied = self.runs.len() + self.results.len() + self.reserved.load(Ordering::SeqCst);
        let needed = occupied.saturating_add(count).saturating_sub(self.capacity);
        if needed == 0 { return; }
        let completed = self.runs.iter()
            .filter(|(_, run)| run.delivered_terminal())
            .map(|(id, _)| *id)
            .take(needed)
            .collect::<Vec<_>>();
        // Failed reservations must not discard even cached terminal status.
        if completed.len() == needed {
            for id in completed { self.runs.remove(&id); }
        }
    }
    /// Consume one slot exactly once, including on failed or cancelled start.
    /// This remains a manual start; atomic rule admission is a separate adapter.
    pub async fn start_reserved(&mut self, reservation:&mut Reservation, steps:Vec<Step>, now:u64, lateness:u64)->Result<u64,String> {
        if !Arc::ptr_eq(&self.reserved,&reservation.owner) || reservation.remaining==0 || self.stopped {return Err("invalid scene reservation".into());}
        reservation.remaining-=1;
        reservation.owner.fetch_sub(1,Ordering::SeqCst);
        self.start(steps,now,lateness).await
    }
    /// Install an entire accepted batch under a prior capacity reservation.
    /// No member is made runnable before every member is constructed. A failed
    /// or abandoned installation gates the admission owner; receipts are never
    /// replayed automatically after that failure.
    pub async fn install_accepted(&mut self, reservation:&mut Reservation, accepted:Vec<crate::admission::AcceptedScene>, now:u64)->Result<Vec<u64>,String> {
        self.install_accepted_as(reservation,accepted,now,self.controller).await
    }
    pub(crate) async fn install_accepted_as(&mut self, reservation:&mut Reservation, accepted:Vec<crate::admission::AcceptedScene>, now:u64, controller:Uuid)->Result<Vec<u64>,String> {
        self.clock(now)?;
        if !Arc::ptr_eq(&self.reserved,&reservation.owner) || accepted.is_empty() || accepted.len()>reservation.remaining {return Err("invalid batch reservation".into());}
        struct Guard(Admission,bool);
        impl Drop for Guard {fn drop(&mut self){if !self.1 {self.0.stop_now();}}}
        let mut guard=Guard(self.admission.clone(),false);
        let count=accepted.len();
        let end=self.next.checked_add(count as u64).ok_or("scene id exhausted")?;
        let generation=self.admission.task_generation().await.map_err(|e|format!("{e:?}"))?;
        if accepted.iter().any(|a|a.controller!=controller || a.generation!=generation) {return Err("accepted batch owner or generation mismatch".into());}
        let bundle=self.admission.automation_bundle(controller,generation).await.map_err(|e|format!("{e:?}"))?;
        let document=bundle.automation.ok_or("accepted automation missing")?;
        let mut devices=BTreeSet::new();
        let mut batch_devices=Vec::with_capacity(count);
        for token in &accepted {
            let definition=document.timed_scenes.get(&token.start.definition).ok_or("accepted scene missing")?;
            let targets: BTreeSet<_> = definition.steps.iter().map(|s|s.device.clone()).collect();
            devices.extend(targets.iter().cloned());
            batch_devices.push(targets);
        }
        let mut constructed=Vec::with_capacity(count);
        for token in accepted {constructed.push(SceneRuntime::from_accepted(self.admission.clone(),token).await?);}
        for old in self.runs.values_mut() {
            for device in &devices {old.cancel_device_with_reason(device,homesense_storage::scene_run::CancellationReason::SceneReplacement).await?;}
        }
        // Batch order is source order: a later scene supersedes only the
        // overlapping devices of earlier scenes, just like separate starts.
        // All cancellation audits settle before any new member becomes runnable.
        for (index, targets) in batch_devices.iter().enumerate().skip(1) {
            for earlier in &mut constructed[..index] {
                for device in targets { earlier.cancel_device_with_reason(device,homesense_storage::scene_run::CancellationReason::SceneReplacement).await?; }
            }
        }
        let ids=(self.next..end).collect::<Vec<_>>();
        for (id,scene) in ids.iter().copied().zip(constructed) {self.runs.insert(id,scene);}
        self.next=end;
        reservation.remaining-=count;
        reservation.owner.fetch_sub(count,Ordering::SeqCst);
        guard.1=true;
        Ok(ids)
    }
    fn clock(&mut self, now: u64) -> Result<(), String> {
        if self.stopped {
            return Err("scene scheduler stopped".into());
        }
        if now < self.now {
            return Err("scene clock moved backwards".into());
        }
        self.now = now;
        Ok(())
    }
    /// Explicit start of an enabled, reviewed file definition. Loading a document
    /// never calls this method and therefore never starts timers or commands.
    pub async fn start_document(
        &mut self,
        document: &homesense_config::rule_document::Document,
        id: &str,
        now: u64,
    ) -> Result<u64, String> {
        self.start_document_as(document, id, now, self.controller).await
    }
    /// Starts under the authenticated caller, preserving its identity for later
    /// command admission and revocation checks.
    pub async fn start_document_as(
        &mut self, document: &homesense_config::rule_document::Document,
        id: &str, now: u64, controller: Uuid,
    ) -> Result<u64, String> {
        let generation = self
            .admission
            .task_generation()
            .await
            .map_err(|e| format!("{e:?}"))?;
        if document.revision != generation.epochs.configuration {
            return Err("timed scene document revision expired".into());
        }
        let scene = document.timed_scenes.get(id).ok_or("unknown timed scene")?;
        if !scene.enabled
            || scene
                .review_evidence
                .as_ref()
                .is_none_or(|s| s.trim().is_empty())
        {
            return Err("timed scene disabled or unreviewed".into());
        }
        let run=self.start_inner(
            scene.steps.clone(),
            now,
            scene.max_lateness_ms,
            Some(document.revision),
            Some(id.to_owned()),
            controller,
        )
        .await?;
        self.runs.get_mut(&run).unwrap().configured_fades=scene.fades.clone();
        Ok(run)
    }
    /// Reserve all slots before durable acceptance; retain the actual caller.
    /// No scene can become runnable until the complete batch is installed.
    pub async fn start_documents_as(&mut self, revision:u64, definitions:Vec<String>, now:u64, controller:Uuid)->Result<Vec<u64>,String> {
        self.start_documents_with_variables_as(revision,definitions,now,controller,vec![]).await
    }
    pub(crate) async fn start_documents_with_variables_as(&mut self,revision:u64,definitions:Vec<String>,now:u64,controller:Uuid,updates:Vec<homesense_storage::variables::Update>)->Result<Vec<u64>,String> {
        self.clock(now)?;
        self.next.checked_add(definitions.len() as u64).ok_or("scene id exhausted")?;
        let mut slots=self.reserve_slots(definitions.len())?;
        let generation=self.admission.task_generation().await.map_err(|e|format!("{e:?}"))?;
        if generation.epochs.configuration!=revision {return Err("scene batch revision expired".into());}
        struct Guard(Admission,bool);
        impl Drop for Guard {fn drop(&mut self){if !self.1 {self.0.stop_now();}}}
        let mut guard=Guard(self.admission.clone(),false);
        let accepted=match self.admission.accept_manual_scenes_with_variables(controller,generation,definitions,updates).await {
            Ok(accepted)=>accepted,
            Err(error)=>{guard.1=true;return Err(format!("{error:?}"));}
        };
        let runs=self.install_accepted_as(&mut slots,accepted,now,controller).await?;
        guard.1=true;
        Ok(runs)
    }
    /// New scene targets supersede future and queued output for the same device.
    /// Cancellation failure prevents installation of the new scene; already
    /// cancelled old actions remain cancelled and are never restored.
    pub async fn start(
        &mut self,
        steps: Vec<Step>,
        now: u64,
        lateness: u64,
    ) -> Result<u64, String> {
        self.start_inner(steps, now, lateness, None, None, self.controller).await
    }
    async fn start_inner(
        &mut self,
        steps: Vec<Step>,
        now: u64,
        lateness: u64,
        revision: Option<u64>,
        definition: Option<String>,
        controller: Uuid,
    ) -> Result<u64, String> {
        self.clock(now)?;
        self.reclaim_delivered_slots(1);
        if self.runs.len() + self.results.len() + self.reserved.load(Ordering::SeqCst) >= self.capacity {
            return Err("scene capacity reached; retire terminal runs".into());
        }
        let next = self.next.checked_add(1).ok_or("scene id exhausted")?;
        let devices: BTreeSet<_> = steps.iter().map(|s| s.device.clone()).collect();
        let scene = SceneRuntime::new_named(
            self.admission.clone(),
            controller,
            steps,
            now,
            lateness,
            definition,
        )
        .await?;
        if revision.is_some_and(|r| r != scene.configuration_revision()) {
            return Err("timed scene generation changed during start".into());
        }
        for old in self.runs.values_mut() {
            for device in &devices {
                old.cancel_device_with_reason(device,homesense_storage::scene_run::CancellationReason::SceneReplacement).await?;
            }
        }
        let id = self.next;
        self.next = next;
        self.runs.insert(id, scene);
        Ok(id)
    }
    /// Install explicitly prepared fade work into an existing scene. Reserve a
    /// shared allocation of 8 fades, 1024 samples and 20 targets/second across
    /// this scheduler. Expanded wire frames and other producers still require
    /// service-wide budgeting; fresh feedback preparation belongs to the caller.
    pub async fn install_due_fade(&mut self, run: u64, mut fade: crate::software_fade::SoftwareFade, now: u64) -> Result<(), String> {
        if let Err(error)=self.clock(now) {
            fade.cancel().await?;
            return Err(error);
        }
        let (samples,rate)=fade.reservation();
        let active=self.runs.values().filter_map(SceneRuntime::fade_reservation).collect::<Vec<_>>();
        if active.len()>=8 || active.iter().map(|v|v.0).sum::<u64>()+samples>1024
            || active.iter().map(|v|v.1).sum::<u64>()+rate>20 {
            fade.cancel().await?;
            return Err("scene fade allocation exhausted".into());
        }
        let Some(scene)=self.runs.get_mut(&run) else {
            fade.cancel().await?;
            return Err("unknown scene run".into());
        };
        scene.install_due_fade(fade,now).await
    }
    /// Called after a manual control has been admitted by the command owner.
    pub async fn manual_override(&mut self, device: &DeviceId) -> Result<usize, String> {
        let mut cancelled = self
            .runs
            .values_mut()
            .map(|run| run.cancel_device_timers(device))
            .sum::<usize>();
        for run in self.runs.values_mut() {
            cancelled += run.cancel_device(device).await?;
        }
        Ok(cancelled)
    }
    /// At most one admission per scene per tick. Transport remains the sole
    /// writer; waiting for Writing is normal and does not fail other scenes.
    pub async fn tick(&mut self, now: u64) -> Result<Vec<Tick>, String> {
        self.tick_with_feedback(now,None).await
    }
    pub async fn tick_with_feedback(&mut self, now:u64, feedback:Option<(&crate::feedback::Feedback,u64)>) -> Result<Vec<Tick>,String> {
        self.clock(now)?;
        let mut results=vec![];
        for id in self.runs.keys().copied().collect::<Vec<_>>() {
            if !matches!(self.runs[&id].status(),Status::Pending|Status::Running) {continue;}
            let result=self.tick_run(id,now,feedback).await;
            results.push(Tick{run:id,result,status:self.runs[&id].status()});
        }
        Ok(results)
    }
    async fn tick_run(&mut self,id:u64,now:u64,feedback:Option<(&crate::feedback::Feedback,u64)>)->Result<Vec<u64>,String> {
        let run=self.runs.get_mut(&id).unwrap();
        if !run.source_valid() {run.cancel_revoked_source().await?;return Ok(vec![]);}
        run.reconcile().await?;
        if run.has_pending() {return Ok(vec![]);}
        let controller=run.controller();
        let source_authorization=run.source_authorization();
        if let Some((step,timing))=run.due_fade(now) {
            let prepared=async {
                let (feedback,max_age)=feedback.ok_or("configured fade requires fresh feedback")?;
                let homesense_core::Command::SetLevel{level,..}=step.command else {return Err("invalid fade target".into());};
                let fade=crate::software_fade::SoftwareFade::start(self.admission.with_source(source_authorization),controller,step.device,level,
                    timing.duration_ms,timing.interval_ms,timing.lateness_ms,timing.samples().map_err(|e|e.to_string())?,feedback,max_age).await?;
                self.install_due_fade(id,fade,now).await
            }.await;
            if let Err(error)=prepared {
                self.runs.get_mut(&id).unwrap().cancel_with_reason(homesense_storage::scene_run::CancellationReason::PreparationFailed).await?;
                return Err(error);
            }
            return Ok(vec![]);
        }
        self.runs.get_mut(&id).unwrap().poll(now).await.map(|ids|ids.unwrap_or_default())
    }
    pub(crate) fn settled_terminal(&self, run:u64)->bool {
        self.runs.get(&run).is_some_and(SceneRuntime::settled_terminal)
    }
    pub(crate) fn identity(&self)->Uuid {self.identity}
    pub(crate) fn audit_id(&self,run:u64)->Option<&str> {self.runs.get(&run).map(SceneRuntime::audit_id)}
    pub(crate) fn matches_controller(&self, controller:Uuid)->bool {self.controller==controller}
    pub(crate) fn matches_admission(&self, admission: &Admission) -> bool {
        self.admission.boot_id() == admission.boot_id()
    }
    pub(crate) async fn cancel_gateway_operations(&mut self,operations:&[homesense_storage::gateway_audit::OperationKey])->Result<(),String> {
        let targets=operations.iter().filter(|key|key.kind==homesense_storage::gateway_audit::OperationKind::Scene)
            .map(|key|key.id.as_str()).collect::<std::collections::BTreeSet<_>>();
        for run in self.runs.values_mut().filter(|run|targets.contains(run.audit_id())) {
            run.cancel_timers();
            run.cancel_with_reason(homesense_storage::scene_run::CancellationReason::TransportFailure).await?;
            run.reconcile().await?;
        }
        Ok(())
    }
    pub fn stop_now(&mut self) {
        self.stopped = true;
        for run in self.runs.values_mut() {
            run.cancel_timers();
        }
    }
    /// Disable admission synchronously before the first await. All volatile
    /// timers are invalidated even if cancellation fails or this future is dropped.
    /// Call again to retry cleanup; reopening requires a fresh scheduler.
    pub async fn stop(&mut self) -> Vec<(u64, Result<usize, String>)> {
        self.stop_with_reason(homesense_storage::scene_run::CancellationReason::ServiceStopped).await
    }
    pub(crate) async fn stop_with_reason(&mut self,reason:homesense_storage::scene_run::CancellationReason)->Vec<(u64,Result<usize,String>)> {
        self.stopped = true;
        let mut counts = BTreeMap::new();
        for (id, run) in &mut self.runs {
            counts.insert(*id, run.cancel_timers());
        }
        let mut results = vec![];
        for (id, run) in &mut self.runs {
            let cancelled = run.cancel_with_reason(reason).await.map(|count| count + counts[id]);
            // A changed generation may reject cancellation after the transport
            // owner already made the commands terminal. Still settle from audit.
            let reconciled = run.reconcile().await;
            results.push((
                *id,
                match (cancelled, reconciled) {
                    (Err(error), _) | (_, Err(error)) => Err(error),
                    (Ok(count), Ok(())) => Ok(count),
                },
            ));
        }
        results
    }
    /// Consume terminal named results once. No replay after restart; the durable
    /// receipt remains the authoritative historical record.
    pub fn take_results(&mut self) -> Vec<SceneResult> {
        let mut results = std::mem::take(&mut self.results);
        for (id, run) in &mut self.runs {
            if let Some((definition, audit_id, status)) = run.take_result() {
                results.push(SceneResult {
                    run: *id,
                    definition,
                    audit_id,
                    status,
                });
            }
        }
        results
    }
    /// Observed target projection for a retained run; never inferred from status.
    pub async fn activity(&self, id:u64, feedback:&crate::feedback::Feedback, max_age_ms:u64) -> Result<Option<crate::scene_activity::SceneActivity>,String> {
        match self.runs.get(&id) {
            Some(run)=>run.activity(feedback,max_age_ms).await.map(Some),
            None=>Ok(None),
        }
    }
    pub fn status(&self, id: u64) -> Option<Status> {
        self.runs.get(&id).map(SceneRuntime::status)
    }
    pub async fn cancel(&mut self, id: u64) -> Result<usize, String> {
        self.cancel_with_reason(id,homesense_storage::scene_run::CancellationReason::CancelRequested).await
    }
    pub(crate) async fn cancel_with_reason(&mut self,id:u64,reason:homesense_storage::scene_run::CancellationReason)->Result<usize,String> {
        self.runs
            .get_mut(&id)
            .ok_or("unknown scene run")?
            .cancel_with_reason(reason)
            .await
    }
    /// Explicit retirement prevents silently losing a terminal run's result.
    pub fn retire(&mut self, id: u64) -> Result<Status, String> {
        let status = self.status(id).ok_or("unknown scene run")?;
        if matches!(status, Status::Pending | Status::Running) {
            return Err("scene still active".into());
        }
        if let Some((definition, audit_id, status)) =
            self.runs.get_mut(&id).and_then(SceneRuntime::take_result)
        {
            self.results.push(SceneResult {
                run: id,
                definition,
                audit_id,
                status,
            });
        }
        self.runs.remove(&id);
        Ok(status)
    }
}
