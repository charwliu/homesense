//! Observation scheduling only; targets never replace canonical module truth.
use crate::{admission::Admission, feedback::Feedback, physical_queries::Target};
use homesense_config::Configuration;
use homesense_core::{ModuleKind, Observation};

pub(crate) async fn needed(
    admission: &Admission,
    feedback: &Feedback,
    config: &Configuration,
    target: &Target,
    until: Option<u64>,
) -> Result<bool, String> {
    let Some(until) = until.filter(|until| admission.monotonic_ms() < *until) else {
        return Ok(false);
    };
    let pending = match admission
        .projection_levels(config.revision(), feedback.snapshot().cursor.boot_id)
        .await
    {
        Ok(pending) => pending,
        Err(hap::server::WriteStatus::ResourceBusy) => return Ok(admission.monotonic_ms() < until),
        Err(error) => return Err(format!("post-write confirmation targets: {error:?}")),
    };
    if admission.monotonic_ms() >= until {
        return Ok(false);
    }
    Ok(pending.iter().any(|(id, expected)| {
        let Some((binding, module)) = config.binding(id) else {
            return false;
        };
        if module.kind() != ModuleKind::Dimmer
            || binding.gateway != target.gateway
            || module.address() != target.module.address()
            || !target.channels.contains(&binding.channel.get())
            || (expected.get() == 0 && binding.supply_relay.is_some())
        {
            return false;
        }
        feedback
            .module_truth()
            .received_port_state(
                &target.gateway,
                module.address().get(),
                binding.channel.get(),
            )
            .is_none_or(|state| {
                state.quality != homesense_state::Quality::Confirmed
                    || state.observed != Some(Observation::Level(*expected))
            })
    }))
}

/// Whole-module AD8 reads must also wait for any of their ramping channels.
/// A later direct write to that same channel clears its previous ramp fence.
#[derive(Default)]
pub(crate) struct RampReads(std::collections::BTreeMap<(String,u8,u8),u64>);
impl RampReads {
    pub(crate) fn written(&mut self,config:&Configuration,device:&homesense_core::DeviceId,now:u64,ramped:bool) {
        let Some((binding,module))=config.binding(device) else{return;};
        let key=(binding.gateway.clone(),module.address().get(),binding.channel.get());
        // Admission's clock floors elapsed time to milliseconds. Retain one
        // rounding quantum so the real wire-to-query delay is never <6s.
        if ramped {self.0.insert(key,now.saturating_add(6_001));}
        else {self.0.remove(&key);}
    }
    pub(crate) fn ready(&self,target:&Target,now:u64)->bool {
        target.channels.iter().all(|channel|self.0.get(&(target.gateway.clone(),target.module.address().get(),*channel)).is_none_or(|at|now>=*at))
    }
    pub(crate) fn forget_gateway(&mut self,gateway:&str) {self.0.retain(|(bus,_,_),_|bus!=gateway);}
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ramp_reads_wait_six_seconds_and_direct_writes_supersede_only_their_channel() {
        use homesense_core::{DeviceId,ExecutionContext};
        let config=Configuration::parse(&serde_json::to_vec(&serde_json::json!({
            "schema_version":2,"revision":1,"gateways":[{"id":"bus","endpoint":"127.0.0.1:1","modules":[
                {"id":"ad8","address":"11","profile":"ad8_din_rs485_v2_2_1","verification":{"status":"simulated"}},
                {"id":"relay","address":"03","profile":"r8i8_din_pro_v2_7_7","verification":{"status":"simulated"}}
            ]}],"devices":[
                {"id":"one","name":"One","module_id":"ad8","channel":1,"enabled":true},
                {"id":"two","name":"Two","module_id":"ad8","channel":2,"enabled":true},
                {"id":"supply","name":"Supply","module_id":"relay","channel":1,"enabled":true}
            ]
        })).unwrap(),ExecutionContext::Simulation).unwrap();
        let targets=crate::physical_queries::plan(&config,ExecutionContext::Simulation).unwrap();
        let dimmer=targets.iter().find(|t|t.module.kind()==ModuleKind::Dimmer).unwrap();
        let relay=targets.iter().find(|t|t.module.kind()==ModuleKind::Switch).unwrap();
        let mut delay=RampReads::default();
        let one=DeviceId::new("one").unwrap();let two=DeviceId::new("two").unwrap();
        delay.written(&config,&one,100,true);
        assert!(!delay.ready(dimmer,6100),"floor-rounded write time must retain the last fractional millisecond");assert!(delay.ready(dimmer,6101));assert!(delay.ready(relay,100));
        delay.written(&config,&two,200,true);
        delay.written(&config,&one,300,false);
        assert!(!delay.ready(dimmer,6100),"direct channel one does not erase channel two's ramp");
        delay.written(&config,&two,400,false);
        assert!(delay.ready(dimmer,400),"direct off/full-on cancels old ramp wait");
        delay.written(&config,&one,500,true);delay.written(&config,&one,600,true);
        assert!(!delay.ready(dimmer,6500));assert!(!delay.ready(dimmer,6600));assert!(delay.ready(dimmer,6601),"new ramp restarts its own six seconds plus the clock rounding guard");
        delay.forget_gateway("unrelated");assert!(!delay.ready(dimmer,1000));
        delay.forget_gateway("bus");assert!(delay.ready(dimmer,1000),"recovery must query actual state immediately");
    }
}
