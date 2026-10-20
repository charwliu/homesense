//! Manual Immediate roots use the ordinary ordered rule admission and causal
//! owner. These receipts never claim a timeline run or physical completion.
use crate::{
    admission::{Admission, TaskGeneration},
    automation_control::Reply,
    deferred_router::Router,
    feedback::Feedback,
    scene_scheduler::SceneScheduler,
};
use homesense_config::rule_document::Document;
use homesense_core::rule::Budget;
use std::time::Instant;
use uuid::Uuid;
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Receipt {
    pub scenes: Vec<String>,
    pub correlation: String,
    pub event: u64,
    pub commands: Vec<u64>,
    pub followup_runs: Vec<u64>,
    pub unknown_branches: Vec<String>,
}
#[allow(clippy::too_many_arguments)]
pub(crate) async fn writes(
    admission: &Admission,
    document: &Document,
    generation: &TaskGeneration,
    controller: Uuid,
    writes: &[(String, Option<bool>)],
    router: Option<&mut Router>,
    scheduler: &mut SceneScheduler,
    feedback: &Feedback,
    max_age: u64,
    budget: Budget,
    deadline: Instant,
) -> Result<Reply, String> {
    if writes.iter().any(|(id,value)| *value==Some(true) && (document.whole_house_off_scene()==Some(id.as_str())||document.whole_house_on_scene()==Some(id.as_str()))) {
        return Err("household Off requires its managed operation entry".into());
    }
    if writes.len()>1 && writes.iter().any(|(id,_)|document.whole_house_on_scene()==Some(id.as_str())) {
        return Err("household absolute action cannot share a scene batch".into());
    }
    if writes.is_empty()
        || writes.len() > 64
        || writes
            .iter()
            .map(|(id, _)| id)
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != writes.len()
        || writes
            .iter()
            .any(|(id, _)| document.scene_execution_kind(id).is_none())
    {
        return Err("scene missing, disabled, duplicated or unreviewed".into());
    }
    if !admission
        .diagnostics(controller, generation.clone())
        .await
        .map_err(|e| format!("{e:?}"))?
        .ready()
    {
        return Err("immediate scene runtime unavailable".into());
    }
    if Instant::now() >= deadline {
        return Err("immediate scene request expired".into());
    }
    let ids = writes
        .iter()
        .filter(|(id, value)| value.is_some()||document.scene_presentations.contains_key(id))
        .map(|(id, _)| id.clone())
        .collect::<Vec<_>>();
    if !writes.iter().any(|(id, value)| {
        *value == Some(true) || document.scene_presentations.contains_key(id) || value.is_some() && crate::scene_switches::related(document, id)
    }) {
        return Ok(Reply::ScenesAccepted(vec![]));
    }
    let positive = writes
        .iter()
        .filter(|(_, v)| *v == Some(true))
        .map(|(id, _)| id.clone())
        .collect::<Vec<_>>();
    if !positive.is_empty() {
        document
            .linked_manual_scenes(&positive)
            .map_err(|e| e.to_string())?;
    }
    let deadline = admission
        .monotonic_ms()
        .checked_add(
            u64::try_from(
                deadline
                    .saturating_duration_since(Instant::now())
                    .as_millis(),
            )
            .map_err(|_| "request deadline overflow")?,
        )
        .ok_or("request deadline overflow")?;
    let router = router.ok_or("immediate scene requires the runtime causal owner")?;
    let outcomes = router
        .start_manual_scenes_as(
            crate::deferred_router::ManualRoot {
                controller,
                writes,
                generation,
                deadline,
            },
            scheduler,
            Some((feedback, max_age)),
            budget,
        )
        .await?;
    let first = outcomes
        .first()
        .ok_or("manual scene causal receipt missing")?;
    Ok(Reply::ImmediateAccepted(Receipt {
        scenes: ids,
        correlation: format!("{:032x}", first.cause.correlation_id()),
        event: first.cause.event_id(),
        commands: outcomes
            .iter()
            .flat_map(|o| o.command_ids.iter().copied())
            .collect(),
        followup_runs: outcomes
            .iter()
            .flat_map(|o| o.scene_runs.iter().copied())
            .collect(),
        unknown_branches: outcomes
            .iter()
            .flat_map(|o| o.unknown_branches.iter().cloned())
            .collect(),
    }))
}
