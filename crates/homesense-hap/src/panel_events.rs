//! Single-owner panel notification delivery. Cancellation is terminal so an
//! ambiguous partial batch cannot be retried as new button presses.
use crate::{
    panel_graph::PanelKeyBinding,
    panel_input::{Generation, InputError, PanelInput},
};
use hap::{HapType, characteristic::Perm, server::AccessoryHandle};
use homesense_core::gesture::Settings;
use homesense_state::{Cursor, Event};
use std::collections::BTreeMap;

#[derive(Debug, PartialEq, Eq)]
pub enum PublishError {
    Input(InputError),
    InvalidBinding,
    DeliveryFailed,
    Stopped,
}
pub struct PanelEvents {
    input: PanelInput,
    targets: BTreeMap<(u64, u64), AccessoryHandle>,
    usable: bool,
}
impl PanelEvents {
    pub async fn new(
        bindings: Vec<(PanelKeyBinding, Settings, AccessoryHandle)>,
        generation: Generation,
        cursor: Cursor,
        now: u64,
        max_age: u64,
    ) -> Result<Self, PublishError> {
        let input = PanelInput::new(
            bindings.iter().map(|(b, s, _)| (b.clone(), *s)).collect(),
            generation,
            cursor,
            now,
            max_age,
        )
        .map_err(PublishError::Input)?;
        let mut targets = BTreeMap::new();
        for (binding, _, handle) in bindings {
            {
                let accessory = handle.lock().await;
                if accessory.get_id() != binding.aid {
                    return Err(PublishError::InvalidBinding);
                }
                let characteristic = accessory
                    .get_service(HapType::StatelessProgrammableSwitch)
                    .and_then(|s| s.get_characteristic(HapType::ProgrammableSwitchEvent))
                    .ok_or(PublishError::InvalidBinding)?;
                if characteristic.get_id() != binding.event_iid
                    || !characteristic.get_perms().contains(&Perm::Events)
                    || characteristic.get_perms().contains(&Perm::PairedWrite)
                {
                    return Err(PublishError::InvalidBinding);
                }
            }
            targets.insert((binding.aid, binding.event_iid), handle);
        }
        Ok(Self {
            input,
            targets,
            usable: true,
        })
    }
    pub fn cursor(&self) -> Cursor {
        self.input.cursor()
    }
    pub fn stop(&mut self) {
        self.usable = false;
        self.input.stop();
    }
    /// Success means handed to the HAP emitter, not acknowledged by a controller.
    /// Any error or cancelled future requires a fresh live-cursor subscription.
    pub async fn publish(
        &mut self,
        generation: Generation,
        events: &[Event],
        now: u64,
    ) -> Result<usize, PublishError> {
        if !self.usable {
            return Err(PublishError::Stopped);
        }
        // Set before the first await; dropping this future cannot permit replay.
        self.usable = false;
        let notifications = self
            .input
            .consume(generation, events, now)
            .map_err(PublishError::Input)?;
        for notification in &notifications {
            let handle = self
                .targets
                .get(&(notification.aid, notification.iid))
                .ok_or(PublishError::InvalidBinding)?;
            let mut accessory = handle.lock().await;
            if accessory.get_id() != notification.aid {
                return Err(PublishError::InvalidBinding);
            }
            let characteristic = accessory
                .get_mut_service(HapType::StatelessProgrammableSwitch)
                .and_then(|s| s.get_mut_characteristic(HapType::ProgrammableSwitchEvent))
                .ok_or(PublishError::InvalidBinding)?;
            if characteristic.get_id() != notification.iid
                || !characteristic.get_perms().contains(&Perm::Events)
                || characteristic.get_perms().contains(&Perm::PairedWrite)
            {
                return Err(PublishError::InvalidBinding);
            }
            characteristic
                .set_observed_value(serde_json::json!(notification.value))
                .await
                .map_err(|_| PublishError::DeliveryFailed)?;
        }
        self.usable = true;
        Ok(notifications.len())
    }
}
