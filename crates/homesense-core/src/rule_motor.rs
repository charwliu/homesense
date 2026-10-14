//! Explicit motor predicate channel, separate from actual light feedback.
use crate::{
    DeviceId,
    curtain_state::{Movement, Provenance, Scope},
    rule::{Action, Condition, Node, Truth},
};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Moving,
    Stopped,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Evidence {
    #[default]
    ActualOnly,
    ReviewedCommandEstimate,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Predicate {
    pub device: DeviceId,
    pub state: State,
    pub evidence: Evidence,
    pub max_age_ms: u64,
    pub mapping_review: String,
    pub evidence_review: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub source: String,
    pub predicate: Predicate,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reading {
    pub request: Request,
    pub scope: Scope,
    pub captured_ms: u64,
    pub movement: Option<Movement>,
    pub provenance: Option<Provenance>,
    pub pending: bool,
}
#[derive(Clone, Copy)]
pub struct Snapshot<'a> {
    pub readings: &'a [Reading],
    pub scope: Option<Scope>,
    pub now_ms: u64,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Read {
    pub request: Request,
    pub truth: Truth,
    pub provenance: Option<Provenance>,
    pub captured_ms: Option<u64>,
}
impl Snapshot<'_> {
    pub fn test(&self, request: Request) -> Read {
        let sample = self
            .readings
            .iter()
            .find(|s| s.request == request)
            .filter(|s| {
                self.scope == Some(s.scope)
                    && s.scope.boot != 0
                    && ![
                        s.scope.epochs.configuration,
                        s.scope.epochs.transport,
                        s.scope.epochs.control,
                    ]
                    .contains(&0)
                    && !s.pending
                    && request.predicate.max_age_ms > 0
                    && !request.predicate.mapping_review.trim().is_empty()
                    && (request.predicate.evidence == Evidence::ActualOnly
                        || request
                            .predicate
                            .evidence_review
                            .as_ref()
                            .is_some_and(|e| !e.trim().is_empty()))
                    && self
                        .now_ms
                        .checked_sub(s.captured_ms)
                        .is_some_and(|age| age < request.predicate.max_age_ms)
                    && matches!(
                        (s.provenance, request.predicate.evidence),
                        (Some(Provenance::Observed), _)
                            | (
                                Some(Provenance::Inferred),
                                Evidence::ReviewedCommandEstimate
                            )
                    )
            });
        let known = sample.and_then(|s| s.movement.map(|movement| (s, movement)));
        let truth = known.map_or(Truth::Unknown, |(_, movement)| {
            let matching = match request.predicate.state {
                State::Moving => matches!(movement, Movement::Opening | Movement::Closing),
                State::Stopped => movement == Movement::Stopped,
            };
            if matching { Truth::True } else { Truth::False }
        });
        Read {
            request,
            truth,
            provenance: known.and_then(|(s, _)| s.provenance),
            captured_ms: known.map(|(s, _)| s.captured_ms),
        }
    }
}
/// Full bounded branch scan. Disabled candidates need all references reviewed;
/// the caller later narrows a captured lease to actually consulted predicates.
pub fn requests(nodes: &[Node]) -> Result<Vec<Request>, &'static str> {
    fn condition(
        c: &Condition,
        source: &str,
        out: &mut Vec<Request>,
        left: &mut usize,
        depth: usize,
    ) -> Result<(), &'static str> {
        if depth > 64 || *left == 0 {
            return Err("motor predicate scan budget exceeded");
        }
        *left -= 1;
        match c {
            Condition::MotorState(predicate) => {
                let request = Request {
                    source: source.into(),
                    predicate: predicate.clone(),
                };
                if !out.contains(&request) {
                    out.push(request);
                }
                if out.len() > 256 {
                    return Err("motor predicate scope exceeds 256");
                }
            }
            Condition::Not(c) => condition(c, source, out, left, depth + 1)?,
            Condition::All(cs) | Condition::Any(cs) => {
                for c in cs {
                    condition(c, source, out, left, depth + 1)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    fn walk(
        nodes: &[Node],
        out: &mut Vec<Request>,
        left: &mut usize,
        depth: usize,
    ) -> Result<(), &'static str> {
        for node in nodes {
            if depth > 64 || *left == 0 {
                return Err("motor predicate scan budget exceeded");
            }
            *left -= 1;
            if let Action::If {
                condition: c,
                then_actions,
                else_actions,
            } = &node.action
            {
                condition(c, &node.source, out, left, depth + 1)?;
                walk(then_actions, out, left, depth + 1)?;
                walk(else_actions, out, left, depth + 1)?;
            }
        }
        Ok(())
    }
    let mut out = vec![];
    walk(nodes, &mut out, &mut 65536, 0)?;
    Ok(out)
}
