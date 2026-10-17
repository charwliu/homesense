//! Private authority of one configured physical event owner. No bearer secret,
//! HTTP grant or externally supplied origin can construct this capability.
use crate::admission::TaskGeneration;
use homesense_config::{Configuration,rule_document::Document};
use homesense_core::DeviceId;
use std::{collections::BTreeSet,sync::{Arc,RwLock}};
use uuid::Uuid;

struct State {
    active:RwLock<bool>,controller:Uuid,generation:TaskGeneration,
    owned:BTreeSet<DeviceId>,devices:BTreeSet<String>,scenes:BTreeSet<String>,
}
pub(crate) struct Owner { state:Arc<State> }
#[derive(Clone)]
pub(crate) struct Registration(std::sync::Weak<State>);
#[derive(Clone,Debug,Default,PartialEq,Eq,serde::Serialize)]
pub struct Status {
    pub configured_keypads:usize,pub configured_endpoints:usize,pub owner_active:bool,
    pub authority_generation:Option<Generation>,pub legacy_writer_isolation_evidence:Option<&'static str>,
}
#[derive(Clone,Debug,PartialEq,Eq,serde::Serialize)]
pub struct Generation {pub boot:String,pub configuration:String,pub transport:String,pub control:String}
#[derive(Clone)]
pub(crate) struct Lease { state:Arc<State>,endpoint:DeviceId }
impl std::fmt::Debug for Lease {fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result{f.write_str("ConfiguredPanelLease(<opaque>)")}}
impl Owner {
    pub(crate) fn new(config:&Configuration,document:&Document,controller:Uuid,generation:TaskGeneration)->Result<Option<Self>,String> {
        if config.revision()!=document.revision || generation.epochs.configuration!=document.revision{return Err("event owner revision mismatch".into());}
        let owned=config.keypads().values().filter(|panel|panel.executes_events()).flat_map(|panel|panel.keys.values())
            .map(|key|DeviceId::new(&key.led_endpoint.id).map_err(|e|e.to_string())).collect::<Result<BTreeSet<_>,_>>()?;
        if owned.is_empty(){return Ok(None);}
        if owned.len()>256{return Err("configured event owner key capacity exceeded".into());}
        let devices=config.devices().keys().filter(|id|config.binding(id).is_some()).map(|id|id.as_str().to_owned()).collect();
        let scenes=document.immediate_scenes.keys().chain(document.timed_scenes.keys()).filter(|id|document.scene_execution_kind(id).is_some()).cloned().collect();
        Ok(Some(Self{state:Arc::new(State{active:RwLock::new(true),controller,generation,owned,devices,scenes})}))
    }
    pub(crate) fn lease(&self,endpoint:&DeviceId)->Option<Lease> {
        self.state.owned.contains(endpoint).then(||Lease{state:self.state.clone(),endpoint:endpoint.clone()}).filter(Lease::valid)
    }
    pub(crate) fn retire(&self){if let Ok(mut active)=self.state.active.write(){*active=false;}}
    pub(crate) fn registration(&self)->Registration{Registration(Arc::downgrade(&self.state))}
}
impl Registration {
    pub(crate) fn validate(&self,controller:Uuid,generation:&TaskGeneration,config:&Configuration)->bool {
        let Some(state)=self.0.upgrade() else{return false;};
        let owned=config.keypads().values().filter(|panel|panel.executes_events()).flat_map(|panel|panel.keys.values()).filter_map(|key|DeviceId::new(&key.led_endpoint.id).ok()).collect::<BTreeSet<_>>();
        let devices=config.devices().keys().filter(|id|config.binding(id).is_some()).map(|id|id.as_str().to_owned()).collect::<BTreeSet<_>>();
        state.controller==controller&&&state.generation==generation&&state.owned==owned&&state.devices==devices&&state.active.read().is_ok_and(|active|*active)
    }
}
pub(crate) fn status(config:&Configuration,registration:Option<&Registration>,current:Option<&TaskGeneration>)->Status {
    let keypads=config.keypads().values().filter(|panel|panel.executes_events()).collect::<Vec<_>>();
    let active=registration.and_then(|registration|registration.0.upgrade()).filter(|state|current==Some(&state.generation)&&state.active.read().is_ok_and(|active|*active));
    Status {configured_keypads:keypads.len(),configured_endpoints:keypads.iter().map(|panel|panel.keys.len()).sum(),owner_active:active.is_some(),
        authority_generation:active.map(|state|Generation{boot:state.generation.boot_id().into(),configuration:state.generation.epochs.configuration.to_string(),transport:state.generation.epochs.transport.to_string(),control:state.generation.epochs.control.to_string()}),
        legacy_writer_isolation_evidence:(!keypads.is_empty()).then_some("operator_declaration"),}
}
impl Drop for Owner {fn drop(&mut self){self.retire();}}
impl Lease {
    pub(crate) fn controller(&self)->Uuid {self.state.controller}
    pub(crate) fn generation(&self)->&TaskGeneration {&self.state.generation}
    pub(crate) fn endpoint(&self)->&DeviceId {&self.endpoint}
    pub(crate) fn devices(&self)->&BTreeSet<String> {&self.state.devices}
    pub(crate) fn scenes(&self)->&BTreeSet<String> {&self.state.scenes}
    pub(crate) fn valid(&self)->bool {self.with_valid(||()).is_some()}
    pub(crate) fn same_authority(&self,other:&Self)->bool {Arc::ptr_eq(&self.state,&other.state)&&self.endpoint==other.endpoint}
    pub(crate) fn with_valid<T>(&self,operation:impl FnOnce()->T)->Option<T> {
        let active=self.state.active.read().ok()?;
        if !*active || !self.state.owned.contains(&self.endpoint){return None;}Some(operation())
    }
    pub(crate) fn with_valid_dependency<T>(&self,other:&Self,operation:impl FnOnce()->T)->Option<T> {
        if !Arc::ptr_eq(&self.state,&other.state)||!self.state.owned.contains(&other.endpoint){return None;}
        self.with_valid(operation)
    }
}
