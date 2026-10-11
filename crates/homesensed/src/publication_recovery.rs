//! Rebuild a committed automation mapping on fresh, caller-established streams.
//! No endpoint connection or EA-5 configuration changes happen here.
use crate::{
    admission::Admission, automation_runtime::AutomationRuntime, feedback::Feedback,
    observed_transport::ObservedTransport, transport_owner::TransportOwner,
};
use std::io::{Read, Write};
use uuid::Uuid;
struct Guard(Admission, bool);
impl Drop for Guard {
    fn drop(&mut self) {
        if !self.1 {
            self.0.stop_now();
        }
    }
}
#[derive(Clone, Copy)]
pub struct Settings {
    pub query_timeout_ms: u64,
    pub max_age_ms: u64,
    pub wall_unix_ms: i64,
    pub calendar_lateness_ms: u32,
    pub scene_capacity: usize,
}
/// The trusted coordinator must finish the old loop and reserved STOP drain,
/// then commit before calling. Fresh query replies are required for every mapped
/// device; event-only devices need a separate, explicit synchronization policy.
pub async fn rebuild<S: Read + Write + Send + 'static>(
    admission: Admission,
    controller: Uuid,
    transport: TransportOwner<S>,
    epoch: u64,
    settings: Settings,
) -> Result<(AutomationRuntime, ObservedTransport<S>), String> {
    rebuild_with_policy(
        admission,
        controller,
        transport,
        epoch,
        settings,
        Synchronization::CompleteFeedback,
    )
    .await
}
pub use crate::observed_transport::Synchronization;
/// The event policy leaves verified keypads unknown until fresh events arrive.
/// All other validation and publication barriers match the strict entry point.
pub async fn rebuild_with_policy<S: Read + Write + Send + 'static>(
    admission: Admission,
    controller: Uuid,
    transport: TransportOwner<S>,
    epoch: u64,
    settings: Settings,
    policy: Synchronization,
) -> Result<(AutomationRuntime, ObservedTransport<S>), String> {
    if settings.query_timeout_ms == 0
        || settings.query_timeout_ms > 5000
        || settings.max_age_ms == 0
    {
        return Err("invalid publication recovery bounds".into());
    }
    let (bundle, context, prior_epoch) = admission
        .publication_bundle(controller)
        .await
        .map_err(|e| format!("{e:?}"))?;
    if !transport.belongs_to(&admission)
        || epoch <= prior_epoch
        || !transport.matches_generation(
            &bundle
                .configuration
                .gateways()
                .keys()
                .cloned()
                .collect::<Vec<_>>(),
            epoch,
        )
    {
        return Err("fresh complete publication transport required".into());
    }
    let document = bundle
        .automation
        .as_ref()
        .ok_or("published automation required")?;
    homesense_core::calendar::Calendar::new(
        document.schedules.clone(),
        settings.wall_unix_ms,
        admission.monotonic_ms(),
        None,
        settings.calendar_lateness_ms,
    )
    .map_err(|e| format!("{e:?}"))?;
    if settings.scene_capacity == 0 || settings.scene_capacity > 64 {
        return Err("invalid scene capacity".into());
    }
    let mut guard = Guard(admission.clone(), false);
    admission
        .transport_failed()
        .await
        .map_err(|e| format!("publication recovery barrier: {e:?}"))?;
    let feedback = Feedback::new(
        &bundle.configuration,
        context,
        Uuid::parse_str(admission.boot_id())
            .map_err(|e| e.to_string())?
            .as_u128(),
        epoch,
    )?;
    let mut owner = ObservedTransport::new_recovering(transport, feedback, epoch)?;
    match policy {
        Synchronization::CompleteFeedback => {
            owner
                .query_recovery(
                    &bundle.configuration,
                    context,
                    settings.query_timeout_ms,
                    settings.max_age_ms,
                )
                .await?
        }
        Synchronization::OutputsAndCalibratedMotors => {
            owner.query_recovery_with_policy(&bundle.configuration,context,settings.query_timeout_ms,settings.max_age_ms,policy).await?
        }
        Synchronization::OutputsAndKeypadEvents => {
            owner
                .query_automation_recovery(
                    &bundle.configuration,
                    context,
                    settings.query_timeout_ms,
                    settings.max_age_ms,
                )
                .await?
        }
    }
    let runtime = AutomationRuntime::load(
        admission,
        controller,
        settings.wall_unix_ms,
        settings.calendar_lateness_ms,
        settings.scene_capacity,
        Default::default(),
    )
    .await?;
    guard.1 = true;
    Ok((runtime, owner))
}
