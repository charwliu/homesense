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
    pub deactivated_scenes: std::collections::BTreeSet<String>,
}
#[derive(Debug, PartialEq, Eq)]
pub enum Error { MissingScene(String), UnknownDeactivation(String), Budget, InvalidSource, ActivationConflict(String) }
/// `definitions` must contain only reviewed, enabled and validated timelines.
/// Charge every future step now, so repeated calls cannot evade the rule budget
/// by deferring their output. Runtime must also reserve scheduler capacity.
pub fn reserve(plan:Plan, definitions:&BTreeMap<String,Vec<Step>>, max_actions:usize)->Result<Reserved,Error> {
    reserve_with_deactivations(plan, definitions, &BTreeMap::new(), max_actions)
}
/// Explicit target links only. Caller must enforce the returned quiescence set
/// atomically with admission, including a wholly Immediate target.
pub fn reserve_with_deactivations(plan:Plan, definitions:&BTreeMap<String,Vec<Step>>, deactivations:&BTreeMap<String,String>, max_actions:usize)->Result<Reserved,Error> {
    let mut deactivated_scenes=std::collections::BTreeSet::new();
    let mut cost=plan.action_cost;
    if cost>max_actions {return Err(Error::Budget);}
    let mut invocations=Vec::new();
    for (action_index,node) in plan.actions.iter().enumerate() {
        if let Action::RequireQuiescence {scene}=&node.action {deactivated_scenes.insert(scene.clone());}
        if let Action::Scene{id,activate}=&node.action {
            let target=if *activate {id} else {
                let target=deactivations.get(id).ok_or_else(||Error::UnknownDeactivation(id.clone()))?;
                deactivated_scenes.insert(id.clone());target
            };
            let steps=definitions.get(target).ok_or_else(||Error::MissingScene(target.clone()))?;
            cost=cost.checked_add(steps.len()).filter(|c|*c<=max_actions).ok_or(Error::Budget)?;
            let mut copied=Vec::with_capacity(steps.len());
            for step in steps {
                if node.source.is_empty() || step.source.is_empty() {return Err(Error::InvalidSource);}
                let source=format!("{} -> {}",node.source,step.source);
                if source.len()>8192 {return Err(Error::InvalidSource);}
                let mut step=step.clone();step.source=source;copied.push(step);
            }
            invocations.push(Invocation{action_index,definition:target.clone(),source:node.source.clone(),steps:copied});
        }
    }
    for node in &plan.actions {
        let activated=match &node.action {Action::SceneActivation{scene}=>Some(scene),Action::Scene{id,activate:true}=>Some(id),_=>None};
        if let Some(id)=activated && deactivated_scenes.contains(id){return Err(Error::ActivationConflict(id.clone()));}
    }
    Ok(Reserved{plan,invocations,action_cost:cost,deactivated_scenes})
}
