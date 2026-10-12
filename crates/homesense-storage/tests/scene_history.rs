use homesense_core::ExecutionContext;
use homesense_storage::{Store,scene_history::Cursor,scene_run::Status};
#[test]
fn scene_history_pages_across_boots_without_clock_order_or_cross_scope_cursor() {
    let root=std::env::temp_dir().join(format!("scene-history-{}-{}",std::process::id(),std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    std::fs::create_dir(&root).unwrap();let path=root.join("state.sqlite");
    let mut store=Store::open(&path).unwrap();
    store.publish_configuration(None,include_bytes!("../../../config/simulation.json"),ExecutionContext::Simulation,&[]).unwrap();
    assert!(store.scene_history(1,"light",None,2).unwrap().runs.is_empty());
    for (boot,run,scene,time) in [("old","1","light",9000),("old","2","other",9500),("new","1","light",1),("new","2","light",2)] {
        store.begin_scene_run(boot,run,1,Some(scene),time).unwrap();
    }
    store.checkpoint_scene_run("new","2",0,Status::Cancelled,3).unwrap();
    let first=store.scene_history(1,"light",None,2).unwrap();
    assert_eq!(first.runs.iter().map(|r|(&*r.identity.boot,&*r.identity.run)).collect::<Vec<_>>(),vec![("new","2"),("new","1")]);
    assert_eq!(first.runs[0].status,Status::Cancelled);
    store.begin_scene_run("new","3",1,Some("light"),4).unwrap();
    let second=store.scene_history(1,"light",first.next_before.as_ref(),2).unwrap();
    assert_eq!(second.runs.len(),1);assert_eq!(second.runs[0].identity.boot,"old");assert!(second.next_before.is_none());
    assert!(store.scene_history(1,"other",first.next_before.as_ref(),2).is_err());
    assert!(store.scene_history(2,"light",first.next_before.as_ref(),2).is_err());
    assert!(store.scene_history(1,"light",Some(&Cursor{boot:"missing".into(),run:"1".into()}),2).is_err());
    for limit in [0,101] {assert!(store.scene_history(1,"light",None,limit).is_err());}
    assert!(store.scene_history(1,"",None,2).is_err());
    drop(store);let store=Store::open(&path).unwrap();
    assert_eq!(store.scene_history(1,"light",first.next_before.as_ref(),2).unwrap(),second);
    assert_eq!(store.scene_history(1,"light",None,100).unwrap().runs.len(),4);
    drop(store);std::fs::remove_dir_all(root).unwrap();
}
