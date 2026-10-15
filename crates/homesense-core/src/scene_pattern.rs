//! Scene selection from a complete physical light On/Off pattern.
//!
//! Brightness is displayed separately and never distinguishes scene selections.
//! The caller must validate current boot/connection generation, freshness and
//! settled feedback before passing a known observation. This pure matcher does
//! not read desired values, persist a selection or produce device commands.
use crate::{DeviceId, Percent};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LightPower {
    Off,
    On,
}
impl From<bool> for LightPower {
    fn from(on: bool) -> Self {
        if on { Self::On } else { Self::Off }
    }
}
impl From<Percent> for LightPower {
    fn from(level: Percent) -> Self {
        Self::from(level.get() > 0)
    }
}

pub type MemberPatterns = BTreeMap<i64, BTreeMap<DeviceId, LightPower>>;
pub type Observations = BTreeMap<DeviceId, Option<LightPower>>;

/// Returns 0 for a complete known pattern selecting no scene (including all-Off),
/// a positive member for a unique complete match, or None for unknown, missing,
/// inconsistent or ambiguous patterns. Zero does not assert that all lamps are Off.
///
/// Every member must explicitly cover the same reviewed group light set.
/// Missing/unknown lamps never supply an implicit Off. Extra observations must
/// be scoped out by the caller rather than silently enlarging the scene group.
pub fn matching_member(targets: &MemberPatterns, actual: &Observations) -> Option<i64> {
    let first = targets.values().next()?;
    if first.is_empty()
        || actual.keys().ne(first.keys())
        || actual.values().any(Option::is_none)
        || targets
            .iter()
            .any(|(value, vector)| *value <= 0 || vector.keys().ne(first.keys()))
    {
        return None;
    }
    let mut matches = targets.iter().filter(|(_, expected)| {
        expected
            .iter()
            .all(|(id, power)| actual.get(id) == Some(&Some(*power)))
    });
    let first_match = matches.next();
    if matches.next().is_some() {
        return None;
    }
    // Zero means no selected scene, not a claim that every lamp is Off.
    // Preserve the all-Off reset, including a unique explicit Off member.
    if actual.values().all(|power| *power == Some(LightPower::Off)) {
        return Some(0);
    }
    Some(first_match.map_or(0, |(value, _)| *value))
}
