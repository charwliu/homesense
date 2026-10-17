//! Exact source authority retained by an approved logical operation and
//! its derived work. Physical expansion never broadens the originating grant.
use crate::{automation_control::Action, web_auth::ModuleOffLease};
use serde_json::{Value, json};
use uuid::Uuid;
/// One immutable proof is shared by all derived work. Keep the handle small:
/// Admission is retained repeatedly through nested async control operations.
#[derive(Clone)]
pub struct Source {
    proof: std::sync::Arc<Proof>,
}
struct Proof {
    lease: Authority,
    controller: Uuid,
    revision: u64,
    signature: Value,
    administrator: bool,
    keypad: Option<std::sync::Arc<crate::keypad_origin::Origin>>,
    event:Option<EventEvidence>,
}
#[derive(Clone)]
enum Authority {Web(ModuleOffLease),Panel(crate::panel_execution_auth::Lease)}
impl Authority {
    fn controller(&self)->Uuid {match self{Self::Web(lease)=>lease.controller(),Self::Panel(lease)=>lease.controller()}}
    fn valid(&self)->bool {match self{Self::Web(lease)=>lease.valid(),Self::Panel(lease)=>lease.valid()}}
    fn administrator(&self)->bool {match self{Self::Web(lease)=>lease.is_administrator(),Self::Panel(_)=>false}}
    fn devices(&self)->&std::collections::BTreeSet<String> {match self{Self::Web(lease)=>lease.devices(),Self::Panel(lease)=>lease.devices()}}
    fn scenes(&self)->&std::collections::BTreeSet<String> {match self{Self::Web(lease)=>lease.scenes(),Self::Panel(lease)=>lease.scenes()}}
    fn revision_permitted(&self,revision:u64)->bool{match self{Self::Web(_)=>true,Self::Panel(lease)=>lease.generation().epochs.configuration==revision}}
    fn with_valid<T>(&self,operation:impl FnOnce()->T)->Option<T>{match self{Self::Web(lease)=>lease.with_valid(operation),Self::Panel(lease)=>lease.with_valid(operation)}}
    fn with_dependency<T>(&self,other:&Self,operation:impl FnOnce()->T)->Option<T>{match(self,other){
        (Self::Web(a),Self::Web(b))=>a.with_valid_dependency(b,operation),
        (Self::Panel(a),Self::Panel(b))=>a.with_valid_dependency(b,operation),
        (Self::Web(web),Self::Panel(panel))|(Self::Panel(panel),Self::Web(web))=>web.with_valid(||panel.with_valid(operation))?,
    }}
}
#[derive(Clone)]
pub(crate) struct EventEvidence {
    generation:crate::admission::TaskGeneration,device:homesense_core::DeviceId,
    cursor:homesense_state::Cursor,observed_ms:u64,wire:Option<homesense_core::gateway_scope::WireScope>,
}
impl EventEvidence {
    pub(crate) fn generation(&self)->&crate::admission::TaskGeneration{&self.generation}
    pub(crate) fn device(&self)->&homesense_core::DeviceId{&self.device}
    pub(crate) fn cursor(&self)->homesense_state::Cursor{self.cursor}
    pub(crate) fn observed_ms(&self)->u64{self.observed_ms}
    pub(crate) fn wire(&self)->Option<&homesense_core::gateway_scope::WireScope>{self.wire.as_ref()}
}
impl std::fmt::Debug for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CommandSource(<opaque>)")
    }
}
type Description = (Uuid, u64, Value, Vec<String>, Vec<String>, bool);
fn describe(action: &Action) -> Option<Description> {
    Some(match action {
        Action::KeypadAction { controller, request } => (
            *controller, request.validate().ok()?, json!({"kind":"keypad","request":request}),
            vec![],vec![],false,
        ),
        Action::AwayOff {controller,revision,submit,..} => (
            *controller,*revision,json!({"kind":"away","submit":submit}),vec![],vec![],false,
        ),
        Action::DeviceCommand {
            controller,
            revision,
            device,
            action,
        } => (
            *controller,
            *revision,
            json!({"kind":"device","id":device.as_str(),"action":action}),
            vec![device.as_str().into()],
            vec![],
            false,
        ),
        Action::ModuleCommand {
            controller,
            revision,
            module_id,
            channel,
            allowed_devices,
            action,
        } => (
            *controller,
            *revision,
            json!({"kind":"module","module":module_id,"channel":channel,"allowed":allowed_devices,"action":action}),
            allowed_devices.iter().cloned().collect(),
            vec![],
            true,
        ),
        Action::SceneAction {
            controller,
            revision,
            scene,
            execute,
        } => (
            *controller,
            *revision,
            json!({"kind":"scene","scene":scene,"execute":execute}),
            vec![],
            vec![scene.clone()],
            false,
        ),
        Action::SceneRelative {
            controller,
            revision,
            scene,
            request_id,
        } => (
            *controller,
            *revision,
            json!({"kind":"relative","scene":scene,"request":request_id}),
            vec![],
            vec![scene.clone()],
            false,
        ),
        Action::SceneActions {
            controller,
            revision,
            scenes,
        } => (
            *controller,
            *revision,
            json!({"kind":"scenes","scenes":scenes}),
            vec![],
            scenes.iter().map(|(id, _)| id.clone()).collect(),
            false,
        ),
        Action::SceneWrites {
            controller,
            revision,
            scenes,
        }
        | Action::SceneActivityWrites {
            controller,
            revision,
            scenes,
        } => (
            *controller,
            *revision,
            json!({"kind":if matches!(action,Action::SceneWrites{..}){"writes"}else{"activity_writes"},"scenes":scenes}),
            vec![],
            scenes.iter().map(|(id, _)| id.clone()).collect(),
            false,
        ),
        Action::SceneCancel {
            controller,
            revision,
            run,
        } => (
            *controller,
            *revision,
            json!({"kind":"cancel","run":run}),
            vec![],
            vec![],
            false,
        ),
        Action::ModuleQuery {
            controller,
            revision,
            module_id,
            scope,
            submit,
        } => (
            *controller,
            *revision,
            json!({"kind":"module_query","module":module_id,"scope":scope,"submit":submit}),
            vec![],
            vec![],
            true,
        ),
        _ => return None,
    })
}
impl Source {
    pub(crate) fn same_authority(&self,other:&Self)->bool{std::sync::Arc::ptr_eq(&self.proof,&other.proof)}

    pub(crate) fn supports(action: &Action) -> bool {
        describe(action).is_some()
    }
    pub(crate) fn capture(
        lease: ModuleOffLease,
        action: &Action,
        scene_hint: Option<&str>,
    ) -> Option<Self> {
        Self::capture_authority(Authority::Web(lease),action,scene_hint)
    }
    fn capture_authority(lease:Authority,action:&Action,scene_hint:Option<&str>)->Option<Self>{
        let (controller, revision, signature, devices, mut scenes, administrator) =
            describe(action)?;
        if matches!(action, Action::SceneCancel { .. }) {
            scenes.push(scene_hint?.to_owned());
        }
        if !lease.valid()
            || lease.controller() != controller
            || !lease.revision_permitted(revision)
            || (administrator && !lease.administrator())
            || devices.iter().any(|id| !lease.devices().contains(id))
            || scenes.iter().any(|id| !lease.scenes().contains(id))
        {
            return None;
        }
        Some(Self {
            proof: std::sync::Arc::new(Proof {
                lease,
                controller,
                revision,
                signature,
                administrator,
                keypad: None,
                event:None,
            }),
        })
    }
    pub(crate) fn capture_event(lease:crate::panel_execution_auth::Lease,action:&Action,proof:&crate::feedback::SceneKeyObservation,now:u64)->Option<Self>{
        let event=proof.event();let generation=lease.generation();
        let boot=uuid::Uuid::parse_str(generation.boot_id()).ok()?.as_u128();let at=event.state.observed_at_ms?;
        if !matches!(action,Action::KeypadAction{..})||event.device_id!=*lease.endpoint()||event.cursor.boot_id!=boot
            ||now.checked_sub(at).is_none_or(|age|age>=5000){return None;}
        let _guard=proof.lease().lock(boot,generation.epochs.configuration,generation.epochs.transport,now).ok()?;
        let evidence=EventEvidence{generation:generation.clone(),device:event.device_id.clone(),cursor:event.cursor,observed_ms:at,wire:proof.wire().cloned()};
        let mut source=Self::capture_authority(Authority::Panel(lease),action,None)?;
        std::sync::Arc::get_mut(&mut source.proof)?.event=Some(evidence);Some(source)
    }
    pub(crate) fn event(&self)->Option<&EventEvidence>{self.proof.event.as_ref()}
    pub(crate) fn derive(&self,action:&Action)->Option<Self>{
        let mut derived=Self::capture_authority(self.proof.lease.clone(),action,None)?;
        std::sync::Arc::get_mut(&mut derived.proof)?.event=self.proof.event.clone();Some(derived)
    }
    pub(crate) fn away_keypad(&self,keypad:String,key:u8)->Option<crate::away_auth::Authorization>{
        match &self.proof.lease {
            Authority::Web(lease)=>crate::away_auth::Authorization::keypad(lease.clone(),&crate::away_auth::KeypadCommand::KeypadPress{keypad,key}),
            Authority::Panel(lease)=>crate::away_auth::Authorization::configured_keypad(lease.clone(),keypad,key),
        }
    }
    pub(crate) fn matches(&self, action: &Action) -> bool {
        describe(action).is_some_and(|(controller, revision, signature, ..)| {
            self.valid_for(controller, revision) && self.proof.signature == signature
        })
    }
    pub(crate) fn valid(&self) -> bool {
        self.proof.lease.valid()
            && (!self.proof.administrator || self.proof.lease.administrator())
    }
    pub(crate) fn permits_device(&self, device: &str) -> bool {
        self.valid() && self.proof.lease.devices().contains(device)
    }
    pub(crate) fn permits_scene(&self, scene: &str) -> bool {
        self.valid() && self.proof.lease.scenes().contains(scene)
    }
    pub(crate) fn with_keypad(mut self,origin:std::sync::Arc<crate::keypad_origin::Origin>)->Self {
        std::sync::Arc::get_mut(&mut self.proof).expect("new exact source has one owner").keypad=Some(origin);self
    }
    pub(crate) fn keypad(&self)->Option<&std::sync::Arc<crate::keypad_origin::Origin>> {self.proof.keypad.as_ref()}
    pub(crate) fn indication_current(&self)->bool {self.keypad().is_none_or(|origin|origin.current())}
    pub(crate) fn with_valid_command<T>(&self,command:&homesense_core::queue::QueuedCommand,operation:impl FnOnce()->T)->Option<T> {
        self.with_valid(||if matches!(command.command,homesense_core::Command::PanelLed(_)|homesense_core::Command::PanelLedOff) {
            match self.keypad(){Some(origin)=>origin.with_current(operation),None=>Some(operation())}
        }else{Some(operation())})?
    }
    pub(crate) fn valid_for(&self, controller: Uuid, revision: u64) -> bool {
        self.proof.controller == controller && self.proof.revision == revision && self.valid()
    }
    pub(crate) fn with_valid<T>(&self, operation: impl FnOnce() -> T) -> Option<T> {
        self.proof.lease.with_valid(operation)
    }
}

/// Hold both exact source generations through conditional queue acceptance.
/// An old Written source revoked after the decision cannot authorize its STOP.
pub(crate) fn commit_with_dependency<T>(source:Option<&Source>, dependency:Option<&Source>, operation:impl FnOnce()->T)->Option<T> {
    match (source,dependency) {
        (Some(source),Some(dependency))=>source.proof.lease.with_dependency(&dependency.proof.lease,operation),
        (_,None)=>commit(source,operation),
        (None,Some(_))=>None,
    }
}

/// Internal mutation boundary. This takes no credential or scope from payloads.
/// The same immutable source read guard is used for durable admission and I/O.
pub(crate) fn commit<T>(source: Option<&Source>, operation: impl FnOnce() -> T) -> Option<T> {
    match source {
        Some(source) => source.with_valid(operation),
        None => Some(operation()),
    }
}

#[cfg(test)]
#[test]
fn source_proof_handle_stays_small_in_nested_control_futures() {
    assert_eq!(std::mem::size_of::<Source>(), std::mem::size_of::<usize>());
    assert!(std::mem::size_of::<Option<Source>>() <= 2 * std::mem::size_of::<usize>());
}
