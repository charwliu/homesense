//! Retained receipts for a key-specific completion. These are process-local
//! evidence, not authority to enqueue a command or change a variable. The
//! Admission owner must additionally retain and revalidate the exact Source,
//! target-variable CAS, manual fences and gateway scopes at commit.
// Configured gestures retain the same source through execution and feedback.
// Receipts alone never authorize an effect.

use crate::admission::TaskGeneration;
use homesense_core::{Command, DeviceId, queue::QueuedCommand};
use homesense_storage::{journal::Status, variables::ReadCheck};
use std::collections::{BTreeMap, BTreeSet};

/// A successful logical receipt describes the complete original execution.
/// Native execution retains its original request and deadline across phases.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Execution {
    Explicit {
        commands: BTreeSet<u64>,
        runs: BTreeSet<String>,
    },
    Native {
        request: homesense_storage::native_scene::Request,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Ticket {
    command: QueuedCommand,
    accepted: u64,
    written: Option<u64>,
}

/// Stable diagnostic codes. The first terminal cause is retained until the
/// record is removed; diagnostics contain no credential or source payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum End {
    InvalidCapture,
    InvalidAcceptance,
    WriteUncertain,
    WriteOutsideWindow,
    ConflictingReceipt,
    Ineligible,
    ReceiptMissing,
    Cancelled,
    TicketCancelled,
    TicketFailed,
    SceneIncomplete,
    NativeUnknown,
    GenerationChanged,
    Deadline,
    DeliveryDeadline,
    AuthorityChanged,
    SourceSuperseded,
    VariableChanged,
    ManualChanged,
    GatewayChanged,
    CompetingControl,
    NoEffect,
    Applied,
    CommitConflict,
}
impl End {
    fn label(self) -> &'static str {
        match self {
            Self::InvalidCapture => "invalid_capture",
            Self::InvalidAcceptance => "invalid_acceptance",
            Self::WriteUncertain => "write_uncertain",
            Self::WriteOutsideWindow => "write_outside_window",
            Self::ConflictingReceipt => "conflicting_receipt",
            Self::Ineligible => "ineligible_snapshot",
            Self::ReceiptMissing => "receipt_missing",
            Self::Cancelled => "cancelled",
            Self::TicketCancelled => "source_ticket_cancelled",
            Self::TicketFailed => "source_ticket_failed",
            Self::SceneIncomplete => "scene_incomplete",
            Self::NativeUnknown => "native_unknown",
            Self::GenerationChanged => "generation_changed",
            Self::Deadline => "original_deadline",
            Self::DeliveryDeadline => "delivery_deadline",
            Self::AuthorityChanged => "authority_changed",
            Self::SourceSuperseded => "source_superseded",
            Self::VariableChanged => "variable_changed",
            Self::ManualChanged => "manual_changed",
            Self::GatewayChanged => "gateway_changed",
            Self::CompetingControl => "competing_control",
            Self::NoEffect => "condition_not_met",
            Self::Applied => "committed",
            Self::CommitConflict => "commit_conflict",
        }
    }
}

/// Immutable before the first source admission. Capturing a newer variable or
/// manual fence later would incorrectly turn a competing operation into ours.
#[derive(Clone)]
pub(crate) struct Capture {
    pub endpoint: DeviceId,
    pub request_id: String,
    pub admit_before: u64,
    pub command_fence: u64,
    pub effect: homesense_config::keypad_completion::SelectionCompletion,
    pub generation: TaskGeneration,
    pub variable: ReadCheck,
    pub manual: BTreeMap<DeviceId, u64>,
    pub wires: Vec<homesense_core::gateway_scope::WireScope>,
    /// The physical relay for a combined-light Off, otherwise the logical
    /// actuator. A query or a written LED cannot satisfy this source write.
    pub after_device: DeviceId,
}

pub(crate) struct Pending {
    capture: Capture,
    tickets: BTreeMap<u64, Ticket>,
    execution: Option<Execution>,
    deadline: Option<u64>,
    own_manual: BTreeMap<DeviceId, u64>,
    retired: Option<End>,
}

#[derive(Clone, Debug)]
pub(crate) struct Evidence {
    pub command_fence: u64,
    pub effect: homesense_config::keypad_completion::SelectionCompletion,
    pub generation: TaskGeneration,
    pub variable: ReadCheck,
    pub written: BTreeMap<u64, u64>,
    pub writes_by_device: BTreeMap<DeviceId, u64>,
    pub after_device: DeviceId,
    pub after_written: u64,
    pub after_command: Command,
    pub deadline: u64,
}

impl Pending {
    pub(crate) fn new(capture: Capture) -> Self {
        let retired = (capture.variable.revision != capture.generation.epochs.configuration
            || capture.variable.name != capture.effect.variable)
            .then_some(End::InvalidCapture);
        Self {
            capture,
            tickets: BTreeMap::new(),
            execution: None,
            deadline: None,
            own_manual: BTreeMap::new(),
            retired,
        }
    }

    pub(crate) fn retire(&mut self, reason: End) {
        self.retired.get_or_insert(reason);
    }

    /// Called only after durable queue acceptance. No receipt can create a
    /// later budget, change an existing ticket or import another generation.
    pub(crate) fn accepted(
        &mut self,
        commands: &[QueuedCommand],
        original_deadline: u64,
        now: u64,
        own_manual: &BTreeMap<DeviceId, u64>,
    ) {
        if self.retired.is_some() {
            return;
        }
        let deadline = self
            .deadline
            .map_or(original_deadline, |old| old.min(original_deadline));
        let mut ids = BTreeSet::new();
        let added = commands
            .iter()
            .filter(|command| !self.tickets.contains_key(&command.id))
            .count();
        if commands.is_empty()
            || now >= deadline
            || self.tickets.len().saturating_add(added) > 512
            || commands.iter().any(|command| {
                command.id == 0
                    || command.epochs != self.capture.generation.epochs
                    || command.deadline_ms <= now
                    || !ids.insert(command.id)
                    || self
                        .tickets
                        .get(&command.id)
                        .is_some_and(|old| old.command != *command)
            })
            || own_manual.keys().any(|device| {
                !commands.iter().any(|command| {
                    &command.device_id == device
                        && matches!(
                            command.command,
                            Command::Switch(_) | Command::SetLevel { .. } | Command::Toggle
                        )
                })
            })
            || own_manual.iter().any(|(device, version)| {
                self.capture.manual.contains_key(device)
                    && !commands.iter().any(|command| {
                        &command.device_id == device && !self.tickets.contains_key(&command.id)
                    })
                    && self.own_manual.get(device) != Some(version)
            })
        {
            self.retire(if now >= deadline {
                End::Deadline
            } else {
                End::InvalidAcceptance
            });
            return;
        }
        self.deadline = Some(deadline);
        for (device, version) in own_manual {
            if self.capture.manual.contains_key(device) {
                self.own_manual.insert(device.clone(), *version);
            }
        }
        for command in commands {
            self.tickets.entry(command.id).or_insert_with(|| Ticket {
                command: command.clone(),
                accepted: now,
                written: None,
            });
        }
    }

    /// The physical writer reports a complete write once. Journal timestamps
    /// are not substituted for this callback, and an ambiguous write retires
    /// the entire effect rather than permitting a retry.
    pub(crate) fn written(&mut self, id: u64, complete: bool, at: u64) {
        let Some(ticket) = self.tickets.get_mut(&id) else {
            return;
        };
        if !complete
            || at < ticket.accepted
            || at >= ticket.command.deadline_ms
            || self.deadline.is_none_or(|limit| at >= limit)
        {
            self.retire(if !complete {
                End::WriteUncertain
            } else {
                End::WriteOutsideWindow
            });
            return;
        }
        if ticket.written.is_none() {
            ticket.written = Some(at);
        }
    }

    /// Seal only from the successful logical reply, never an API retry's JSON
    /// or recovered journal row. Unknown native final On still has a completed
    /// execution; its Off-specific condition is decided from feedback later.
    pub(crate) fn seal(&mut self, execution: Execution) {
        let valid = match &execution {
            Execution::Explicit { commands, runs } => !commands.is_empty() || !runs.is_empty(),
            Execution::Native { request } => {
                request.boot == self.capture.generation.boot_id()
                    && request.epochs == self.capture.generation.epochs
                    && self
                        .deadline
                        .is_some_and(|deadline| request.deadline_ms == deadline)
            }
        };
        if !valid || self.execution.as_ref().is_some_and(|old| *old != execution) {
            self.retire(End::ConflictingReceipt);
        } else {
            self.execution = Some(execution);
        }
    }

    /// These immutable checks are performed by Admission before considering a
    /// feedback proof. They do not read current fences and adopt their values.
    pub(crate) fn current(
        &self,
        generation: &TaskGeneration,
        variable_version: u64,
        manual: &BTreeMap<DeviceId, u64>,
        wire_ready: impl Fn(&homesense_core::gateway_scope::WireScope) -> bool,
        now: u64,
    ) -> bool {
        self.invalid_reason(generation, variable_version, manual, wire_ready, now)
            .is_none()
    }
    fn invalid_reason(
        &self,
        generation: &TaskGeneration,
        variable_version: u64,
        manual: &BTreeMap<DeviceId, u64>,
        wire_ready: impl Fn(&homesense_core::gateway_scope::WireScope) -> bool,
        now: u64,
    ) -> Option<End> {
        if let Some(reason) = self.retired {
            return Some(reason);
        }
        if generation != &self.capture.generation {
            return Some(End::GenerationChanged);
        }
        if self.deadline.is_some_and(|deadline| now >= deadline) {
            return Some(End::Deadline);
        }
        if variable_version != self.capture.variable.expected_version {
            return Some(End::VariableChanged);
        }
        if !self.capture.manual.iter().all(|(device, before)| {
            manual.get(device).copied().unwrap_or(0)
                == self.own_manual.get(device).copied().unwrap_or(*before)
        }) {
            return Some(End::ManualChanged);
        }
        if !self.capture.wires.iter().all(wire_ready) {
            return Some(End::GatewayChanged);
        }
        None
    }

    /// Read durable status without synthesizing Written, Observed or terminal
    /// success. The callback is supplied by the single Admission/Store owner.
    /// A scene's CommandsWritten is execution success, never target feedback.
    pub(crate) fn evidence(
        &self,
        now: u64,
        mut status: impl FnMut(u64) -> Option<Status>,
        mut scene: impl FnMut(&str) -> Option<homesense_storage::scene_run::Status>,
        native: Option<&homesense_storage::native_scene::Record>,
    ) -> Option<Evidence> {
        if self.retired.is_some() || now >= self.deadline? {
            return None;
        }
        match self.execution.as_ref()? {
            Execution::Explicit { commands, runs } => {
                if !commands.iter().all(|id| self.tickets.contains_key(id))
                    || !runs.iter().all(|run| {
                        scene(run) == Some(homesense_storage::scene_run::Status::CommandsWritten)
                    })
                {
                    return None;
                }
            }
            Execution::Native { request } => {
                let native = native?;
                if native.request != *request
                    || native.status != homesense_storage::native_scene::Status::Observed
                    || native.initial_commands.is_empty()
                    || native.commands.is_empty()
                    || !native
                        .initial_commands
                        .iter()
                        .chain(&native.commands)
                        .all(|id| self.tickets.contains_key(id))
                {
                    return None;
                }
            }
        }
        let mut written = BTreeMap::new();
        let mut writes_by_device = BTreeMap::new();
        let mut after_write = None;
        for (id, ticket) in &self.tickets {
            let at = ticket.written?;
            if at > now {
                return None;
            }
            let valid = match status(*id)? {
                Status::Written => true,
                Status::Observed => matches!(
                    ticket.command.command,
                    Command::Switch(_) | Command::SetLevel { .. } | Command::Toggle
                ),
                _ => false,
            };
            if !valid {
                return None;
            }
            written.insert(*id, at);
            if matches!(
                ticket.command.command,
                Command::Switch(_) | Command::SetLevel { .. } | Command::Toggle
            ) {
                writes_by_device
                    .entry(ticket.command.device_id.clone())
                    .and_modify(|old: &mut u64| *old = (*old).max(at))
                    .or_insert(at);
                if ticket.command.device_id == self.capture.after_device
                    && after_write
                        .as_ref()
                        .is_none_or(|(old_at, old_id, _)| (at, *id) > (*old_at, *old_id))
                {
                    after_write = Some((at, *id, ticket.command.command));
                }
            }
        }
        let (after_written, _, after_command) = after_write?;
        Some(Evidence {
            command_fence: self.capture.command_fence,
            effect: self.capture.effect.clone(),
            generation: self.capture.generation.clone(),
            variable: self.capture.variable.clone(),
            written,
            writes_by_device,
            after_device: self.capture.after_device.clone(),
            after_written,
            after_command,
            deadline: self.deadline?,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Decision {
    Waiting,
    NoEffect,
    Apply,
}

/// Still not a commit capability. Admission must lock this physical lease
/// again with its own Store and Source fences before mutating anything.
pub(crate) struct FeedbackProof {
    evidence: Evidence,
    lease: crate::feedback::RuleLease,
    decision: Decision,
    send_before: u64,
    observation_max_age_ms: u64,
}
impl FeedbackProof {
    pub(crate) fn evidence(&self) -> &Evidence {
        &self.evidence
    }
    pub(crate) fn lease(&self) -> &crate::feedback::RuleLease {
        &self.lease
    }
    pub(crate) fn decision(&self) -> Decision {
        self.decision
    }
    pub(crate) fn send_before(&self) -> u64 {
        self.send_before
    }
    pub(crate) fn observation_max_age_ms(&self) -> u64 {
        self.observation_max_age_ms
    }
    pub(crate) fn prepare(
        evidence: Evidence,
        config: &homesense_config::Configuration,
        feedback: &crate::feedback::Feedback,
        now: u64,
    ) -> Result<Self, String> {
        if now >= evidence.deadline || config.revision() != evidence.generation.epochs.configuration
        {
            return Err("key completion execution expired or changed".into());
        }
        let conditions = std::iter::once(&evidence.effect.after)
            .chain(&evidence.effect.when)
            .collect::<Vec<_>>();
        let (lease, values) = crate::component_modes::sample_conditions(
            config,
            &conditions,
            feedback,
            now,
            evidence.effect.max_age_ms,
        )?;
        // Reject a snapshot from a different Feedback owner even if its values
        // and monotonic timestamps happen to match this execution.
        drop(
            lease.lock(
                Uuid::parse_str(evidence.generation.boot_id())
                    .map_err(|_| "key completion boot invalid")?
                    .as_u128(),
                evidence.generation.epochs.configuration,
                evidence.generation.epochs.transport,
                now,
            )?,
        );
        let after_current = lease
            .observations
            .get(&evidence.after_device)
            .is_some_and(|at| at.is_some_and(|at| at > evidence.after_written));
        let writes_current = evidence.writes_by_device.iter().all(|(id, written)| {
            lease
                .observations
                .get(id)
                .is_none_or(|at| at.is_some_and(|at| at > *written))
        });
        let compatible = match evidence.after_command {
            Command::Switch(on) => on == evidence.effect.after.on,
            Command::SetLevel { level, .. } => {
                (level.get() > 0) == evidence.effect.after.on
                    && evidence
                        .effect
                        .after
                        .level
                        .is_none_or(|wanted| wanted == level.get())
            }
            Command::Toggle => true, // Only actual final feedback decides this branch.
            _ => false,
        };
        let decision = if !compatible {
            // A known explicit On cannot acquire an Off completion merely
            // because independent later feedback happens to say Off.
            Decision::NoEffect
        } else if !after_current || !writes_current || values[0].is_none() {
            Decision::Waiting
        } else if values[0] == Some(false) {
            Decision::NoEffect
        } else if values[1..].iter().any(Option::is_none) {
            Decision::Waiting
        } else if values[1..].iter().all(|value| *value == Some(true)) {
            Decision::Apply
        } else {
            Decision::NoEffect
        };
        let age = feedback.effective_max_age_ms(evidence.effect.max_age_ms);
        let physical_expiry = lease
            .observations
            .values()
            .filter_map(|at| *at)
            .min()
            .unwrap_or(now)
            .saturating_add(age);
        let send_before = evidence
            .deadline
            .min(now.saturating_add(2_000))
            .min(physical_expiry);
        Ok(Self {
            evidence,
            lease,
            decision,
            send_before,
            observation_max_age_ms: age,
        })
    }
}

use uuid::Uuid;
#[path = "keypad_completion_commit.rs"]
mod commit;
pub(crate) use commit::CommitContext;

/// Kept outside Capture so receipt-only unit tests never manufacture Source
/// authority. The production constructor below resolves everything afresh in
/// Admission before any command is accepted.
pub(crate) struct AuthorizedCapture {
    pub receipt: Capture,
    source: crate::source_auth::Source,
}
impl AuthorizedCapture {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn prepare(
        config: &homesense_config::Configuration,
        document: &homesense_config::rule_document::Document,
        controller: Uuid,
        generation: TaskGeneration,
        request: &crate::keypad_actions::Request,
        source: crate::source_auth::Source,
        store: &homesense_storage::Store,
        manual: &BTreeMap<DeviceId, u64>,
        command_fence: u64,
        wires: impl FnOnce(
            BTreeSet<DeviceId>,
        ) -> Result<
            Vec<homesense_core::gateway_scope::WireScope>,
            hap::server::WriteStatus,
        >,
        now: u64,
    ) -> Result<Self, hap::server::WriteStatus> {
        use hap::server::WriteStatus as W;
        let original = crate::automation_control::Action::KeypadAction {
            controller,
            request: request.clone(),
        };
        if !source.matches(&original)
            || request.validate().ok() != Some(config.revision())
            || document.revision != config.revision()
            || generation.epochs.configuration != config.revision()
        {
            return Err(W::InsufficientPrivileges);
        }
        let parent = DeviceId::new(&request.keypad).map_err(|_| W::InvalidValueInRequest)?;
        let key = config
            .keypads()
            .get(&parent)
            .filter(|p| p.enabled)
            .and_then(|panel| panel.keys.get(&request.key))
            .filter(|key| key.led_endpoint.enabled)
            .ok_or(W::ResourceDoesNotExist)?;
        let endpoint = DeviceId::new(&key.led_endpoint.id).map_err(|_| W::InvalidValueInRequest)?;
        let (_, module) = config.binding(&endpoint).ok_or(W::ResourceDoesNotExist)?;
        let Some(homesense_config::keypad::KeyAction::SceneToggle { scene, .. }) = &key.action
        else {
            return Err(W::InvalidValueInRequest);
        };
        if module.profile() != homesense_core::ProtocolProfile::Ts686Rs485SixKey
            || module.led_mode() != homesense_core::PanelLedMode::Manual
        {
            return Err(W::InvalidValueInRequest);
        }
        let effect = key
            .selection_completion
            .clone()
            .ok_or(W::InvalidValueInRequest)?;
        let devices = homesense_config::keypad_completion::devices(config, &effect)
            .map_err(|_| W::InvalidValueInRequest)?;
        let scene_devices = crate::keypad_actions::scene_devices(config, document, scene)
            .map_err(|_| W::InvalidValueInRequest)?;
        if !source.permits_device(endpoint.as_str())
            || !source.permits_scene(scene)
            || devices
                .iter()
                .chain(&scene_devices)
                .any(|device| !source.permits_device(device.as_str()))
        {
            return Err(W::InsufficientPrivileges);
        }
        let variable = store
            .logical_variable(config.revision(), &effect.variable)
            .map_err(|_| W::ServiceCommunicationFailure)?
            .ok_or(W::InvalidValueInRequest)?;
        let logical = DeviceId::new(&effect.after.device).map_err(|_| W::InvalidValueInRequest)?;
        let after_device = if effect.after.on {
            logical.clone()
        } else {
            config
                .devices()
                .get(&logical)
                .and_then(|device| device.supply_relay.clone())
                .unwrap_or(logical)
        };
        let manual = devices
            .iter()
            .map(|id| (id.clone(), manual.get(id).copied().unwrap_or(0)))
            .collect();
        let requirements = devices
            .into_iter()
            .chain(scene_devices)
            .chain([endpoint.clone()])
            .collect();
        let capture = Capture {
            endpoint,
            request_id: request.request_id.clone(),
            command_fence,
            admit_before: now
                .checked_add(crate::control_batch::FEEDBACK_TOTAL_MS)
                .ok_or(W::ServiceCommunicationFailure)?,
            variable: ReadCheck {
                revision: config.revision(),
                name: effect.variable.clone(),
                expected_version: variable.version,
            },
            effect,
            generation,
            manual,
            wires: wires(requirements)?,
            after_device,
        };
        Ok(Self {
            receipt: capture,
            source,
        })
    }
}

struct Retained {
    source: crate::source_auth::Source,
    controller: Uuid,
    pending: Pending,
    native: Option<homesense_storage::native_scene::Request>,
    delivery_deadline: Option<u64>,
    declared_timeline: bool,
}
pub(crate) struct Submission {
    pub sequence: u64,
    pub source: crate::source_auth::Source,
    pub proof: FeedbackProof,
}
pub(crate) struct Plan {
    pub controller: Uuid,
    pub source: crate::source_auth::Source,
    pub receipt: homesense_storage::keypad_completion::Commit,
    pub originals: Vec<QueuedCommand>,
    pub wires: Vec<homesense_core::gateway_scope::WireScope>,
}
#[derive(Clone)]
pub(crate) struct View {
    pub sequence: u64,
    pub source: crate::source_auth::Source,
    pub evidence: Evidence,
}
#[derive(Default)]
pub(crate) struct Owner {
    records: BTreeMap<u64, Retained>,
}
impl Owner {
    /// A declared timeline is accepted before it has physical tickets. Hold
    /// the captured target version now, using that declaration's original end
    /// deadline, never a fresh budget when its first command becomes due.
    pub(crate) fn registered(
        &mut self,
        source: Option<&crate::source_auth::Source>,
        controller: Uuid,
        runs: BTreeSet<String>,
        deadline: u64,
        eligible: bool,
        now: u64,
    ) {
        let Some(source) = source else {
            return;
        };
        let Some(origin) = source.keypad() else {
            return;
        };
        let Some(capture) = &origin.completion else {
            return;
        };
        if !origin.start_completion() {
            return;
        }
        if !eligible || runs.is_empty() || now >= deadline {
            let reason = if !eligible {
                End::Ineligible
            } else if runs.is_empty() {
                End::InvalidAcceptance
            } else {
                End::Deadline
            };
            log::info!(
                "key_completion skipped sequence={} reason={}",
                origin.sequence,
                reason.label()
            );
            return;
        }
        let mut pending = Pending::new(capture.receipt.clone());
        pending.deadline = Some(deadline);
        pending.seal(Execution::Explicit {
            commands: BTreeSet::new(),
            runs,
        });
        self.records.insert(
            origin.sequence,
            Retained {
                source: source.clone(),
                controller,
                pending,
                native: None,
                delivery_deadline: None,
                declared_timeline: true,
            },
        );
    }
    /// Validate the exact retained capability, not receipt fields supplied by a
    /// caller. The first complete settled proof starts a nonrenewable window,
    /// even when a subsequent queue reservation is busy.
    pub(crate) fn prepare(
        &mut self,
        store: &homesense_storage::Store,
        input: &Submission,
        now: u64,
    ) -> Result<Option<Plan>, hap::server::WriteStatus> {
        use hap::server::WriteStatus as W;
        let Some(record) = self.records.get(&input.sequence) else {
            return Ok(None);
        };
        if !record.source.same_authority(&input.source) {
            return Err(W::InsufficientPrivileges);
        }
        let Some(actual) = Self::evidence_for(record, store, now)? else {
            return Ok(None);
        };
        let proof = &input.proof;
        let e = &proof.evidence;
        if actual.command_fence != e.command_fence
            || actual.effect != e.effect
            || actual.generation != e.generation
            || actual.variable.revision != e.variable.revision
            || actual.variable.name != e.variable.name
            || actual.variable.expected_version != e.variable.expected_version
            || actual.written != e.written
            || actual.writes_by_device != e.writes_by_device
            || actual.after_device != e.after_device
            || actual.after_written != e.after_written
            || actual.after_command != e.after_command
            || actual.deadline != e.deadline
        {
            return Err(W::InvalidValueInRequest);
        }
        let _guard = proof
            .lease
            .lock(
                Uuid::parse_str(e.generation.boot_id())
                    .map_err(|_| W::InvalidValueInRequest)?
                    .as_u128(),
                e.generation.epochs.configuration,
                e.generation.epochs.transport,
                now,
            )
            .map_err(|_| W::ResourceBusy)?;
        match proof.decision {
            Decision::Waiting => return Ok(None),
            Decision::NoEffect => {
                self.retire(input.sequence, End::NoEffect);
                return Ok(None);
            }
            Decision::Apply => {}
        }
        for (device, at) in &proof.lease.observations {
            if !store
                .keypad_completion_feedback_settled(
                    e.generation.boot_id(),
                    device,
                    *at,
                    e.generation.epochs.transport,
                    e.command_fence,
                    &e.written,
                )
                .map_err(|_| W::ServiceCommunicationFailure)?
            {
                return Err(W::ResourceBusy);
            }
        }
        let record = self.records.get_mut(&input.sequence).unwrap();
        let first = *record.delivery_deadline.get_or_insert(proof.send_before);
        let send_before = first.min(proof.send_before);
        if now >= send_before {
            self.retire(input.sequence, End::DeliveryDeadline);
            return Ok(None);
        }
        let origin = record.source.keypad().ok_or(W::InsufficientPrivileges)?;
        let observations = proof
            .lease
            .observations
            .iter()
            .map(|(id, at)| at.map(|at| (id.clone(), at)).ok_or(W::ResourceBusy))
            .collect::<Result<_, _>>()?;
        Ok(Some(Plan {
            controller: record.controller,
            source: record.source.clone(),
            receipt: homesense_storage::keypad_completion::Commit {
                update: homesense_storage::variables::Update {
                    revision: e.variable.revision,
                    name: e.variable.name.clone(),
                    expected_version: e.variable.expected_version,
                    value: Some(e.effect.value.value()),
                },
                policy: e.effect.id.clone(),
                sequence: input.sequence,
                endpoint: origin.endpoint.clone(),
                request_id: origin.request_id.clone(),
                epochs: e.generation.epochs,
                command_fence: e.command_fence,
                written: e.written.clone(),
                observations,
                after_device: e.after_device.clone(),
                after_written: e.after_written,
                original_deadline_ms: e.deadline,
                send_before_ms: send_before,
            },
            originals: record
                .pending
                .tickets
                .values()
                .map(|ticket| ticket.command.clone())
                .collect(),
            wires: record.pending.capture.wires.clone(),
        }))
    }
    pub(crate) fn retire(&mut self, sequence: u64, reason: End) {
        if let Some(record) = self.records.remove(&sequence) {
            // Committed means the variable/LED queue transaction succeeded.
            // Actual linked LED delivery remains in the command journal.
            log::info!(
                "key_completion terminal sequence={} reason={} source_commands={} source_written={} scene_runs={}",
                sequence,
                reason.label(),
                record.pending.tickets.len(),
                record
                    .pending
                    .tickets
                    .values()
                    .filter(|ticket| ticket.written.is_some())
                    .count(),
                match &record.pending.execution {
                    Some(Execution::Explicit { runs, .. }) => runs.len(),
                    Some(Execution::Native { .. }) => 1,
                    None => 0,
                }
            );
        }
    }
    /// Only live, already accepted gestures can reserve an existing version.
    /// This prevents the old physical owner from clearing that version before
    /// the completion CAS; it never locks out a newer explicit operation.
    pub(crate) fn holds(&self) -> Vec<ReadCheck> {
        self.records
            .values()
            .map(|r| r.pending.capture.variable.clone())
            .collect()
    }
    pub(crate) fn capacity(&self) -> bool {
        self.records.len() < 64
    }
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn preflight(
        &self,
        source: Option<&crate::source_auth::Source>,
        controller: Uuid,
        generation: &TaskGeneration,
        store: &homesense_storage::Store,
        manual: &BTreeMap<DeviceId, u64>,
        ready: impl Fn(&homesense_core::gateway_scope::WireScope) -> bool,
        now: u64,
    ) -> Result<bool, hap::server::WriteStatus> {
        use hap::server::WriteStatus as W;
        let Some(source) = source else {
            return Ok(true);
        };
        let Some(origin) = source.keypad() else {
            return Ok(true);
        };
        let Some(capture) = &origin.completion else {
            return Ok(true);
        };
        if !source.valid_for(controller, generation.epochs.configuration)
            || !capture.source.valid()
            || !source.derived_from(&capture.source)
            || !origin.current()
            || origin.endpoint != capture.receipt.endpoint
            || origin.request_id != capture.receipt.request_id
        {
            return Err(W::InsufficientPrivileges);
        }
        if &capture.receipt.generation != generation
            || capture.receipt.wires.iter().any(|wire| !ready(wire))
        {
            return Err(W::ResourceBusy);
        }
        let version = store
            .logical_variable(
                capture.receipt.variable.revision,
                &capture.receipt.variable.name,
            )
            .map_err(|_| W::ServiceCommunicationFailure)?
            .ok_or(W::InvalidValueInRequest)?
            .version;
        let owned = if let Some(record) = self.records.get(&origin.sequence) {
            if !record.source.same_authority(source) {
                return Err(W::InsufficientPrivileges);
            }
            record
                .pending
                .current(generation, version, manual, ready, now)
        } else {
            !origin.completion_started()
                && now < capture.receipt.admit_before
                && Pending::new(capture.receipt.clone())
                    .current(generation, version, manual, ready, now)
        };
        if owned && !self.records.contains_key(&origin.sequence) && !self.capacity() {
            return Err(W::ResourceBusy);
        }
        // A newer linked-component operation cancels only this conditional
        // completion. It must not silently change the original light action.
        Ok(owned)
    }
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn accepted(
        &mut self,
        source: Option<&crate::source_auth::Source>,
        controller: Uuid,
        commands: &[QueuedCommand],
        deadline: u64,
        manual_updates: &BTreeMap<DeviceId, u64>,
        native: Option<&homesense_storage::native_scene::Request>,
        eligible: bool,
        now: u64,
    ) {
        let Some(source) = source else {
            return;
        };
        let Some(origin) = source.keypad() else {
            return;
        };
        let Some(capture) = &origin.completion else {
            return;
        };
        let fresh = origin.start_completion();
        if !eligible {
            if let Some(record) = self.records.get_mut(&origin.sequence) {
                record.pending.retire(End::Ineligible);
            } else if fresh {
                log::info!(
                    "key_completion skipped sequence={} reason={}",
                    origin.sequence,
                    End::Ineligible.label()
                );
            }
            return;
        }
        if !fresh && !self.records.contains_key(&origin.sequence) {
            return;
        }
        let record = self
            .records
            .entry(origin.sequence)
            .or_insert_with(|| Retained {
                source: source.clone(),
                controller,
                pending: Pending::new(capture.receipt.clone()),
                native: None,
                delivery_deadline: None,
                declared_timeline: false,
            });
        let deadline = if record.declared_timeline {
            record.pending.deadline.unwrap_or(deadline)
        } else {
            deadline
        };
        record
            .pending
            .accepted(commands, deadline, now, manual_updates);
        if let Some(request) = native {
            record.pending.seal(Execution::Native {
                request: request.clone(),
            });
            record.native = Some(request.clone());
        }
    }
    pub(crate) fn written(&mut self, id: u64, complete: bool, at: u64) {
        for record in self.records.values_mut() {
            record.pending.written(id, complete, at);
        }
    }
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn audit(
        &mut self,
        store: &homesense_storage::Store,
        generation: Option<&TaskGeneration>,
        manual: &BTreeMap<DeviceId, u64>,
        authorize: impl Fn(Uuid) -> bool,
        ready: impl Fn(&homesense_core::gateway_scope::WireScope) -> bool,
        now: u64,
    ) -> Result<(), hap::server::WriteStatus> {
        use hap::server::WriteStatus as W;
        let mut remove = vec![];
        for (sequence, record) in &mut self.records {
            let reason = if let Some(generation) = generation {
                Self::record_invalid_reason(
                    record, store, generation, manual, &authorize, &ready, now,
                )?
            } else {
                Some(record.pending.retired.unwrap_or(End::GenerationChanged))
            };
            if let Some(reason) = reason {
                remove.push((*sequence, reason));
                continue;
            }
            if record.native.is_none()
                && let Some(receipt) = record
                    .source
                    .keypad()
                    .and_then(|origin| origin.completion_receipt())
            {
                record.pending.seal(receipt);
            }
            for id in record.pending.tickets.keys() {
                let status = store
                    .component_mode_ticket(record.pending.capture.generation.boot_id(), *id)
                    .map_err(|_| W::ServiceCommunicationFailure)?;
                let reason = match status.as_ref().map(|(status, _)| status.as_str()) {
                    None => Some(End::ReceiptMissing),
                    Some("cancelled") => Some(End::TicketCancelled),
                    Some("failed") => Some(End::TicketFailed),
                    Some("uncertain") => Some(End::WriteUncertain),
                    _ => None,
                };
                if let Some(reason) = reason {
                    record.pending.retire(reason);
                    break;
                }
            }
            if let Some(Execution::Explicit { runs, .. }) = &record.pending.execution {
                let mut reason = None;
                for run in runs {
                    let row = store
                        .scene_run(record.pending.capture.generation.boot_id(), run)
                        .map_err(|_| W::ServiceCommunicationFailure)?;
                    reason = match row {
                        None => Some(End::ReceiptMissing),
                        Some(row)
                            if row.status == homesense_storage::scene_run::Status::Cancelled =>
                        {
                            Some(End::Cancelled)
                        }
                        Some(row)
                            if row.status.terminal()
                                && row.status
                                    != homesense_storage::scene_run::Status::CommandsWritten =>
                        {
                            Some(End::SceneIncomplete)
                        }
                        _ => None,
                    };
                    if reason.is_some() {
                        break;
                    }
                }
                if let Some(reason) = reason {
                    record.pending.retire(reason);
                }
            }
            if let Some(request) = &record.native {
                let row = store
                    .native_scene_request(&request.controller, &request.key)
                    .map_err(|_| W::ServiceCommunicationFailure)?;
                let reason = match row {
                    None => Some(End::ReceiptMissing),
                    Some(row) if row.request != *request => Some(End::ConflictingReceipt),
                    Some(row) if row.status == homesense_storage::native_scene::Status::Unknown => {
                        Some(End::NativeUnknown)
                    }
                    _ => None,
                };
                if let Some(reason) = reason {
                    record.pending.retire(reason);
                }
            }
            if let Some(reason) = record.pending.retired {
                remove.push((*sequence, reason));
            }
        }
        for (sequence, reason) in remove {
            self.retire(sequence, reason);
        }
        Ok(())
    }
    #[allow(clippy::too_many_arguments)]
    fn record_invalid_reason(
        record: &Retained,
        store: &homesense_storage::Store,
        generation: &TaskGeneration,
        manual: &BTreeMap<DeviceId, u64>,
        authorize: &impl Fn(Uuid) -> bool,
        ready: &impl Fn(&homesense_core::gateway_scope::WireScope) -> bool,
        now: u64,
    ) -> Result<Option<End>, hap::server::WriteStatus> {
        if let Some(reason) = record.pending.retired {
            return Ok(Some(reason));
        }
        if generation != &record.pending.capture.generation {
            return Ok(Some(End::GenerationChanged));
        }
        if record
            .delivery_deadline
            .is_some_and(|deadline| now >= deadline)
        {
            return Ok(Some(End::DeliveryDeadline));
        }
        let Some(origin) = record.source.keypad() else {
            return Ok(Some(End::InvalidCapture));
        };
        if !record
            .source
            .valid_for(record.controller, generation.epochs.configuration)
            || !authorize(record.controller)
        {
            return Ok(Some(End::AuthorityChanged));
        }
        if !origin.current() {
            return Ok(Some(End::SourceSuperseded));
        }
        let Some(variable) = store
            .logical_variable(
                record.pending.capture.variable.revision,
                &record.pending.capture.variable.name,
            )
            .map_err(|_| hap::server::WriteStatus::ServiceCommunicationFailure)?
        else {
            return Ok(Some(End::VariableChanged));
        };
        if let Some(reason) =
            record
                .pending
                .invalid_reason(generation, variable.version, manual, ready, now)
        {
            return Ok(Some(reason));
        }
        if !store
            .keypad_completion_dependencies_current(
                generation.boot_id(),
                generation.epochs.configuration,
                record.pending.capture.command_fence,
                &record.pending.tickets.keys().copied().collect(),
                &record.pending.capture.manual.keys().cloned().collect(),
            )
            .map_err(|_| hap::server::WriteStatus::ServiceCommunicationFailure)?
        {
            return Ok(Some(End::CompetingControl));
        }
        Ok(None)
    }
    fn evidence_for(
        record: &Retained,
        store: &homesense_storage::Store,
        now: u64,
    ) -> Result<Option<Evidence>, hap::server::WriteStatus> {
        use hap::server::WriteStatus as W;
        let boot = record.pending.capture.generation.boot_id();
        let mut statuses = BTreeMap::new();
        for id in record.pending.tickets.keys() {
            let row = store
                .component_mode_ticket(boot, *id)
                .map_err(|_| W::ServiceCommunicationFailure)?;
            statuses.insert(
                *id,
                row.and_then(|(status, _)| match status.as_str() {
                    "accepted" => Some(Status::Accepted),
                    "writing" => Some(Status::Writing),
                    "written" => Some(Status::Written),
                    "observed" => Some(Status::Observed),
                    "cancelled" => Some(Status::Cancelled),
                    "failed" => Some(Status::Failed),
                    "uncertain" => Some(Status::Uncertain),
                    _ => None,
                }),
            );
        }
        let mut runs = BTreeMap::new();
        if let Some(Execution::Explicit { runs: ids, .. }) = &record.pending.execution {
            for run in ids {
                runs.insert(
                    run.clone(),
                    store
                        .scene_run(boot, run)
                        .map_err(|_| W::ServiceCommunicationFailure)?
                        .map(|row| row.status),
                );
            }
        }
        let native = record
            .native
            .as_ref()
            .map(|request| store.native_scene_request(&request.controller, &request.key))
            .transpose()
            .map_err(|_| W::ServiceCommunicationFailure)?
            .flatten();
        Ok(record.pending.evidence(
            now,
            |id| statuses.get(&id).copied().flatten(),
            |run| runs.get(run).copied().flatten(),
            native.as_ref(),
        ))
    }
    pub(crate) fn views(
        &self,
        store: &homesense_storage::Store,
        now: u64,
    ) -> Result<Vec<View>, hap::server::WriteStatus> {
        let mut views = vec![];
        for (sequence, record) in &self.records {
            if let Some(evidence) = Self::evidence_for(record, store, now)? {
                views.push(View {
                    sequence: *sequence,
                    source: record.source.clone(),
                    evidence,
                });
            }
        }
        Ok(views)
    }
}

#[cfg(test)]
#[path = "keypad_completion_admission_tests.rs"]
mod admission_tests;
#[cfg(test)]
#[path = "keypad_completion_tests.rs"]
mod tests;

#[derive(Default)]
pub(crate) struct Consumer {
    next: usize,
}
impl Consumer {
    pub(crate) async fn tick(
        &mut self,
        admission: &crate::admission::Admission,
        controller: Uuid,
        feedback: &crate::feedback::Feedback,
    ) -> Result<(), String> {
        use hap::server::WriteStatus as W;
        let generation = match admission.task_generation().await {
            Ok(generation) => generation,
            Err(W::ResourceBusy | W::ServiceCommunicationFailure) => return Ok(()),
            Err(e) => return Err(format!("key completion generation: {e:?}")),
        };
        let views = match admission
            .key_completion_views(controller, generation.clone())
            .await
        {
            Ok(views) => views,
            Err(W::ResourceBusy) => return Ok(()),
            Err(e) => return Err(format!("key completion views: {e:?}")),
        };
        if views.is_empty() {
            return Ok(());
        }
        let bundle = match admission.automation_bundle(controller, generation).await {
            Ok(bundle) => bundle,
            Err(W::ResourceBusy) => return Ok(()),
            Err(e) => return Err(format!("key completion bundle: {e:?}")),
        };
        for i in 0..views.len().min(4) {
            let view = &views[(self.next + i) % views.len()];
            let proof = match FeedbackProof::prepare(
                view.evidence.clone(),
                &bundle.configuration,
                feedback,
                admission.monotonic_ms(),
            ) {
                Ok(proof) => proof,
                Err(_) => continue,
            };
            match admission
                .key_completion_proof(
                    controller,
                    Submission {
                        sequence: view.sequence,
                        source: view.source.clone(),
                        proof,
                    },
                )
                .await
            {
                Ok(_)
                | Err(W::ResourceBusy | W::InvalidValueInRequest | W::InsufficientPrivileges) => {}
                Err(e) => return Err(format!("key completion commit: {e:?}")),
            }
        }
        self.next = (self.next + 4) % views.len();
        Ok(())
    }
}
