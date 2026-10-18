//! Scene selection from a complete physical light On/Off pattern.
//!
//! The default matcher uses power alone. The explicit collision refinement
//! below reads brightness only when multiple members share one power vector.
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

/// Observation scope and actuator scope are deliberately separate. Sparse
/// members still require fresh observations for the entire reviewed union.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatternScope {
    Complete,
    PreserveOmissions,
}

/// Whether power alone always distinguishes the reviewed members. Different
/// sparse maps can still overlap; only a contradictory shared target separates
/// two candidates. This is also used to choose the relay-only LED barrier.
pub fn power_patterns_disjoint(targets: &MemberPatterns, scope: PatternScope) -> bool {
    let Some(first) = targets.values().next() else { return false; };
    if targets.iter().any(|(value, vector)| *value <= 0 || vector.is_empty()
        || (scope == PatternScope::Complete && vector.keys().ne(first.keys()))) { return false; }
    targets.values().enumerate().all(|(index, vector)| targets.values().skip(index + 1)
        .all(|other| vector.iter().any(|(id, power)| other.get(id).is_some_and(|p| p != power))))
}

fn scoped_candidates(targets: &MemberPatterns, actual: &Observations, scope: PatternScope) -> Option<Vec<i64>> {
    if scope == PatternScope::Complete { return power_candidates(targets, actual); }
    let union = targets.values().flat_map(|v| v.keys()).collect::<std::collections::BTreeSet<_>>();
    if union.is_empty() || actual.keys().ne(union) || actual.values().any(Option::is_none)
        || targets.iter().any(|(value, vector)| *value <= 0 || vector.is_empty()) { return None; }
    Some(targets.iter().filter(|(_, vector)| vector.iter().all(|(id, power)| actual.get(id) == Some(&Some(*power)))).map(|(value, _)| *value).collect())
}

/// Explicit sparse matching never treats an omitted target as Off, nor an
/// omitted/missing observation as evidence. Existing complete matching is kept.
pub fn matching_member_scoped(targets: &MemberPatterns, actual: &Observations, scope: PatternScope) -> Option<i64> {
    if scope == PatternScope::Complete { return matching_member(targets, actual); }
    let candidates = scoped_candidates(targets, actual, scope)?;
    if actual.values().all(|power| *power == Some(LightPower::Off)) { return Some(0); }
    match candidates.as_slice() { [] => Some(0), [value] => Some(*value), _ => None }
}

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

/// Target and feedback percentages in a profile's established readback domain.
/// Callers must not round arbitrary source units or substitute remembered levels.
pub type MemberLevels = BTreeMap<i64, BTreeMap<DeviceId, Percent>>;
pub type LevelObservations = BTreeMap<DeviceId, Option<Percent>>;

/// Only shared, explicitly specified levels can distinguish sparse members.
/// A level omitted by any candidate cannot become an implicit zero or a read.
pub fn brightness_reads_for_collision_scoped(targets: &MemberPatterns, levels: &MemberLevels, actual: &Observations, scope: PatternScope) -> Option<std::collections::BTreeSet<DeviceId>> {
    if scope == PatternScope::Complete { return brightness_reads_for_collision(targets, levels, actual); }
    let candidates = scoped_candidates(targets, actual, scope)?;
    if actual.values().all(|power| *power == Some(LightPower::Off)) || candidates.len() < 2 { return Some(Default::default()); }
    for value in &candidates {
        let vector = levels.get(value)?;
        if vector.iter().any(|(id, level)| targets[value].get(id) != Some(&LightPower::from(*level))) { return None; }
    }
    let first = levels.get(candidates.first()?)?;
    let reads = first.iter().filter(|(id, level)| candidates.iter().all(|v| levels[v].contains_key(*id))
        && candidates.iter().any(|v| levels[v].get(*id) != Some(*level))).map(|(id, _)| id.clone()).collect::<std::collections::BTreeSet<_>>();
    (!reads.is_empty()).then_some(reads)
}

pub fn matching_member_with_levels_scoped(targets: &MemberPatterns, levels: &MemberLevels, actual: &Observations, actual_levels: &LevelObservations, scope: PatternScope) -> Option<i64> {
    if scope == PatternScope::Complete { return matching_member_with_levels(targets, levels, actual, actual_levels); }
    let candidates = scoped_candidates(targets, actual, scope)?;
    if actual.values().all(|power| *power == Some(LightPower::Off)) { return Some(0); }
    if candidates.len() < 2 { return Some(candidates.first().copied().unwrap_or(0)); }
    let reads = brightness_reads_for_collision_scoped(targets, levels, actual, scope)?;
    if actual_levels.keys().ne(reads.iter()) || actual_levels.values().any(Option::is_none) { return None; }
    let mut matches = candidates.into_iter().filter(|value| reads.iter().all(|id| actual_levels.get(id) == Some(&Some(levels[value][id]))));
    let first = matches.next()?;
    matches.next().is_none().then_some(first)
}

fn power_candidates(targets: &MemberPatterns, actual: &Observations) -> Option<Vec<i64>> {
    let first=targets.values().next()?;
    if first.is_empty() || actual.keys().ne(first.keys()) || actual.values().any(Option::is_none)
        || targets.iter().any(|(value,vector)|*value<=0 || vector.keys().ne(first.keys())) {return None;}
    Some(targets.iter().filter(|(_,expected)|expected.iter().all(|(id,power)|actual.get(id)==Some(&Some(*power)))).map(|(value,_)|*value).collect())
}
/// Only dimmers which distinguish the currently colliding power candidates are
/// read. An empty set means the power vector alone decides; None means missing
/// power or malformed/insufficient level targets. All-Off always needs no level.
pub fn brightness_reads_for_collision(targets:&MemberPatterns, levels:&MemberLevels, actual:&Observations)->Option<std::collections::BTreeSet<DeviceId>> {
    let candidates=power_candidates(targets,actual)?;
    if actual.values().all(|power|*power==Some(LightPower::Off)) || candidates.len()<2 {return Some(Default::default());}
    let first=levels.get(candidates.first()?)?;
    if candidates.iter().any(|value|levels.get(value).is_none_or(|vector|vector.keys().ne(first.keys()) || vector.iter().any(|(id,level)|targets[value].get(id)!=Some(&LightPower::from(*level))))) {return None;}
    let reads=first.iter().filter(|(id,level)|candidates.iter().any(|value|levels[value].get(*id)!=Some(*level))).map(|(id,_)|id.clone()).collect::<std::collections::BTreeSet<_>>();
    (!reads.is_empty()).then_some(reads)
}
/// Opt-in refinement: unique On/Off patterns ignore brightness, including a
/// nonmatching level. Only a collision uses exact, fresh profile readback values.
/// Missing or nonunique level evidence never selects a candidate by order.
pub fn matching_member_with_levels(targets:&MemberPatterns, levels:&MemberLevels, actual:&Observations, actual_levels:&LevelObservations)->Option<i64> {
    let candidates=power_candidates(targets,actual)?;
    if actual.values().all(|power|*power==Some(LightPower::Off)){return Some(0);}
    if candidates.len()<2{return Some(candidates.first().copied().unwrap_or(0));}
    let reads=brightness_reads_for_collision(targets,levels,actual)?;
    if actual_levels.keys().ne(reads.iter()) || actual_levels.values().any(Option::is_none){return None;}
    let mut matches=candidates.into_iter().filter(|value|reads.iter().all(|id|actual_levels.get(id)==Some(&Some(levels[value][id]))));
    let first=matches.next()?;
    matches.next().is_none().then_some(first)
}
