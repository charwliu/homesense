//! Generated TCP fixtures: recovery rotation never combines observations across epochs.
use super::*;
use homesense_storage::Store;
use std::{collections::BTreeSet,sync::{Arc,Mutex}};
use tokio::io::{AsyncReadExt,AsyncWriteExt};
#[derive(Clone,Copy,PartialEq,Eq)]
enum Mode {PersistentFirstPartial,Full,DisjointPartial}
struct Trace {
    queries:Mutex<Vec<(usize,[u8;5])>>,
    replies:Mutex<BTreeSet<(usize,u8,u8)>>,
}
async fn run(mode:Mode) {
    let directory=std::env::temp_dir().join(format!("recovery-query-fairness-{}",uuid::Uuid::new_v4()));std::fs::create_dir_all(&directory).unwrap();
    let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();let address=listener.local_addr().unwrap();
    let modules=(0x16..=0x18).map(|address|serde_json::json!({"id":format!("module{address}"),"address":format!("{address:02X}"),"profile":"ad8_din_rs485_v2_2_1","verification":{"status":"verified","evidence":"synthetic loopback fairness","verified_at_unix_ms":1}})).collect::<Vec<_>>();
    let devices=serde_json::json!({"schema_version":2,"revision":1,"gateways":[{"id":"bus","endpoint":address.to_string(),"modules":modules}],"devices":[{"id":"bound","name":"Synthetic light","module_id":"module22","channel":1,"enabled":true}]});
    let path=directory.join("runtime.sqlite");let configuration=Store::open(&path).unwrap().publish_configuration(None,&serde_json::to_vec(&devices).unwrap(),ExecutionContext::Live,&[]).unwrap();
    let controller=uuid::Uuid::new_v4();let actor=Admission::open(path.clone(),crate::admission::Options::default(),move |id|id==controller).await.unwrap();
    let trace=Arc::new(Trace{queries:Mutex::new(vec![]),replies:Mutex::new(BTreeSet::new())});let observed=trace.clone();
    let (stop,receive)=watch::channel(false);let mut stopped=receive.clone();
    let gateway=tokio::spawn(async move {
        let mut next=0usize;let mut workers=tokio::task::JoinSet::new();
        loop {tokio::select! {
            _=stopped.changed()=>break,
            result=workers.join_next(),if !workers.is_empty()=>{result.unwrap().unwrap();},
            accepted=listener.accept()=>{
                let (mut socket,_)=accepted.unwrap();let connection=next;next+=1;let observed=observed.clone();
                workers.spawn(async move {loop {
                    let mut frame=[0;5];if socket.read_exact(&mut frame).await.is_err(){break;}
                    assert_eq!(frame[0],0xae);assert!((0x16..=0x18).contains(&frame[1]));assert_eq!(frame[2..],[0xa0,0xf2,0xee],"only whole-module queries permitted");
                    observed.queries.lock().unwrap().push((connection,frame));
                    let addresses=if mode==Mode::DisjointPartial{vec![0x16,0x17,0x18]}else{vec![frame[1]]};
                    let mut reply=vec![];let mut reply_slots=vec![];
                    for address in addresses {for channel in 1..=8 {
                        let omitted=channel==8 && match mode {Mode::PersistentFirstPartial=>address==0x16,Mode::Full=>false,Mode::DisjointPartial=>address==0x16+(connection%3) as u8};
                        if omitted {continue;}
                        reply_slots.push((connection,address,channel));reply.extend_from_slice(&[0xae,address,0xd0+channel,30,0xee]);
                    }}
                    if socket.write_all(&reply).await.is_err(){break;}
                    observed.replies.lock().unwrap().extend(reply_slots);
                }});
            },
        }}
        workers.abort_all();while let Some(result)=workers.join_next().await {if let Err(error)=result {assert!(error.is_cancelled(),"gateway worker panicked: {error}");}}
    });
    let window=Duration::from_secs(5);let started=tokio::time::Instant::now();
    let work=until_ready(actor.clone(),&configuration,[("bus".into(),address)].into(),1,3,started+window,receive);
    tokio::pin!(work);
    let result=loop {tokio::select! {
        result=&mut work=>break result.unwrap(),
        _=tokio::time::sleep(Duration::from_millis(100)),if mode!=Mode::Full=>{
            let generation=actor.diagnostic_generation(controller,1).await.unwrap();let state=actor.diagnostics(controller,generation).await.unwrap();
            assert!(!state.ready(),"partial observations from different rounds must never reopen admission");
            assert!(actor.task_generation().await.is_err());
        },
    }};
    if mode==Mode::Full {
        let (owner,epoch)=result.unwrap();assert_eq!(epoch,2);assert_eq!(actor.task_generation().await.unwrap().epochs.transport,epoch);
        let objects=owner.feedback().module_truth().module_objects();assert_eq!(objects.len(),3);
        for object in objects {for channel in 1..=8 {let state=owner.feedback().module_truth().located_state(&object.gateway,object.spec.address().get(),channel).unwrap();assert_eq!(state.quality,homesense_state::Quality::Confirmed);}}
        drop(owner);
    } else {
        assert!(result.is_none(),"no round had every registered output channel");assert!(started.elapsed()>=window);
        assert!(actor.task_generation().await.is_err());
    }
    stop.send_replace(true);gateway.await.unwrap();
    {let queries=trace.queries.lock().unwrap();let first=queries.iter().fold(std::collections::BTreeMap::new(),|mut map,(connection,frame)|{map.entry(*connection).or_insert(frame[1]);map});
    if mode==Mode::PersistentFirstPartial {
        assert!(first.len()>=3,"three recovery rounds required for fairness evidence: {first:?}");
        assert_eq!(first.values().take(3).copied().collect::<Vec<_>>(),vec![0x16,0x17,0x18]);
        assert_eq!(queries.iter().map(|(_,frame)|frame[1]).collect::<BTreeSet<_>>(),[0x16,0x17,0x18].into());
    } else if mode==Mode::Full {
        assert_eq!(first.len(),1);assert_eq!(queries.iter().map(|(_,frame)|frame[1]).collect::<Vec<_>>(),vec![0x16,0x17,0x18],"first/default query order must remain unchanged");
    } else {
        assert!(first.len()>=3);
        let replies=trace.replies.lock().unwrap();assert_eq!(replies.iter().map(|(_,address,channel)|(*address,*channel)).collect::<BTreeSet<_>>().len(),24,"the union was complete but must not be reused as one epoch");
        for connection in first.keys() {assert_eq!(replies.iter().filter(|(round,_,_)|round==connection).map(|(_,address,channel)|(*address,*channel)).collect::<BTreeSet<_>>().len(),23);}
    }
    }
    let db=rusqlite::Connection::open(path).unwrap();let count:i64=db.query_row("SELECT count(*) FROM command_journal",[],|row|row.get(0)).unwrap();assert_eq!(count,0);drop(db);
    actor.shutdown().await.unwrap();std::fs::remove_dir_all(directory).unwrap();
}
#[tokio::test]
async fn recovery_retries_rotate_past_persistently_partial_first_of_three_modules(){run(Mode::PersistentFirstPartial).await;}
#[tokio::test]
async fn first_recovery_round_keeps_default_order_and_requires_all_twenty_four_ports(){run(Mode::Full).await;}
#[tokio::test]
async fn complete_union_across_partial_epochs_never_satisfies_recovery(){run(Mode::DisjointPartial).await;}
