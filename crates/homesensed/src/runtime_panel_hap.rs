//! Opt-in panel publication. Every bridge owns a bounded loss-intolerant feed;
//! overload retires that bridge without blocking the physical automation owner.
use crate::admission::{Admission, TaskGeneration};
use hap::server::{AccessoryHandle, WriteBatchHandler, WriteStatus};
use homesense_core::DeviceId;
use homesense_hap::{
    panel_events::PanelEvents, panel_graph::PanelGraph, panel_input::Generation,
    storage::IdentityStore,
};
use homesense_state::{Cursor, Event};
use std::{collections::BTreeSet, sync::Arc};
use tokio::sync::{mpsc, watch};

#[derive(Clone)]
pub struct Feed {
    send: mpsc::Sender<Batch>,
    invalid: watch::Sender<bool>,
}
struct Batch {
    generation: Generation,
    events: Vec<Event>,
    now: u64,
}
impl Feed {
    pub fn invalidate(&self) {
        self.invalid.send_replace(true);
    }
    pub fn publish(&self, generation: Generation, events: &[Event], now: u64) {
        if *self.invalid.borrow() {
            return;
        }
        if events.len() > 4096 {
            self.invalidate();
            return;
        }
        if self
            .send
            .try_send(Batch {
                generation,
                events: events.to_vec(),
                now,
            })
            .is_err()
        {
            self.invalidate();
        }
    }
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    characteristics: Vec<Subscription>,
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Subscription {
    aid: u64,
    iid: u64,
    ev: bool,
    #[serde(rename = "remote")]
    _remote: Option<bool>,
}
struct Handler {
    addresses: BTreeSet<(u64, u64)>,
    identity: IdentityStore,
    admission: Admission,
    generation: TaskGeneration,
}
impl WriteBatchHandler for Handler {
    fn write(&self, _: uuid::Uuid, _: &[u8]) -> Result<(), WriteStatus> {
        Err(WriteStatus::ServiceCommunicationFailure)
    }
    fn write_async<'a>(
        &'a self,
        controller: uuid::Uuid,
        body: &'a [u8],
    ) -> hap::futures::future::BoxFuture<'a, Result<(), WriteStatus>> {
        Box::pin(async move {
            if !self
                .identity
                .controller_is_paired(controller)
                .unwrap_or(false)
            {
                return Err(WriteStatus::InsufficientPrivileges);
            }
            if self.admission.task_generation().await? != self.generation {
                return Err(WriteStatus::ServiceCommunicationFailure);
            }
            if body.len() > 65536 {
                return Err(WriteStatus::InvalidValueInRequest);
            }
            let request: Request =
                serde_json::from_slice(body).map_err(|_| WriteStatus::InvalidValueInRequest)?;
            if request.characteristics.is_empty() || request.characteristics.len() > 128 {
                return Err(WriteStatus::InvalidValueInRequest);
            }
            let mut seen = BTreeSet::new();
            for item in request.characteristics {
                let _enabled = item.ev;
                if !seen.insert((item.aid, item.iid)) {
                    return Err(WriteStatus::InvalidValueInRequest);
                }
                if !self.addresses.contains(&(item.aid, item.iid)) {
                    return Err(WriteStatus::ResourceDoesNotExist);
                }
            }
            Ok(())
        })
    }
}
pub struct RuntimePanelHap {
    pub handles: Vec<AccessoryHandle>,
    pub addresses: Vec<(u64, u64)>,
    pub handler: Arc<dyn WriteBatchHandler>,
    pub feed: Feed,
    events: PanelEvents,
    receive: mpsc::Receiver<Batch>,
    invalid: watch::Receiver<bool>,
    clock: Admission,
    selected: BTreeSet<DeviceId>,
}
impl RuntimePanelHap {
    pub async fn new(
        config: &homesense_config::Configuration,
        document: &homesense_config::rule_document::Document,
        store: &IdentityStore,
        admission: Admission,
        selected: &[DeviceId],
        cursor: Cursor,
    ) -> Result<Self, String> {
        crate::hap_bridge_plan::validate_panel_ownership(selected, document)?;
        let specs = crate::hap_bridge_plan::panel_specs(selected, config)?;
        let generation = admission
            .task_generation()
            .await
            .map_err(|e| format!("{e:?}"))?;
        if config.revision() != generation.epochs.configuration
            || cursor.boot_id
                != uuid::Uuid::parse_str(admission.boot_id())
                    .map_err(|e| e.to_string())?
                    .as_u128()
        {
            return Err("panel generation mismatch".into());
        }
        let graph = PanelGraph::build(store, specs)
            .await
            .map_err(|e| e.to_string())?;
        let handles: Vec<_> = graph
            .keys
            .into_values()
            .map(|a| {
                let handle: AccessoryHandle = Arc::new(hap::futures::lock::Mutex::new(Box::new(a)));
                handle
            })
            .collect();
        let addresses = graph
            .bindings
            .iter()
            .map(|b| (b.aid, b.event_iid))
            .collect::<Vec<_>>();
        let events = PanelEvents::new(
            graph
                .bindings
                .into_iter()
                .zip(handles.iter().cloned())
                .map(|(b, h)| {
                    let settings = document
                        .gestures
                        .get(&b.device)
                        .copied()
                        .unwrap_or_default();
                    (b, settings, h)
                })
                .collect(),
            Generation {
                configuration: generation.epochs.configuration,
                transport: generation.epochs.transport,
            },
            cursor,
            admission.monotonic_ms(),
            5000,
        )
        .await
        .map_err(|e| format!("{e:?}"))?;
        let (send, receive) = mpsc::channel(32);
        let (invalid_tx, invalid) = watch::channel(false);
        Ok(Self {
            clock: admission.clone(),
            selected: selected.iter().cloned().collect(),
            handles,
            addresses: addresses.clone(),
            handler: Arc::new(Handler {
                addresses: addresses.into_iter().collect(),
                identity: store.clone(),
                admission,
                generation,
            }),
            feed: Feed {
                send,
                invalid: invalid_tx,
            },
            events,
            receive,
            invalid,
        })
    }
    pub async fn run(mut self) -> Result<(), String> {
        loop {
            if *self.invalid.borrow() {
                return Err("panel event feed invalidated".into());
            }
            tokio::select! {
                biased;
                _ = self.invalid.changed() => return Err("panel event feed invalidated".into()),
                batch = self.receive.recv() => {
                    let batch = batch.ok_or("panel event feed ended")?;
                    let now=self.clock.monotonic_ms();
                    if now.checked_sub(batch.now).is_none_or(|age|age>=5000)
                        || batch.events.iter().any(|event|event.change==homesense_state::Change::Button
                            && self.selected.contains(&event.device_id)
                            && event.state.observed_at_ms.is_none_or(|at|now.checked_sub(at).is_none_or(|age|age>=5000))) {
                        return Err("panel event feed stale at delivery".into());
                    }
                    // Delivery may wait for a subscriber's accessory locks. Source
                    // overload still interrupts it and retires the endpoint.
                    tokio::select! {
                        biased;
                        _ = self.invalid.changed() => return Err("panel event feed invalidated".into()),
                        result = self.events.publish(batch.generation,&batch.events,batch.now) => { result.map_err(|e|format!("panel event delivery: {e:?}"))?; }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn saturated_feed_is_invalidated_without_blocking_or_disabling_another_bridge() {
        let (send, mut receive) = mpsc::channel(1);
        let (invalid, state) = watch::channel(false);
        let first = Feed { send, invalid };
        let (send, mut other) = mpsc::channel(1);
        let (invalid, state_other) = watch::channel(false);
        let second = Feed { send, invalid };
        let generation = Generation {
            configuration: 1,
            transport: 1,
        };
        first.publish(generation, &[], 1);
        first.publish(generation, &[], 2);
        assert!(*state.borrow());
        second.publish(generation, &[], 2);
        assert!(!*state_other.borrow());
        assert_eq!(other.try_recv().unwrap().now, 2);
        assert_eq!(receive.try_recv().unwrap().now, 1);
        first.publish(generation, &[], 3);
        assert!(receive.try_recv().is_err());
    }
}
