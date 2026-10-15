//! Semantic fade contracts with actual SQLite admission and a private test clock.
//! Other runtime tests continue to exercise real time and actual transport.
use crate as homesensed;
use crate::admission::{Admission, Options};
use homesense_core::ExecutionContext;
use homesense_storage::Store;
use std::{path::PathBuf, sync::atomic::{AtomicU64, Ordering}};
use uuid::Uuid;

struct Db(PathBuf);
impl Drop for Db {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let _=std::fs::remove_file(format!("{}{suffix}",self.0.display()));
        }
        let _=std::fs::remove_file(self.0.with_extension("runtime.lock"));
    }
}
async fn actor(db:&Db,id:Uuid,time:&'static AtomicU64)->Admission {
    let admission=Admission::open_with_test_time(db.0.clone(),Options {context:ExecutionContext::Simulation,..Default::default()},move |controller|controller==id,time).await.unwrap();
    admission.synchronized(1,1).await.unwrap();
    admission
}

#[tokio::test]
async fn dropping_unfinished_software_fade_closes_output_admission() {
    static TIME:AtomicU64=AtomicU64::new(0);
    TIME.store(0,Ordering::Release);
    use homesensed::{software_fade::SoftwareFade,feedback::Feedback,transport_owner::GatewayRead};
    let db=Db(std::env::temp_dir().join(format!("fade-drop-{}.sqlite",Uuid::new_v4())));
    let devices=include_bytes!("../../../config/simulation.json");
    Store::open(&db.0).unwrap().publish_configuration(None,devices,ExecutionContext::Simulation,&[]).unwrap();
    let controller=Uuid::new_v4();let a=actor(&db,controller,&TIME).await;
    let config=homesense_config::Configuration::parse(devices,ExecutionContext::Simulation).unwrap();
    let mut feedback=Feedback::new(&config,ExecutionContext::Simulation,Uuid::parse_str(a.boot_id()).unwrap().as_u128(),1).unwrap();
    let device=homesense_core::DeviceId::new("simulation:light").unwrap();
    let zero=homesense_core::Percent::new(0).unwrap();
    assert!(SoftwareFade::start(a.clone(),controller,device.clone(),zero,200,50,100,4,&feedback,2000).await.is_err());
    feedback.apply(&GatewayRead{gateway_id:"simulation".into(),transport_epoch:1,received_at_ms:0,result:hb_transport::Receive::Frames{transport_epoch:1,frames:vec![hb_protocol::RawFrame{address:0x15,command:0xd1,value:100}]}}).unwrap();
    let mut fade=SoftwareFade::start(a.clone(),controller,device,zero,200,50,100,4,&feedback,2000).await.unwrap();
    TIME.store(60,Ordering::Release);
    assert!(!fade.tick().await.unwrap());
    // No transport is driven: a second tick must not build a queue of samples.
    TIME.store(120,Ordering::Release);
    assert!(!fade.tick().await.unwrap());
    let sql=rusqlite::Connection::open(&db.0).unwrap();
    let (count,deadline):(i64,i64)=sql.query_row("SELECT count(*),max(deadline_ms-updated_ms) FROM command_journal",[],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
    assert_eq!(count,1);assert!((1..=300).contains(&deadline));
    drop(fade);
    assert!(a.task_generation().await.is_err(),"dropping unfinished work must close admission before any transport error");
    struct NoWrite;
    impl std::io::Read for NoWrite {fn read(&mut self,_:&mut [u8])->std::io::Result<usize>{Err(std::io::ErrorKind::WouldBlock.into())}}
    impl std::io::Write for NoWrite {
        fn write(&mut self,_:&[u8])->std::io::Result<usize>{panic!("abandoned fade reached transport")}
        fn flush(&mut self)->std::io::Result<()>{Ok(())}
    }
    let mut owner=homesensed::transport_owner::TransportOwner::new(a.clone(),[("simulation".into(),hb_transport::Connection::for_simulation(NoWrite,1))].into());
    assert!(owner.step().await.is_err());
    a.shutdown().await.unwrap();
    assert_eq!(sql.query_row("SELECT count(*) FROM command_journal WHERE status='written'",[],|r|r.get::<_,i64>(0)).unwrap(),0);
}


#[tokio::test]
async fn configured_fade_prepares_from_feedback_and_never_falls_back() {
    static TIME:AtomicU64=AtomicU64::new(0);
    for fresh in [false,true] {
    TIME.store(0,Ordering::Release);
    let db=Db(std::env::temp_dir().join(format!("fade-config-{}.sqlite",Uuid::new_v4())));
    let devices=include_bytes!("../../../config/simulation.json");
    Store::open(&db.0).unwrap().publish_configuration(None,devices,ExecutionContext::Simulation,&[]).unwrap();
    let controller=Uuid::new_v4();let a=actor(&db,controller,&TIME).await;
    let config=homesense_config::Configuration::parse(devices,ExecutionContext::Simulation).unwrap();
    let rules=br#"{"schema_version":1,"configuration_revision":1,"variables":[],"rules":[],"timed_scenes":[{"id":"fade","enabled":true,"review_evidence":"simulation","max_lateness_ms":1000,"steps":[{"source":"test","offset_ms":0,"device":"simulation:light","command":{"kind":"level","value":0,"ramp":false},"fade":{"duration_ms":200,"interval_ms":50,"lateness_ms":100}}]}]}"#;
    let document=homesense_config::rule_document::Document::parse(rules,&config).unwrap();
    let mut scheduler=homesensed::scene_scheduler::SceneScheduler::new(a.clone(),controller,4,0).unwrap();
    let run=scheduler.start_document(&document,"fade",0).await.unwrap();
    let mut feedback=homesensed::feedback::Feedback::new(&config,ExecutionContext::Simulation,Uuid::parse_str(a.boot_id()).unwrap().as_u128(),1).unwrap();
    if fresh {
        feedback.apply(&homesensed::transport_owner::GatewayRead{gateway_id:"simulation".into(),transport_epoch:1,received_at_ms:0,result:hb_transport::Receive::Frames{transport_epoch:1,frames:vec![hb_protocol::RawFrame{address:0x15,command:0xd1,value:100}]}}).unwrap();
    }
    let ticks=scheduler.tick_with_feedback(0,Some((&feedback,2000))).await.unwrap();
    assert_eq!(ticks.len(),1);
    assert_eq!(ticks[0].result.is_ok(),fresh);
    let mut written=0;
    if fresh {
        tokio::time::timeout(std::time::Duration::from_secs(2),async {
            while scheduler.status(run)==Some(homesense_core::scene::Status::Running) {
                for tick in scheduler.tick_with_feedback(TIME.load(Ordering::Acquire),Some((&feedback,2000))).await.unwrap() {tick.result.unwrap();}
                if let Some(ticket)=a.dispatch().await.unwrap() {
                    a.finish_write(ticket.command.id,true).await.unwrap();written+=1;
                }
                TIME.fetch_add(50,Ordering::AcqRel);
                tokio::task::yield_now().await;
            }
        }).await.unwrap();
        assert!((1..=4).contains(&written));
        assert_eq!(written,4,"all four scheduled samples must be written at exact test times");
        assert_eq!(scheduler.status(run),Some(homesense_core::scene::Status::CommandsWritten));
    } else {
        assert_eq!(scheduler.status(run),Some(homesense_core::scene::Status::Cancelled));
    }
    assert!(a.dispatch().await.unwrap().is_none());
    let sql=rusqlite::Connection::open(&db.0).unwrap();
    assert_eq!(sql.query_row("SELECT count(*) FROM command_journal",[],|r|r.get::<_,i64>(0)).unwrap(),written);
    assert_eq!(sql.query_row("SELECT count(*) FROM scene_sample",[],|r|r.get::<_,i64>(0)).unwrap(),written);
    a.shutdown().await.unwrap();
    }
}


#[tokio::test]
async fn expired_pending_fade_never_writes_and_retains_failure() {
    use homesensed::{software_fade::SoftwareFade,feedback::Feedback,transport_owner::GatewayRead};
    static TIME:AtomicU64=AtomicU64::new(0);
    for dispatch_before_tick in [false,true] {
        TIME.store(0,Ordering::Release);
        let db=Db(std::env::temp_dir().join(format!("fade-clock-expired-{}.sqlite",Uuid::new_v4())));
        let devices=include_bytes!("../../../config/simulation.json");
        Store::open(&db.0).unwrap().publish_configuration(None,devices,ExecutionContext::Simulation,&[]).unwrap();
        let controller=Uuid::new_v4();let a=actor(&db,controller,&TIME).await;
        let config=homesense_config::Configuration::parse(devices,ExecutionContext::Simulation).unwrap();
        let mut feedback=Feedback::new(&config,ExecutionContext::Simulation,Uuid::parse_str(a.boot_id()).unwrap().as_u128(),1).unwrap();
        feedback.apply(&GatewayRead{gateway_id:"simulation".into(),transport_epoch:1,received_at_ms:0,result:hb_transport::Receive::Frames{transport_epoch:1,frames:vec![hb_protocol::RawFrame{address:0x15,command:0xd1,value:100}]}}).unwrap();
        let mut fade=SoftwareFade::start(a.clone(),controller,homesense_core::DeviceId::new("simulation:light").unwrap(),homesense_core::Percent::new(0).unwrap(),200,50,100,4,&feedback,2000).await.unwrap();
        TIME.store(60,Ordering::Release);
        assert!(!fade.tick().await.unwrap());
        TIME.store(300,Ordering::Release);
        assert!(!fade.tick().await.unwrap(),"the configured deadline itself is not past the fade guard");
        TIME.store(301,Ordering::Release);
        if dispatch_before_tick {assert!(a.dispatch().await.unwrap().is_none());}
        let expected=if dispatch_before_tick {"fade output interrupted"} else {"fade output expired"};
        assert_eq!(fade.tick().await.unwrap_err(),expected);
        assert_eq!(fade.tick().await.unwrap_err(),expected,"failure must remain terminal");
        assert!(a.dispatch().await.unwrap().is_none());
        let sql=rusqlite::Connection::open(&db.0).unwrap();
        let (count,deadline,status,reason):(i64,i64,String,String)=sql.query_row("SELECT count(*),deadline_ms,status,cancellation_reason FROM command_journal",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).unwrap();
        assert_eq!(count,1);assert_eq!(deadline,300);assert_eq!(status,"cancelled");
        assert_eq!(reason,if dispatch_before_tick {"Expired"} else {"RunCancelled"});
        assert_eq!(sql.query_row("SELECT count(*) FROM command_journal WHERE status IN ('written','observed')",[],|r|r.get::<_,i64>(0)).unwrap(),0);
        a.shutdown().await.unwrap();
    }
}
