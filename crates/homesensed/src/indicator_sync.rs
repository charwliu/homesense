//! Reviewed LED reconciliation, separate from button automation. No light
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
type AttemptKey=(String,Vec<homesense_core::DeviceId>);
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub(crate) enum Evaluation {Unknown,NotRequired,Desired(bool)}

/// Sealed proof of the reviewed power predicate, minted only by power_link's
/// actual-feedback evaluator. A generic lease alone cannot authorize red.
pub(crate) struct PowerProof {
    link: String,
    lights: Vec<homesense_core::DeviceId>,
    leds: Vec<homesense_core::DeviceId>,
    policy: homesense_config::indicator_links::PowerPolicy,
    desired: bool,
    lease: crate::feedback::RuleLease,
}
impl PowerProof {
    pub(crate) fn matches(&self, link: &homesense_config::indicator_links::Link,
        device: &homesense_core::DeviceId, on: bool, lease: &crate::feedback::RuleLease) -> bool {
        self.link == link.id && self.lights == link.lights && self.policy == link.power_policy
            && self.leds.contains(device) && self.leds.iter().all(|id| link.leds.contains(id))
            && self.desired == on && lease.covers(&self.lease)
    }
}

struct Attempt {
    desired:bool,selection:i64,generation:crate::admission::TaskGeneration,controller:uuid::Uuid,
    fingerprint:crate::feedback::IndicatorFingerprint,
    output_fence:crate::indicator_outputs::Fence,commands:Vec<u64>,
}
#[derive(Default)]
pub struct Synchronizer {
    attempted: BTreeMap<AttemptKey, (bool, Vec<(String, u64)>)>,
    physical_attempted: BTreeMap<AttemptKey,Attempt>,
    emitted:Vec<Vec<u64>>,
    evaluations:BTreeMap<AttemptKey,Evaluation>,
    legacy_commands:BTreeMap<AttemptKey,Vec<u64>>,
    prepare_deadline:Option<u64>,
    visited:Vec<AttemptKey>,
}
impl Synchronizer {
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn tick_wave(&mut self,admission:&Admission,controller:uuid::Uuid,document:&Document,scenes:&SceneScheduler,feedback:&Feedback,max_age:u64,deadline:u64)->Result<usize,String> {
        self.prepare_deadline=Some(deadline);
        let result=self.tick(admission,controller,document,scenes,feedback,max_age).await;
        self.prepare_deadline=None;
        result.map(|()|self.visited.len())
    }
    pub(crate) fn evaluation(&self,link:&str,id:&homesense_core::DeviceId)->Evaluation {
        self.evaluations.get(&(link.to_owned(),vec![id.clone()])).copied().unwrap_or(Evaluation::Unknown)
    }
    pub(crate) fn take_emitted_batches(&mut self)->Vec<Vec<u64>> {std::mem::take(&mut self.emitted)}
    #[allow(clippy::too_many_arguments)]
    async fn physical_link(
        &mut self,
        admission: &Admission,
        controller: uuid::Uuid,
        document: &Document,
        group: &homesense_config::scene_switches::Group,
        link: &homesense_config::indicator_links::Link,
        feedback: &Feedback,
        max_age: u64,
        generation: crate::admission::TaskGeneration,
    ) -> Result<(), String> {
        let member = group
            .members
            .iter()
            .find(|m| Some(&m.scene) == link.scene.as_ref())
            .ok_or("indicator member missing")?;
        let patterns = group
            .light_on_patterns(&document.timed_scenes)
            .map_err(|e| e.to_string())?;
        let max_age = max_age.min(
            group
                .reconciliation
                .as_ref()
                .ok_or("reconciliation missing")?
                .max_age_ms,
        );
        let mut selection_value=None;
        let mut desired = None;
        let mut lease = None;
        let prepared = PreparedRule::prepare_indicator(admission, controller, generation.clone(), vec![group.variable.clone()], |variables, reads, policy| {
            let state = &reads[&group.variable];
            let selected = state.physical.as_ref().filter(|p| !p.pending_relative && p.observed_ms.is_some())
                .and_then(|_| match state.stored.value {Some(Value::Integer(v)) if v == 0 || patterns.contains_key(&v) => Some(v),_=>None});
            let mut nodes = vec![];
            if let Some(selected) = selected {
                selection_value=Some(selected);
                let on = selected == member.value;
                let condition = if selected == 0 {
                    // Zero is a complete known pattern with no selected scene,
                    // including unmatched patterns; it does not mean all-Off.
                    // Read every light again. Admission separately revalidates
                    // this exact physical selection snapshot before LED writes.
                    let ids=patterns.values().flat_map(|pattern|pattern.keys()).collect::<std::collections::BTreeSet<_>>();
                    if ids.is_empty(){return Err("empty physical scene pattern".into());}
                    Condition::All(ids.into_iter().map(|id|
                        Condition::Not(Box::new(Condition::Unknown(Expr::DeviceOn(id.clone()))))
                    ).collect())
                } else {
                    Condition::All(patterns[&selected].iter().map(|(id,power)| Condition::Equal(Expr::DeviceOn(id.clone()),Expr::Literal(Value::Bool(*power == homesense_core::scene_pattern::LightPower::On)))).collect())
                };
                nodes.push(Node {source:format!("indicator:{}:physical",link.id), action:Action::If {
                    condition,
                    then_actions:link.leds.iter().map(|id|Node {source:format!("indicator:{}",link.id),action:Action::Command {device:id.clone(),command:Command::PanelLed(on)}}).collect(),
                    else_actions:vec![],
                }});
                desired = Some(on);
            }
            let (plan, read) = feedback.plan_indicator_rule(&nodes,variables,Budget::default(),admission.monotonic_ms(),max_age,&BTreeMap::new(),policy)?;
            if plan.commands.is_empty() {desired = None;}
            lease = Some(read);
            Ok(plan)
        }).await;
        let prepared = match prepared {
            Ok(p) => p,
            Err(PreparationFailure::Admission(WriteStatus::ResourceBusy)) => return Ok(()),
            Err(PreparationFailure::Admission(e)) => {
                return Err(format!("indicator snapshot: {e:?}"));
            }
            Err(PreparationFailure::Planning(e)) => return Err(e),
        };
        let Some(desired) = desired else { return Ok(()); };
        self.submit_physical(admission, controller, link, feedback, generation,
            prepared, lease.ok_or("indicator feedback lease missing")?, desired,
            selection_value.ok_or("physical indicator selection missing")?,
            group.reconciliation.as_ref().is_some_and(|p| p.policy == homesense_config::scene_switches::ReconciliationPolicy::PowerPatternThenProfileLevel)).await
    }

    #[allow(clippy::too_many_arguments)]
    async fn submit_physical(
        &mut self, admission: &Admission, controller: uuid::Uuid,
        link: &homesense_config::indicator_links::Link, feedback: &Feedback,
        generation: crate::admission::TaskGeneration, mut prepared: PreparedRule,
        lease: crate::feedback::RuleLease, desired: bool, selected: i64, include_levels: bool,
    ) -> Result<(), String> {
        let key=(link.id.clone(),link.leds.clone());
        match admission
            .feedback_settled(controller, generation.clone(), lease.clone())
            .await
        {
            Ok(true) => {}
            Ok(false) | Err(WriteStatus::ResourceBusy) => return Ok(()),
            Err(e) => return Err(format!("{e:?}")),
        }
        self.evaluations.insert(key.clone(),Evaluation::Desired(desired));
        let fingerprint=feedback.indicator_fingerprint(&lease,include_levels)?;
        let prior=self.physical_attempted.get(&key).filter(|p|p.generation==generation && p.controller==controller);
        let output=match admission.indicator_outputs(controller,generation.clone(),link.leds.clone(),prior.map(|p|p.commands.clone()).unwrap_or_default()).await {
            Ok(output)=>output,Err(WriteStatus::ResourceBusy)=>return Ok(()),Err(e)=>return Err(format!("indicator output fence: {e:?}")),
        };
        if output.fence.values().any(|state|state.requested.as_ref().is_some_and(|r|matches!(r.status.as_str(),"accepted"|"writing"|"uncertain"|"failed"))){return Ok(());}
        if let Some(prior)=prior && prior.desired==desired {
            // An uncompleted/uncertain attempt is never replayed. A later
            // semantic input or successful independent LED override is a new
            // reconciliation cause only after this attempt reached Written.
            if !output.prior_complete {return Ok(());}
            let overridden=output.fence.iter().any(|(id,current)|current.written.as_ref().is_some_and(|w|
                w.color!=Some(desired) && prior.output_fence.get(id).and_then(|out|out.written.as_ref()).is_none_or(|old|w.sequence>old.sequence)
            ));
            if (prior.fingerprint==fingerprint || prior.selection!=selected) && !overridden {
                // A different selected member may leave this LED's false value
                // unchanged. Advance its evidence baseline without rewriting it;
                // a same-selection semantic round trip remains a distinct edge.
                if let Some(cached)=self.physical_attempted.get_mut(&key){cached.fingerprint=fingerprint;cached.selection=selected;cached.output_fence=output.fence;}
                return Ok(());
            }
        }
        prepared.attach_feedback(Some(lease));
        prepared.attach_indicator_outputs(output.fence.clone());
        let previous = self.physical_attempted.insert(key.clone(),Attempt{desired,selection:selected,generation,controller,fingerprint,output_fence:output.fence,commands:vec![]});
        match prepared.commit_status(admission).await {
            Err(WriteStatus::ResourceBusy | WriteStatus::InvalidValueInRequest) => {
                if let Some(previous) = previous {
                    self.physical_attempted.insert(key.clone(), previous);
                } else {
                    self.physical_attempted.remove(&key);
                }
            }
            Ok(ids) => {if !ids.is_empty(){self.emitted.push(ids.clone());}
                        if let Some(attempt)=self.physical_attempted.get_mut(&key){attempt.commands=ids;}}
            Err(e) => log::warn!(
                "indicator {} admission failed: {e:?}; no automatic replay",
                link.id
            ),
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn power_link(
        &mut self, admission: &Admission, controller: uuid::Uuid,
        link: &homesense_config::indicator_links::Link, feedback: &Feedback,
        max_age: u64, generation: crate::admission::TaskGeneration,
    ) -> Result<(), String> {
        use homesense_config::indicator_links::PowerPolicy;
        let mut lease = None;
        let mut desired = None;
        let mut known_input = false;
        let all_on = Condition::All(link.lights.iter().map(|id| Condition::Equal(
            Expr::DeviceOn(id.clone()), Expr::Literal(Value::Bool(true)))).collect());
        let known = Condition::All(link.lights.iter().map(|id|
            Condition::Not(Box::new(Condition::Unknown(Expr::DeviceOn(id.clone()))))).collect());
        let writes = |on| link.leds.iter().map(|id| Node {
            source: format!("indicator:{}:power",link.id),
            action: Action::Command { device:id.clone(), command:Command::PanelLed(on) },
        }).collect();
        let inner = match link.power_policy {
            PowerPolicy::FollowAllOn => Node { source:format!("indicator:{}:all",link.id),
                action:Action::If { condition:all_on, then_actions:writes(true), else_actions:writes(false) } },
            PowerPolicy::ClearIfAnyOff => Node { source:format!("indicator:{}:negative",link.id),
                action:Action::If { condition:Condition::Not(Box::new(all_on)), then_actions:writes(false), else_actions:vec![] } },
            PowerPolicy::LegacyOff => return Ok(()),
        };
        let flag = format!("indicator:{}:known",link.id);
        let nodes = vec![Node { source:flag.clone(),
            action:Action::If { condition:known, then_actions:vec![Node {
                source:flag.clone(), action:Action::Set { variable:flag.clone(), value:Expr::Literal(Value::Bool(true)) },
            },inner], else_actions:vec![] } }];
        let prepared = PreparedRule::prepare_indicator(admission, controller, generation.clone(), vec![], |variables, _, policy| {
            let mut variables = variables.clone();
            variables.insert(flag.clone(),homesense_core::rule::Variable { value_type:homesense_core::rule::ValueType::Bool,
                lifetime:homesense_core::rule::Lifetime::Temporary, value:None });
            let (plan, read) = feedback.plan_indicator_rule(&nodes,&variables,Budget::default(),admission.monotonic_ms(),max_age,&BTreeMap::new(),policy)?;
            known_input = plan.variables.get(&flag).is_some_and(|v| v.value == Some(Value::Bool(true)));
            desired = plan.commands.first().and_then(|c| match c.command {Command::PanelLed(on)=>Some(on),_=>None});
            lease=Some(read);
            Ok(plan)
        }).await;
        let mut prepared = match prepared {
            Ok(p)=>p, Err(PreparationFailure::Admission(WriteStatus::ResourceBusy))=>return Ok(()),
            Err(PreparationFailure::Admission(e))=>return Err(format!("power indicator: {e:?}")),
            Err(PreparationFailure::Planning(e))=>return Err(e),
        };
        let Some(desired)=desired else {
            self.evaluations.insert((link.id.clone(),link.leds.clone()),if known_input {Evaluation::NotRequired}else{Evaluation::Unknown});
            return Ok(());
        };
        prepared.attach_indicator_power(PowerProof { link:link.id.clone(), lights:link.lights.clone(),
            leds:link.leds.clone(), policy:link.power_policy, desired,
            lease:lease.as_ref().ok_or("power indicator lease missing")?.clone() });
        self.submit_physical(admission,controller,link,feedback,generation,prepared,
            lease.ok_or("power indicator lease missing")?,desired,i64::from(desired),false).await
    }

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
        self.evaluations.clear();self.visited.clear();
        if !document.indicator_links.iter().any(|l| l.enabled) {
            return Ok(());
        }
        let generation = match admission.task_generation().await {
            Ok(value) => value,
            Err(WriteStatus::ResourceBusy) => return Ok(()),
            Err(error) => return Err(format!("{error:?}")),
        };
        // Preserve controller authorization, but historical settings no longer
        // replace current physical lamp feedback in LED conditions.
        let _history = match admission
            .recorded_dimmer_levels(controller, generation.clone())
            .await
        {
            Ok(value) => value,
            Err(WriteStatus::ResourceBusy) => return Ok(()),
            Err(error) => return Err(format!("{error:?}")),
        };
        let snapshot = feedback.snapshot();
        admission
            .remember_levels(generation.epochs.configuration, snapshot.clone(), max_age)
            .await
            .map_err(|e| format!("indicator restore memory: {e:?}"))?;
        let mut pending = admission
            .projection_levels(generation.epochs.configuration, snapshot.cursor.boot_id)
            .await
            .map_err(|e| format!("indicator pending targets: {e:?}"))?;
        // Relay commands have an internal policy entry too, but physical relay
        // feedback is their authority. Only dimmer transitions defer LEDs.
        pending.retain(|id,_|snapshot.devices.get(id).is_some_and(|state|state.kind==homesense_core::ModuleKind::Dimmer));
        let recorded = BTreeMap::new();
        // Delayed scene/fade output is not yet a settled off observation.
        // Conservatively defer while any scheduled scene is still active.
        if !scenes.publication_impact().is_empty() {
            return Ok(());
        }
        // Shared snapshot/restore preparation can exceed the short per-wave
        // planning quantum on Linux with a full catalog. Start that soft
        // quantum here, while retaining the original hard delivery deadline.
        // Otherwise every wave can visit zero links forever.
        let planning_deadline=self.prepare_deadline.map(|hard|hard.min(admission.monotonic_ms().saturating_add(100)));
        for link in document.indicator_links.iter().filter(|l| l.enabled) {
            if planning_deadline.is_some_and(|deadline|admission.monotonic_ms()>=deadline) || !admission.source_valid() {break;}
            let key=(link.id.clone(),link.leds.clone());self.visited.push(key.clone());
            if link.power_policy != homesense_config::indicator_links::PowerPolicy::LegacyOff {
                self.power_link(admission,controller,link,feedback,max_age,generation.clone()).await?;
                continue;
            }
            if let Some(scene) = &link.scene
                && let Some(group) = document.scene_switch_groups.iter().find(|g| {
                    g.members.iter().any(|m| &m.scene == scene)
                        && g.reconciliation.as_ref().is_some_and(|p| p.enabled)
                })
            {
                self.physical_link(
                    admission,
                    controller,
                    document,
                    group,
                    link,
                    feedback,
                    max_age,
                    generation.clone(),
                )
                .await?;
                continue;
            }
            // A restore target is not current scene truth. Reviewed physical
            // links above independently require a complete, fresh projection,
            // settled feedback and no pending native operation. Keep the
            // conservative target wait for legacy/nonphysical links only.
            if link.lights.iter().any(|id| pending.contains_key(id)) {
                continue;
            }
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
            let physical_scene = link.scene.as_ref().is_some_and(|scene| {
                document.scene_switch_groups.iter().any(|g| {
                    g.members.iter().any(|m| &m.scene == scene)
                        && g.reconciliation.as_ref().is_some_and(|p| p.enabled)
                })
            });
            let physical_levels = BTreeMap::new();
            let prepared = PreparedRule::prepare_indicator(
                admission,
                controller,
                generation.clone(),
                names,
                |variables, selections, policy| {
                    relative_pending = selections.values().any(|selection| {
                        selection
                            .physical
                            .as_ref()
                            .is_some_and(|physical| physical.pending_relative)
                    });
                    let (plan, read) = feedback.plan_indicator_rule(
                        &nodes,
                        variables,
                        Budget::default(),
                        admission.monotonic_ms(),
                        max_age,
                        if physical_scene {
                            &physical_levels
                        } else {
                            &recorded
                        },
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
                    self.evaluations.insert(key.clone(),Evaluation::NotRequired);
                    self.attempted.insert(key.clone(), (false, vec![]));
                }
                continue;
            }
            self.evaluations.insert(key.clone(),Evaluation::Desired(false));
            let mode_clearing = !prepared.changed_variables.is_empty();
            let versions = prepared.input_versions();
            let previous_ids=self.legacy_commands.get(&key).cloned().unwrap_or_default();
            let output=match admission.indicator_outputs(controller,generation.clone(),link.leds.clone(),previous_ids).await {
                Ok(value)=>value,Err(WriteStatus::ResourceBusy)=>continue,Err(error)=>return Err(format!("indicator fence: {error:?}")),
            };
            let overridden=output.prior_complete && output.fence.values().any(|value|value.written.as_ref().is_some_and(|written|written.color!=Some(false)));
            if self.attempted.get(&key).is_some_and(|(attempted,old)|*attempted && (!mode_clearing || old==&versions)) && !overridden {
                continue;
            }
            prepared.attach_feedback(Some(lease));
            prepared.attach_indicator_outputs(output.fence);
            // Record before awaiting so dropping the future cannot retry an
            // admission that may already be durable. Undo only known rejection.
            let previous = self.attempted.insert(key.clone(), (true, versions));
            match prepared.commit_status(admission).await {
                // Both statuses are definite rejection before durable acceptance.
                // Do not reuse this plan: the next tick captures fresh state/CAS.
                Err(WriteStatus::ResourceBusy | WriteStatus::InvalidValueInRequest) => {
                    if let Some(previous) = previous {
                        self.attempted.insert(key.clone(), previous);
                    } else {
                        self.attempted.remove(&key);
                    }
                }
                Ok(ids) => {if !ids.is_empty(){self.legacy_commands.insert(key.clone(),ids.clone());self.emitted.push(ids);}}
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
