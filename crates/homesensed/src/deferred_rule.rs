//! Explicit delayed-rule adapter. No subscriptions or startup actions are enabled.
use crate::{
    admission::{Admission, DeferredRule},
    feedback::Feedback,
    scene_scheduler::SceneScheduler,
};
use homesense_core::{
    deferred_plan,
    rule::{self, Variable},
    rule_chain::{Cause, Chain, Error},
};
use homesense_hap::requests::{Batch, Intent};
use std::collections::BTreeMap;
use uuid::Uuid;
#[derive(Debug)]
pub struct Outcome {
    pub rule: String,
    pub cause: Cause,
    pub causation_id: u64,
    pub command_ids: Vec<u64>,
    pub scene_runs: Vec<u64>,
    pub changed_variables: Vec<String>,
}
/// Only planning rejection proves this invocation performed no admission.
/// Earlier invocations in the same chain may already have committed.
#[derive(Debug)]
pub enum ExecutionFailure {
    Planning(Error),
    Runtime(String),
    Partial {
        completed: Vec<Outcome>,
        error: Box<ExecutionFailure>,
    },
}
impl std::fmt::Display for ExecutionFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Planning(error) => write!(f, "{error:?}"),
            Self::Runtime(error) => f.write_str(error),
            Self::Partial { completed, error } => write!(
                f,
                "{} accepted successor rules before failure: {error}",
                completed.len()
            ),
        }
    }
}
impl From<String> for ExecutionFailure {
    fn from(error: String) -> Self {
        Self::Runtime(error)
    }
}
impl From<&str> for ExecutionFailure {
    fn from(error: &str) -> Self {
        Self::Runtime(error.into())
    }
}
/// Uses the caller's chain, charging future outputs before durable admission.
/// The returned cause must be retained for subsequent derived events. A command
/// accepted before a later installation failure is not undone or retried.
pub async fn execute(
    admission: &Admission,
    controller: Uuid,
    rule_id: &str,
    chain: &mut Chain,
    parent: &Cause,
    scheduler: &mut SceneScheduler,
    feedback: Option<(&Feedback, u64)>,
) -> Result<Outcome, String> {
    execute_classified(
        admission, controller, rule_id, chain, parent, scheduler, feedback,
    )
    .await
    .map_err(|e| e.to_string())
}
/// Typed execution entry for supervisors implementing chain-local failure policy.
pub async fn execute_classified(
    admission: &Admission,
    controller: Uuid,
    rule_id: &str,
    chain: &mut Chain,
    parent: &Cause,
    scheduler: &mut SceneScheduler,
    feedback: Option<(&Feedback, u64)>,
) -> Result<Outcome, ExecutionFailure> {
    if !scheduler.matches_admission(admission) || !scheduler.matches_controller(controller) {
        return Err("foreign scene scheduler".into());
    }
    let generation = admission
        .task_generation()
        .await
        .map_err(|e| format!("{e:?}"))?;
    let bundle = admission
        .automation_bundle(controller, generation.clone())
        .await
        .map_err(|e| format!("{e:?}"))?;
    let document = bundle.automation.ok_or("automation document missing")?;
    let linked = document
        .linked_rules_with_deferred()
        .map_err(|e| e.to_string())?;
    let rule = linked
        .iter()
        .find(|r| r.id == rule_id)
        .ok_or("enabled rule missing")?;
    let definitions: BTreeMap<_, _> = document
        .timed_scenes
        .iter()
        .filter(|(_, s)| s.enabled)
        .map(|(id, s)| (id.clone(), s.steps.clone()))
        .collect();
    let names = rule::variable_references_with_deferred(
        &rule.actions,
        &definitions.keys().cloned().collect(),
    )
    .map_err(str::to_owned)?;
    let snapshot = admission
        .variable_snapshot(controller, generation.clone(), names.into_iter().collect())
        .await
        .map_err(|e| format!("{e:?}"))?;
    let variables = snapshot
        .iter()
        .map(|(name, v)| {
            (
                name.clone(),
                Variable {
                    value_type: v.kind,
                    lifetime: v.lifetime,
                    value: v.value.clone(),
                },
            )
        })
        .collect();
    let mut lease = None;
    let mut invocations = vec![];
    let planned = chain
        .plan_using(parent, rule_id, admission.monotonic_ms(), |budget| {
            let plan = if let Some((feedback, max_age)) = feedback {
                let (plan, read) = feedback
                    .plan_rule(
                        &rule.actions,
                        &variables,
                        budget,
                        admission.monotonic_ms(),
                        max_age,
                    )
                    .map_err(Error::Planning)?;
                lease = Some(read);
                plan
            } else {
                rule::evaluate(&rule.actions, &variables, budget).map_err(Error::Rule)?
            };
            let reserved = deferred_plan::reserve(plan, &definitions, budget.commands)
                .map_err(|e| Error::Planning(format!("{e:?}")))?;
            invocations = reserved.invocations;
            let mut plan = reserved.plan;
            plan.action_cost = reserved.action_cost;
            Ok(plan)
        })
        .map_err(ExecutionFailure::Planning)?;
    let mut reservation = if invocations.is_empty() {
        None
    } else {
        Some(scheduler.reserve_slots(invocations.len())?)
    };
    let changed_variables = snapshot
        .iter()
        .filter(|(name, v)| planned.plan.variables[*name].value != v.value)
        .map(|(name, _)| name.clone())
        .collect();
    let updates = if planned.plan.actions.is_empty() {
        vec![]
    } else {
        snapshot
            .into_iter()
            .map(|(name, v)| homesense_storage::variables::Update {
                revision: document.revision,
                expected_version: v.version,
                value: planned.plan.variables[&name].value.clone(),
                name,
            })
            .collect()
    };
    let starts = invocations
        .into_iter()
        .map(|i| homesense_storage::scene_run::Start {
            run: Uuid::new_v4().to_string(),
            revision: document.revision,
            definition: i.definition,
            source: i.source,
            action_index: i.action_index,
        })
        .collect();
    let payload = DeferredRule {
        batch: Batch {
            controller,
            intents: planned
                .plan
                .commands
                .into_iter()
                .map(|c| Intent {
                    device: c.device,
                    command: c.command,
                })
                .collect(),
            subscriptions: vec![],
        },
        variables: updates,
        starts,
        audit: homesense_storage::rule_audit::Admission {
            revision: document.revision,
            correlation: planned.cause.correlation_id(),
            event: planned.cause.event_id(),
            parent: planned.causation_id,
            rule: rule_id.into(),
        },
    };
    struct Guard(Admission, bool);
    impl Drop for Guard {
        fn drop(&mut self) {
            if !self.1 {
                self.0.stop_now();
            }
        }
    }
    let mut guard = Guard(admission.clone(), false);
    let accepted = match admission
        .submit_deferred_rule_with_feedback(generation, payload, lease)
        .await
    {
        Ok(v) => v,
        Err(e) => {
            guard.1 = true;
            return Err(ExecutionFailure::Runtime(format!(
                "deferred admission: {e:?}"
            )));
        }
    };
    let scene_runs = if let Some(reservation) = &mut reservation {
        scheduler
            .install_accepted(reservation, accepted.scenes, admission.monotonic_ms())
            .await?
    } else {
        vec![]
    };
    guard.1 = true;
    Ok(Outcome {
        rule: rule_id.into(),
        cause: planned.cause,
        causation_id: planned.causation_id,
        command_ids: accepted.command_ids,
        scene_runs,
        changed_variables,
    })
}

/// Resume only a persisted terminal result belonging to this causal parent.
/// Claiming occurs before dispatch; failed or interrupted delivery is never
/// retried as a new root. The caller keeps this Chain alive across the delay.
pub async fn resume_scene(
    admission: &Admission,
    controller: Uuid,
    chain: &mut Chain,
    parent: &Cause,
    result: &crate::scene_scheduler::SceneResult,
    scheduler: &mut SceneScheduler,
    feedback: Option<(&Feedback, u64)>,
) -> Result<Vec<Outcome>, String> {
    resume_scene_classified(
        admission, controller, chain, parent, result, scheduler, feedback,
    )
    .await
    .map_err(|e| e.to_string())
}
/// Typed continuation preserves already accepted successors when a later one fails.
pub async fn resume_scene_classified(
    admission: &Admission,
    controller: Uuid,
    chain: &mut Chain,
    parent: &Cause,
    result: &crate::scene_scheduler::SceneResult,
    scheduler: &mut SceneScheduler,
    feedback: Option<(&Feedback, u64)>,
) -> Result<Vec<Outcome>, ExecutionFailure> {
    use homesense_core::scene::Status;
    use homesense_storage::scene_run::{Query, Status as Stored};
    let expected = match result.status {
        Status::CommandsWritten => Stored::CommandsWritten,
        Status::Partial => Stored::Partial,
        Status::Cancelled => Stored::Cancelled,
        Status::Failed => Stored::Failed,
        _ => return Err("nonterminal scene result".into()),
    };
    let generation = admission
        .task_generation()
        .await
        .map_err(|e| format!("{e:?}"))?;
    let page = admission
        .scene_history(
            controller,
            generation.clone(),
            Query::Run {
                boot: admission.boot_id().into(),
                run: result.audit_id.clone(),
                after: 0,
                limit: 1,
            },
        )
        .await
        .map_err(|e| format!("{e:?}"))?
        .ok_or("scene result receipt missing")?;
    let cause = page.cause.ok_or("scene result has no rule cause")?;
    if page.record.status != expected
        || page.record.definition.as_deref() != Some(result.definition.as_str())
        || cause.correlation != parent.correlation_id()
        || cause.event != parent.event_id()
    {
        return Err("scene result cause or status mismatch".into());
    }
    let document = admission
        .automation_bundle(controller, generation)
        .await
        .map_err(|e| format!("{e:?}"))?
        .automation
        .ok_or("automation missing")?;
    let rules = document
        .linked_rules_with_deferred()
        .map_err(|e| e.to_string())?;
    if !chain
        .claim_completion(parent, &result.audit_id, admission.monotonic_ms())
        .map_err(ExecutionFailure::Planning)?
    {
        return Ok(vec![]);
    }
    let mut outcomes = vec![];
    for rule in rules.iter().filter(|r|matches!(&r.trigger,homesense_config::rule_document::Trigger::SceneResult{scene} if scene==&result.definition)) {
        match execute_classified(admission,controller,&rule.id,chain,parent,scheduler,feedback).await {
            Ok(outcome)=>outcomes.push(outcome),
            Err(error)=>{chain.cancel();return Err(if outcomes.is_empty() {error} else {ExecutionFailure::Partial {completed:outcomes,error:Box::new(error)}});}
        }
    }
    Ok(outcomes)
}
