//! Gateway-local readiness and wire provenance within one service generation.
//! This model grants no authority and performs no I/O. Authentication, durable
//! acceptance and global publication/storage gates remain the owner's duties.
use crate::queue::Epochs;
use std::collections::{BTreeMap, BTreeSet};

/// Matches the supported configuration capacity across parsing and runtime.
pub const MAX_GATEWAYS: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct WireScope {
    gateway: String,
    epoch: u64,
}
impl WireScope {
    pub fn gateway(&self) -> &str {
        &self.gateway
    }
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
    /// Trusted persisted-scope decoder, not a client-issued control capability.
    pub fn from_persisted(gateway: String, epoch: u64) -> Result<Self, Error> {
        if !valid_id(&gateway) || epoch == 0 {
            return Err(Error::InvalidScope);
        }
        Ok(Self { gateway, epoch })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Port {
    pub address: u8,
    pub channel: u8,
}
impl Port {
    fn valid(self) -> bool {
        self.address != 0 && (1..=16).contains(&self.channel)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Offline,
    Connecting,
    Synchronizing,
    Ready,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReadRequirement {
    pub port: Port,
    pub before_count: u64,
    pub sent_at_ms: u64,
    pub deadline_ms: u64,
}

#[derive(Clone, Debug)]
struct Gateway {
    epoch: u64,
    phase: Phase,
    requirements: BTreeMap<Port, ReadRequirement>,
    completed: BTreeSet<Port>,
}

/// Captured once before a task or command is accepted. Recovery never rewrites
/// this read-set to make old delayed/relative work current again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dependencies {
    global: Epochs,
    wires: Vec<WireScope>,
}
impl Dependencies {
    pub fn global(&self) -> Epochs {
        self.global
    }
    pub fn wires(&self) -> &[WireScope] {
        &self.wires
    }
    pub fn depends_on(&self, gateway: &str) -> bool {
        self.wires.iter().any(|scope| scope.gateway() == gateway)
    }
    /// Readiness and original connection identity are both required.
    pub fn current(&self, table: &GatewayTable) -> bool {
        self.global == table.global && self.wires.iter().all(|s| table.ready(s))
    }
}

#[derive(Clone, Debug)]
pub struct GatewayTable {
    global: Epochs,
    gateways: BTreeMap<String, Gateway>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidScope,
    UnknownGateway,
    DuplicateGateway,
    InvalidRequirement,
    DuplicatePort,
    StaleScope,
    Unavailable,
    Busy,
    CounterExhausted,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Error {}
fn valid_id(id: &str) -> bool {
    !id.trim().is_empty() && id.len() <= 256 && !id.chars().any(char::is_control)
}

impl GatewayTable {
    pub fn new(global: Epochs, ids: impl IntoIterator<Item = String>) -> Result<Self, Error> {
        if global.transport == 0 || global.configuration == 0 || global.control == 0 {
            return Err(Error::InvalidScope);
        }
        let mut gateways = BTreeMap::new();
        for id in ids {
            if !valid_id(&id) || gateways.len() >= MAX_GATEWAYS {
                return Err(Error::InvalidScope);
            }
            if gateways
                .insert(
                    id,
                    Gateway {
                        epoch: 0,
                        phase: Phase::Offline,
                        requirements: BTreeMap::new(),
                        completed: BTreeSet::new(),
                    },
                )
                .is_some()
            {
                return Err(Error::DuplicateGateway);
            }
        }
        if gateways.is_empty() {
            return Err(Error::InvalidScope);
        }
        Ok(Self { global, gateways })
    }
    pub fn global(&self) -> Epochs {
        self.global
    }
    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.gateways.keys().map(String::as_str)
    }
    pub fn phase(&self, gateway: &str) -> Result<Phase, Error> {
        Ok(self
            .gateways
            .get(gateway)
            .ok_or(Error::UnknownGateway)?
            .phase)
    }
    /// Every attempt, including a failed connect, consumes a unique local epoch.
    pub fn begin_attempt(&mut self, gateway: &str) -> Result<WireScope, Error> {
        let entry = self
            .gateways
            .get_mut(gateway)
            .ok_or(Error::UnknownGateway)?;
        if entry.phase != Phase::Offline {
            return Err(Error::Busy);
        }
        let next = entry.epoch.checked_add(1).ok_or(Error::CounterExhausted)?;
        entry.epoch = next;
        entry.phase = Phase::Connecting;
        entry.requirements.clear();
        entry.completed.clear();
        Ok(WireScope {
            gateway: gateway.into(),
            epoch: next,
        })
    }
    /// Attach a constructor's actual already-established wire identity before
    /// checking complete startup receipt proof. It cannot replace existing work.
    pub fn adopt_initial_attempt(&mut self,scope:&WireScope)->Result<(),Error> {
        let entry=self.gateways.get_mut(scope.gateway()).ok_or(Error::UnknownGateway)?;
        if entry.epoch!=0 || entry.phase!=Phase::Offline {return Err(Error::Busy);}
        entry.epoch=scope.epoch();entry.phase=Phase::Connecting;
        Ok(())
    }
    pub fn current_wire(&self,gateway:&str)->Result<WireScope,Error> {
        let entry=self.gateways.get(gateway).ok_or(Error::UnknownGateway)?;
        if entry.epoch==0 {return Err(Error::Unavailable);}
        Ok(WireScope {gateway:gateway.into(),epoch:entry.epoch})
    }
    pub fn all_ready(&self)->bool {self.gateways.values().all(|entry|entry.phase==Phase::Ready)}
    /// A control fence changes task authority but not established wire identity.
    /// Configuration/service transport changes require a separate new owner.
    pub fn advance_control(&mut self,next:Epochs)->Result<(),Error> {
        if next.configuration!=self.global.configuration || next.transport!=self.global.transport
            || next.control<self.global.control || next.control==0 {return Err(Error::InvalidScope);}
        self.global=next;Ok(())
    }

    fn matching(&self, scope: &WireScope) -> Result<&Gateway, Error> {
        let entry = self
            .gateways
            .get(scope.gateway())
            .ok_or(Error::UnknownGateway)?;
        if entry.epoch != scope.epoch() || entry.epoch == 0 {
            return Err(Error::StaleScope);
        }
        Ok(entry)
    }
    pub fn ready(&self, scope: &WireScope) -> bool {
        self.matching(scope)
            .is_ok_and(|entry| entry.phase == Phase::Ready)
    }
    /// Connection and configured profile/calibration validation precede this.
    /// Empty requirements are valid only for buses without queryable outputs;
    /// the configured transport adapter must derive the complete required set.
    pub fn begin_synchronization(
        &mut self,
        scope: &WireScope,
        requirements: impl IntoIterator<Item = ReadRequirement>,
    ) -> Result<(), Error> {
        if self.matching(scope)?.phase != Phase::Connecting {
            return Err(Error::Busy);
        }
        let mut required = BTreeMap::new();
        for requirement in requirements {
            if !requirement.port.valid() || requirement.deadline_ms < requirement.sent_at_ms {
                return Err(Error::InvalidRequirement);
            }
            if required.insert(requirement.port, requirement).is_some() {
                return Err(Error::DuplicatePort);
            }
            if required.len() > 4096 {
                return Err(Error::InvalidRequirement);
            }
        }
        let entry = self
            .gateways
            .get_mut(scope.gateway())
            .ok_or(Error::UnknownGateway)?;
        entry.phase = if required.is_empty() {
            Phase::Ready
        } else {
            Phase::Synchronizing
        };
        entry.requirements = required;
        entry.completed.clear();
        Ok(())
    }
    /// Caller supplies a valid actual received sample/count, never a sent query
    /// or cached state-event version. Old epochs and samples outside the window
    /// cannot complete synchronization, even when the value happens to match.
    pub fn received(
        &mut self,
        scope: &WireScope,
        port: Port,
        observation_count: u64,
        observed_at_ms: u64,
    ) -> Result<bool, Error> {
        let entry = self.matching(scope)?;
        if entry.phase != Phase::Synchronizing {
            return Err(Error::Busy);
        }
        let required = entry
            .requirements
            .get(&port)
            .ok_or(Error::InvalidRequirement)?;
        if observation_count <= required.before_count
            || observed_at_ms < required.sent_at_ms
            || observed_at_ms > required.deadline_ms
        {
            return Ok(false);
        }
        let entry = self
            .gateways
            .get_mut(scope.gateway())
            .ok_or(Error::UnknownGateway)?;
        entry.completed.insert(port);
        if entry.completed.len() == entry.requirements.len() {
            entry.phase = Phase::Ready;
        }
        Ok(entry.phase == Phase::Ready)
    }
    /// Fence only the failed bus. Global generation and healthy buses survive.
    /// The caller must durably audit affected work before retrying this bus.
    pub fn fault(&mut self, scope: &WireScope) -> Result<(), Error> {
        self.matching(scope)?;
        let entry = self
            .gateways
            .get_mut(scope.gateway())
            .ok_or(Error::UnknownGateway)?;
        entry.phase = Phase::Offline;
        entry.requirements.clear();
        entry.completed.clear();
        Ok(())
    }
    pub fn capture(
        &self,
        gateways: impl IntoIterator<Item = String>,
    ) -> Result<Dependencies, Error> {
        let mut wires = BTreeMap::new();
        for gateway in gateways {
            let entry = self.gateways.get(&gateway).ok_or(Error::UnknownGateway)?;
            if entry.phase != Phase::Ready {
                return Err(Error::Unavailable);
            }
            wires.insert(
                gateway.clone(),
                WireScope {
                    gateway,
                    epoch: entry.epoch,
                },
            );
        }
        Ok(Dependencies {
            global: self.global,
            wires: wires.into_values().collect(),
        })
    }
}

#[cfg(test)]
#[path = "gateway_scope_tests.rs"]
mod tests;
