//! Authenticated in-process management handlers and real local SQLite only.
use homesensed::{admission::{Admission,Options},automation_runtime::{AutomationRuntime,StartupPolicy},web_api::{Api,Runtime},web_auth::{Tokens,Grant,Role},web_receipts::Receipts,web_drafts::{Drafts,Draft}};
use homesense_core::ExecutionContext;
use hyper::{Request,StatusCode,body::Bytes};
use http_body_util::BodyExt;
use serde_json::{Value,json};
use std::sync::{Arc,Mutex,RwLock};
use uuid::Uuid;
struct Lab {dir:std::path::PathBuf,api:Api,actor:Admission,runtime:AutomationRuntime,caller:Uuid,secret:String,devices:Value}
impl Lab {
 async fn new()->Self {
  let dir=std::env::temp_dir().join(format!("root-web-management-{}",Uuid::new_v4()));std::fs::create_dir(&dir).unwrap();
  let devices:Value=serde_json::from_slice(include_bytes!("../../../config/simulation.json")).unwrap();
  homesense_storage::Store::open(dir.join("runtime.sqlite")).unwrap().publish_automation(None,&serde_json::to_vec(&devices).unwrap(),&serde_json::to_vec(&json!({"schema_version":1,"configuration_revision":1,"variables":[],"rules":[]})).unwrap(),ExecutionContext::Simulation,&[]).unwrap();
  let caller=Uuid::new_v4();let actor=Admission::open(dir.join("runtime.sqlite"),Options{context:ExecutionContext::Simulation,..Default::default()},move|id|id==caller).await.unwrap();actor.synchronized(1,1).await.unwrap();
  let runtime=AutomationRuntime::load_with_startup(actor.clone(),caller,1_800_000_000_000,1000,8,Default::default(),StartupPolicy::Suppress).await.unwrap();
  let secret="RootSyntheticManagementSecret0123456789".to_owned();let tokens=Arc::new(Tokens::open(&dir.join("credentials.sqlite")).unwrap());
  tokens.insert(Uuid::new_v4(),&secret,Grant{controller:caller,role:Role::Administrator,devices:["simulation:light".into()].into(),scenes:Default::default()}).unwrap();
  let api=Api{daily_backup:Default::default(),hap_bridges:Default::default(),hap_status:Default::default(),backup_source:None,publisher:None,origin:"http://127.0.0.1:8088".into(),tokens,runtime:RwLock::new(Some(Runtime{control:runtime.control(),revision:1,boot:actor.boot_id().into(),scenes:Default::default()})),drafts:Some((ExecutionContext::Simulation,Mutex::new(Drafts::open(&dir.join("drafts.sqlite")).unwrap()))),receipts:Mutex::new(Receipts::open(&dir.join("web.sqlite")).unwrap())};
  Self{dir,api,actor,runtime,caller,secret,devices}
 }
 async fn post(&self,path:&str,key:&str,body:Value)->(StatusCode,Value){
  let req=Request::builder().method("POST").uri(path).header("host","127.0.0.1:8088").header("origin",&self.api.origin).header("authorization",format!("Bearer {}",self.secret)).header("content-type","application/json").header("idempotency-key",key).body(Bytes::from(serde_json::to_vec(&body).unwrap())).unwrap();
  let res=self.api.handle(req).await;let status=res.status();let body=serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();(status,body)
 }
 async fn finish(self){drop(self.api);drop(self.runtime);self.actor.shutdown().await.unwrap();std::fs::remove_dir_all(self.dir).unwrap();}
}
#[tokio::test]
async fn credential_retry_reuses_issuance_record_and_does_not_redisclose_secret(){
 let l=Lab::new().await;let body=json!({"role":"Member","devices":["simulation:light"],"scenes":[]});
 let first=l.post("/api/v1/credentials","same-issuance",body.clone()).await;
 let repeat=l.post("/api/v1/credentials","same-issuance",body).await;
 let count=l.api.tokens.summaries().unwrap().len();
 let sql=rusqlite::Connection::open(l.dir.join("credentials.sqlite")).unwrap();
 let raw=sql.query_row("SELECT group_concat(grant_json) FROM web_credential",[],|r|r.get::<_,String>(0)).unwrap();drop(sql);
 let secret=first.1["secret"].as_str().map(str::to_owned);
 l.finish().await;
 assert_eq!(first.0,StatusCode::CREATED,"fixture first issuance not accepted: {}",first.1.get("error").unwrap_or(&Value::Null));
 assert!(secret.as_ref().is_some_and(|s|s.len()==64));assert!(!raw.contains(secret.as_ref().unwrap()),"original credential privacy must remain intact");
 assert_eq!(repeat.1["id"],first.1["id"],"same endpoint andkey must reuse issuance record");
 assert!(repeat.1.get("secret").is_none_or(|s|s.is_null()),"a duplicate must not disclose a secret again");
 assert_eq!(count,2,"one admin plus one Member; retry must not issue a second Member");
}
#[tokio::test]
async fn same_room_edit_endpoint_key_with_different_body_is_conflict_not_second_draft(){
 let l=Lab::new().await;let source=Uuid::new_v4();let mut devices=l.devices.clone();devices["revision"]=2.into();
 l.api.drafts.as_ref().unwrap().1.lock().unwrap().save(l.caller,&Draft{id:source,expected_revision:1,revision:2,devices,automation:json!({"schema_version":1,"configuration_revision":2,"variables":[],"rules":[]})},ExecutionContext::Simulation).unwrap();
 let endpoint=format!("/api/v1/config/drafts/{source}/room");
 let body=|id:Uuid,name:&str|json!({"id":id,"expected_revision":"1","revision":"3","room":{"id":"root-room","name":name},"remove":false});
 let first=l.post(&endpoint,"same-room-key",body(Uuid::new_v4(),"First")).await;
 let changed=l.post(&endpoint,"same-room-key",body(Uuid::new_v4(),"Second")).await;
 let count=l.api.drafts.as_ref().unwrap().1.lock().unwrap().list(l.caller).unwrap().len();l.finish().await;
 assert_eq!(first.0,StatusCode::OK,"valid first edit failed: {}",first.1);
 assert_eq!(changed.0,StatusCode::CONFLICT,"same endpoint andkey different content must conflict, actual {}",changed.1);
 assert_eq!(count,2,"original plus one derived draft, no conflicting second effect");
}

async fn reopen_same_databases(l:Lab)->Lab {
 let Lab{dir,api,actor,runtime,caller,secret,devices}=l;
 drop(api);drop(runtime);actor.shutdown().await.unwrap();
 let actor=Admission::open(dir.join("runtime.sqlite"),Options{context:ExecutionContext::Simulation,..Default::default()},move|id|id==caller).await.unwrap();actor.synchronized(1,1).await.unwrap();
 let runtime=AutomationRuntime::load_with_startup(actor.clone(),caller,1_800_000_000_000,1000,8,Default::default(),StartupPolicy::Suppress).await.unwrap();
 let tokens=Arc::new(Tokens::open(&dir.join("credentials.sqlite")).unwrap());
 let api=Api{daily_backup:Default::default(),hap_bridges:Default::default(),hap_status:Default::default(),backup_source:None,publisher:None,origin:"http://127.0.0.1:8088".into(),tokens,runtime:RwLock::new(Some(Runtime{control:runtime.control(),revision:1,boot:actor.boot_id().into(),scenes:Default::default()})),drafts:Some((ExecutionContext::Simulation,Mutex::new(Drafts::open(&dir.join("drafts.sqlite")).unwrap()))),receipts:Mutex::new(Receipts::open(&dir.join("web.sqlite")).unwrap())};
 Lab{dir,api,actor,runtime,caller,secret,devices}
}
#[tokio::test]
async fn draft_completed_record_survives_real_runtime_restart(){
 let l=Lab::new().await;let first_boot=l.actor.boot_id().to_owned();let id=Uuid::new_v4();let endpoint=format!("/api/v1/config/drafts/{id}");let mut devices=l.devices.clone();devices["revision"]=2.into();
 let body=json!({"expected_revision":1,"revision":2,"devices":devices,"automation":{"schema_version":1,"configuration_revision":2,"variables":[],"rules":[]}});
 let first=l.post(&endpoint,"restart-draft",body.clone()).await;
 let l=reopen_same_databases(l).await;let boot_changed=l.actor.boot_id()!=first_boot;
 let repeat=l.post(&endpoint,"restart-draft",body).await;
 let count=l.api.drafts.as_ref().unwrap().1.lock().unwrap().list(l.caller).unwrap().len();l.finish().await;
 assert!(boot_changed,"actual admission owner must have a new boot");
 assert_eq!(first.0,StatusCode::OK,"valid first draft save failed");
 assert_eq!(repeat.0,StatusCode::OK,"sameprincipal/endpoint/key/body/configuration must retrieve old execution after real restart, actual {}",repeat.1);
 assert_eq!(repeat.1,first.1,"original execution record must remain unchanged");assert_eq!(count,1);
}

#[tokio::test]
async fn issued_secret_is_never_persisted_in_receipt_or_database_files(){
 let l=Lab::new().await;let r=l.post("/api/v1/credentials","privacy-issuance",json!({"role":"Member","devices":["simulation:light"],"scenes":[]})).await;
 let secret=r.1["secret"].as_str().map(str::to_owned);
 let sql=rusqlite::Connection::open(l.dir.join("web.sqlite")).unwrap();
 let rows:Vec<String>=sql.prepare("select response from web_receipt_legacy where response is not null UNION ALL select response from web_receipt_scoped where response is not null").unwrap().query_map([],|r|r.get(0)).unwrap().map(Result::unwrap).collect();drop(sql);
 let mut persisted=vec![];
 for name in ["web.sqlite","web.sqlite-wal","credentials.sqlite","credentials.sqlite-wal"]{if let Ok(bytes)=std::fs::read(l.dir.join(name)){persisted.extend(bytes);}}
 l.finish().await;
 assert_eq!(r.0,StatusCode::CREATED);let secret=secret.unwrap();assert_eq!(secret.len(),64);
 assert_eq!(rows.len(),1,"actual management execution receipt must exist");
 assert!(rows.iter().all(|r|!r.contains(&secret)),"one-time credential secret leaked into logical receipt");
 assert!(!persisted.windows(secret.len()).any(|b|b==secret.as_bytes()),"one-time credential secret persisted in DB/WAL files");
}
