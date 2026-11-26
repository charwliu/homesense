use std::{path::PathBuf, time::Duration};
fn root() -> PathBuf {
    let p = std::env::temp_dir().join(format!("hs-retention-worker-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&p).unwrap();
    p
}
#[tokio::test]
async fn worker_starts_and_is_joined_without_replaying_commands() {
    let root = root();
    let path = root.join("state.sqlite");
    let mut store = homesense_storage::Store::open(&path).unwrap();
    store
        .publish_automation(
            None,
            include_bytes!("../../../config/simulation.json"),
            br#"{"schema_version":1,"configuration_revision":1,"variables":[],"rules":[]}"#,
            homesense_core::ExecutionContext::Simulation,
            &[],
        )
        .unwrap();
    let verify = path.clone();
    let service = async move {
        tokio::time::timeout(Duration::from_secs(5),async {
            loop {
                let ready=rusqlite::Connection::open(&verify).unwrap().query_row("SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='history_retention_clock')",[],|r|r.get::<_,bool>(0)).unwrap();
                if ready{break;}tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.unwrap();
        Ok(())
    };
    homesensed::history_retention::supervise(path.clone(), service)
        .await
        .unwrap();
    let db = rusqlite::Connection::open(path).unwrap();
    assert_eq!(
        db.query_row("SELECT count(*) FROM command_journal", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(store.active().unwrap().unwrap().revision, 1);
    assert!(!root.join("history-archives").exists());
    drop(db);
    drop(store);
    std::fs::remove_dir_all(root).unwrap();
}
#[tokio::test]
async fn missing_source_failure_never_creates_db_or_stops_service() {
    let root = root();
    let source = root.join("missing.sqlite");
    let service = async {
        tokio::time::sleep(Duration::from_millis(150)).await;
        Ok(())
    };
    assert!(
        homesensed::history_retention::supervise(source.clone(), service)
            .await
            .is_ok()
    );
    assert!(!source.exists());
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test(start_paused = true)]
async fn worker_revisits_on_minute_tick_and_stops_cleanly() {
    let root = root();
    let source = root.join("state.sqlite");
    let mut store = homesense_storage::Store::open(&source).unwrap();
    store.publish(None, 1, b"fixture", &[]).unwrap();
    let db = rusqlite::Connection::open(&source).unwrap();
    db.execute_batch("BEGIN").unwrap();
    for id in 1..=260 {
        db.execute("INSERT INTO command_journal(boot_id,command_id,revision,device_id,origin,intent,transport_epoch,control_epoch,deadline_ms,updated_ms,status) VALUES('old',?1,1,'read','background',x'01000000',1,1,100,10,'written')",[id]).unwrap();
    }
    db.execute_batch("COMMIT").unwrap();
    let (stop, receive) = tokio::sync::watch::channel(false);
    let worker = tokio::spawn(homesensed::history_retention::run(source, receive));
    for expected in [128, 256] {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let exists=db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='history_retention_command')",[],|r|r.get::<_,bool>(0)).unwrap();
            let count = if exists {
                db.query_row("SELECT count(*) FROM history_retention_command", [], |r| {
                    r.get::<_, i64>(0)
                })
                .unwrap()
            } else {
                0
            };
            if count == expected {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "worker did not complete scheduled observation pass"
            );
            tokio::task::yield_now().await;
        }
        if expected == 128 {
            tokio::time::advance(Duration::from_secs(60)).await;
        }
    }
    stop.send(true).unwrap();
    worker.await.unwrap();
    assert_eq!(
        db.query_row("SELECT count(*) FROM command_journal", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        260
    );
    drop(db);
    drop(store);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn supervision_boxes_large_service_before_constructing_async_state() {
    let bytes = [7_u8; 65_536];
    let service = async move {
        assert_eq!(std::hint::black_box(bytes)[0], 7);
        Ok(())
    };
    assert!(std::mem::size_of_val(&service) >= 65_536);
    let future = homesensed::daily_backup::supervise_monitored(
        PathBuf::from("not-opened.sqlite"),
        homesense_core::ExecutionContext::Simulation,
        service,
        homesensed::daily_backup::Monitor::default(),
    );
    assert!(
        std::mem::size_of_val(&future) < 16_384,
        "supervision must not embed the raw control future"
    );
    drop(future); // No I/O or service activation is needed for the layout guard.
}
