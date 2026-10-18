//! One actor's target-local runtime fences, never external authority.
use hap::server::WriteStatus;
use homesense_config::Configuration;
use homesense_core::{DeviceId, ModuleKind, gateway_scope::{GatewayTable,Phase,Port,ReadRequirement,WireScope},queue::{Epochs,QueuedCommand}};
use homesense_storage::gateway_audit::{Acceptance,CommandScope,OperationScope,OperationKey,OperationKind};
use std::collections::{BTreeMap,BTreeSet};

#[derive(Clone,Debug)]
pub(crate) struct Sample {
    pub requirement:ReadRequirement,
    pub count:u64,
    pub at:u64,
}
#[derive(Clone,Debug)]
pub(crate) struct Initial {
    pub wire:WireScope,
    pub samples:Vec<Sample>,
}
#[derive(Clone,Debug,PartialEq,Eq,serde::Serialize)]
pub struct View {
    pub gateway:String,
    pub connection_epoch:String,
    pub state:&'static str,
}
#[derive(Clone,Debug)]
pub(crate) struct FaultReport {pub audit:homesense_storage::gateway_audit::FaultAudit,pub retired:bool}
pub(crate) struct State {
    pub table:GatewayTable,
    pub faults:BTreeMap<String,(u64,homesense_storage::gateway_audit::FaultAudit)>,
}
fn failure(_: impl std::fmt::Display)->WriteStatus {WriteStatus::ServiceCommunicationFailure}
fn required(config:&Configuration,gateway:&str)->Result<BTreeSet<Port>,WriteStatus> {
    let configured=config.gateways().get(gateway).ok_or(WriteStatus::InvalidValueInRequest)?;
    Ok(configured.modules().values().filter(|m|matches!(m.kind(),ModuleKind::Switch|ModuleKind::Dimmer))
        .flat_map(|m|(1..=m.channels()).map(move|channel|Port {address:m.address().get(),channel})).collect())
}
impl State {
    pub fn scene_scopes(&self,config:&Configuration,document:&homesense_config::rule_document::Document,
        starts:&[(String,String)],extra:&[WireScope])->Result<Vec<OperationScope>,WriteStatus> {
        starts.iter().map(|(run,definition)| {
            let scene=document.timed_scenes.get(definition).ok_or(WriteStatus::InvalidValueInRequest)?;
            let wires=self.device_requirements(config,scene.steps.iter().map(|step|step.device.clone()))?;
            let mut all=extra.iter().chain(wires.iter()).map(|wire|(wire.gateway().to_owned(),wire.clone())).collect::<BTreeMap<_,_>>();
            for wire in extra {if !self.table.ready(wire) {return Err(WriteStatus::ServiceCommunicationFailure);}}
            Ok(OperationScope {key:OperationKey {kind:OperationKind::Scene,id:run.clone()},global:self.table.global(),wires:std::mem::take(&mut all).into_values().collect()})
        }).collect()
    }
    pub fn activate(config:&Configuration,global:Epochs,initial:Vec<Initial>)->Result<Self,WriteStatus> {
        let mut table=GatewayTable::new(global,config.gateways().keys().cloned()).map_err(failure)?;
        if initial.len()!=config.gateways().len() {return Err(WriteStatus::InvalidValueInRequest);}
        let mut seen=BTreeSet::new();
        for entry in initial {
            if !seen.insert(entry.wire.gateway().to_owned()) {return Err(WriteStatus::InvalidValueInRequest);}
            table.adopt_initial_attempt(&entry.wire).map_err(failure)?;
            synchronize(config,&mut table,&entry.wire,&entry.samples)?;
        }
        if !table.all_ready(){return Err(WriteStatus::ServiceCommunicationFailure);}
        Ok(Self {table,faults:BTreeMap::new()})
    }
    pub fn views(&self)->Vec<View> {
        self.table.ids().map(|id|View {gateway:id.into(),
            connection_epoch:self.table.current_wire(id).map_or_else(|_|"0".into(),|s|s.epoch().to_string()),
            state:match self.table.phase(id) {Ok(Phase::Ready)=>"ready",Ok(Phase::Offline)=>"offline",_=>"synchronizing"},
        }).collect()
    }
    pub fn ready(&self,gateway:&str)->bool {
        self.table.current_wire(gateway).is_ok_and(|wire|self.table.ready(&wire))
    }
    pub fn wire(&self,gateway:&str)->Result<WireScope,WriteStatus> {
        let wire=self.table.current_wire(gateway).map_err(failure)?;
        if !self.ready(gateway) {return Err(WriteStatus::ServiceCommunicationFailure);}
        Ok(wire)
    }
    pub fn wire_for(&self,config:&Configuration,id:&DeviceId)->Result<WireScope,WriteStatus> {
        self.wire(&config.binding(id).ok_or(WriteStatus::InvalidValueInRequest)?.0.gateway)
    }
    pub fn scope(&self,config:&Configuration,commands:&[QueuedCommand],
        physical_overrides:&BTreeMap<u64,String>,requirements:&[WireScope],
        operations:Vec<OperationScope>,
    )->Result<Acceptance,WriteStatus> {
        let requirements=canonical_requirements(requirements)?;
        if requirements.iter().any(|wire|!self.table.ready(wire)) {return Err(WriteStatus::ServiceCommunicationFailure);}
        let commands=commands.iter().map(|command| {
            let physical=match physical_overrides.get(&command.id) {
                Some(gateway)=>self.wire(gateway)?,None=>self.wire_for(config,&command.device_id)?,
            };
            Ok(CommandScope {command:command.id,physical,requirements:requirements.clone(),operations:vec![]})
        }).collect::<Result<Vec<_>,WriteStatus>>()?;
        Ok(Acceptance {commands,operations})
    }
    pub fn device_requirements(&self,config:&Configuration,ids:impl IntoIterator<Item=DeviceId>)->Result<Vec<WireScope>,WriteStatus> {
        let mut wires=BTreeMap::new();
        for id in ids {
            let wire=self.wire_for(config,&id)?;
            wires.insert(wire.gateway().to_owned(),wire);
        }
        Ok(wires.into_values().collect())
    }
    pub fn recover_ready(&mut self,config:&Configuration,wire:&WireScope,samples:&[Sample])->Result<(),WriteStatus> {
        synchronize(config,&mut self.table,wire,samples)
    }
}
fn synchronize(config:&Configuration,table:&mut GatewayTable,wire:&WireScope,samples:&[Sample])->Result<(),WriteStatus> {
    let expected=required(config,wire.gateway())?;
    let actual=samples.iter().map(|sample|sample.requirement.port).collect::<BTreeSet<_>>();
    if samples.len()!=actual.len() || actual!=expected {return Err(WriteStatus::InvalidValueInRequest);}
    table.begin_synchronization(wire,samples.iter().map(|sample|sample.requirement)).map_err(failure)?;
    for sample in samples {
        table.received(wire,sample.requirement.port,sample.count,sample.at).map_err(failure)?;
    }
    if !table.ready(wire) {return Err(WriteStatus::ServiceCommunicationFailure);}
    Ok(())
}

/// Health metadata is independent from grants and operation acceptance. Capture
/// the reviewed scene's possible reads and outputs, including a selection group's
/// explicit off members; this never schedules any member or invents Off policy.
#[derive(Debug,PartialEq,Eq,serde::Serialize)]
pub struct ActionGate {pub available:bool,pub required_gateways:Vec<View>}
pub(crate) fn scene_devices(document:&homesense_config::rule_document::Document,id:&str)->Result<BTreeSet<DeviceId>,String> {
    scene_scope_devices(document,&scene_scope(document,id)?)
}
pub(crate) fn scene_scope(document:&homesense_config::rule_document::Document,id:&str)->Result<BTreeSet<String>,String> {
    if document.scene_execution_kind(id).is_none(){return Err("unknown enabled scene".into());}
    let mut scenes=BTreeSet::from([id.to_owned()]);
    for group in &document.scene_switch_groups {
        if group.members.iter().any(|member|member.scene==id) {
            for member in &group.members {scenes.insert(member.scene.clone());scenes.insert(member.off_scene.clone());}
        }
    }
    Ok(scenes)
}
pub(crate) fn scene_scope_devices(document:&homesense_config::rule_document::Document,scenes:&BTreeSet<String>)->Result<BTreeSet<DeviceId>,String> {
    use homesense_core::rule::{Action,Condition,Expr,Node};
    fn expr(value:&Expr,ids:&mut BTreeSet<DeviceId>) {
        match value {
            Expr::DeviceOn(id)|Expr::DeviceLevel(id)=>{ids.insert(id.clone());},
            Expr::Add(a,b)|Expr::Subtract(a,b)=>{expr(a,ids);expr(b,ids);},
            _=>{},
        }
    }
    fn condition(value:&Condition,ids:&mut BTreeSet<DeviceId>) {
        match value {
            Condition::MotorState(value)=>{ids.insert(value.device.clone());},
            Condition::Equal(a,b)|Condition::Less(a,b)=>{expr(a,ids);expr(b,ids);},
            Condition::Unknown(value)=>expr(value,ids),
            Condition::Not(value)=>condition(value,ids),
            Condition::All(values)|Condition::Any(values)=>for value in values {condition(value,ids);},
        }
    }
    fn walk(nodes:&[Node],document:&homesense_config::rule_document::Document,ids:&mut BTreeSet<DeviceId>)->Result<(),String> {
        for node in nodes {
            match &node.action {
                Action::Command {device,..}|Action::Motor {device,..}=>{ids.insert(device.clone());},
                Action::Set {value,..}=>expr(value,ids),
                Action::If {condition:test,then_actions,else_actions}=>{condition(test,ids);walk(then_actions,document,ids)?;walk(else_actions,document,ids)?;},
                Action::Scene {id,activate:true}=>{
                    let scene=document.timed_scenes.get(id).filter(|scene|scene.enabled).ok_or("unresolved scene scope")?;
                    ids.extend(scene.steps.iter().map(|step|step.device.clone()));
                },
                Action::Scene {..}=>return Err("unresolved off scope".into()),
                Action::RequireQuiescence {..}|Action::SceneActivation {..}=>{},
            }
        }
        Ok(())
    }
    let mut ids=BTreeSet::new();
    for scene in scenes {
        let nodes=document.linked_manual_scenes(std::slice::from_ref(scene)).map_err(|e|e.to_string())?;
        walk(&nodes,document,&mut ids)?;
    }
    Ok(ids)
}

#[cfg(test)]
mod scene_scope_inventory_tests {
    use super::*;
    use homesense_core::{Command,ExecutionContext,rule::{Action,Condition,Expr,Node,Value}};
    fn document()->homesense_config::rule_document::Document {
        let bytes=br#"{"schema_version":1,"revision":1,"gateways":[{"id":"a","endpoint":"127.0.0.1:50007","modules":[{"address":"15","profile":"gt_d8_rl_v2_2_6_2","verification":{"status":"simulated"}}]},{"id":"b","endpoint":"127.0.0.1:50008","modules":[{"address":"15","profile":"gt_d8_rl_v2_2_6_2","verification":{"status":"simulated"}}]}],"devices":[{"id":"a-light","name":"A light","gateway":"a","address":"15","channel":1,"enabled":true},{"id":"b-light","name":"B light","gateway":"b","address":"15","channel":1,"enabled":true}]}"#;
        let config=Configuration::parse(bytes,ExecutionContext::Simulation).unwrap();
        let scene=|id,device,level|serde_json::json!({"id":id,"enabled":true,"review_evidence":"synthetic","max_lateness_ms":1000,"steps":[{"source":"synthetic","offset_ms":1000,"device":device,"command":{"kind":"level","value":level,"ramp":false}}]});
        let bytes=serde_json::to_vec(&serde_json::json!({"schema_version":1,"configuration_revision":1,"variables":[{"id":"selection","kind":"integer","lifetime":"persistent"}],"rules":[],"timed_scenes":[scene("first","a-light",50),scene("second","b-light",30),scene("off","b-light",0)]})).unwrap();
        homesense_config::rule_document::Document::parse(&bytes,&config).unwrap()
    }
    #[test]
    fn pure_scene_gate_stays_local_but_reviewed_group_collects_other_members_and_off() {
        let mut document=document();
        assert_eq!(scene_devices(&document,"second").unwrap(),BTreeSet::from([DeviceId::new("b-light").unwrap()]));
        document.scene_switch_groups.push(homesense_config::scene_switches::Group {id:"group".into(),variable:"selection".into(),review_evidence:"synthetic".into(),members:vec![
            homesense_config::scene_switches::Member {scene:"first".into(),value:1,off_scene:"off".into()},
            homesense_config::scene_switches::Member {scene:"second".into(),value:2,off_scene:"off".into()},
        ],unselected_values:vec![],reset_scenes:vec![],initialization:None,reconciliation:None});
        assert_eq!(scene_devices(&document,"second").unwrap(),BTreeSet::from([DeviceId::new("a-light").unwrap(),DeviceId::new("b-light").unwrap()]));
        assert!(scene_devices(&document,"unknown").is_err());
    }
    #[test]
    fn predicate_a_with_output_b_gate_retains_both_original_dependencies() {
        let mut document=document();
        let a=DeviceId::new("a-light").unwrap();let b=DeviceId::new("b-light").unwrap();
        document.immediate_scenes.insert("guarded".into(),homesense_core::scene_expand::Definition {activate:vec![Node {source:"synthetic decision".into(),action:Action::If {condition:Condition::Equal(Expr::DeviceOn(a.clone()),Expr::Literal(Value::Bool(true))),then_actions:vec![Node {source:"synthetic output".into(),action:Action::Command {device:b.clone(),command:Command::Switch(false)}}],else_actions:vec![]}}],deactivate:None});
        assert_eq!(scene_devices(&document,"guarded").unwrap(),BTreeSet::from([a,b]));
    }
}

fn canonical_requirements(wires:&[WireScope])->Result<Vec<WireScope>,WriteStatus> {
    let mut by_gateway=BTreeMap::new();
    for wire in wires {
        if let Some(previous)=by_gateway.insert(wire.gateway().to_owned(),wire.clone()) && previous!=*wire {
            return Err(WriteStatus::ServiceCommunicationFailure);
        }
    }
    Ok(by_gateway.into_values().collect())
}
#[cfg(test)]
mod requirement_union_tests {
    use super::*;
    #[test]
    fn duplicate_same_wire_is_canonical_but_epoch_conflict_never_recaptures() {
        let wire=|id:&str,epoch|WireScope::from_persisted(id.into(),epoch).unwrap();
        assert_eq!(canonical_requirements(&[wire("b",1),wire("a",2),wire("b",1)]).unwrap(),vec![wire("a",2),wire("b",1)]);
        assert!(canonical_requirements(&[wire("a",1),wire("a",2)]).is_err());
    }
}
