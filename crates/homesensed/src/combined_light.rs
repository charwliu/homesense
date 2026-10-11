//! Pure planning for a dimmer with a separate supply relay.
//! Callers must validate physical bindings and atomically admit the entire plan.
//! A plan is not an atomic bus transaction or proof of a physical state.
use homesense_core::{Command, LevelTransition, Percent};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Output {
    Relay,
    Dimmer,
}

/// Input is an already normalized light intent. In particular, same-request
/// On=false and Brightness=0 must have been resolved before calling this API.
pub fn plan(
    target: Command,
    confirmed_restore: Option<Percent>,
    default: Option<Percent>,
) -> Result<Vec<(Output, Command)>, &'static str> {
    let level = match target {
        Command::Switch(false) => return Ok(vec![(Output::Relay, Command::Switch(false))]),
        Command::Switch(true) => confirmed_restore
            .filter(|v| v.get() != 0)
            .or(default.filter(|v| v.get() != 0))
            .ok_or("missing nonzero restore level")?,
        Command::SetLevel {
            level,
            transition: LevelTransition::Immediate,
        } => level,
        _ => return Err("normalized light target required"),
    };
    if level.get() == 0 {
        // Keep the last confirmed nonzero dimmer output for the next activation.
        return Ok(vec![(Output::Relay, Command::Switch(false))]);
    }
    Ok(vec![
        (
            Output::Dimmer,
            Command::SetLevel {
                level,
                transition: if level.get() == 100 {
                    LevelTransition::Immediate
                } else {
                    LevelTransition::Ramp
                },
            },
        ),
        (Output::Relay, Command::Switch(true)),
    ])
}

/// Both inputs must belong to the current configuration/transport generation and
/// satisfy freshness checks. Missing either feedback makes the combined state
/// unavailable, even if the other value alone appears to imply off.
pub fn observed(relay: Option<bool>, dimmer: Option<Percent>) -> Option<(bool, Percent)> {
    let relay = relay?;
    let level = dimmer?;
    Some((relay && level.get() != 0, level))
}

/// Project physical feedback into logical lights without mutating the source.
/// The logical dimmer retains its stable ID; its supply relay is hidden. The
/// older timestamp governs freshness, so a fresh dimmer cannot conceal a stale
/// relay. Restore-level persistence must consume the physical snapshot instead.
pub fn project(
    configuration: &homesense_config::Configuration,
    physical: &homesense_state::Snapshot,
) -> Result<homesense_state::Snapshot, &'static str> {
    use homesense_core::{ModuleKind, Observation};
    use homesense_state::Quality;
    let mut logical = physical.clone();
    for (id, device) in configuration.devices() {
        if !device.enabled {
            continue;
        }
        let Some(relay_id) = &device.supply_relay else {
            continue;
        };
        let relay = physical
            .devices
            .get(relay_id)
            .ok_or("missing supply feedback")?;
        let dimmer = physical.devices.get(id).ok_or("missing dimmer feedback")?;
        if relay.kind != ModuleKind::Switch || dimmer.kind != ModuleKind::Dimmer {
            return Err("combined feedback kind mismatch");
        }
        let state = logical.devices.get_mut(id).ok_or("missing logical light")?;
        state.observed_at_ms = relay
            .observed_at_ms
            .zip(dimmer.observed_at_ms)
            .map(|(a, b)| a.min(b));
        state.version = relay.version.max(dimmer.version);
        // Physical intent and completion IDs are not a combined transaction.
        state.desired = None;
        state.pending_command = None;
        state.last_command_id = None;
        state.quality = match (relay.quality, dimmer.quality) {
            (Quality::Confirmed, Quality::Confirmed) => Quality::Confirmed,
            (Quality::Unknown, _) | (_, Quality::Unknown) => Quality::Unknown,
            _ => Quality::Stale,
        };
        state.observed = match (relay.observed, dimmer.observed) {
            (Some(Observation::Switch(on)), Some(Observation::Level(level))) => {
                Some(Observation::Level(if on {
                    level
                } else {
                    Percent::new(0).expect("zero percent")
                }))
            }
            _ => {
                state.quality = Quality::Unknown;
                None
            }
        };
        logical.devices.remove(relay_id);
    }
    Ok(logical)
}
