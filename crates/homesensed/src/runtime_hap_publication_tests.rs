//! Pure unavailable projection must not access the old restore ledger during publication.
use super::*;
use hap::{HapType,server::{EventEmitter,ServerEvent},storage::Storage};
use homesense_core::{DeviceId,ExecutionContext,Observation,Percent,Command,LevelTransition};
use homesense_state::Quality;
use std::{path::PathBuf,sync::Mutex,time::Duration};

struct Fixture {
    directory:PathBuf,
    admission:Admission,
    graph:RuntimeHap,
    physical:Snapshot,
    next:Vec<u8>,
    events:Arc<Mutex<Vec<serde_json::Value>>>,
}
impl Fixture {
    async fn new(combined:bool)->Self {
        let directory=std::env::temp_dir().join(format!("hap-publication-unavailable-{}",uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let mut document=serde_json::json!({"schema_version":2,"revision":1,"gateways":[{"id":"bus","endpoint":"127.0.0.1:1","modules":[{"id":"dimmer","address":"15","profile":"ad8_din_rs485_v2_2_1","verification":{"status":"simulated"}}]}],"devices":[{"id":"light","name":"Synthetic light","module_id":"dimmer","channel":1,"enabled":true}]});
        if combined {
            document["gateways"][0]["modules"].as_array_mut().unwrap().push(serde_json::json!({"id":"supply","address":"03","profile":"r8i8_din_pro_v2_7_7","verification":{"status":"simulated"}}));
            document["devices"][0]["supply"]=serde_json::json!({"module_id":"supply","channel":1});
            document["devices"][0]["default_on_level"]=30.into();
            document["devices"].as_array_mut().unwrap().push(serde_json::json!({"id":"supply","name":"Synthetic supply","module_id":"supply","channel":1,"enabled":true}));
        }
        let path=directory.join("runtime.sqlite");let mut store=homesense_storage::Store::open(&path).unwrap();
        let configuration=store.publish_configuration(None,&serde_json::to_vec(&document).unwrap(),ExecutionContext::Simulation,&[]).unwrap();
        store.remember_light_levels(1,&[(DeviceId::new("light").unwrap(),Percent::new(30).unwrap())]).unwrap();drop(store);
        let admission=Admission::open(path,crate::admission::Options{context:ExecutionContext::Simulation,..Default::default()},|_|true).await.unwrap();
        admission.synchronized(1,1).await.unwrap();
        let boot=uuid::Uuid::parse_str(admission.boot_id()).unwrap().as_u128();
        let mut physical=crate::feedback::Feedback::new(&configuration,ExecutionContext::Simulation,boot,1).unwrap().snapshot();
        for state in physical.devices.values_mut(){
            state.quality=Quality::Confirmed;state.observed_at_ms=Some(admission.monotonic_ms());
            state.observed=Some(if state.kind==ModuleKind::Switch {Observation::Switch(true)}else{Observation::Level(Percent::new(30).unwrap())});
        }
        let mut identity=IdentityStore::open(directory.join("identity")).await.unwrap();identity.save_config(&hap::Config::default()).await.unwrap();
        let graph=RuntimeHap::new(&configuration,physical.clone(),&identity,admission.clone(),5000).await.unwrap();
        let events=Arc::new(Mutex::new(vec![]));let captured=events.clone();let mut emitter=EventEmitter::new();
        emitter.add_listener(Box::new(move |event| {if let ServerEvent::CharacteristicValueChanged{value,..}=event {captured.lock().unwrap().push(value.clone());}Box::pin(async {})}));
        {let mut accessory=graph.lights[0].1.lock().await;accessory.set_event_emitter_on_characteristics(Some(Arc::new(hap::futures::lock::Mutex::new(emitter))));let service=accessory.get_mut_service(HapType::Lightbulb).unwrap();
            for kind in [HapType::PowerState,HapType::Brightness]{service.get_mut_characteristic(kind).unwrap().set_event_notifications(Some(true));}
        }
        document["revision"]=2.into();document["devices"][0]["channel"]=2.into();
        Self{directory,admission,graph,physical,next:serde_json::to_vec(&document).unwrap(),events}
    }
    fn stale(&self)->Snapshot {
        let mut stale=self.physical.clone();stale.cursor.sequence+=1;stale.state_version=stale.cursor.sequence;
        for state in stale.devices.values_mut(){state.quality=Quality::Stale;}
        stale
    }
    async fn begin(&self){self.admission.begin_publication(self.next.clone(),1,Duration::from_secs(3)).await.unwrap();}
    async fn assert_unknown(&self){
        let mut accessory=self.graph.lights[0].1.lock().await;let service=accessory.get_mut_service(HapType::Lightbulb).unwrap();
        for kind in [HapType::PowerState,HapType::Brightness]{assert!(service.get_mut_characteristic(kind).unwrap().get_value().await.is_err());}
        assert!(self.graph.snapshot.read().unwrap().devices.values().all(|state|state.quality!=Quality::Confirmed));
    }
    fn saved(&self,revision:u64)->std::collections::BTreeMap<DeviceId,Percent>{homesense_storage::Store::open(self.directory.join("runtime.sqlite")).unwrap().light_restore_levels(revision).unwrap()}
    async fn finish(self){
        let db=rusqlite::Connection::open(self.directory.join("runtime.sqlite")).unwrap();let count:i64=db.query_row("SELECT count(*) FROM command_journal",[],|row|row.get(0)).unwrap();assert_eq!(count,0);drop(db);
        let Self{directory,admission,graph,..}=self;drop(graph);admission.shutdown().await.unwrap();std::fs::remove_dir_all(directory).unwrap();
    }
}
#[tokio::test]
async fn old_graph_stays_unknown_before_and_after_durable_commit_without_restore_writes(){
    for combined in [false,true]{
        let mut fixture=Fixture::new(combined).await;let saved=fixture.saved(1);
        let binding=fixture.graph.lights[0].0.clone();fixture.begin().await;
        let stale=fixture.stale();fixture.graph.publish_snapshot(stale.clone()).await.unwrap();fixture.assert_unknown().await;
        assert_eq!(*fixture.events.lock().unwrap(),vec![serde_json::Value::Null;2]);assert_eq!(fixture.saved(1),saved);
        assert!(fixture.admission.task_generation().await.is_err());
        assert_eq!(fixture.admission.commit_publication().await.unwrap(),2);let committed_saved=fixture.saved(2);
        fixture.graph.publish_snapshot(stale).await.unwrap();fixture.assert_unknown().await;
        assert_eq!(*fixture.events.lock().unwrap(),vec![serde_json::Value::Null;2],"duplicate invalidation must not emit known or duplicate values");
        assert_eq!(fixture.saved(1),saved);assert_eq!(fixture.saved(2),committed_saved);
        assert_eq!(fixture.graph.restore_levels.read().unwrap().get(&DeviceId::new("light").unwrap()),Some(&Percent::new(30).unwrap()));
        assert_eq!(fixture.graph.lights[0].0.aid,binding.aid);assert_eq!(fixture.graph.lights[0].0.on_iid,binding.on_iid);assert_eq!(fixture.graph.lights[0].0.brightness_iid,binding.brightness_iid);
        let mut fresh=fixture.physical.clone();fresh.cursor.sequence+=2;fresh.state_version=fresh.cursor.sequence;
        fixture.graph.publish_snapshot(fresh).await.unwrap();
        fixture.assert_unknown().await;assert_eq!(fixture.saved(1),saved);assert_eq!(fixture.saved(2),committed_saved);
        assert!(fixture.admission.remember_levels(1,fixture.physical.clone(),5000).await.is_err(),"ordinary old-revision API must remain fenced");
        fixture.finish().await;
    }
}
#[tokio::test]
async fn unavailable_projection_rejects_foreign_regressed_or_changed_mapping_and_mixed_quality(){
    let mut fixture=Fixture::new(true).await;fixture.begin().await;
    let stale=fixture.stale();fixture.graph.publish_snapshot(stale.clone()).await.unwrap();
    let mut wrong_boot=stale.clone();wrong_boot.cursor.boot_id+=1;assert!(fixture.graph.publish_snapshot(wrong_boot).await.is_err());
    let mut regressed=stale.clone();regressed.cursor.sequence=0;assert!(fixture.graph.publish_snapshot(regressed).await.is_err());
    let mut missing=stale.clone();assert!(missing.devices.remove(&DeviceId::new("supply").unwrap()).is_some());assert!(fixture.graph.publish_snapshot(missing).await.is_err());
    let mut wrong_kind=stale.clone();wrong_kind.devices.get_mut(&DeviceId::new("light").unwrap()).unwrap().kind=ModuleKind::Switch;assert!(fixture.graph.publish_snapshot(wrong_kind).await.is_err());
    let mut mixed=stale.clone();mixed.devices.get_mut(&DeviceId::new("light").unwrap()).unwrap().quality=Quality::Confirmed;
    fixture.graph.publish_snapshot(mixed).await.unwrap();fixture.assert_unknown().await;
    assert!(fixture.admission.remember_levels(1,fixture.physical.clone(),5000).await.is_err(),"mixed input must not reopen ordinary actor admission");
    fixture.admission.abort_publication().await.unwrap();fixture.admission.shutdown().await.unwrap();
    let mut fresh=fixture.physical.clone();fresh.cursor.sequence+=2;fresh.state_version=fresh.cursor.sequence;
    let error=fixture.graph.publish_snapshot(fresh).await.unwrap_err();
    eprintln!("closed actor HAP projection: {error}");
    assert!(["HAP projection: ResourceBusy","HAP projection: ServiceCommunicationFailure"].contains(&error.as_str()),"unexpected closed-channel status: {error}");
    drop(fixture.graph);std::fs::remove_dir_all(fixture.directory).unwrap();
}
#[tokio::test]
async fn unavailable_projection_clears_local_transition_suppression_without_reading_restore_ledger(){
    let mut fixture=Fixture::new(false).await;
    fixture.graph.projection.transitions().accepted(DeviceId::new("light").unwrap(),Command::SetLevel{level:Percent::new(25).unwrap(),transition:LevelTransition::Ramp},fixture.admission.monotonic_ms());
    fixture.begin().await;let stale=fixture.stale();fixture.graph.publish_snapshot(stale).await.unwrap();fixture.assert_unknown().await;
    assert!(fixture.admission.task_generation().await.is_err());fixture.admission.abort_publication().await.unwrap();
    let mut fresh=fixture.physical.clone();fresh.cursor.sequence+=2;fresh.state_version=fresh.cursor.sequence;
    fixture.graph.publish_snapshot(fresh).await.unwrap();
    assert_eq!(*fixture.events.lock().unwrap(),vec![serde_json::Value::Null,serde_json::Value::Null,serde_json::json!(true),serde_json::json!(30)],"cancelled target25 must not suppress explicit fresh30 after abort");
    fixture.finish().await;
}

#[tokio::test]
async fn confirmed_projection_crossing_publication_gate_reports_unknown_without_reusing_split_reads(){
    let mut fixture=Fixture::new(false).await;let saved=fixture.saved(1);
    // Deterministically reproduce the former two-request interleaving: the
    // remember succeeds, publication enters, then the old targets read fails.
    fixture.admission.remember_levels(1,fixture.physical.clone(),5000).await.unwrap();
    fixture.begin().await;
    assert!(fixture.admission.projection_levels(1,fixture.physical.cursor.boot_id).await.is_err());
    fixture.graph.publish_snapshot(fixture.physical.clone()).await.unwrap();fixture.assert_unknown().await;
    assert_eq!(*fixture.events.lock().unwrap(),vec![serde_json::Value::Null;2]);assert_eq!(fixture.saved(1),saved);
    assert!(fixture.admission.task_generation().await.is_err());
    assert_eq!(fixture.admission.commit_publication().await.unwrap(),2);
    fixture.graph.publish_snapshot(fixture.physical.clone()).await.unwrap();fixture.assert_unknown().await;
    fixture.admission.synchronized(1,2).await.unwrap();
    // Even once the new configuration is ready, the old graph cannot promote
    // its old-channel Confirmed sample back to known state.
    fixture.graph.publish_snapshot(fixture.physical.clone()).await.unwrap();fixture.assert_unknown().await;
    assert_eq!(*fixture.events.lock().unwrap(),vec![serde_json::Value::Null;2]);assert_eq!(fixture.saved(1),saved);
    assert!(fixture.physical.devices.values().all(|state|state.quality==Quality::Confirmed),"projection must not mutate source measurements");
    fixture.finish().await;
}
#[tokio::test]
async fn confirmed_projection_preserves_real_store_read_failures_as_errors(){
    let mut fixture=Fixture::new(false).await;
    // Damage only this generated fixture ledger while its actor is ready.
    // A database error must never be classified as an unavailable lifecycle.
    assert!(fixture.admission.task_generation().await.is_ok());
    let db=rusqlite::Connection::open(fixture.directory.join("runtime.sqlite")).unwrap();
    db.execute_batch("DROP TABLE light_restore").unwrap();drop(db);
    let error=fixture.graph.publish_snapshot(fixture.physical.clone()).await.unwrap_err();
    assert!(error.contains("ServiceCommunicationFailure"),"{error}");
    assert!(fixture.events.lock().unwrap().is_empty(),"storage failure must not be converted into a successful unknown projection");
    fixture.finish().await;
}

#[tokio::test]
async fn confirmed_old_graph_stays_unavailable_after_transport_epoch_replacement(){
    let mut fixture=Fixture::new(false).await;let saved=fixture.saved(1);
    fixture.admission.transport_failed().await.unwrap();
    fixture.graph.publish_snapshot(fixture.physical.clone()).await.unwrap();fixture.assert_unknown().await;
    fixture.admission.recover_synchronized(2,1).await.unwrap();
    assert!(fixture.admission.task_generation().await.is_ok(),"the new owner is ready, but the old graph is not its measurement source");
    fixture.graph.publish_snapshot(fixture.physical.clone()).await.unwrap();fixture.assert_unknown().await;
    assert_eq!(*fixture.events.lock().unwrap(),vec![serde_json::Value::Null;2]);assert_eq!(fixture.saved(1),saved);
    fixture.finish().await;
}

#[tokio::test]
async fn failed_transport_audit_is_not_hidden_by_an_earlier_audited_outage(){
    let mut fixture=Fixture::new(false).await;
    fixture.admission.transport_failed().await.unwrap();
    fixture.graph.publish_snapshot(fixture.physical.clone()).await.unwrap();fixture.assert_unknown().await;
    let db=rusqlite::Connection::open(fixture.directory.join("runtime.sqlite")).unwrap();
    // Renaming only this generated fixture table makes the next transport
    // fault audit fail, after a previous audit demonstrably succeeded.
    db.execute_batch("ALTER TABLE command_journal RENAME TO parked_journal").unwrap();
    assert!(fixture.admission.transport_failed().await.is_err());
    let error=fixture.graph.publish_snapshot(fixture.physical.clone()).await.unwrap_err();
    assert!(error.contains("ServiceCommunicationFailure"),"a real failed audit cannot become an unavailable success: {error}");
    db.execute_batch("ALTER TABLE parked_journal RENAME TO command_journal").unwrap();drop(db);
    fixture.assert_unknown().await;fixture.finish().await;
}
