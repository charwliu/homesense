//! No unsolicited relay feedback: write the evaluated cohort, then read modules.
use super::*;
use crate::admission::SceneWriteMode;
use homesense_core::{Channel,Command,DeviceId};
async fn fixture()->Harness {
    let(cfg,doc)=writer_batch_inputs();
    Harness::from_inputs_mode(cfg,doc,None,SceneWriteMode::BatchQuery).await
}
async fn execute(h:&mut Harness) {
    h.request(Action::SceneAction{controller:h.controller,revision:1,scene:"act".into(),execute:true}).await.unwrap();
}
#[tokio::test]
async fn batch_query_collects_all_modules_without_feedback_but_leds_need_actual_final_targets() {
    let mut h=fixture().await;
    mode_frame(&mut h,3,1,0xf1);mode_frame(&mut h,3,2,0xf1);mode_frame(&mut h,0x15,1,40);
    execute(&mut h).await;
    assert!(h.actor.scene_writes_pending("bus"));
    assert!(h.actor.control_batch_views(h.controller,h.generation.clone()).await.unwrap().is_empty());
    assert!(h.actor.reserve_physical_read("bus".into(),h.actor.monotonic_ms()+500).await.is_err());
    let(mut owner,calls)=batch_owner(&h,usize::MAX);
    let outcome=owner.step().await.unwrap().unwrap();outcome.result.unwrap();assert_eq!(outcome.completed.len(),4);
    assert_eq!(calls.lock().unwrap()[0].len(),20);
    assert!(!h.actor.scene_writes_pending("bus"));
    assert!(!owner.query_response_pending("bus"),"no receive window for disabled unsolicited feedback");
    assert!(owner.step().await.unwrap().is_none());
    crate::control_batch::tick(&h.actor,h.controller,&h.feedback).await.unwrap();
    assert!(owner.step().await.unwrap().is_none(),"a matching prewrite cache is not physical confirmation");
    tokio::time::sleep(Duration::from_millis(3)).await;
    mode_frame(&mut h,3,1,0xf1);mode_frame(&mut h,3,2,0xf1);mode_frame(&mut h,0x15,1,20);
    crate::control_batch::tick(&h.actor,h.controller,&h.feedback).await.unwrap();
    assert!(owner.step().await.unwrap().is_none(),"intermediate level must not release LEDs");
    tokio::time::sleep(Duration::from_millis(20)).await;mode_frame(&mut h,0x15,1,40);
    crate::control_batch::tick(&h.actor,h.controller,&h.feedback).await.unwrap();
    let outcome=owner.step().await.unwrap().unwrap();outcome.result.unwrap();assert_eq!(outcome.completed.len(),3);
    assert_eq!(calls.lock().unwrap().iter().map(Vec::len).collect::<Vec<_>>(),vec![20,15]);
    h.finish().await;
}
#[tokio::test]
async fn batch_query_inflight_read_drains_before_controls_new_reads_cannot_split_cohort() {
    let mut h=fixture().await;
    let permit=h.actor.reserve_physical_read("bus".into(),h.actor.monotonic_ms()+500).await.unwrap();
    execute(&mut h).await;
    let(mut owner,calls)=batch_owner(&h,usize::MAX);
    assert!(owner.step().await.unwrap().is_none());assert!(calls.lock().unwrap().is_empty());
    drop(permit);
    owner.step().await.unwrap().unwrap().result.unwrap();
    assert_eq!(calls.lock().unwrap()[0].len(),20);
    assert!(h.actor.reserve_physical_read("bus".into(),h.actor.monotonic_ms()+500).await.is_ok());
    h.finish().await;
}
#[tokio::test]
async fn batch_query_partial_write_keeps_exact_prefix_and_never_replays() {
    for amount in [0,5,7,10,14,15,19] {
        let mut h=fixture().await;execute(&mut h).await;
        let(mut owner,calls)=batch_owner(&h,amount);
        assert_eq!(owner.step().await.unwrap().unwrap().result.unwrap_err().bytes_written,amount);
        let store=Store::open(h.directory.join("runtime.sqlite")).unwrap();
        for index in 0..4 {
            let expected=if amount>=(index+1)*5 {homesense_storage::journal::Status::Written}
                else if amount>index*5 {homesense_storage::journal::Status::Uncertain}
                else {homesense_storage::journal::Status::Failed};
            assert_eq!(store.command_status(h.actor.boot_id(),index as u64+1).unwrap(),Some(expected));
        }
        for id in 5..=7 {assert_eq!(store.command_status(h.actor.boot_id(),id).unwrap(),Some(homesense_storage::journal::Status::Cancelled));}
        assert!(owner.step().await.is_err());assert_eq!(calls.lock().unwrap().iter().map(Vec::len).sum::<usize>(),amount);
        drop(store);h.finish().await;
    }
}
#[tokio::test]
async fn batch_query_split_frame_budget_defers_queries_until_last_original_control() {
    let(mut cfg,mut doc)=writer_batch_inputs();let mut data:Json=serde_json::from_slice(&cfg).unwrap();
    data["gateways"][0]["modules"].as_array_mut().unwrap().push(json!({"id":"extra","address":"04","profile":"r8i8_din_pro_v2_7_7","verification":{"status":"simulated"}}));
    data["devices"].as_array_mut().unwrap().push(json!({"id":"extra-light","name":"Extra","module_id":"extra","channel":1,"enabled":true}));
    cfg=serde_json::to_vec(&data).unwrap();
    doc["immediate_scenes"][0]["activate"]=json!((0..40).map(|n|command(&format!("batch/{n}"),if n%2==0{"relay"}else{"extra-light"},json!({"kind":"switch","on":true}))).collect::<Vec<_>>());
    let mut h=Harness::from_inputs_mode(cfg,doc,None,SceneWriteMode::BatchQuery).await;execute(&mut h).await;
    let(mut owner,calls)=batch_owner(&h,usize::MAX);
    let first=owner.step().await.unwrap().unwrap();first.result.unwrap();assert_eq!(first.completed.len(),hb_transport::MAX_BATCH_FRAMES);
    assert!(h.actor.scene_writes_pending("bus"));
    assert!(h.actor.reserve_physical_read("bus".into(),h.actor.monotonic_ms()+500).await.is_err());
    assert!(h.actor.control_batch_views(h.controller,h.generation.clone()).await.unwrap().is_empty());
    let second=owner.step().await.unwrap().unwrap();second.result.unwrap();assert_eq!(second.completed.len(),8);
    assert!(!h.actor.scene_writes_pending("bus"));
    assert_eq!(calls.lock().unwrap().iter().map(Vec::len).collect::<Vec<_>>(),vec![160,40]);
    h.finish().await;
}
#[tokio::test]
async fn batch_query_manual_override_cancels_unsent_tail_and_feedback_leds() {
    let mut h=fixture().await;execute(&mut h).await;
    let ticket=h.actor.dispatch().await.unwrap().unwrap();let id=ticket.command.id;
    h.actor.finish_write(id,true).await.unwrap();
    h.actor.submit(homesense_hap::requests::Batch{controller:h.controller,subscriptions:vec![],intents:vec![homesense_hap::requests::Intent{device:DeviceId::new("light").unwrap(),command:Command::SetLevel{level:homesense_core::Percent::new(0).unwrap(),transition:homesense_core::LevelTransition::Immediate}}]}).await.unwrap();
    let(mut owner,calls)=batch_owner(&h,usize::MAX);owner.step().await.unwrap().unwrap().result.unwrap();
    assert_eq!(calls.lock().unwrap().as_slice(),&[vec![0xae,0x15,1,0,0xee]]);
    let store=Store::open(h.directory.join("runtime.sqlite")).unwrap();
    assert_eq!(store.command_status(h.actor.boot_id(),id).unwrap(),Some(homesense_storage::journal::Status::Written));
    for n in 2..=7 {assert_eq!(store.command_status(h.actor.boot_id(),n).unwrap(),Some(homesense_storage::journal::Status::Cancelled));}
    drop(store);h.finish().await;
}
// The bus applies controls silently. Only explicit queries reply, after 3–20ms.
struct QuietModules {calls:BatchWrites,levels:std::collections::BTreeMap<(u8,u8),u8>,pending:std::collections::VecDeque<(tokio::time::Instant,Vec<u8>)>,delay:u64}
impl std::io::Read for QuietModules {fn read(&mut self,out:&mut[u8])->std::io::Result<usize>{
    if self.pending.front().is_none_or(|(at,_)|tokio::time::Instant::now()<*at) {return Err(std::io::ErrorKind::WouldBlock.into());}
    let(_,bytes)=self.pending.pop_front().unwrap();out[..bytes.len()].copy_from_slice(&bytes);Ok(bytes.len())
}}
impl std::io::Write for QuietModules {fn write(&mut self,bytes:&[u8])->std::io::Result<usize>{
    self.calls.lock().unwrap().push(bytes.to_vec());
    for f in bytes.as_chunks::<5>().0 {
        if (1..=8).contains(&f[2]) {self.levels.insert((f[1],f[2]),f[3]);}
        else if (0xa0..=0xa8).contains(&f[2]) {
            assert!(f[1]!=3 || f[2]!=0xa0,"R818 PRO has no whole-module query");
            let channels:Vec<u8>=if f[2]==0xa0{(1..=8).collect()}else{vec![f[2]-0xa0]};
            let response=channels.into_iter().flat_map(|ch|[0xae,f[1],0xd0+ch,*self.levels.get(&(f[1],ch)).unwrap_or(&0),0xee]).collect();
            self.pending.push_back((tokio::time::Instant::now()+Duration::from_millis(self.delay),response));
        }
    }Ok(bytes.len())
}fn flush(&mut self)->std::io::Result<()>{Ok(())}}
#[tokio::test]
async fn batch_query_silent_modules_are_confirmed_only_by_explicit_profile_specific_reads() {
    for delay in [3,20] {
        let mut h=fixture().await;execute(&mut h).await;
        let calls:BatchWrites=Arc::default();
        let mut owner=TransportOwner::new(h.actor.clone(),[("bus".into(),hb_transport::Connection::for_simulation(QuietModules{calls:calls.clone(),levels:Default::default(),pending:Default::default(),delay},1))].into());
        owner.step().await.unwrap().unwrap().result.unwrap();
        assert!(matches!(owner.receive("bus").await.unwrap().result,hb_transport::Receive::Idle));
        for (module,whole,channel) in [("relay",false,1),("relay",false,2),("dimmer",true,1)] {
            let actual=if module=="dimmer" {h.configuration.binding(&DeviceId::new("light").unwrap()).unwrap().1.clone()}else{h.configuration.binding(&DeviceId::new("relay").unwrap()).unwrap().1.clone()};
            owner.query_status_scope("bus",&actual,Channel::new(channel).unwrap(),ExecutionContext::Simulation,500,whole).await.unwrap();
            tokio::time::sleep(Duration::from_millis(delay)).await;
            let read=owner.receive("bus").await.unwrap();h.feedback.apply(&read).unwrap();owner.finish_status_read("bus");owner.finish_reply_turnaround().await;
            crate::control_batch::tick(&h.actor,h.controller,&h.feedback).await.unwrap();
        }
        owner.step().await.unwrap().unwrap().result.unwrap();
        {let writes=calls.lock().unwrap();assert_eq!(writes.len(),5);assert_eq!(writes[0].len(),20);assert_eq!(writes[4].len(),15);
        assert_eq!(&writes[1..4],&[vec![0xae,3,0xa1,0xf2,0xee],vec![0xae,3,0xa2,0xf2,0xee],vec![0xae,0x15,0xa0,0xf2,0xee]]);}
        h.finish().await;
    }
}

#[tokio::test]
async fn batch_query_expired_write_budget_cancels_originals_without_timeout_as_success() {
    let mut h=fixture().await;execute(&mut h).await;
    tokio::time::sleep(Duration::from_millis(2050)).await;
    let(mut owner,calls)=batch_owner(&h,usize::MAX);
    assert!(owner.step().await.unwrap().is_none());assert!(calls.lock().unwrap().is_empty());
    let store=Store::open(h.directory.join("runtime.sqlite")).unwrap();
    for id in 1..=7 {assert_eq!(store.command_status(h.actor.boot_id(),id).unwrap(),Some(homesense_storage::journal::Status::Cancelled));}
    drop(store);h.finish().await;
}
#[tokio::test]
async fn batch_query_caller_revocation_retires_entire_cohort_without_writes() {
    let mut h=fixture().await;
    h.request(Action::SceneAction{controller:h.member,revision:1,scene:"act".into(),execute:true}).await.unwrap();
    h.allowed.store(false,Ordering::Release);
    let(mut owner,calls)=batch_owner(&h,usize::MAX);assert!(owner.step().await.unwrap().is_none());
    assert!(calls.lock().unwrap().is_empty());assert!(!h.actor.scene_writes_pending("bus"));h.finish().await;
}

#[tokio::test]
async fn batch_query_runtime_refresh_sends_controls_before_any_poll_and_confirms_silent_outputs() {
    for delay in [3,20] {
        let mut h=fixture().await;execute(&mut h).await;
        let calls:BatchWrites=Arc::default();
        let owner=TransportOwner::new(h.actor.clone(),[("bus".into(),hb_transport::Connection::for_simulation(QuietModules{calls:calls.clone(),levels:Default::default(),pending:Default::default(),delay},1))].into());
        let replacement=Feedback::new(&h.configuration,ExecutionContext::Simulation,Uuid::parse_str(h.actor.boot_id()).unwrap().as_u128(),1).unwrap();
        let mut observed=crate::observed_transport::ObservedTransport::new(owner,std::mem::replace(&mut h.feedback,replacement));
        observed.configure_output_refresh(&h.configuration,h.controller,h.generation.clone(),ExecutionContext::Simulation,500,5000).unwrap();
        // Admission owns pending batch writes even when the poller is called first.
        observed.refresh_outputs_if_due().await.unwrap();assert!(calls.lock().unwrap().is_empty());
        let first=observed.step_after_refresh_read().await.unwrap().unwrap();first.result.unwrap();assert_eq!(first.completed.len(),4);
        let until=tokio::time::Instant::now()+Duration::from_secs(2);let mut leds=false;
        while tokio::time::Instant::now()<until {
            observed.receive("bus").await.unwrap();
            observed.refresh_outputs_if_due().await.unwrap();
            crate::control_batch::tick(&h.actor,h.controller,observed.feedback()).await.unwrap();
            if let Some(write)=observed.step_after_refresh_read().await.unwrap() {write.result.unwrap();assert_eq!(write.completed.len(),3);leds=true;break;}
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        assert!(leds,"the production periodic planner must query silent modules without deadlock");
        {let writes=calls.lock().unwrap();assert_eq!(writes[0].len(),20);assert!(writes[1..writes.len()-1].iter().all(|bytes|(0xa0..=0xa8).contains(&bytes[2])));assert_eq!(writes.last().unwrap().len(),15);}
        drop(observed);h.finish().await;
    }
}
