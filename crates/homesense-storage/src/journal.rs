//! Durable command audit, never a replay queue. Runtime owns queue reservation
//! and must not dispatch before acceptance commits.
use crate::{Error, Store, sql_revision};
use homesense_core::{
    Command, LevelTransition, MotorMotion,
    queue::{Cancellation, Origin, QueuedCommand},
};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
type CompletionReceipt<'a> = (&'a str, &'a crate::keypad_completion::Commit, Option<&'a dyn Fn() -> u64>);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Accepted,
    Writing,
    Written,
    Observed,
    Cancelled,
    Failed,
    Uncertain,
}
impl Status {
    fn text(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Writing => "writing",
            Self::Written => "written",
            Self::Observed => "observed",
            Self::Cancelled => "cancelled",
            Self::Failed => "failed",
            Self::Uncertain => "uncertain",
        }
    }
    pub(crate) fn parse(text: &str) -> Result<Self, Error> {
        match text {
            "accepted" => Ok(Self::Accepted),
            "writing" => Ok(Self::Writing),
            "written" => Ok(Self::Written),
            "observed" => Ok(Self::Observed),
            "cancelled" => Ok(Self::Cancelled),
            "failed" => Ok(Self::Failed),
            "uncertain" => Ok(Self::Uncertain),
            _ => Err(Error::InvalidAudit),
        }
    }
}
/// Read-only motor audit. Only Written retains a trustworthy write timestamp;
/// Observed.updated_ms is an observation timestamp and must not stand in for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MotorReceipt {
    pub status: Status,
    pub written_at_ms: Option<u64>,
}
fn boot(value: &str) -> Result<(), Error> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
    {
        Err(Error::InvalidAudit)
    } else {
        Ok(())
    }
}
fn integer(value: u64) -> Result<i64, Error> {
    i64::try_from(value).map_err(|_| Error::InvalidAudit)
}
/// Version 1 audit encoding: version, action, value, reserved. Not bus bytes.
fn intent(command: Command) -> [u8; 4] {
    let (tag, value) = match command {
        Command::Query => (0, 0),
        Command::Switch(v) => (1, u8::from(v)),
        Command::SetLevel { level, transition } => (
            if transition == LevelTransition::Immediate {
                2
            } else {
                3
            },
            level.get(),
        ),
        Command::PauseRamp => (4, 0),
        Command::Motor(m) => (
            5,
            match m {
                MotorMotion::Up => 0,
                MotorMotion::Down => 1,
                MotorMotion::Stop => 2,
            },
        ),
        Command::PanelLedOff => (10,0),
        Command::PanelLed(v) => (6, u8::from(v)),
        Command::PanelParameter(parameter) => {let (command,value)=parameter.bytes();return [1,9,command,value];},
        Command::Toggle => (7, 0),
        Command::ModuleAllOff => (8, 0),
    };
    [1, tag, value, 0]
}
/// Exclusive, non-cloneable reservation whose transport capability has not
/// been released to the caller. Dropping it leaves Writing conservatively;
/// ordinary Writing transitions still cannot become Cancelled.
#[must_use]
pub struct UndeliveredWrite<'a> {
    store: &'a mut Store,
    boot: String,
    id: u64,
}
impl UndeliveredWrite<'_> {
    /// Consume the reservation before constructing or delivering a ticket.
    pub fn release(self) {}
    /// A plain light request may expire during its synchronous Writing commit.
    /// Only this still-exclusive capability can prove no transmitter received
    /// it. Motor/STOP/module operations and released tickets are excluded.
    pub fn cancel_expired_light_before_delivery(self, command:Command, now_ms:u64)->Result<(),Error> {
        if !matches!(command,Command::Switch(_)|Command::SetLevel{..}|Command::Toggle) {return Err(Error::InvalidAudit)}
        let tx=self.store.connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let count=tx.execute("UPDATE command_journal SET status='cancelled',updated_ms=?1,cancellation_reason='ExpiredBeforeDelivery' WHERE boot_id=?2 AND command_id=?3 AND status='writing' AND updated_ms<=?1 AND deadline_ms<=?1 AND intent=?4",params![integer(now_ms)?,self.boot,integer(self.id)?,intent(command).as_slice()])?;
        if count!=1 {return Err(Error::Conflict)}
        tx.commit()?;Ok(())
    }
    /// A guarded projection can have a shorter wire lease than its original
    /// total wait budget. This reservation still proves no ticket was delivered.
    pub fn cancel_expired_projection_before_delivery(self,now_ms:u64,effective_deadline:u64)->Result<(),Error> {
        self.store.cancel_projection_before_delivery(&self.boot,&[(self.id,effective_deadline)],now_ms,homesense_core::queue::CancelReason::Expired)
    }
    /// Only expired LEDs, and only while the exclusive reservation is held.
    /// No connection/ticket exists outside the admission owner at this point.
    pub fn cancel_expired_led_before_delivery(self, now_ms:u64)->Result<(),Error> {
        let tx=self.store.connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let count=tx.execute("UPDATE command_journal SET status='cancelled',updated_ms=?1,cancellation_reason='ExpiredBeforeDelivery' WHERE boot_id=?2 AND command_id=?3 AND status='writing' AND updated_ms<=?1 AND deadline_ms<=?1 AND intent IN (?4,?5,?6)",params![integer(now_ms)?,self.boot,integer(self.id)?,intent(Command::PanelLed(false)).as_slice(),intent(Command::PanelLed(true)).as_slice(),intent(Command::PanelLedOff).as_slice()])?;
        if count!=1 {return Err(Error::Conflict)}
        tx.commit()?;Ok(())
    }
}
impl Store {
    /// Caller must hold an undelivered reservation or the writer's sealed proof
    /// that this complete projection batch emitted zero bytes. Never use this
    /// for uncertain/partial I/O or ordinary actuator Writing obligations.
    pub fn cancel_projection_before_delivery(
        &mut self,
        boot_id: &str,
        members: &[(u64, u64)],
        now_ms: u64,
        reason: homesense_core::queue::CancelReason,
    ) -> Result<(), Error> {
        self.cancel_guarded_before_delivery(boot_id,members,now_ms,reason,false)
    }
    /// Only a synchronous writer's sealed all-members zero-byte proof may retire
    /// selected Away attempts. Partial/uncertain writes retain their obligations.
    pub fn cancel_guarded_away_before_delivery(&mut self,boot_id:&str,members:&[(u64,u64)],now_ms:u64,reason:homesense_core::queue::CancelReason)->Result<(),Error> {
        self.cancel_guarded_before_delivery(boot_id,members,now_ms,reason,true)
    }
    fn cancel_guarded_before_delivery(&mut self,boot_id:&str,members:&[(u64,u64)],now_ms:u64,reason:homesense_core::queue::CancelReason,away:bool)->Result<(),Error> {
        boot(boot_id)?;
        let expired = match reason {
            homesense_core::queue::CancelReason::Expired => true,
            homesense_core::queue::CancelReason::RunCancelled => false,
            _ => return Err(Error::InvalidAudit),
        };
        if members.is_empty()
            || members.len() > 128
            || members
                .iter()
                .map(|(id, _)| id)
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                != members.len()
            || members
                .iter()
                .any(|(_, deadline)| *deadline == 0 || (expired && now_ms < *deadline))
        {
            return Err(Error::InvalidAudit);
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        for (id, deadline) in members {
            let count=if away {
                tx.execute("UPDATE command_journal SET status='cancelled',updated_ms=?1,cancellation_reason=?2 WHERE boot_id=?3 AND command_id=?4 AND status='writing' AND updated_ms<=?1 AND deadline_ms=?5 AND ((intent=?6 AND (SELECT count(*) FROM module_off_effect e WHERE e.boot_id=?3 AND e.command_id=?4)=8) OR intent IN (?7,?8))",params![integer(now_ms)?,if expired {"Expired"}else{"RunCancelled"},boot_id,integer(*id)?,integer(*deadline)?,intent(Command::ModuleAllOff).as_slice(),intent(Command::PanelLed(false)).as_slice(),intent(Command::PanelLed(true)).as_slice()])?
            }else{
                tx.execute("UPDATE command_journal SET status='cancelled',updated_ms=?1,cancellation_reason=?2 WHERE boot_id=?3 AND command_id=?4 AND status='writing' AND updated_ms<=?1 AND deadline_ms>=?5 AND intent IN (?6,?7,?8)",params![integer(now_ms)?,if expired {"Expired"}else{"RunCancelled"},boot_id,integer(*id)?,integer(*deadline)?,intent(Command::PanelLed(false)).as_slice(),intent(Command::PanelLed(true)).as_slice(),intent(Command::PanelLedOff).as_slice()])?
            };
            if count != 1 {
                return Err(Error::Conflict);
            }
        }
        tx.commit()?;
        Ok(())
    }
    pub fn accept_module_off_commands(&mut self,boot_id:&str,commands:&[QueuedCommand],cancellations:&[Cancellation],now_ms:u64,effects:&[crate::module_off::Effects])->Result<(),Error> {
        self.accept_queue_batch_detailed(boot_id,commands,cancellations,now_ms,None,&[],None,None,&[],&[],None,effects,None,None,None,None)
    }
    /// All commands are committed together against the currently active revision.
    /// Duplicate IDs fail the whole transaction; this is not request idempotency.
    pub fn accept_commands(
        &mut self,
        boot_id: &str,
        commands: &[QueuedCommand],
        now_ms: u64,
    ) -> Result<(), Error> {
        self.accept_queue_batch(boot_id, commands, &[], now_ms)
    }
    /// Persist accepted commands and queue coalescing/STOP cancellations together.
    pub fn accept_queue_batch(
        &mut self,
        boot_id: &str,
        commands: &[QueuedCommand],
        cancellations: &[Cancellation],
        now_ms: u64,
    ) -> Result<(), Error> {
        self.accept_queue_batch_with_study(boot_id, commands, cancellations, now_ms, None)
    }
    pub fn study_mode(&self, revision: u64) -> Result<Option<u8>, Error> {
        Ok(self
            .connection
            .query_row(
                "SELECT value FROM study_mode WHERE revision=?1",
                [sql_revision(revision)?],
                |r| r.get(0),
            )
            .optional()?)
    }
    pub fn accept_queue_batch_with_study(
        &mut self,
        boot_id: &str,
        commands: &[QueuedCommand],
        cancellations: &[Cancellation],
        now_ms: u64,
        study: Option<(Option<u8>, u8)>,
    ) -> Result<(), Error> {
        self.accept_queue_batch_with_variables(boot_id, commands, cancellations, now_ms, study, &[])
    }
    /// Logical variable CAS and command acceptance share the same transaction.
    pub fn accept_queue_batch_with_variables(
        &mut self,
        boot_id: &str,
        commands: &[QueuedCommand],
        cancellations: &[Cancellation],
        now_ms: u64,
        study: Option<(Option<u8>, u8)>,
        variables: &[crate::variables::Update],
    ) -> Result<(), Error> {
        self.accept_queue_batch_with_rule_audit(
            boot_id,
            commands,
            cancellations,
            now_ms,
            study,
            variables,
            None,
        )
    }
    // One transaction owns the queue, variable CAS and optional causal audit.
    #[allow(clippy::too_many_arguments)]
    pub fn accept_queue_batch_with_rule_audit(
        &mut self,
        boot_id: &str,
        commands: &[QueuedCommand],
        cancellations: &[Cancellation],
        now_ms: u64,
        study: Option<(Option<u8>, u8)>,
        variables: &[crate::variables::Update],
        audit: Option<&crate::rule_audit::Admission>,
    ) -> Result<(), Error> {
        self.accept_queue_batch_with_audits(
            boot_id,
            commands,
            cancellations,
            now_ms,
            study,
            variables,
            audit,
            None,
        )
    }
    /// All causal links commit with the command rows, never after admission.
    #[allow(clippy::too_many_arguments)]
    pub fn accept_queue_batch_with_audits(
        &mut self,
        boot_id: &str,
        commands: &[QueuedCommand],
        cancellations: &[Cancellation],
        now_ms: u64,
        study: Option<(Option<u8>, u8)>,
        variables: &[crate::variables::Update],
        audit: Option<&crate::rule_audit::Admission>,
        scene: Option<&crate::scene_run::StepLink>,
    ) -> Result<(), Error> {
        self.accept_queue_batch_with_scene_starts(boot_id,commands,cancellations,now_ms,study,variables,audit,scene,&[])
    }
    /// Variables, immediate commands, rule audit and all deferred run receipts
    /// commit together. Receipts do not contain executable plans or replay work.
    #[allow(clippy::too_many_arguments)]
    pub fn accept_queue_batch_with_scene_starts(
        &mut self, boot_id:&str, commands:&[QueuedCommand], cancellations:&[Cancellation],
        now_ms:u64, study:Option<(Option<u8>,u8)>, variables:&[crate::variables::Update],
        audit:Option<&crate::rule_audit::Admission>, scene:Option<&crate::scene_run::StepLink>,
        starts:&[crate::scene_run::Start],
    )->Result<(),Error> {
        self.accept_queue_batch_with_read_checks(boot_id,commands,cancellations,now_ms,study,variables,audit,scene,starts,&[])
    }
    /// Output and actual variable changes share a transaction with condition-only
    /// version guards. An unchanged read never detaches its inferred provenance.
    #[allow(clippy::too_many_arguments)]
    pub fn accept_queue_batch_with_read_checks(
        &mut self, boot_id:&str, commands:&[QueuedCommand], cancellations:&[Cancellation],
        now_ms:u64, study:Option<(Option<u8>,u8)>, variables:&[crate::variables::Update],
        audit:Option<&crate::rule_audit::Admission>, scene:Option<&crate::scene_run::StepLink>,
        starts:&[crate::scene_run::Start], reads:&[crate::variables::ReadCheck],
    )->Result<(),Error> {
        self.accept_queue_batch_with_read_checks_and_sources(boot_id,commands,cancellations,now_ms,study,variables,audit,scene,starts,reads,None)
    }
    /// Exact source labels follow expanded physical commands by ordinal. They
    /// commit atomically with queue receipts, rule audit and variable updates.
    #[allow(clippy::too_many_arguments)]
    pub fn accept_queue_batch_with_read_checks_and_sources(
        &mut self, boot_id:&str, commands:&[QueuedCommand], cancellations:&[Cancellation],
        now_ms:u64, study:Option<(Option<u8>,u8)>, variables:&[crate::variables::Update],
        audit:Option<&crate::rule_audit::Admission>, scene:Option<&crate::scene_run::StepLink>,
        starts:&[crate::scene_run::Start], reads:&[crate::variables::ReadCheck],
        sources:Option<&[String]>,
    )->Result<(),Error> {
        self.accept_queue_batch_detailed(boot_id,commands,cancellations,now_ms,study,variables,audit,scene,starts,reads,sources,&[],None,None,None,None)
    }
    #[allow(clippy::too_many_arguments)]
    fn accept_queue_batch_detailed(
        &mut self, boot_id:&str, commands:&[QueuedCommand], cancellations:&[Cancellation],
        now_ms:u64, study:Option<(Option<u8>,u8)>, variables:&[crate::variables::Update],
        audit:Option<&crate::rule_audit::Admission>, scene:Option<&crate::scene_run::StepLink>,
        starts:&[crate::scene_run::Start], reads:&[crate::variables::ReadCheck],
        sources:Option<&[String]>,
        module_effects:&[crate::module_off::Effects],
        gateway_scopes:Option<&crate::gateway_audit::Acceptance>,
        canonical:Option<(&str,&[crate::component_modes::Canonical])>,
        phase:Option<&[crate::scene_run::PhaseStep]>,
        completion:Option<CompletionReceipt<'_>>,
    )->Result<(),Error> {
        crate::rule_audit::validate_sources(sources, commands.len())?;
        if sources.is_some() && audit.is_none() {return Err(Error::InvalidAudit);}
        if starts.len()>64 || (!starts.is_empty() && audit.is_none()) {return Err(Error::InvalidAudit);}
        if starts.iter().any(|s|audit.is_none_or(|a|s.revision!=a.revision)) {return Err(Error::Conflict);}
        boot(boot_id)?;
        if variables.len() > 1024 || (study.is_some() && commands.is_empty()) {
            return Err(Error::InvalidAudit);
        }
        if cancellations.len() > 4096 {
            return Err(Error::InvalidAudit);
        }
        if (commands.is_empty() && variables.is_empty() && audit.is_none()) || commands.len() > 128
        {
            return Err(Error::InvalidAudit);
        }
        let now = integer(now_ms)?;
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
        crate::variables::check_reads(&tx, reads, active)?;
        for command in commands {
            let revision = sql_revision(command.epochs.configuration)?;
            if active != Some(revision) {
                return Err(Error::Conflict);
            }
            if command.id == 0
                || command.device_id.as_str().len() > 256
                || command.epochs.transport == 0
                || command.epochs.control == 0
                || command.deadline_ms <= now_ms
            {
                return Err(Error::InvalidAudit);
            }
            let origin = match command.origin {
                Origin::Http => "http",
                Origin::Hap => "hap",
                Origin::Panel => "panel",
                Origin::Rule => "rule",
                Origin::Background => "background",
            };
            tx.execute("INSERT INTO command_journal(boot_id,command_id,revision,device_id,origin,intent,transport_epoch,control_epoch,deadline_ms,updated_ms,run_id,status) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,'accepted')",params![boot_id,integer(command.id)?,revision,command.device_id.as_str(),origin,intent(command.command).as_slice(),integer(command.epochs.transport)?,integer(command.epochs.control)?,integer(command.deadline_ms)?,now,command.run_id.map(integer).transpose()?])?;
        }
        crate::module_off::insert(&tx,boot_id,commands,module_effects)?;
        if let Some((expected, next)) = study {
            if next > 3
                || expected.is_some_and(|v| v > 3)
                || commands
                    .iter()
                    .any(|c| c.origin != Origin::Rule || c.run_id != Some(commands[0].id))
            {
                return Err(Error::InvalidAudit);
            }
            let revision = sql_revision(commands[0].epochs.configuration)?;
            let current: Option<u8> = tx
                .query_row(
                    "SELECT value FROM study_mode WHERE revision=?1",
                    [revision],
                    |r| r.get(0),
                )
                .optional()?;
            if current != expected {
                return Err(Error::Conflict);
            }
            tx.execute("INSERT INTO study_mode(revision,value) VALUES(?1,?2) ON CONFLICT(revision) DO UPDATE SET value=excluded.value",params![revision,next])?;
        }
        if let Some(phase)=phase {
            if scene.is_some()||phase.is_empty()||phase.len()>128{return Err(Error::InvalidAudit);}
            let mut covered=std::collections::BTreeSet::new();let mut steps=std::collections::BTreeSet::new();
            for step in phase {
                if step.link.run!=phase[0].link.run||step.link.sample.is_some()||!steps.insert(step.link.index){return Err(Error::InvalidAudit);}
                let mut rows=vec![];for id in &step.command_ids{
                    if !covered.insert(*id){return Err(Error::InvalidAudit);}
                    rows.push(commands.iter().find(|command|command.id==*id).cloned().ok_or(Error::InvalidAudit)?);
                }
                crate::scene_run::insert_step(&tx,boot_id,&step.link,&rows,active)?;
            }
            if covered.len()!=commands.len(){return Err(Error::InvalidAudit);}
        }else if let Some(scene)=scene {crate::scene_run::insert_step(&tx,boot_id,scene,commands,active)?;}

        if let Some(audit) = audit {
            crate::rule_audit::insert(&tx, boot_id, audit, commands, sources, now_ms, active)?;
        }
        for start in starts {
            crate::scene_run::insert_start(&tx,boot_id,start,now_ms,active)?;
            crate::scene_run::insert_cause(&tx,boot_id,start,audit.ok_or(Error::InvalidAudit)?)?;
        }
        if let Some(scopes) = gateway_scopes {
            crate::gateway_audit::insert(&tx, boot_id, commands, scopes, active, now_ms)?;
        }
        crate::variables::apply(&tx, variables, active)?;
        if let Some((controller,entries))=canonical{crate::component_modes::insert_canonical(&tx,boot_id,controller,variables,entries,now_ms,active)?;}
        if let Some((controller,completion,_))=completion{crate::keypad_completion::insert(&tx,boot_id,controller,commands,variables,completion,now_ms,active)?;}
        cancel_in_transaction(&tx, boot_id, cancellations, now)?;
        if let Some((_,receipt,Some(clock)))=completion {
            let commit_now=clock();
            if commit_now<now_ms || commit_now>=receipt.send_before_ms {return Err(Error::Conflict);}
        }
        tx.commit()?;
        Ok(())
    }
    /// Scoped runtime acceptance: physical route, decision dependencies and
    /// retained operation receipts commit with the queue/variable/rule audit.
    #[allow(clippy::too_many_arguments)]
    pub fn accept_queue_batch_scoped(
        &mut self, boot_id:&str, commands:&[QueuedCommand], cancellations:&[Cancellation],
        now_ms:u64, study:Option<(Option<u8>,u8)>, variables:&[crate::variables::Update],
        audit:Option<&crate::rule_audit::Admission>, scene:Option<&crate::scene_run::StepLink>,
        starts:&[crate::scene_run::Start], reads:&[crate::variables::ReadCheck],
        sources:Option<&[String]>, module_effects:&[crate::module_off::Effects],
        scopes:&crate::gateway_audit::Acceptance,
    )->Result<(),Error> {
        self.accept_queue_batch_detailed(boot_id,commands,cancellations,now_ms,study,
            variables,audit,scene,starts,reads,sources,module_effects,Some(scopes),None,None,None)
    }
    /// Only explicit local canonical metadata; all existing callers keep None.
    #[allow(clippy::too_many_arguments)]
    pub fn accept_queue_batch_with_components(&mut self,boot:&str,commands:&[QueuedCommand],cancelled:&[Cancellation],now:u64,study:Option<(Option<u8>,u8)>,variables:&[crate::variables::Update],audit:Option<&crate::rule_audit::Admission>,scene:Option<&crate::scene_run::StepLink>,starts:&[crate::scene_run::Start],reads:&[crate::variables::ReadCheck],sources:Option<&[String]>,scopes:Option<&crate::gateway_audit::Acceptance>,controller:&str,canonical:&[crate::component_modes::Canonical])->Result<(),Error>{
        self.accept_queue_batch_detailed(boot,commands,cancelled,now,study,variables,audit,scene,starts,reads,sources,&[],scopes,Some((controller,canonical)),None,None)
    }
    /// Every due member keeps its exact source index; all step links and
    /// expanded commands become durable in the original acceptance transaction.
    #[allow(clippy::too_many_arguments)]
    pub fn accept_queue_phase_with_components(&mut self,boot:&str,commands:&[QueuedCommand],cancelled:&[Cancellation],now:u64,study:Option<(Option<u8>,u8)>,variables:&[crate::variables::Update],audit:Option<&crate::rule_audit::Admission>,scene:Option<&crate::scene_run::StepLink>,starts:&[crate::scene_run::Start],reads:&[crate::variables::ReadCheck],sources:Option<&[String]>,scopes:Option<&crate::gateway_audit::Acceptance>,controller:&str,canonical:&[crate::component_modes::Canonical],phase:Option<&[crate::scene_run::PhaseStep]>)->Result<(),Error>{
        self.accept_queue_batch_detailed(boot,commands,cancelled,now,study,variables,audit,scene.filter(|_|phase.is_none()),starts,reads,sources,&[],scopes,Some((controller,canonical)),phase,None)
    }
    /// One source completion: component CAS/provenance, consumed gesture and
    /// bounded LED queue acceptance commit together. This does not deliver or
    /// replay LEDs and does not authorize a caller from receipt metadata.
    #[allow(clippy::too_many_arguments)]
    pub fn accept_keypad_completion(&mut self,boot:&str,commands:&[QueuedCommand],cancelled:&[Cancellation],
        now:u64,scopes:Option<&crate::gateway_audit::Acceptance>,controller:&str,completion:&crate::keypad_completion::Commit)->Result<(),Error>{
        self.ensure_keypad_completion_audit()?;
        self.accept_queue_batch_detailed(boot,commands,cancelled,now,None,std::slice::from_ref(&completion.update),None,None,&[],&[],None,&[],scopes,None,None,Some((controller,completion,None)))
    }
    /// Runtime path: check the live monotonic clock inside the transaction,
    /// after any database wait and before publishing its queue or variable CAS.
    #[allow(clippy::too_many_arguments)]
    pub fn accept_keypad_completion_before(&mut self,boot:&str,commands:&[QueuedCommand],cancelled:&[Cancellation],
        now:u64,scopes:Option<&crate::gateway_audit::Acceptance>,controller:&str,completion:&crate::keypad_completion::Commit,
        clock:&dyn Fn()->u64)->Result<(),Error>{
        self.ensure_keypad_completion_audit()?;
        self.accept_queue_batch_detailed(boot,commands,cancelled,now,None,std::slice::from_ref(&completion.update),None,None,&[],&[],None,&[],scopes,None,None,Some((controller,completion,Some(clock))))
    }
    /// Persist a publication/run barrier before attempting a configuration commit.
    /// Every snapshot must still be Accepted; any mismatch rolls back the batch.
    /// This never cancels Writing/Written commands or changes the active revision.
    pub fn cancel_queued(
        &mut self,
        boot_id: &str,
        cancellations: &[Cancellation],
        now_ms: u64,
    ) -> Result<(), Error> {
        boot(boot_id)?;
        if cancellations.len() > 4096 {
            return Err(Error::InvalidAudit);
        }
        let now = integer(now_ms)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        cancel_in_transaction(&tx, boot_id, cancellations, now)?;
        tx.commit()?;
        Ok(())
    }
    /// Close this boot's pending transport work atomically. Written commands
    /// remain written: loss of feedback is not proof their local write failed.
    pub fn fail_transport(&mut self, boot_id: &str, now_ms: u64) -> Result<(), Error> {
        boot(boot_id)?;
        let now = integer(now_ms)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let future: i64 = tx.query_row(
            "SELECT count(*) FROM command_journal WHERE boot_id=?1 AND status IN ('accepted','writing') AND updated_ms>?2",
            params![boot_id, now], |r| r.get(0))?;
        if future != 0 {
            return Err(Error::Conflict);
        }
        tx.execute("UPDATE command_journal SET status='cancelled',cancellation_reason='TransportFailed',updated_ms=?2 WHERE boot_id=?1 AND status='accepted'", params![boot_id, now])?;
        tx.execute("UPDATE command_journal SET status='uncertain',updated_ms=?2 WHERE boot_id=?1 AND status='writing'", params![boot_id, now])?;
        tx.commit()?;
        Ok(())
    }
    /// A feedback-based rule may not read through pending output or use a
    /// sample at/before the latest completed/ambiguous write. Cancelled output
    /// did not reach the transport and does not invalidate a prior observation.
    pub fn feedback_is_settled(
        &self,
        boot_id: &str,
        device: &homesense_core::DeviceId,
        observed_ms: Option<u64>,
    ) -> Result<bool, Error> {
        self.feedback_is_settled_in_epoch(boot_id,device,observed_ms,0)
    }
    /// A validated, freshly synchronized newer transport epoch supersedes old
    /// terminal commands even when both share a millisecond timestamp. Pending
    /// commands in any epoch and terminal commands in the current/future epoch
    /// still block. Epoch zero retains the conservative legacy behavior.
    pub fn feedback_is_settled_in_epoch(
        &self, boot_id: &str, device: &homesense_core::DeviceId,
        observed_ms: Option<u64>, transport_epoch: u64,
    ) -> Result<bool, Error> {
        boot(boot_id)?;
        let sample = observed_ms.map(integer).transpose()?;
        let blocked:bool=self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM command_journal WHERE boot_id=?1 AND device_id=?2 AND (status IN ('accepted','writing') OR (status IN ('written','observed','failed','uncertain') AND (?3 IS NULL OR (transport_epoch>=?4 AND updated_ms>=?3)))))",
            params![boot_id,device.as_str(),sample,integer(transport_epoch)?],|row|row.get(0))?;
        Ok(!blocked)
    }
    /// The same completion fence as `feedback_is_settled_in_epoch`, evaluated
    /// for a publication snapshot in one read. A relay reporting Off does not
    /// settle an outstanding write to another shared matrix component.
    pub fn unsettled_feedback_in_epoch(
        &self,
        boot_id: &str,
        observations: &std::collections::BTreeMap<homesense_core::DeviceId, Option<u64>>,
        transport_epoch: u64,
    ) -> Result<std::collections::BTreeSet<homesense_core::DeviceId>, Error> {
        boot(boot_id)?;
        let mut blocked = std::collections::BTreeSet::new();
        if observations.is_empty() { return Ok(blocked); }
        let mut statement = self.connection.prepare(
            "SELECT device_id, MAX(status IN ('accepted','writing')), MAX(status IN ('written','observed','failed','uncertain')), MAX(CASE WHEN status IN ('written','observed','failed','uncertain') AND transport_epoch>=?2 THEN updated_ms END) FROM command_journal WHERE boot_id=?1 AND status!='cancelled' GROUP BY device_id",
        )?;
        let mut rows = statement.query(params![boot_id, integer(transport_epoch)?])?;
        while let Some(row) = rows.next()? {
            let device = homesense_core::DeviceId::new(row.get::<_, String>(0)?)
                .map_err(|_| Error::InvalidAudit)?;
            let Some(sample) = observations.get(&device) else { continue };
            let sample = sample.map(integer).transpose()?;
            let pending: bool = row.get(1)?;
            let terminal: bool = row.get(2)?;
            let latest: Option<i64> = row.get(3)?;
            if pending || match sample {
                None => terminal,
                Some(at) => latest.is_some_and(|written| written >= at),
            } {
                blocked.insert(device);
            }
        }
        Ok(blocked)
    }
    pub fn command_status(&self, boot_id: &str, id: u64) -> Result<Option<Status>, Error> {
        boot(boot_id)?;
        let value: Option<String> = self
            .connection
            .query_row(
                "SELECT status FROM command_journal WHERE boot_id=?1 AND command_id=?2",
                params![boot_id, integer(id)?],
                |r| r.get(0),
            )
            .optional()?;
        value.map(|s| Status::parse(&s)).transpose()
    }
    pub fn motor_receipt(
        &self, boot_id: &str, id: u64, device: &homesense_core::DeviceId,
        command: Command, epochs: homesense_core::queue::Epochs,
    ) -> Result<Option<MotorReceipt>, Error> {
        boot(boot_id)?;
        if !matches!(command,Command::Motor(_)) {return Err(Error::InvalidAudit)}
        let row:Option<(String,i64)>=self.connection.query_row(
            "SELECT status,updated_ms FROM command_journal WHERE boot_id=?1 AND command_id=?2 AND device_id=?3 AND intent=?4 AND revision=?5 AND transport_epoch=?6 AND control_epoch=?7",
            params![boot_id,integer(id)?,device.as_str(),intent(command).as_slice(),integer(epochs.configuration)?,integer(epochs.transport)?,integer(epochs.control)?],
            |r|Ok((r.get(0)?,r.get(1)?)),
        ).optional()?;
        row.map(|(status,at)| {
            let status=Status::parse(&status)?;
            let at=u64::try_from(at).map_err(|_|Error::InvalidAudit)?;
            Ok(MotorReceipt {status,written_at_ms:(status==Status::Written).then_some(at)})
        }).transpose()
    }
    /// Atomic final scene-lifecycle check at the transport boundary.
    /// False records cancellation; no bytes may be written for that command.
    /// Already accepted motor STOP survives scene cancellation, as in the queue.
    /// Transport epochs, deadline and authorization remain the caller's gates.
    pub fn begin_command_write(&mut self, boot_id: &str, id: u64, now_ms: u64) -> Result<bool, Error> {
        self.begin_command_write_inner(boot_id,id,now_ms,None)
    }
    /// Runtime path: physical wire scope and fault/operation fences supplement
    /// the caller's current readiness, source and publication authorization.
    pub fn begin_command_write_scoped(&mut self, boot_id: &str, id: u64, now_ms: u64,
        wire: &homesense_core::gateway_scope::WireScope) -> Result<bool, Error> {
        self.begin_command_write_inner(boot_id,id,now_ms,Some(wire))
    }
    /// Reserve durable Writing without releasing a transport capability. The
    /// caller must revalidate its lease after this potentially blocking audit.
    pub fn reserve_command_write(&mut self,boot_id:&str,id:u64,now_ms:u64,
        wire:Option<&homesense_core::gateway_scope::WireScope>)->Result<Option<UndeliveredWrite<'_>>,Error> {
        if !self.begin_command_write_inner(boot_id,id,now_ms,wire)? {return Ok(None)}
        Ok(Some(UndeliveredWrite{store:self,boot:boot_id.to_owned(),id}))
    }
    fn begin_command_write_inner(&mut self, boot_id: &str, id: u64, now_ms: u64,
        wire:Option<&homesense_core::gateway_scope::WireScope>) -> Result<bool, Error> {
        boot(boot_id)?;
        let tx=self.connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let ended: bool=tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM scene_command c JOIN scene_run s USING(boot_id,run_id) JOIN command_journal j ON j.boot_id=c.boot_id AND j.command_id=c.command_id WHERE c.boot_id=?1 AND c.command_id=?2 AND j.intent!=?3 AND s.status NOT IN ('pending','running'))",
            params![boot_id,integer(id)?,intent(Command::Motor(MotorMotion::Stop)).as_slice()],|r|r.get(0))?;
        let scoped_ready=match wire {
            Some(wire)=>crate::gateway_audit::check_before_write(&tx,boot_id,id,wire)?,
            None=>true,
        };
        let ended=ended || !scoped_ready;
        let status=if ended {"cancelled"} else {"writing"};
        let count=tx.execute("UPDATE command_journal SET status=?1,updated_ms=?2 WHERE boot_id=?3 AND command_id=?4 AND status='accepted' AND updated_ms<=?2",
            params![status,integer(now_ms)?,boot_id,integer(id)?])?;
        if count!=1 {return Err(Error::Conflict);}
        tx.commit()?;
        Ok(!ended)
    }
    pub fn transition_command(
        &mut self,
        boot_id: &str,
        id: u64,
        from: Status,
        to: Status,
        now_ms: u64,
    ) -> Result<(), Error> {
        self.transition_commands(boot_id, &[id], from, to, now_ms)
    }
    /// One bounded transport receipt, committed together. A bad later member
    /// rolls back earlier journal and light-setting changes in this receipt.
    pub fn transition_commands(
        &mut self,
        boot_id: &str,
        ids: &[u64],
        from: Status,
        to: Status,
        now_ms: u64,
    ) -> Result<(), Error> {
        boot(boot_id)?;
        if ids.is_empty() || ids.len() > 128
            || ids.iter().collect::<std::collections::BTreeSet<_>>().len() != ids.len()
        {
            return Err(Error::InvalidAudit);
        }
        if !matches!(
            (from, to),
            (
                Status::Accepted,
                Status::Writing | Status::Cancelled | Status::Failed
            ) | (
                Status::Writing,
                Status::Written | Status::Failed | Status::Uncertain
            ) | (
                Status::Written,
                Status::Observed | Status::Failed | Status::Uncertain
            )
        ) {
            return Err(Error::InvalidAudit);
        }
        let tx=self.connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        for &id in ids {
            let count = tx.execute("UPDATE command_journal SET status=?1,updated_ms=?2 WHERE boot_id=?3 AND command_id=?4 AND status=?5 AND updated_ms<=?2",params![to.text(),integer(now_ms)?,boot_id,integer(id)?,from.text()])?;
            if count != 1 {return Err(Error::Conflict);}
            if to==Status::Written {
                let (revision,device,bytes,row):(i64,String,Vec<u8>,i64)=tx.query_row("SELECT revision,device_id,intent,rowid FROM command_journal WHERE boot_id=?1 AND command_id=?2",params![boot_id,integer(id)?],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
                if bytes.len()==4 && bytes[0]==1 && matches!(bytes[1],2|3) && bytes[3]==0 {
                    let level=homesense_core::Percent::new(bytes[2]).map_err(|_|Error::InvalidAudit)?;
                    tx.execute("INSERT INTO light_setting(revision,device_id,level,journal_row) VALUES(?1,?2,?3,?4) ON CONFLICT(revision,device_id) DO UPDATE SET level=excluded.level,journal_row=excluded.journal_row WHERE light_setting.journal_row<excluded.journal_row",params![revision,device,level.get(),row])?;
                }
            }
        }
        tx.commit()?;
        Ok(())
    }
    /// Call during exclusive startup with a fresh boot ID. Returns only an audit
    /// count; no recovered command is returned for transport replay.
    pub fn recover_commands(&mut self, current_boot: &str) -> Result<usize, Error> {
        boot(current_boot)?;
        Ok(self.connection.execute("UPDATE command_journal SET status='uncertain',recovered_by=?1 WHERE boot_id<>?1 AND status IN ('accepted','writing','written')",[current_boot])?)
    }
}

fn cancel_in_transaction(
    tx: &rusqlite::Transaction<'_>,
    boot_id: &str,
    cancellations: &[Cancellation],
    now: i64,
) -> Result<(), Error> {
    for cancellation in cancellations {
        let command = &cancellation.command;
        let count = tx.execute("UPDATE command_journal SET status='cancelled',updated_ms=?1,cancellation_reason=?2 WHERE boot_id=?3 AND command_id=?4 AND status='accepted' AND revision=?5 AND device_id=?6 AND intent=?7 AND updated_ms<=?1 AND transport_epoch=?8 AND control_epoch=?9 AND deadline_ms=?10 AND run_id IS ?11",
            params![now, format!("{:?}", cancellation.reason), boot_id, integer(command.id)?,
                sql_revision(command.epochs.configuration)?, command.device_id.as_str(),
                intent(command.command).as_slice(), integer(command.epochs.transport)?,
                integer(command.epochs.control)?, integer(command.deadline_ms)?, command.run_id.map(integer).transpose()?])?;
        if count != 1 {
            return Err(Error::Conflict);
        }
    }
    Ok(())
}


impl Store {
    /// Quiescence is durable execution work for this boot/revision. Pure Query
    /// intents are excluded, including Accepted/Writing background read tickets.
    /// Uncertain control receipts remain a barrier until explicit reconciliation.
    pub fn has_unsettled_control_commands(&self,boot_id:&str,revision:u64)->Result<bool,Error> {
        boot(boot_id)?;let revision=crate::sql_revision(revision)?;
        Ok(self.connection.query_row("SELECT EXISTS(SELECT 1 FROM command_journal WHERE boot_id=?1 AND revision=?2 AND intent!=?3 AND status IN ('accepted','writing','uncertain'))",params![boot_id,revision,intent(Command::Query).as_slice()],|r|r.get(0))?)
    }
}

#[cfg(test)]
#[path = "batch_receipt_tests.rs"]
mod batch_receipt_tests;

#[cfg(test)]
mod native_toggle_tests {
    use super::*;
    #[test]
    fn relative_audit_encoding_never_claims_an_absolute_target() {
        assert_eq!(intent(Command::Toggle), [1, 7, 0, 0]);
        for other in [Command::Query, Command::Switch(false), Command::Switch(true),
            Command::PauseRamp, Command::PanelLed(false), Command::PanelLed(true)] {
            assert_ne!(intent(Command::Toggle), intent(other));
        }
    }
    #[test]
    fn relative_journal_survives_restart_only_as_uncertain_history() {
        let path = std::env::temp_dir().join(format!("native-journal-{}",
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir(&path).unwrap();
        let file = path.join("runtime.sqlite");
        let mut store = Store::open(&file).unwrap();
        store.publish(None, 1, b"synthetic", &[]).unwrap();
        let device = homesense_core::DeviceId::new("synthetic").unwrap();
        store.accept_commands("first", &[QueuedCommand {
            id: 1, device_id: device.clone(), command: Command::Toggle,
            origin: homesense_core::queue::Origin::Http,
            epochs: homesense_core::queue::Epochs { configuration: 1, transport: 1, control: 1 },
            deadline_ms: 100, run_id: None,
        }], 0).unwrap();
        store.transition_command("first", 1, Status::Accepted, Status::Writing, 1).unwrap();
        store.transition_command("first", 1, Status::Writing, Status::Written, 2).unwrap();
        assert!(store.recorded_dimmer_levels(1, &[device]).unwrap().is_empty());
        drop(store);
        let mut store = Store::open(&file).unwrap();
        assert_eq!(store.recover_commands("second").unwrap(), 1);
        assert_eq!(store.command_status("first", 1).unwrap(), Some(Status::Uncertain));
        let bytes: Vec<u8> = store.connection.query_row(
            "SELECT intent FROM command_journal WHERE boot_id='first' AND command_id=1",
            [], |row| row.get(0)).unwrap();
        assert_eq!(bytes, [1, 7, 0, 0]);
        assert_eq!(store.recover_commands("second").unwrap(), 0);
        drop(store);
        std::fs::remove_dir_all(path).unwrap();
    }
}
