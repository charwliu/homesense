//! Authenticated virtual gestures resolve only through the published key map.
//! This adapter never emits hardware button frames or enables wall event rules.
use crate::{admission::{Admission, TaskGeneration}, automation_control::{Action, Reply}, source_auth::Source};
use homesense_config::{Configuration, keypad::{KeyAction, KeyMotorAction, MotorPressBehavior}, rule_document::Document};
use homesense_core::{DeviceId, PanelLedMode, ProtocolProfile};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;
#[cfg(test)]
#[path="keypad_actions_tests.rs"]
mod tests;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Gesture { Activate }

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub revision: String,
    pub keypad: String,
    pub key: u8,
    pub request_id: String,
    pub gesture: Gesture,
}
impl Request {
    pub(crate) fn validate(&self) -> Result<u64, Rejection> {
        let revision = self.revision.parse::<u64>().map_err(|_| Rejection::InvalidBody)?;
        if revision == 0 || self.revision != revision.to_string() || !(1..=6).contains(&self.key)
            || DeviceId::new(&self.keypad).is_err() || self.keypad.len() > 250 || self.keypad.chars().any(char::is_control)
            || self.request_id.is_empty() || self.request_id.len() > 128
            || !self.request_id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
            return Err(Rejection::InvalidBody);
        }
        Ok(revision)
    }
}

pub(crate) fn fingerprint(request:&Request,lease:&crate::web_auth::ModuleOffLease)->String {
    serde_json::to_string(&json!({"request":request,"scope":lease.receipt_scope()})).expect("bounded typed keypad request")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Rejection { InvalidBody, StaleRevision, KeypadUnavailable, KeypadActionUnavailable, KeypadScopeDenied, RuntimeUnavailable }
impl Rejection {
    pub(crate) fn status(self) -> hyper::StatusCode {
        use hyper::StatusCode as S;
        match self { Self::InvalidBody => S::BAD_REQUEST, Self::StaleRevision => S::CONFLICT,
            Self::KeypadScopeDenied => S::FORBIDDEN, Self::RuntimeUnavailable => S::SERVICE_UNAVAILABLE,
            Self::KeypadUnavailable | Self::KeypadActionUnavailable => S::UNPROCESSABLE_ENTITY }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Indication {
    pub request_id: String,
    pub operation_sequence: String,
    pub event_sequence: Option<String>,
    pub endpoint_id: String,
    pub source: &'static str,
    pub status: &'static str,
    pub color: Option<&'static str>,
    pub observed: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Receipt {
    pub request_id: String,
    pub outcome: &'static str,
    pub boot: String,
    pub revision: String,
    pub keypad: String,
    pub key: u8,
    pub gesture: Gesture,
    pub retry: bool,
    pub execution: Value,
    pub indication: Indication,
}
pub(crate) struct Prepared { pub action: Action, pub source: Option<Source>, pub context: Context }
pub(crate) struct Context { request: Request, boot: String, kind: &'static str,source:Source }
pub(crate) fn indication(source:&Source,status:Option<homesense_storage::journal::Status>)->Option<Indication> {
    let origin=source.keypad()?;
    let cue=origin.cue();
    let current=source.valid()&&origin.current();
    let status=if !current {"superseded"} else if origin.cue_retired(){"retired"} else {match (cue,status) {
        (Some(_),Some(homesense_storage::journal::Status::Written|homesense_storage::journal::Status::Observed))=>"written",
        (Some(_),Some(homesense_storage::journal::Status::Cancelled))=>"cancelled",
        (Some(_),Some(homesense_storage::journal::Status::Failed|homesense_storage::journal::Status::Uncertain))=>"unknown",
        (Some(_),_)=>"pending",
        (None,_) if !origin.coverage=>"mapping_gap",
        _=>"unknown",
    }};
    Some(Indication{request_id:origin.request_id.clone(),operation_sequence:origin.sequence.to_string(),event_sequence:source.event().map(|event|event.cursor().sequence.to_string()),
        endpoint_id:origin.endpoint.as_str().into(),source:if source.event().is_some(){"button_event"}else{"authenticated_click"},status,
        color:(current&&!origin.cue_retired()&&status!="cancelled").then_some(cue).flatten().and_then(|(_,command)|match command {
            homesense_core::Command::PanelLed(true)=>Some("red"),homesense_core::Command::PanelLed(false)=>Some("green"),
            homesense_core::Command::PanelLedOff=>Some("off"),_=>None,
        }),observed:false})
}
impl Context {
    pub(crate) fn finish(self, reply: Reply) -> Result<Receipt, String> {
        let execution = match reply {
            Reply::CommandsAccepted(ids) => json!({"kind":self.kind,"commands":ids,"runs":[],"relative_status":null}),
            Reply::ImmediateAccepted(receipt) => json!({"kind":self.kind,"commands":receipt.commands,"runs":receipt.followup_runs,"relative_status":null,"receipt":receipt}),
            Reply::SceneAccepted(run) => json!({"kind":self.kind,"commands":[],"runs":run.into_iter().collect::<Vec<_>>(),"relative_status":null}),
            Reply::ScenesAccepted(runs) => json!({"kind":self.kind,"commands":[],"runs":runs,"relative_status":null}),
            Reply::SceneRelative(receipt) => json!({"kind":self.kind,"commands":[],"runs":[],"relative_status":receipt.status,
                "relative":{"request_id":receipt.key,"scene":receipt.scene,"selected":receipt.selected,"boot":receipt.boot}}),
            _ => return Err("keypad execution receipt unavailable".into()),
        };
        Ok(Receipt {
            indication: indication(&self.source,None).ok_or("keypad source indication missing")?,
            request_id:self.request.request_id,outcome:"accepted",boot:self.boot,revision:self.request.revision,
            keypad:self.request.keypad,key:self.request.key,gesture:self.request.gesture,retry:false,execution,
        })
    }
}

/// The complete reviewed expansion, including mutually exclusive off branches,
/// is authorized before resolving a keypad into a scene operation.
fn scene_scope(config:&Configuration,document:&Document,source:&Source,scene:&str)->Result<(),Rejection> {
    if !source.permits_scene(scene) { return Err(Rejection::KeypadScopeDenied); }
    let ids=scene_devices(config,document,scene)?;
    if ids.iter().any(|id|!source.permits_device(id.as_str())) {return Err(Rejection::KeypadScopeDenied);}
    Ok(())
}
pub(crate) fn scene_devices(config:&Configuration,document:&Document,scene:&str)->Result<std::collections::BTreeSet<DeviceId>,Rejection> {
    let mut names = std::collections::BTreeSet::from([scene.to_owned()]);
    if let Some(group)=document.scene_switch_groups.iter().find(|g|g.members.iter().any(|m|m.scene==scene)) {
        for member in &group.members { names.insert(member.scene.clone()); names.insert(member.off_scene.clone()); }
    }
    let mut all=std::collections::BTreeSet::new();
    for name in names {
        let ids=homesense_config::keypad::scene_execute_devices(document,config,&name).map_err(|_|Rejection::KeypadActionUnavailable)?;
        all.extend(ids);
    }
    Ok(all)
}
/// Shared scene groups have the same full-branch authority union. Reuse it
/// within one immutable bundle snapshot, never across publications or readers.
pub(crate) fn execution_scopes(config:&Configuration,document:&Document)->Result<std::collections::BTreeMap<String,std::collections::BTreeSet<String>>,Rejection> {
    use std::collections::{BTreeMap,BTreeSet};
    let scenes=config.keypads().values().flat_map(|panel|panel.keys.values()).filter_map(|key|match &key.action {
        Some(KeyAction::SceneExecute{scene,..}|KeyAction::SceneToggle{scene,..})=>Some(scene.clone()),_=>None,
    }).collect::<BTreeSet<_>>();
    let mut groups=BTreeMap::<String,BTreeSet<String>>::new();let mut result=BTreeMap::new();
    for scene in scenes {
        let group=document.scene_switch_groups.iter().find(|group|group.members.iter().any(|member|member.scene==scene));
        let scope=if let Some(cached)=group.and_then(|group|groups.get(&group.id)) {cached.clone()} else {
            let scope=scene_devices(config,document,&scene)?.into_iter().map(|id|id.as_str().to_owned()).collect::<BTreeSet<_>>();
            if let Some(group)=group {groups.insert(group.id.clone(),scope.clone());}scope
        };
        result.insert(scene,scope);
    }
    Ok(result)
}
fn led_coverage(document:&Document,scene:&str,endpoint:&DeviceId)->bool {
    fn walk(nodes:&[homesense_core::rule::Node],document:&Document,endpoint:&DeviceId)->bool {
        use homesense_core::rule::Action;
        nodes.iter().any(|node|match &node.action {
            Action::Command{device,command}=>device==endpoint && matches!(command,homesense_core::Command::PanelLed(_)|homesense_core::Command::PanelLedOff),
            Action::If{then_actions,else_actions,..}=>walk(then_actions,document,endpoint)||walk(else_actions,document,endpoint),
            Action::Scene{id,..}=>document.timed_scenes.get(id).is_some_and(|scene|scene.steps.iter().any(|step|&step.device==endpoint&&matches!(step.command,homesense_core::Command::PanelLed(_)|homesense_core::Command::PanelLedOff))),
            _=>false,
        })
    }
    document.linked_manual_scenes(&[scene.into()]).is_ok_and(|nodes|walk(&nodes,document,endpoint))
}

pub(crate) async fn prepare(admission:&Admission,generation:&TaskGeneration,document:&Document,controller:Uuid,
    request:&Request,source:&Source)->Result<Prepared,Rejection> {
    let revision=request.validate()?;
    if revision!=document.revision {return Err(Rejection::StaleRevision);}
    let original=Action::KeypadAction {controller,request:request.clone()};
    if !source.matches(&original) {return Err(Rejection::KeypadScopeDenied);}
    let bundle=admission.automation_bundle(controller,generation.clone()).await.map_err(|_|Rejection::RuntimeUnavailable)?;
    let config=&bundle.configuration;
    if config.revision()!=revision {return Err(Rejection::StaleRevision);}
    let parent=DeviceId::new(&request.keypad).map_err(|_|Rejection::InvalidBody)?;
    let panel=config.keypads().get(&parent).filter(|p|p.enabled).ok_or(Rejection::KeypadUnavailable)?;
    let key=panel.keys.get(&request.key).filter(|k|k.led_endpoint.enabled).ok_or(Rejection::KeypadUnavailable)?;
    let endpoint=DeviceId::new(&key.led_endpoint.id).map_err(|_|Rejection::KeypadUnavailable)?;
    if let Some(event)=source.event() {
        if event.device()!=&endpoint || event.generation()!=generation {return Err(Rejection::KeypadScopeDenied);}
        admission.validate_keypad_event(controller,generation.clone(),event.clone()).await.map_err(|_|Rejection::KeypadScopeDenied)?;
    }
    let (_,module)=config.binding(&endpoint).ok_or(Rejection::KeypadUnavailable)?;
    if module.profile()!=ProtocolProfile::Ts686Rs485SixKey || module.led_mode()!=PanelLedMode::Manual {
        return Err(Rejection::KeypadActionUnavailable);
    }
    if !source.permits_device(endpoint.as_str()) {return Err(Rejection::KeypadScopeDenied);}
    let mapping=key.action.as_ref().ok_or(Rejection::KeypadActionUnavailable)?;
    let mut coverage=key.local_indicator.is_some() || match mapping {KeyAction::SceneExecute{scene,..}|KeyAction::SceneToggle{scene,..}=>led_coverage(document,scene,&endpoint),_=>false};
    let mut fallback=None;
    let mut selection=vec![];
    let mut motor_target=None;
    let (action,kind)=match mapping {
        KeyAction::SceneExecute{scene,..} => {
            scene_scope(config,document,source,scene)?;
            (Action::SceneAction{controller,revision,scene:scene.clone(),execute:true},"scene")
        },
        KeyAction::SceneToggle{scene,..} => {
            scene_scope(config,document,source,scene)?;
            let snapshot=admission.scene_selection_snapshot(controller,generation.clone(),crate::scene_switches::variable_names(document)).await.map_err(|_|Rejection::RuntimeUnavailable)?;
            let states=crate::scene_switches::details(document,&snapshot).map_err(|_|Rejection::KeypadActionUnavailable)?;
            let selected_group=document.scene_switch_groups.iter().find(|candidate|candidate.members.iter().any(|member|&member.scene==scene)).ok_or(Rejection::KeypadActionUnavailable)?;
            let selected=states.get(scene).and_then(|state|state.selected);
            let action=if selected.is_none() && selected_group.reconciliation.as_ref().is_some_and(|policy|policy.enabled) {
                // This is an accepted gesture cue, not a guessed final mode.
                // Native planning still decides each light from fresh truth.
                if let Some(homesense_config::keypad::LocalIndicator::SceneSelection{group,selected_color,..})=&key.local_indicator {
                    if &selected_group.id!=group{return Err(Rejection::KeypadActionUnavailable);}
                    fallback=Some(homesense_core::Command::PanelLed(selected_color.red()));
                }
                Action::SceneRelative{controller,revision,scene:scene.clone(),request_id:request.request_id.clone()}
            } else {
                // Recorded/ambiguous modes have no relative light recipe. On a
                // fresh unknown selection, execute the reviewed On branch;
                // only its successful execution may establish local selection.
                // Keep the read fence even for unknown so a competing action
                // cannot silently change this decision before queue admission.
                let on=selected.unwrap_or(false);
                selection.push(homesense_storage::variables::ReadCheck{revision,name:selected_group.variable.clone(),expected_version:snapshot[&selected_group.variable].stored.version});
                if let Some(homesense_config::keypad::LocalIndicator::SceneSelection{group,selected_color,unselected_color,..})=&key.local_indicator {
                    if &selected_group.id!=group{return Err(Rejection::KeypadActionUnavailable);}
                    fallback=Some(homesense_core::Command::PanelLed(if on{unselected_color.red()}else{selected_color.red()}));
                }
                Action::SceneAction{controller,revision,scene:scene.clone(),execute:!on}
            };
            (action,"scene")
        },
        KeyAction::Motor{device,action,press_behavior,..} => {
            let device=DeviceId::new(device).map_err(|_|Rejection::KeypadActionUnavailable)?;
            if !source.permits_device(device.as_str()) {return Err(Rejection::KeypadScopeDenied);}
            if let Some(model)=config.devices().get(&device).and_then(|device|device.curtain_state.as_ref()) {
                let leds=match action {KeyMotorAction::Open=>&model.leds.open,KeyMotorAction::Close=>&model.leds.close,KeyMotorAction::Stop=>&model.leds.stop};
                if leds.contains(&endpoint) {coverage=true;motor_target=Some(crate::keypad_origin::MotorIndicator{device:device.clone(),stop_key:*action==KeyMotorAction::Stop});}
            }
            let command=if *press_behavior==MotorPressBehavior::RepeatToStop && *action!=KeyMotorAction::Stop {
                crate::management_devices::DeviceAction::KeypadPress {keypad:request.keypad.clone(),key:request.key}
            } else { match action {KeyMotorAction::Open=>crate::management_devices::DeviceAction::Open,
                KeyMotorAction::Close=>crate::management_devices::DeviceAction::Close,KeyMotorAction::Stop=>crate::management_devices::DeviceAction::Stop} };
            (Action::DeviceCommand{controller,revision,device,action:command},"motor")
        },
        KeyAction::AwayOff{indicator_reset,..} => {
            coverage=indicator_reset.is_some();
            let lease=source.away_keypad(request.keypad.clone(),request.key).ok_or(Rejection::KeypadScopeDenied)?;
            admission.submit_away_off(revision,lease.clone(),false).await.map_err(|_|Rejection::KeypadScopeDenied)?;
            (Action::AwayOff{controller,revision,lease,submit:true},"away")
        },
    };
    let origin=crate::keypad_origin::Origin::new(endpoint,revision,request.request_id.clone(),coverage,admission.monotonic_ms(),fallback,selection,motor_target).ok_or(Rejection::RuntimeUnavailable)?;
    let derived=source.derive(&action).ok_or(Rejection::KeypadScopeDenied)?.with_keypad(origin);
    if !source.valid() {return Err(Rejection::KeypadScopeDenied);}
    Ok(Prepared {action,source:Some(derived.clone()),context:Context{request:request.clone(),boot:admission.boot_id().into(),kind,source:derived}})
}
