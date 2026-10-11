//! Pure reservation of delayed output. No variable is committed and no timeline
//! starts here. Runtime admission must commit the entire returned plan together.
use crate::{rule::{Action, Plan}, scene::Step};
use std::collections::BTreeMap;
#[derive(Debug)]
pub struct Invocation {
    /// Position among evaluated leaf actions, including variable assignments.
    pub action_index: usize,
    pub definition: String,
    pub source: String,
    pub steps: Vec<Step>,
}
#[derive(Debug)]
pub struct Reserved {
    pub plan: Plan,
    pub invocations: Vec<Invocation>,
    /// Includes the scene-call action itself and every future physical output.
    pub action_cost: usize,
}
#[derive(Debug, PartialEq, Eq)]
pub enum Error { MissingScene(String), UnknownDeactivation(String), Budget, InvalidSource }
/// `definitions` must contain only reviewed, enabled and validated timelines.
/// Charge every future step now, so repeated calls cannot evade the rule budget
/// by deferring their output. Runtime must also reserve scheduler capacity.
pub fn reserve(plan:Plan, definitions:&BTreeMap<String,Vec<Step>>, max_actions:usize)->Result<Reserved,Error> {
    let mut cost=plan.action_cost;
    if cost>max_actions {return Err(Error::Budget);}
    let mut invocations=Vec::new();
    for (action_index,node) in plan.actions.iter().enumerate() {
        if let Action::Scene{id,activate}=&node.action {
            if !activate {return Err(Error::UnknownDeactivation(id.clone()));}
            let steps=definitions.get(id).ok_or_else(||Error::MissingScene(id.clone()))?;
            cost=cost.checked_add(steps.len()).filter(|c|*c<=max_actions).ok_or(Error::Budget)?;
            let mut copied=Vec::with_capacity(steps.len());
            for step in steps {
                if node.source.is_empty() || step.source.is_empty() {return Err(Error::InvalidSource);}
                let source=format!("{} -> {}",node.source,step.source);
                if source.len()>8192 {return Err(Error::InvalidSource);}
                let mut step=step.clone();step.source=source;copied.push(step);
            }
            invocations.push(Invocation{action_index,definition:id.clone(),source:node.source.clone(),steps:copied});
        }
    }
    Ok(Reserved{plan,invocations,action_cost:cost})
}
