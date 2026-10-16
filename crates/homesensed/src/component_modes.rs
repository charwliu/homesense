//! Bounded local component inference. Pending confirmations never own a new
//! command or query; the Admission owner alone mutates confirmed variables.
use crate::{
    admission::{Admission, TaskGeneration},
    feedback::{Feedback, RuleLease},
    source_auth::Source,
};
use hap::server::WriteStatus as W;
use homesense_config::{
    Configuration,
    component_modes::{Basis, Policy},
    rule_document::Document,
};
use homesense_core::{
    Command, DeviceId,
    queue::QueuedCommand,
    rule::{Action, Budget, Expr, Lifetime, Node, Value, ValueType, Variable},
};
use homesense_storage::{
    Store,
    variables::{ReadCheck, Stored, Update},
};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;
#[derive(Clone)]
pub(crate) struct Prepared {
    policies: Vec<Policy>,
    updates: Vec<Update>,
    guards: Vec<ReadCheck>,
    prior_known: Vec<Update>,
    scenes: Vec<String>,
    baseline: Option<RuleLease>,
    logical_policies: Vec<Policy>,
    canonical: Vec<homesense_storage::component_modes::Canonical>,
    logical_initial: Vec<ReadCheck>,
}
impl Prepared {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn capture(
        document: &Document,
        config: &Configuration,
        scenes: &[String],
        snapshot: &BTreeMap<String, Stored>,
        actual_reads: &BTreeSet<String>,
        evaluated: &BTreeMap<String, Variable>,
        updates: &[Update],
        intents: &[homesense_hap::requests::Intent],
        feedback: Option<(&Feedback, u64)>,
        now: u64,
    ) -> Result<Option<Self>, String> {
        let mut policies = vec![];
        let mut deferred = BTreeSet::new();
        let active = |p: &&Policy| {
            p.scenes.iter().any(|s| scenes.contains(s))
                && updates.iter().any(|u| u.name == p.variable)
                && p.when_initial_unknown
                    .iter()
                    .all(|name| snapshot.get(name).is_some_and(|v| v.value.is_none()))
        };
        let primaries = document
            .component_mode_confirmations
            .iter()
            .filter(active)
            .filter(|p| {
                !p.include_when_first_unknown
                    && (p.confirm_when_known
                        || snapshot.get(&p.variable).is_some_and(|v| v.value.is_none()))
            })
            .collect::<Vec<_>>();
        if primaries.is_empty() {
            return Ok(None);
        }
        let unknown_scenes = primaries
            .iter()
            .filter(|p| {
                p.basis == Basis::Physical
                    && !p.confirm_when_known
                    && snapshot.get(&p.variable).is_some_and(|v| v.value.is_none())
                    // Only an actually selected first-unknown outcome owns
                    // companions. A dormant opposite outcome must not activate
                    // physical companions beside a known-source canonical path.
                    && updates.iter().any(|u|u.name==p.variable&&u.value==Some(p.first_value.value()))
                    && p.when_values.iter().all(|(name,wanted)|evaluated.get(name).is_some_and(|v|v.value==Some(wanted.value())))
            })
            .flat_map(|p| p.scenes.iter().filter(|s| scenes.contains(s)).cloned())
            .collect::<BTreeSet<_>>();
        let candidates = primaries
            .into_iter()
            .chain(
                document
                    .component_mode_confirmations
                    .iter()
                    .filter(active)
                    .filter(|p| {
                        p.include_when_first_unknown
                            && p.scenes.iter().any(|s| unknown_scenes.contains(s))
                    }),
            )
            .collect::<Vec<_>>();
        let required = candidates
            .iter()
            .map(|p| p.variable.clone())
            .collect::<BTreeSet<_>>();
        for policy in candidates {
            let update = updates
                .iter()
                .find(|u| u.name == policy.variable)
                .ok_or("component executed update missing")?;
            if policy.basis == Basis::LogicalSource
                && policy
                    .when_values
                    .keys()
                    .any(|name| snapshot.get(name).is_none_or(|v| v.value.is_none()))
            {
                continue;
            }
            if update.value != Some(policy.first_value.value())
                || policy.when_values.iter().any(|(name, wanted)| {
                    evaluated
                        .get(name)
                        .is_none_or(|v| v.value != Some(wanted.value()))
                })
            {
                continue;
            }
            if !deferred.insert(policy.variable.clone()) {
                return Err("ambiguous component first activation policy".into());
            }
            for condition in &policy.postconditions {
                let id = DeviceId::new(&condition.device).map_err(|e| e.to_string())?;
                if let Some(last) = intents.iter().rev().find(|i| i.device == id) {
                    let on = match last.command {
                        Command::Switch(on) => on,
                        Command::SetLevel { level, .. } => level.get() > 0,
                        _ => {
                            return Err(
                                "component confirmation only owns explicit light commands".into()
                            );
                        }
                    };
                    if on!=condition.on || condition.level.is_some_and(|expected|!matches!(last.command,Command::SetLevel{level,..} if level.get()==expected)) || config.binding(&id).is_none(){return Err("component postcondition contradicts actual branch".into());}
                }
            }
            policies.push(policy.clone());
        }
        if deferred != required {
            return Err(
                "executed component outcome lacks a unique reviewed physical vector".into(),
            );
        }
        if policies.is_empty() {
            return Ok(None);
        }
        if policies.len() > 16
            || policies
                .iter()
                .map(|p| p.postconditions.len())
                .sum::<usize>()
                > 128
            || intents.len() > 512
        {
            return Err("component operation bound exceeded".into());
        }
        let logical_policies = policies
            .iter()
            .filter(|p| p.basis == Basis::LogicalSource)
            .cloned()
            .collect::<Vec<_>>();
        policies.retain(|p| p.basis == Basis::Physical);
        deferred = policies.iter().map(|p| p.variable.clone()).collect();
        let mut canonical = vec![];
        let mut logical_initial = vec![];
        for policy in &logical_policies {
            let update = updates
                .iter()
                .find(|u| u.name == policy.variable)
                .ok_or("canonical target write missing")?
                .clone();
            let mut sources = vec![];
            for (name, wanted) in &policy.when_values {
                let before = snapshot
                    .get(name)
                    .filter(|v| v.value.is_some())
                    .ok_or("logical source was initially unknown")?;
                if deferred.contains(name) {
                    return Err(
                        "logical source cannot depend on an unconfirmed deferred source".into(),
                    );
                }
                logical_initial.push(ReadCheck {
                    revision: document.revision,
                    name: name.clone(),
                    expected_version: before.version,
                });
                sources.push(homesense_storage::component_modes::SourceValue {
                    check: ReadCheck {
                        revision: document.revision,
                        name: name.clone(),
                        expected_version: before.version
                            + u64::from(updates.iter().any(|u| u.name == *name)),
                    },
                    value: wanted.value(),
                });
            }
            canonical.push(homesense_storage::component_modes::Canonical {
                update,
                policy: policy.id.clone(),
                sources,
            });
        }
        let prior_known = snapshot
            .iter()
            .filter(|(name, v)| deferred.contains(*name) && v.value.is_some())
            .map(|(name, v)| Update {
                revision: document.revision,
                name: name.clone(),
                expected_version: v.version,
                value: v.value.clone(),
            })
            .collect();
        let selected = updates
            .iter()
            .filter(|u| deferred.contains(&u.name))
            .cloned()
            .collect();
        let selectors = policies
            .iter()
            .flat_map(|p| {
                p.when_values
                    .keys()
                    .cloned()
                    .chain(p.when_initial_unknown.iter().cloned())
            })
            .collect::<BTreeSet<_>>();
        let guards = snapshot
            .iter()
            .filter(|(name, v)| {
                v.lifetime == Lifetime::Persistent
                    && (actual_reads.contains(*name)
                        || deferred.contains(*name)
                        || selectors.contains(*name))
            })
            .map(|(name, v)| ReadCheck {
                revision: document.revision,
                name: name.clone(),
                expected_version: v.version
                    + u64::from(
                        updates.iter().any(|u| u.name == *name) && !deferred.contains(name),
                    ),
            })
            .collect();
        let baseline = if policies.is_empty() {
            None
        } else {
            let (feedback, _) =
                feedback.ok_or("component confirmation requires unified physical feedback")?;
            let (_, values) = sample(config, &policies, feedback, now)?;
            let mut written_lighting = false;
            for (index, condition) in policies.iter().flat_map(|p| &p.postconditions).enumerate() {
                let id = DeviceId::new(&condition.device).map_err(|e| e.to_string())?;
                if intents.iter().any(|i| i.device == id) {
                    written_lighting = true;
                } else if values[index] != Some(true) {
                    return Err("unwritten component precondition is unknown or mismatched".into());
                }
            }
            if !written_lighting {
                return Err("first unknown component requires explicit lighting output".into());
            }
            let mut preconditions = policies.clone();
            for p in &mut preconditions {
                p.postconditions
                    .retain(|c| !intents.iter().any(|i| i.device.as_str() == c.device));
            }
            preconditions.retain(|p| !p.postconditions.is_empty());
            if preconditions.is_empty() {
                None
            } else {
                Some(sample(config, &preconditions, feedback, now)?.0)
            }
        };
        Ok(Some(Self {
            policies,
            prior_known,
            updates: selected,
            guards,
            scenes: scenes.to_vec(),
            baseline,
            logical_policies,
            canonical,
            logical_initial,
        }))
    }
    pub(crate) fn canonical(&self) -> &[homesense_storage::component_modes::Canonical] {
        &self.canonical
    }
    pub(crate) fn baseline(&self) -> Option<&RuleLease> {
        self.baseline.as_ref()
    }
    pub(crate) fn devices(&self, config: &Configuration) -> Result<BTreeSet<DeviceId>, W> {
        let mut ids = BTreeSet::new();
        for condition in self.policies.iter().flat_map(|p| &p.postconditions) {
            let id = DeviceId::new(&condition.device).map_err(|_| W::InvalidValueInRequest)?;
            if let Some(supply) = config
                .devices()
                .get(&id)
                .and_then(|d| d.supply_relay.clone())
            {
                ids.insert(supply);
            }
            ids.insert(id);
        }
        Ok(ids)
    }
    pub(crate) fn variables(&self) -> BTreeSet<String> {
        self.updates.iter().map(|u| u.name.clone()).collect()
    }
    pub(crate) fn reads(&self) -> Vec<ReadCheck> {
        self.updates
            .iter()
            .map(|u| ReadCheck {
                revision: u.revision,
                name: u.name.clone(),
                expected_version: u.expected_version,
            })
            .collect()
    }
    pub(crate) fn validate(&self, store: &Store, document: &Document) -> Result<(), W> {
        if self.policies.iter().any(|p| {
            !document.component_mode_confirmations.contains(p)
                || !p.scenes.iter().any(|s| self.scenes.contains(s))
        }) {
            return Err(W::InvalidValueInRequest);
        }
        if self
            .logical_policies
            .iter()
            .any(|p| !document.component_mode_confirmations.contains(p))
        {
            return Err(W::InvalidValueInRequest);
        }
        for read in &self.logical_initial {
            if store
                .logical_variable(read.revision, &read.name)
                .map_err(|_| W::ServiceCommunicationFailure)?
                .is_none_or(|v| v.value.is_none() || v.version != read.expected_version)
            {
                return Err(W::InvalidValueInRequest);
            }
        }
        for p in &self.policies {
            let value = store
                .logical_variable(document.revision, &p.variable)
                .map_err(|_| W::ServiceCommunicationFailure)?
                .ok_or(W::InvalidValueInRequest)?;
            let u = self
                .updates
                .iter()
                .find(|u| u.name == p.variable)
                .ok_or(W::InvalidValueInRequest)?;
            if (!p.include_when_first_unknown && !p.confirm_when_known && value.value.is_some())
                || value.version != u.expected_version
                || u.value != Some(p.first_value.value())
            {
                return Err(W::InvalidValueInRequest);
            }
        }
        Ok(())
    }
}
#[derive(Clone)]
pub(crate) struct View {
    pub id: u64,
    pub generation: TaskGeneration,
    pub policies: Vec<Policy>,
    pub after: BTreeMap<DeviceId, u64>,
}
struct Record {
    view: View,
    controller: Uuid,
    source: Option<Source>,
    updates: Vec<Update>,
    guards: Vec<ReadCheck>,
    prior_known: Vec<Update>,
    tickets: Vec<QueuedCommand>,
    written: BTreeMap<u64, u64>,
    manual: BTreeMap<DeviceId, u64>,
    wires: Vec<homesense_core::gateway_scope::WireScope>,
    deadline: u64,
}
struct Logical {
    generation: TaskGeneration,
    controller: Uuid,
    source: Option<Source>,
    canonical: homesense_storage::component_modes::Canonical,
    wires: Vec<homesense_core::gateway_scope::WireScope>,
}
#[derive(Default)]
pub(crate) struct Owner {
    next: u64,
    scope: Option<TaskGeneration>,
    pending: BTreeMap<u64, Record>,
    confirmed: BTreeMap<u64, Record>,
    logical: BTreeMap<u64, Logical>,
}
impl Owner {
    pub(crate) fn matches_scope(&self, generation: &TaskGeneration) -> bool {
        self.scope.as_ref() == Some(generation)
    }
    pub(crate) fn empty_scope(&mut self, generation: TaskGeneration) {
        self.scope = Some(generation);
        self.pending.clear();
        self.confirmed.clear();
        self.logical.clear();
    }
    pub(crate) fn project_selection(
        &self,
        store: &Store,
        name: &str,
        generation: &TaskGeneration,
        snapshot: &mut homesense_storage::scene_observation::Snapshot,
    ) -> Result<bool, W> {
        if self
            .pending
            .values()
            .any(|r| r.view.generation == *generation && r.updates.iter().any(|u| u.name == name))
        {
            snapshot.stored.value = None;
            snapshot.component_inferred = true;
            snapshot.physical = None;
            return Ok(true);
        }
        for record in self.confirmed.values() {
            if record.view.generation != *generation
                || !record.updates.iter().any(|u| u.name == name)
                || !Self::owned(store, record)?
            {
                continue;
            }
            snapshot.component_inferred = true;
            snapshot.physical = Some(homesense_storage::scene_observation::PhysicalProjection {
                boot_id: Uuid::parse_str(generation.boot_id())
                    .map_err(|_| W::ServiceCommunicationFailure)?
                    .as_u128(),
                epochs: generation.epochs,
                observed_ms: record
                    .view
                    .policies
                    .iter()
                    .map(|p| record.deadline.saturating_sub(p.max_age_ms))
                    .min(),
                history_value: snapshot.stored.value.clone(),
                pending_relative: false,
                relative_eligible: false,
            });
            return Ok(true);
        }
        Ok(false)
    }
    pub(crate) fn capacity(&self) -> bool {
        self.pending.len() < 64 && self.next < u64::MAX - 64
    }
    pub(crate) fn written(&mut self, id: u64, at: u64) {
        for record in self.pending.values_mut() {
            if record.tickets.iter().any(|c| c.id == id) {
                record.written.insert(id, at);
            }
        }
    }
    pub(crate) fn busy(&self, names: &BTreeSet<String>) -> bool {
        self.pending
            .values()
            .any(|r| r.updates.iter().any(|u| names.contains(&u.name)))
    }
    fn versions(store: &Store, revision: u64, reads: &[ReadCheck]) -> Result<bool, W> {
        for read in reads {
            if store
                .logical_variable(revision, &read.name)
                .map_err(|_| W::ServiceCommunicationFailure)?
                .is_none_or(|v| v.version != read.expected_version)
            {
                return Ok(false);
            }
        }
        Ok(true)
    }
    fn owned(store: &Store, r: &Record) -> Result<bool, W> {
        for u in &r.updates {
            if store
                .logical_variable(u.revision, &u.name)
                .map_err(|_| W::ServiceCommunicationFailure)?
                .is_none_or(|v| v.version != u.expected_version || v.value != u.value)
            {
                return Ok(false);
            }
        }
        Ok(true)
    }
    fn logical_owned(store: &Store, r: &Logical) -> Result<bool, W> {
        let u = &r.canonical.update;
        Ok(store
            .logical_variable(u.revision, &u.name)
            .map_err(|_| W::ServiceCommunicationFailure)?
            .is_some_and(|v| v.version == u.expected_version && v.value == u.value))
    }
    fn invalidate_logical(store: &mut Store, r: &Logical, now: u64) -> Result<(), W> {
        if !Self::logical_owned(store, r)? {
            return Ok(());
        }
        let u = &r.canonical.update;
        match store.apply_component_mode_inference(
            &[(
                Update {
                    value: None,
                    ..u.clone()
                },
                r.canonical.policy.clone(),
            )],
            &[],
            r.generation.boot_id(),
            &r.controller.to_string(),
            now,
            &[],
            false,
        ) {
            Ok(()) | Err(homesense_storage::Error::Conflict) => Ok(()),
            Err(_) => Err(W::ServiceCommunicationFailure),
        }
    }
    fn invalidate_prior(store: &mut Store, r: &Record, now: u64) -> Result<(), W> {
        let mut updates = vec![];
        for prior in &r.prior_known {
            if store
                .logical_variable(prior.revision, &prior.name)
                .map_err(|_| W::ServiceCommunicationFailure)?
                .is_some_and(|v| v.version == prior.expected_version && v.value == prior.value)
            {
                let policy = r
                    .view
                    .policies
                    .iter()
                    .find(|p| p.variable == prior.name)
                    .ok_or(W::ServiceCommunicationFailure)?;
                updates.push((
                    Update {
                        value: None,
                        ..prior.clone()
                    },
                    policy.id.clone(),
                ));
            }
        }
        if updates.is_empty() {
            return Ok(());
        }
        match store.apply_component_mode_inference(
            &updates,
            &[],
            r.view.generation.boot_id(),
            &r.controller.to_string(),
            now,
            &[],
            false,
        ) {
            Ok(()) | Err(homesense_storage::Error::Conflict) => Ok(()),
            Err(_) => Err(W::ServiceCommunicationFailure),
        }
    }
    fn invalidate(store: &mut Store, r: &Record, now: u64) -> Result<(), W> {
        if !Self::owned(store, r)? {
            return Ok(());
        }
        let updates = r
            .updates
            .iter()
            .map(|u| {
                (
                    Update {
                        value: None,
                        ..u.clone()
                    },
                    r.view
                        .policies
                        .iter()
                        .find(|p| p.variable == u.name)
                        .unwrap()
                        .id
                        .clone(),
                )
            })
            .collect::<Vec<_>>();
        match store.apply_component_mode_inference(
            &updates,
            &[],
            r.view.generation.boot_id(),
            &r.controller.to_string(),
            now,
            &[],
            false,
        ) {
            Ok(()) | Err(homesense_storage::Error::Conflict) => Ok(()),
            Err(_) => Err(W::ServiceCommunicationFailure),
        }
    }
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn audit(
        &mut self,
        store: &mut Store,
        current: Option<&TaskGeneration>,
        now: u64,
        authorized: &impl Fn(Uuid) -> bool,
        manual: &BTreeMap<DeviceId, u64>,
        wire_ready: &impl Fn(&homesense_core::gateway_scope::WireScope) -> bool,
    ) -> Result<(), W> {
        let mut remove = vec![];
        for (id, r) in &self.logical {
            if !Self::logical_owned(store, r)? {
                remove.push(*id);
                continue;
            }
            let mut changed = false;
            for source in &r.canonical.sources {
                let expected = &source.check;
                if store
                    .logical_variable(expected.revision, &expected.name)
                    .map_err(|_| W::ServiceCommunicationFailure)?
                    .is_none_or(|v| {
                        v.version != expected.expected_version
                            || v.value != Some(source.value.clone())
                    })
                {
                    changed = true;
                    break;
                }
            }
            if current != Some(&r.generation)
                || !authorized(r.controller)
                || r.source.as_ref().is_some_and(|s| !s.valid())
                || r.wires.iter().any(|w| !wire_ready(w))
                || changed
            {
                Self::invalidate_logical(store, r, now)?;
                remove.push(*id);
            }
        }
        for id in remove {
            self.logical.remove(&id);
        }
        let mut remove = vec![];
        for (id, r) in &self.pending {
            let invalid = current != Some(&r.view.generation)
                || now >= r.deadline
                || !authorized(r.controller)
                || r.source.as_ref().is_some_and(|s| !s.valid())
                || r.manual
                    .iter()
                    .any(|(id, version)| manual.get(id).copied().unwrap_or(0) != *version)
                || r.wires.iter().any(|w| !wire_ready(w))
                || !Self::versions(store, r.view.generation.epochs.configuration, &r.guards)?;
            let mut terminal_bad = false;
            for ticket in &r.tickets {
                if store
                    .component_mode_ticket(r.view.generation.boot_id(), ticket.id)
                    .map_err(|_| W::ServiceCommunicationFailure)?
                    .is_none_or(|(status, _)| {
                        matches!(status.as_str(), "cancelled" | "uncertain" | "failed")
                    })
                {
                    terminal_bad = true;
                    break;
                }
            }
            if invalid || terminal_bad {
                remove.push(*id);
            }
        }
        for id in remove {
            if let Some(record) = self.pending.remove(&id) {
                Self::invalidate_prior(store, &record, now)?;
            }
        }
        let mut remove = vec![];
        for (id, r) in &self.confirmed {
            if !Self::owned(store, r)? {
                remove.push(*id);
                continue;
            }
            if current != Some(&r.view.generation)
                || now >= r.deadline
                || r.source.as_ref().is_some_and(|s| !s.valid())
                || !authorized(r.controller)
                || r.manual
                    .iter()
                    .any(|(id, version)| manual.get(id).copied().unwrap_or(0) != *version)
                || r.wires.iter().any(|w| !wire_ready(w))
            {
                Self::invalidate(store, r, now)?;
                remove.push(*id);
            }
        }
        for id in remove {
            self.confirmed.remove(&id);
        }
        Ok(())
    }
    pub(crate) fn synchronize(
        &mut self,
        store: &mut Store,
        _config: &Configuration,
        document: &Document,
        generation: &TaskGeneration,
        now: u64,
    ) -> Result<(), W> {
        if self.scope.as_ref() == Some(generation) {
            return Ok(());
        }
        self.pending.clear();
        self.confirmed.clear();
        self.logical.clear();
        if !document.component_mode_confirmations.is_empty() {
            store
                .ensure_component_mode_audit()
                .map_err(|_| W::ServiceCommunicationFailure)?;
            let mut seen = BTreeSet::new();
            let mut resets = vec![];
            for policy in &document.component_mode_confirmations {
                if !seen.insert(&policy.variable) {
                    continue;
                }
                let Some(value) = store
                    .logical_variable(document.revision, &policy.variable)
                    .map_err(|_| W::ServiceCommunicationFailure)?
                else {
                    continue;
                };
                if value.value.is_some() {
                    resets.push((
                        Update {
                            revision: document.revision,
                            name: policy.variable.clone(),
                            expected_version: value.version,
                            value: None,
                        },
                        policy.id.clone(),
                    ));
                }
            }
            if !resets.is_empty() {
                store
                    .apply_component_mode_inference(
                        &resets,
                        &[],
                        generation.boot_id(),
                        "component-generation-reset",
                        now,
                        &[],
                        false,
                    )
                    .map_err(|_| W::ServiceCommunicationFailure)?;
            }
        }
        self.scope = Some(generation.clone());
        Ok(())
    }
    fn allocate(&mut self) -> Result<u64, W> {
        self.next = self
            .next
            .checked_add(1)
            .ok_or(W::ServiceCommunicationFailure)?;
        Ok(self.next)
    }
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn accepted(
        &mut self,
        mut prepared: Prepared,
        generation: TaskGeneration,
        controller: Uuid,
        source: Option<Source>,
        tickets: Vec<QueuedCommand>,
        manual: &BTreeMap<DeviceId, u64>,
        config: &Configuration,
        wires: Vec<homesense_core::gateway_scope::WireScope>,
        now: u64,
    ) -> Result<(), W> {
        if self.pending.len() >= 64 || self.busy(&prepared.variables()) {
            return Err(W::ResourceBusy);
        }
        let logical_names = prepared
            .canonical
            .iter()
            .map(|c| c.update.name.clone())
            .collect::<BTreeSet<_>>();
        self.logical
            .retain(|_, r| !logical_names.contains(&r.canonical.update.name));
        for mut canonical in prepared.canonical.drain(..) {
            canonical.update.expected_version = canonical
                .update
                .expected_version
                .checked_add(1)
                .ok_or(W::ServiceCommunicationFailure)?;
            let id = self.allocate()?;
            self.logical.insert(
                id,
                Logical {
                    generation: generation.clone(),
                    controller,
                    source: source.clone(),
                    canonical,
                    wires: wires.clone(),
                },
            );
        }
        if prepared.policies.is_empty() {
            return Ok(());
        }
        let names = prepared.variables();
        // Handoff still-owned inference to the new explicitly confirmed mutation.
        // Its prior value must not be invalidated by our own accepted commands.
        self.confirmed
            .retain(|_, r| !r.updates.iter().any(|u| names.contains(&u.name)));
        let id = self.allocate()?;
        let mut fence = BTreeMap::new();
        for p in &prepared.policies {
            for c in &p.postconditions {
                let target = DeviceId::new(&c.device).map_err(|_| W::InvalidValueInRequest)?;
                fence.insert(target.clone(), manual.get(&target).copied().unwrap_or(0));
                if let Some(supply) = config
                    .devices()
                    .get(&target)
                    .and_then(|d| d.supply_relay.clone())
                {
                    fence.insert(supply.clone(), manual.get(&supply).copied().unwrap_or(0));
                }
            }
        }
        let timeout = prepared
            .policies
            .iter()
            .map(|p| p.timeout_ms)
            .min()
            .ok_or(W::InvalidValueInRequest)?;
        self.pending.insert(
            id,
            Record {
                view: View {
                    id,
                    generation,
                    policies: prepared.policies,
                    after: Default::default(),
                },
                controller,
                source,
                updates: prepared.updates,
                guards: prepared.guards,
                prior_known: prepared.prior_known,
                tickets,
                written: Default::default(),
                manual: fence,
                wires,
                deadline: now.saturating_add(timeout),
            },
        );
        Ok(())
    }
    pub(crate) fn views(&self, store: &Store, _config: &Configuration) -> Result<Vec<View>, W> {
        let mut views = self
            .confirmed
            .values()
            .map(|r| r.view.clone())
            .collect::<Vec<_>>();
        for record in self.pending.values() {
            let mut written = BTreeMap::new();
            let mut ready = true;
            for ticket in &record.tickets {
                let Some((status, _)) = store
                    .component_mode_ticket(record.view.generation.boot_id(), ticket.id)
                    .map_err(|_| W::ServiceCommunicationFailure)?
                else {
                    ready = false;
                    break;
                };
                let Some(at) = record.written.get(&ticket.id).copied() else {
                    ready = false;
                    break;
                };
                let led = matches!(ticket.command, Command::PanelLed(_) | Command::PanelLedOff);
                if status != "written" && (led || status != "observed") {
                    ready = false;
                    break;
                }
                written
                    .entry(ticket.device_id.clone())
                    .and_modify(|old: &mut u64| *old = (*old).max(at))
                    .or_insert(at);
            }
            if !ready {
                continue;
            }
            let mut view = record.view.clone();
            view.after = written;
            views.push(view);
        }
        Ok(views)
    }
}
fn sample(
    config: &Configuration,
    policies: &[Policy],
    feedback: &Feedback,
    now: u64,
) -> Result<(RuleLease, Vec<Option<bool>>), String> {
    let age = policies
        .iter()
        .map(|p| p.max_age_ms)
        .min()
        .ok_or("component proof empty")?;
    let mut variables = BTreeMap::new();
    let mut nodes = vec![];
    for (i, condition) in policies.iter().flat_map(|p| &p.postconditions).enumerate() {
        let logical = DeviceId::new(&condition.device).map_err(|e| e.to_string())?;
        // Off depends solely on a reliable power relay, never residual AD8.
        let source = if !condition.on {
            config
                .devices()
                .get(&logical)
                .and_then(|d| d.supply_relay.clone())
                .unwrap_or(logical.clone())
        } else {
            logical.clone()
        };
        let name = format!("component:{i}:on");
        variables.insert(
            name.clone(),
            Variable {
                value_type: ValueType::Bool,
                lifetime: Lifetime::Temporary,
                value: None,
            },
        );
        nodes.push(Node {
            source: format!("component-confirm/{i}/on"),
            action: Action::Set {
                variable: name,
                value: Expr::DeviceOn(source),
            },
        });
        if condition.level.is_some() {
            let name = format!("component:{i}:level");
            variables.insert(
                name.clone(),
                Variable {
                    value_type: ValueType::Integer,
                    lifetime: Lifetime::Temporary,
                    value: None,
                },
            );
            nodes.push(Node {
                source: format!("component-confirm/{i}/level"),
                action: Action::Set {
                    variable: name,
                    value: Expr::DeviceLevel(logical),
                },
            });
        }
    }
    let (plan, lease) = feedback.plan_rule(&nodes, &variables, Budget::default(), now, age)?;
    let values = policies
        .iter()
        .flat_map(|p| &p.postconditions)
        .enumerate()
        .map(|(i, c)| {
            let Some(Value::Bool(on)) = plan.variables[&format!("component:{i}:on")].value else {
                return None;
            };
            let level_match = match c.level {
                Some(expected) => match plan.variables[&format!("component:{i}:level")].value {
                    Some(Value::Integer(actual)) => Some(actual == i64::from(expected)),
                    _ => None,
                },
                None => Some(true),
            };
            level_match.map(|level| on == c.on && level)
        })
        .collect();
    Ok((lease, values))
}
pub(crate) struct Proof {
    pub id: u64,
    pub generation: TaskGeneration,
    lease: RuleLease,
    matched: Option<bool>,
    valid_until: u64,
}
impl Proof {
    pub(crate) fn prepare(
        view: &View,
        config: &Configuration,
        feedback: &Feedback,
        now: u64,
    ) -> Result<Self, String> {
        let (lease, values) = sample(config, &view.policies, feedback, now)?;
        let after_written = lease.observations.iter().all(|(id, at)| {
            at.is_some_and(|at| view.after.get(id).is_none_or(|written| at > *written))
        });
        let matched = if values.iter().any(Option::is_none) || !after_written {
            None
        } else {
            Some(values.into_iter().all(|value| value == Some(true)))
        };
        let age = view.policies.iter().map(|p| p.max_age_ms).min().unwrap();
        let valid_until = lease
            .observations
            .values()
            .filter_map(|at| *at)
            .min()
            .unwrap_or(now)
            .saturating_add(age);
        Ok(Self {
            id: view.id,
            generation: view.generation.clone(),
            lease,
            matched,
            valid_until,
        })
    }
}
impl Owner {
    pub(crate) fn apply(
        &mut self,
        store: &mut Store,
        config: &Configuration,
        proof: Proof,
        now: u64,
    ) -> Result<bool, W> {
        let confirmed = self.confirmed.contains_key(&proof.id);
        let record = if confirmed {
            self.confirmed.get(&proof.id)
        } else {
            self.pending.get(&proof.id)
        };
        let Some(record) = record else {
            return Ok(false);
        };
        if record.view.generation != proof.generation {
            return Ok(false);
        }
        if !confirmed {
            for ticket in &record.tickets {
                let Some(written) = record.written.get(&ticket.id) else {
                    return Ok(false);
                };
                let Some((status, _)) = store
                    .component_mode_ticket(proof.generation.boot_id(), ticket.id)
                    .map_err(|_| W::ServiceCommunicationFailure)?
                else {
                    return Ok(false);
                };
                if status != "written"
                    && (status != "observed"
                        || matches!(ticket.command, Command::PanelLed(_) | Command::PanelLedOff))
                {
                    return Ok(false);
                }
                if proof
                    .lease
                    .observations
                    .get(&ticket.device_id)
                    .is_some_and(|at| at.is_none_or(|at| at <= *written))
                {
                    return Ok(false);
                }
            }
        }
        let Some(matched) = proof.matched else {
            return Ok(false);
        };
        let _guard = proof
            .lease
            .lock(
                Uuid::parse_str(proof.generation.boot_id())
                    .map_err(|_| W::InvalidValueInRequest)?
                    .as_u128(),
                proof.generation.epochs.configuration,
                proof.generation.epochs.transport,
                now,
            )
            .map_err(|_| W::ResourceBusy)?;
        for (id, at) in &proof.lease.observations {
            if !store
                .component_mode_feedback_settled(
                    proof.generation.boot_id(),
                    id,
                    *at,
                    proof.generation.epochs.transport,
                    &record.written,
                )
                .map_err(|_| W::ServiceCommunicationFailure)?
            {
                return Err(W::ResourceBusy);
            }
        }
        if confirmed {
            if matched {
                self.confirmed.get_mut(&proof.id).unwrap().deadline = proof.valid_until;
                return Ok(true);
            }
            Self::invalidate(store, record, now)?;
            self.confirmed.remove(&proof.id);
            return Ok(false);
        }
        if !matched {
            return Ok(false);
        }
        let writes = record
            .updates
            .iter()
            .map(|u| {
                (
                    u.clone(),
                    record
                        .view
                        .policies
                        .iter()
                        .find(|p| p.variable == u.name)
                        .unwrap()
                        .id
                        .clone(),
                )
            })
            .collect::<Vec<_>>();
        let tickets = record.tickets.iter().map(|c| c.id).collect::<Vec<_>>();
        let result = crate::source_auth::commit(record.source.as_ref(), || {
            store.apply_component_mode_inference(
                &writes,
                &record.guards,
                proof.generation.boot_id(),
                &record.controller.to_string(),
                now,
                &tickets,
                true,
            )
        })
        .ok_or(W::InsufficientPrivileges)?;
        match result {
            Err(homesense_storage::Error::Conflict) => {
                if let Some(record) = self.pending.remove(&proof.id) {
                    Self::invalidate_prior(store, &record, now)?;
                }
                return Ok(false);
            }
            Err(_) => return Err(W::ServiceCommunicationFailure),
            Ok(()) => {}
        }
        let record = self.pending.remove(&proof.id).unwrap();
        for policy in &record.view.policies {
            let mut update = record
                .updates
                .iter()
                .find(|u| u.name == policy.variable)
                .unwrap()
                .clone();
            update.expected_version += 1;
            let id = self.allocate()?;
            let mut manual = BTreeMap::new();
            let mut gateways = BTreeSet::new();
            for condition in &policy.postconditions {
                let device =
                    DeviceId::new(&condition.device).map_err(|_| W::InvalidValueInRequest)?;
                let related = std::iter::once(device.clone()).chain(
                    config
                        .devices()
                        .get(&device)
                        .and_then(|d| d.supply_relay.clone()),
                );
                for device in related {
                    manual.insert(
                        device.clone(),
                        record.manual.get(&device).copied().unwrap_or(0),
                    );
                    if let Some(binding) = config.devices().get(&device) {
                        gateways.insert(binding.gateway.clone());
                    }
                }
            }
            let after = record.view.after.clone();
            self.confirmed.insert(
                id,
                Record {
                    view: View {
                        id,
                        generation: proof.generation.clone(),
                        policies: vec![policy.clone()],
                        after,
                    },
                    controller: record.controller,
                    source: record.source.clone(),
                    updates: vec![update],
                    guards: vec![],
                    prior_known: vec![],
                    tickets: record.tickets.clone(),
                    written: record.written.clone(),
                    manual,
                    wires: record
                        .wires
                        .iter()
                        .filter(|w| gateways.contains(w.gateway()))
                        .cloned()
                        .collect(),
                    deadline: proof.valid_until,
                },
            );
        }
        Ok(true)
    }
}
#[derive(Default)]
pub(crate) struct Consumer {
    next: usize,
}
impl Consumer {
    pub(crate) async fn tick(
        &mut self,
        admission: &Admission,
        controller: Uuid,
        document: &Document,
        feedback: &Feedback,
    ) -> Result<(), String> {
        if document.component_mode_confirmations.is_empty() {
            return Ok(());
        }
        let generation = match admission.task_generation().await {
            Ok(g) => g,
            Err(W::ResourceBusy) => return Ok(()),
            Err(e) => return Err(format!("component generation: {e:?}")),
        };
        let views = match admission
            .component_mode_views(controller, generation.clone())
            .await
        {
            Ok(v) => v,
            Err(W::ResourceBusy) => return Ok(()),
            Err(e) => return Err(format!("component views: {e:?}")),
        };
        if views.is_empty() {
            return Ok(());
        }
        let bundle = admission
            .automation_bundle(controller, generation)
            .await
            .map_err(|e| format!("component bundle: {e:?}"))?;
        for i in 0..views.len().min(4) {
            let view = &views[(self.next + i) % views.len()];
            let proof = match Proof::prepare(
                view,
                &bundle.configuration,
                feedback,
                admission.monotonic_ms(),
            ) {
                Ok(p) => p,
                Err(_) => continue,
            };
            match admission.component_mode_proof(controller, proof).await {
                Ok(_)
                | Err(W::ResourceBusy | W::InvalidValueInRequest | W::InsufficientPrivileges) => {}
                Err(e) => return Err(format!("component proof: {e:?}")),
            }
        }
        self.next = (self.next + 4) % views.len();
        Ok(())
    }
}
