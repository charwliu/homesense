//! Poll replies are not activity. Observation time is never renewed by waiting.
use homesense_config::RefreshPolicy;
pub(crate) struct Schedule {
    policy: RefreshPolicy,
    stage: usize,
    quiet: u32,
    activity: bool,
    allowance: u64,
}
impl Schedule {
    pub fn new(policy: RefreshPolicy) -> Self {
        Self {
            policy,
            stage: 3,
            quiet: 0,
            activity: false,
            allowance: policy.observation_max_age_ms(),
        }
    }
    pub fn interval(&self) -> u64 {
        match self.stage {
            0 => self.policy.active_interval_ms,
            1 => self.policy.backoff_ms[0],
            2 => self.policy.backoff_ms[1],
            _ => self.policy.idle_interval_ms,
        }
    }
    pub fn allowance(&self) -> u64 {
        self.allowance
    }
    pub fn activity(&mut self) {
        self.stage = 0;
        self.quiet = 0;
        self.activity = true;
    }
    pub fn completed(&mut self) -> u64 {
        if self.activity {
            self.activity = false;
            self.quiet = 0;
        } else if self.stage < 3 {
            self.quiet += 1;
            if self.quiet >= self.policy.quiet_rounds_per_step {
                self.quiet = 0;
                self.stage += 1;
            }
        }
        self.allowance = self.policy.age_for_interval(self.interval());
        self.interval()
    }
}
/// Pre-receive candidate only: activity requires the unified decoder to advance
/// the same registered port's count after apply, under the current wire epoch.
pub(crate) fn candidate(truth:&crate::module_truth::ModuleTruth,gateway:&str,frame:&hb_protocol::RawFrame)->Option<(u8,u8,u64)>{
    let channel=frame.command.checked_sub(0xd0)?;let state=truth.received_port_state(gateway,frame.address,channel)?;
    let allowed=matches!(state.kind,homesense_core::ModuleKind::Switch|homesense_core::ModuleKind::Dimmer)
        ||state.kind==homesense_core::ModuleKind::Keypad&&(1..=6).contains(&channel)&&matches!(frame.value,0xf1|0xf2)&&truth.module_objects().iter().any(|m|m.gateway==gateway&&m.spec.address().get()==frame.address&&m.spec.profile()==homesense_core::ProtocolProfile::Ts686Rs485SixKey);
    allowed.then(||truth.port_observation_count(gateway,frame.address,channel).map(|count|(frame.address,channel,count))).flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn startup_is_idle_activity_accelerates_and_quiet_rounds_back_off() {
        let mut s = Schedule::new(RefreshPolicy::default());
        assert_eq!(s.interval(), 10_800_000);
        assert_eq!(s.allowance(), 10_815_000);
        s.activity();
        assert_eq!(s.interval(), 10_000);
        assert_eq!(
            s.allowance(),
            10_815_000,
            "retain old-port allowance until complete fast round"
        );
        assert_eq!(s.completed(), 10_000);
        assert_eq!(s.allowance(), 25_000);
        assert_eq!(s.completed(), 10_000);
        assert_eq!(s.completed(), 30_000);
        assert_eq!(s.completed(), 30_000);
        assert_eq!(s.completed(), 60_000);
        assert_eq!(s.completed(), 60_000);
        assert_eq!(s.completed(), 10_800_000);
        assert_eq!(
            s.completed(),
            10_800_000,
            "quiet query replies never accelerate"
        );
        s.activity();
        s.completed();
        assert_eq!(s.interval(), 10_000);
    }
    #[test]
    fn longer_idle_is_supported_without_forging_observations() {
        let p = RefreshPolicy {
            idle_interval_ms: 43_200_000,
            ..Default::default()
        };
        let mut s = Schedule::new(p);
        assert_eq!(s.completed(), 43_200_000);
        assert_eq!(s.allowance(), 43_215_000);
    }
}
