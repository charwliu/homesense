//! Real SQLite and a private clock; no connection or physical writer exists.
use super::*;
use homesense_core::{Command,DeviceId,LevelTransition,Percent,queue::QueuedCommand};

#[tokio::test]
async fn near_expiry_plain_lights_cancel_before_writing_and_leave_admission_ready() {
    for command in [Command::Switch(true),Command::SetLevel{level:Percent::new(40).unwrap(),transition:LevelTransition::Immediate}] {
        for remaining in [1,20] {
            let directory=std::env::temp_dir().join(format!("light-delivery-expiry-{}",Uuid::new_v4()));
            std::fs::create_dir(&directory).unwrap();let path=directory.join("runtime.sqlite");
            let profile=if matches!(command,Command::Switch(_)){"r8i8_din_pro_v2_7_7"}else{"ad8_din_rs485_v2_2_1"};
            let configuration=serde_json::to_vec(&serde_json::json!({"schema_version":1,"revision":1,"gateways":[{"id":"bus","endpoint":"127.0.0.1:1","modules":[{"address":"01","profile":profile,"verification":{"status":"simulated"}}]}],"devices":[{"id":"light","name":"synthetic","gateway":"bus","address":"01","channel":1,"enabled":true}]})).unwrap();
            Store::open(&path).unwrap().publish_configuration(None,&configuration,ExecutionContext::Simulation,&[]).unwrap();
            let time=Box::leak(Box::new(AtomicU64::new(0)));let controller=Uuid::new_v4();
            let actor=Admission::open_with_test_time(path.clone(),Options{context:ExecutionContext::Simulation,..Default::default()},move|id|id==controller,time).await.unwrap();
            actor.synchronized(1,1).await.unwrap();
            let batch=||Batch{controller,intents:vec![homesense_hap::requests::Intent{device:DeviceId::new("light").unwrap(),command}],subscriptions:vec![]};
            let ids=actor.submit(batch()).await.unwrap();time.store(2000-remaining,Ordering::Release);
            let ticket=actor.dispatch().await.unwrap();let delivered=ticket.is_some();
            // Also cleanly finish the old implementation's unsent ticket before
            // asserting, so the regression does not leak an admission worker.
            if let Some(ticket)=ticket {actor.finish_unsent(ticket.command.id).await.unwrap();}
            let status=Store::open(&path).unwrap().command_status(actor.boot_id(),ids[0]).unwrap();
            let ready=actor.task_generation().await.is_ok();
            let fresh=actor.submit(batch()).await.unwrap();let ticket=actor.dispatch().await.unwrap().unwrap();
            assert_eq!(ticket.command.id,fresh[0]);actor.finish_unsent(ticket.command.id).await.unwrap();
            actor.shutdown().await.unwrap();std::fs::remove_dir_all(directory).unwrap();
            assert!(!delivered,"{remaining}ms is insufficient for delivery and audit; no ticket should escape");
            assert_eq!(status,Some(Status::Cancelled));assert!(ready);
        }
    }
}

#[test]
fn expired_light_cancellation_requires_exclusive_undelivered_exact_intent() {
    let directory=std::env::temp_dir().join(format!("light-reservation-expiry-{}",Uuid::new_v4()));
    std::fs::create_dir(&directory).unwrap();let path=directory.join("runtime.sqlite");
    let mut store=Store::open(&path).unwrap();store.publish(None,1,b"config",&[]).unwrap();
    let mut id=0;
    for command in [Command::Switch(true),Command::SetLevel{level:Percent::new(40).unwrap(),transition:LevelTransition::Immediate},Command::Toggle,Command::Motor(homesense_core::MotorMotion::Stop)] {
        for expired in [false,true] {
            id+=1;let queued=QueuedCommand{id,device_id:DeviceId::new("light").unwrap(),command,origin:Origin::Hap,epochs:Epochs{transport:1,configuration:1,control:1},deadline_ms:100,run_id:None};
            store.accept_commands("boot1",&[queued],10).unwrap();
            let result=store.reserve_command_write("boot1",id,20,None).unwrap().unwrap().cancel_expired_light_before_delivery(command,if expired{100}else{99});
            let allowed=expired && matches!(command,Command::Switch(_)|Command::SetLevel{..}|Command::Toggle);
            assert_eq!(result.is_ok(),allowed);
            assert_eq!(store.command_status("boot1",id).unwrap(),Some(if allowed{Status::Cancelled}else{Status::Writing}));
            if !allowed {
                assert!(store.transition_command("boot1",id,Status::Writing,Status::Cancelled,100).is_err());
                assert!(store.reserve_command_write("boot1",id,100,None).is_err(),"consumed reservation cannot be recreated");
            }
        }
    }
    id+=1;let queued=QueuedCommand{id,device_id:DeviceId::new("light").unwrap(),command:Command::Switch(true),origin:Origin::Hap,epochs:Epochs{transport:1,configuration:1,control:1},deadline_ms:100,run_id:None};
    store.accept_commands("boot1",&[queued],10).unwrap();
    assert!(store.reserve_command_write("boot1",id,20,None).unwrap().unwrap().cancel_expired_light_before_delivery(Command::Switch(false),100).is_err());
    assert_eq!(store.command_status("boot1",id).unwrap(),Some(Status::Writing),"a different intent cannot cancel the reserved command");
    id+=1;let queued=QueuedCommand{id,device_id:DeviceId::new("light").unwrap(),command:Command::Switch(true),origin:Origin::Hap,epochs:Epochs{transport:1,configuration:1,control:1},deadline_ms:100,run_id:None};
    store.accept_commands("boot1",&[queued],10).unwrap();
    assert!(store.reserve_command_write("boot1",id,20,None).unwrap().unwrap().cancel_expired_light_before_delivery(Command::ModuleAllOff,100).is_err());
    assert_eq!(store.command_status("boot1",id).unwrap(),Some(Status::Writing),"module commands cannot enter the plain-light cancellation path");
    drop(store);std::fs::remove_dir_all(directory).unwrap();
}
