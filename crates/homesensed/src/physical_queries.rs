//! Complete registry output-read scope, independent of logical device bindings.
use homesense_config::Configuration;
use homesense_core::{Channel, Command, ExecutionContext, ModuleKind, ModuleSpec};

#[derive(Debug, Clone)]
pub(crate) struct Target {
    pub gateway: String,
    pub module: ModuleSpec,
    pub channels: Vec<u8>,
    pub whole: bool,
}
impl Target {
    pub fn first_channel(&self) -> Channel {
        Channel::new(self.channels[0]).expect("validated physical query channels")
    }
    pub fn fresh(&self, truth: &crate::module_truth::ModuleTruth, now: u64, max_age: u64) -> bool {
        self.channels.iter().all(|channel| {
            truth
                .located_state(&self.gateway, self.module.address().get(), *channel)
                .is_some_and(|state| {
                    state.quality == homesense_state::Quality::Confirmed
                        && state.observed.is_some()
                        && state
                            .observed_at_ms
                            .and_then(|at| now.checked_sub(at))
                            .is_some_and(|age| age < max_age)
                })
        })
    }
}
pub(crate) fn plan(
    config: &Configuration,
    context: ExecutionContext,
) -> Result<Vec<Target>, String> {
    let mut targets = vec![];
    for (gateway, g) in config.gateways() {
        for module in g.modules().values() {
            if !matches!(module.kind(), ModuleKind::Switch | ModuleKind::Dimmer) {
                continue;
            }
            module
                .validate_context(context)
                .map_err(|e| e.to_string())?;
            if hb_protocol::supports_module_query(module.profile()) {
                hb_protocol::encode_module_query(module, context).map_err(|e| e.to_string())?;
                targets.push(Target {
                    gateway: gateway.clone(),
                    module: module.clone(),
                    channels: (1..=module.channels()).collect(),
                    whole: true,
                });
            } else {
                for channel in 1..=module.channels() {
                    hb_protocol::encode_in_context(
                        module,
                        Channel::new(channel).map_err(|e| e.to_string())?,
                        Command::Query,
                        context,
                    )
                    .map_err(|e| e.to_string())?;
                    targets.push(Target {
                        gateway: gateway.clone(),
                        module: module.clone(),
                        channels: vec![channel],
                        whole: false,
                    });
                }
            }
        }
    }
    if targets.is_empty() {
        return Err("registered queryable output modules required".into());
    }
    Ok(targets)
}
pub(crate) fn interval(targets: &[Target], gap: u64, max_age: u64) -> Result<u64, String> {
    if targets.is_empty() || max_age == 0 {
        return Err("invalid physical refresh scope".into());
    }
    let interval = (max_age / 2 / targets.len() as u64)
        .max(1)
        .max(gap.saturating_mul(2));
    if interval.saturating_mul(targets.len() as u64) >= max_age {
        return Err("registered physical query scope exceeds freshness and wire capacity".into());
    }
    Ok(interval)
}

/// Queries within an adaptive round are fast; idle applies only between rounds.
/// Explicit tighter standalone API bounds retain their original polling budget.
pub(crate) fn policy_interval(targets:&[Target],gap:u64,max_age:u64,
    policy:homesense_config::RefreshPolicy)->Result<u64,String> {
    if max_age!=policy.observation_max_age_ms() {return interval(targets,gap,max_age);}
    if targets.is_empty() {return Err("invalid physical refresh scope".into());}
    let slot=gap.saturating_mul(2).max(1);
    if slot.saturating_mul(targets.len() as u64)>=policy.round_budget_ms {
        return Err("physical query scope exceeds adaptive round wire capacity".into());
    }
    Ok(slot)
}
