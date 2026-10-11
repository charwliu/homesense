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
