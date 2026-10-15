//! Read-only blocker views. Unknown ownership stays visible for manual review.
use crate::{Report, program::Code};
use serde_json::{Value, json};
use std::collections::BTreeSet;

pub fn blockers(report: &Report, room: &str) -> Result<Value, String> {
    let selected: BTreeSet<_> = report
        .devices
        .iter()
        .filter(|d| d.room.as_deref() == Some(room))
        .map(|d| d.control4_id.as_str())
        .collect();
    if selected.is_empty() {
        return Err("room has no imported HopeBeauty devices".into());
    }
    let known: BTreeSet<_> = report
        .devices
        .iter()
        .filter(|d| d.room.is_some())
        .map(|d| d.control4_id.as_str())
        .collect();
    fn targets<'a>(nodes: &'a [Code], out: &mut BTreeSet<&'a str>) {
        for node in nodes {
            if let Some(id) = &node.target {
                out.insert(id);
            }
            targets(&node.children, out);
        }
    }
    let mut programs = Vec::new();
    let mut unassigned_programs = Vec::new();
    for program in &report.programs {
        let mut ids = BTreeSet::new();
        if let Some(id) = program.owner.split(':').nth(1) {
            ids.insert(id);
        }
        targets(&program.nodes, &mut ids);
        if !ids.is_disjoint(&selected) {
            programs.push(program);
        } else if ids.iter().any(|id| !known.contains(id)) || ids.is_empty() {
            unassigned_programs.push(program);
        }
    }
    let mut scenes = Vec::new();
    let mut unassigned_scenes = Vec::new();
    for scene in &report.scenes {
        let ids: BTreeSet<_> = scene
            .members
            .iter()
            .map(|m| m.control4_id.as_str())
            .collect();
        if !ids.is_disjoint(&selected) {
            scenes.push(scene);
        } else if ids.is_empty() || ids.iter().any(|id| !known.contains(id)) {
            unassigned_scenes.push(scene);
        }
    }
    Ok(json!({"source":report.source,"room":room,
        "devices":report.devices.iter().filter(|d|selected.contains(d.control4_id.as_str())).collect::<Vec<_>>(),
        "programs_pending_translation":programs,"scenes_pending_translation":scenes,
        "unassigned_programs_review_required":unassigned_programs,"unassigned_scenes_review_required":unassigned_scenes,
        "global_diagnostics":report.diagnostics,
        "unresolved_references_review_required":report.references.iter().filter(|r|!r.resolved).collect::<Vec<_>>(),
        "automatic_enable":false,
        "limitation":"Membership follows imported HopeBeauty owner/target IDs; unknown ownership and unresolved references remain global review items. Program trees retain source containers and code IDs; no program execution is implemented."}))
}
