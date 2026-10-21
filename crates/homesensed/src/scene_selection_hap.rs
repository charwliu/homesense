//! Read-only selection projection. The actor is sampled directly, independently
//! of gateway polling and of HAP accessory locks. Unknown never becomes false.
use crate::admission::{Admission, TaskGeneration};
use crate::scene_switches::{SelectionState, UnavailableReason};
use homesense_config::scene_switches::Group;
use std::{
    collections::BTreeMap,
    sync::{Arc, RwLock},
    time::{Duration, Instant},
};
use uuid::Uuid;

const MAX_AGE: Duration = Duration::from_secs(3);
struct Snapshot {
    sampled: Instant,
    values: BTreeMap<String, Option<bool>>,
}
#[derive(Clone, Default)]
pub(crate) struct Cache(Arc<RwLock<Cached>>);
#[derive(Default)]
struct Cached {
    snapshot: Option<Snapshot>,
    unavailable: UnavailableReason,
}
impl Cache {
    pub fn invalidate(&self) {
        self.unavailable(UnavailableReason::Invalidated);
    }
    pub fn unavailable(&self, reason: UnavailableReason) {
        if let Ok(mut cached) = self.0.write() {
            cached.snapshot = None;
            cached.unavailable = reason;
        }
    }
    pub fn install(&self, values: BTreeMap<String, Option<bool>>) -> Result<(), String> {
        *self
            .0
            .write()
            .map_err(|_| "scene selection cache unavailable")? = Cached {
            snapshot: Some(Snapshot {
                sampled: Instant::now(),
                values,
            }),
            unavailable: UnavailableReason::NotSampled,
        };
        Ok(())
    }
    pub fn read(&self, scene: &str) -> SelectionState {
        let Ok(cached) = self.0.read() else {
            return SelectionState::Unavailable(UnavailableReason::ReadFailed);
        };
        let Some(snapshot) = &cached.snapshot else {
            return SelectionState::Unavailable(cached.unavailable);
        };
        if snapshot.sampled.elapsed() >= MAX_AGE {
            return SelectionState::Unavailable(UnavailableReason::CacheExpired);
        }
        match snapshot.values.get(scene) {
            Some(value) => SelectionState::from_selected(*value),
            None => SelectionState::Unavailable(UnavailableReason::ReadFailed),
        }
    }
}
#[derive(Clone)]
pub(crate) struct Source {
    admission: Admission,
    generation: TaskGeneration,
    controller: Uuid,
    groups: Vec<Group>,
    presentations: BTreeMap<String, String>,
    actions: Option<(crate::automation_control::Control, u64, Vec<String>)>,
}
impl Source {
    pub async fn new(
        admission: Admission,
        controller: Uuid,
        groups: Vec<Group>,
    ) -> Result<Self, String> {
        let generation = admission
            .task_generation()
            .await
            .map_err(|e| format!("{e:?}"))?;
        Ok(Self {
            admission,
            generation,
            controller,
            groups,
            presentations: BTreeMap::new(),
            actions: None,
        })
    }
    pub fn with_actions(mut self, control: crate::automation_control::Control, revision: u64, scenes: Vec<String>) -> Self {
        if !scenes.is_empty() { self.actions = Some((control, revision, scenes)); }
        self
    }
    /// Only the accepted document's reviewed presentation mappings are passed
    /// here. This aliases readback, never execution or authorization.
    pub fn with_presentations(mut self, presentations: BTreeMap<String, String>) -> Self {
        self.presentations = presentations;
        self
    }
    pub async fn sample(&self) -> Result<BTreeMap<String, Option<bool>>, UnavailableReason> {
        self.sample_inner(true).await
    }
    /// Endpoint construction precedes the control owner's event loop. Never
    /// wait on that owner here; leave load-backed actions unknown until the
    /// ordinary projection task can sample them after startup.
    pub async fn initial_sample(&self) -> Result<BTreeMap<String, Option<bool>>, UnavailableReason> {
        self.sample_inner(false).await
    }
    async fn sample_inner(&self, read_actions: bool) -> Result<BTreeMap<String, Option<bool>>, UnavailableReason> {
        let names = self.groups.iter().map(|g| g.variable.clone()).collect();
        tokio::time::timeout(Duration::from_millis(500), async {
            let snapshot = self
                .admission
                .sampled_scene_selection_snapshot(self.controller,self.generation.clone(),names)
                .await
                .map_err(|_| UnavailableReason::ReadFailed)?;
            // Selection and readiness share one owner turn, including both live
            // authorization fences. Do not repeat three complete owner audits.
            if !snapshot.runtime_ready {
                return Err(UnavailableReason::RuntimeNotReady);
            }
            let stored=snapshot.values.into_iter().map(|(name,value)|(name,value.stored)).collect();
            let mut values = crate::scene_switches::states_in_groups(&self.groups, &stored)
                .map_err(|_| UnavailableReason::ReadFailed)?;
            for (public, provider) in &self.presentations {
                let selected = values.get(provider).copied()
                    .ok_or(UnavailableReason::ReadFailed)?;
                values.insert(public.clone(), selected);
            }
            if let Some((control, revision, scenes)) = &self.actions {
                if !read_actions {
                    values.extend(scenes.iter().map(|id| (id.clone(), None)));
                    return Ok(values);
                }
                use crate::automation_control::{Action, Reply};
                let Reply::SceneLoadStates(entries) = control.request(Action::SceneLoadStates {
                    controller: self.controller, revision: *revision, scenes: scenes.clone(),
                }).await.map_err(|_| UnavailableReason::ReadFailed)? else {
                    return Err(UnavailableReason::ReadFailed);
                };
                for id in scenes {
                    let entry = entries.get(id).ok_or(UnavailableReason::ReadFailed)?;
                    values.insert(id.clone(), *entry);
                }
            }
            Ok(values)
        })
        .await
        .map_err(|_| UnavailableReason::ReadTimedOut)?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn selection_sample_does_not_reenter_the_fifo_behind_work_arriving_during_its_read() {
        use std::sync::{Mutex,Condvar,atomic::{AtomicBool,Ordering}};
        use homesense_core::ExecutionContext;
        let directory=std::env::temp_dir().join(format!("selection-fifo-{}",Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();let path=directory.join("runtime.sqlite");
        homesense_storage::Store::open(&path).unwrap().publish_configuration(None,
            include_bytes!("../../../config/simulation.json"),ExecutionContext::Simulation,&[]).unwrap();
        let controller=Uuid::new_v4();let busy_reader=Uuid::new_v4();
        let armed=Arc::new(AtomicBool::new(false));
        let owner=Arc::new(Mutex::new(None::<(Admission,TaskGeneration)>));
        let queued=Arc::new(Mutex::new(None::<crate::admission::SelectionTestRead>));
        let gate=Arc::new((Mutex::new(false),Condvar::new()));
        let (start,worker,reply,hold)=(armed.clone(),owner.clone(),queued.clone(),gate.clone());
        let admission=Admission::open(path,crate::admission::Options {context:ExecutionContext::Simulation,..Default::default()},move|id|{
            if id==controller && start.swap(false,Ordering::AcqRel) {
                // Put real actor work after the in-progress read, before its
                // reply reaches the caller. A second FIFO wait cannot pass it.
                let current=worker.lock().unwrap();let (actor,generation)=current.as_ref().unwrap();
                *reply.lock().unwrap()=Some(actor.enqueue_selection_test_read(busy_reader,generation.clone()));
            }
            if id==busy_reader {
                let (lock,wake)=&*hold;let mut released=lock.lock().unwrap();
                while !*released {released=wake.wait(released).unwrap();}
            }
            id==controller || id==busy_reader
        }).await.unwrap();
        admission.synchronized(1,1).await.unwrap();let generation=admission.task_generation().await.unwrap();
        *owner.lock().unwrap()=Some((admission.clone(),generation));
        let source=Source::new(admission.clone(),controller,vec![]).await.unwrap();
        armed.store(true,Ordering::Release);
        let outcome=source.sample().await; // Keep the production 500ms limit.
        {let (lock,wake)=&*gate;*lock.lock().unwrap()=true;wake.notify_all();}
        let pending=queued.lock().unwrap().take().expect("intervening read queued");
        assert!(pending.await.unwrap().is_ok());
        *owner.lock().unwrap()=None;
        admission.shutdown().await.unwrap();std::fs::remove_dir_all(directory).unwrap();
        assert_eq!(outcome,Ok(BTreeMap::new()),"a coherent read must complete before later FIFO work; extending its timeout cannot solve the old reentry");
    }
    #[tokio::test]
    async fn selection_sample_keeps_two_authority_fences_without_three_owner_round_trips() {
        use std::sync::atomic::{AtomicBool,AtomicUsize,Ordering};
        use homesense_core::ExecutionContext;
        let directory=std::env::temp_dir().join(format!("selection-coherent-{}",Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let path=directory.join("runtime.sqlite");
        homesense_storage::Store::open(&path).unwrap().publish_configuration(
            None,include_bytes!("../../../config/simulation.json"),ExecutionContext::Simulation,&[],
        ).unwrap();
        let controller=Uuid::new_v4();
        let checks=Arc::new(AtomicUsize::new(0));let revoke=Arc::new(AtomicBool::new(false));let allowed=Arc::new(AtomicBool::new(true));
        let (count,retire,permission)=(checks.clone(),revoke.clone(),allowed.clone());
        let admission=Admission::open(path.clone(),crate::admission::Options {context:ExecutionContext::Simulation,..Default::default()},move|id|{
            if id!=controller{return false;}
            count.fetch_add(1,Ordering::AcqRel);
            if retire.load(Ordering::Acquire){permission.swap(false,Ordering::AcqRel)}else{permission.load(Ordering::Acquire)}
        }).await.unwrap();
        admission.synchronized(1,1).await.unwrap();
        let source=Source::new(admission.clone(),controller,vec![]).await.unwrap();
        checks.store(0,Ordering::Release);
        assert_eq!(source.sample().await.unwrap(),BTreeMap::new());
        let live_checks=checks.load(Ordering::Acquire);
        // A revoke between the initial and final fences must discard the read.
        checks.store(0,Ordering::Release);revoke.store(true,Ordering::Release);
        assert_eq!(source.sample().await,Err(UnavailableReason::ReadFailed));
        let revoked_checks=checks.load(Ordering::Acquire);
        admission.clone().shutdown().await.unwrap();
        assert!(homesense_storage::Store::open(path).unwrap().command_status(admission.boot_id(),1).unwrap().is_none());
        std::fs::remove_dir_all(directory).unwrap();
        assert_eq!(live_checks,2,"one owner read needs live authorization before and after projection, not three independent reads");
        assert_eq!(revoked_checks,2,"the final authorization fence must not be optimized away");
    }
    #[test]
    fn selection_cache_does_not_invent_false_for_unknown_expired_or_invalidated_state() {
        let cache = Cache::default();
        assert_eq!(cache.read("a"), SelectionState::Unavailable(UnavailableReason::NotSampled));
        cache
            .install(
                [
                    ("a".into(), Some(true)),
                    ("b".into(), Some(false)),
                    ("c".into(), None),
                ]
                .into(),
            )
            .unwrap();
        assert_eq!(cache.read("a"), SelectionState::Known(true));
        assert_eq!(cache.read("b"), SelectionState::Known(false));
        assert_eq!(cache.read("c"), SelectionState::Unknown);
        assert_eq!(cache.read("missing"), SelectionState::Unavailable(UnavailableReason::ReadFailed));
        cache.0.write().unwrap().snapshot.as_mut().unwrap().sampled = Instant::now() - MAX_AGE;
        assert_eq!(cache.read("a"), SelectionState::Unavailable(UnavailableReason::CacheExpired));
        assert_eq!(cache.read("b"), SelectionState::Unavailable(UnavailableReason::CacheExpired));
        assert_eq!(cache.read("c"), SelectionState::Unavailable(UnavailableReason::CacheExpired));
        cache.install([("a".into(), Some(true))].into()).unwrap();
        cache.invalidate();
        assert_eq!(cache.read("a"), SelectionState::Unavailable(UnavailableReason::Invalidated));
        for reason in [UnavailableReason::ReadTimedOut, UnavailableReason::RuntimeNotReady, UnavailableReason::ReadFailed] {
            cache.unavailable(reason);
            assert_eq!(cache.read("a"), SelectionState::Unavailable(reason));
            assert_eq!(cache.read("a").selected(), None);
        }
        cache.install([("a".into(), None)].into()).unwrap();
        assert_eq!(cache.read("a"), SelectionState::Unknown);
    }
    #[tokio::test]
    async fn selection_source_reports_not_ready_without_establishing_a_selection() {
        use homesense_core::ExecutionContext;
        let directory = std::env::temp_dir().join(format!("selection-source-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("runtime.sqlite");
        homesense_storage::Store::open(&path).unwrap().publish_configuration(
            None, include_bytes!("../../../config/simulation.json"), ExecutionContext::Simulation, &[],
        ).unwrap();
        let controller = Uuid::new_v4();
        let admission = Admission::open(path, crate::admission::Options {
            context: ExecutionContext::Simulation, ..Default::default()
        }, move |id| id == controller).await.unwrap();
        admission.synchronized(1, 1).await.unwrap();
        let source = Source::new(admission.clone(), controller, vec![]).await.unwrap();
        assert_eq!(source.sample().await.unwrap(), BTreeMap::new());
        admission.transport_failed().await.unwrap();
        assert_eq!(source.sample().await, Err(UnavailableReason::RuntimeNotReady));
        admission.shutdown().await.unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }
}
