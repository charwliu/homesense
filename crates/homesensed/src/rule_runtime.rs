//! Immediate rule planning against an actor-owned variable snapshot. No event
//! subscription is installed here. Scene calls require a separate reviewed
//! expansion and are rejected before any writes by this immediate adapter.
use crate::admission::{Admission, TaskGeneration};
use homesense_core::rule::{self, Action, Budget, Node, Variable};
use homesense_hap::requests::{Batch, Intent};
use homesense_storage::variables::Update;
use uuid::Uuid;
pub struct PreparedRule {
    audit: Option<homesense_storage::rule_audit::Admission>,
    feedback: Option<crate::feedback::RuleLease>,
    generation: TaskGeneration,
    batch: Batch,
    updates: Vec<Update>,
    pub unknown_branches: Vec<String>,
    pub(crate) changed_variables: Vec<String>,
}
impl PreparedRule {
    pub(crate) fn attach_cause(
        &mut self,
        rule: &str,
        cause: &homesense_core::rule_chain::Cause,
        parent: u64,
    ) {
        self.audit = Some(homesense_storage::rule_audit::Admission {
            revision: self.generation.epochs.configuration,
            correlation: cause.correlation_id(),
            event: cause.event_id(),
            parent,
            rule: rule.into(),
        });
    }

    pub(crate) fn attach_feedback(&mut self, lease: Option<crate::feedback::RuleLease>) {
        self.feedback = lease;
    }
    /// Capture all referenced persistent and boot-scoped temporary variables. The evaluator rejects a
    /// reference absent from this explicit set; it never invents a zero value.
    pub async fn prepare(
        admission: &Admission,
        controller: Uuid,
        names: Vec<String>,
        nodes: &[Node],
        budget: Budget,
    ) -> Result<Self, String> {
        let generation = admission
            .task_generation()
            .await
            .map_err(|e| format!("{e:?}"))?;
        Self::prepare_at(admission, controller, generation, names, nodes, budget).await
    }
    pub(crate) async fn prepare_at(
        admission: &Admission,
        controller: Uuid,
        generation: TaskGeneration,
        names: Vec<String>,
        nodes: &[Node],
        budget: Budget,
    ) -> Result<Self, String> {
        Self::prepare_using(admission, controller, generation, names, |variables| {
            rule::evaluate(nodes, variables, budget).map_err(|e| format!("rule: {e:?}"))
        })
        .await
    }
    pub(crate) async fn prepare_using(
        admission: &Admission,
        controller: Uuid,
        generation: TaskGeneration,
        names: Vec<String>,
        planner: impl FnOnce(
            &std::collections::BTreeMap<String, Variable>,
        ) -> Result<rule::Plan, String>,
    ) -> Result<Self, String> {
        let snapshot = admission
            .variable_snapshot(controller, generation.clone(), names)
            .await
            .map_err(|e| format!("snapshot: {e:?}"))?;
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
        let plan = planner(&variables)?;
        if plan
            .actions
            .iter()
            .any(|a| matches!(a.action, Action::Scene { .. }))
        {
            return Err("scene expansion required before immediate admission".into());
        }
        let changed_variables = snapshot
            .iter()
            .filter(|(name, before)| plan.variables[*name].value != before.value)
            .map(|(name, _)| name.clone())
            .collect();
        let intents = plan
            .commands
            .into_iter()
            .map(|c| Intent {
                device: c.device,
                command: c.command,
            })
            .collect();
        // Validate the whole read set at commit, including condition-only values.
        // A successful rule advances their versions even when values match.
        let updates = if plan.actions.is_empty() {
            vec![]
        } else {
            snapshot
                .into_iter()
                .map(|(name, v)| Update {
                    revision: generation.epochs.configuration,
                    expected_version: v.version,
                    value: plan.variables[&name].value.clone(),
                    name,
                })
                .collect()
        };
        Ok(Self {
            audit: None,
            feedback: None,
            generation,
            batch: Batch {
                controller,
                intents,
                subscriptions: vec![],
            },
            updates,
            unknown_branches: plan.unknown_branches,
            changed_variables,
        })
    }
    pub async fn prepare_with_feedback(
        admission: &Admission,
        controller: Uuid,
        names: Vec<String>,
        nodes: &[Node],
        budget: Budget,
        feedback: &crate::feedback::Feedback,
        max_age_ms: u64,
    ) -> Result<Self, String> {
        let generation = admission
            .task_generation()
            .await
            .map_err(|e| format!("{e:?}"))?;
        let mut lease = None;
        let mut prepared =
            Self::prepare_using(admission, controller, generation, names, |variables| {
                let (plan, read) = feedback.plan_rule(
                    nodes,
                    variables,
                    budget,
                    admission.monotonic_ms(),
                    max_age_ms,
                )?;
                lease = Some(read);
                Ok(plan)
            })
            .await?;
        prepared.feedback = lease;
        Ok(prepared)
    }
    pub async fn prepare_with_scenes(
        admission: &Admission,
        controller: Uuid,
        names: Vec<String>,
        nodes: &[Node],
        scenes: &std::collections::BTreeMap<String, homesense_core::scene_expand::Definition>,
        budget: Budget,
    ) -> Result<Self, String> {
        let expanded = homesense_core::scene_expand::expand(nodes, scenes, budget.nodes)
            .map_err(|e| format!("scene expansion: {e:?}"))?;
        Self::prepare(admission, controller, names, &expanded, budget).await
    }
    /// Consumes the plan: failed/ambiguous admission is not automatically replayed.
    pub async fn commit(self, admission: &Admission) -> Result<Vec<u64>, String> {
        if let Some(audit) = self.audit {
            return admission
                .submit_audited_rule(
                    self.generation,
                    self.batch,
                    self.updates,
                    self.feedback,
                    audit,
                )
                .await
                .map_err(|e| format!("rule admission: {e:?}"));
        }
        if self.batch.intents.is_empty() && self.updates.is_empty() {
            return Ok(vec![]);
        }
        if let Some(feedback) = self.feedback {
            return admission
                .submit_feedback_rule(self.generation, self.batch, self.updates, feedback)
                .await
                .map_err(|e| format!("rule admission: {e:?}"));
        }
        admission
            .submit_rule_update(self.generation, self.batch, self.updates)
            .await
            .map_err(|e| format!("rule admission: {e:?}"))
    }
}
