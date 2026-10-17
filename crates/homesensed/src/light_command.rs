//! Compile a HAP target into a module command before queueing or journaling.
//! HAP brightness has no transition-duration parameter. AD8 uses its configured
//! hardware ramp for intermediate targets; endpoint on/off remains immediate.
use homesense_core::{Command, LevelTransition, ModuleSpec, ProtocolProfile};

pub(crate) fn plan(module: &ModuleSpec, target: Command) -> Result<Command, &'static str> {
    match target {
        Command::Switch(_) => Ok(target),
        Command::SetLevel {
            level,
            transition: LevelTransition::Immediate,
        } => {
            let transition =
                if module.profile() == ProtocolProfile::Ad8Din && !matches!(level.get(), 0 | 100) {
                    LevelTransition::Ramp
                } else {
                    LevelTransition::Immediate
                };
            Ok(Command::SetLevel { level, transition })
        }
        _ => Err("HAP light target required"),
    }
}

/// Configured automation may explicitly request a native hardware ramp. AD8
/// endpoints always use direct off/full-on; keep intermediate ramps intact.
/// The admission worker still performs the normal
/// verified-profile/channel/context encoding check before any durable queueing.
/// Manual HAP targets continue through `plan` and cannot select this transition.
pub(crate) fn plan_automation(module:&ModuleSpec,target:Command)->Result<Command,&'static str> {
    if module.profile() == ProtocolProfile::Ad8Din
        && let Command::SetLevel { level, .. } = target
        && matches!(level.get(), 0 | 100)
    {
        return Ok(Command::SetLevel {
            level,
            transition: LevelTransition::Immediate,
        });
    }
    if module.kind()==homesense_core::ModuleKind::Dimmer
        && matches!(target,Command::SetLevel{transition:LevelTransition::Ramp,..}) {
        Ok(target)
    }else{plan(module,target)}
}
