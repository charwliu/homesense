//! Read-only publication impact. This is not a reservation or permission to publish.
use homesense_config::Configuration;
#[derive(Debug, PartialEq, Eq, serde::Serialize)]
pub struct Run {
    pub run: u64,
    pub audit_id: String,
    pub definition: Option<String>,
    pub status: String,
}
#[derive(Debug, PartialEq, Eq, serde::Serialize)]
pub struct Preview {
    pub boot: String,
    pub active_revision: u64,
    pub candidate_revision: u64,
    pub connection_rebuild_required: bool,
    pub interrupted_runs: Vec<Run>,
    pub invalidates_old_rules_and_timers: bool,
}
pub fn connection_rebuild_required(current: &Configuration, candidate: &Configuration) -> bool {
    current.gateways().len() != candidate.gateways().len()
        || current.gateways().iter().any(|(id, gateway)| {
            candidate.gateways().get(id).is_none_or(|next| {
                gateway.endpoint != next.endpoint || gateway.modules() != next.modules()
            })
        })
}
