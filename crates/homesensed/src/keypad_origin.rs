//! Process-local, one-use key indication authority. It is never reconstructed
//! from a receipt, network field, device observation or a scene name.
use homesense_core::{Command, DeviceId};
use std::sync::{Arc, Mutex, RwLock, atomic::{AtomicBool, AtomicU64, Ordering}};

static NEXT: AtomicU64 = AtomicU64::new(1);
pub(crate) struct MotorIndicator {
    pub device: DeviceId,
    pub stop_key: bool,
}
pub(crate) struct Origin {
    pub endpoint: DeviceId,
    pub sequence: u64,
    pub revision: u64,
    pub request_id: String,
    pub coverage: bool,
    immediate_until: u64,
    active: RwLock<bool>,
    cue: Mutex<Option<(u64, Command)>>,
    pub fallback: Option<Command>,
    pub selection: Vec<homesense_storage::variables::ReadCheck>,
    pub motor_target: Option<MotorIndicator>,
    retired: AtomicBool,
}
impl Origin {
    #[allow(clippy::too_many_arguments)] // Immutable, server-resolved key action and its original decision fence.
    pub(crate) fn new(endpoint:DeviceId,revision:u64,request_id:String,coverage:bool,now:u64,
        fallback:Option<Command>,selection:Vec<homesense_storage::variables::ReadCheck>,motor_target:Option<MotorIndicator>)->Option<Arc<Self>> {
        let sequence=NEXT.try_update(Ordering::Relaxed,Ordering::Relaxed,|n|n.checked_add(1)).ok()?;
        Some(Arc::new(Self{endpoint,sequence,revision,request_id,coverage,immediate_until:now.checked_add(2000)?,
            active:RwLock::new(true),cue:Mutex::new(None),fallback,selection,motor_target,retired:AtomicBool::new(false)}))
    }
    pub(crate) fn current(&self)->bool {self.active.read().is_ok_and(|active|*active)}
    pub(crate) fn cue_retired(&self)->bool {self.retired.load(Ordering::Acquire)}
    pub(crate) fn invalidate(&self) {if let Ok(mut active)=self.active.write(){*active=false;}}
    pub(crate) fn with_current<T>(&self,operation:impl FnOnce()->T)->Option<T> {
        let active=self.active.read().ok()?;if !*active{return None;}Some(operation())
    }
    pub(crate) fn candidate(&self,now:u64,intents:&[homesense_hap::requests::Intent])->Option<usize> {
        if now>=self.immediate_until || !self.current() || self.cue_retired() || self.cue.lock().ok()?.is_some(){return None;}
        // Only the final evaluated LED value for this exact key is promoted.
        intents.iter().rposition(|intent|intent.device==self.endpoint
            && matches!(intent.command,Command::PanelLed(_)|Command::PanelLedOff))
    }
    pub(crate) fn fallback(&self,now:u64,intents:&[homesense_hap::requests::Intent])->Option<Command> {
        if now>=self.immediate_until || !self.current() || self.cue_retired() || self.cue.lock().ok()?.is_some()
            || intents.iter().any(|intent|intent.device==self.endpoint && matches!(intent.command,Command::PanelLed(_)|Command::PanelLedOff)) {return None;}
        if let Some(motor)=&self.motor_target {
            // Repeat-to-stop is decided by Admission's actual written motion,
            // never by the requested direction or a cached App key color.
            // A resolved STOP makes the dedicated stop key green. A direction
            // key remains red at a middle stop; green would falsely hint that
            // its endpoint had been reached before the canonical update.
            return intents.iter().find(|intent|intent.device==motor.device).and_then(|intent|match intent.command {
                Command::Motor(action)=>Some(Command::PanelLed(action!=homesense_core::MotorMotion::Stop || !motor.stop_key)),_=>None,
            });
        }
        self.fallback
    }
    pub(crate) fn accepted(&self,id:u64,command:Command) {
        if let Ok(mut cue)=self.cue.lock() && cue.is_none(){*cue=Some((id,command));}
    }
    pub(crate) fn cue(&self)->Option<(u64,Command)> {self.cue.lock().ok().and_then(|cue|*cue)}
}

#[derive(Default)]
pub(crate) struct Latest(std::collections::BTreeMap<DeviceId,crate::source_auth::Source>);
impl Latest {
    pub(crate) fn permits(&self,origin:Option<&Arc<Origin>>,revision:u64)->bool {
        origin.is_none_or(|origin|origin.revision==revision && origin.current()
            && self.0.get(&origin.endpoint).and_then(|source|source.keypad()).is_none_or(|latest|latest.sequence<=origin.sequence))
    }
    /// Called only after the action's durable acceptance committed. An invalid
    /// or rejected click cannot retire the last successful source.
    pub(crate) fn accepted(&mut self,source:Option<&crate::source_auth::Source>) {
        let Some(source)=source else{return;};let Some(origin)=source.keypad() else{return;};
        if self.0.get(&origin.endpoint).and_then(|source|source.keypad()).is_some_and(|old|old.sequence==origin.sequence){return;}
        if let Some(old)=self.0.insert(origin.endpoint.clone(),source.clone()) && let Some(origin)=old.keypad(){origin.invalidate();}
    }
    pub(crate) fn sources(&self)->impl Iterator<Item=&crate::source_auth::Source> {self.0.values()}
    pub(crate) fn contains(&self,origin:&Origin)->bool {self.0.get(&origin.endpoint).and_then(|source|source.keypad()).is_some_and(|current|current.sequence==origin.sequence)}
    /// Only a successful later output retires the display cue. Keep its
    /// sequence tombstone and every actuator/source authorization intact.
    pub(crate) fn led_written(&self,endpoint:&DeviceId,command:u64) {
        if let Some(origin)=self.0.get(endpoint).and_then(|source|source.keypad())
            && origin.cue().is_some_and(|cue|cue.0!=command){origin.retired.store(true,Ordering::Release);}
    }
    pub(crate) fn clear(&mut self){for source in self.0.values(){if let Some(origin)=source.keypad(){origin.invalidate();}}self.0.clear();}
}
