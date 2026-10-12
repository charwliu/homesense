//! Admission-thread scene observer: trusted events, current variables and source
//! evidence commit synchronously. It cannot construct a physical command.
use homesense_config::rule_document::Document;
use homesense_core::{
    DeviceId, Observation,
    queue::Epochs,
    scene_observer::{Authority, Group, Key, Observer, Press, Scope, Selection},
};
use homesense_storage::{Error, Store};
use std::collections::BTreeMap;

struct Session {
    scope: Scope,
    observer: Observer,
    variables: BTreeMap<String, String>,
    devices: BTreeMap<DeviceId, String>,
    /// Last actual versions, including inferred versions admitted by this owner.
    versions: BTreeMap<String, u64>,
    local_fences: BTreeMap<String, u64>,
}
#[derive(Default)]
pub(crate) struct Owner {
    next_stream: u64,
    session: Option<Session>,
}
impl Owner {
    pub(crate) fn scope(&self) -> Option<Scope> {
        self.session.as_ref().map(|s| s.scope)
    }
    pub(crate) fn invalidate(&mut self) {
        self.session = None;
    }
    /// Run immediately after an admission operation, before awaiting the next
    /// message. Any independent variable update fences out older queued presses.
    pub(crate) fn refresh_local_versions(&mut self, store: &Store, now: u64) -> Result<(), Error> {
        let Some(session) = &mut self.session else {
            return Ok(());
        };
        for (name, version) in &mut session.versions {
            let current = store
                .scene_selection(
                    session.scope.epochs.configuration,
                    name,
                    Some(session.scope),
                )?
                .ok_or(Error::InvalidAudit)?
                .stored
                .version;
            if current != *version {
                *version = current;
                session.local_fences.insert(name.clone(), now);
            }
        }
        Ok(())
    }
    pub(crate) fn begin(
        &mut self,
        store: &Store,
        document: &Document,
        snapshot: &homesense_state::Snapshot,
        boot: u128,
        epochs: Epochs,
        now: u64,
    ) -> Result<Scope, Error> {
        let settings = document
            .scene_observation
            .as_ref()
            .filter(|s| s.enabled)
            .ok_or(Error::InvalidAudit)?;
        if snapshot.cursor.boot_id != boot || document.revision != epochs.configuration {
            return Err(Error::Conflict);
        }
        let generation = self.next_stream.checked_add(1).ok_or(Error::InvalidAudit)?;
        let scope = Scope {
            boot_id: boot,
            epochs,
            stream_generation: generation,
        };
        let mut variables = BTreeMap::new();
        let mut groups = vec![];
        let mut keys = vec![];
        let mut held = vec![];
        let mut versions = BTreeMap::new();
        let mut devices = BTreeMap::new();
        for binding in &settings.bindings {
            let group = document
                .scene_switch_groups
                .iter()
                .find(|g| g.id == binding.group)
                .ok_or(Error::InvalidAudit)?;
            if !variables.contains_key(&group.id) {
                variables.insert(group.id.clone(), group.variable.clone());
                groups.push(Group {
                    id: group.id.clone(),
                    members: group.members.iter().map(|m| m.value).collect(),
                });
                versions.insert(
                    group.variable.clone(),
                    store
                        .scene_selection(document.revision, &group.variable, Some(scope))?
                        .ok_or(Error::InvalidAudit)?
                        .stored
                        .version,
                );
            }
            let device = DeviceId::new(&binding.device).map_err(|_| Error::InvalidAudit)?;
            if snapshot.devices.get(&device).is_some_and(|s| {
                s.quality == homesense_state::Quality::Confirmed
                    && s.observed == Some(Observation::Button { pressed: true })
            }) {
                held.push(device.clone());
            }
            devices.insert(device.clone(), group.id.clone());
            keys.push(Key {
                device,
                group: group.id.clone(),
                member: binding.member,
            });
        }
        let mut observer = Observer::new(
            scope,
            snapshot.cursor.sequence,
            now,
            groups,
            keys,
            settings.debounce_ms,
            settings.max_age_ms,
        )
        .map_err(|_| Error::InvalidAudit)?;
        observer
            .initially_held(&held)
            .map_err(|_| Error::InvalidAudit)?;
        self.session = Some(Session {
            scope,
            observer,
            variables,
            devices,
            versions,
            local_fences: BTreeMap::new(),
        });
        self.next_stream = generation;
        Ok(scope)
    }
    pub(crate) fn observe(
        &mut self,
        store: &mut Store,
        scope: Scope,
        event: &homesense_state::Event,
        now: u64,
    ) -> Result<(), Error> {
        let session = self
            .session
            .as_mut()
            .filter(|s| s.scope == scope)
            .ok_or(Error::Conflict)?;
        let Some(group) = session.devices.get(&event.device_id) else {
            return Ok(());
        };
        let variable = session.variables.get(group).ok_or(Error::InvalidAudit)?;
        let revision = scope.epochs.configuration;
        let stored = store
            .scene_selection(revision, variable, Some(scope))?
            .ok_or(Error::InvalidAudit)?;
        let Some(Observation::Button { pressed }) = event.state.observed else {
            return Err(Error::InvalidAudit);
        };
        let at = event.state.observed_at_ms.ok_or(Error::InvalidAudit)?;
        let press = Press {
            scope,
            sequence: event.cursor.sequence,
            device: event.device_id.clone(),
            down: pressed,
            observed_ms: at,
        };
        // Do not clear a newer manual selection merely to consume a delayed key.
        // The state machine still consumes/latches the event to avoid replay.
        let fenced = session
            .local_fences
            .get(variable)
            .is_some_and(|fence| at <= *fence);
        let selection = if fenced {
            Selection {
                value: None,
                version: stored.stored.version,
                authority: Authority::Recorded,
            }
        } else {
            stored.selection()
        };
        if let Some(inference) = session
            .observer
            .observe(&press, selection, now)
            .map_err(|_| Error::InvalidAudit)?
            && !fenced
        {
            store.record_scene_inference(revision, variable, &inference)?;
            log::info!(
                "scene selection source=inferred group={} basis={:?} event={}",
                inference.group,
                inference.basis,
                inference.sequence
            );
            session.versions.insert(
                variable.clone(),
                inference
                    .expected_version
                    .checked_add(1)
                    .ok_or(Error::InvalidAudit)?,
            );
        }
        Ok(())
    }
}
