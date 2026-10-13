//! Administrator read requests use ordinary durable queue tickets, never a socket.
use homesense_config::Configuration;
use homesense_core::{Channel, DeviceId, ExecutionContext, ModuleKind};
use serde::{Deserialize, Serialize};

pub(crate) fn matches_feedback(
    admission: &crate::admission::Admission,
    generation: &crate::admission::TaskGeneration,
    feedback: &crate::feedback::Feedback,
    revision: u64,
) -> bool {
    feedback.is_active()
        && feedback.configuration_revision() == revision
        && feedback.transport_epoch() == generation.epochs.transport
        && uuid::Uuid::parse_str(admission.boot_id())
            .is_ok_and(|boot| boot.as_u128() == feedback.snapshot().cursor.boot_id)
}

#[cfg(test)]
#[path = "admin_module_query_tests.rs"]
mod tests;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Scope {
    Channel { channel: u8 },
    WholeModule,
}
#[derive(Debug, Clone)]
pub(crate) struct Target {
    pub module_id: String,
    pub channel: Option<u8>,
}
impl Target {
    pub(crate) fn frame(
        &self,
        config: &Configuration,
        context: ExecutionContext,
        epoch: u64,
    ) -> Result<(String, hb_transport::PreparedFrame), String> {
        let (gateway, module) = config.module(&self.module_id).ok_or("module missing")?;
        let frame = match self.channel {
            None => hb_transport::PreparedFrame::prepare_module_query(module, context, epoch),
            Some(channel) => hb_transport::PreparedFrame::prepare(
                module,
                Channel::new(channel).map_err(|e| e.to_string())?,
                homesense_core::Command::Query,
                context,
                epoch,
            ),
        }
        .map_err(|e| e.to_string())?;
        Ok((gateway.gateway.to_owned(), frame))
    }
}
pub(crate) fn plan(
    config: &Configuration,
    context: ExecutionContext,
    module_id: &str,
    scope: &Scope,
) -> Result<Vec<Target>, String> {
    let (_, module) = config
        .module(module_id)
        .ok_or("persistent module missing")?;
    if !matches!(module.kind(), ModuleKind::Switch | ModuleKind::Dimmer) {
        return Err("module does not support output query".into());
    }
    let channels = match scope {
        Scope::Channel { channel } => vec![Some(*channel)],
        Scope::WholeModule if hb_protocol::supports_module_query(module.profile()) => vec![None],
        Scope::WholeModule => (1..=module.channels()).map(Some).collect(),
    };
    let targets: Vec<_> = channels
        .into_iter()
        .map(|channel| Target {
            module_id: module_id.to_owned(),
            channel,
        })
        .collect();
    for target in &targets {
        target.frame(config, context, 1)?;
    }
    Ok(targets)
}
pub(crate) fn receipt_device(id: u64) -> DeviceId {
    DeviceId::new(format!("module-query-ticket:{id}")).expect("private query ticket")
}
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct Accepted {
    pub boot: String,
    pub revision: String,
    pub transport_epoch: String,
    pub commands: Vec<String>,
}
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct Status {
    pub writes: Vec<Write>,
    pub channels: Vec<crate::management_modules::Slot>,
    pub fresh_channels: usize,
    pub requested_channels: usize,
    pub feedback_causality: &'static str,
    pub atomic_snapshot: bool,
}
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct Write {
    pub command: String,
    pub status: String,
    pub updated_ms: String,
}
pub(crate) fn project_status(
    records: Vec<homesense_storage::module_history::Record>,
    module: &crate::management_modules::Module,
    scope: &Scope,
    expected_epoch: u64,
    expected_commands: &[u64],
) -> Result<Status, String> {
    if records.len() != expected_commands.len()
        || records.iter().any(|r| {
            r.transport_epoch != expected_epoch
                || !expected_commands.contains(&r.command)
                || r.intent != [1, 0, 0, 0]
        })
    {
        return Err("query receipts outside current scope".into());
    }
    let writes = records
        .into_iter()
        .map(|r| Write {
            command: r.command.to_string(),
            status: r.status,
            updated_ms: r.updated_ms.to_string(),
        })
        .collect();
    let channels: Vec<_> = module
        .slots
        .iter()
        .filter(|slot| match scope {
            Scope::Channel { channel } => slot.channel == *channel,
            Scope::WholeModule => true,
        })
        .cloned()
        .collect();
    if channels.is_empty() {
        return Err("query channel missing".into());
    }
    let fresh_channels = channels.iter().filter(|c| c.quality == "confirmed").count();
    Ok(Status {
        writes,
        requested_channels: channels.len(),
        channels,
        fresh_channels,
        feedback_causality: "not_established",
        atomic_snapshot: false,
    })
}
