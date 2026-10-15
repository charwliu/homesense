//! Immutable physical/dependency scope for accepted runtime work.
//! These receipts are not executable plans or authentication capabilities.
//! The owner validates current local readiness and original authority before
//! calling acceptance, and repeats those gates before durable Writing.
use crate::{Error, Store, sql_revision};
use homesense_core::{
    Command,
    gateway_scope::WireScope,
    queue::{Epochs, QueuedCommand},
};
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) fn migrate(tx: &Transaction<'_>) -> Result<(), Error> {
    tx.execute_batch("ALTER TABLE command_journal ADD COLUMN gateway_id TEXT;
        ALTER TABLE command_journal ADD COLUMN gateway_epoch INTEGER
            CHECK((gateway_id IS NULL AND gateway_epoch IS NULL) OR
                  (gateway_id IS NOT NULL AND length(gateway_id)>0 AND gateway_epoch IS NOT NULL AND gateway_epoch>0));
        CREATE INDEX command_gateway_pending ON command_journal(boot_id,revision,gateway_id,gateway_epoch,status);
        CREATE INDEX command_led_wire_scope ON command_journal(boot_id,revision,device_id,transport_epoch,control_epoch,gateway_id,gateway_epoch,command_id DESC)
            WHERE intent IN (x'01060000',x'01060100',x'010A0000');
        CREATE TABLE gateway_operation (
            boot_id TEXT NOT NULL, kind TEXT NOT NULL, operation_id TEXT NOT NULL,
            revision INTEGER NOT NULL REFERENCES configuration_revision(revision),
            transport_epoch INTEGER NOT NULL CHECK(transport_epoch>0),
            control_epoch INTEGER NOT NULL CHECK(control_epoch>0),
            status TEXT NOT NULL CHECK(status IN ('active','completed','cancelled','partial')),
            updated_ms INTEGER NOT NULL CHECK(updated_ms>=0),
            version INTEGER NOT NULL CHECK(version>=0),
            PRIMARY KEY(boot_id,kind,operation_id));
        CREATE TABLE gateway_operation_wire (
            boot_id TEXT NOT NULL, kind TEXT NOT NULL, operation_id TEXT NOT NULL,
            gateway_id TEXT NOT NULL CHECK(length(gateway_id)>0),
            gateway_epoch INTEGER NOT NULL CHECK(gateway_epoch>0),
            PRIMARY KEY(boot_id,kind,operation_id,gateway_id),
            FOREIGN KEY(boot_id,kind,operation_id) REFERENCES gateway_operation(boot_id,kind,operation_id));
        CREATE INDEX gateway_operation_fault ON gateway_operation_wire(boot_id,gateway_id,gateway_epoch);
        CREATE TABLE gateway_command_requirement (
            boot_id TEXT NOT NULL, command_id INTEGER NOT NULL,
            gateway_id TEXT NOT NULL CHECK(length(gateway_id)>0),
            gateway_epoch INTEGER NOT NULL CHECK(gateway_epoch>0),
            PRIMARY KEY(boot_id,command_id,gateway_id),
            FOREIGN KEY(boot_id,command_id) REFERENCES command_journal(boot_id,command_id));
        CREATE INDEX gateway_command_fault ON gateway_command_requirement(boot_id,gateway_id,gateway_epoch,command_id);
        CREATE TABLE gateway_command_operation (
            boot_id TEXT NOT NULL, command_id INTEGER NOT NULL, kind TEXT NOT NULL, operation_id TEXT NOT NULL,
            PRIMARY KEY(boot_id,command_id,kind,operation_id),
            FOREIGN KEY(boot_id,command_id) REFERENCES command_journal(boot_id,command_id),
            FOREIGN KEY(boot_id,kind,operation_id) REFERENCES gateway_operation(boot_id,kind,operation_id));
        CREATE TABLE gateway_fault (
            boot_id TEXT NOT NULL, revision INTEGER NOT NULL REFERENCES configuration_revision(revision),
            gateway_id TEXT NOT NULL, gateway_epoch INTEGER NOT NULL CHECK(gateway_epoch>0),
            updated_ms INTEGER NOT NULL CHECK(updated_ms>=0),
            PRIMARY KEY(boot_id,revision,gateway_id,gateway_epoch));
        CREATE TRIGGER immutable_command_wire BEFORE UPDATE OF gateway_id,gateway_epoch ON command_journal
            WHEN OLD.gateway_id IS NOT NULL BEGIN SELECT RAISE(ABORT,'immutable command wire'); END;
        CREATE TRIGGER immutable_command_requirement_update BEFORE UPDATE ON gateway_command_requirement
            BEGIN SELECT RAISE(ABORT,'immutable command requirement'); END;
        CREATE TRIGGER immutable_command_requirement_delete BEFORE DELETE ON gateway_command_requirement
            BEGIN SELECT RAISE(ABORT,'immutable command requirement'); END;
        CREATE TRIGGER immutable_operation_wire_update BEFORE UPDATE ON gateway_operation_wire
            BEGIN SELECT RAISE(ABORT,'immutable operation wire'); END;
        CREATE TRIGGER immutable_operation_wire_delete BEFORE DELETE ON gateway_operation_wire
            BEGIN SELECT RAISE(ABORT,'immutable operation wire'); END;
        CREATE TRIGGER immutable_operation_scope BEFORE UPDATE OF revision,transport_epoch,control_epoch ON gateway_operation
            BEGIN SELECT RAISE(ABORT,'immutable operation generation'); END;")?;
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum OperationKind {
    Scene,
    Native,
    Rule,
    Delay,
    Fade,
    Hold,
    Panel,
    Calendar,
}
impl OperationKind {
    fn text(self) -> &'static str {
        match self {
            Self::Scene => "scene",
            Self::Native => "native",
            Self::Rule => "rule",
            Self::Delay => "delay",
            Self::Fade => "fade",
            Self::Hold => "hold",
            Self::Panel => "panel",
            Self::Calendar => "calendar",
        }
    }
    fn parse(s: &str) -> Result<Self, Error> {
        Ok(match s {
            "scene" => Self::Scene,
            "native" => Self::Native,
            "rule" => Self::Rule,
            "delay" => Self::Delay,
            "fade" => Self::Fade,
            "hold" => Self::Hold,
            "panel" => Self::Panel,
            "calendar" => Self::Calendar,
            _ => return Err(Error::InvalidAudit),
        })
    }
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct OperationKey {
    pub kind: OperationKind,
    pub id: String,
}
#[derive(Clone, Debug)]
pub struct OperationScope {
    pub key: OperationKey,
    pub global: Epochs,
    pub wires: Vec<WireScope>,
}
#[derive(Clone, Debug)]
pub struct CommandScope {
    pub command: u64,
    pub physical: WireScope,
    /// Decision reads, including a different bus used by a predicate.
    pub requirements: Vec<WireScope>,
    /// Original retained-operation scopes are inherited, never recaptured.
    pub operations: Vec<OperationKey>,
}
#[derive(Clone, Debug, Default)]
pub struct Acceptance {
    pub commands: Vec<CommandScope>,
    pub operations: Vec<OperationScope>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FaultAudit {
    pub cancelled: Vec<u64>,
    pub uncertain: Vec<u64>,
    pub operations: Vec<OperationKey>,
}
fn valid(s: &str, bound: usize) -> bool {
    !s.trim().is_empty() && s.len() <= bound && !s.chars().any(char::is_control)
}
fn number(n: u64) -> Result<i64, Error> {
    i64::try_from(n).map_err(|_| Error::InvalidAudit)
}
fn wire_map(wires: &[WireScope]) -> Result<BTreeMap<String, u64>, Error> {
    if wires.len() > homesense_core::gateway_scope::MAX_GATEWAYS {
        return Err(Error::InvalidAudit);
    }
    let mut result = BTreeMap::new();
    for wire in wires {
        if !valid(wire.gateway(), 256)
            || wire.epoch() == 0
            || number(wire.epoch()).is_err()
            || result.insert(wire.gateway().into(), wire.epoch()).is_some()
        {
            return Err(Error::InvalidAudit);
        }
    }
    Ok(result)
}
fn stored_wires(
    tx: &Transaction<'_>,
    boot: &str,
    key: &OperationKey,
) -> Result<BTreeMap<String, u64>, Error> {
    let mut query=tx.prepare("SELECT gateway_id,gateway_epoch FROM gateway_operation_wire WHERE boot_id=?1 AND kind=?2 AND operation_id=?3 ORDER BY gateway_id")?;
    let rows = query.query_map(params![boot, key.kind.text(), key.id], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
    })?;
    rows.map(|row| {
        let (id, epoch) = row?;
        Ok((id, u64::try_from(epoch).map_err(|_| Error::InvalidAudit)?))
    })
    .collect()
}
fn ensure_unfaulted(
    tx: &Transaction<'_>,
    boot: &str,
    revision: u64,
    wires: &BTreeMap<String, u64>,
) -> Result<(), Error> {
    for (id, epoch) in wires {
        let failed:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM gateway_fault WHERE boot_id=?1 AND revision=?2 AND gateway_id=?3 AND gateway_epoch>=?4)",params![boot,sql_revision(revision)?,id,number(*epoch)?],|r|r.get(0))?;
        if failed {
            return Err(Error::Conflict);
        }
    }
    Ok(())
}
pub(crate) fn insert_operations(
    tx: &Transaction<'_>,
    boot: &str,
    operations: &[OperationScope],
    active: Option<i64>,
    now: u64,
) -> Result<(), Error> {
    if !valid(boot, 128) || operations.len() > 128 {
        return Err(Error::InvalidAudit);
    }
    let mut seen = BTreeSet::new();
    for scope in operations {
        if !valid(&scope.key.id, 256)
            || !seen.insert(scope.key.clone())
            || active != Some(sql_revision(scope.global.configuration)?)
        {
            return Err(Error::InvalidAudit);
        }
        let wires = wire_map(&scope.wires)?;
        ensure_unfaulted(tx, boot, scope.global.configuration, &wires)?;
        let existing:Option<(i64,i64,i64,String)>=tx.query_row("SELECT revision,transport_epoch,control_epoch,status FROM gateway_operation WHERE boot_id=?1 AND kind=?2 AND operation_id=?3",params![boot,scope.key.kind.text(),scope.key.id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
        if let Some((revision, transport, control, status)) = existing {
            if revision != sql_revision(scope.global.configuration)?
                || transport != sql_revision(scope.global.transport)?
                || control != sql_revision(scope.global.control)?
                || status != "active"
                || stored_wires(tx, boot, &scope.key)? != wires
            {
                return Err(Error::Conflict);
            }
            continue;
        }
        tx.execute("INSERT INTO gateway_operation(boot_id,kind,operation_id,revision,transport_epoch,control_epoch,status,updated_ms,version) VALUES(?1,?2,?3,?4,?5,?6,'active',?7,0)",params![boot,scope.key.kind.text(),scope.key.id,sql_revision(scope.global.configuration)?,sql_revision(scope.global.transport)?,sql_revision(scope.global.control)?,number(now)?])?;
        for (id, epoch) in wires {
            tx.execute("INSERT INTO gateway_operation_wire(boot_id,kind,operation_id,gateway_id,gateway_epoch) VALUES(?1,?2,?3,?4,?5)",params![boot,scope.key.kind.text(),scope.key.id,id,number(epoch)?])?;
        }
    }
    Ok(())
}

/// Called inside the same transaction as command/variable/scene acceptance.
pub(crate) fn insert(
    tx: &Transaction<'_>,
    boot: &str,
    commands: &[QueuedCommand],
    scopes: &Acceptance,
    active: Option<i64>,
    now: u64,
) -> Result<(), Error> {
    if scopes.commands.len() != commands.len() {
        return Err(Error::InvalidAudit);
    }
    insert_operations(tx, boot, &scopes.operations, active, now)?;
    let mut seen = BTreeSet::new();
    for scope in &scopes.commands {
        if !seen.insert(scope.command) || scope.operations.len() > 128 {
            return Err(Error::InvalidAudit);
        }
        let command = commands
            .iter()
            .find(|c| c.id == scope.command)
            .ok_or(Error::InvalidAudit)?;
        let mut requirements = wire_map(&scope.requirements)?;
        if let Some(old) =
            requirements.insert(scope.physical.gateway().into(), scope.physical.epoch())
            && old != scope.physical.epoch()
        {
            return Err(Error::Conflict);
        }
        if !valid(scope.physical.gateway(), 256) || scope.physical.epoch() == 0 {
            return Err(Error::InvalidAudit);
        }
        let mut linked = BTreeSet::new();
        for key in &scope.operations {
            if !valid(&key.id, 256) || !linked.insert(key.clone()) {
                return Err(Error::InvalidAudit);
            }
            let stored:Option<(i64,i64,i64,String)>=tx.query_row("SELECT revision,transport_epoch,control_epoch,status FROM gateway_operation WHERE boot_id=?1 AND kind=?2 AND operation_id=?3",params![boot,key.kind.text(),key.id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
            if stored
                != Some((
                    sql_revision(command.epochs.configuration)?,
                    sql_revision(command.epochs.transport)?,
                    sql_revision(command.epochs.control)?,
                    "active".into(),
                ))
            {
                return Err(Error::Conflict);
            }
            for (gateway, epoch) in stored_wires(tx, boot, key)? {
                if let Some(old) = requirements.insert(gateway, epoch)
                    && old != epoch
                {
                    return Err(Error::Conflict);
                }
            }
            tx.execute("INSERT INTO gateway_command_operation(boot_id,command_id,kind,operation_id) VALUES(?1,?2,?3,?4)",params![boot,number(command.id)?,key.kind.text(),key.id])?;
        }
        if requirements.len() > homesense_core::gateway_scope::MAX_GATEWAYS {
            return Err(Error::InvalidAudit);
        }
        ensure_unfaulted(tx, boot, command.epochs.configuration, &requirements)?;
        let changed=tx.execute("UPDATE command_journal SET gateway_id=?1,gateway_epoch=?2 WHERE boot_id=?3 AND command_id=?4 AND status='accepted' AND gateway_id IS NULL",params![scope.physical.gateway(),number(scope.physical.epoch())?,boot,number(command.id)?])?;
        if changed != 1 {
            return Err(Error::Conflict);
        }
        if command.command == Command::ModuleAllOff {
            let mismatch:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM module_off_effect WHERE boot_id=?1 AND command_id=?2 AND gateway_id!=?3)",params![boot,number(command.id)?,scope.physical.gateway()],|r|r.get(0))?;
            if mismatch {
                return Err(Error::Conflict);
            }
        }
        for (gateway, epoch) in requirements {
            tx.execute("INSERT INTO gateway_command_requirement(boot_id,command_id,gateway_id,gateway_epoch) VALUES(?1,?2,?3,?4)",params![boot,number(command.id)?,gateway,number(epoch)?])?;
        }
    }
    Ok(())
}

impl Store {
    pub fn gateway_fault_recorded(&self,boot:&str,revision:u64,wire:&WireScope)->Result<bool,Error> {
        Ok(self.connection.query_row("SELECT EXISTS(SELECT 1 FROM gateway_fault WHERE boot_id=?1 AND revision=?2 AND gateway_id=?3 AND gateway_epoch=?4)",
            params![boot,sql_revision(revision)?,wire.gateway(),number(wire.epoch())?],|row|row.get(0))?)
    }
    /// Original retained dependency set, never inferred from its next command.
    pub fn gateway_operation_scope(&self,boot:&str,key:&OperationKey)->Result<Option<(OperationScope,String)>,Error> {
        let row:Option<(i64,i64,i64,String)>=self.connection.query_row(
            "SELECT revision,transport_epoch,control_epoch,status FROM gateway_operation WHERE boot_id=?1 AND kind=?2 AND operation_id=?3",
            params![boot,key.kind.text(),key.id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
        let Some((configuration,transport,control,status))=row else{return Ok(None);};
        let mut query=self.connection.prepare("SELECT gateway_id,gateway_epoch FROM gateway_operation_wire WHERE boot_id=?1 AND kind=?2 AND operation_id=?3 ORDER BY gateway_id")?;
        let wires=query.query_map(params![boot,key.kind.text(),key.id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?)))?
            .map(|row|{let(id,epoch)=row?;WireScope::from_persisted(id,u64::try_from(epoch).map_err(|_|Error::InvalidAudit)?).map_err(|_|Error::InvalidAudit)})
            .collect::<Result<Vec<_>,_>>()?;
        Ok(Some((OperationScope {key:key.clone(),global:Epochs {
            configuration:u64::try_from(configuration).map_err(|_|Error::InvalidAudit)?,
            transport:u64::try_from(transport).map_err(|_|Error::InvalidAudit)?,
            control:u64::try_from(control).map_err(|_|Error::InvalidAudit)?,
        },wires},status)))
    }
    /// A completed local reconnect supersedes old terminal receipts, even at
    /// equal monotonic timestamps. Pending receipts still require fault audit;
    /// no connection change silently replays or settles an Accepted command.
    pub fn feedback_is_settled_in_wire_scope(
        &self,
        boot: &str,
        device: &homesense_core::DeviceId,
        observed_ms: Option<u64>,
        global: Epochs,
        wire: &WireScope,
    ) -> Result<bool, Error> {
        if !valid(boot, 128) {
            return Err(Error::InvalidAudit);
        }
        let missing:bool=self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM command_journal WHERE boot_id=?1 AND device_id=?2 AND status IN ('accepted','writing') AND gateway_id IS NULL)",
            params![boot,device.as_str()],|r|r.get(0))?;
        if missing {
            return Err(Error::InvalidAudit);
        }
        let sample = observed_ms.map(number).transpose()?;
        let blocked:bool=self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM command_journal WHERE boot_id=?1 AND device_id=?2 AND gateway_id=?3 AND (status IN ('accepted','writing') OR (status IN ('written','observed','failed','uncertain') AND (?4 IS NULL OR (transport_epoch>=?5 AND gateway_epoch>=?6 AND updated_ms>=?4)))))",
            params![boot,device.as_str(),wire.gateway(),sample,sql_revision(global.transport)?,sql_revision(wire.epoch())?],|r|r.get(0))?;
        Ok(!blocked)
    }
    /// Reserve dependency receipts before installing non-command retained work.
    /// Scene/native-specific pending receipt creation must use this insert in
    /// its own acceptance transaction; this public primitive alone is not enough
    /// to authorize a runnable scene or bypass original source/variable gates.
    pub fn reserve_gateway_operations(
        &mut self,
        boot: &str,
        scopes: &[OperationScope],
        now: u64,
    ) -> Result<(), Error> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let active: Option<i64> = tx
            .query_row(
                "SELECT revision FROM active_configuration WHERE singleton=1",
                [],
                |r| r.get(0),
            )
            .optional()?;
        insert_operations(&tx, boot, scopes, active, now)?;
        tx.commit()?;
        Ok(())
    }
    /// Atomic, target-local fault closure. A writing B frame is not recalled or
    /// marked uncertain because its parent operation depended on failed A.
    /// The returned identities drive owned queue/task cleanup without guessing
    /// physical ownership from logical or synthetic receipt IDs.
    pub fn fail_gateway(
        &mut self,
        boot: &str,
        global: Epochs,
        wire: &WireScope,
        now: u64,
    ) -> Result<FaultAudit, Error> {
        if !valid(boot, 128) || !valid(wire.gateway(), 256) || wire.epoch() == 0 {
            return Err(Error::InvalidAudit);
        }
        let at = number(now)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let active: Option<i64> = tx
            .query_row(
                "SELECT revision FROM active_configuration WHERE singleton=1",
                [],
                |r| r.get(0),
            )
            .optional()?;
        if active != Some(sql_revision(global.configuration)?) {
            return Err(Error::Conflict);
        }
        // Never infer mandatory scope for current-boot legacy pending work.
        let missing:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM command_journal WHERE boot_id=?1 AND status IN ('accepted','writing') AND (gateway_id IS NULL OR gateway_epoch IS NULL))",[boot],|r|r.get(0))?;
        if missing {
            return Err(Error::InvalidAudit);
        }
        let mut result = FaultAudit::default();
        {
            let mut query=tx.prepare("SELECT j.command_id,j.status,j.gateway_id,j.gateway_epoch,j.intent,j.updated_ms,EXISTS(SELECT 1 FROM gateway_command_requirement r WHERE r.boot_id=j.boot_id AND r.command_id=j.command_id AND r.gateway_id=?3 AND r.gateway_epoch<=?4) FROM command_journal j WHERE j.boot_id=?1 AND j.revision=?2 AND j.status IN ('accepted','writing') ORDER BY j.command_id")?;
            let rows = query.query_map(
                params![
                    boot,
                    sql_revision(global.configuration)?,
                    wire.gateway(),
                    number(wire.epoch())?
                ],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, i64>(3)?,
                        r.get::<_, Vec<u8>>(4)?,
                        r.get::<_, i64>(5)?,
                        r.get::<_, bool>(6)?,
                    ))
                },
            )?;
            for row in rows {
                let (id, status, gateway, epoch, intent, updated, dependent) = row?;
                let physical = gateway == wire.gateway() && epoch <= number(wire.epoch())?;
                if !physical && !dependent {
                    continue;
                }
                if updated > at {
                    return Err(Error::Conflict);
                }
                let id = u64::try_from(id).map_err(|_| Error::InvalidAudit)?;
                if status == "writing" {
                    if physical {
                        result.uncertain.push(id);
                    }
                } else if physical || intent != [1, 5, 2, 0] {
                    result.cancelled.push(id);
                }
            }
        }
        for id in &result.cancelled {
            if tx.execute("UPDATE command_journal SET status='cancelled',cancellation_reason='GatewayTransportFailed',updated_ms=?3 WHERE boot_id=?1 AND command_id=?2 AND status='accepted'",params![boot,number(*id)?,at])?!=1 { return Err(Error::Conflict); }
        }
        for id in &result.uncertain {
            if tx.execute("UPDATE command_journal SET status='uncertain',updated_ms=?3 WHERE boot_id=?1 AND command_id=?2 AND status='writing'",params![boot,number(*id)?,at])?!=1 { return Err(Error::Conflict); }
        }
        let affected = {
            let mut query=tx.prepare("SELECT o.kind,o.operation_id,o.updated_ms,o.version FROM gateway_operation o WHERE o.boot_id=?1 AND o.revision=?2 AND o.status='active' AND EXISTS(SELECT 1 FROM gateway_operation_wire w WHERE w.boot_id=o.boot_id AND w.kind=o.kind AND w.operation_id=o.operation_id AND w.gateway_id=?3 AND w.gateway_epoch<=?4) ORDER BY o.kind,o.operation_id")?;
            query
                .query_map(
                    params![
                        boot,
                        sql_revision(global.configuration)?,
                        wire.gateway(),
                        number(wire.epoch())?
                    ],
                    |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, i64>(2)?,
                            r.get::<_, i64>(3)?,
                        ))
                    },
                )?
                .collect::<Result<Vec<_>, _>>()?
        };
        for (kind, id, updated, version) in affected {
            if updated > at || version == i64::MAX {
                return Err(Error::Conflict);
            }
            let has_effect:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM gateway_command_operation c JOIN command_journal j USING(boot_id,command_id) WHERE c.boot_id=?1 AND c.kind=?2 AND c.operation_id=?3 AND j.status IN ('writing','written','observed','uncertain'))",params![boot,kind,id],|r|r.get(0))?;
            let status = if has_effect { "partial" } else { "cancelled" };
            if tx.execute("UPDATE gateway_operation SET status=?4,updated_ms=?5,version=version+1 WHERE boot_id=?1 AND kind=?2 AND operation_id=?3 AND status='active' AND version=?6",params![boot,kind,id,status,at,version])?!=1 { return Err(Error::Conflict); }
            // Production native operations are explicitly minted from the
            // durable first phase's command id. Preserve that exact birth link;
            // never capture a fresh scope for a later native phase after fault.
            if kind=="native" && let Some(birth)=id.parse::<u64>().ok().and_then(|id|id.checked_sub(1u64<<62)) {
                let birth=birth.to_string();
                tx.execute("UPDATE native_scene_request SET status='unknown',selected=NULL,updated_ms=?6 WHERE boot_id=?1 AND revision=?2 AND transport_epoch=?3 AND control_epoch=?4 AND status='pending' AND (initial_commands=?5 OR substr(initial_commands,1,instr(initial_commands,',')-1)=?5)",params![boot,sql_revision(global.configuration)?,number(global.transport)?,number(global.control)?,birth,at])?;
            }
            result.operations.push(OperationKey {
                kind: OperationKind::parse(&kind)?,
                id,
            });
        }
        let backwards:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM gateway_fault WHERE boot_id=?1 AND revision=?2 AND gateway_id=?3 AND gateway_epoch=?4 AND updated_ms>?5)",params![boot,sql_revision(global.configuration)?,wire.gateway(),number(wire.epoch())?,at],|r|r.get(0))?;
        if backwards {
            return Err(Error::Conflict);
        }
        tx.execute("INSERT INTO gateway_fault(boot_id,revision,gateway_id,gateway_epoch,updated_ms) VALUES(?1,?2,?3,?4,?5) ON CONFLICT DO NOTHING",params![boot,sql_revision(global.configuration)?,wire.gateway(),number(wire.epoch())?,at])?;
        tx.commit()?;
        Ok(result)
    }
    /// Exact original dependency rows (including inherited operation wires).
    /// Missing mandatory scope is an invariant failure, never guessed from IDs.
    pub fn command_gateway_requirements(&self,boot:&str,command:u64)->Result<Vec<WireScope>,Error> {
        let physical=self.command_wire_scope(boot,command)?.ok_or(Error::InvalidAudit)?;
        let mut query=self.connection.prepare("SELECT gateway_id,gateway_epoch FROM gateway_command_requirement WHERE boot_id=?1 AND command_id=?2 ORDER BY gateway_id")?;
        let wires=query.query_map(params![boot,number(command)?],|r|Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?)))?
            .map(|row|{let(id,epoch)=row?;WireScope::from_persisted(id,u64::try_from(epoch).map_err(|_|Error::InvalidAudit)?).map_err(|_|Error::InvalidAudit)})
            .collect::<Result<Vec<_>,_>>()?;
        if wires.len()>homesense_core::gateway_scope::MAX_GATEWAYS || !wires.contains(&physical){return Err(Error::InvalidAudit);}
        Ok(wires)
    }
    pub fn command_wire_scope(&self, boot: &str, command: u64) -> Result<Option<WireScope>, Error> {
        let row:Option<(Option<String>,Option<i64>)>=self.connection.query_row("SELECT gateway_id,gateway_epoch FROM command_journal WHERE boot_id=?1 AND command_id=?2",params![boot,number(command)?],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        match row {
            None => Ok(None),
            Some((Some(gateway), Some(epoch))) => Ok(Some(
                WireScope::from_persisted(
                    gateway,
                    u64::try_from(epoch).map_err(|_| Error::InvalidAudit)?,
                )
                .map_err(|_| Error::InvalidAudit)?,
            )),
            Some(_) => Err(Error::InvalidAudit),
        }
    }
}

pub(crate) fn check_before_write(
    tx: &Transaction<'_>,
    boot: &str,
    command: u64,
    physical: &WireScope,
) -> Result<bool, Error> {
    let(row_gateway,row_epoch,intent,revision):(Option<String>,Option<i64>,Vec<u8>,i64)=tx.query_row("SELECT gateway_id,gateway_epoch,intent,revision FROM command_journal WHERE boot_id=?1 AND command_id=?2",params![boot,number(command)?],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
    if row_gateway.as_deref() != Some(physical.gateway())
        || row_epoch != Some(number(physical.epoch())?)
    {
        return Err(Error::Conflict);
    }
    let own_failed:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM gateway_fault WHERE boot_id=?1 AND revision=?2 AND gateway_id=?3 AND gateway_epoch>=?4)",params![boot,revision,physical.gateway(),number(physical.epoch())?],|r|r.get(0))?;
    if own_failed {
        return Ok(false);
    }
    // An already accepted safe STOP survives cancellation of its parent run,
    // never a fault or epoch change of its own physical bus.
    if intent == [1, 5, 2, 0] {
        return Ok(true);
    }
    let ended:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM gateway_command_operation c JOIN gateway_operation o USING(boot_id,kind,operation_id) WHERE c.boot_id=?1 AND c.command_id=?2 AND o.status!='active') OR EXISTS(SELECT 1 FROM gateway_command_requirement r JOIN gateway_fault f ON f.boot_id=r.boot_id AND f.gateway_id=r.gateway_id AND f.gateway_epoch>=r.gateway_epoch WHERE r.boot_id=?1 AND r.command_id=?2 AND f.revision=?3)",params![boot,number(command)?,revision],|r|r.get(0))?;
    Ok(!ended)
}

#[cfg(test)]
#[path = "gateway_audit_tests.rs"]
mod tests;
