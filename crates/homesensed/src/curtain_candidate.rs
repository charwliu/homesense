//! Explicit calibrated candidate handle to the one Admission-owned trajectory.
//! HAP, Web and other candidates share it; this handle only caches diagnostics.
use crate::admission::{Admission, TaskGeneration};
use hap::server::WriteStatus;
use homesense_core::{
    DeviceId, MotorAction,
    curtain_state::{Movement, Provenance, Snapshot, StopEvidence, Timing},
};
#[cfg(test)]
use homesense_storage::journal::Status;
use uuid::Uuid;
#[derive(Debug)]
pub struct Accepted {
    pub command_ids: Vec<u64>,
    pub projection_tracked: bool,
}
pub struct Candidate {
    admission: Admission,
    controller: Uuid,
    device: DeviceId,
    revision: u64,
    generation: TaskGeneration,
    timing: Timing,
    cached: Snapshot,
    condition_policy: StopEvidence,
}
struct SubmissionGuard<'a> {
    candidate: &'a mut Candidate,
    complete: bool,
}
impl Drop for SubmissionGuard<'_> {
    fn drop(&mut self) {
        if !self.complete {
            self.candidate.invalidate();
        }
    }
}
impl Candidate {
    /// Explicit software candidate timing; conflicts with an existing owner's
    /// timing are rejected. Published LED bindings are never changed here.
    pub async fn new(
        admission: Admission,
        controller: Uuid,
        device: DeviceId,
        timing: Timing,
    ) -> Result<Self, WriteStatus> {
        let generation = admission.task_generation().await?;
        let bundle = admission
            .automation_bundle(controller, generation.clone())
            .await?;
        let cached = admission
            .curtain_candidate(controller, generation.clone(), device.clone(), timing)
            .await?;
        Ok(Self {
            revision: bundle.configuration.revision(),
            admission,
            controller,
            device,
            generation,
            timing,
            cached,
            condition_policy: StopEvidence::ActualOnly,
        })
    }
    fn forget(&mut self) {
        self.cached.observed_movement = None;
        self.cached.observed_position = None;
        self.cached.inferred_movement = None;
        self.cached.inferred_position = None;
        self.cached.inferred_percent = None;
        self.cached.pending = None;
        self.cached.last_written = None;
        self.cached.stop_led_active = None;
    }
    pub async fn refresh(&mut self) -> Result<Snapshot, WriteStatus> {
        let result = self.refresh_inner().await;
        if result.is_err() {
            self.forget();
        }
        result
    }
    async fn refresh_inner(&mut self) -> Result<Snapshot, WriteStatus> {
        let current = self.admission.task_generation().await?;
        let bundle = self
            .admission
            .automation_bundle(self.controller, current.clone())
            .await?;
        if bundle.configuration.revision() != self.revision {
            return Err(WriteStatus::ServiceCommunicationFailure);
        }
        if current != self.generation {
            self.forget();
            self.generation = current.clone();
            self.cached = self
                .admission
                .curtain_candidate(
                    self.controller,
                    current.clone(),
                    self.device.clone(),
                    self.timing,
                )
                .await?;
        }
        self.cached = self
            .admission
            .curtain_snapshots(self.controller, current, vec![self.device.clone()])
            .await?
            .remove(&self.device)
            .ok_or(WriteStatus::ServiceCommunicationFailure)?;
        Ok(self.cached)
    }
    /// Each valid manual action passes the unchanged real principal, revision
    /// and calibrated owner path. Cache/gates/LEDs never suppress motor output.
    pub async fn submit(&mut self, action: MotorAction) -> Result<Accepted, WriteStatus> {
        let mut guard = SubmissionGuard {
            candidate: self,
            complete: false,
        };
        let c = &mut *guard.candidate;
        let _ = c.refresh().await;
        let accepted_generation = c.generation.clone();
        let ids = c
            .admission
            .submit_motor_for_revision(c.revision, c.controller, c.device.clone(), action)
            .await?;
        let tracked = c.refresh().await.is_ok() && c.generation == accepted_generation;
        guard.complete = true;
        Ok(Accepted {
            command_ids: ids,
            projection_tracked: tracked,
        })
    }
    pub fn set_condition_policy(&mut self, policy: StopEvidence) {
        self.condition_policy = policy;
    }
    /// Cached automatic-condition diagnosis only; never called by submit.
    pub fn automatic_open_gate(
        &self,
        previous_was_open: Option<bool>,
        max_age_ms: u64,
    ) -> Option<Provenance> {
        let s = self.cached;
        if self.admission.generation_updates().borrow().as_ref() != Some(&self.generation) {
            return None;
        }
        let now = self.admission.monotonic_ms();
        if previous_was_open != Some(false)
            || s.pending.is_some()
            || max_age_ms == 0
            || now
                .checked_sub(s.captured_ms)
                .is_none_or(|a| a >= max_age_ms)
        {
            return None;
        }
        if s.observed_movement.is_some_and(|f| {
            f.value == Movement::Stopped && now.checked_sub(f.at_ms).is_some_and(|a| a < max_age_ms)
        }) {
            return Some(Provenance::Observed);
        }
        if self.condition_policy == StopEvidence::ReviewedLocalEstimate
            && s.last_written.is_some()
            && s.inferred_movement
                .is_some_and(|f| f.value == Movement::Stopped)
        {
            Some(Provenance::Inferred)
        } else {
            None
        }
    }
    /// Known intervention invalidates the shared projection, never sends STOP.
    pub fn invalidate(&mut self) {
        self.admission.invalidate_curtain(
            self.controller,
            self.generation.clone(),
            self.device.clone(),
        );
        self.forget();
    }
    /// Captured diagnostic only, not a current physical reading.
    pub fn cached(&self) -> Snapshot {
        self.cached
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use homesense_core::{
        ExecutionContext,
        curtain_state::{Movement, Position, Provenance},
    };
    use std::{
        io::{Read, Write},
        sync::{
            Arc, Mutex,
            atomic::{AtomicBool, Ordering},
        },
        time::Duration,
    };
    pub(super) struct Wire(Arc<Mutex<Vec<u8>>>);
    impl Read for Wire {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            Ok(0)
        }
    }
    impl Write for Wire {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    pub(super) struct Fixture {
        pub(super) root: std::path::PathBuf,
        pub(super) actor: Admission,
        pub(super) controller: Uuid,
        pub(super) allowed: Arc<AtomicBool>,
        pub(super) bytes: Arc<Mutex<Vec<u8>>>,
        pub(super) owner: crate::transport_owner::TransportOwner<Wire>,
        pub(super) config: homesense_config::Configuration,
    }
    impl Fixture {
        pub(super) async fn new(calibrated: bool) -> Self {
            let root = std::env::temp_dir().join(format!("curtain-candidate-{}", Uuid::new_v4()));
            std::fs::create_dir_all(&root).unwrap();
            let mut document = serde_json::json!({"schema_version":2,"revision":1,"gateways":[{"id":"bus","endpoint":"127.0.0.1:1","modules":[{"id":"motor-role","address":"26","profile":"c4_din_pro_v2_7_7","verification":{"status":"simulated"}}]}],"devices":[{"id":"curtain","name":"Synthetic curtain","module_id":"motor-role","channel":1,"enabled":true,"motor_calibration":{"open_direction":"down","stop_verified":true,"evidence":"synthetic direction and stop","verified_at_unix_ms":1}}]});
            if !calibrated {
                document["devices"][0]
                    .as_object_mut()
                    .unwrap()
                    .remove("motor_calibration");
            }
            let raw = serde_json::to_vec(&document).unwrap();
            let config =
                homesense_config::Configuration::parse(&raw, ExecutionContext::Simulation).unwrap();
            homesense_storage::Store::open(root.join("runtime.sqlite"))
                .unwrap()
                .publish_configuration(None, &raw, ExecutionContext::Simulation, &[])
                .unwrap();
            let controller = Uuid::new_v4();
            let allowed = Arc::new(AtomicBool::new(true));
            let auth = allowed.clone();
            let actor = Admission::open(
                root.join("runtime.sqlite"),
                crate::admission::Options {
                    context: ExecutionContext::Simulation,
                    ..Default::default()
                },
                move |id| id == controller && auth.load(Ordering::SeqCst),
            )
            .await
            .unwrap();
            actor.synchronized(1, 1).await.unwrap();
            let bytes = Arc::new(Mutex::new(vec![]));
            let owner = crate::transport_owner::TransportOwner::new(
                actor.clone(),
                [(
                    "bus".into(),
                    hb_transport::Connection::for_simulation(Wire(bytes.clone()), 1),
                )]
                .into(),
            );
            Self {
                root,
                actor,
                controller,
                allowed,
                bytes,
                owner,
                config,
            }
        }
        pub(super) async fn candidate(&self, timing: Timing) -> Candidate {
            Candidate::new(
                self.actor.clone(),
                self.controller,
                DeviceId::new("curtain").unwrap(),
                timing,
            )
            .await
            .unwrap()
        }
        pub(super) async fn close(self) {
            self.actor.shutdown().await.unwrap();
            drop(self.owner);
            std::fs::remove_dir_all(self.root).unwrap();
        }
    }
    #[tokio::test]
    async fn calibrated_owner_receipts_only_infer_and_midstop_never_writes_feedback_or_leds() {
        let mut f = Fixture::new(true).await;
        let physical = crate::feedback::Feedback::new(
            &f.config,
            ExecutionContext::Simulation,
            Uuid::parse_str(f.actor.boot_id()).unwrap().as_u128(),
            1,
        )
        .unwrap();
        let before = physical.snapshot();
        let mut c = f.candidate(Timing::default()).await;
        let open = c.submit(MotorAction::Open).await.unwrap();
        assert!(open.projection_tracked);
        let s = c.refresh().await.unwrap();
        assert_eq!(s.pending, Some(MotorAction::Open));
        assert_eq!(s.inferred_movement, None);
        f.owner.step().await.unwrap().unwrap().result.unwrap();
        let s = c.refresh().await.unwrap();
        assert_eq!(s.inferred_movement.unwrap().value, Movement::Opening);
        assert_eq!(s.inferred_position, None);
        assert_eq!(s.observed_movement, None);
        let stop = c.submit(MotorAction::Stop).await.unwrap();
        assert!(stop.projection_tracked);
        assert_eq!(
            c.refresh().await.unwrap().inferred_movement.unwrap().value,
            Movement::Opening
        );
        f.owner.step().await.unwrap().unwrap().result.unwrap();
        let s = c.refresh().await.unwrap();
        assert_eq!(s.inferred_movement.unwrap().value, Movement::Stopped);
        assert_eq!(s.inferred_movement.unwrap().source, Provenance::Inferred);
        assert_eq!(s.inferred_position.unwrap().value, Position::Intermediate);
        assert_eq!(s.stop_led_active, None);
        assert_eq!(physical.snapshot(), before);
        assert_eq!(
            &*f.bytes.lock().unwrap(),
            &[0xae, 0x26, 1, 0xf2, 0xee, 0xae, 0x26, 1, 0xf0, 0xee]
        );
        assert!(f.owner.step().await.unwrap().is_none());
        drop(c);
        f.close().await;
    }
    #[tokio::test]
    async fn queued_motion_cancelled_by_stop_and_transport_failure_do_not_mean_completion() {
        let mut f = Fixture::new(true).await;
        let mut c = f
            .candidate(Timing {
                open_travel_ms: Some(5),
                ..Timing::default()
            })
            .await;
        let open = c.submit(MotorAction::Open).await.unwrap();
        f.actor
            .submit_motor(
                f.controller,
                DeviceId::new("curtain").unwrap(),
                MotorAction::Stop,
            )
            .await
            .unwrap();
        let s = c.refresh().await.unwrap();
        assert_eq!(s.inferred_movement, None);
        assert_eq!(s.pending, Some(MotorAction::Stop));
        assert_eq!(s.last_written, None);
        assert_eq!(
            homesense_storage::Store::open(f.root.join("runtime.sqlite"))
                .unwrap()
                .command_status(f.actor.boot_id(), open.command_ids[0])
                .unwrap(),
            Some(Status::Cancelled)
        );
        f.owner.step().await.unwrap().unwrap().result.unwrap();
        c.submit(MotorAction::Open).await.unwrap();
        f.owner.step().await.unwrap().unwrap().result.unwrap();
        assert_eq!(
            c.refresh().await.unwrap().inferred_movement.unwrap().value,
            Movement::Opening
        );
        f.actor.transport_failed().await.unwrap();
        assert!(c.refresh().await.is_err());
        tokio::time::sleep(Duration::from_millis(8)).await;
        let s = c.cached();
        assert_eq!(s.inferred_movement, None);
        assert_eq!(s.inferred_position, None);
        assert_eq!(s.last_written, None);
        drop(c);
        f.close().await;
    }
    #[tokio::test]
    async fn calibration_authorization_and_write_failure_keep_candidate_unknown() {
        let f = Fixture::new(false).await;
        assert!(
            Candidate::new(
                f.actor.clone(),
                f.controller,
                DeviceId::new("curtain").unwrap(),
                Timing::default()
            )
            .await
            .is_err()
        );
        assert!(f.bytes.lock().unwrap().is_empty());
        f.close().await;
        let f = Fixture::new(true).await;
        let mut c = f.candidate(Timing::default()).await;
        f.allowed.store(false, Ordering::SeqCst);
        assert_eq!(
            c.submit(MotorAction::Stop).await.unwrap_err(),
            WriteStatus::InsufficientPrivileges
        );
        assert_eq!(c.cached().last_written, None);
        assert!(f.bytes.lock().unwrap().is_empty());
        f.allowed.store(true, Ordering::SeqCst);
        let accepted = c.submit(MotorAction::Open).await.unwrap();
        let ticket = f.actor.dispatch().await.unwrap().unwrap();
        assert_eq!(ticket.command.id, accepted.command_ids[0]);
        f.actor
            .finish_write(ticket.command.id, false)
            .await
            .unwrap();
        assert!(c.refresh().await.is_err());
        assert_eq!(c.cached().inferred_movement, None);
        assert_eq!(c.cached().last_written, None);
        drop(c);
        f.close().await;
    }
    #[tokio::test]
    async fn abandoned_future_invalidates_projection_and_never_auto_stops_or_replays() {
        let mut f = Fixture::new(true).await;
        let mut c = f.candidate(Timing::default()).await;
        c.submit(MotorAction::Open).await.unwrap();
        f.owner.step().await.unwrap().unwrap().result.unwrap();
        c.refresh().await.unwrap();
        // Pending actor call after refresh, not a running physical test.
        let mut action = Box::pin(c.submit(MotorAction::Stop));
        use std::task::{Context, Poll, Waker};
        let mut cx = Context::from_waker(Waker::noop());
        assert!(matches!(
            std::future::Future::poll(action.as_mut(), &mut cx),
            Poll::Pending
        ));
        drop(action);
        // Cancellation during preflight also invalidates the old estimate.
        assert_eq!(c.cached().inferred_movement, None);
        assert!(f.owner.step().await.unwrap().is_none());
        assert_eq!(c.refresh().await.unwrap().inferred_movement, None);
        assert_eq!(c.cached().inferred_movement, None);
        assert_eq!(f.bytes.lock().unwrap().len(), 5);
        drop(c);
        f.close().await;
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use super::tests::Fixture;
    use super::*;
    use homesense_core::{ExecutionContext, curtain_state::Movement};
    #[tokio::test]
    async fn recovery_and_restart_never_rehydrate_a_written_motor_command() {
        let mut f = Fixture::new(true).await;
        let mut c = f
            .candidate(Timing {
                open_travel_ms: Some(10),
                ..Timing::default()
            })
            .await;
        c.submit(MotorAction::Open).await.unwrap();
        f.owner.step().await.unwrap().unwrap().result.unwrap();
        assert_eq!(
            c.refresh().await.unwrap().inferred_movement.unwrap().value,
            Movement::Opening
        );
        f.actor.transport_failed().await.unwrap();
        f.actor.recover_synchronized(2, 1).await.unwrap();
        let s = c.refresh().await.unwrap();
        assert_eq!(s.inferred_movement, None);
        assert_eq!(s.last_written, None);
        assert_eq!(s.pending, None);
        assert!(f.actor.dispatch().await.unwrap().is_none());
        drop(c);
        f.actor.clone().shutdown().await.unwrap();
        drop(f.owner);
        let controller = f.controller;
        let actor = Admission::open(
            f.root.join("runtime.sqlite"),
            crate::admission::Options {
                context: ExecutionContext::Simulation,
                ..Default::default()
            },
            move |id| id == controller,
        )
        .await
        .unwrap();
        actor.synchronized(1, 1).await.unwrap();
        let mut fresh = Candidate::new(
            actor.clone(),
            controller,
            DeviceId::new("curtain").unwrap(),
            Timing::default(),
        )
        .await
        .unwrap();
        assert_eq!(fresh.refresh().await.unwrap().last_written, None);
        assert!(actor.dispatch().await.unwrap().is_none());
        assert_eq!(f.bytes.lock().unwrap().len(), 5);
        drop(fresh);
        actor.shutdown().await.unwrap();
        std::fs::remove_dir_all(f.root).unwrap();
    }
}

#[cfg(test)]
mod stop_hint_tests {
    use super::tests::Fixture;
    use super::*;
    #[tokio::test]
    async fn inferred_idle_hint_cannot_deny_calibrated_emergency_stop() {
        let mut f = Fixture::new(true).await;
        let mut c = f
            .candidate(Timing {
                stop_led_idle_ms: Some(1),
                ..Timing::default()
            })
            .await;
        c.submit(MotorAction::Stop).await.unwrap();
        f.owner.step().await.unwrap().unwrap().result.unwrap();
        c.refresh().await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(3)).await;
        assert!(!c.refresh().await.unwrap().stop_led_active.unwrap().value);
        let accepted = c.submit(MotorAction::Stop).await.unwrap();
        assert!(accepted.projection_tracked);
        f.owner.step().await.unwrap().unwrap().result.unwrap();
        assert_eq!(
            &*f.bytes.lock().unwrap(),
            &[0xae, 0x26, 1, 0xf0, 0xee, 0xae, 0x26, 1, 0xf0, 0xee]
        );
        drop(c);
        f.close().await;
    }
}

#[cfg(test)]
mod receipt_clock_tests {
    use super::tests::Fixture;
    use super::*;
    use homesense_core::curtain_state::{Movement, Position};
    #[tokio::test]
    async fn delayed_refresh_after_written_is_already_terminal_with_green_led() {
        let mut f = Fixture::new(true).await;
        let mut c = f
            .candidate(Timing {
                open_travel_ms: Some(5),
                stop_led_idle_ms: Some(3),
                ..Timing::default()
            })
            .await;
        c.submit(MotorAction::Open).await.unwrap();
        f.owner.step().await.unwrap().unwrap().result.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(12)).await;
        let s = c.refresh().await.unwrap();
        assert_eq!(s.inferred_position.unwrap().value, Position::Open);
        assert_eq!(s.inferred_movement.unwrap().value, Movement::Stopped);
        assert!(!s.stop_led_active.unwrap().value);
        assert!(s.captured_ms >= s.last_written.unwrap().written_ms + 8);
        assert_eq!(s.observed_position, None);
        c.submit(MotorAction::Open).await.unwrap();
        f.owner.step().await.unwrap().unwrap().result.unwrap();
        assert_eq!(f.bytes.lock().unwrap().len(), 10);
        assert_eq!(
            c.refresh().await.unwrap().inferred_movement.unwrap().value,
            Movement::Opening
        );
        drop(c);
        f.close().await;
    }
    #[tokio::test]
    async fn owner_preserves_true_written_anchor_when_observed_precedes_first_reader() {
        let mut f = Fixture::new(true).await;
        let mut c = f
            .candidate(Timing {
                open_travel_ms: Some(5),
                ..Timing::default()
            })
            .await;
        let accepted = c.submit(MotorAction::Open).await.unwrap();
        f.owner.step().await.unwrap().unwrap().result.unwrap();
        let mut store = homesense_storage::Store::open(f.root.join("runtime.sqlite")).unwrap();
        store
            .transition_command(
                f.actor.boot_id(),
                accepted.command_ids[0],
                Status::Written,
                Status::Observed,
                f.actor.monotonic_ms(),
            )
            .unwrap();
        drop(store);
        tokio::time::sleep(std::time::Duration::from_millis(12)).await;
        let s = c.refresh().await.unwrap();
        assert_eq!(s.inferred_movement.unwrap().value, Movement::Stopped);
        assert_eq!(s.inferred_position.unwrap().value, Position::Open);
        assert_eq!(s.inferred_percent.unwrap().value.percent(), 100.0);
        assert!(s.captured_ms >= s.last_written.unwrap().written_ms + 5);
        assert_eq!(s.observed_position, None);
        assert!(f.owner.step().await.unwrap().is_none());
        drop(c);
        f.close().await;
    }
}

#[cfg(test)]
mod manual_repeat_tests {
    use super::tests::Fixture;
    use super::*;
    use homesense_core::curtain_state::Provenance;
    #[tokio::test]
    async fn terminal_cached_position_and_unobserved_external_movement_never_deduplicate_open() {
        let mut f = Fixture::new(true).await;
        let mut c = f
            .candidate(Timing {
                open_travel_ms: Some(2),
                ..Timing::default()
            })
            .await;
        c.set_condition_policy(StopEvidence::ReviewedLocalEstimate);
        c.submit(MotorAction::Open).await.unwrap();
        f.owner.step().await.unwrap().unwrap().result.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        let cached = c.refresh().await.unwrap();
        assert_eq!(cached.inferred_percent.unwrap().value.percent(), 100.0);
        assert_eq!(cached.observed_position, None);
        assert_eq!(c.automatic_open_gate(Some(true), 5000), None);
        // There is no position feedback. A cached100 may be wrong after an
        // external/manual move; the model cannot claim to have detected it.
        let before = f.bytes.lock().unwrap().len();
        for _ in 0..2 {
            let accepted = c.submit(MotorAction::Open).await.unwrap();
            assert!(accepted.projection_tracked);
            f.owner.step().await.unwrap().unwrap().result.unwrap();
            let s = c.refresh().await.unwrap();
            assert_eq!(s.last_written.unwrap().action, MotorAction::Open);
            assert_eq!(s.inferred_movement.unwrap().source, Provenance::Inferred);
            assert_eq!(s.observed_position, None);
        }
        assert_eq!(f.bytes.lock().unwrap().len() - before, 10);
        assert_eq!(
            &f.bytes.lock().unwrap()[before..],
            &[0xae, 0x26, 1, 0xf2, 0xee, 0xae, 0x26, 1, 0xf2, 0xee]
        );
        drop(c);
        f.close().await;
    }
    #[tokio::test]
    async fn lost_old_receipt_does_not_block_fresh_owner_validated_manual_stop() {
        let mut f = Fixture::new(true).await;
        let mut c = f.candidate(Timing::default()).await;
        let accepted = c.submit(MotorAction::Open).await.unwrap();
        f.owner.step().await.unwrap().unwrap().result.unwrap();
        let sql = rusqlite::Connection::open(f.root.join("runtime.sqlite")).unwrap();
        sql.execute(
            "DELETE FROM command_journal WHERE command_id=?1",
            [i64::try_from(accepted.command_ids[0]).unwrap()],
        )
        .unwrap();
        drop(sql);
        assert_eq!(c.refresh().await.unwrap().last_written, None);
        let stop = c.submit(MotorAction::Stop).await.unwrap();
        assert!(stop.projection_tracked);
        f.owner.step().await.unwrap().unwrap().result.unwrap();
        assert_eq!(f.bytes.lock().unwrap().len(), 10);
        drop(c);
        f.close().await;
    }
}
