//! Pure, bounded rule planning against one immutable input snapshot. A plan is
//! not an admission or a physical action. Its variable delta must be committed
//! atomically with command admission by the runtime adapter.
use crate::{Command, DeviceId};
use std::collections::{BTreeMap, BTreeSet};
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Bool(bool),
    Integer(i64),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lifetime {
    Persistent,
    Temporary,
    FeedbackCache,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueType {
    Bool,
    Integer,
}
impl Value {
    fn value_type(&self) -> ValueType {
        match self {
            Self::Bool(_) => ValueType::Bool,
            Self::Integer(_) => ValueType::Integer,
        }
    }
}
#[derive(Debug, Clone)]
pub struct Variable {
    pub value_type: ValueType,
    pub lifetime: Lifetime,
    pub value: Option<Value>,
}
#[derive(Debug, Clone)]
pub enum Expr {
    Literal(Value),
    Variable(String),
    DeviceOn(DeviceId),
    DeviceLevel(DeviceId),
    Add(Box<Expr>, Box<Expr>),
    Subtract(Box<Expr>, Box<Expr>),
}
#[derive(Debug, Clone)]
pub enum Condition {
    Equal(Expr, Expr),
    Less(Expr, Expr),
    Not(Box<Condition>),
    All(Vec<Condition>),
    Any(Vec<Condition>),
    Unknown(Expr),
}
#[derive(Debug, Clone)]
pub enum Action {
    Scene {
        id: String,
        activate: bool,
    },
    Set {
        variable: String,
        value: Expr,
    },
    Command {
        device: DeviceId,
        command: Command,
    },
    If {
        condition: Condition,
        then_actions: Vec<Node>,
        else_actions: Vec<Node>,
    },
}
#[derive(Debug, Clone)]
pub struct Node {
    pub source: String,
    pub action: Action,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Truth {
    True,
    False,
    Unknown,
}
#[derive(Debug, Clone, Copy)]
pub struct Budget {
    pub nodes: usize,
    pub depth: usize,
    pub commands: usize,
}
impl Default for Budget {
    fn default() -> Self {
        Self {
            nodes: 1024,
            depth: 32,
            commands: 128,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Budget,
    UndefinedVariable(String),
    TypeMismatch,
    ReadOnlyVariable,
    MissingSource,
    ArithmeticOverflow,
}
#[derive(Debug)]
pub struct PlannedCommand {
    pub source: String,
    pub device: DeviceId,
    pub command: Command,
}
#[derive(Debug)]
pub struct Plan {
    /// Devices actually consulted, including missing/unknown feedback.
    pub feedback_reads: BTreeSet<DeviceId>,
    pub visited_nodes: usize,
    pub action_cost: usize,
    /// Executed leaf actions, in source order, for domain-specific admission.
    pub actions: Vec<Node>,
    pub variables: BTreeMap<String, Variable>,
    pub commands: Vec<PlannedCommand>,
    pub unknown_branches: Vec<String>,
}
/// Actual feedback only. Callers must exclude pending commands and observations
/// from an earlier connection/configuration generation before building this map.
#[derive(Debug, Clone)]
pub struct Feedback {
    pub on: Option<bool>,
    pub level: Option<crate::Percent>,
    pub observed_ms: u64,
}
pub struct FeedbackSnapshot<'a> {
    pub devices: &'a BTreeMap<DeviceId, Feedback>,
    pub now_ms: u64,
    pub max_age_ms: u64,
}
struct Evaluator<'a> {
    feedback: FeedbackSnapshot<'a>,
    remaining: Budget,
    plan: Plan,
}
impl Evaluator<'_> {
    fn spend(&mut self, depth: usize) -> Result<(), Error> {
        if self.remaining.nodes == 0 || depth > self.remaining.depth {
            return Err(Error::Budget);
        }
        self.remaining.nodes -= 1;
        Ok(())
    }
    fn expression(&mut self, value: &Expr, depth: usize) -> Result<Option<Value>, Error> {
        self.spend(depth)?;
        match value {
            Expr::Add(a, b) | Expr::Subtract(a, b) => {
                match (
                    self.expression(a, depth + 1)?,
                    self.expression(b, depth + 1)?,
                ) {
                    (Some(Value::Integer(a)), Some(Value::Integer(b))) => {
                        let v = if matches!(value, Expr::Add(..)) {
                            a.checked_add(b)
                        } else {
                            a.checked_sub(b)
                        };
                        Ok(Some(Value::Integer(v.ok_or(Error::ArithmeticOverflow)?)))
                    }
                    (Some(Value::Bool(_)), _) | (_, Some(Value::Bool(_))) => {
                        Err(Error::TypeMismatch)
                    }
                    _ => Ok(None),
                }
            }
            Expr::DeviceOn(device) | Expr::DeviceLevel(device) => {
                self.plan.feedback_reads.insert(device.clone());
                let sample = self.feedback.devices.get(device).filter(|sample| {
                    self.feedback.max_age_ms > 0
                        && self
                            .feedback
                            .now_ms
                            .checked_sub(sample.observed_ms)
                            .is_some_and(|age| age < self.feedback.max_age_ms)
                });
                Ok(sample.and_then(|sample| match value {
                    Expr::DeviceOn(_) => sample.on.map(Value::Bool),
                    Expr::DeviceLevel(_) => sample
                        .level
                        .map(|level| Value::Integer(i64::from(level.get()))),
                    _ => unreachable!(),
                }))
            }
            Expr::Literal(value) => Ok(Some(value.clone())),
            Expr::Variable(name) => self
                .plan
                .variables
                .get(name)
                .map(|v| v.value.clone())
                .ok_or_else(|| Error::UndefinedVariable(name.clone())),
        }
    }
    fn condition(&mut self, c: &Condition, depth: usize) -> Result<Truth, Error> {
        self.spend(depth)?;
        Ok(match c {
            Condition::Less(a, b) => match (
                self.expression(a, depth + 1)?,
                self.expression(b, depth + 1)?,
            ) {
                (Some(Value::Integer(a)), Some(Value::Integer(b))) => {
                    if a < b {
                        Truth::True
                    } else {
                        Truth::False
                    }
                }
                (Some(Value::Bool(_)), _) | (_, Some(Value::Bool(_))) => {
                    return Err(Error::TypeMismatch);
                }
                _ => Truth::Unknown,
            },
            Condition::Equal(a, b) => match (
                self.expression(a, depth + 1)?,
                self.expression(b, depth + 1)?,
            ) {
                (Some(a), Some(b)) => {
                    if std::mem::discriminant(&a) != std::mem::discriminant(&b) {
                        return Err(Error::TypeMismatch);
                    }
                    if a == b { Truth::True } else { Truth::False }
                }
                _ => Truth::Unknown,
            },
            Condition::Unknown(e) => {
                if self.expression(e, depth + 1)?.is_none() {
                    Truth::True
                } else {
                    Truth::False
                }
            }
            Condition::Not(c) => match self.condition(c, depth + 1)? {
                Truth::True => Truth::False,
                Truth::False => Truth::True,
                Truth::Unknown => Truth::Unknown,
            },
            Condition::All(cs) | Condition::Any(cs) => {
                let all = matches!(c, Condition::All(_));
                let mut unknown = false;
                let mut decisive = false;
                // Evaluate all terms so invalid types/references are not hidden by short circuit.
                for term in cs {
                    let v = self.condition(term, depth + 1)?;
                    unknown |= v == Truth::Unknown;
                    decisive |= v == if all { Truth::False } else { Truth::True };
                }
                if decisive {
                    if all { Truth::False } else { Truth::True }
                } else if unknown {
                    Truth::Unknown
                } else if all {
                    Truth::True
                } else {
                    Truth::False
                }
            }
        })
    }
    fn nodes(&mut self, nodes: &[Node], depth: usize) -> Result<(), Error> {
        for node in nodes {
            self.spend(depth)?;
            if node.source.is_empty() {
                return Err(Error::MissingSource);
            }
            if !matches!(node.action, Action::If { .. }) {
                self.plan.actions.push(node.clone());
            }
            match &node.action {
                Action::Scene { id, .. } => {
                    if id.is_empty() {
                        return Err(Error::MissingSource);
                    }
                    if self.remaining.commands == 0 {
                        return Err(Error::Budget);
                    }
                    self.remaining.commands -= 1;
                }
                Action::Set { variable, value } => {
                    let value = self.expression(value, depth + 1)?;
                    let prior = self
                        .plan
                        .variables
                        .get_mut(variable)
                        .ok_or_else(|| Error::UndefinedVariable(variable.clone()))?;
                    if prior.lifetime == Lifetime::FeedbackCache {
                        return Err(Error::ReadOnlyVariable);
                    }
                    if let (Some(a), Some(b)) = (&prior.value, &value)
                        && std::mem::discriminant(a) != std::mem::discriminant(b)
                    {
                        return Err(Error::TypeMismatch);
                    }
                    if value
                        .as_ref()
                        .is_some_and(|v| v.value_type() != prior.value_type)
                    {
                        return Err(Error::TypeMismatch);
                    }
                    prior.value = value;
                }
                Action::Command { device, command } => {
                    if self.remaining.commands == 0 {
                        return Err(Error::Budget);
                    }
                    self.remaining.commands -= 1;
                    self.plan.commands.push(PlannedCommand {
                        source: node.source.clone(),
                        device: device.clone(),
                        command: *command,
                    });
                }
                Action::If {
                    condition,
                    then_actions,
                    else_actions,
                } => match self.condition(condition, depth + 1)? {
                    Truth::True => self.nodes(then_actions, depth + 1)?,
                    Truth::False => self.nodes(else_actions, depth + 1)?,
                    Truth::Unknown => self.plan.unknown_branches.push(node.source.clone()),
                },
            }
        }
        Ok(())
    }
}
pub fn evaluate(
    nodes: &[Node],
    variables: &BTreeMap<String, Variable>,
    budget: Budget,
) -> Result<Plan, Error> {
    evaluate_with_feedback(
        nodes,
        variables,
        budget,
        FeedbackSnapshot {
            devices: &BTreeMap::new(),
            now_ms: 0,
            max_age_ms: 0,
        },
    )
}

pub fn evaluate_with_feedback(
    nodes: &[Node],
    variables: &BTreeMap<String, Variable>,
    budget: Budget,
    feedback: FeedbackSnapshot<'_>,
) -> Result<Plan, Error> {
    // Hard caps also bound recursion when a caller provides an excessive budget.
    if budget.depth > 64 || budget.nodes > 65536 || budget.commands > 4096 {
        return Err(Error::Budget);
    }
    if variables.values().any(|v| {
        v.value
            .as_ref()
            .is_some_and(|value| value.value_type() != v.value_type)
    }) {
        return Err(Error::TypeMismatch);
    }
    let mut evaluator = Evaluator {
        feedback,
        remaining: budget,
        plan: Plan {
            feedback_reads: BTreeSet::new(),
            visited_nodes: 0,
            action_cost: 0,
            actions: vec![],
            variables: variables.clone(),
            commands: vec![],
            unknown_branches: vec![],
        },
    };
    evaluator.nodes(nodes, 0)?;
    evaluator.plan.visited_nodes = budget.nodes - evaluator.remaining.nodes;
    evaluator.plan.action_cost = budget.commands - evaluator.remaining.commands;
    Ok(evaluator.plan)
}
/// Only logical persistent values survive process restart. Feedback caches and
/// temporary state must become unknown, rather than inheriting stale truth.
pub fn restart(variables: &mut BTreeMap<String, Variable>) {
    for v in variables.values_mut() {
        if v.lifetime != Lifetime::Persistent {
            v.value = None;
        }
    }
}

/// Static read/write set across all branches, including branches that would be
/// unknown or unreachable for a particular snapshot. Scene calls must be expanded.
pub fn variable_references(nodes: &[Node]) -> Result<BTreeSet<String>, &'static str> {
    variable_references_with_deferred(nodes, &BTreeSet::new())
}
/// Explicitly linked timelines contain commands, not variable expressions. Only
/// their activation calls can remain opaque to this complete branch read scan.
pub fn variable_references_with_deferred(nodes:&[Node], deferred:&BTreeSet<String>)->Result<BTreeSet<String>, &'static str> {
    struct Visitor<'a> {
        deferred: &'a BTreeSet<String>,
        names: BTreeSet<String>,
        remaining: usize,
    }
    impl Visitor<'_> {
        fn charge(&mut self, depth: usize) -> Result<(), &'static str> {
            if depth > 64 || self.remaining == 0 {
                return Err("rule reference budget exceeded");
            }
            self.remaining -= 1;
            Ok(())
        }
        fn expr(&mut self, e: &Expr, depth: usize) -> Result<(), &'static str> {
            self.charge(depth)?;
            match e {
                Expr::Variable(name) => {
                    self.names.insert(name.clone());
                }
                Expr::Add(a, b) | Expr::Subtract(a, b) => {
                    self.expr(a, depth + 1)?;
                    self.expr(b, depth + 1)?;
                }
                _ => {}
            }
            Ok(())
        }
        fn condition(&mut self, c: &Condition, depth: usize) -> Result<(), &'static str> {
            self.charge(depth)?;
            match c {
                Condition::Equal(a, b) | Condition::Less(a, b) => {
                    self.expr(a, depth + 1)?;
                    self.expr(b, depth + 1)?;
                }
                Condition::Not(c) => self.condition(c, depth + 1)?,
                Condition::All(items) | Condition::Any(items) => {
                    for c in items {
                        self.condition(c, depth + 1)?;
                    }
                }
                Condition::Unknown(e) => self.expr(e, depth + 1)?,
            }
            Ok(())
        }
        fn nodes(&mut self, nodes: &[Node], depth: usize) -> Result<(), &'static str> {
            for node in nodes {
                self.charge(depth)?;
                match &node.action {
                    Action::Set { variable, value } => {
                        self.names.insert(variable.clone());
                        self.expr(value, depth + 1)?;
                    }
                    Action::If {
                        condition,
                        then_actions,
                        else_actions,
                    } => {
                        self.condition(condition, depth + 1)?;
                        self.nodes(then_actions, depth + 1)?;
                        self.nodes(else_actions, depth + 1)?;
                    }
                    Action::Scene { id, activate } if *activate && self.deferred.contains(id) => {},
                    Action::Scene { .. } => return Err("unexpanded scene in reference scan"),
                    Action::Command { .. } => {}
                }
            }
            Ok(())
        }
    }
    let mut visitor = Visitor {
        deferred,
        names: BTreeSet::new(),
        remaining: 65536,
    };
    visitor.nodes(nodes, 0)?;
    Ok(visitor.names)
}
