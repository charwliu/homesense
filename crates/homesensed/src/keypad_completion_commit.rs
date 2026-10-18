//! Admission-thread completion. This owns no transport and cannot reconstruct
//! a capability from persisted receipt metadata.
use super::*;
use hap::server::WriteStatus as W;
use homesense_core::{
    admission::Access,
    arbiter::{Arbiter, ArbiterError, BatchCommitError},
    queue::Origin,
};

pub(crate) struct CommitContext<'a> {
    pub keys: &'a mut Owner,
    pub modes: &'a mut crate::component_modes::Owner,
    pub batches: &'a mut crate::control_batch::Owner,
    pub config: &'a homesense_config::Configuration,
    pub store: &'a mut homesense_storage::Store,
    pub arbiter: &'a mut Arbiter,
    pub gateways: Option<&'a crate::gateway_runtime::State>,
    pub manual: &'a BTreeMap<DeviceId, u64>,
    pub next_id: &'a mut u64,
    pub principals: &'a mut BTreeMap<u64, Uuid>,
    pub sources: &'a mut BTreeMap<u64, crate::source_auth::Source>,
    pub read_end: u64,
    pub write_slot_ms: u64,
    pub authorize: &'a dyn Fn(Uuid) -> bool,
    pub clock: &'a dyn Fn() -> u64,
}
impl CommitContext<'_> {
    pub(crate) fn accept(&mut self, input: Submission, now: u64) -> Result<Vec<u64>, W> {
        let Some(plan) = self.keys.prepare(self.store, &input, now)? else {
            return Ok(vec![]);
        };
        if !(self.authorize)(plan.controller) || !plan.source.valid() {
            self.keys.retire(input.sequence, End::AuthorityChanged);
            return Err(W::InsufficientPrivileges);
        }
        let e = input.proof.evidence();
        if self.read_end.max(now).saturating_add(
            self.write_slot_ms
                .saturating_mul(e.effect.indicators.len() as u64),
        ) >= plan.receipt.send_before_ms
        {
            return Err(W::ResourceBusy);
        }
        let end = self
            .next_id
            .checked_add(e.effect.indicators.len() as u64)
            .ok_or(W::ServiceCommunicationFailure)?;
        let mut commands = vec![];
        for (index, indicator) in e.effect.indicators.iter().enumerate() {
            let device = DeviceId::new(&indicator.device).map_err(|_| W::InvalidValueInRequest)?;
            let (binding, module) = self
                .config
                .binding(&device)
                .ok_or(W::ResourceDoesNotExist)?;
            if !binding.enabled || !plan.source.permits_device(device.as_str()) {
                return Err(W::InsufficientPrivileges);
            }
            commands.push((
                QueuedCommand {
                    id: *self.next_id + index as u64,
                    device_id: device,
                    command: Command::PanelLed(indicator.on),
                    origin: Origin::Rule,
                    epochs: e.generation.epochs,
                    deadline_ms: plan.receipt.send_before_ms,
                    // Queue ordering identity, not a new scene or a source receipt.
                    run_id: Some(*self.next_id),
                },
                Access {
                    authorized: true,
                    device_exists: true,
                    device_verified: matches!(
                        module.verification(),
                        homesense_core::Verification::Verified(_)
                    ),
                    existing_direct_manual: false,
                },
            ));
        }
        let leds = commands
            .iter()
            .map(|(command, _)| command.clone())
            .collect::<Vec<_>>();
        let gate = self.batches.prepare_completion(
            self.config,
            &input.proof,
            plan.source.clone(),
            plan.controller,
            plan.originals.clone(),
            &leds,
            self.manual,
            plan.receipt.send_before_ms,
            now,
        )?;
        let handoff =
            self.modes
                .prepare_completion(self.config, &plan, &input.proof, self.manual, now)?;
        let scopes = self
            .gateways
            .map(|g| g.scope(self.config, &leds, &BTreeMap::new(), &plan.wires, vec![]))
            .transpose()?;
        let _feedback = input
            .proof
            .lease()
            .lock(
                Uuid::parse_str(e.generation.boot_id())
                    .map_err(|_| W::InvalidValueInRequest)?
                    .as_u128(),
                e.generation.epochs.configuration,
                e.generation.epochs.transport,
                now,
            )
            .map_err(|_| W::ResourceBusy)?;
        let result = crate::source_auth::commit(Some(&plan.source), || {
            plan.source.keypad().and_then(|origin| {
                origin.with_current(|| {
                    self.arbiter.submit_ordered_rule_batch_committed(
                        commands,
                        now,
                        |commands, cancelled| {
                            self.store.accept_keypad_completion_before(
                                e.generation.boot_id(),
                                commands,
                                cancelled,
                                now,
                                scopes.as_ref(),
                                &plan.controller.to_string(),
                                &plan.receipt,
                                self.clock,
                            )
                        },
                    )
                })
            })
        })
        .flatten()
        .ok_or(W::InsufficientPrivileges)?;
        let cancelled = match result {
            Ok(cancelled) => cancelled,
            Err(BatchCommitError::Admission(ArbiterError::Queue(_) | ArbiterError::Busy)) => {
                return Err(W::ResourceBusy);
            }
            Err(BatchCommitError::Persistence(
                homesense_storage::Error::Conflict | homesense_storage::Error::InvalidAudit,
            )) => {
                self.keys.retire(input.sequence, End::CommitConflict);
                return Err(W::InvalidValueInRequest);
            }
            Err(error) => {
                log::error!(
                    "key completion acceptance failed sequence={} error={error:?}",
                    input.sequence
                );
                return Err(W::ServiceCommunicationFailure);
            }
        };
        for cancellation in cancelled {
            self.principals.remove(&cancellation.command.id);
            self.sources.remove(&cancellation.command.id);
        }
        for command in &leds {
            self.principals.insert(command.id, plan.controller);
            self.sources.insert(command.id, plan.source.clone());
        }
        self.batches.install_completion(gate);
        self.modes.install_completion(handoff);
        self.keys.retire(input.sequence, End::Applied);
        self.modes.set_completion_holds(self.keys.holds());
        let ids = leds.iter().map(|command| command.id).collect();
        *self.next_id = end;
        Ok(ids)
    }
}
