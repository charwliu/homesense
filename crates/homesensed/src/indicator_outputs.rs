//! Private software output fence, never a hardware LED observation.
use homesense_core::{Command, DeviceId, queue::Epochs};
use std::collections::BTreeMap;
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Written {
    pub sequence: u64,
    pub command: u64,
    pub color: Option<bool>,
    epochs: Epochs,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Requested {
    pub command: u64,
    pub status: String,
    pub color: Option<bool>,
    epochs: Epochs,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Output {
    pub written: Option<Written>,
    pub requested: Option<Requested>,
}
pub(crate) type Fence = BTreeMap<DeviceId, Output>;
pub(crate) struct Capture {
    pub fence: Fence,
    pub prior_complete: bool,
}
#[derive(Default)]
pub(crate) struct Ledger {
    sequence: u64,
    outputs: BTreeMap<DeviceId, Written>,
}
impl Ledger {
    pub(crate) fn sequence(&self)->u64 {self.sequence}
    pub(crate) fn capture(
        &self,
        store: &homesense_storage::Store,
        boot: &str,
        revision: u64,
        ids: &[DeviceId],
        epochs: Epochs,
    ) -> Result<Fence, homesense_storage::Error> {
        ids.iter()
            .map(|id| {
                let requested = store
                    .latest_led_request(boot, revision, id, epochs)?
                    .map(|r| Requested {
                        command: r.command,
                        status: r.status,
                        color: match r.intent.as_slice() {
                            [1, 6, 0, 0] => Some(false),
                            [1, 6, 1, 0] => Some(true),
                            _ => None,
                        },
                        epochs,
                    });
                Ok((
                    id.clone(),
                    Output {
                        written: self.outputs.get(id).filter(|w| w.epochs == epochs).cloned(),
                        requested,
                    },
                ))
            })
            .collect()
    }
    pub(crate) fn color(command: Command) -> Option<Option<bool>> {
        match command {
            Command::PanelLed(on) => Some(Some(on)),
            Command::PanelLedOff => Some(None),
            _ => None,
        }
    }
    pub(crate) fn written(
        &mut self,
        id: DeviceId,
        command: u64,
        color: Option<bool>,
        epochs: Epochs,
    ) -> Result<(), String> {
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or("LED written fence exhausted")?;
        self.outputs.retain(|_, record| record.epochs == epochs);
        self.outputs.insert(
            id,
            Written {
                sequence: self.sequence,
                command,
                color,
                epochs,
            },
        );
        Ok(())
    }
}
