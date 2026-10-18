//! Receive-only grace after an actually written managed module cohort. This
//! guards new queries, never control admission, observations or completion.
use crate::module_truth::ModuleTruth;
use homesense_core::DeviceId;
use homesense_state::Quality;
use std::collections::BTreeMap;

#[derive(Default)]
pub(super) struct Guard(BTreeMap<String, Wait>);
struct Wait {
    epoch: u64,
    written: u64,
    until: u64,
    before: BTreeMap<DeviceId, u64>,
}
impl Wait {
    fn deferred(&self, truth: &ModuleTruth, epoch: u64, now: u64) -> bool {
        self.epoch == epoch && now < self.until && !self.before.iter().all(|(id, count)| {
            truth.observation_count(id).is_some_and(|next| next > *count)
                && truth.alias_state(id).is_some_and(|state|
                    state.quality == Quality::Confirmed
                    && matches!(state.observed, Some(homesense_core::Observation::Switch(_) | homesense_core::Observation::Level(_)))
                    && state.observed_at_ms.is_some_and(|at| at >= self.written && at <= now))
        })
    }
}
impl Guard {
    pub fn written(&mut self, gateway: String, devices: &[DeviceId], truth: &ModuleTruth, epoch: u64, at: u64) {
        if devices.is_empty() { return; }
        self.0.insert(gateway, Wait {
            epoch, written: at, until: at.saturating_add(crate::control_batch::FEEDBACK_SAMPLE_MS),
            before: devices.iter().map(|id|(id.clone(),truth.observation_count(id).unwrap_or(0))).collect(),
        });
    }
    pub fn deferred(&self, gateway: &str, truth: &ModuleTruth, epoch: u64, now: u64) -> bool {
        self.0.get(gateway).is_some_and(|wait|wait.deferred(truth,epoch,now))
    }
    pub fn expire(&mut self, truth: &ModuleTruth, epoch: u64, now: u64) {
        self.0.retain(|_,wait|wait.deferred(truth,epoch,now));
    }
    pub fn reset(&mut self) { self.0.clear(); }
    pub fn reset_gateway(&mut self, gateway: &str) { self.0.remove(gateway); }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{feedback::Feedback, transport_owner::GatewayRead};
    use homesense_core::ExecutionContext;

    fn fixture() -> (Feedback, Vec<DeviceId>) {
        let config=homesense_config::Configuration::parse(
            include_bytes!("../../../config/test/native-app-keypad-away/devices.template.json"),
            ExecutionContext::Simulation,
        ).unwrap();
        let ids=(1..=8).flat_map(|channel|["relay-a","relay-b"].map(move |name|
            DeviceId::new(format!("{name}:{channel}")).unwrap())).collect();
        (Feedback::new(&config,ExecutionContext::Simulation,1,1).unwrap(),ids)
    }
    fn receive(feedback:&mut Feedback,address:u8,channels:impl Iterator<Item=u8>,at:u64) {
        feedback.apply_at(&GatewayRead {
            gateway_id:"bus".into(),transport_epoch:1,received_at_ms:at,
            result:hb_transport::Receive::Frames {transport_epoch:1,frames:channels.map(|channel|
                hb_protocol::RawFrame {address,command:0xd0+channel,value:0xf1}).collect()},
        },at).unwrap();
    }
    #[test]
    fn partial_bulk_replies_reserve_reads_but_on_feedback_is_not_completion() {
        let(mut feedback,ids)=fixture();let mut guard=Guard::default();
        receive(&mut feedback,3,1..=8,90);receive(&mut feedback,4,1..=8,90);
        guard.written("bus".into(),&ids,feedback.module_truth(),1,100);
        assert!(guard.deferred("bus",feedback.module_truth(),1,101),"pre-write observations cannot release the receive window");
        receive(&mut feedback,3,1..=8,110);
        assert!(guard.deferred("bus",feedback.module_truth(),1,111),"one module cannot stand for all affected outputs");
        receive(&mut feedback,4,1..=7,120);
        assert!(guard.deferred("bus",feedback.module_truth(),1,121),"a partial second module still owns its receive window");
        receive(&mut feedback,4,std::iter::once(8),130);
        assert!(!guard.deferred("bus",feedback.module_truth(),1,131));
        for id in ids {
            assert_eq!(feedback.module_truth().alias_state(&id).unwrap().observed,Some(homesense_core::Observation::Switch(true)),"a valid reply can release query pacing without pretending that Away succeeded");
        }
        guard.expire(feedback.module_truth(),1,131);assert!(guard.0.is_empty());
    }
    #[test]
    fn receive_window_is_bounded_and_cannot_cross_gateway_or_epoch() {
        let(feedback,ids)=fixture();let mut guard=Guard::default();
        guard.written("bus".into(),&ids,feedback.module_truth(),1,100);
        assert!(!guard.deferred("other",feedback.module_truth(),1,101));
        assert!(!guard.deferred("bus",feedback.module_truth(),2,101));
        assert!(guard.deferred("bus",feedback.module_truth(),1,5099));
        assert!(!guard.deferred("bus",feedback.module_truth(),1,5100),"missing replies cannot reserve the bus indefinitely");
        guard.reset_gateway("other");assert!(guard.deferred("bus",feedback.module_truth(),1,101));
        guard.reset_gateway("bus");assert!(!guard.deferred("bus",feedback.module_truth(),1,101));
        guard.written("bus".into(),&ids,feedback.module_truth(),1,100);guard.reset();assert!(guard.0.is_empty());
    }
}
