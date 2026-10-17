//! Logical panel objects. Nested endpoints preserve legacy rule, scope and HAP
//! identities; physical truth remains with the stable module/channel owner.
use super::*;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LedEndpoint {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub enabled: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum KeyAction {
    Motor {
        device: String,
        action: KeyMotorAction,
        #[serde(default, skip_serializing_if = "MotorPressBehavior::is_absolute")]
        press_behavior: MotorPressBehavior,
        review_evidence: String,
    },
    AwayOff {
        review_evidence: String,
        #[serde(default,skip_serializing_if="Option::is_none")]
        indicator_reset: Option<AwayIndicatorReset>,
    },
    SceneExecute {
        scene: String,
        review_evidence: String,
    },
    SceneToggle {
        scene: String,
        review_evidence: String,
    },
}
#[derive(Debug,Clone,Copy,PartialEq,Eq,serde::Serialize,Deserialize)]
#[serde(rename_all="snake_case")]
pub enum AwayIndicatorReset {Green}
/// Explicit origin-key cue for a reviewed logical selection. This never adds
/// LED commands to a room scene or supplies physical observation authority.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, Deserialize)]
#[serde(tag="kind",rename_all="snake_case",deny_unknown_fields)]
pub enum LocalIndicator {
    SceneSelection {group:String,value:i64,selected_color:IndicatorColor,unselected_color:IndicatorColor,review_evidence:String},
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, Deserialize)]
#[serde(rename_all="snake_case")]
pub enum IndicatorColor {Red,Green}
impl IndicatorColor {pub fn red(self)->bool {self==Self::Red}}
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, Deserialize)]
#[serde(rename_all="snake_case")]
pub enum EventOwner {ObserveOnly,#[serde(rename="homesense")]HomeSense}
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventControl {
    pub owner:EventOwner,
    #[serde(default)]
    pub legacy_writer_isolated:bool,
    pub review_evidence:Option<String>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyMotorAction {
    Open,
    Stop,
    Close,
}
/// Native card interpretation only. Absolute directions remain available through
/// the original action descriptor, HAP and the physical keypad event rules.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MotorPressBehavior {
    #[default]
    Absolute,
    RepeatToStop,
}
impl MotorPressBehavior {
    fn is_absolute(&self) -> bool { *self == Self::Absolute }
}
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Key {
    pub number: u8,
    pub label: String,
    pub led_endpoint: LedEndpoint,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<KeyAction>,
    #[serde(default,skip_serializing_if="Option::is_none")]
    pub local_indicator: Option<LocalIndicator>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct KeypadInput {
    pub id: String,
    pub name: String,
    pub module_id: String,
    #[serde(default)]
    pub room: Option<String>,
    #[serde(default)]
    pub enabled: bool,
    pub keys: Vec<Key>,
    #[serde(default)]
    pub event_control:Option<EventControl>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Keypad {
    pub id: DeviceId,
    pub name: String,
    pub module_id: String,
    pub room: Option<String>,
    pub enabled: bool,
    pub keys: BTreeMap<u8, Key>,
    pub event_control:Option<EventControl>,
}
impl Keypad {
    pub fn executes_events(&self)->bool {self.enabled && self.event_control.as_ref().is_some_and(|control|control.owner==EventOwner::HomeSense)}
}
fn label(value: &str) -> Result<(), Error> {
    if value.trim().is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
        Err(invalid("invalid keypad display label"))
    } else {
        Ok(())
    }
}
pub(super) fn expand(
    schema_version: u32,
    inputs: Vec<KeypadInput>,
    devices: &mut Vec<DeviceInput>,
    registry: &BTreeMap<String, ModuleLocation>,
    gateways: &BTreeMap<String, Gateway>,
    rooms: &BTreeMap<String, Room>,
) -> Result<BTreeMap<DeviceId, Keypad>, Error> {
    if inputs.len() > 256 || (!inputs.is_empty() && schema_version != 2) {
        return Err(invalid("keypads require bounded schema 2 stable modules"));
    }
    let mut panels = BTreeMap::new();
    let mut modules = BTreeSet::new();
    let mut identities: BTreeSet<String> = devices.iter().map(|d| d.id.clone()).collect();
    for panel in inputs {
        identifier(&panel.id)?;
        identifier(&panel.module_id)?;
        label(&panel.name)?;
        if !identities.insert(panel.id.clone()) || !modules.insert(panel.module_id.clone()) {
            return Err(invalid("duplicate keypad identity or module"));
        }
        let location = registry
            .get(&panel.module_id)
            .ok_or_else(|| invalid("unknown keypad module"))?;
        let module = &gateways[&location.gateway].modules[&location.address];
        if module.profile() != ProtocolProfile::Ts686Rs485SixKey {
            return Err(invalid(
                "keypad parent requires verified six-key TS6 capability",
            ));
        }
        if panel
            .room
            .as_ref()
            .is_some_and(|id| !rooms.contains_key(id))
            || panel.keys.is_empty()
            || panel.keys.len() > 6
        {
            return Err(invalid("invalid keypad room or key count"));
        }
        // A module is represented by one object. Legacy standalone channels on
        // that module must be moved into this object, never exposed twice.
        if devices
            .iter()
            .any(|d| d.module_id.as_ref() == Some(&panel.module_id))
        {
            return Err(invalid("keypad module mixes top-level and child endpoints"));
        }
        let mut keys = BTreeMap::new();
        for key in panel.keys {
            if !(1..=6).contains(&key.number) || keys.contains_key(&key.number) {
                return Err(invalid("duplicate or invalid keypad key number"));
            }
            label(&key.label)?;
            label(&key.led_endpoint.name)?;
            identifier(&key.led_endpoint.id)?;
            if !identities.insert(key.led_endpoint.id.clone()) {
                return Err(invalid("duplicate keypad child identity"));
            }
            if let Some(action) = &key.action {
                if matches!(action, KeyAction::Motor { action: KeyMotorAction::Stop, press_behavior: MotorPressBehavior::RepeatToStop, .. }) {
                    return Err(invalid("repeat-to-stop requires an open or close key"));
                }
                let (target, evidence) = match action {
                    KeyAction::SceneToggle { scene, review_evidence } | KeyAction::SceneExecute { scene, review_evidence } => (Some(scene), review_evidence),
                    KeyAction::Motor {
                        device,
                        review_evidence,
                        ..
                    } => (Some(device), review_evidence),
                    KeyAction::AwayOff{review_evidence,..}=>(None,review_evidence),
                };
                if let Some(target)=target {identifier(target)?;}
                if evidence.trim().is_empty() || evidence.len() > 4096 {
                    return Err(invalid("key action requires explicit bounded review"));
                }
            }
            if let Some(LocalIndicator::SceneSelection{group,value,selected_color,unselected_color,review_evidence})=&key.local_indicator {
                identifier(group)?;
                if *value<=0 || selected_color==unselected_color || review_evidence.trim().is_empty() || review_evidence.len()>8192
                    || !matches!(key.action,Some(KeyAction::SceneToggle{..})) || module.led_mode()!=PanelLedMode::Manual {
                    return Err(invalid("local indicator requires a reviewed distinct-color scene selection"));
                }
            }
            devices.push(DeviceInput {
                id: key.led_endpoint.id.clone(),
                name: key.led_endpoint.name.clone(),
                room: panel.room.clone(),
                module_id: Some(panel.module_id.clone()),
                gateway: None,
                address: None,
                channel: key.number,
                enabled: panel.enabled && key.led_endpoint.enabled,
                catalog_visible: true,
                default_on_level: None,
                motor_calibration: None,
                curtain_state: None,
                supply_relay: None,
                supply: None,
            });
            keys.insert(key.number, key);
        }
        let id = DeviceId::new(panel.id)?;
        if let Some(control)=&panel.event_control
            && control.owner==EventOwner::HomeSense && (!control.legacy_writer_isolated
                || control.review_evidence.as_ref().is_none_or(|e|e.trim().is_empty()||e.len()>8192)
                || module.led_mode()!=PanelLedMode::Manual || keys.len()!=6
                || keys.values().any(|key|!key.led_endpoint.enabled||key.action.is_none())) {
                return Err(invalid("event execution requires explicit legacy-writer isolation declaration, review and six mapped manual-mode keys"));
        }
        panels.insert(
            id.clone(),
            Keypad {
                id,
                name: panel.name,
                module_id: panel.module_id,
                room: panel.room,
                enabled: panel.enabled,
                keys,
                event_control:panel.event_control,
            },
        );
    }
    if devices.len() > 4096 {
        return Err(invalid("expanded endpoint limit exceeded"));
    }
    Ok(panels)
}
/// Entire reviewed manual AST authority, including both conditional branches,
/// predicate reads, nested expanded scenes and explicitly linked timelines.
pub fn scene_execute_devices(document:&crate::rule_document::Document,config:&Configuration,scene:&str)->Result<BTreeSet<DeviceId>,Error> {
    use homesense_core::rule::{Action,Condition,Expr,Node};
    fn expr(e:&Expr,ids:&mut BTreeSet<DeviceId>) {match e {Expr::DeviceOn(id)|Expr::DeviceLevel(id)=>{ids.insert(id.clone());},Expr::Add(a,b)|Expr::Subtract(a,b)=>{expr(a,ids);expr(b,ids);},_=>{}}}
    fn condition(c:&Condition,ids:&mut BTreeSet<DeviceId>) {match c {Condition::Equal(a,b)|Condition::Less(a,b)=>{expr(a,ids);expr(b,ids);},Condition::Not(c)=>condition(c,ids),Condition::All(cs)|Condition::Any(cs)=>for c in cs{condition(c,ids)},Condition::Unknown(e)=>expr(e,ids),Condition::MotorState(p)=>{ids.insert(p.device.clone());}}}
    fn walk(nodes:&[Node],doc:&crate::rule_document::Document,ids:&mut BTreeSet<DeviceId>,remaining:&mut usize,depth:usize)->Result<(),Error>{
        if depth>64{return Err(invalid("manual authority depth exceeded"));}
        for node in nodes {if *remaining==0{return Err(invalid("manual authority budget exceeded"));}*remaining-=1;
            match &node.action {
                Action::Command{device,..}|Action::Motor{device,..}=>{ids.insert(device.clone());},
                Action::Set{value,..}=>expr(value,ids),
                Action::If{condition:c,then_actions,else_actions}=>{condition(c,ids);walk(then_actions,doc,ids,remaining,depth+1)?;walk(else_actions,doc,ids,remaining,depth+1)?;},
                Action::Scene{id,..}=>{let timeline=doc.timed_scenes.get(id).filter(|s|s.enabled).ok_or_else(||invalid("unexpanded manual scene authority"))?;for step in &timeline.steps {ids.insert(step.device.clone());}},
                Action::RequireQuiescence{..}|Action::SceneActivation{..}=>{},
            }
        }Ok(())
    }
    let linked=document.linked_manual_scenes(&[scene.into()])?;let mut ids=BTreeSet::new();walk(&linked,document,&mut ids,&mut 4096,0)?;
    // Explicit confirmation vectors are read dependencies, even where the
    // selected AST leaves an already-known light unchanged.
    for policy in document.component_mode_confirmations.iter().filter(|p|p.scenes.iter().any(|id|id==scene)){for condition in &policy.postconditions{ids.insert(DeviceId::new(&condition.device)?);}}
    let supplies=ids.iter().filter_map(|id|config.devices().get(id).and_then(|d|d.supply_relay.clone())).collect::<Vec<_>>();ids.extend(supplies);Ok(ids)
}
