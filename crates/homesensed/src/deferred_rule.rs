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
    /// Source nodes skipped by three-valued evaluation, never treated as Else.
    pub unknown_branches: Vec<String>,
    pub motor_reads:Vec<homesense_core::rule_motor::Read>,
}
/// Only planning rejection proves this invocation performed no admission.
/// Earlier invocations in the same chain may already have committed.
#[derive(Debug)]
pub enum ExecutionFailure {
    Planning(Error),
    Runtime(String),
    BusyBeforeAcceptance,
    RejectedBeforeAcceptance(String),
    Partial {
        completed: Vec<Outcome>,
        error: Box<ExecutionFailure>,
    },
}
impl ExecutionFailure {
    pub fn is_chain_rejection(&self) -> bool {
        matches!(
            self,
            Self::Planning(Error::Budget | Error::Cycle | Error::Rule(_))
                | Self::BusyBeforeAcceptance | Self::RejectedBeforeAcceptance(_)
        )
    }
}
impl std::fmt::Display for ExecutionFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Planning(error) => write!(f, "{error:?}"),
            Self::Runtime(error) | Self::RejectedBeforeAcceptance(error) => f.write_str(error),
            Self::BusyBeforeAcceptance => {
                f.write_str("deferred admission rejected before acceptance: ResourceBusy")
            }
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
    if !scheduler.matches_controller(controller){return Err("foreign scene scheduler".into());}
    let generation=admission.task_generation().await.map_err(|e|format!("{e:?}"))?;
    Box::pin(execute_entry_classified(Caller{admission,controller,generation:&generation,admit_before:None},Entry::Rule(rule_id),chain,parent,scheduler,feedback)).await
}
/// A session captures the caller and generation once; no continuation may
/// rebind an old request to a newer published document after an await.
#[derive(Clone,Copy)]
pub(crate) struct Caller<'a> {pub admission:&'a Admission,pub controller:Uuid,pub generation:&'a crate::admission::TaskGeneration,pub admit_before:Option<u64>}
pub(crate) async fn execute_caller_rule_classified(caller:Caller<'_>,rule_id:&str,chain:&mut Chain,parent:&Cause,scheduler:&mut SceneScheduler,feedback:Option<(&Feedback,u64)>)->Result<Outcome,ExecutionFailure> {
    Box::pin(execute_entry_classified(caller,Entry::Rule(rule_id),chain,parent,scheduler,feedback)).await
}
pub(crate) async fn execute_manual_scenes_classified(caller:Caller<'_>,writes:&[(String,Option<bool>)],chain:&mut Chain,parent:&Cause,scheduler:&mut SceneScheduler,feedback:Option<(&Feedback,u64)>)->Result<Outcome,ExecutionFailure> {
    Box::pin(execute_entry_classified(caller,Entry::Scenes(writes),chain,parent,scheduler,feedback)).await
}
enum Entry<'a> { Rule(&'a str), Scenes(&'a [(String,Option<bool>)]) }
async fn execute_entry_classified(caller:Caller<'_>,entry:Entry<'_>,chain:&mut Chain,parent:&Cause,scheduler:&mut SceneScheduler,feedback:Option<(&Feedback,u64)>)->Result<Outcome,ExecutionFailure> {
    let Caller{admission,controller,generation,admit_before}=caller;
    if !scheduler.matches_admission(admission) {return Err("foreign scene scheduler".into());}
    let generation=generation.clone();
    let is_manual=matches!(&entry,Entry::Scenes(_));
    let bundle = admission
        .automation_bundle(controller, generation.clone())
        .await
        .map_err(|e|if is_manual {ExecutionFailure::RejectedBeforeAcceptance(format!("snapshot rejected before acceptance: {e:?}"))}else{ExecutionFailure::Runtime(format!("{e:?}"))})?;
    let document = bundle.automation.ok_or("automation document missing")?;
    let manual=if let Entry::Scenes(writes)=&entry {Some(crate::manual_scene_plan::capture(admission,controller,&generation,&document,writes).await.map_err(|e|match e{crate::manual_scene_plan::Failure::Planning(e)=>ExecutionFailure::Planning(Error::Planning(e)),crate::manual_scene_plan::Failure::Snapshot(status)=>ExecutionFailure::RejectedBeforeAcceptance(format!("native snapshot rejected before acceptance: {status:?}"))})?)}else{None};
    let (rule_id,actions)=match &entry {
        Entry::Rule(id)=>{
            let mut linked=document.linked_rules_with_deferred().map_err(|e|e.to_string())?;
            let index=linked.iter().position(|r|r.id==*id).ok_or("enabled rule missing")?;
            ((*id).to_owned(),linked.swap_remove(index).actions)
        },
        Entry::Scenes(_)=>("manual-immediate-scenes".to_owned(),manual.as_ref().unwrap().actions.clone()),
    };
    let definitions: BTreeMap<_, _> = document
        .timed_scenes
        .iter()
        .filter(|(_, s)| s.enabled)
        .map(|(id, s)| (id.clone(), s.steps.clone()))
        .collect();
    let mut names = rule::variable_references_with_deferred(
        &actions,
        &definitions.keys().cloned().collect(),
    )
    .map_err(str::to_owned)?;
    if let Entry::Scenes(writes)=&entry{for policy in document.component_mode_confirmations.iter().filter(|p|p.scenes.iter().any(|s|writes.iter().any(|(id,on)|id==s&&(*on==Some(true)||document.scene_presentations.contains_key(id))))){names.extend(policy.when_values.keys().cloned());names.extend(policy.when_initial_unknown.iter().cloned());}}
    let snapshot = admission
        .variable_snapshot(controller, generation.clone(), names.into_iter().collect())
        .await
        .map_err(|e|if is_manual {ExecutionFailure::RejectedBeforeAcceptance(format!("snapshot rejected before acceptance: {e:?}"))}else{ExecutionFailure::Runtime(format!("{e:?}"))})?;
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
    let motor=if feedback.is_some(){crate::motor_rules::capture(admission,controller,generation.clone(),&actions).await?}else{None};
    let mut lease = None;
    let mut invocations = vec![];
    let mut deactivated_scenes=std::collections::BTreeSet::new();
    let planner=|budget:rule::Budget| {
            let plan = if let Some((feedback, max_age)) = feedback {
                let (plan, read) = feedback
                    .plan_rule_with_motor(
                        &actions,
                        &variables,
                        budget,
                        admission.monotonic_ms(),
                        max_age,
                        motor.as_ref(),
                    )
                    .map_err(Error::Planning)?;
                lease = Some(read);
                plan
            } else {
                rule::evaluate(&actions, &variables, budget).map_err(Error::Rule)?
            };
            let reserved = deferred_plan::reserve(plan, &definitions, budget.commands)
                .map_err(|e| Error::Planning(format!("{e:?}")))?;
            let extra_fade_cost=if reserved.deactivated_scenes.is_empty(){0}else{reserved.invocations.iter().try_fold(0usize,|sum,i|document.timed_scenes[&i.definition].fades.values().try_fold(sum,|cost,f|cost.checked_add(f.samples().map_err(|e|Error::Planning(e.to_string()))? as usize-1).filter(|n|*n<=budget.commands).ok_or(Error::Budget)))?};
            let reserved_cost=reserved.action_cost.checked_add(extra_fade_cost).filter(|n|*n<=budget.commands).ok_or(Error::Budget)?;
            deactivated_scenes=reserved.deactivated_scenes;
            invocations = reserved.invocations;
            let mut plan = reserved.plan;
            plan.action_cost = reserved_cost;
            Ok(plan)
        };
    let planned=match &entry {
        Entry::Rule(id)=>chain.plan_using(parent,id,admission.monotonic_ms(),planner),
        Entry::Scenes(writes)=>chain.plan_manual_scenes_using(parent,&writes.iter().map(|(id,_)|id.clone()).collect::<Vec<_>>(),admission.monotonic_ms(),planner),
    }.map_err(ExecutionFailure::Planning)?;
    let mut reservation = if invocations.is_empty() {
        None
    } else {
        Some(scheduler.reserve_slots(invocations.len()).map_err(|e|if is_manual {ExecutionFailure::RejectedBeforeAcceptance(e)}else{ExecutionFailure::Runtime(e)})?)
    };
    let explicit_writes=planned.plan.actions.iter().filter_map(|node|match &node.action {rule::Action::Set{variable,..}=>Some(variable.clone()),_=>None}).collect::<std::collections::BTreeSet<_>>();
    let ordinary_writes=planned.plan.actions.iter().filter(|node|manual.as_ref().is_none_or(|m|!m.provider_set_sources.contains(&node.source))).filter_map(|node|match &node.action {rule::Action::Set{variable,..}=>Some(variable.clone()),_=>None}).collect::<std::collections::BTreeSet<_>>();
    let mut condition_reads=if manual.is_some(){snapshot.iter().filter(|(name,_)|!explicit_writes.contains(*name)).map(|(name,v)|homesense_storage::variables::ReadCheck{revision:document.revision,name:name.clone(),expected_version:v.version}).collect()}else{vec![]};
    let initial_snapshot=snapshot.clone();
    let mut changed_variables:Vec<String> = snapshot
        .iter()
        .filter(|(name, v)| planned.plan.variables[*name].value != v.value && (manual.is_none() || ordinary_writes.contains(*name)))
        .map(|(name, _)| name.clone())
        .collect();
    let mut updates:Vec<homesense_storage::variables::Update> = if planned.plan.actions.is_empty() {
        vec![]
    } else {
        snapshot
            .into_iter()
            .filter(|(name,_)|manual.is_none() || explicit_writes.contains(name))
            .map(|(name, v)| homesense_storage::variables::Update {
                revision: document.revision,
                expected_version: v.version,
                value: planned.plan.variables[&name].value.clone(),
                name,
            })
            .collect()
    };
    let requested=match &entry{Entry::Scenes(writes)=>writes.iter().filter(|(id,on)|*on==Some(true)||document.scene_presentations.contains_key(id)).map(|(id,_)|id.clone()).collect::<Vec<_>>(),_=>vec![]};
    let intents=planned.plan.commands.iter().map(|c|Intent{device:c.device.clone(),command:c.command}).collect::<Vec<_>>();
    let mode_confirmation=if requested.is_empty(){None}else{crate::component_modes::Prepared::capture(&document,&bundle.configuration,&requested,&initial_snapshot,&planned.plan.variable_reads,&planned.plan.variables,&updates,&intents,feedback,admission.monotonic_ms()).map_err(ExecutionFailure::RejectedBeforeAcceptance)?};
    if let Some(mode)=&mode_confirmation {let deferred=mode.variables();condition_reads.extend(mode.reads());updates.retain(|u|!deferred.contains(&u.name));changed_variables.retain(|name|!deferred.contains(name));if let Some(baseline)=mode.baseline(){lease=Some(match lease {Some(original)=>original.merge_physical(baseline).map_err(ExecutionFailure::RejectedBeforeAcceptance)?,None=>baseline.clone()});}}
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
    let unknown_branches = planned.plan.unknown_branches;
    let motor_reads=planned.plan.motor_reads;
    let command_sources = planned.plan.commands.iter().map(|c| c.source.clone()).collect();
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
            rule: rule_id.clone(),
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
        .submit_deferred_rule_for_entry(generation, payload, lease, Some(command_sources),crate::admission::RuleEntry{deadline:Some(chain.deadline_ms()),admit_before,selection_reads:manual.map(|m|m.selection_reads),condition_reads,deactivated_scenes,mode_confirmation:mode_confirmation.map(Box::new)})
        .await
    {
        Ok(v) => v,
        Err(e) => {
            guard.1 = true;
            return Err(match e {
                crate::admission::DeferredFailure::Rejected(hap::server::WriteStatus::InsufficientPrivileges) if !admission.source_valid()=>ExecutionFailure::RejectedBeforeAcceptance("source authority revoked before derived admission".into()),
                crate::admission::DeferredFailure::Rejected(status) if is_manual && matches!(status,hap::server::WriteStatus::ResourceBusy|hap::server::WriteStatus::InsufficientPrivileges|hap::server::WriteStatus::InvalidValueInRequest|hap::server::WriteStatus::ResourceDoesNotExist)=>ExecutionFailure::RejectedBeforeAcceptance(format!("manual scene rejected before acceptance: {status:?}")),
                crate::admission::DeferredFailure::Rejected(
                    hap::server::WriteStatus::ResourceBusy,
                ) => ExecutionFailure::BusyBeforeAcceptance,
                other => ExecutionFailure::Runtime(format!("deferred admission: {other:?}")),
            });
        }
    };
    let scene_runs = if let Some(reservation) = &mut reservation {
        scheduler
            .install_accepted_as(reservation, accepted.scenes, admission.monotonic_ms(),controller)
            .await?
    } else {
        vec![]
    };
    guard.1 = true;
    for source in &unknown_branches {
        log::warn!("rule unknown branch skipped correlation={} event={} rule={:?} source={:?}",
            planned.cause.correlation_id(), planned.cause.event_id(), rule_id, source);
    }
    Ok(Outcome {
        rule: rule_id.clone(),
        cause: planned.cause,
        causation_id: planned.causation_id,
        command_ids: accepted.command_ids,
        scene_runs,
        changed_variables,
        unknown_branches,
        motor_reads,
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
    let generation=admission.task_generation().await.map_err(|e|format!("{e:?}"))?;
    Box::pin(resume_scene_for_caller(Caller{admission,controller,generation:&generation,admit_before:None},chain,parent,result,scheduler,feedback)).await
}
pub(crate) async fn resume_scene_for_caller(caller:Caller<'_>,chain:&mut Chain,parent:&Cause,result:&crate::scene_scheduler::SceneResult,scheduler:&mut SceneScheduler,feedback:Option<(&Feedback,u64)>)->Result<Vec<Outcome>,ExecutionFailure> {
    let Caller{admission,controller,generation,..}=caller;
    let generation=generation.clone();
    use homesense_core::scene::Status;
    use homesense_storage::scene_run::{Query, Status as Stored};
    let expected = match result.status {
        Status::CommandsWritten => Stored::CommandsWritten,
        Status::Partial => Stored::Partial,
        Status::Cancelled => Stored::Cancelled,
        Status::Failed => Stored::Failed,
        _ => return Err("nonterminal scene result".into()),
    };
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
        .automation_bundle(controller, generation.clone())
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
        match execute_caller_rule_classified(Caller{admission,controller,generation:&generation,admit_before:None},&rule.id,chain,parent,scheduler,feedback).await {
            Ok(outcome)=>outcomes.push(outcome),
            Err(error)=>{chain.cancel();return Err(if outcomes.is_empty() {error} else {ExecutionFailure::Partial {completed:outcomes,error:Box::new(error)}});}
        }
    }
    Ok(outcomes)
}
