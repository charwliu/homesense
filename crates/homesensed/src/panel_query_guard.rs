//! Scheduling only: never invent releases, observations, or command receipts.
use std::collections::BTreeMap;

const RELEASE_WINDOW_MS: u64 = 2_000;
const PERIODIC_QUIET_MS: u64 = 300_000;

#[derive(Default)]
pub(super) struct Guard {
    held: BTreeMap<(String, u8, u8), u64>,
    quiet_until: BTreeMap<String, u64>,
}
impl Guard {
    /// Caller has decoded a registered TS6 button in the current wire epoch.
    pub(super) fn button(
        &mut self,
        gateway: &str,
        address: u8,
        channel: u8,
        pressed: bool,
        received: u64,
    ) {
        let key = (gateway.to_owned(), address, channel);
        if pressed {
            // Keep expired entries until release/reset: repeated F1 must not
            // perpetually extend the short receive-protection window.
            self.held
                .entry(key)
                .or_insert(received.saturating_add(RELEASE_WINDOW_MS));
        } else {
            self.held.remove(&key);
        }
        let until = received.saturating_add(PERIODIC_QUIET_MS);
        let quiet = self.quiet_until.entry(gateway.to_owned()).or_default();
        *quiet = (*quiet).max(until);
    }
    pub(super) fn deferred(&self, gateway: &str, now: u64, postwrite_or_retry: bool) -> bool {
        self.held
            .iter()
            .any(|((bus, _, _), until)| bus == gateway && now < *until)
            || (!postwrite_or_retry
                && self
                    .quiet_until
                    .get(gateway)
                    .is_some_and(|until| now < *until))
    }
    pub(super) fn reset_gateway(&mut self, gateway: &str) {
        self.held.retain(|(bus, _, _), _| bus != gateway);
        self.quiet_until.remove(gateway);
    }
    pub(super) fn reset(&mut self) {
        self.held.clear();
        self.quiet_until.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn release_allows_postwrite_but_periodic_waits_300_seconds_from_last_key_frame() {
        let mut g = Guard::default();
        g.button("bus", 0x41, 1, true, 100);
        assert!(g.deferred("bus", 200, true));
        assert!(!g.deferred("other", 200, false));
        g.button("bus", 0x41, 1, false, 242);
        assert!(!g.deferred("bus", 242, true));
        assert!(g.deferred("bus", 300_241, false));
        assert!(!g.deferred("bus", 300_242, false));
    }
    #[test]
    fn missing_release_duplicate_down_and_late_application_do_not_extend_receive_window() {
        let mut g = Guard::default();
        g.button("bus", 0x41, 1, true, 100);
        g.button("bus", 0x41, 1, true, 2_000);
        assert!(g.deferred("bus", 2_099, true));
        assert!(!g.deferred("bus", 2_100, true));
        g.button("bus", 0x41, 1, true, 2_200);
        assert!(!g.deferred("bus", 2_200, true));
        g.button("bus", 0x41, 1, false, 2_300);
        g.button("bus", 0x41, 1, true, 2_400);
        assert!(g.deferred("bus", 2_400, true));
        // A queued old frame uses receive time, never application time.
        let mut old = Guard::default();
        old.button("bus", 0x41, 1, true, 100);
        assert!(!old.deferred("bus", 400_000, false));
    }
    #[test]
    fn another_key_extends_only_its_bus_quiet_period_and_reset_drops_retired_wire() {
        let mut g = Guard::default();
        g.button("bus", 0x41, 1, true, 100);
        g.button("other", 0x42, 2, true, 150);
        g.button("bus", 0x41, 2, true, 200);
        g.button("bus", 0x41, 1, false, 250);
        assert!(g.deferred("bus", 251, true), "second key remains pressed");
        g.reset_gateway("bus");
        assert!(!g.deferred("bus", 251, false));
        assert!(g.deferred("other", 251, false));
        g.reset();
        assert!(!g.deferred("other", 251, false));
    }
    #[test]
    fn time_near_u64_limit_never_wraps_to_immediate_polling() {
        let mut g = Guard::default();
        g.button("bus", 0x41, 1, true, u64::MAX - 1);
        assert!(g.deferred("bus", u64::MAX - 1, false));
        assert!(!g.deferred("bus", u64::MAX, false));
    }
    #[test]
    fn only_current_epoch_registered_ts6_events_change_query_schedule() {
        use crate::{feedback::Feedback, transport_owner::GatewayRead};
        use homesense_core::ExecutionContext;
        let raw = br#"{"schema_version":2,"revision":1,"gateways":[{"id":"bus","endpoint":"127.0.0.1:1","modules":[{"id":"ts6","address":"41","profile":"ts6_86_rs485_v2_6_5_6key","verification":{"status":"simulated"},"led_mode":"manual"}]}],"devices":[]}"#;
        let c = homesense_config::Configuration::parse(raw, ExecutionContext::Simulation).unwrap();
        let mut f = Feedback::new(&c, ExecutionContext::Simulation, 1, 1).unwrap();
        let read = |epoch, at, bytes: [u8; 5]| GatewayRead {
            gateway_id: "bus".into(),
            transport_epoch: epoch,
            received_at_ms: at,
            result: hb_transport::Receive::Frames {
                transport_epoch: epoch,
                frames: vec![hb_protocol::RawFrame::parse(&bytes).unwrap()],
            },
        };
        for bytes in [
            [0xae, 0x41, 1, 0xf1, 0xee],
            [0xae, 0x41, 0xd7, 0xf1, 0xee],
            [0xae, 0x70, 0xd1, 0xf1, 0xee],
        ] {
            f.apply(&read(1, 100, bytes)).unwrap();
        }
        assert!(!f.query_deferred("bus", 100, false));
        assert!(
            f.apply(&read(2, 100, [0xae, 0x41, 0xd1, 0xf1, 0xee]))
                .is_err()
        );
        assert!(!f.query_deferred("bus", 100, false));
        f.apply(&read(1, 101, [0xae, 0x41, 0xd1, 0xf1, 0xee]))
            .unwrap();
        assert!(f.query_deferred("bus", 101, true));
        f.invalidate().unwrap();
        f.resume(2, 102).unwrap();
        assert!(!f.query_deferred("bus", 102, false));
        f.enable_local_wires([("bus".into(), 2)].into()).unwrap();
        f.apply(&read(2, 103, [0xae, 0x41, 0xd1, 0xf1, 0xee]))
            .unwrap();
        f.invalidate_gateway("bus").unwrap();
        f.attach_gateway_wire("bus", 3).unwrap();
        assert!(!f.query_deferred("bus", 104, false));
        assert!(
            f.apply(&read(2, 104, [0xae, 0x41, 0xd1, 0xf1, 0xee]))
                .is_err()
        );
        assert!(!f.query_deferred("bus", 104, false));
    }
}
