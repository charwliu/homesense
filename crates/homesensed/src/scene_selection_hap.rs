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
            let readiness = self.admission.diagnostics(self.controller, self.generation.clone())
                .await.map_err(|_| UnavailableReason::ReadFailed)?;
            if !readiness.ready() {
                return Err(UnavailableReason::RuntimeNotReady);
            }
            let snapshot = self
                .admission
                .scene_selection_values(self.controller, self.generation.clone(), names)
                .await
                .map_err(|_| UnavailableReason::ReadFailed)?;
            let state = self
                .admission
                .diagnostics(self.controller, self.generation.clone())
                .await
                .map_err(|_| UnavailableReason::ReadFailed)?;
            if !state.ready() {
                return Err(UnavailableReason::RuntimeNotReady);
            }
            let mut values = crate::scene_switches::states_in_groups(&self.groups, &snapshot)
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
