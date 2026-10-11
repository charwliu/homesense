//! One correlation chain owns cumulative budgets across sibling and nested rules.
//! Tokens carry lineage; the runtime must persist admission separately and must
//! not use planning success as proof of execution.
use crate::rule::{self, Budget, Node, Plan, Variable};
use std::collections::{BTreeMap,BTreeSet};
#[derive(Debug, Clone)]
pub struct Cause {
    correlation: u128,
    id: u64,
    path: Vec<String>,
}
impl Cause {
    pub fn correlation_id(&self) -> u128 {
        self.correlation
    }
    pub fn event_id(&self) -> u64 {
        self.id
    }
}
#[derive(Debug)]
pub struct PlannedRun {
    pub cause: Cause,
    pub causation_id: u64,
    pub plan: Plan,
}
#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    InvalidLimits,
    ForeignCause,
    Cycle,
    Budget,
    Clock,
    Stopped,
    Rule(rule::Error),
    Planning(String),
}
pub struct Chain {
    correlation: u128,
    next: u64,
    now: u64,
    deadline: u64,
    remaining: Budget,
    max_path: usize,
    stopped: bool,
    completions: BTreeSet<String>,
}
impl Chain {
    pub fn new(
        correlation: u128,
        now: u64,
        duration_ms: u64,
        budget: Budget,
        max_path: usize,
    ) -> Result<Self, Error> {
        if correlation == 0
            || duration_ms == 0
            || duration_ms > 60_000
            || max_path == 0
            || max_path > 64
            || budget.nodes == 0
            || budget.nodes > 65536
            || budget.commands > 4096
            || budget.depth > 64
        {
            return Err(Error::InvalidLimits);
        }
        Ok(Self {
            correlation,
            next: 1,
            now,
            deadline: now.checked_add(duration_ms).ok_or(Error::Clock)?,
            remaining: budget,
            max_path,
            stopped: false,
            completions: BTreeSet::new(),
        })
    }
    pub fn deadline_ms(&self) -> u64 { self.deadline }
    pub fn root(&self) -> Cause {
        Cause {
            correlation: self.correlation,
            id: 0,
            path: vec![],
        }
    }
    pub fn cancel(&mut self) {
        self.stopped = true;
    }
    /// Claim one persisted asynchronous result without resetting this chain's
    /// deadline, ancestry or budgets. Duplicate delivery is a no-op.
    pub fn claim_completion(&mut self,parent:&Cause,key:&str,now:u64)->Result<bool,Error> {
        if self.stopped {return Err(Error::Stopped);}
        if parent.correlation!=self.correlation || parent.id==0 || parent.id>=self.next {return Err(Error::ForeignCause);}
        if key.is_empty() || key.len()>128 || key.chars().any(char::is_control) {return Err(Error::InvalidLimits);}
        if now<self.now {self.stopped=true;return Err(Error::Clock);}
        self.now=now;
        if now>=self.deadline {self.stopped=true;return Err(Error::Budget);}
        if self.completions.contains(key) {return Ok(false);}
        if self.remaining.nodes==0 {self.stopped=true;return Err(Error::Budget);}
        self.remaining.nodes-=1;
        self.completions.insert(key.into());
        Ok(true)
    }
    pub fn plan(
        &mut self,
        parent: &Cause,
        rule_id: &str,
        nodes: &[Node],
        variables: &BTreeMap<String, Variable>,
        now: u64,
    ) -> Result<PlannedRun, Error> {
        self.plan_using(parent, rule_id, now, |budget| {
            rule::evaluate(nodes, variables, budget).map_err(Error::Rule)
        })
    }
    pub fn plan_using(
        &mut self,
        parent: &Cause,
        rule_id: &str,
        now: u64,
        planner: impl FnOnce(Budget) -> Result<Plan, Error>,
    ) -> Result<PlannedRun, Error> {
        if self.stopped {
            return Err(Error::Stopped);
        }
        if parent.correlation != self.correlation || parent.id >= self.next {
            return Err(Error::ForeignCause);
        }
        if now < self.now {
            self.stopped = true;
            return Err(Error::Clock);
        }
        self.now = now;
        if now >= self.deadline || parent.path.len() >= self.max_path || self.remaining.nodes == 0 {
            self.stopped = true;
            return Err(Error::Budget);
        }
        if rule_id.is_empty() || parent.path.iter().any(|id| id == rule_id) {
            self.stopped = true;
            return Err(Error::Cycle);
        }
        // Charge dispatch itself, including empty rules, so wide fan-out is bounded.
        self.remaining.nodes -= 1;
        let plan = match planner(self.remaining) {
            Ok(plan) => plan,
            Err(error) => {
                self.stopped = true;
                return Err(error);
            }
        };
        if plan.visited_nodes > self.remaining.nodes || plan.action_cost > self.remaining.commands {
            self.stopped = true;
            return Err(Error::Budget);
        }
        self.remaining.nodes -= plan.visited_nodes;
        self.remaining.commands -= plan.action_cost;
        let mut path = parent.path.clone();
        path.push(rule_id.to_owned());
        let cause = Cause {
            correlation: self.correlation,
            id: self.next,
            path,
        };
        self.next += 1;
        Ok(PlannedRun {
            cause,
            causation_id: parent.id,
            plan,
        })
    }
}
