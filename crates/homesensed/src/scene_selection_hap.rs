//! Read-only selection projection. The actor is sampled directly, independently
//! of gateway polling and of HAP accessory locks. Unknown never becomes false.
use crate::admission::{Admission, TaskGeneration};
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
pub(crate) struct Cache(Arc<RwLock<Option<Snapshot>>>);
impl Cache {
    pub fn invalidate(&self) {
        if let Ok(mut snapshot) = self.0.write() {
            *snapshot = None;
        }
    }
    pub fn install(&self, values: BTreeMap<String, Option<bool>>) -> Result<(), String> {
        *self
            .0
            .write()
            .map_err(|_| "scene selection cache unavailable")? = Some(Snapshot {
            sampled: Instant::now(),
            values,
        });
        Ok(())
    }
    pub fn read(&self, scene: &str) -> Option<bool> {
        let snapshot = self.0.read().ok()?;
        let snapshot = snapshot.as_ref()?;
        if snapshot.sampled.elapsed() >= MAX_AGE {
            return None;
        }
        snapshot.values.get(scene).copied().flatten()
    }
}
#[derive(Clone)]
pub(crate) struct Source {
    admission: Admission,
    generation: TaskGeneration,
    controller: Uuid,
    groups: Vec<Group>,
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
        })
    }
    pub async fn sample(&self) -> Result<BTreeMap<String, Option<bool>>, String> {
        let names = self.groups.iter().map(|g| g.variable.clone()).collect();
        tokio::time::timeout(Duration::from_millis(500), async {
            let snapshot = self
                .admission
                .scene_selection_values(self.controller, self.generation.clone(), names)
                .await
                .map_err(|_| "scene selection read unavailable")?;
            let state = self
                .admission
                .diagnostics(self.controller, self.generation.clone())
                .await
                .map_err(|_| "scene selection readiness unavailable")?;
            if !state.ready() {
                return Err("scene selection runtime not ready".into());
            }
            crate::scene_switches::states_in_groups(&self.groups, &snapshot)
        })
        .await
        .map_err(|_| "scene selection read timed out")?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selection_cache_does_not_invent_false_for_unknown_expired_or_invalidated_state() {
        let cache = Cache::default();
        assert_eq!(cache.read("a"), None);
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
        assert_eq!(cache.read("a"), Some(true));
        assert_eq!(cache.read("b"), Some(false));
        assert_eq!(cache.read("c"), None);
        cache.0.write().unwrap().as_mut().unwrap().sampled = Instant::now() - MAX_AGE;
        assert_eq!(cache.read("a"), None);
        assert_eq!(cache.read("b"), None);
        cache.install([("a".into(), Some(true))].into()).unwrap();
        cache.invalidate();
        assert_eq!(cache.read("a"), None);
    }
}
