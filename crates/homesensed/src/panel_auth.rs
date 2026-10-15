//! Exact source credential capability for native panel commands. Controller
//! identity alone cannot preserve permission after a grant change or logout.
use crate::web_auth::ModuleOffLease;
use homesense_config::Configuration;
use homesense_core::{Command, DeviceId, PanelLedMode, ProtocolProfile};

#[derive(Clone, Debug)]
enum Scope {
    Led,
    Settings { keypad: DeviceId, module_id: String },
}
#[derive(Clone)]
pub struct Lease {
    source: ModuleOffLease,
    revision: u64,
    device: DeviceId,
    command: Command,
    scope: Scope,
}
impl std::fmt::Debug for Lease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PanelLease(<opaque>)")
    }
}
impl Lease {
    pub(crate) fn led(
        source: ModuleOffLease,
        config: &Configuration,
        device: DeviceId,
        command: Command,
    ) -> Option<Self> {
        let lease = Self {
            source,
            revision: config.revision(),
            device,
            command,
            scope: Scope::Led,
        };
        lease
            .valid_for(config, lease.source.controller(), &lease.device, command)
            .then_some(lease)
    }
    pub(crate) fn settings(
        source: ModuleOffLease,
        config: &Configuration,
        keypad: DeviceId,
        device: DeviceId,
        command: Command,
    ) -> Option<Self> {
        let module_id = config.keypads().get(&keypad)?.module_id.clone();
        let lease = Self {
            source,
            revision: config.revision(),
            device,
            command,
            scope: Scope::Settings { keypad, module_id },
        };
        lease
            .valid_for(config, lease.source.controller(), &lease.device, command)
            .then_some(lease)
    }
    pub(crate) fn valid(&self) -> bool {
        self.source.valid()
    }
    pub(crate) fn valid_for(
        &self,
        config: &Configuration,
        controller: uuid::Uuid,
        device: &DeviceId,
        command: Command,
    ) -> bool {
        if !self.valid()
            || controller != self.source.controller()
            || self.revision != config.revision()
            || device != &self.device
            || command != self.command
        {
            return false;
        }
        let Some((_, module)) = config.binding(device) else {
            return false;
        };
        if module.profile() != ProtocolProfile::Ts686Rs485SixKey {
            return false;
        }
        match &self.scope {
            Scope::Led => {
                matches!(command, Command::PanelLed(_) | Command::PanelLedOff)
                    && module.led_mode() == PanelLedMode::Manual
                    && self.source.devices().contains(device.as_str())
            }
            Scope::Settings { keypad, module_id } => {
                matches!(command, Command::PanelParameter(_))
                    && self.source.is_administrator()
                    && config.keypads().get(keypad).is_some_and(|panel| {
                        panel.enabled
                            && &panel.module_id == module_id
                            && panel.keys.len() == 6
                            && panel
                                .keys
                                .get(&1)
                                .is_some_and(|k| k.led_endpoint.id == device.as_str())
                            && panel.keys.values().all(|k| {
                                k.led_endpoint.enabled
                                    && self.source.devices().contains(&k.led_endpoint.id)
                            })
                    })
            }
        }
    }
    pub(crate) fn with_valid<T>(&self, operation: impl FnOnce() -> T) -> Option<T> {
        self.source.with_valid(operation)
    }
}
