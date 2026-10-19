//! Poll replies are not activity. Observation time is never renewed by waiting.
use homesense_config::RefreshPolicy;
pub(crate) struct Schedule {
    policy: RefreshPolicy,
    interval: u64,
    quiet: u32,
    allowance: u64,
    last_activity: Option<u64>,
}
impl Schedule {
    pub fn new(policy: RefreshPolicy) -> Self {
        Self { policy, interval:policy.active_interval_ms, quiet:0,
            allowance:policy.observation_max_age_ms(), last_activity:None }
    }
    /// Keep published older configurations compatible while applying the live
    /// safety bounds. Simulation can still use explicit short virtual clocks.
    pub fn for_context(mut policy:RefreshPolicy, context:homesense_core::ExecutionContext)->Self {
        if context==homesense_core::ExecutionContext::Live {
            policy.active_interval_ms=policy.active_interval_ms.clamp(300_000,7_200_000);
            policy.idle_interval_ms=policy.idle_interval_ms.clamp(policy.active_interval_ms,7_200_000);
            policy.quiet_rounds_per_step=1;
        }
        Self::new(policy)
    }
    pub fn interval(&self)->u64 { self.interval }
    pub fn allowance(&self)->u64 { self.allowance }
    pub fn activity(&mut self,now:u64) {
        self.last_activity=Some(now);self.interval=self.policy.active_interval_ms;self.quiet=0;
    }
    fn double(&mut self) {
        self.interval=self.interval.saturating_mul(2).min(self.policy.idle_interval_ms);
    }
    /// A failed round backs off without certifying or restamping missing data.
    pub fn timeout(&mut self)->u64 {
        self.double();self.quiet=0;self.interval
    }
    pub fn completed(&mut self,now:u64)->u64 {
        // Activity inside an unfinished scan resets the next cycle. Activity
        // that began an already elapsed quiet cycle does not buy another cycle.
        if self.last_activity.is_some_and(|at|now.saturating_sub(at)<self.policy.active_interval_ms) {
            self.interval=self.policy.active_interval_ms;self.quiet=0;
        } else {
            self.quiet+=1;
            if self.quiet>=self.policy.quiet_rounds_per_step {self.double();self.quiet=0;}
        }
        self.allowance=self.policy.age_for_interval(self.interval);self.interval
    }
}
/// Pre-receive candidate only: activity requires the unified decoder to advance
/// the same registered port's count after apply, under the current wire epoch.
pub(crate) fn candidate(truth:&crate::module_truth::ModuleTruth,gateway:&str,frame:&hb_protocol::RawFrame)->Option<(u8,u8,u64)>{
    let channel=frame.command.checked_sub(0xd0)?;let state=truth.received_port_state(gateway,frame.address,channel)?;
    let allowed=state.kind==homesense_core::ModuleKind::Keypad && (1..=6).contains(&channel)
        && matches!(frame.value,0xf1|0xf2) && truth.module_objects().iter().any(|m|
            m.gateway==gateway && m.spec.address().get()==frame.address
                && m.spec.profile()==homesense_core::ProtocolProfile::Ts686Rs485SixKey);
    allowed.then(||truth.port_observation_count(gateway,frame.address,channel).map(|count|(frame.address,channel,count))).flatten()
}


#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quiet_cycles_double_from_five_minutes_to_two_hours() {
        let mut schedule=Schedule::new(RefreshPolicy::default());
        assert_eq!(schedule.interval(),300_000);
        let mut now=0;
        for next in [600_000,1_200_000,2_400_000,4_800_000,7_200_000,7_200_000] {
            now+=schedule.interval();assert_eq!(schedule.completed(now),next);
        }
    }
    #[test]
    fn command_or_panel_activity_resets_cycle_without_restarting_a_scan() {
        let mut schedule=Schedule::new(RefreshPolicy::default());
        assert_eq!(schedule.completed(300_000),600_000);
        schedule.activity(400_000);assert_eq!(schedule.interval(),300_000);
        assert_eq!(schedule.completed(400_100),300_000,"finish the old scan, then wait a full reset interval");
        assert_eq!(schedule.completed(700_100),600_000,"a whole quiet reset cycle doubles normally");
        schedule.activity(800_000);assert_eq!(schedule.interval(),300_000);
    }
    #[test]
    fn timeout_doubles_without_forging_observation_freshness() {
        let mut schedule=Schedule::new(RefreshPolicy::default());let allowance=schedule.allowance();
        for next in [600_000,1_200_000,2_400_000,4_800_000,7_200_000,7_200_000] {
            assert_eq!(schedule.timeout(),next);assert_eq!(schedule.allowance(),allowance);
        }
        schedule.activity(100);assert_eq!(schedule.interval(),300_000);
    }
    #[test]
    fn old_live_policy_is_clamped_without_modifying_the_published_document() {
        let old=RefreshPolicy{active_interval_ms:10_000,idle_interval_ms:10_800_000,quiet_rounds_per_step:2,..Default::default()};
        let mut schedule=Schedule::for_context(old,homesense_core::ExecutionContext::Live);
        assert_eq!(schedule.interval(),300_000);assert_eq!(schedule.allowance(),7_215_000);
        assert_eq!(schedule.completed(300_000),600_000);
        for _ in 0..10 {schedule.timeout();}assert_eq!(schedule.interval(),7_200_000);
        assert_eq!(old.active_interval_ms,10_000);
        let short=RefreshPolicy{active_interval_ms:100,idle_interval_ms:1000,backoff_ms:[200,500],quiet_rounds_per_step:1,..Default::default()};
        assert_eq!(Schedule::for_context(short,homesense_core::ExecutionContext::Simulation).interval(),100);
    }
}
