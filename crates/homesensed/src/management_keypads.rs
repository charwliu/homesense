//! Read-only logical panel projection. Written LED intents never become module
//! observations, and button feedback never becomes a color measurement.
use crate::{
    admission::{Admission, TaskGeneration},
    management_devices::DeviceView,
};
use homesense_config::Configuration;
use homesense_core::{DeviceId, Observation, PanelLedMode};
use homesense_storage::module_history::Record;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use uuid::Uuid;

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Setting {
    ReadBacklight,
    Backlight { mode: Backlight },
    ReadSound,
    Sound { enabled: bool },
    ReadWorkMode,
    WorkMode { mode: WorkMode },
}
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Backlight {
    DimAfterTenSeconds,
    Bright,
    Dim,
}
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkMode {
    Manual,
    SelfReset,
}
impl Setting {
    fn parameter(self) -> homesense_core::PanelParameter {
        use homesense_core::{PanelBacklight as B, PanelFeedbackMode as M, PanelParameter as P};
        match self {
            Self::ReadBacklight => P::ReadBacklight,
            Self::Backlight { mode } => P::Backlight(match mode {
                Backlight::DimAfterTenSeconds => B::DimAfterTenSeconds,
                Backlight::Bright => B::Bright,
                Backlight::Dim => B::Dim,
            }),
            Self::ReadSound => P::ReadSound,
            Self::Sound { enabled } => P::Sound(enabled),
            Self::ReadWorkMode => P::ReadFeedbackMode,
            Self::WorkMode { mode } => P::FeedbackMode(match mode {
                WorkMode::Manual => M::Manual,
                WorkMode::SelfReset => M::SelfReset,
            }),
        }
    }
}
pub(crate) async fn prepare(
    admission: &Admission,
    controller: Uuid,
    generation: TaskGeneration,
    id: &str,
    source: crate::web_auth::ModuleOffLease,
    setting: Setting,
) -> Result<(homesense_hap::requests::Batch,crate::panel_auth::Lease), String> {
    if !source.valid() || !source.is_administrator() || source.controller()!=controller {
        return Err("source credential no longer permits panel settings".into());
    }
    let allowed=source.devices();
    let bundle = admission
        .automation_bundle(controller, generation)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let parent = DeviceId::new(id).map_err(|e| e.to_string())?;
    let panel = bundle
        .configuration
        .keypads()
        .get(&parent)
        .filter(|p| p.enabled)
        .ok_or("keypad unavailable")?;
    // A one-key grant must never become panel-wide authority. Keep the existing
    // credential format: require explicit coverage of all six enabled keys.
    if panel.keys.len() != 6
        || panel
            .keys
            .values()
            .any(|k| !k.led_endpoint.enabled || !allowed.contains(&k.led_endpoint.id))
    {
        return Err("all six keypad endpoints must be explicitly authorized".into());
    }
    let (_, module) = bundle
        .configuration
        .module(&panel.module_id)
        .ok_or("keypad module unavailable")?;
    let parameter = setting.parameter();
    if let homesense_core::PanelParameter::FeedbackMode(mode) = parameter {
        let expected = match mode {
            homesense_core::PanelFeedbackMode::Manual => PanelLedMode::Manual,
            homesense_core::PanelFeedbackMode::SelfReset => PanelLedMode::SelfReset,
        };
        if module.led_mode() != expected {
            return Err("publish a reviewed matching LED mode and compatible indicator rules before changing hardware mode".into());
        }
    }
    let device = DeviceId::new(&panel.keys[&1].led_endpoint.id).map_err(|e| e.to_string())?;
    let command=homesense_core::Command::PanelParameter(parameter);
    let lease=crate::panel_auth::Lease::settings(source,&bundle.configuration,parent,device.clone(),command)
        .ok_or("source panel settings authorization expired")?;
    Ok((homesense_hap::requests::Batch {
        controller,
        subscriptions: vec![],
        intents: vec![homesense_hap::requests::Intent {
            device,
            command,
        }],
    },lease))
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn snapshot(
    admission: &Admission,
    controller: Uuid,
    generation: TaskGeneration,
    config: &Configuration,
    raw: &homesense_state::Snapshot,
    visible: &[DeviceView],
    transport_epoch: u64,
    now: u64,
) -> Result<(Vec<Value>, BTreeSet<DeviceId>), String> {
    let visible: BTreeSet<_> = visible.iter().map(|v| v.id.clone()).collect();
    let ids: Vec<_> = config
        .keypads()
        .values()
        .filter(|p| p.enabled)
        .flat_map(|p| p.keys.values())
        .filter_map(|key| DeviceId::new(&key.led_endpoint.id).ok())
        .filter(|id| visible.contains(id))
        .collect();
    let mut records = vec![];
    // Read bounded per-endpoint state, not a shared recent-history window.
    // Yield between small chunks so catalog reads do not monopolize the actor.
    for chunk in ids.chunks(32) {
        records.extend(
            admission
                .keypad_state_records(controller, generation.clone(), chunk.to_vec())
                .await
                .map_err(|e| format!("keypad audit: {e:?}"))?,
        );
    }
    let indications=match admission.keypad_indications(controller,generation.clone(),ids).await {
        Ok(values)=>values,Err(hap::server::WriteStatus::ResourceBusy)=>Default::default(),Err(e)=>return Err(format!("keypad indication: {e:?}")),
    };
    let diagnostics=admission.diagnostics(controller,generation).await.map_err(|e|format!("keypad gateway view: {e:?}"))?;
    let (mut panels,endpoints)=project(config,raw,&visible,&records,transport_epoch,now);
    annotate_gateway_availability(config,&mut panels,&diagnostics.gateways,diagnostics.mapping_readable())?;
    for panel in &mut panels {for key in panel["keys"].as_array_mut().into_iter().flatten(){
        key["indication"]=key["endpoint_id"].as_str().and_then(|id|indications.iter().find(|(endpoint,_)|endpoint.as_str()==id).map(|(_,value)|json!(value))).unwrap_or(Value::Null);
    }}
    Ok((panels,endpoints))
}
fn annotate_gateway_availability(config:&Configuration,panels:&mut [Value],views:&[crate::gateway_runtime::View],readable:bool)->Result<(),String> {
    for value in panels {
        let panel=config.keypads().values().find(|panel|Some(panel.id.as_str())==value["id"].as_str()).ok_or("unknown logical keypad")?;
        let gateway=&config.module(&panel.module_id).ok_or("keypad physical module unavailable")?.0.gateway;
        let view=views.iter().find(|view|&view.gateway==gateway);
        if !views.is_empty() && view.is_none(){return Err("keypad gateway scope unavailable".into());}
        value["availability"]=json!(view);
        value["action_gate"]=json!({"available":readable && view.is_none_or(|view|view.state=="ready")});
    }
    Ok(())
}
fn led_record(record: &Record, epoch: u64) -> Option<Value> {
    let color = match record.intent.as_slice() {
        [1, 6, 0, 0] => "green",
        [1, 6, 1, 0] => "red",
        [1, 10, 0, 0] => "off",
        _ => return None,
    };
    Some(
        json!({"command":record.command.to_string(),"status":record.status,
        "color":color,
        "transport_epoch":record.transport_epoch.to_string(),"control_epoch":record.control_epoch.to_string(),
        "updated_ms":record.updated_ms.to_string(),"current_transport_epoch":record.transport_epoch==epoch}),
    )
}
fn setting_value(field: &str, records: &[Record], epoch: u64) -> Value {
    let record_value = |r: &Record| -> Option<(bool, Value)> {
        let [1, 9, function, value] = r.intent.as_slice() else {
            return None;
        };
        let (name, read, value) = match (*function, *value) {
            (0xC5, 0) => ("backlight", true, Value::Null),
            (0xC6, 1..=3) => (
                "backlight",
                false,
                json!(match value {
                    1 => "dim_after_ten_seconds",
                    2 => "bright",
                    _ => "dim",
                }),
            ),
            (0xC1, 0) => ("sound", true, Value::Null),
            (0xC2, 1 | 2) => ("sound", false, json!(*value == 1)),
            (0xC7, 0) => ("work_mode", true, Value::Null),
            (0xC8, 1 | 2) => (
                "work_mode",
                false,
                json!(if *value == 2 { "manual" } else { "self_reset" }),
            ),
            _ => return None,
        };
        if name != field {
            return None;
        }
        Some((
            read,
            json!({"command":r.command.to_string(),"status":r.status,"value":value,"read":read,
            "transport_epoch":r.transport_epoch.to_string(),"control_epoch":r.control_epoch.to_string(),"current_transport_epoch":r.transport_epoch==epoch,"updated_ms":r.updated_ms.to_string()}),
        ))
    };
    let latest = records
        .iter()
        .filter_map(|r| record_value(r).map(|(_, v)| ((r.updated_ms, r.command), v)))
        .max_by_key(|(key, _)| *key)
        .map(|(_, v)| v);
    let written = records
        .iter()
        .filter(|r| r.status == "written")
        .filter_map(|r| {
            record_value(r)
                .filter(|(read, _)| !*read)
                .map(|(_, v)| ((r.updated_ms, r.command), v))
        })
        .max_by_key(|(key, _)| *key)
        .map(|(_, v)| v);
    json!({"observed":Value::Null,"quality":"Unknown","source":if written.is_some(){"last_written"}else{"unknown"},"last_request":latest,"last_written":written,"can_write":false})
}
fn project(
    config: &Configuration,
    raw: &homesense_state::Snapshot,
    visible: &BTreeSet<DeviceId>,
    records: &[Record],
    epoch: u64,
    now: u64,
) -> (Vec<Value>, BTreeSet<DeviceId>) {
    let mut endpoints = BTreeSet::new();
    let mut panels = vec![];
    for panel in config.keypads().values().filter(|p| p.enabled) {
        let visible_keys: BTreeSet<_> = panel
            .keys
            .values()
            .filter(|k| k.led_endpoint.enabled)
            .filter_map(|k| DeviceId::new(&k.led_endpoint.id).ok())
            .filter(|id| visible.contains(id))
            .collect();
        if visible_keys.is_empty() {
            continue;
        }
        endpoints.extend(visible_keys.iter().cloned());
        let mode = config.module(&panel.module_id).map(|(_, m)| m.led_mode());
        let mut fresh = false;
        let keys = (1..=6).map(|number| {
            let key = panel.keys.get(&number).filter(|k| visible_keys.iter().any(|id| id.as_str()==k.led_endpoint.id));
            let state = key.and_then(|k| DeviceId::new(&k.led_endpoint.id).ok()).and_then(|id| raw.devices.get(&id));
            let current = state.is_some_and(|s| s.quality==homesense_state::Quality::Confirmed
                && s.observed_at_ms.and_then(|at|now.checked_sub(at)).is_some_and(|age|age<5000));
            fresh |= current;
            let pressed = state.and_then(|s| match s.observed {Some(Observation::Button{pressed})=>Some(pressed),_=>None});
            let records: Vec<_> = records.iter().filter(|r| key.is_some_and(|k|r.device==k.led_endpoint.id))
                .filter(|r| led_record(r,epoch).is_some()).collect();
            let latest = records.iter().max_by_key(|r|(r.updated_ms,r.command)).and_then(|r|led_record(r,epoch));
            let written = records.iter().filter(|r|r.status=="written").max_by_key(|r|(r.updated_ms,r.command)).and_then(|r|led_record(r,epoch));
            json!({"number":number,"label":key.map(|k|k.label.clone()).unwrap_or_else(||number.to_string()),
                "configured":key.is_some(),"endpoint_id":key.map(|k|&k.led_endpoint.id),
                "can_write_led":key.is_some() && mode==Some(PanelLedMode::Manual),
                "key_event":{"pressed":pressed,"quality":if current{"Confirmed"}else if pressed.is_some(){"Stale"}else{"Unknown"},
                    "observed_at_ms":state.and_then(|s|s.observed_at_ms).map(|n|n.to_string()),"version":state.map(|s|s.version.to_string()),"source":"button_feedback"},
                "activation":key.filter(|key|key.action.is_some()).map(|_|json!({"version":1,"path":"/api/v1/keypad-actions","gestures":["activate"],"revision":config.revision().to_string(),"keypad":panel.id.as_str(),"key":number})),
                "led":{"observed":Value::Null,"quality":"Unknown","source":if written.is_some(){"last_written"}else{"unknown"},
                    "last_request":latest,"last_written":written},
                "action":key.and_then(|k|k.action.as_ref()).map(|a|match a {
                    homesense_config::keypad::KeyAction::AwayOff{..}=>json!({"kind":"away_off","path":"/api/v1/away-off"}),
                    homesense_config::keypad::KeyAction::SceneExecute{scene,..}=>json!({"kind":"scene_execute","scene":scene,"path":"/api/v1/scene-actions"}),
                    homesense_config::keypad::KeyAction::SceneToggle{scene,..}=>json!({"kind":"scene_toggle","scene":scene,"path":"/api/v1/scene-relative-actions"}),
                    homesense_config::keypad::KeyAction::Motor{device,action,..}=>json!({"kind":"motor","device":device,"command":{"action":action},"path":"/api/v1/device-commands"})
                }),
                "card_action":key.and_then(|k| match k.action.as_ref()? {
                    homesense_config::keypad::KeyAction::AwayOff{..}=>Some(json!({"kind":"away_off","command":{"action":"keypad_press","keypad":panel.id.as_str(),"key":number},"path":"/api/v1/away-off"})),
                    homesense_config::keypad::KeyAction::Motor{device, action:homesense_config::keypad::KeyMotorAction::Open | homesense_config::keypad::KeyMotorAction::Close, press_behavior:homesense_config::keypad::MotorPressBehavior::RepeatToStop,..} => Some(json!({"kind":"motor_press","device":device,"behavior":"repeat_to_stop","command":{"action":"keypad_press","keypad":panel.id.as_str(),"key":number},"path":"/api/v1/device-commands"})),
                    _=>None,
                })})
        }).collect::<Vec<_>>();
        let whole_coverage = panel.keys.len() == 6
            && panel.keys.values().all(|k| {
                k.led_endpoint.enabled
                    && visible_keys
                        .iter()
                        .any(|id| id.as_str() == k.led_endpoint.id)
            });
        let mut settings = json!({});
        let setting_records = if whole_coverage {
            records
                .iter()
                .filter(|r| r.device == panel.keys[&1].led_endpoint.id)
                .map(|r| Record {
                    command: r.command,
                    device: r.device.clone(),
                    status: r.status.clone(),
                    intent: r.intent.clone(),
                    transport_epoch: r.transport_epoch,
                    control_epoch: r.control_epoch,
                    updated_ms: r.updated_ms,
                })
                .collect::<Vec<_>>()
        } else {
            vec![]
        };
        for field in ["work_mode", "backlight", "sound"] {
            settings[field] = setting_value(field, &setting_records, epoch);
        }
        settings["work_mode"]["configured"] = json!(match mode {
            Some(PanelLedMode::Manual) => "manual",
            Some(PanelLedMode::SelfReset) => "self_reset",
            _ => "unknown",
        });
        settings["scope_complete"] = json!(whole_coverage);
        panels.push(json!({"id":panel.id.as_str(),"kind":"Keypad","name":panel.name,"module_id":panel.module_id,
            "room":panel.room.as_ref().and_then(|id|config.rooms().get(id)),"quality":if fresh{"Confirmed"}else{"Unknown"},"can_control":false,
            "keys":keys,"capabilities":{"key_events":true,"led_colors":["off","red","green"],"led_readback":false,
                "backlight_write":false,"work_mode_write":false,"sound_write":false},
            "settings":settings}));
    }
    (panels, endpoints)
}

/// Restrict a server-owned full current view before applying the live grant.
/// Hidden keys use exactly the legacy partial-panel placeholders; panel-wide
/// audit/settings records require complete six-key coverage.
pub(crate) fn scope_current(panels:&mut Vec<Value>,visible:&BTreeSet<DeviceId>,now:u64) {
    panels.retain_mut(|panel| {
        let Some(keys)=panel["keys"].as_array_mut() else{return false;};
        let mut count=0;let mut fresh=false;let mut complete=keys.len()==6;
        for key in keys {
            let allowed=key["endpoint_id"].as_str().and_then(|id|DeviceId::new(id).ok()).is_some_and(|id|visible.contains(&id));
            if allowed {
                count+=1;
                if key["key_event"]["quality"]=="Confirmed" && key["key_event"]["observed_at_ms"].as_str().and_then(|at|at.parse::<u64>().ok()).and_then(|at|now.checked_sub(at)).is_none_or(|age|age>=5000) {
                    key["key_event"]["quality"]=json!("Stale");
                }
                fresh|=key["key_event"]["quality"]=="Confirmed";continue;
            }
            complete=false;
            let number=key["number"].as_u64().unwrap_or(0);
            *key=json!({"number":number,"label":number.to_string(),"configured":false,"endpoint_id":null,"can_write_led":false,
                "key_event":{"pressed":null,"quality":"Unknown","observed_at_ms":null,"version":null,"source":"button_feedback"},
                "activation":null,"led":{"observed":null,"quality":"Unknown","source":"unknown","last_request":null,"last_written":null},
                "action":null,"card_action":null,"indication":null});
        }
        if count==0{return false;}
        panel["quality"]=json!(if fresh{"Confirmed"}else{"Unknown"});
        complete &= panel["settings"]["scope_complete"]==true;
        if !complete {
            let configured=panel["settings"]["work_mode"]["configured"].clone();
            for field in ["work_mode","backlight","sound"] {panel["settings"][field]=setting_value(field,&[],0);}
            panel["settings"]["work_mode"]["configured"]=configured;
            panel["settings"]["scope_complete"]=json!(false);
        }
        true
    });
}

pub(crate) fn devices_json(
    snapshot: &crate::management_devices::DeviceSnapshot,
    grant: &crate::web_auth::Grant,
) -> Vec<Value> {
    let mut devices: Vec<_> = snapshot
        .devices
        .iter()
        .filter(|d| !snapshot.keypad_endpoints.contains(&d.id))
        .map(|d| crate::web_api::device_json(d, grant))
        .collect();
    devices.extend(snapshot.keypads.iter().cloned().map(|mut panel| {
        let parent_ready=panel["action_gate"]["available"]==true;
        for key in panel["keys"].as_array_mut().into_iter().flatten() {
            let allowed = key["endpoint_id"]
                .as_str()
                .is_some_and(|id| grant.may_control_device(id));
            key["can_write_led"] = json!(allowed && key["can_write_led"] == true);
            let is_away=key["action"]["kind"]=="away_off";
            let is_execute=key["action"]["kind"]=="scene_execute";
            let target_allowed = match key["action"]["kind"].as_str() {
                Some("away_off")=>snapshot.keypad_away_scope.as_ref().is_some_and(|scope|scope.permitted(grant)),
                Some("scene_execute")=>key["action"]["scene"].as_str().is_some_and(|scene|grant.may_control(scene) && snapshot.keypad_execute_scopes.get(scene).is_some_and(|ids|ids.iter().all(|id|grant.may_control_device(id)))),
                Some("scene_toggle") => key["action"]["scene"]
                    .as_str()
                    .is_some_and(|scene| grant.may_control(scene)),
                Some("motor") => key["action"]["device"]
                    .as_str()
                    .is_some_and(|id| grant.may_control_device(id)),
                _ => false,
            };
            let available=allowed && target_allowed && parent_ready;
            if is_away || is_execute {
                key["can_activate"]=json!(available);
                key["action_gate"]=json!({"available":available});
            }
            if !allowed || !target_allowed || is_away && !available {
                key["action"] = Value::Null;
                key["card_action"] = Value::Null;
                key["activation"] = Value::Null;
            }
            let activation_available=available && key["action"]["scene"].as_str().is_none_or(|scene|snapshot.keypad_execute_scopes.get(scene).is_some_and(|ids|ids.iter().all(|id|grant.may_control_device(id))));
            if let Some(activation)=key["activation"].as_object_mut(){activation.insert("available".into(),json!(activation_available));}
        }
        let whole_control = grant.role == crate::web_auth::Role::Administrator
            && panel["settings"]["scope_complete"] == true
            && panel["keys"].as_array().is_some_and(|keys| {
                keys.iter().all(|k| {
                    k["endpoint_id"]
                        .as_str()
                        .is_some_and(|id| grant.may_control_device(id))
                })
            });
        for field in ["work_mode", "backlight", "sound"] {
            panel["settings"][field]["can_write"] = json!(whole_control);
        }
        for field in ["work_mode_write", "backlight_write", "sound_write"] {
            panel["capabilities"][field] = json!(whole_control);
        }
        panel
    }));
    devices
}

#[cfg(test)]
mod tests {
    use super::*;
    use homesense_core::{ExecutionContext, ModuleKind};
    fn config() -> Configuration {
        Configuration::parse(br#"{"schema_version":2,"revision":1,"gateways":[{"id":"bus","endpoint":"127.0.0.1:50007","modules":[{"id":"module","address":"41","profile":"ts6_86_rs485_v2_6_5_6key","verification":{"status":"simulated"},"led_mode":"manual"}]}],"devices":[],"keypads":[{"id":"panel","name":"Panel","module_id":"module","enabled":true,"keys":[{"number":1,"label":"One","led_endpoint":{"id":"old:1","name":"LED one","enabled":true}},{"number":2,"label":"Two","led_endpoint":{"id":"old:2","name":"LED two","enabled":true}}]}]}"#,ExecutionContext::Simulation).unwrap()
    }
    fn record(command: u64, status: &str, on: bool, epoch: u64) -> Record {
        Record {
            command,
            device: "old:1".into(),
            status: status.into(),
            intent: vec![1, 6, u8::from(on), 0],
            transport_epoch: epoch,
            control_epoch: 1,
            updated_ms: command,
        }
    }
    #[test]
    fn child_scope_is_not_expanded_and_key_events_are_not_led_readback() {
        let config = config();
        let id = DeviceId::new("old:1").unwrap();
        let mut store = homesense_state::StateStore::new(1, 1, 8, 16).unwrap();
        store.register(id.clone(), ModuleKind::Keypad).unwrap();
        store
            .observe(&id, Observation::Button { pressed: true }, 1, 100)
            .unwrap();
        let (panels, aliases) = project(
            &config,
            &store.snapshot(),
            &[id.clone()].into(),
            &[],
            1,
            101,
        );
        assert_eq!(aliases, [id].into());
        assert_eq!(panels.len(), 1);
        assert_eq!(panels[0]["keys"].as_array().unwrap().len(), 6);
        for (i, key) in panels[0]["keys"].as_array().unwrap().iter().enumerate() {
            assert_eq!(key["number"], i + 1);
            assert!(key["led"]["observed"].is_null());
            assert_eq!(key["led"]["quality"], "Unknown");
            if i > 0 {
                assert_eq!(key["configured"], false);
                assert!(key["endpoint_id"].is_null());
            }
        }
        assert_eq!(panels[0]["keys"][0]["key_event"]["pressed"], true);
        assert_eq!(panels[0]["keys"][0]["led"]["source"], "unknown");
    }
    #[test]
    fn written_intent_is_distinct_from_accepted_uncertain_and_transport_freshness() {
        let config = config();
        let id = DeviceId::new("old:1").unwrap();
        let mut store = homesense_state::StateStore::new(1, 1, 8, 16).unwrap();
        store.register(id.clone(), ModuleKind::Keypad).unwrap();
        store
            .observe(&id, Observation::Button { pressed: false }, 1, 100)
            .unwrap();
        let records = [
            record(1, "written", false, 1),
            record(2, "accepted", true, 1),
            record(3, "uncertain", true, 1),
        ];
        let (panels, _) = project(
            &config,
            &store.snapshot(),
            &[id.clone()].into(),
            &records,
            2,
            10000,
        );
        let key = &panels[0]["keys"][0];
        assert_eq!(key["key_event"]["quality"], "Stale");
        assert_eq!(key["led"]["last_request"]["status"], "uncertain");
        assert_eq!(key["led"]["last_written"]["color"], "green");
        assert_eq!(key["led"]["last_written"]["current_transport_epoch"], false);
        assert!(key["led"]["observed"].is_null());
        let (restarted, _) = project(&config, &store.snapshot(), &[id].into(), &[], 2, 10000);
        assert!(restarted[0]["keys"][0]["led"]["last_written"].is_null());
    }
}

#[cfg(test)]
mod gateway_availability_tests {
    use super::*;
    #[test]
    fn parent_gate_uses_physical_module_without_changing_masked_key_permissions() {
        let bytes=br#"{"schema_version":2,"revision":1,"gateways":[{"id":"real-bus","endpoint":"127.0.0.1:50007","modules":[{"id":"registered-module","address":"41","profile":"ts6_86_rs485_v2_6_5_6key","verification":{"status":"simulated"},"led_mode":"manual"}]}],"devices":[],"keypads":[{"id":"unrelated-parent-name","name":"Synthetic panel","module_id":"registered-module","enabled":true,"keys":[{"number":1,"label":"One","led_endpoint":{"id":"legacy:1","name":"One","enabled":true}},{"number":2,"label":"Two","led_endpoint":{"id":"legacy:2","name":"Two","enabled":true}}]}]}"#;
        let config=Configuration::parse(bytes,homesense_core::ExecutionContext::Simulation).unwrap();
        let mut panels=vec![json!({"id":"unrelated-parent-name","keys":[{"number":1,"configured":true,"can_write_led":true},{"number":2,"configured":false,"can_write_led":false}],"settings":{"scope_complete":false,"backlight":{"can_write":false}}})];
        let before=panels[0].clone();
        for (epoch,state,available) in [(1,"offline",false),(2,"synchronizing",false),(2,"ready",true)] {
            let view=crate::gateway_runtime::View {gateway:"real-bus".into(),connection_epoch:epoch.to_string(),state};
            annotate_gateway_availability(&config,&mut panels,&[view],true).unwrap();
            assert_eq!(panels[0]["availability"]["gateway"],"real-bus");assert_eq!(panels[0]["availability"]["connection_epoch"],epoch.to_string());assert_eq!(panels[0]["action_gate"]["available"],available);
            assert_eq!(panels[0]["keys"],before["keys"]);assert_eq!(panels[0]["settings"],before["settings"]);
        }
        annotate_gateway_availability(&config,&mut panels,&[],false).unwrap();assert_eq!(panels[0]["action_gate"]["available"],false);
    }
}
