//! Recoverable state updates, coalesced to current observed state. Never replay
//! device commands, button gestures or historical intermediate dimmer values.
use crate::{
    admission::{Admission, TaskGeneration},
    feedback::Feedback,
    management_devices::DeviceSnapshot,
};
use homesense_core::DeviceId;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use uuid::Uuid;
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cursor {
    pub boot: String,
    pub revision: u64,
    pub transport_epoch: u64,
    pub sequence: u64,
    pub scope: String,
}
#[derive(Debug, PartialEq, Eq)]
pub struct Update {
    pub cursor: Cursor,
    pub reset: bool,
    pub snapshot: DeviceSnapshot,
}
pub(crate) fn scoped_cursor(controller:Uuid,authorization_scope:&str,ids:&[DeviceId],snapshot:&DeviceSnapshot,transport_epoch:u64)->Cursor {
    let ids=ids.iter().map(DeviceId::as_str).collect::<BTreeSet<_>>();
    let scope=serde_json::to_vec(&(controller.to_string(),authorization_scope,ids)).expect("string cursor scope");
    Cursor{boot:snapshot.boot.clone(),revision:snapshot.revision,transport_epoch,sequence:snapshot.sequence,
        scope:Sha256::digest(scope).iter().map(|byte|format!("{byte:02x}")).collect()}
}
#[allow(clippy::too_many_arguments)]
pub async fn read(
    admission: &Admission,
    controller: Uuid,
    generation: TaskGeneration,
    ids: Vec<DeviceId>,
    feedback: &Feedback,
    previous: Option<Cursor>,
) -> Result<Update, String> {
    read_bound(
        admission, controller, generation, ids, feedback, previous, "", false,
    )
    .await
}
pub(crate) async fn read_scoped(
    admission: &Admission,
    controller: Uuid,
    generation: TaskGeneration,
    ids: Vec<DeviceId>,
    feedback: &Feedback,
    previous: Option<Cursor>,
    authorization_scope: &str,
) -> Result<Update, String> {
    // This internal entry is the WSS owner's bounded software projection view.
    // Ordinary public event reads retain their empty-idle delta contract.
    read_bound(admission,controller,generation,ids,feedback,previous,authorization_scope,true).await
}
#[allow(clippy::too_many_arguments)]
async fn read_bound(
    admission: &Admission,
    controller: Uuid,
    generation: TaskGeneration,
    ids: Vec<DeviceId>,
    feedback: &Feedback,
    previous: Option<Cursor>,
    authorization_scope: &str,
    include_projection: bool,
) -> Result<Update, String> {
    if ids.len() > crate::web_auth::MAX_DEVICE_SCOPE {
        return Err("event device scope too large".into());
    }
    let mut ids = ids;
    ids.sort();
    let scope = serde_json::to_vec(&(
        controller.to_string(),
        authorization_scope,
        ids.iter().map(DeviceId::as_str).collect::<Vec<_>>(),
    ))
    .map_err(|e| e.to_string())?;
    let scope = Sha256::digest(scope)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let bundle = admission
        .automation_bundle(controller, generation.clone())
        .await
        .map_err(|e| format!("{e:?}"))?;
    let mut snapshot =
        crate::management_devices::snapshot(admission, controller, generation, ids, feedback)
            .await?;
    let cursor = Cursor {
        boot: snapshot.boot.clone(),
        revision: snapshot.revision,
        transport_epoch: feedback.transport_epoch(),
        sequence: snapshot.sequence,
        scope,
    };
    let events = previous
        .filter(|old| {
            old.boot == cursor.boot
                && old.revision == cursor.revision
                && old.transport_epoch == cursor.transport_epoch
                && old.scope == cursor.scope
        })
        .and_then(|old| {
            feedback
                .events_since(homesense_state::Cursor {
                    boot_id: Uuid::parse_str(&old.boot).ok()?.as_u128(),
                    sequence: old.sequence,
                })
                .ok()
        });
    let reset = events.is_none();
    if let Some(events) = events {
        let physical: BTreeSet<_> = events.iter().map(|e| e.device_id.clone()).collect();
        // Relay observations affect their logical combined lights too. Project
        // the latest coherent physical snapshot instead of inventing old pairs.
        snapshot.devices.retain(|view| {
            // Local gateway health has its own epoch, without advancing the
            // service cursor. Include the bounded local views even when no
            // queryable state exists (keypads and uncalibrated motors).
            // Dimmer overlay settlement can change without a physical event.
            // Retain its bounded view so WSS can compare stable projection data.
            view.communication_error.is_some() || view.availability.is_some() || view.motor_estimate.is_some()
                || (include_projection && view.state.kind == homesense_core::ModuleKind::Dimmer)
                || view.recorded || physical.contains(&view.id)
                || bundle
                    .configuration
                    .logical_components(&view.id)
                    .iter().any(|component| physical.contains(*component))
        });
    }
    Ok(Update {
        cursor,
        reset,
        snapshot,
    })
}
