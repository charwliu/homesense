//! Poll replies are not activity. Observation time is never renewed by waiting.
use homesense_config::RefreshPolicy;
pub(crate) struct Schedule {
    policy: RefreshPolicy,
    stage: usize,
    quiet: u32,
    activity: bool,
    allowance: u64,
    active_since: Option<u64>,
    retry_interval: Option<u64>,
}
impl Schedule {
    pub fn new(policy: RefreshPolicy) -> Self {
        Self {
            policy,
            stage: 3,
            quiet: 0,
            activity: false,
            allowance: policy.observation_max_age_ms(),
            active_since: None,
            retry_interval: None,
        }
    }
    pub fn interval(&self) -> u64 {
        if let Some(interval)=self.retry_interval {return interval;}
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
    pub fn activity(&mut self, now:u64) {
        self.active_since=Some(now);
        self.retry_interval=None;
        self.stage = 0;
        self.quiet = 0;
        self.activity = true;
    }
    /// Failure to complete a whole round must not pin rapid polling forever.
    /// This changes scheduling only: missing ports stay unknown, and pending
    /// commands retain their independent confirmation deadlines.
    pub fn settle_if_quiet(&mut self, now:u64)->bool {
        if self.active_since.is_some_and(|at|now.saturating_sub(at)>=300_000) {
            self.active_since=None;
            self.retry_interval=None;
            self.stage=3;
            self.quiet=0;
            self.activity=false;
            self.allowance=self.policy.observation_max_age_ms();
            true
        } else {false}
    }
    /// A missed reply is congestion or missing evidence, never activity.
    /// Back off immediately while retaining the existing observation bound.
    pub fn timeout(&mut self)->u64 {
        let next=self.interval().max(15_000).saturating_mul(2).min(self.policy.idle_interval_ms);
        self.retry_interval=Some(next);
        self.stage=(self.stage+1).min(3);
        self.quiet=0;
        self.activity=false;
        self.interval()
    }
    pub fn completed(&mut self) -> u64 {
        self.retry_interval=None;
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
    // Repeated equal status packets are observations, not new user activity.
    // Otherwise an unsolicited echo stream can indefinitely rearm fast scans.
    let same=match (state.kind,state.observed,frame.value) {
        (homesense_core::ModuleKind::Switch,Some(homesense_core::Observation::Switch(old)),0xf1|0xf2)=>old==(frame.value==0xf1),
        (homesense_core::ModuleKind::Dimmer,Some(homesense_core::Observation::Level(old)),0..=100)=>old.get()==frame.value,
        _=>false,
    };
    if same && state.quality==homesense_state::Quality::Confirmed {return None;}
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
        s.activity(0);
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
        s.activity(0);
        s.completed();
        assert_eq!(s.interval(), 10_000);
    }
    #[test]
    fn incomplete_rounds_cannot_keep_fast_scans_alive_after_five_quiet_minutes() {
        let mut s=Schedule::new(RefreshPolicy::default());
        s.activity(100);
        assert!(!s.settle_if_quiet(300_099));
        assert!(s.settle_if_quiet(300_100));
        assert_eq!(s.interval(),10_800_000);
        assert_eq!(s.allowance(),10_815_000);
        assert!(!s.settle_if_quiet(600_100),"do not repeatedly postpone the idle due time");
        s.activity(600_101);
        assert_eq!(s.interval(),10_000,"a real new command rearms bounded confirmation");
        assert!(!s.settle_if_quiet(900_100));
        assert!(s.settle_if_quiet(900_101));
    }
    #[test]
    fn timeout_lengthens_the_interval_without_certifying_missing_feedback() {
        let mut s=Schedule::new(RefreshPolicy::default());s.activity(0);s.completed();
        let allowance=s.allowance();
        assert_eq!(s.timeout(),30_000);assert_eq!(s.allowance(),allowance);
        assert_eq!(s.timeout(),60_000);assert_eq!(s.timeout(),120_000);
        assert_eq!(s.timeout(),240_000);
        s.activity(400_000);assert_eq!(s.interval(),10_000);
    }
    #[test]
    fn new_activity_defers_settlement_but_query_replies_do_not() {
        let mut s=Schedule::new(RefreshPolicy::default());
        s.activity(0);
        for at in [10_000,30_000,60_000] {s.completed();assert!(!s.settle_if_quiet(at));}
        s.activity(250_000);
        assert!(!s.settle_if_quiet(300_000));
        assert!(s.settle_if_quiet(550_000));
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
