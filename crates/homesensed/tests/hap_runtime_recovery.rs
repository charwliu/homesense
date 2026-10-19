//! A real paired/encrypted controller across silent and partial gateway feedback.
//! Synthetic loopback AD8 only; no production identity or physical device access.
#[path="../../homesense-hap/tests/support/mod.rs"]
mod support;
use hap::storage::Storage;
use serde_json::{Value,json};
use std::{collections::BTreeSet,io::{Read,Write},net::{SocketAddr,TcpStream},sync::{Arc,Mutex,atomic::{AtomicU8,AtomicU64,Ordering}},time::{Duration,Instant}};
use tokio::io::{AsyncReadExt,AsyncWriteExt};
use uuid::Uuid;

const TOKEN:&str="abcdefghijklmnopqrstuvwxyz0123456789";
#[derive(Clone,Debug,serde::Serialize)]
struct Frame {connection:u64,phase:u8,raw:[u8;5]}
#[derive(Default)]
struct BusState {phase:AtomicU8,level:AtomicU8,connections:AtomicU64,partial_queries:AtomicU64,frames:Mutex<Vec<Frame>>}
#[derive(Clone)]
struct Trace(Arc<Mutex<Value>>);
impl Trace {
    fn stage(&self,stage:&str){self.0.lock().unwrap()["stage"]=stage.into();}
    fn event(&self,event:Value){
        let mut trace=self.0.lock().unwrap();
        if matches!(trace["stage"].as_str(),Some("unavailable-silent"|"partial-new-generation")) {
            assert!(event["characteristics"].as_array().unwrap().iter().all(|value|value["value"].is_null()),"known HAP notification during unavailable phase: {event}");
        }
        trace["events"].as_array_mut().unwrap().push(event);
    }
}
struct Report(Trace);
impl Drop for Report {
    fn drop(&mut self){
        if let Some(directory)=std::env::var_os("HAP_RECOVERY_ARTIFACT_ROOT")
            && let Ok(value)=self.0.0.lock(){
            let directory=std::path::PathBuf::from(directory);
            let _=std::fs::create_dir_all(&directory);
            let _=std::fs::write(directory.join("paired-hap-recovery.json"),format!("{value:#}\n"));
        }
    }
}
fn address()->SocketAddr{std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap()}
fn connect(address:SocketAddr)->TcpStream {
    let socket=TcpStream::connect_timeout(&address,Duration::from_secs(2)).unwrap();
    socket.set_read_timeout(Some(Duration::from_secs(8))).unwrap();socket.set_write_timeout(Some(Duration::from_secs(2))).unwrap();socket
}
fn management_result(address:SocketAddr,path:&str)->Result<(u16,Value),String> {
    let mut socket=TcpStream::connect_timeout(&address,Duration::from_millis(500)).map_err(|e|format!("connect: {e}"))?;
    socket.set_read_timeout(Some(Duration::from_secs(1))).map_err(|e|format!("timeout: {e}"))?;
    write!(socket,"GET {path} HTTP/1.1\r\nHost: {address}\r\nOrigin: http://{address}\r\nAuthorization: Bearer {TOKEN}\r\nConnection: close\r\n\r\n").map_err(|e|format!("request: {e}"))?;
    let mut bytes=Vec::new();socket.read_to_end(&mut bytes).map_err(|e|format!("response: {e}"))?;
    let text=String::from_utf8(bytes).map_err(|e|format!("response utf8: {e}"))?;let (headers,body)=text.split_once("\r\n\r\n").ok_or("missing HTTP header separator")?;
    let code=headers.split_whitespace().nth(1).ok_or("missing HTTP status")?.parse::<u16>().map_err(|e|e.to_string())?;
    Ok((code,serde_json::from_str(body).map_err(|e|format!("response JSON: {e}"))?))
}
fn management(address:SocketAddr,path:&str)->Option<(u16,Value)> {management_result(address,path).ok()}

fn wait(mut check:impl FnMut()->bool,seconds:u64,label:&str) {
    let deadline=Instant::now()+Duration::from_secs(seconds);
    while Instant::now()<deadline{if check(){return;}std::thread::sleep(Duration::from_millis(20));}
    panic!("timed out waiting for {label}");
}
fn exchange(session:&mut support::Session,socket:&mut TcpStream,method:&str,path:&str,body:&str,trace:&Trace)->(String,Value) {
    let request=format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/hap+json\r\nContent-Length: {}\r\n\r\n{body}",body.len());
    let mut first=true;
    for _ in 0..32 {
        let (headers,bytes)=session.request(socket,if first{request.as_bytes()}else{b""});first=false;
        let value=if bytes.is_empty(){Value::Null}else{serde_json::from_slice(&bytes).unwrap()};
        if headers.starts_with("EVENT/1.0"){trace.event(value);continue;}
        return (headers,value);
    }
    panic!("unbounded notification backlog before HTTP response");
}
fn graph(session:&mut support::Session,socket:&mut TcpStream,trace:&Trace)->(u64,u64,u64) {
    let (headers,graph)=exchange(session,socket,"GET","/accessories","",trace);assert!(headers.starts_with("HTTP/1.1 200"),"{headers}: {graph}");
    assert_eq!(graph["accessories"].as_array().unwrap().len(),2);
    let light=&graph["accessories"][1];let aid=light["aid"].as_u64().unwrap();
    let characteristics=light["services"].as_array().unwrap().iter().flat_map(|service|service["characteristics"].as_array().unwrap());
    let on=characteristics.clone().find(|c|c["format"]=="bool" && c["perms"].as_array().unwrap().iter().any(|p|p=="pw") && c["perms"].as_array().unwrap().iter().any(|p|p=="ev")).unwrap()["iid"].as_u64().unwrap();
    let brightness=characteristics.clone().find(|c|c["format"]=="int" && c["unit"]=="percentage").unwrap()["iid"].as_u64().unwrap();
    (aid,on,brightness)
}
fn read_level(session:&mut support::Session,socket:&mut TcpStream,ids:(u64,u64,u64),trace:&Trace)->(String,Value) {
    let (aid,on,brightness)=ids;
    exchange(session,socket,"GET",&format!("/characteristics?id={aid}.{on},{aid}.{brightness}"),"",trace)
}
fn write_level(session:&mut support::Session,socket:&mut TcpStream,ids:(u64,u64,u64),level:u8,trace:&Trace)->(String,Value) {
    exchange(session,socket,"PUT","/characteristics",&json!({"characteristics":[{"aid":ids.0,"iid":ids.2,"value":level}]}).to_string(),trace)
}
fn unavailable(session:&mut support::Session,socket:&mut TcpStream,ids:(u64,u64,u64),trace:&Trace) {
    let (headers,read)=read_level(session,socket,ids,trace);assert!(headers.starts_with("HTTP/1.1 207"),"{headers}: {read}");
    let values=read["characteristics"].as_array().unwrap();assert_eq!(values.len(),2);
    assert!(values.iter().all(|value|value["status"]==-70402 && value.get("value").is_none()),"{read}");
    let (headers,write)=write_level(session,socket,ids,88,trace);assert!(headers.starts_with("HTTP/1.1 207"),"{headers}: {write}");
    assert_eq!(write["characteristics"].as_array().unwrap().len(),1);assert_eq!(write["characteristics"][0]["status"],-70402);
}
fn controls(state:&BusState)->Vec<(u8,[u8;5])> {
    state.frames.lock().unwrap().iter().filter(|frame|frame.raw[2]!=0xa0).map(|frame|(frame.phase,frame.raw)).collect()
}
async fn gateway(listener:tokio::net::TcpListener,state:Arc<BusState>,mut stop:tokio::sync::oneshot::Receiver<()>){
    let mut workers=tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            accepted=listener.accept()=>{
                let (mut socket,peer)=accepted.unwrap();assert!(peer.ip().is_loopback());
                let state=state.clone();let connection=state.connections.fetch_add(1,Ordering::AcqRel)+1;
                workers.spawn(async move {
                    loop {
                        let mut raw=[0;5];if socket.read_exact(&mut raw).await.is_err(){break;}
                        let phase=state.phase.load(Ordering::Acquire);state.frames.lock().unwrap().push(Frame{connection,phase,raw});
                        assert_eq!((raw[0],raw[1],raw[4]),(0xae,0x11,0xee));
                        if raw[2]==0xa0 {
                            assert_eq!(raw[3],0xf2);
                            if phase==1 {continue;}
                            let channels=if phase==2 {state.partial_queries.fetch_add(1,Ordering::AcqRel);vec![1]}else{(1..=8).collect::<Vec<_>>()};
                            let level=state.level.load(Ordering::Acquire);
                            let reply=channels.into_iter().flat_map(|channel|[0xae,0x11,0xd0+channel,level,0xee]).collect::<Vec<_>>();
                            if socket.write_all(&reply).await.is_err(){break;}
                        }else{
                            assert_eq!(raw[2],0x01,"AD8 logical brightness must encode a channel1 direct percentage");
                            assert!(matches!(phase,0|3),"unavailable phase wrote control: {raw:?}");
                            assert!(matches!(raw[3],40|25),"unexpected or replayed control: {raw:?}");
                            state.level.store(raw[3],Ordering::Release);
                            if socket.write_all(&[0xae,0x11,0xd1,raw[3],0xee]).await.is_err(){break;}
                        }
                    }
                });
            },
            _=&mut stop=>break,
        }
    }
    tokio::time::timeout(Duration::from_secs(2),async{while let Some(result)=workers.join_next().await{result.unwrap();}}).await.expect("gateway streams must be released at service deadline");
}
#[tokio::test]
async fn paired_encrypted_hap_reports_outage_and_preserves_pairing_after_complete_recovery() {
    let root=std::env::temp_dir().join(format!("homesense-paired-recovery-{}",Uuid::new_v4()));std::fs::create_dir(&root).unwrap();
    let trace=Trace(Arc::new(Mutex::new(json!({"stage":"prepare","events":[],"passed":false,"synthetic_profile":"ad8_din_rs485_v2_2_1","field_access":false}))));let _report=Report(trace.clone());
    let web=address();let hap=address();assert_ne!(web,hap);
    let bus=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let devices=serde_json::to_vec(&json!({"schema_version":2,"revision":1,"gateways":[{"id":"bus","endpoint":bus.local_addr().unwrap().to_string(),"modules":[{"id":"dimmer","address":"11","profile":"ad8_din_rs485_v2_2_1","verification":{"status":"verified","verified_at_unix_ms":1,"evidence":"Generated loopback test, not field calibration"}}]}],"devices":[{"id":"light","name":"Synthetic light","module_id":"dimmer","channel":1,"enabled":true}]})).unwrap();
    // A silent peer can only be detected when a read is actually due. Keep the
    // production idle floor; this explicit test-only freshness consumer creates
    // the bounded read rather than relying on an obsolete 100ms idle sweep.
    // Its scene is never invoked, so the original two-control assertion stands.
    let rules=br#"{"schema_version":1,"configuration_revision":1,"variables":[],"rules":[],"timed_scenes":[{"id":"fault-observation","enabled":true,"review_evidence":"Synthetic read consumer only; never invoked","max_lateness_ms":1000,"steps":[{"source":"synthetic","offset_ms":0,"device":"light","command":{"kind":"level","value":30,"ramp":false}}]}],"scene_tracking":[{"scene":"fault-observation","enabled":true,"aggregation":"all","unknown_policy":"require_all_known","pending_policy":"block_scene","control_policy":"separate_action_and_read_only_activity","activity_name":"Synthetic fault observation","max_age_ms":1000,"review_evidence":"Explicit bounded read consumer triggers fault detection independently of the 300s idle sweep","members":[{"id":"light","device":"light","predicate":{"kind":"power","on":true},"review_evidence":"Synthetic single-port observation"}]}]}"#;
    let mut identity=homesense_hap::storage::IdentityStore::open(root.join("identity")).await.unwrap();
    let initial=hap::Config{host:hap.ip(),port:hap.port(),name:"Paired recovery fixture".into(),..Default::default()};
    identity.save_config(&initial).await.unwrap();identity.initialize_aid_cache().await.unwrap();
    // The identity store deliberately rejects pairing-only databases with no
    // semantic identifiers. Reproduce an already paired *published graph*, not
    // an orphan pairing which must fail closed before the listener can start.
    let allocated=homesense_hap::graph::LightGraph::build(&identity,&initial.name,vec![homesense_hap::graph::LightSpec{device:homesense_core::DeviceId::new("light").unwrap(),name:"Synthetic light".into(),dimmable:true,default_on_level:None}]).await.unwrap();
    let binding=&allocated.bindings[0];let expected_graph_ids=(binding.aid,binding.on_iid,binding.brightness_iid.unwrap());
    trace.0.lock().unwrap()["initial_graph_ids"]=json!(expected_graph_ids);drop(allocated);
    let controller=Uuid::new_v4();let signing=ed25519_dalek::SigningKey::from_bytes(&[63;32]);
    identity.save_pairing(&hap::pairing::Pairing::new(controller,hap::pairing::Permissions::Admin,signing.verifying_key().to_bytes())).await.unwrap();
    let state=Arc::new(BusState::default());state.level.store(30,Ordering::Release);
    let (stop,receive)=tokio::sync::oneshot::channel();let bus_task=tokio::spawn(gateway(bus,state.clone(),receive));
    let directory=root.clone();let mut service=tokio::spawn(async move {
        homesensed::automation_tcp::serve_combined(&devices,rules,&directory,Duration::from_secs(24),true,2,Duration::from_millis(20),homesensed::service::WebSettings{address:web,secret:TOKEN.into(),administrator:true,tls:None},homesensed::hap_endpoint::Settings{address:hap,scenes:vec![],motors:vec![]}).await
    });
    trace.stage("initial-listener-readiness");
    let readiness_trace=trace.clone();let readiness_state=state.clone();
    let mut readiness=tokio::task::spawn_blocking(move||wait(||{
        let result=management_result(web,"/health/ready");
        let ready=matches!(&result,Ok((200,body)) if body["ready"]==true);
        let mut trace=readiness_trace.0.lock().unwrap();
        trace["initial_ready_response"]=match result{Ok((status,body))=>json!({"status":status,"body":body}),Err(error)=>json!({"error":error})};
        trace["initial_bus_frames"]=json!(readiness_state.frames.lock().unwrap().clone());
        ready
    },4,"initial listener ready"));
    tokio::select! {
        ready=&mut readiness=>ready.expect("initial readiness helper failed; inspect last HTTP response/bus frames"),
        outcome=&mut service=>{
            trace.0.lock().unwrap()["service_ended_before_ready"]=format!("{outcome:?}").into();
            panic!("service ended before initial readiness: {outcome:?}");
        },
    }
    let initial_configuration_number=identity.load_config().await.unwrap().configuration_number;
    let initial_id=initial.device_id.to_string();let initial_key=initial.device_ed25519_keypair.verifying_key();
    let observed=state.clone();let observed_trace=trace.clone();
    let client=tokio::task::spawn_blocking(move|| {
        let trace=observed_trace;trace.stage("initial-readiness");
        wait(||management(web,"/health/ready").is_some_and(|(status,body)|status==200 && body["ready"]==true),4,"initial complete8port truth");
        let before=management(web,"/api/v1/diagnostics").unwrap().1["transport_epoch"].as_str().unwrap().parse::<u64>().unwrap();
        let mut socket=connect(hap);let mut session=support::verify(&mut socket,controller,&signing,&initial_id,&initial_key);
        let ids=graph(&mut session,&mut socket,&trace);assert_eq!(ids,expected_graph_ids,"published graph must reuse the already paired semantic IDs");let (headers,read)=read_level(&mut session,&mut socket,ids,&trace);assert!(headers.starts_with("HTTP/1.1 200"));assert_eq!(read["characteristics"][0]["value"],true);assert_eq!(read["characteristics"][1]["value"],30);
        let subscribe=json!({"characteristics":[{"aid":ids.0,"iid":ids.1,"ev":true},{"aid":ids.0,"iid":ids.2,"ev":true}]}).to_string();
        let (headers,body)=exchange(&mut session,&mut socket,"PUT","/characteristics",&subscribe,&trace);assert!(headers.starts_with("HTTP/1.1 204"),"{headers}: {body}");
        trace.stage("initial-authorized-write");let (headers,body)=write_level(&mut session,&mut socket,ids,40,&trace);assert!(headers.starts_with("HTTP/1.1 204"),"{headers}: {body}");
        wait(||controls(&observed)==vec![(0,[0xae,0x11,0x01,40,0xee])],3,"one initial authorized control");
        wait(||{let (headers,body)=read_level(&mut session,&mut socket,ids,&trace);headers.starts_with("HTTP/1.1 200") && body["characteristics"][1]["value"]==40},3,"confirmed level40");
        trace.stage("silent-gateway");observed.phase.store(1,Ordering::Release);
        let mut unavailable_iids=BTreeSet::new();
        while unavailable_iids.len()<2 {
            let (header,bytes)=session.request(&mut socket,b"");assert!(header.starts_with("EVENT/1.0"),"{header}");let event:Value=serde_json::from_slice(&bytes).unwrap();trace.event(event.clone());
            for characteristic in event["characteristics"].as_array().unwrap(){
                assert_eq!(characteristic["aid"],ids.0);let iid=characteristic["iid"].as_u64().unwrap();assert!([ids.1,ids.2].contains(&iid));
                if characteristic["value"].is_null(){assert!(unavailable_iids.insert(iid),"duplicate unavailable notification");}else{assert!(unavailable_iids.is_empty(),"stale state became known again before recovery");}
            }
        }
        // Expiring a sample does not prove a failed transport. Require the
        // actual bounded consumer read to fail and close the transport gate
        // before asserting that an explicit write is rejected as offline.
        wait(||management(web,"/api/v1/diagnostics").is_some_and(|(status,body)|status==200 && body["gateway_ready"]==false && body["ready"]==false),8,"actual silent gateway fault, not only expired observations");
        trace.stage("unavailable-silent");unavailable(&mut session,&mut socket,ids,&trace);
        assert!(management(web,"/health/live").is_some_and(|(status,body)|status==200 && body["live"]==true));
        assert!(management(web,"/health/ready").is_some_and(|(status,body)|status==503 && body["ready"]==false));
        wait(||observed.connections.load(Ordering::Acquire)>=2,3,"new recovery connection while silent");
        assert_eq!(controls(&observed),vec![(0,[0xae,0x11,0x01,40,0xee])]);
        trace.stage("partial-new-generation");observed.phase.store(2,Ordering::Release);
        wait(||observed.partial_queries.load(Ordering::Acquire)>0,6,"bound-channel-only reply");
        std::thread::sleep(Duration::from_millis(700));unavailable(&mut session,&mut socket,ids,&trace);
        assert!(management(web,"/health/ready").is_some_and(|(status,body)|status==503 && body["ready"]==false),"one fresh bound channel must not authorize missing7registered ports");
        assert_eq!(controls(&observed),vec![(0,[0xae,0x11,0x01,40,0xee])]);
        trace.stage("complete-new-generation");observed.phase.store(3,Ordering::Release);
        wait(||management(web,"/health/ready").is_some_and(|(status,body)|status==200 && body["ready"]==true),8,"complete8port recovery");
        let after=management(web,"/api/v1/diagnostics").unwrap().1["transport_epoch"].as_str().unwrap().parse::<u64>().unwrap();assert!(after>before);
        // Rebinding the runtime may retire old encrypted sessions. Authenticate
        // again with the existing long-term pair; never run Pair-Setup again.
        drop(socket);let mut socket=connect(hap);let mut session=support::verify(&mut socket,controller,&signing,&initial_id,&initial_key);
        assert_eq!(graph(&mut session,&mut socket,&trace),ids,"AID/IID changed across recovery");
        let (headers,body)=exchange(&mut session,&mut socket,"PUT","/characteristics",&subscribe,&trace);assert!(headers.starts_with("HTTP/1.1 204"),"recovered event subscription failed {headers}: {body}");
        let (headers,read)=read_level(&mut session,&mut socket,ids,&trace);assert!(headers.starts_with("HTTP/1.1 200"));assert_eq!(read["characteristics"][0]["value"],true);assert_eq!(read["characteristics"][1]["value"],40,"new truth should restore unchanged physical level");
        assert_eq!(controls(&observed),vec![(0,[0xae,0x11,0x01,40,0xee])],"old command replayed during recovery");
        trace.stage("fresh-post-recovery-write");let event_start=trace.0.lock().unwrap()["events"].as_array().unwrap().len();let (headers,body)=write_level(&mut session,&mut socket,ids,25,&trace);assert!(headers.starts_with("HTTP/1.1 204"),"{headers}: {body}");
        wait(||controls(&observed)==vec![(0,[0xae,0x11,0x01,40,0xee]),(3,[0xae,0x11,0x01,25,0xee])],3,"new authenticated command exactly once");
        // A completed local write is not a physical observation. Wait for both
        // the encrypted projection and current physical module feedback.
        wait(||{
            let (headers,read)=read_level(&mut session,&mut socket,ids,&trace);
            headers.starts_with("HTTP/1.1 200") && read["characteristics"][1]["value"]==25
                && trace.0.lock().unwrap()["events"].as_array().unwrap().iter().skip(event_start).any(|event|event["characteristics"].as_array().unwrap().iter().any(|value|value["aid"]==ids.0 && value["iid"]==ids.2 && value["value"]==25))
                && management(web,"/api/v1/modules").is_some_and(|(status,page)|status==200 && page["ready"]==true && page["modules"][0]["slots"][0]["quality"]=="confirmed" && page["modules"][0]["slots"][0]["actual"]["level"]==25)
        },3,"fresh post-write physical25 and encrypted projection");
        trace.0.lock().unwrap()["protocol"]=json!({"pair_verify_sessions":2,"no_pair_setup_during_recovery":true,"recovered_event_subscription":true,"fresh_target_event_confirmed":true,"stable_aid":ids.0,"stable_on_iid":ids.1,"stable_brightness_iid":ids.2,"initial_transport_epoch":before,"recovered_transport_epoch":after,"unavailable_characteristics":unavailable_iids});
    });
    tokio::time::timeout(Duration::from_secs(23),client).await.expect("paired HAP regression exceeded bounded service window").unwrap();
    trace.stage("deadline-and-pairing-check");service.await.unwrap().unwrap();let _=stop.send(());bus_task.await.unwrap();
    let after=identity.load_config().await.unwrap();assert_eq!(after.device_id,initial.device_id);assert!(after.device_ed25519_keypair.to_bytes()==initial.device_ed25519_keypair.to_bytes(),"long-term accessory identity changed");assert_eq!(after.configuration_number,initial_configuration_number);
    assert_eq!(identity.count_pairings().await.unwrap(),1);let pairing=identity.load_pairing(&controller).await.unwrap();assert_eq!(pairing.public_key,signing_key());assert_eq!(pairing.permissions,hap::pairing::Permissions::Admin);
    assert_eq!(controls(&state),vec![(0,[0xae,0x11,0x01,40,0xee]),(3,[0xae,0x11,0x01,25,0xee])]);
    let db=rusqlite::Connection::open(root.join("automation-tcp.sqlite")).unwrap();assert_eq!(db.query_row("SELECT count(*) FROM command_journal WHERE status='written'",[],|row|row.get::<_,i64>(0)).unwrap(),2);assert_eq!(db.query_row("SELECT count(*) FROM command_journal WHERE status IN ('accepted','writing')",[],|row|row.get::<_,i64>(0)).unwrap(),0);drop(db);
    {let mut report=trace.0.lock().unwrap();report["stage"]="complete".into();report["passed"]=true.into();report["frames"]=json!(state.frames.lock().unwrap().clone());report["pairing_and_identity_unchanged"]=true.into();}
    drop(identity);std::fs::remove_dir_all(root).unwrap();
}
fn signing_key()->[u8;32]{ed25519_dalek::SigningKey::from_bytes(&[63;32]).verifying_key().to_bytes()}
