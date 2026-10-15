//! Immutable physical scope attached atomically to one module-control receipt.
use crate::Error;
use homesense_core::{Command, queue::QueuedCommand};
use rusqlite::{Transaction, params};
#[derive(Debug, Clone)]
pub struct Effects {
    pub command: u64,
    pub module_id: String,
    pub gateway_id: String,
    pub address: u8,
    pub profile: String,
    pub devices: Vec<(u8, String)>,
}
pub(crate) fn insert(
    tx: &Transaction<'_>,
    boot: &str,
    commands: &[QueuedCommand],
    effects: &[Effects],
) -> Result<(), Error> {
    let module_commands: Vec<_> = commands
        .iter()
        .filter(|c| c.command == Command::ModuleAllOff)
        .collect();
    if module_commands.len() != effects.len() {
        return Err(Error::InvalidAudit);
    }
    let mut seen = std::collections::BTreeSet::new();
    for effect in effects {
        if !seen.insert(effect.command)
            || !module_commands.iter().any(|c| c.id == effect.command)
            || effect.address == 0
            || effect.module_id.is_empty()
            || effect.gateway_id.is_empty()
            || !matches!(
                effect.profile.as_str(),
                "r8i8_din_pro_v2_7_7" | "ad8_din_rs485_v2_2_1"
            )
            || effect.devices.len() != 8
        {
            return Err(Error::InvalidAudit);
        }
        let channels: std::collections::BTreeSet<_> =
            effect.devices.iter().map(|(c, _)| *c).collect();
        let devices: std::collections::BTreeSet<_> =
            effect.devices.iter().map(|(_, d)| d.as_str()).collect();
        if channels.iter().copied().ne(1..=8)
            || devices.len() != 8
            || devices.iter().any(|s| s.is_empty() || s.len() > 256)
        {
            return Err(Error::InvalidAudit);
        }
        for (channel, device) in &effect.devices {
            tx.execute("INSERT INTO module_off_effect(boot_id,command_id,module_id,gateway_id,address,profile,channel,device_id) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",params![boot,i64::try_from(effect.command).map_err(|_|Error::InvalidAudit)?,effect.module_id,effect.gateway_id,effect.address,effect.profile,channel,device])?;
        }
    }
    Ok(())
}
