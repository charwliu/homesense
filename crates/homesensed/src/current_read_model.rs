//! One runtime-owned, bounded current-state publication. Readers never start a
//! sampling pass. Protocol adapters retain their own live authorization fences.
use crate::automation_control::{Action, Reply, SceneCatalogEntry};
use homesense_core::DeviceId;
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::{
        Arc, RwLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use uuid::Uuid;

const PERIOD: Duration = Duration::from_millis(250);
const MAX_PUBLICATION_AGE: Duration = Duration::from_millis(1500);

#[cfg(test)]
mod cadence_tests {
    use super::*;

    #[test]
    fn slow_pass_does_not_add_another_full_period_to_its_work() {
        let started = Instant::now() - Duration::from_millis(650);
        let mut owner = Owner { started: Some(started), ..Default::default() };
        owner.completed(Ok(Reply::SceneCatalog(BTreeMap::new())));
        // A second 650ms pass fits the original1.5s freshness bound only
        // when an overdue producer starts immediately, without another250ms.
        let next = owner.next.unwrap();
        assert!(next <= Instant::now(), "slow work must consume, not extend, the cadence");
        assert!(next + Duration::from_millis(650) < started + MAX_PUBLICATION_AGE);
        assert_eq!(owner.shared.current().unwrap().started, started,
            "completion cannot relabel old observations as newly sampled");
    }
}

#[derive(Clone, Default)]
struct Frame {
    devices: Option<crate::management_devices::DeviceSnapshot>,
    modules: Vec<crate::management_modules::Page>,
    modules_complete: bool,
    module_max_age_ms: u64,
    selections: BTreeMap<String, crate::scene_switches::Details>,
    catalog: BTreeMap<String, SceneCatalogEntry>,
    loads: BTreeMap<String, Option<bool>>,
    ready: bool,
    selections_complete: bool,
    catalog_complete: bool,
    selection_version: u64,
    catalog_version: u64,
    household_scope: BTreeSet<DeviceId>,
    physical_deadlines: BTreeMap<DeviceId, u64>,
    activity_deadline: Option<u64>,
    selection_deadlines: BTreeMap<String, u64>,
}
struct Published {
    started: Instant,
    frame: Frame,
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct Stamp {
    sample: Instant,
    selection: u64,
}
#[derive(Clone)]
pub(crate) struct Shared {
    data: Arc<RwLock<Option<Arc<Published>>>>,
    terminal: tokio::sync::watch::Sender<()>,
    updates: tokio::sync::watch::Sender<()>,
    changed: Arc<AtomicU64>,
    exhausted: Arc<AtomicBool>,
}
impl Default for Shared {
    fn default() -> Self {
        let (terminal, _) = tokio::sync::watch::channel(());
        let (updates, _) = tokio::sync::watch::channel(());
        Self {
            data: Default::default(),
            terminal,
            updates,
            changed: Default::default(),
            exhausted: Default::default(),
        }
    }
}
impl Shared {
    pub(crate) fn terminal_updates(&self) -> tokio::sync::watch::Receiver<()> {
        self.terminal.subscribe()
    }
    pub(crate) fn stamp(&self) -> Option<Stamp> {
        let current = self.current().ok()?;
        Some(Stamp {
            sample: current.started,
            selection: current.frame.selection_version,
        })
    }
    pub(crate) fn state_updates(&self) -> tokio::sync::watch::Receiver<()> {
        self.updates.subscribe()
    }
    pub(crate) fn mark_changed(&self) {
        if self
            .changed
            .try_update(Ordering::AcqRel, Ordering::Acquire, |version| {
                version.checked_add(1)
            })
            .is_err()
        {
            self.exhausted.store(true, Ordering::Release);
            self.invalidate();
        }
    }
    pub(crate) fn ready_for(&self, action: &Action) -> bool {
        let Ok(current) = self.current() else {
            return false;
        };
        match action {
            Action::ModuleEvents { .. } => current.frame.modules_complete,
            Action::Devices { .. } | Action::DeviceEvents { .. } => current.frame.devices.is_some(),
            Action::SceneSelectionDetails { .. } | Action::SceneSwitchStates { .. } => {
                current.frame.selections_complete
                    && current.frame.selection_version == self.changed.load(Ordering::Acquire)
            }
            _ => current.frame.catalog_complete,
        }
    }
    fn current(&self) -> Result<Arc<Published>, String> {
        if self.exhausted.load(Ordering::Acquire) {
            return Err("current state version exhausted".into());
        }
        let frame = self
            .data
            .read()
            .map_err(|_| "current state publication poisoned")?
            .clone()
            .ok_or("current state not sampled")?;
        if frame.started.elapsed() >= MAX_PUBLICATION_AGE {
            return Err("current state publication expired".into());
        }
        Ok(frame)
    }
    pub(crate) fn lights(
        &self,
        now: u64,
        config: &homesense_config::Configuration,
    ) -> Result<crate::management_devices::LightPublication, String> {
        let current = self.current()?;
        let mut value = current
            .frame
            .devices
            .as_ref()
            .and_then(|snapshot| snapshot.light_publication.clone())
            .ok_or("current light publication unavailable")?;
        for (id, state) in &mut value.physical.devices {
            if current
                .frame
                .physical_deadlines
                .get(id)
                .is_some_and(|until| now >= *until)
                && state.quality == homesense_state::Quality::Confirmed
            {
                state.quality = homesense_state::Quality::Stale;
            }
        }
        for (id, state) in &mut value.logical.devices {
            if config.logical_components(id).iter().any(|component| {
                current
                    .frame
                    .physical_deadlines
                    .get(*component)
                    .is_some_and(|until| now >= *until)
            }) && state.quality == homesense_state::Quality::Confirmed
            {
                state.quality = homesense_state::Quality::Stale;
            }
        }
        Ok(value)
    }
    pub(crate) fn invalidate(&self) {
        if let Ok(mut frame) = self.data.write() {
            *frame = None;
        }
        self.updates.send_replace(());
    }
    pub(crate) fn ready(&self) -> bool {
        self.current()
            .is_ok_and(|frame| frame.frame.ready && frame.frame.selections_complete)
    }
    pub(crate) fn reply(
        &self,
        action: &Action,
        now: u64,
        configuration: &homesense_config::Configuration,
        transport_epoch: u64,
    ) -> Option<Result<Reply, String>> {
        if !matches!(
            action,
            Action::Devices { .. }
                | Action::ModuleEvents { .. }
                | Action::DeviceEvents { .. }
                | Action::SceneSelectionDetails { .. }
                | Action::SceneCatalog { .. }
                | Action::SceneLoadStates { .. }
                | Action::SceneTracking { .. }
                | Action::SceneSwitchStates { .. }
        ) {
            return None;
        }
        Some((|| {
            let current = self.current()?;
            let frame = &current.frame;
            match action {
                Action::ModuleEvents { .. } => {
                    let mut pages = frame.modules.clone();
                    for page in &mut pages {
                        crate::management_modules::expire_page(page, now, frame.module_max_age_ms);
                    }
                    Ok(Reply::ModuleEvents(pages))
                }
                Action::Devices { ids, .. } => {
                    scoped_devices(frame, ids, now, configuration).map(Reply::Devices)
                }
                Action::DeviceEvents {
                    controller,
                    ids,
                    cursor,
                    scope,
                    ..
                } => {
                    let snapshot = scoped_devices(frame, ids, now, configuration)?;
                    // State delivery coalesces to the latest view; it never
                    // replays gestures or control requests. The scope is still
                    // tied to this reader and its current grant.
                    let next = crate::management_events::scoped_cursor(
                        *controller,
                        scope,
                        ids,
                        &snapshot,
                        transport_epoch,
                    );
                    let reset = cursor.as_ref().is_none_or(|old| {
                        old.boot != next.boot
                            || old.revision != next.revision
                            || old.transport_epoch != next.transport_epoch
                            || old.scope != next.scope
                            || old.sequence > next.sequence
                    });
                    Ok(Reply::DeviceEvents(crate::management_events::Update {
                        cursor: next,
                        reset,
                        snapshot,
                    }))
                }
                Action::SceneTracking { scene, .. } => {
                    let row = frame
                        .catalog
                        .get(scene)
                        .ok_or("scene tracking not in current catalog")?;
                    let mut tracking = row.tracking.clone();
                    if let Some(value) = &mut tracking
                        && (frame.catalog_version != self.changed.load(Ordering::Acquire)
                            || value
                                .valid_until_ms
                                .or(frame.activity_deadline)
                                .is_some_and(|until| now >= until))
                    {
                        value.activity = crate::scene_activity::SceneActivity::Unknown;
                        value.action_available = false;
                    }
                    Ok(Reply::SceneTracking(tracking))
                }
                Action::SceneSwitchStates { .. } => Ok(Reply::SceneSwitchStates(
                    frame
                        .selections
                        .iter()
                        .map(|(id, value)| {
                            let expired = frame
                                .selection_deadlines
                                .get(id)
                                .is_some_and(|until| now >= *until);
                            (id.clone(), if expired { None } else { value.selected })
                        })
                        .collect(),
                )),
                Action::SceneSelectionDetails { .. } => {
                    let mut values = frame.selections.clone();
                    for (id, value) in &mut values {
                        if frame
                            .selection_deadlines
                            .get(id)
                            .is_some_and(|until| now >= *until)
                        {
                            value.selected = None;
                            value.selection_state = crate::scene_switches::SelectionState::Unknown;
                            value.observation_current = Some(false);
                            value.relative_unknown_allowed = false;
                        }
                    }
                    Ok(Reply::SceneSelectionDetails(values))
                }
                Action::SceneCatalog { scenes, .. } => {
                    if scenes.len() > 2048
                        || scenes.iter().collect::<BTreeSet<_>>().len() != scenes.len()
                    {
                        return Err("scene catalog scope invalid".into());
                    }
                    let mut rows = BTreeMap::new();
                    for id in scenes {
                        let mut row = frame
                            .catalog
                            .get(id)
                            .cloned()
                            .ok_or("scene not in current catalog")?;
                        if row.tracking.is_none()
                            && (frame.catalog_version != self.changed.load(Ordering::Acquire)
                                || frame.activity_deadline.is_some_and(|until| now >= until))
                        {
                            row.activity = crate::scene_activity::SceneActivity::Unknown;
                        }
                        if let Some(tracking) = &mut row.tracking
                            && (frame.catalog_version != self.changed.load(Ordering::Acquire)
                                || tracking
                                    .valid_until_ms
                                    .or(frame.activity_deadline)
                                    .is_some_and(|until| now >= until))
                        {
                            tracking.activity = crate::scene_activity::SceneActivity::Unknown;
                            tracking.action_available = false;
                            row.activity = crate::scene_activity::SceneActivity::Unknown;
                        }
                        rows.insert(id.clone(), row);
                    }
                    Ok(Reply::SceneCatalog(rows))
                }
                Action::SceneLoadStates { scenes, .. } => {
                    if scenes.is_empty() || scenes.len() > 64 {
                        return Err("load state scope invalid".into());
                    }
                    scenes
                        .iter()
                        .map(|id| {
                            frame
                                .loads
                                .get(id)
                                .map(|value| {
                                    let expired = frame.catalog_version
                                        != self.changed.load(Ordering::Acquire)
                                        || frame
                                            .catalog
                                            .get(id)
                                            .and_then(|row| row.tracking.as_ref())
                                            .and_then(|tracking| tracking.valid_until_ms)
                                            .or(frame.activity_deadline)
                                            .is_some_and(|until| now >= until);
                                    (id.clone(), if expired { None } else { *value })
                                })
                                .ok_or_else(|| "unknown load-backed HAP action".to_owned())
                        })
                        .collect::<Result<BTreeMap<_, _>, _>>()
                        .map(Reply::SceneLoadStates)
                }
                _ => unreachable!(),
            }
        })())
    }
}

fn scoped_devices(
    frame: &Frame,
    ids: &[DeviceId],
    now: u64,
    config: &homesense_config::Configuration,
) -> Result<crate::management_devices::DeviceSnapshot, String> {
    if ids.len() > crate::web_auth::MAX_DEVICE_SCOPE
        || ids.iter().collect::<BTreeSet<_>>().len() != ids.len()
    {
        return Err("invalid device read scope".into());
    }
    let visible = ids.iter().cloned().collect::<BTreeSet<_>>();
    for id in ids {
        if config.binding(id).is_none() {
            return Err("unknown or disabled device".into());
        }
    }
    let mut out = frame.devices.clone().ok_or("current devices not sampled")?;
    out.light_publication = None;
    out.devices.retain(|view| visible.contains(&view.id));
    for view in &mut out.devices {
        if view.state.quality == homesense_state::Quality::Confirmed
            && matches!(
                view.state.kind,
                homesense_core::ModuleKind::Switch | homesense_core::ModuleKind::Dimmer
            )
            && config.logical_components(&view.id).iter().any(|component| {
                frame
                    .physical_deadlines
                    .get(*component)
                    .is_some_and(|until| now >= *until)
            })
        {
            view.state.quality = homesense_state::Quality::Stale;
        }
    }
    crate::management_keypads::scope_current(&mut out.keypads, &visible, now);
    out.keypad_endpoints.retain(|id| visible.contains(id));
    out.scoped_catalog_rooms = out
        .devices
        .iter()
        .filter(|view| view.catalog_visible)
        .filter_map(|view| view.room.clone())
        .chain(
            out.keypads
                .iter()
                .filter_map(|panel| serde_json::from_value(panel["room"].clone()).ok()),
        )
        .map(|room: homesense_config::Room| (room.id.clone(), room))
        .collect::<BTreeMap<_, _>>()
        .into_values()
        .collect();
    if frame.household_scope.is_empty() || !frame.household_scope.is_subset(&visible) {
        out.household_operations.clear();
    }
    Ok(out)
}

fn install_selections(
    frame: &mut Frame,
    document: Option<&homesense_config::rule_document::Document>,
    value: BTreeMap<String, crate::scene_switches::Details>,
    version: u64,
) {
    frame.selection_deadlines.clear();
    if let Some(doc) = document {
        for (id, detail) in &value {
            if detail.observation_current != Some(true) {
                continue;
            }
            let provider = doc
                .scene_presentations
                .get(id)
                .map_or(id.as_str(), |presentation| {
                    presentation.selection_scene.as_str()
                });
            let age = doc
                .scene_switch_groups
                .iter()
                .find(|group| group.members.iter().any(|member| member.scene == provider))
                .and_then(|group| {
                    group
                        .reconciliation
                        .as_ref()
                        .filter(|policy| policy.enabled)
                        .map(|policy| policy.max_age_ms)
                })
                .unwrap_or(5000);
            if let Some(at) = detail
                .observed_ms
                .as_ref()
                .and_then(|at| at.parse::<u64>().ok())
            {
                frame
                    .selection_deadlines
                    .insert(id.clone(), at.saturating_add(age));
            }
        }
    }

    frame.selections = value;
    frame.selections_complete = true;
    frame.selection_version = version;
}

/// Incremental sampling stays on the existing runtime owner, one small read
/// quantum per drive turn. No consumer owns a timer, queue, or projection job.
#[derive(Default)]
pub(crate) struct Owner {
    pub(crate) shared: Shared,
    plan: VecDeque<Action>,
    frame: Frame,
    started: Option<Instant>,
    next: Option<Instant>,
    controller: Option<Uuid>,
    configuration: Option<homesense_config::Configuration>,
    document: Option<homesense_config::rule_document::Document>,
    pub(crate) samples: u64,
    max_age_ms: u64,
    protected: bool,
    terminal_source: Option<tokio::sync::watch::Receiver<()>>,
    terminal_pending: bool,
    urgent_selection: bool,
    last_selection_version: u64,
}
impl Owner {
    pub(crate) fn bind(
        &mut self,
        controller: Uuid,
        configuration: homesense_config::Configuration,
        document: homesense_config::rule_document::Document,
        terminal: tokio::sync::watch::Receiver<()>,
    ) {
        self.terminal_source = Some(terminal);
        self.controller = Some(controller);
        self.configuration = Some(configuration);
        self.document = Some(document);
        self.shared.invalidate();
        self.next = None;
        self.plan.clear();
        self.started = None;
    }
    pub(crate) fn observation_age(&mut self, age: u64) {
        self.max_age_ms = age;
    }
    pub(crate) fn next_request(&mut self, protected: bool) -> Option<Action> {
        if let Some(source) = &mut self.terminal_source
            && source.has_changed().unwrap_or(false)
        {
            source.borrow_and_update();
            self.terminal_pending = true;
        }
        if self.protected != protected {
            self.protected = protected;
            self.plan.clear();
            self.started = None;
            self.next = None;
        }
        if !protected
            && self.last_selection_version != self.shared.changed.load(Ordering::Acquire)
            && self
                .shared
                .data
                .read()
                .ok()
                .is_some_and(|data| data.is_some())
        {
            self.last_selection_version = self.shared.changed.load(Ordering::Acquire);
            self.urgent_selection = true;
            return Some(Action::SceneSelectionDetails {
                controller: self.controller?,
                revision: self.document.as_ref()?.revision,
            });
        }
        if self.plan.is_empty() {
            if self.started.is_some() || self.next.is_some_and(|next| Instant::now() < next) {
                return None;
            }
            let (controller, config, doc) = (
                self.controller?,
                self.configuration.as_ref()?,
                self.document.as_ref()?,
            );
            let revision = doc.revision;
            self.started = Some(Instant::now());
            self.frame = Frame::default();
            self.frame.catalog_version = self.shared.changed.load(Ordering::Acquire);
            self.frame.household_scope = doc
                .away_off
                .as_ref()
                .into_iter()
                .flat_map(|policy| &policy.modules)
                .flat_map(|module| &module.devices)
                .filter_map(|id| DeviceId::new(id).ok())
                .collect();
            self.plan.push_back(Action::Diagnostics {
                controller,
                revision,
            });
            self.plan.push_back(Action::Devices {
                controller,
                revision,
                ids: config
                    .devices()
                    .iter()
                    .filter(|(_, d)| d.enabled)
                    .map(|(id, _)| id.clone())
                    .collect(),
            });
            if protected {
                if let Ok(previous) = self.shared.current() {
                    self.frame.selections_complete = previous.frame.selections_complete;
                    self.frame.catalog_complete = previous.frame.catalog_complete;
                    self.frame.selection_version = self.shared.changed.load(Ordering::Acquire);
                    self.frame.selections = previous.frame.selections.clone();
                    for value in self.frame.selections.values_mut() {
                        value.selected = None;
                        value.selection_state = crate::scene_switches::SelectionState::Unknown;
                        value.observation_current = None;
                        value.relative_unknown_allowed = false;
                    }
                    self.frame.catalog = previous.frame.catalog.clone();
                    for row in self.frame.catalog.values_mut() {
                        row.activity = crate::scene_activity::SceneActivity::Unknown;
                        row.action_gate.available = false;
                        if let Some(tracking) = &mut row.tracking {
                            tracking.activity = crate::scene_activity::SceneActivity::Unknown;
                            tracking.action_available = false;
                        }
                    }
                    self.frame.loads = previous
                        .frame
                        .loads
                        .keys()
                        .map(|id| (id.clone(), None))
                        .collect();
                }
                return self.plan.pop_front();
            }
            self.frame.module_max_age_ms = self.max_age_ms.min(config.refresh_policy().observation_max_age_ms());
            let total_modules = config.gateways().values().map(|g| g.modules().len()).sum::<usize>();
            for offset in (0..total_modules.max(1)).step_by(64) {
                self.plan.push_back(Action::SampleModules { controller, revision, offset, limit: 64 });
            }
            self.plan.push_back(Action::SceneSelectionDetails {
                controller,
                revision,
            });
            let ids = doc
                .scene_catalog()
                .into_keys()
                .chain(
                    doc.immediate_scenes
                        .keys()
                        .chain(doc.timed_scenes.keys())
                        .filter(|id| {
                            doc.hap_scene_action(id).is_some()
                                || doc
                                    .scene_tracking
                                    .get(*id)
                                    .is_some_and(|policy| policy.enabled)
                        })
                        .filter(|id| doc.scene_execution_kind(id).is_some())
                        .cloned(),
                )
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>();
            for chunk in ids.chunks(8) {
                self.plan.push_back(Action::SceneCatalog {
                    controller,
                    revision,
                    scenes: chunk.to_vec(),
                });
            }
        }
        self.plan.pop_front()
    }
    pub(crate) fn completed(&mut self, result: Result<Reply, String>) {
        if self.urgent_selection {
            self.urgent_selection = false;
            match result {
                Ok(Reply::SceneSelectionDetails(values)) => {
                    if let Ok(mut out) = self.shared.data.write()
                        && let Some(previous) = out.as_ref()
                    {
                        let started = previous.started;
                        let mut frame = previous.frame.clone();
                        install_selections(
                            &mut frame,
                            self.document.as_ref(),
                            values,
                            self.last_selection_version,
                        );
                        *out = Some(Arc::new(Published { started, frame }));
                    }
                    self.shared.updates.send_replace(());
                }
                _ => self.shared.invalidate(),
            }
            return;
        }
        match result {
            Ok(Reply::Diagnostics(value)) => self.frame.ready = value.ready(),
            Ok(Reply::Modules(mut page)) => {
                // Command history belongs to the explicit management endpoint,
                // not to the recurring state publication.
                page.commands.clear();
                self.frame.modules_complete = page.next_offset.is_none();
                self.frame.modules.push(page);
            }
            Ok(Reply::Devices(value)) => {
                if let Some(publication) = &value.light_publication {
                    self.frame.physical_deadlines = publication
                        .physical
                        .devices
                        .iter()
                        .filter(|(_, state)| state.quality == homesense_state::Quality::Confirmed)
                        .filter_map(|(id, state)| {
                            Some((
                                id.clone(),
                                state.observed_at_ms?.saturating_add(self.max_age_ms),
                            ))
                        })
                        .collect();
                    self.frame.activity_deadline =
                        self.frame.physical_deadlines.values().copied().min();
                }
                self.frame.devices = Some(value);
                if let Ok(mut out) = self.shared.data.write()
                    && out.is_none()
                    && let Some(started) = self.started
                {
                    *out = Some(Arc::new(Published {
                        started,
                        frame: self.frame.clone(),
                    }));
                    self.shared.updates.send_replace(());
                }
            }
            Ok(Reply::SceneSelectionDetails(value)) => {
                let version = self.shared.changed.load(Ordering::Acquire);
                self.last_selection_version = version;
                install_selections(&mut self.frame, self.document.as_ref(), value, version);
            }
            Ok(Reply::SceneCatalog(value)) => {
                for (id, row) in &value {
                    if self
                        .document
                        .as_ref()
                        .is_some_and(|doc| doc.hap_scene_action(id).is_some())
                    {
                        self.frame.loads.insert(
                            id.clone(),
                            row.tracking.as_ref().and_then(|tracking| tracking.active()),
                        );
                    }
                }
                self.frame.catalog.extend(value);
            }
            Ok(Reply::SceneLoadStates(value)) => self.frame.loads.extend(value),
            _ => {
                self.shared.invalidate();
                self.plan.clear();
                self.started = None;
                self.next = Some(Instant::now() + PERIOD);
                return;
            }
        }
        if self.plan.is_empty() {
            if !self.protected {
                self.frame.catalog_complete = true;
            }
            if let Ok(out) = self.shared.data.read()
                && let Some(previous) = out.as_ref()
                && previous.frame.selection_version > self.frame.selection_version
            {
                self.frame.selections = previous.frame.selections.clone();
                self.frame.selections_complete = previous.frame.selections_complete;
                self.frame.selection_deadlines = previous.frame.selection_deadlines.clone();
                self.frame.selection_version = previous.frame.selection_version;
            }
            let started = self.started.take().expect("sampling pass started");
            self.samples = self.samples.saturating_add(1);
            if started.elapsed() >= Duration::from_millis(250) {
                log::warn!(
                    "current_state_sample sample={} elapsed_ms={} published={}",
                    self.samples,
                    started.elapsed().as_millis(),
                    started.elapsed() < MAX_PUBLICATION_AGE
                );
            }
            if started.elapsed() < MAX_PUBLICATION_AGE {
                if let Ok(mut out) = self.shared.data.write() {
                    *out = Some(Arc::new(Published {
                        started,
                        frame: std::mem::take(&mut self.frame),
                    }));
                }
                self.shared.updates.send_replace(());
                if self.terminal_pending {
                    self.shared.terminal.send_replace(());
                    self.terminal_pending = false;
                }
            } else {
                self.shared.invalidate();
            }
            // Count work towards the fixed sampling cadence. Adding PERIOD
            // after a slow pass creates an avoidable publication-expiry gap.
            // The publication keeps its original start timestamp and TTL.
            self.next = Some(started + PERIOD);
        }
    }
}
