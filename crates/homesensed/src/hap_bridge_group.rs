//! A generation-scoped owner of independently paired HAP listeners.
use crate::{admission::Admission, automation_runtime::AutomationRuntime, hap_endpoint::Endpoint};
use homesense_state::Snapshot;
use std::{collections::BTreeMap, path::Path};
use tokio::{sync::watch, task::JoinHandle};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Listening,
    Degraded,
}

pub struct Group {
    pub projection: watch::Sender<Snapshot>,
    pub statuses: watch::Receiver<BTreeMap<String, Status>>,
    pub panel_feeds: Vec<crate::runtime_panel_hap::Feed>,
    pub(crate) placement: Option<crate::hap_placement::Publication>,
    stop: watch::Sender<bool>,
    task: Option<JoinHandle<()>>,
}
impl Drop for Group {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}
impl Group {
    pub async fn start(
        plan: &[u8],
        base: &Path,
        admission: &Admission,
        controller: Uuid,
        runtime: &AutomationRuntime,
        initial: Snapshot,
    ) -> Result<Self, String> {
        let generation = admission
            .task_generation()
            .await
            .map_err(|e| format!("{e:?}"))?;
        let bundle = admission
            .automation_bundle(controller, generation)
            .await
            .map_err(|e| format!("{e:?}"))?;
        let document = bundle.automation.as_ref().ok_or("automation missing")?;
        let prepared =
            crate::hap_bridges::prepare(plan, &bundle.configuration, document, base).await?;
        let mut endpoints = BTreeMap::new();
        let mut statuses = BTreeMap::new();
        let mut placements=vec![];
        for bridge in prepared {
            match Endpoint::start_selected_fans_panels(
                &bridge.settings,
                &bridge.identity,
                admission,
                controller,
                runtime,
                initial.clone(),
                true,
                Some(bridge.lights),
                &bridge.panels,
                bridge.fans,
            )
            .await
            {
                Ok(Some(endpoint)) => {
                    placements.push(crate::hap_placement::Bridge{id:bridge.id.clone(),name:bridge.name.clone(),available:true,accessories:endpoint.placement.clone()});
                    statuses.insert(bridge.id.clone(), Status::Listening);
                    endpoints.insert(bridge.id, endpoint);
                }
                Ok(None) => {
                    placements.push(crate::hap_placement::Bridge{id:bridge.id.clone(),name:bridge.name.clone(),available:false,accessories:vec![]});
                    statuses.insert(bridge.id, Status::Degraded);
                }
                Err(error) => {
                    for endpoint in endpoints.into_values() {
                        endpoint.shutdown().await;
                    }
                    return Err(error);
                }
            }
        }
        let mut group=Self::supervise(endpoints, statuses, initial);
        group.placement=Some(crate::hap_placement::Publication::new(admission.boot_id().into(),bundle.configuration.revision(),placements));
        Ok(group)
    }
    fn supervise(
        mut endpoints: BTreeMap<String, Endpoint>,
        statuses: BTreeMap<String, Status>,
        initial: Snapshot,
    ) -> Self {
        let panel_feeds=endpoints.values().filter_map(|endpoint|endpoint.panel_feed.clone()).collect();
        let (projection, mut snapshots) = watch::channel(initial);
        let (status_tx, statuses) = watch::channel(statuses);
        let (stop, mut stopped) = watch::channel(false);
        let task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = stopped.changed() => break,
                    changed = snapshots.changed() => {
                        if changed.is_err() { break; }
                        let snapshot = snapshots.borrow_and_update().clone();
                        for endpoint in endpoints.values() { endpoint.projection.send_replace(snapshot.clone()); }
                    }
                    id = async {
                        let waits = endpoints.iter_mut().map(|(id, endpoint)| Box::pin(async move {
                            let result = endpoint.finished().await;
                            if let Err(error) = result { log::error!("HAP bridge {id}: {error}"); }
                            id.clone()
                        })).collect::<Vec<_>>();
                        futures_util::future::select_all(waits).await.0
                    }, if !endpoints.is_empty() => {
                        if let Some(endpoint) = endpoints.remove(&id) { endpoint.shutdown().await; }
                        status_tx.send_modify(|statuses| { statuses.insert(id, Status::Degraded); });
                    }
                }
            }
            for endpoint in endpoints.into_values() {
                endpoint.shutdown().await;
            }
        });
        Self {
            projection,
            statuses,
            panel_feeds,
            placement:None,
            stop,
            task: Some(task),
        }
    }
    pub async fn shutdown(mut self) {
        self.stop.send_replace(true);
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn failed_bridge_isolated_snapshots_continue_and_shutdown_joins_survivor() {
        let initial = Snapshot {
            cursor: homesense_state::Cursor {
                boot_id: 1,
                sequence: 0,
            },
            state_version: 0,
            devices: Default::default(),
        };
        let first = Endpoint::synthetic(initial.clone());
        let second = Endpoint::synthetic(initial.clone());
        let failed = first.abort_handle();
        let survivor = second.abort_handle();
        let mut feedback = second.projection.subscribe();
        let mut group = Group::supervise(
            BTreeMap::from([("a".into(), first), ("b".into(), second)]),
            BTreeMap::from([
                ("a".into(), Status::Listening),
                ("b".into(), Status::Listening),
            ]),
            initial.clone(),
        );
        failed.abort();
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            group.statuses.wait_for(|s| s["a"] == Status::Degraded),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(group.statuses.borrow()["b"], Status::Listening);
        assert!(!survivor.is_finished());
        let mut next = initial;
        next.cursor.sequence = 1;
        next.state_version = 1;
        group.projection.send_replace(next);
        tokio::time::timeout(std::time::Duration::from_secs(2), feedback.changed())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(feedback.borrow().state_version, 1);
        group.shutdown().await;
        assert!(survivor.is_finished());
    }
    #[tokio::test]
    async fn dropping_group_aborts_owned_listeners() {
        let initial = Snapshot {
            cursor: homesense_state::Cursor {
                boot_id: 1,
                sequence: 0,
            },
            state_version: 0,
            devices: Default::default(),
        };
        let endpoint = Endpoint::synthetic(initial.clone());
        let handle = endpoint.abort_handle();
        let group = Group::supervise(
            BTreeMap::from([("a".into(), endpoint)]),
            BTreeMap::from([("a".into(), Status::Listening)]),
            initial,
        );
        drop(group);
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while !handle.is_finished() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
}
