//! Construct both routers from one reviewed document and captured generation.
//! Declares variables without overwriting existing values. No subscriptions or
//! control commands are started.
use crate::{
    admission::Admission,
    event_rules::EventRules,
    panel_rules::{Binding, Key, Mode, PanelRules},
};
use homesense_config::rule_document::{Document, PanelEvent, Trigger};
use homesense_core::{
    DeviceId,
    gesture::{Event, Settings},
    rule::{Budget, Lifetime},
    scene_expand::Definition,
};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;
pub struct ConfiguredRules {
    pub events: EventRules,
    pub panels: PanelRules,
}
impl ConfiguredRules {
    pub async fn load_document(
        admission: Admission,
        controller: Uuid,
        document: &Document,
        budget: Budget,
    ) -> Result<Self, String> {
        Self::load(
            admission,
            controller,
            document,
            document.immediate_scenes.clone(),
            document.schedules.iter().map(|s| s.id.clone()).collect(),
            document.gestures.clone(),
            budget,
        )
        .await
    }

    pub async fn load(
        admission: Admission,
        controller: Uuid,
        document: &Document,
        scenes: BTreeMap<String, Definition>,
        schedules: BTreeSet<String>,
        gestures: BTreeMap<DeviceId, Settings>,
        budget: Budget,
    ) -> Result<Self, String> {
        let generation = admission
            .task_generation()
            .await
            .map_err(|e| format!("{e:?}"))?;
        if document.revision != generation.epochs.configuration {
            return Err("automation document revision expired".into());
        }
        let linked = document
            .linked_rules(&scenes, &schedules)
            .map_err(|e| e.to_string())?;
        for rule in &linked {
            if let Trigger::Panel { device, event } = &rule.trigger {
                let id = DeviceId::new(device).map_err(|e| e.to_string())?;
                let settings = gestures
                    .get(&id)
                    .ok_or("explicit gesture settings required")?;
                homesense_core::gesture::KeyGesture::new(*settings, admission.monotonic_ms())
                    .map_err(|e| format!("{e:?}"))?;
                if matches!(event, PanelEvent::DoubleClick) && !settings.double_click_enabled {
                    return Err("double-click requires deferred single-click mode".into());
                }
            }
        }
        if let Some(holds) = &document.holds {
            for hold in holds.bindings.iter().filter(|h| h.enabled) {
                let key = DeviceId::new(&hold.key).map_err(|e| e.to_string())?;
                let settings = *gestures.get(&key).ok_or("hold gesture settings required")?;
                homesense_core::gesture::KeyGesture::new(settings, admission.monotonic_ms())
                    .map_err(|e| format!("{e:?}"))?;
            }
        }
        admission
            .initialize_variables(
                controller,
                generation.clone(),
                document
                    .variables
                    .iter()
                    .filter(|(_, v)| v.lifetime != Lifetime::FeedbackCache)
                    .map(|(name, v)| (name.clone(), v.clone()))
                    .collect(),
            )
            .await
            .map_err(|e| format!("variable initialization: {e:?}"))?;
        let mut keys: BTreeMap<DeviceId, Key> = BTreeMap::new();
        let mut non_panel = vec![];
        for rule in linked {
            let Trigger::Panel { device, event } = &rule.trigger else {
                non_panel.push(rule);
                continue;
            };
            let device = DeviceId::new(device).map_err(|e| e.to_string())?;
            let settings = gestures.get(&device).ok_or_else(|| {
                format!("{}: explicit gesture settings required", device.as_str())
            })?;
            let variables: Vec<_> = homesense_core::rule::variable_references(&rule.actions)
                .map_err(str::to_owned)?
                .into_iter()
                .collect();
            for name in &variables {
                if document.variables[name].lifetime == Lifetime::FeedbackCache {
                    return Err(format!("{name}: panel runtime scope not yet supported"));
                }
            }
            let stored = admission
                .variable_snapshot(controller, generation.clone(), variables.clone())
                .await
                .map_err(|e| format!("panel variable snapshot: {e:?}"))?;
            for (name, value) in stored {
                if value.kind != document.variables[&name].value_type
                    || value.lifetime != document.variables[&name].lifetime
                {
                    return Err(format!("{name}: stored panel variable type mismatch"));
                }
            }
            let event = match event {
                PanelEvent::Pressed => Event::Pressed,
                PanelEvent::Released => Event::Released,
                PanelEvent::Click => Event::Click,
                PanelEvent::DoubleClick => Event::DoubleClick,
                PanelEvent::LongStart => Event::LongStart,
                PanelEvent::LongEnd => Event::LongEnd,
                PanelEvent::Cancelled => Event::Cancelled,
            };
            keys.entry(device.clone())
                .or_insert_with(|| Key {
                    device,
                    settings: *settings,
                    bindings: vec![],
                })
                .bindings
                .push(Binding {
                    id: rule.id,
                    event,
                    variables,
                    actions: rule.actions,
                });
        }
        let mut hold_bindings = BTreeMap::new();
        if let Some(holds) = &document.holds {
            for hold in holds.bindings.iter().filter(|h| h.enabled) {
                let key = DeviceId::new(&hold.key).map_err(|e| e.to_string())?;
                let settings = *gestures.get(&key).ok_or("hold gesture settings required")?;
                keys.entry(key.clone()).or_insert_with(|| Key {
                    device: key.clone(),
                    settings,
                    bindings: vec![],
                });
                hold_bindings.insert(key, crate::panel_rules::HoldBinding {
                    device: DeviceId::new(&hold.device).map_err(|e| e.to_string())?,
                    up: hold.up,
                    release: match hold.release {
                        homesense_config::rule_document::HoldRelease::CancelPendingOnly => crate::hold_dimming::ReleasePolicy::CancelPendingOnly,
                        homesense_config::rule_document::HoldRelease::PauseRampFieldVerified => crate::hold_dimming::ReleasePolicy::PauseRampFieldVerified,
                    },
                });
            }
        }
        let events = EventRules::from_document(
            admission.clone(),
            controller,
            &Document {
                revision: document.revision,
                variables: document.variables.clone(),
                feedback_bindings: document.feedback_bindings.clone(),
                rules: non_panel,
                immediate_scenes: Default::default(),
                schedules: vec![],
                gestures: Default::default(),
                holds: None,
                timed_scenes: document.timed_scenes.clone(),
            },
            scenes,
            schedules,
            budget,
        )
        .await?;
        let mut panels = PanelRules::new(
            admission.clone(),
            controller,
            Mode::Execute,
            keys.into_values().collect(),
            Default::default(),
            admission.monotonic_ms(),
        )
        .await?;
        if let Some(holds) = &document.holds
            && !hold_bindings.is_empty()
        {
            panels.configure_holds(
                hold_bindings,
                admission.monotonic_ms(),
                holds.interval_ms,
                holds.increment,
            )?;
        }
        if admission
            .task_generation()
            .await
            .map_err(|e| format!("{e:?}"))?
            != generation
        {
            panels.disconnect();
            return Err("automation generation changed during load".into());
        }
        Ok(Self { events, panels })
    }
}
