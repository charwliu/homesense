//! Persistent transport recovery for an already established service.
//! Every round is bounded and uses a new epoch. Failed feedback never opens
//! control; listeners remain owned by the caller until deadline/shutdown.
use crate::{
    admission::Admission,
    feedback::Feedback,
    observed_transport::{ObservedTransport, Synchronization},
    reconnect::{RetryPolicy, reconnect_tcp},
};
use homesense_config::Configuration;
use homesense_core::ExecutionContext;
use std::{
    collections::BTreeMap,
    net::{SocketAddr, TcpStream},
    time::Duration,
};
use tokio::sync::watch;
fn delay(attempt: u32, fast_budget: u8) -> Duration {
    if attempt == 0 {
        return Duration::ZERO;
    }
    let fast = u32::from(fast_budget.max(1));
    if attempt < fast {
        return Duration::from_millis(250);
    }
    Duration::from_secs(
        1u64.checked_shl(attempt.saturating_sub(fast).min(5))
            .unwrap_or(30)
            .min(30),
    )
}
pub(crate) async fn until_ready(
    admission: Admission,
    config: &Configuration,
    endpoints: BTreeMap<String, SocketAddr>,
    previous_epoch: u64,
    fast_budget: u8,
    deadline: tokio::time::Instant,
    stop: watch::Receiver<bool>,
) -> Result<Option<(ObservedTransport<TcpStream>, u64)>, String> {
    let work = async {
        let mut epoch = previous_epoch;
        let mut attempt = 0u32;
        let mut query_round=0usize;
        loop {
            if *stop.borrow() || stop.has_changed().is_err() {
                return Ok(None);
            }
            let wait = delay(attempt, fast_budget);
            if !wait.is_zero() {
                log::info!(
                    "gateway_recovery_backoff attempt={} delay_ms={} previous_epoch={epoch}",
                    attempt.saturating_add(1),
                    wait.as_millis()
                );
                let mut stopped = stop.clone();
                tokio::select! {_=tokio::time::sleep(wait)=>{},_=stopped.changed()=>return Ok(None)}
            }
            let issued = epoch.checked_add(1).ok_or("transport epoch exhausted")?;
            let policy = RetryPolicy {
                attempts: 1,
                ..RetryPolicy::default()
            };
            let connected = reconnect_tcp(
                admission.clone(),
                endpoints.clone(),
                epoch,
                policy,
                true,
                stop.clone(),
            )
            .await;
            // An unsuccessful connect also consumes its attempted epoch. Never
            // reuse an epoch merely because no stream reached synchronization.
            epoch = issued;
            attempt = attempt.saturating_add(1);
            let (transport, connected_epoch) = match connected {
                Ok(value) => value,
                Err(error) if error.starts_with("reconnect attempts exhausted:") => {
                    log::warn!(
                        "gateway_recovery_pending epoch={epoch} attempt={attempt} error={error}"
                    );
                    continue;
                }
                Err(error) => return Err(error), // fault audit / invariant / worker failure
            };
            if connected_epoch != issued {
                return Err("recovery returned a reused transport epoch".into());
            }
            let feedback = Feedback::new(
                config,
                ExecutionContext::Live,
                uuid::Uuid::parse_str(admission.boot_id())
                    .map_err(|e| e.to_string())?
                    .as_u128(),
                epoch,
            )?;
            let mut owner = ObservedTransport::new_recovering(transport, feedback, epoch)?;
            owner.set_recovery_query_offset(query_round)?;
            query_round=query_round.checked_add(1).ok_or("recovery query round exhausted")?;
            match owner
                .query_recovery_with_policy(
                    config,
                    ExecutionContext::Live,
                    1000,
                    5000,
                    Synchronization::OutputsAndCalibratedMotors,
                )
                .await
            {
                Ok(()) => {
                    log::info!(
                        "gateway_recovery_synchronized epoch={epoch} attempt={attempt} revision={}",
                        config.revision()
                    );
                    return Ok(Some((owner, epoch)));
                }
                Err(error)
                    if owner.retryable_failure()
                        || error.starts_with("fresh feedback required")
                        || error.starts_with("fresh logical feedback") =>
                {
                    log::warn!(
                        "gateway_recovery_pending epoch={epoch} attempt={attempt} synchronized=false error={error}"
                    );
                    // No owner or partial truth is reused in the next round.
                    // The Admission fault fence and journal remain authoritative.
                }
                Err(error) => return Err(error),
            }
        }
    };
    let mut shutdown=stop.clone();
    tokio::select! {
        result=work=>result,
        _=tokio::time::sleep_until(deadline)=>Ok(None),
        _=shutdown.changed()=>Ok(None),
    }
}
#[cfg(test)]
#[test]
fn retry_rounds_are_bounded_without_exhausting_the_service_lifetime() {
    assert_eq!(delay(0, 2), Duration::ZERO);
    assert_eq!(delay(1, 2), Duration::from_millis(250));
    assert_eq!(delay(2, 2), Duration::from_secs(1));
    assert_eq!(delay(3, 2), Duration::from_secs(2));
    assert_eq!(delay(99, 2), Duration::from_secs(30));
    assert_eq!(delay(1, 0), Duration::from_secs(1));
}

#[cfg(test)]
#[path="runtime_transport_recovery_tests.rs"]
mod tests;
