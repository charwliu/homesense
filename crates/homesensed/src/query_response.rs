//! Serial-bus occupancy after a queued manual read. Never a command receipt.
use crate::transport_owner::GatewayRead;
use hb_protocol::{Decoded, Registry};
use homesense_core::{ExecutionContext, ModuleSpec};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) const RESPONSE_MS: u64 = 500;
pub(crate) struct Scope {
    registry: Registry,
    address: u8,
    channels: BTreeSet<u8>,
    epoch: u64,
    solicited: bool,
}
impl Scope {
    pub(crate) fn new(
        module: &ModuleSpec,
        channel: Option<u8>,
        context: ExecutionContext,
        epoch: u64,
    ) -> Result<Self, String> {
        let channels = channel.map_or_else(|| (1..=module.channels()).collect(), |ch| [ch].into());
        let mut registry = if context == ExecutionContext::Simulation {
            Registry::for_simulation()
        } else {
            Registry::default()
        };
        registry
            .register(module.clone())
            .map_err(|e| e.to_string())?;
        Ok(Self {
            registry,
            address: module.address().get(),
            channels,
            epoch,
            solicited:true,
        })
    }
    pub(crate) fn control(module:&ModuleSpec,channels:Vec<u8>,context:ExecutionContext,epoch:u64)->Result<Self,String> {
        let mut scope=Self::new(module,None,context,epoch)?;
        if channels.is_empty() || channels.iter().any(|ch|*ch==0||*ch>module.channels()) {return Err("invalid control reply channels".into());}
        scope.channels=channels.into_iter().collect();scope.solicited=false;Ok(scope)
    }
    fn channel(&self, frame: hb_protocol::RawFrame) -> Option<u8> {
        match self.registry.decode(frame) {
            Decoded::Feedback {
                address, channel, ..
            } if address.get() == self.address && self.channels.contains(&channel.get()) => {
                Some(channel.get())
            }
            _ => None,
        }
    }
}
struct SolicitedBatch {
    received: u64,
    frames: BTreeSet<[u8; 3]>,
}
struct Wait {
    scope: Scope,
    missing: BTreeSet<u8>,
    sent: u64,
    deadline: u64,
    quiet_until: u64,
    gap: u64,
    solicited_batch: Option<SolicitedBatch>,
}
#[derive(Default)]
pub(crate) struct Replies {
    waits: BTreeMap<String, Wait>,
}
impl Replies {
    pub(crate) fn start(&mut self, gateway: String, scope: Scope, sent: u64, gap: u64) {
        self.start_with_timeout(gateway, scope, sent, RESPONSE_MS, gap);
    }
    pub(crate) fn start_with_timeout(
        &mut self,
        gateway: String,
        scope: Scope,
        sent: u64,
        timeout: u64,
        gap: u64,
    ) {
        self.waits.insert(
            gateway,
            Wait {
                missing: scope.channels.clone(),
                scope,
                sent,
                deadline: sent.saturating_add(timeout),
                quiet_until: sent,
                gap,
                solicited_batch: None,
            },
        );
    }
    pub(crate) fn waiting(&self, gateway: &str, now: u64) -> bool {
        self.waits.get(gateway).is_some_and(|wait| {
            (!wait.missing.is_empty() && now < wait.deadline) || now < wait.quiet_until
        })
    }
    pub(crate) fn turnaround(&self, now: u64) -> Option<u64> {
        self.waits
            .values()
            .filter(|wait| {
                (wait.missing.is_empty() || now >= wait.deadline) && now < wait.quiet_until
            })
            .map(|wait| wait.quiet_until - now)
            .min()
    }
    pub(crate) fn awaiting(&self, gateway: &str, now: u64) -> bool {
        self.waits
            .get(gateway)
            .is_some_and(|wait| !wait.missing.is_empty() && now < wait.deadline)
    }
    pub(crate) fn next(&self, now: u64) -> Option<String> {
        self.waits
            .keys()
            .find(|gateway| self.awaiting(gateway, now))
            .cloned()
    }
    pub(crate) fn solicited(&self, gateway: &str, frame: hb_protocol::RawFrame, at: u64) -> bool {
        self.waits.get(gateway).is_some_and(|wait| {
            wait.solicited_batch.as_ref().is_some_and(|batch| {
                batch.received == at
                    && batch
                        .frames
                        .contains(&[frame.address, frame.command, frame.value])
            })
        })
    }
    pub(crate) fn observe(&mut self, read: &GatewayRead) {
        let Some(wait) = self.waits.get_mut(&read.gateway_id) else {
            return;
        };
        if (wait.missing.is_empty() && read.received_at_ms >= wait.quiet_until)
            || read.transport_epoch != wait.scope.epoch
            || read.received_at_ms < wait.sent
            || read.received_at_ms > wait.deadline.saturating_add(wait.gap)
        {
            return;
        }
        if let hb_transport::Receive::Frames {
            transport_epoch,
            frames,
        } = &read.result
        {
            if *transport_epoch != wait.scope.epoch || frames.len() > 103 {
                return;
            }
            // Mark the actual received query batch, not a broad future time
            // range: a later genuine change must still accelerate refresh.
            if wait.scope.solicited && !wait.missing.is_empty() && read.received_at_ms <= wait.deadline {
                wait.solicited_batch = Some(SolicitedBatch {
                    received: read.received_at_ms,
                    frames: frames
                        .iter()
                        .filter(|frame| wait.scope.channel(**frame).is_some())
                        .map(|frame| [frame.address, frame.command, frame.value])
                        .collect(),
                });
            }
            // A completed response still needs a bounded turnaround gap. Even
            // an unrelated received frame occupies this same physical bus;
            // late traffic can never extend beyond the original deadline+gap.
            wait.quiet_until = read
                .received_at_ms
                .saturating_add(wait.gap)
                .min(wait.deadline.saturating_add(wait.gap));
            if read.received_at_ms > wait.deadline {
                return;
            }
            for frame in frames {
                if let Some(channel) = wait.scope.channel(*frame) {
                    wait.missing.remove(&channel);
                }
            }
        }
    }
    pub(crate) fn reset(&mut self, gateway: &str) {
        self.waits.remove(gateway);
    }
    pub(crate) fn clear(&mut self) {
        self.waits.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use homesense_core::{BusAddress, ProtocolProfile, Verification};
    fn scope(epoch: u64) -> Scope {
        Scope::new(
            &ModuleSpec::new(
                BusAddress::new(0x15).unwrap(),
                ProtocolProfile::Ad8Din,
                Verification::Simulated,
            )
            .unwrap(),
            None,
            ExecutionContext::Simulation,
            epoch,
        )
        .unwrap()
    }
    fn read(gateway: &str, epoch: u64, at: u64, frames: Vec<[u8; 5]>) -> GatewayRead {
        GatewayRead {
            gateway_id: gateway.into(),
            transport_epoch: epoch,
            received_at_ms: at,
            result: hb_transport::Receive::Frames {
                transport_epoch: epoch,
                frames: frames
                    .into_iter()
                    .map(|b| hb_protocol::RawFrame::parse(&b).unwrap())
                    .collect(),
            },
        }
    }
    #[test]
    fn complete_unique_valid_reply_set_releases_only_its_bus_and_epoch() {
        let mut r = Replies::default();
        r.start("bus".into(), scope(2), 100, 20);
        assert!(r.waiting("bus", 100));
        assert!(!r.waiting("other", 100));
        let first = (1..=7)
            .map(|ch| [0xae, 0x15, 0xd0 + ch, 30, 0xee])
            .collect::<Vec<_>>();
        r.observe(&read("other", 2, 101, first.clone()));
        r.observe(&read("bus", 1, 101, first.clone()));
        r.observe(&read("bus", 2, 99, first.clone()));
        r.observe(&read("bus", 2, 101, first.clone()));
        r.observe(&read("bus", 2, 102, first));
        r.observe(&read(
            "bus",
            2,
            103,
            vec![[0xae, 0x16, 0xd8, 30, 0xee], [0xae, 0x15, 0xd8, 101, 0xee]],
        ));
        assert!(
            r.waiting("bus", 104),
            "duplicate, wrong address and invalid level cannot complete channel eight"
        );
        r.observe(&read("bus", 2, 105, vec![[0xae, 0x15, 0xd8, 30, 0xee]]));
        assert!(
            r.waiting("bus", 124),
            "allow configured turnaround time after the complete reply"
        );
        assert!(!r.waiting("bus", 125));
        r.observe(&read("bus", 2, 126, vec![[0xae, 0x15, 0xd8, 30, 0xee]]));
        assert!(
            !r.waiting("bus", 127),
            "a finished idle response must not reclaim the bus on later activity"
        );
        assert!(r.solicited(
            "bus",
            hb_protocol::RawFrame::parse(&[0xae, 0x15, 0xd8, 30, 0xee]).unwrap(),
            105
        ));
    }
    #[test]
    fn missing_reply_expires_without_extending_window_or_forging_feedback() {
        let mut r = Replies::default();
        r.start("bus".into(), scope(1), 100, 20);
        assert!(r.waiting("bus", 599));
        assert!(!r.waiting("bus", 600));
        r.observe(&read(
            "bus",
            1,
            601,
            (1..=8)
                .map(|ch| [0xae, 0x15, 0xd0 + ch, 30, 0xee])
                .collect(),
        ));
        assert_eq!(r.waits["bus"].missing.len(), 8);
        assert!(r.waiting("bus", 619));
        assert!(
            !r.waiting("bus", 620),
            "late traffic cannot extend beyond original deadline plus one gap"
        );
        r.reset("bus");
        assert!(r.next(100).is_none());
        r.start("other".into(), scope(2), 100, 20);
        r.clear();
        assert!(r.next(100).is_none());
    }
    #[test]
    fn active_control_reply_reserves_turnaround_without_becoming_solicited() {
        let module=ModuleSpec::new(BusAddress::new(3).unwrap(),ProtocolProfile::R8i8DinPro,Verification::Simulated).unwrap();
        let mut replies=Replies::default();
        replies.start("bus".into(),Scope::control(&module,vec![1,2],ExecutionContext::Simulation,1).unwrap(),100,20);
        let first=read("bus",1,110,vec![[0xae,3,0xd1,0xf1,0xee]]);
        replies.observe(&first);assert!(replies.waiting("bus",130));
        assert!(!replies.solicited("bus",hb_protocol::RawFrame::parse(&[0xae,3,0xd1,0xf1,0xee]).unwrap(),110));
        replies.observe(&read("bus",1,140,vec![[0xae,3,0xd2,0xf2,0xee]]));
        assert!(replies.waiting("bus",159));assert!(!replies.waiting("bus",160));
        // Receiving an Off is occupancy evidence, not a successful On proof.
        assert!(!replies.solicited("bus",hb_protocol::RawFrame::parse(&[0xae,3,0xd2,0xf2,0xee]).unwrap(),140));
    }

}
