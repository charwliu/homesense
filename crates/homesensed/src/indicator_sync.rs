//! Reviewed off reconciliation, separate from button automation. No light
//! command or scene replay is emitted; only linked LEDs and local selection.
use crate::{
    admission::Admission,
    feedback::Feedback,
    rule_runtime::{PreparationFailure, PreparedRule},
    scene_scheduler::SceneScheduler,
};
use hap::server::WriteStatus;
use homesense_config::rule_document::Document;
use homesense_core::{
    Command,
    rule::{Action, Budget, Condition, Expr, Node, Value},
};
use std::collections::BTreeMap;

#[derive(Default)]
pub struct Synchronizer {
    attempted: BTreeMap<String, (bool, Vec<(String, u64)>)>,
}
impl Synchronizer {
    #[allow(clippy::too_many_arguments)]
    pub async fn tick(
        &mut self,
        admission: &Admission,
        controller: uuid::Uuid,
        document: &Document,
        scenes: &SceneScheduler,
        feedback: &Feedback,
        max_age: u64,
    ) -> Result<(), String> {
        if !document.indicator_links.iter().any(|l| l.enabled) {
            return Ok(());
        }
        let generation = match admission.task_generation().await {
            Ok(value) => value,
            Err(WriteStatus::ResourceBusy) => return Ok(()),
            Err(error) => return Err(format!("{error:?}")),
        };
        let recorded = match admission
            .recorded_dimmer_levels(controller, generation.clone())
            .await
        {
            Ok(value) => value,
            Err(WriteStatus::ResourceBusy) => return Ok(()),
            Err(error) => return Err(format!("{error:?}")),
        };
        // Delayed scene/fade output is not yet a settled off observation.
        // Conservatively defer while any scheduled scene is still active.
        if !scenes.publication_impact().is_empty() {
            return Ok(());
        }
        for link in document.indicator_links.iter().filter(|l| l.enabled) {
            let names = link
                .scene
                .as_ref()
                .and_then(|scene| {
                    document
                        .scene_switch_groups
                        .iter()
                        .find(|g| g.members.iter().any(|m| &m.scene == scene))
                })
                .map(|g| vec![g.variable.clone()])
                .unwrap_or_default();
            let mut actions = link
                .leds
                .iter()
                .map(|id| Node {
                    source: format!("indicator:{}", link.id),
                    action: Action::Command {
                        device: id.clone(),
                        command: Command::PanelLed(false),
                    },
                })
                .collect::<Vec<_>>();
            if let Some(scene) = &link.scene {
                let group = document
                    .scene_switch_groups
                    .iter()
                    .find(|g| g.members.iter().any(|m| &m.scene == scene))
                    .ok_or("indicator scene missing")?;
                let member = group.members.iter().find(|m| &m.scene == scene).unwrap();
                // Clear an actually selected member only. Physical all-off
                // can clear reviewed LEDs, but cannot establish an external
                // controller's logical toggle baseline from unknown.
                actions.push(Node {
                    source: format!("indicator:{}:selection", link.id),
                    action: Action::If {
                        condition: Condition::Equal(
                            Expr::Variable(group.variable.clone()),
                            Expr::Literal(Value::Integer(member.value)),
                        ),
                        then_actions: vec![Node {
                            source: format!("indicator:{}:clear", link.id),
                            action: Action::Set {
                                variable: group.variable.clone(),
                                value: Expr::Literal(Value::Integer(0)),
                            },
                        }],
                        else_actions: vec![],
                    },
                });
            }
            let condition = if let Some(scene) = &link.scene {
                // Only final targets that should emit light participate. A scene
                // intentionally turning a load off must not cancel itself.
                let mut targets = BTreeMap::new();
                let mut ordered = document.timed_scenes[scene]
                    .steps
                    .iter()
                    .enumerate()
                    .collect::<Vec<_>>();
                ordered.sort_by_key(|(index, step)| (step.delay_ms, *index));
                for (_, step) in ordered {
                    let on = match step.command {
                        Command::Switch(on) => on,
                        Command::SetLevel { level, .. } => level.get() > 0,
                        _ => continue,
                    };
                    targets.insert(step.device.clone(), on);
                }
                Condition::Any(
                    targets
                        .into_iter()
                        .filter(|(_, on)| *on)
                        .map(|(id, _)| {
                            Condition::Equal(Expr::DeviceOn(id), Expr::Literal(Value::Bool(false)))
                        })
                        .collect(),
                )
            } else {
                Condition::All(
                    link.lights
                        .iter()
                        .map(|id| {
                            Condition::Equal(
                                Expr::DeviceOn(id.clone()),
                                Expr::Literal(Value::Bool(false)),
                            )
                        })
                        .collect(),
                )
            };
            let nodes = vec![Node {
                source: format!("indicator:{}:off", link.id),
                action: Action::If {
                    condition,
                    then_actions: actions,
                    else_actions: vec![],
                },
            }];
            let mut lease = None;
            let mut off = false;
            let mut relative_pending = false;
            // Scene truth under the new policy is physical On/Off. Historical
            // dimmer intent remains the policy for ordinary light-only links.
            let physical_scene = link.scene.as_ref().is_some_and(|scene|document.scene_switch_groups.iter().any(|g|g.members.iter().any(|m|&m.scene==scene) && g.reconciliation.as_ref().is_some_and(|p|p.enabled)));
            let physical_levels = BTreeMap::new();
            let prepared = PreparedRule::prepare_indicator(
                admission,
                controller,
                generation.clone(),
                names,
                |variables, selections, policy| {
                    relative_pending = selections.values().any(|selection| {
                        selection.physical.as_ref().is_some_and(|physical| physical.pending_relative)
                    });
                    let (plan, read) = feedback.plan_indicator_rule(
                        &nodes,
                        variables,
                        Budget::default(),
                        admission.monotonic_ms(),
                        max_age,
                        if physical_scene { &physical_levels } else { &recorded },
                        policy,
                    )?;
                    // Unknown lights never authorize output; an unknown selection
                    // stays unknown even when known-off loads clear their LEDs.
                    off = !plan.commands.is_empty();
                    lease = Some(read);
                    Ok(plan)
                },
            )
            .await;
            let mut prepared = match prepared {
                Ok(value) => value,
                Err(PreparationFailure::Admission(WriteStatus::ResourceBusy)) => return Ok(()),
                Err(PreparationFailure::Admission(error)) => {
                    return Err(format!("indicator snapshot: {error:?}"));
                }
                Err(PreparationFailure::Planning(error)) => return Err(error),
            };
            // A fresh intermediate Off can precede the remaining native phase's
            // query replies. Defer this group's LEDs/selection until that phase
            // is observed; independent light links keep their normal scope.
            // Do not change dedup state while waiting for complete feedback.
            if relative_pending {
                continue;
            }
            let lease = lease.ok_or("indicator feedback lease missing")?;
            // An unsettled sample must not change dedup state (off->on->off).
            match admission
                .feedback_settled(controller, generation.clone(), lease.clone())
                .await
            {
                Ok(true) => {}
                Ok(false) | Err(WriteStatus::ResourceBusy) => continue,
                Err(error) => return Err(format!("{error:?}")),
            }
            if !off {
                if prepared.unknown_branches.is_empty() {
                    self.attempted.insert(link.id.clone(), (false, vec![]));
                }
                continue;
            }
            let mode_clearing = !prepared.changed_variables.is_empty();
            let versions = prepared.input_versions();
            if self
                .attempted
                .get(&link.id)
                .is_some_and(|(attempted, old)| *attempted && (!mode_clearing || old == &versions))
            {
                continue;
            }
            prepared.attach_feedback(Some(lease));
            // Record before awaiting so dropping the future cannot retry an
            // admission that may already be durable. Undo only known rejection.
            let previous = self.attempted.insert(link.id.clone(), (true, versions));
            match prepared.commit_status(admission).await {
                // Both statuses are definite rejection before durable acceptance.
                // Do not reuse this plan: the next tick captures fresh state/CAS.
                Err(WriteStatus::ResourceBusy | WriteStatus::InvalidValueInRequest) => {
                    if let Some(previous) = previous {
                        self.attempted.insert(link.id.clone(), previous);
                    } else {
                        self.attempted.remove(&link.id);
                    }
                }
                Ok(_) => {}
                Err(error) => {
                    log::warn!(
                        "indicator {} admission failed: {error:?}; no automatic replay",
                        link.id
                    );
                }
            }
        }
        Ok(())
    }
}
