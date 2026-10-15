//! Compose existing native selection planning with reviewed Immediate ASTs.
//! Provider decisions remain snapshot based; planned assignments are never
//! promoted into physical/inferred selection evidence.
use crate::{
    admission::{Admission, TaskGeneration},
    scene_switches,
};
use homesense_config::rule_document::Document;
use homesense_core::rule::{Action, Expr, Node, Value};
use homesense_storage::scene_observation::Snapshot;
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;
pub(crate) enum Failure {
    Planning(String),
    Snapshot(hap::server::WriteStatus),
}
pub(crate) struct Plan {
    pub actions: Vec<Node>,
    pub selection_reads: BTreeMap<String, Snapshot>,
    pub provider_set_sources: BTreeSet<String>,
}
pub(crate) async fn capture(
    admission: &Admission,
    controller: Uuid,
    generation: &TaskGeneration,
    document: &Document,
    writes: &[(String, Option<bool>)],
) -> Result<Plan, Failure> {
    if writes.is_empty()
        || writes.len() > 64
        || writes
            .iter()
            .map(|(id, _)| id)
            .collect::<BTreeSet<_>>()
            .len()
            != writes.len()
        || writes
            .iter()
            .any(|(id, _)| document.scene_execution_kind(id).is_none())
    {
        return Err(Failure::Planning(
            "manual scene write missing, disabled, duplicated or unreviewed".into(),
        ));
    }
    let relevant = document
        .scene_switch_groups
        .iter()
        .filter(|g| {
            writes.iter().any(|(id, value)| {
                value.is_some() && g.members.iter().any(|m| m.scene == *id)
                    || *value == Some(true) && g.reset_scenes.contains(id)
            })
        })
        .map(|g| g.variable.clone())
        .collect::<Vec<_>>();
    let reads = if relevant.is_empty() {
        BTreeMap::new()
    } else {
        admission
            .scene_selection_snapshot(controller, generation.clone(), relevant)
            .await
            .map_err(Failure::Snapshot)?
    };
    compose(document, writes, reads).map_err(Failure::Planning)
}
pub(crate) fn compose(
    document: &Document,
    writes: &[(String, Option<bool>)],
    selection_reads: BTreeMap<String, Snapshot>,
) -> Result<Plan, String> {
    let snapshot = selection_reads
        .iter()
        .map(|(name, s)| (name.clone(), s.stored.clone()))
        .collect();
    let timed_writes = writes
        .iter()
        .filter(|(id, _)| document.timed_scenes.contains_key(id))
        .cloned()
        .collect::<Vec<_>>();
    // The full original provider owns real call order, shared-reset deduplication
    // and all selection updates. Local plans below locate provenance only.
    let joint = scene_switches::plan(document, &timed_writes, &snapshot)?;
    let mut updates = joint
        .updates
        .into_iter()
        .map(|u| (u.name.clone(), u))
        .collect::<BTreeMap<_, _>>();
    let mut at: BTreeMap<usize, Vec<Node>> = BTreeMap::new();
    let mut origins: BTreeMap<String, BTreeSet<(usize, String)>> = BTreeMap::new();
    let mut provider_set_sources = BTreeSet::new();
    for group in &document.scene_switch_groups {
        let indices = writes
            .iter()
            .enumerate()
            .filter(|(_, (id, _))| {
                group.members.iter().any(|m| m.scene == *id) || group.reset_scenes.contains(id)
            })
            .map(|(i, _)| i)
            .collect::<Vec<_>>();
        if indices.is_empty() {
            continue;
        }
        let group_writes = indices
            .iter()
            .map(|i| writes[*i].clone())
            .collect::<Vec<_>>();
        let local = scene_switches::plan(document, &group_writes, &snapshot)?;
        let selected = selection_reads
            .get(&group.variable)
            .and_then(|s| match s.stored.value {
                Some(Value::Integer(v)) => Some(v),
                _ => None,
            });
        let anchor = indices
            .iter()
            .copied()
            .find(|i| writes[*i].1 == Some(true))
            .or_else(|| {
                indices.iter().copied().find(|i| {
                    writes[*i].1 == Some(false)
                        && group
                            .members
                            .iter()
                            .any(|m| m.scene == writes[*i].0 && selected == Some(m.value))
                })
            })
            .unwrap_or(indices[0]);
        for definition in local.definitions {
            let slot = indices
                .iter()
                .copied()
                .find(|i| writes[*i].1 == Some(true) && writes[*i].0 == definition)
                .unwrap_or(anchor);
            origins
                .entry(definition)
                .or_default()
                .insert((slot, format!("{}/{}", group.id, writes[slot].0)));
        }
        if let Some(update) = updates.remove(&group.variable) {
            if selection_reads
                .get(&update.name)
                .is_none_or(|s| s.stored.version != update.expected_version)
            {
                return Err("native provider version mismatch".into());
            }
            let Some(Value::Integer(value)) = update.value else {
                return Err("native provider selection type mismatch".into());
            };
            let source = format!("manual-native/{}/selection", group.id);
            provider_set_sources.insert(source.clone());
            at.entry(anchor).or_default().push(Node {
                source,
                action: Action::Set {
                    variable: update.name,
                    value: Expr::Literal(Value::Integer(value)),
                },
            });
        }
    }
    if !updates.is_empty() {
        return Err("native provider returned an unbound selection".into());
    }
    let mut roots = Vec::new();
    for (index, (id, value)) in writes.iter().enumerate() {
        if let Some(mut nodes) = at.remove(&index) {
            roots.append(&mut nodes);
        }
        if document.immediate_scenes.contains_key(id) && *value == Some(true) {
            roots.push(Node {
                source: format!("manual-scene/{id}"),
                action: Action::Scene {
                    id: id.clone(),
                    activate: true,
                },
            });
        }
    }
    // Timed calls have no synchronous variable effects. Collect them in the
    // original provider order while leaving assignments at effective slots.
    // Offsets still start at the single durable acceptance clock.
    for definition in joint.definitions {
        let source = if let Some(parents) = origins.get(&definition) {
            format!(
                "manual-native/{}",
                parents
                    .iter()
                    .map(|(slot, id)| format!("{slot}:{id}"))
                    .collect::<Vec<_>>()
                    .join("|")
            )
        } else {
            format!("manual-scene/{definition}")
        };
        roots.push(Node {
            source,
            action: Action::Scene {
                id: definition,
                activate: true,
            },
        });
    }
    let actions = document
        .linked_manual_actions(&roots)
        .map_err(|e| e.to_string())?;
    Ok(Plan {
        actions,
        selection_reads,
        provider_set_sources,
    })
}
