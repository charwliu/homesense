use futures::future::{join_all, BoxFuture};
use log::debug;
use serde_json::Value;
use std::fmt::Debug;
use uuid::Uuid;

#[derive(Debug)]
pub enum Event {
    ControllerPaired { id: Uuid },
    ControllerUnpaired { id: Uuid },
    CharacteristicValueChanged { aid: u64, iid: u64, value: Value },
}

#[derive(Default)]
pub struct EventEmitter {
    listeners: Vec<(u64, Box<dyn (Fn(&Event) -> BoxFuture<()>) + Send + Sync>)>,
    next_id: u64,
}

impl EventEmitter {
    pub fn new() -> EventEmitter {
        EventEmitter::default()
    }

    pub fn add_listener(
        &mut self,
        listener: Box<dyn (Fn(&Event) -> BoxFuture<()>) + Send + Sync>,
    ) -> u64 {
        let id = self.next_id;
        self.next_id = self
            .next_id
            .checked_add(1)
            .expect("HAP listener ID exhausted");
        self.listeners.push((id, listener));
        id
    }
    pub fn remove_listener(&mut self, id: u64) {
        self.listeners.retain(|(key, _)| *key != id);
    }

    pub async fn emit(&self, event: &Event) {
        debug!("emitting event: {:?}", event);

        join_all(self.listeners.iter().map(|(_, listener)| listener(&event))).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    #[tokio::test]
    async fn removing_session_listener_releases_capture_and_preserves_other_listeners() {
        let mut emitter = EventEmitter::new();
        let counter = Arc::new(AtomicUsize::new(0));
        let keep = counter.clone();
        let permanent = emitter.add_listener(Box::new(move |_| {
            keep.fetch_add(1, Ordering::SeqCst);
            Box::pin(async {})
        }));
        for _ in 0..100 {
            let marker = Arc::new(());
            let weak = Arc::downgrade(&marker);
            let id = emitter.add_listener(Box::new(move |_| {
                let _ = &marker;
                Box::pin(async {})
            }));
            assert!(weak.upgrade().is_some());
            emitter.remove_listener(id);
            assert!(weak.upgrade().is_none());
            assert_eq!(emitter.listeners.len(), 1);
        }
        emitter
            .emit(&Event::ControllerUnpaired { id: Uuid::new_v4() })
            .await;
        assert_eq!(counter.load(Ordering::SeqCst), 1);
        emitter.remove_listener(permanent);
        assert!(emitter.listeners.is_empty());
    }
}
