//! Compile a HAP target into a module command before queueing or journaling.
//! All light targets use immediate percentages; no runtime adapter emits a
//! native hardware ramp, including commands from older scene definitions.
use homesense_core::{Command, LevelTransition, ModuleSpec};

pub(crate) fn plan(_module: &ModuleSpec, target: Command) -> Result<Command, &'static str> {
    match target {
        Command::Switch(_) => Ok(target),
        Command::SetLevel {
            level,
            transition: LevelTransition::Immediate,
        } => {
            let transition = LevelTransition::Immediate;
            Ok(Command::SetLevel { level, transition })
        }
        _ => Err("HAP light target required"),
    }
}

/// Normalize legacy configured transitions before encoding and journaling.
/// Profile/channel/context validation remains in the admission worker.
pub(crate) fn plan_automation(module:&ModuleSpec,target:Command)->Result<Command,&'static str> {
    if module.kind()==homesense_core::ModuleKind::Dimmer
        && let Command::SetLevel { level, .. } = target
    {
        return Ok(Command::SetLevel {
            level,
            transition: LevelTransition::Immediate,
        });
    }
    plan(module,target)
}
