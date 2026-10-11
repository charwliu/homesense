//! Bounded TCP connection establishment; never sends a command or enables control.
use crate::{admission::Admission, transport_owner::TransportOwner};
use hb_transport::Connection;
use std::{
    collections::BTreeMap,
    net::{SocketAddr, TcpStream},
    time::Duration,
};
use tokio::sync::watch;

#[derive(Clone, Copy)]
pub struct RetryPolicy {
    pub attempts: u8,
    pub connect_timeout: Duration,
    pub io_timeout: Duration,
    pub initial_delay: Duration,
    pub maximum_delay: Duration,
}
impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            attempts: 5,
            connect_timeout: Duration::from_secs(2),
            io_timeout: Duration::from_millis(100),
            initial_delay: Duration::from_millis(250),
            maximum_delay: Duration::from_secs(4),
        }
    }
}
impl RetryPolicy {
    fn validate(self) -> Result<(), String> {
        if self.attempts == 0
            || self.attempts > 16
            || self.connect_timeout.is_zero()
            || self.connect_timeout > Duration::from_secs(5)
            || self.io_timeout.is_zero()
            || self.io_timeout > Duration::from_secs(2)
            || self.initial_delay.is_zero()
            || self.initial_delay > self.maximum_delay
            || self.maximum_delay > Duration::from_secs(30)
        {
            return Err("invalid reconnect bounds".into());
        }
        Ok(())
    }
}
struct CancellationGuard {
    admission: Admission,
    complete: bool,
}
impl Drop for CancellationGuard {
    fn drop(&mut self) {
        if !self.complete {
            self.admission.stop_now();
        }
    }
}
/// Caller must have verified concurrent gateway clients do not displace EA-5.
/// Every attempt uses a new epoch and discards all partial connection sets.
/// The returned owner still requires new-generation feedback and explicit
/// synchronization; no queued command is replayed by this function.
pub async fn reconnect_tcp(
    admission: Admission,
    endpoints: BTreeMap<String, SocketAddr>,
    previous_epoch: u64,
    policy: RetryPolicy,
    coexistence_confirmed: bool,
    mut stop: watch::Receiver<bool>,
) -> Result<(TransportOwner<TcpStream>, u64), String> {
    policy.validate()?;
    if !coexistence_confirmed
        || previous_epoch == 0
        || endpoints.is_empty()
        || endpoints.len() > 32
        || endpoints.iter().any(|(id, address)| {
            id.is_empty()
                || id.len() > 256
                || address.port() == 0
                || address.ip().is_unspecified()
                || address.ip().is_multicast()
        })
    {
        return Err("invalid reconnect scope or coexistence not confirmed".into());
    }
    let mut guard = CancellationGuard {
        admission: admission.clone(),
        complete: false,
    };
    admission
        .transport_failed()
        .await
        .map_err(|e| format!("fault audit: {e:?}"))?;
    let mut delay = policy.initial_delay;
    let mut epoch = previous_epoch;
    let mut last_error = String::new();
    for attempt in 0..policy.attempts {
        if *stop.borrow() || stop.has_changed().is_err() {
            return Err("reconnect stopped".into());
        }
        epoch = epoch.checked_add(1).ok_or("transport epoch exhausted")?;
        log::info!("gateway_connect_attempt epoch={epoch} attempt={} gateways={}", attempt + 1, endpoints.len());
        let mut connections = BTreeMap::new();
        for (id, address) in &endpoints {
            let address = *address;
            let work = tokio::task::spawn_blocking(move || {
                Connection::connect(address, epoch, policy.connect_timeout, policy.io_timeout)
            });
            let connected = tokio::select! {
                result = work => result.map_err(|e| format!("connect worker: {e}"))?,
                _ = stop.changed() => return Err("reconnect interrupted".into()),
            };
            match connected {
                Ok(connection) => {
                    log::info!("gateway_connected gateway={id:?} epoch={epoch}; synchronization pending");
                    connections.insert(id.clone(), connection);
                }
                Err(error) => {
                    log::warn!("gateway_connect_failed gateway={id:?} epoch={epoch} error={error:?}");
                    last_error = format!("gateway {id}: {error}");
                    break;
                }
            }
        }
        if connections.len() == endpoints.len() {
            if *stop.borrow() || stop.has_changed().is_err() {
                return Err("reconnect stopped".into());
            }
            guard.complete = true;
            return Ok((TransportOwner::new(admission, connections), epoch));
        }
        drop(connections);
        if attempt + 1 < policy.attempts {
            log::info!("gateway_connect_backoff epoch={epoch} delay_ms={}", delay.as_millis());
            tokio::select! {
                _ = tokio::time::sleep(delay) => {},
                _ = stop.changed() => return Err("reconnect interrupted".into()),
            }
            delay = delay.saturating_mul(2).min(policy.maximum_delay);
        }
    }
    // Exhaustion leaves admission faulted but permits a later explicit retry.
    guard.complete = true;
    Err(format!("reconnect attempts exhausted: {last_error}"))
}
