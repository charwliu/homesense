//! Admission policy for trusted runtime adapters, not an authentication mechanism.
//! Inputs must come from validated configuration and authenticated sessions, never
//! from client-provided flags. Capability/encoding and durable acceptance follow
//! this gate; passing it alone does not authorize a transport write.
use crate::{
    Command, LevelTransition, MotorMotion,
    queue::{Epochs, Origin, QueuedCommand},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejection {
    Unauthorized,
    UnknownDevice,
    NotController,
    GatewayNotReady,
    ConfigurationChanged,
    Expired,
    PersistenceUnavailable,
}
impl std::fmt::Display for Rejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Rejection {}

#[derive(Debug, Clone, Copy)]
pub struct RuntimePolicy {
    pub epochs: Epochs,
    pub publishing: bool,
    pub controller: bool,
    pub gateway_ready: bool,
    pub storage_healthy: bool,
    /// Loaded from persisted configuration while storage was healthy.
    pub manual_degraded_enabled: bool,
    pub valid_configuration_loaded: bool,
}

/// Result of resolving an authenticated principal and configured device binding.
#[derive(Debug, Clone, Copy)]
pub struct Access {
    pub authorized: bool,
    pub device_exists: bool,
    pub device_verified: bool,
    /// Existing paired session or verified direct panel binding; never a scene.
    pub existing_direct_manual: bool,
}

impl RuntimePolicy {
    /// Call at admission AND immediately before dispatch. The runtime must bump
    /// control_epoch on storage failure/recovery and coordinate queue cancellation.
    pub fn check(
        &self,
        request: &QueuedCommand,
        access: Access,
        now: u64,
    ) -> Result<(), Rejection> {
        if !access.authorized {
            return Err(Rejection::Unauthorized);
        }
        if !access.device_exists {
            return Err(Rejection::UnknownDevice);
        }
        if self.publishing || request.epochs != self.epochs || !self.valid_configuration_loaded {
            return Err(Rejection::ConfigurationChanged);
        }
        if now >= request.deadline_ms {
            return Err(Rejection::Expired);
        }
        if !self.gateway_ready {
            return Err(Rejection::GatewayNotReady);
        }
        // Only the internal scheduler can perform a read-only refresh. External
        // reads consume snapshots instead of causing bus queries.
        let internal_query =
            request.origin == Origin::Background && request.command == Command::Query;
        if !internal_query && !self.controller {
            return Err(Rejection::NotController);
        }
        if !self.storage_healthy && !internal_query {
            let direct_origin = matches!(request.origin, Origin::Hap | Origin::Panel);
            let allowed_action = matches!(
                request.command,
                Command::Switch(_)
                    | Command::SetLevel {
                        transition: LevelTransition::Immediate,
                        ..
                    }
                    | Command::Motor(MotorMotion::Stop)
            );
            if !self.manual_degraded_enabled
                || !direct_origin
                || !access.existing_direct_manual
                || !access.device_verified
                || request.run_id.is_some()
                || !allowed_action
            {
                return Err(Rejection::PersistenceUnavailable);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DeviceId, Percent};
    fn fixture() -> (RuntimePolicy, Access, QueuedCommand) {
        let epochs = Epochs {
            transport: 1,
            configuration: 1,
            control: 1,
        };
        (
            RuntimePolicy {
                epochs,
                publishing: false,
                controller: true,
                gateway_ready: true,
                storage_healthy: false,
                manual_degraded_enabled: false,
                valid_configuration_loaded: true,
            },
            Access {
                authorized: true,
                device_exists: true,
                device_verified: true,
                existing_direct_manual: true,
            },
            QueuedCommand {
                id: 1,
                device_id: DeviceId::new("study:light").unwrap(),
                command: Command::Switch(true),
                origin: Origin::Hap,
                epochs,
                deadline_ms: 100,
                run_id: None,
            },
        )
    }
    #[test]
    fn degraded_mode_is_opt_in_and_http_never_bypasses_persistence() {
        let (mut p, a, mut r) = fixture();
        assert_eq!(p.check(&r, a, 0), Err(Rejection::PersistenceUnavailable));
        p.manual_degraded_enabled = true;
        assert_eq!(p.check(&r, a, 0), Ok(()));
        for origin in [Origin::Http, Origin::Rule, Origin::Background] {
            r.origin = origin;
            assert_eq!(p.check(&r, a, 0), Err(Rejection::PersistenceUnavailable));
        }
    }
    #[test]
    fn degraded_whitelist_excludes_ramps_scenes_and_unverified_bindings() {
        let (mut p, a, mut r) = fixture();
        p.manual_degraded_enabled = true;
        for origin in [Origin::Hap, Origin::Panel] {
            r.origin = origin;
            for command in [
                Command::Switch(false),
                Command::SetLevel {
                    level: Percent::new(30).unwrap(),
                    transition: LevelTransition::Immediate,
                },
                Command::Motor(MotorMotion::Stop),
            ] {
                r.command = command;
                assert_eq!(p.check(&r, a, 0), Ok(()));
            }
            for command in [
                Command::Query,
                Command::PauseRamp,
                Command::PanelLed(true),
                Command::Motor(MotorMotion::Up),
                Command::Motor(MotorMotion::Down),
                Command::SetLevel {
                    level: Percent::new(30).unwrap(),
                    transition: LevelTransition::Ramp,
                },
            ] {
                r.command = command;
                assert_eq!(p.check(&r, a, 0), Err(Rejection::PersistenceUnavailable));
            }
        }
        r.command = Command::Switch(true);
        r.run_id = Some(1);
        assert_eq!(p.check(&r, a, 0), Err(Rejection::PersistenceUnavailable));
        r.run_id = None;
        assert_eq!(
            p.check(
                &r,
                Access {
                    device_verified: false,
                    ..a
                },
                0
            ),
            Err(Rejection::PersistenceUnavailable)
        );
        assert_eq!(
            p.check(
                &r,
                Access {
                    existing_direct_manual: false,
                    ..a
                },
                0
            ),
            Err(Rejection::PersistenceUnavailable)
        );
    }
    #[test]
    fn manual_gates_still_apply_and_publication_rejects_stop() {
        let (mut p, a, mut r) = fixture();
        p.manual_degraded_enabled = true;
        r.command = Command::Motor(MotorMotion::Stop);
        assert_eq!(
            p.check(
                &r,
                Access {
                    authorized: false,
                    ..a
                },
                0
            ),
            Err(Rejection::Unauthorized)
        );
        assert_eq!(
            p.check(
                &r,
                Access {
                    device_exists: false,
                    ..a
                },
                0
            ),
            Err(Rejection::UnknownDevice)
        );
        assert_eq!(p.check(&r, a, 100), Err(Rejection::Expired));
        p.controller = false;
        assert_eq!(p.check(&r, a, 0), Err(Rejection::NotController));
        p.controller = true;
        p.gateway_ready = false;
        assert_eq!(p.check(&r, a, 0), Err(Rejection::GatewayNotReady));
        p.gateway_ready = true;
        p.publishing = true;
        assert_eq!(p.check(&r, a, 0), Err(Rejection::ConfigurationChanged));
        p.publishing = false;
        p.epochs.control += 1;
        assert_eq!(p.check(&r, a, 0), Err(Rejection::ConfigurationChanged));
        r.epochs = p.epochs;
        p.valid_configuration_loaded = false;
        assert_eq!(p.check(&r, a, 0), Err(Rejection::ConfigurationChanged));
    }
    #[test]
    fn readonly_refresh_works_without_control_but_still_needs_authorization() {
        let (mut p, a, mut r) = fixture();
        p.controller = false;
        r.command = Command::Query;
        r.origin = Origin::Background;
        assert_eq!(p.check(&r, a, 0), Ok(()));
        assert_eq!(
            p.check(
                &r,
                Access {
                    authorized: false,
                    ..a
                },
                0
            ),
            Err(Rejection::Unauthorized)
        );
        r.origin = Origin::Http;
        assert_eq!(p.check(&r, a, 0), Err(Rejection::NotController));
    }
}
