//! Final light targets derived from scene time order, independently of selection.
pub use crate::study_runtime::SceneActivity;
use homesense_core::{
    Command, DeviceId,
    rule::{Action, Condition, Expr, Node, Value},
    scene::Step,
};
use std::collections::BTreeMap;
pub(crate) struct Targets(Option<BTreeMap<DeviceId, Value>>);
impl Targets {
    pub fn new(steps: &[Step]) -> Self {
        let mut ordered: Vec<_> = steps.iter().enumerate().collect();
        ordered.sort_by_key(|(index, step)| (step.delay_ms, *index));
        let mut targets = BTreeMap::new();
        for (_, step) in ordered {
            let value = match step.command {
                Command::Switch(on) => Value::Bool(on),
                Command::SetLevel { level, .. } => Value::Integer(level.get().into()),
                Command::PanelLed(_) => continue,
                // A motor or paused ramp has no known final light target.
                _ => return Self(None),
            };
            targets.insert(step.device.clone(), value);
        }
        Self((!targets.is_empty()).then_some(targets))
    }
    pub(crate) async fn project(
        &self,
        admission: &crate::admission::Admission,
        controller: uuid::Uuid,
        generation: &crate::admission::TaskGeneration,
        feedback: &crate::feedback::Feedback,
        max_age: u64,
    ) -> Result<SceneActivity,String> {
        use crate::scene_activity::SceneActivity as Activity;
        use homesense_core::rule::{Budget, Lifetime, Value, ValueType, Variable};
        if max_age == 0
            || !feedback.is_active()
            || !admission
                .task_generation()
                .await
                .is_ok_and(|current| current == *generation)
        {
            return Ok(Activity::Unknown);
        }
        let Some(nodes) = self.nodes(feedback) else {
            return Ok(Activity::Unknown);
        };
        let variables = ["unknown", "mismatch"]
            .into_iter()
            .map(|name| {
                (
                    name.into(),
                    Variable {
                        value_type: ValueType::Bool,
                        lifetime: Lifetime::Temporary,
                        value: Some(Value::Bool(false)),
                    },
                )
            })
            .collect();
        let (plan, lease) = feedback.plan_rule(
            &nodes,
            &variables,
            Budget::default(),
            admission.monotonic_ms(),
            max_age,
        )?;
        if plan.variables["unknown"].value == Some(Value::Bool(true)) {
            return Ok(Activity::Unknown);
        }
        match admission
            .feedback_settled(controller, generation.clone(), lease)
            .await
        {
            Ok(false) => Ok(Activity::Pending),
            Ok(true) => Ok(
                if plan.variables["mismatch"].value == Some(Value::Bool(true)) {
                    Activity::Inactive
                } else {
                    Activity::Active
                },
            ),
            Err(hap::server::WriteStatus::ResourceBusy) => Ok(Activity::Unknown),
            Err(error) => Err(format!("scene activity: {error:?}")),
        }
    }
    pub fn nodes(&self, feedback: &crate::feedback::Feedback) -> Option<Vec<Node>> {
        self.0.as_ref().map(|targets| {
            let mut checks = vec![];
            for (device, target) in targets {
                let expr = match target {
                    Value::Bool(_) => Expr::DeviceOn(device.clone()),
                    Value::Integer(_) => Expr::DeviceLevel(device.clone()),
                };
                if let Some(relay) = feedback.supply_relay(device) {
                    let on = match target {
                        Value::Bool(on) => *on,
                        Value::Integer(level) => *level > 0,
                    };
                    // Off preserves the dimmer level, but both component samples
                    // must remain known and fresh, matching combined_light::observed.
                    checks.push((expr, on.then_some(target.clone())));
                    checks.push((Expr::DeviceOn(relay.clone()), Some(Value::Bool(on))));
                } else {
                    checks.push((expr, Some(target.clone())));
                }
            }
            checks
                .into_iter()
                .map(|(expr, target)| {
                    let set = |name: &str| Node {
                        source: "scene:activity".into(),
                        action: Action::Set {
                            variable: name.into(),
                            value: Expr::Literal(Value::Bool(true)),
                        },
                    };
                    let else_actions = target
                        .map(|target| {
                            vec![Node {
                                source: "scene:activity:match".into(),
                                action: Action::If {
                                    condition: Condition::Equal(
                                        expr.clone(),
                                        Expr::Literal(target),
                                    ),
                                    then_actions: vec![],
                                    else_actions: vec![set("mismatch")],
                                },
                            }]
                        })
                        .unwrap_or_default();
                    Node {
                        source: "scene:activity".into(),
                        action: Action::If {
                            condition: Condition::Unknown(expr),
                            then_actions: vec![set("unknown")],
                            else_actions,
                        },
                    }
                })
                .collect()
        })
    }
}
