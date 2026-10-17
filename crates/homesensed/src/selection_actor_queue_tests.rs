//! Real Admission FIFO interleaving; unchanged production Web 500ms helper.
use super::*;
use crate::{admission::{Admission,Options,SelectionTestRead},automation_control::{channel,Control}};
use homesense_core::ExecutionContext;
use homesense_storage::Store;
use std::{future::Future,sync::{Arc,Mutex,Condvar,OnceLock,atomic::{AtomicBool,AtomicUsize,Ordering}},task::Poll,time::{Duration,Instant}};

#[derive(Clone,Debug)]
struct Window {index:usize,start:Duration,end:Duration}
struct Cleanup {admission:Option<Admission>,first:Arc<(Mutex<bool>,Condvar)>}
impl Drop for Cleanup {
    fn drop(&mut self) {
        let(lock,wake)=&*self.first;if let Ok(mut released)=lock.lock(){*released=true;wake.notify_all();}
        if let Some(actor)=self.admission.take() {
            // A panic must not leave the real actor holding its runtime lease.
            // The actor reads are finite and the initial Condvar is now open.
            let _=std::thread::spawn(move||tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(actor.shutdown())).join();
        }
    }
}
async fn interleaved_read(nonempty:bool,initial_hold_ms:u64,expect_timeout:bool) {
    let directory=std::env::temp_dir().join(format!("selection-actor-interleave-{}",uuid::Uuid::new_v4()));std::fs::create_dir(&directory).unwrap();let path=directory.join("runtime.sqlite");
    let (devices,rules):(&[u8],&[u8])=if nonempty {
        (include_bytes!("../../../config/test/native-app-keypad-away/devices.template.json"),include_bytes!("../../../config/test/native-app-keypad-away/automation.json"))
    }else{(include_bytes!("../../../config/simulation.json"),br#"{"schema_version":1,"configuration_revision":1,"variables":[],"rules":[]}"#)};
    let bundle=Store::open(&path).unwrap().publish_automation(None,devices,rules,ExecutionContext::Simulation,&[]).unwrap();let document=bundle.automation.unwrap();
    let caller=uuid::Uuid::new_v4();let blockers=[uuid::Uuid::new_v4(),uuid::Uuid::new_v4(),uuid::Uuid::new_v4()];
    let actor=Arc::new(OnceLock::<Admission>::new());let generation=Arc::new(OnceLock::<crate::admission::TaskGeneration>::new());
    let armed=Arc::new(AtomicBool::new(false));let checks=Arc::new(AtomicUsize::new(0));let entered=Arc::new(tokio::sync::Notify::new());
    let first=Arc::new((Mutex::new(false),Condvar::new()));let began=Arc::new(OnceLock::<Instant>::new());began.set(Instant::now()).unwrap();let windows=Arc::new(Mutex::new(Vec::<Window>::new()));let receivers=Arc::new(Mutex::new(Vec::<SelectionTestRead>::new()));
    let(a,g,on,count,ready,gate,start,holds,replies)=(actor.clone(),generation.clone(),armed.clone(),checks.clone(),entered.clone(),first.clone(),began.clone(),windows.clone(),receivers.clone());
    let admission=Admission::open(path.clone(),Options{context:ExecutionContext::Simulation,..Default::default()},move|id|{
        if let Some(index)=blockers.iter().position(|blocker|*blocker==id) {
            if index==0 {
                ready.notify_one();let(lock,wake)=&*gate;let mut released=lock.lock().unwrap();while !*released {released=wake.wait(released).unwrap();}
            }
            let started=start.get().unwrap().elapsed();std::thread::sleep(Duration::from_millis(if index==0 {initial_hold_ms}else{180}));let end=start.get().unwrap().elapsed();
            let mut holds=holds.lock().unwrap();assert!(holds.len()<3);holds.push(Window{index,start:started,end});
            return true;
        }
        if id!=caller {return false;}
        if on.load(Ordering::Acquire) {
            let number=count.fetch_add(1,Ordering::AcqRel);
            // Queue the next unrelated read BEFORE replying to the current
            // selection RPC. Its own authorizer is slow; this handler is fast.
            if number<2 && !expect_timeout {let read=a.get().unwrap().enqueue_selection_test_read(blockers[number+1],g.get().unwrap().clone());let mut replies=replies.lock().unwrap();assert!(replies.len()<3);replies.push(read);}
        }
        true
    }).await.unwrap();
    let mut cleanup=Cleanup{admission:Some(admission.clone()),first:first.clone()};
    admission.synchronized(1,1).await.unwrap();let scope=admission.task_generation().await.unwrap();generation.set(scope.clone()).unwrap();assert!(actor.set(admission.clone()).is_ok());
    let(control,mut receiver):(Control,_)=channel(caller,scope.clone());let(_stop,receive)=tokio::sync::watch::channel(false);receiver.bind_selection_reader(&admission,&document,receive).await.unwrap();
    // No runtime/transport owner is constructed: all queued work is read-only.
    armed.store(true,Ordering::Release);receivers.lock().unwrap().push(admission.enqueue_selection_test_read(blockers[0],scope));
    tokio::time::timeout(Duration::from_secs(2),entered.notified()).await.unwrap();
    let mut read=Box::pin(selection_details(&control,caller,1));let read_started=Instant::now();
    // Poll once while the first actor message is held. This starts the actual
    // helper's existing 500ms timer and queues its base read before releasing
    // exactly180ms of actor work. No scheduler delay can shorten that window.
    std::future::poll_fn(|cx|{assert!(read.as_mut().poll(cx).is_pending());let(lock,wake)=&*first;*lock.lock().unwrap()=true;wake.notify_all();Poll::Ready(())}).await;
    let result=read.await;let read_elapsed=read_started.elapsed();
    // A normal actor read flushes the finite remaining fixture work; no retry
    // of selection and no canceled control command can enter this sequence.
    admission.task_generation().await.unwrap();
    let mut held=windows.lock().unwrap().clone();held.sort_by_key(|w|w.index);assert_eq!(held.len(),if expect_timeout {1}else{3});assert_eq!(held.iter().map(|w|w.index).collect::<Vec<_>>(),if expect_timeout {vec![0]}else{vec![0,1,2]});
    for window in &held {assert!(window.end-window.start>=Duration::from_millis(if window.index==0 {initial_hold_ms}else{180}));}
    for pair in held.windows(2) {assert!(pair[1].start>=pair[0].end);assert!(pair[1].start-pair[0].end<Duration::from_millis(100),"actual read handler+actor preamble must be fast: {pair:?}");}
    let held_total=held.iter().map(|w|w.end-w.start).sum::<Duration>();
    receiver.close();admission.shutdown().await.unwrap();cleanup.admission=None;
    let replies=std::mem::take(&mut *receivers.lock().unwrap());
    for reply in replies {assert!(reply.await.unwrap().is_ok());}
    let sql=rusqlite::Connection::open(&path).unwrap();let mut counts=vec![];for table in ["command_journal","scene_run","rule_admission"] {let count=sql.query_row(&format!("SELECT count(*) FROM {table}"),[],|r|r.get::<_,i64>(0)).unwrap();assert_eq!(count,0);counts.push((table,count));}
    drop(sql);std::fs::remove_dir_all(directory).unwrap();
    eprintln!("actor_interleave nonempty={nonempty} initial_ms={initial_hold_ms} read_ms={} held_ms={} authorizations={} windows={held:?} journals={counts:?} no_transport_constructed=true result={result:?}",read_elapsed.as_millis(),held_total.as_millis(),checks.load(Ordering::Acquire));
    if expect_timeout {assert!(matches!(result,Err(crate::scene_switches::UnavailableReason::ReadTimedOut)),"singleinitialstall must still failclosed");}
    else {let values=result.expect("three separate legitimate mailbox waits must not exhaust the unchanged500ms Web read");assert_eq!(values.is_empty(),!nonempty);}
}
#[tokio::test]
async fn empty_selection_read_avoids_cumulative_actor_mailbox_waits(){interleaved_read(false,180,false).await;}
#[tokio::test]
async fn nonempty_selection_read_avoids_cumulative_actor_mailbox_waits(){interleaved_read(true,180,false).await;}
#[tokio::test]
async fn selection_read_single_initial_actor_stall_still_exhausts_original_500ms(){interleaved_read(false,600,true).await;}
