//! Synthetic qualified source programs; no physical devices or source approvals.
#[path = "../../homesense-import/tests/support/reviewed_scene.rs"]
mod fixture;
use fixture::*;
use homesense_config::rule_document::{DeactivationPolicy, Document};
use homesense_core::{ExecutionContext, rule::Budget, rule_chain::Chain};
use homesense_import::reviewed_scenes::{DeactivationRuntime, convert, link_event};
use homesense_storage::{Store, journal::Status};
use homesensed::{
    admission::{Admission, Options},
    deferred_rule,
    scene_scheduler::SceneScheduler,
    transport_owner::TransportOwner,
};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use uuid::Uuid;
struct Memory(Arc<Mutex<Vec<u8>>>);
impl std::io::Read for Memory {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        Err(std::io::ErrorKind::WouldBlock.into())
    }
}
impl std::io::Write for Memory {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn draft(st: bool, tt: bool) -> Value {
    let first = (2000..2004)
        .map(|id| (id, 100, if st { 1000 } else { 0 }, 0))
        .collect::<Vec<_>>();
    let target = (2004..2040)
        .map(|id| (id, 0, if tt { 50 } else { 0 }, 0))
        .collect::<Vec<_>>();
    let a = format!(
        "{}{}",
        scene(50, "11", &first),
        scene(11, "unknown", &target)
    );
    let event = guarded_event(&call(3, 7, 50, false, true));
    let xml = project(&a, "", &event, false);
    let cfg = config(false);
    let mut r = review(&xml);
    r.scenes
        .iter_mut()
        .find(|s| s.scene == "control4:scene:7:50")
        .unwrap()
        .deactivation_runtime = Some(DeactivationRuntime {
        active_execution_policy: DeactivationPolicy::RequireQuiescentRuntime,
        review_evidence: "synthetic explicit no-active-work policy".into(),
    });
    let b = convert(&xml, &cfg, &r).unwrap();
    assert!(
        b.scenes.iter().all(|s| s.status == "converted_disabled"),
        "{:?}",
        b.scenes.iter().map(|s| &s.blockers).collect::<Vec<_>>()
    );
    let action = link_event(&xml, &event, &cfg, &r, &b).unwrap();
    assert_eq!(
        action["actions"][1]["action"]["then_actions"][0]["action"]["id"],
        "control4:scene:7:50"
    );
    let mut d = b.document;
    d["variables"] = json!([{"id":"7","kind":"integer","lifetime":"persistent"}]);
    d["rules"] = json!([document_rule(action["actions"].clone(), true)]);
    for k in ["immediate_scenes", "timed_scenes", "scene_deactivations"] {
        for x in d[k].as_array_mut().unwrap() {
            x["enabled"] = true.into();
        }
    }
    d
}
struct Lab {
    actor: Admission,
    scheduler: SceneScheduler,
    controller: Uuid,
    path: std::path::PathBuf,
    bytes: Arc<Mutex<Vec<u8>>>,
    owner: TransportOwner<Memory>,
    permit: Arc<AtomicBool>,
}
impl Lab {
    async fn new(d: Value) -> Self {
        let dir = std::env::temp_dir().join(format!("deactivation-{}", Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("runtime.sqlite");
        let b = Store::open(&path)
            .unwrap()
            .publish_automation(
                None,
                &config_bytes(false),
                &serde_json::to_vec(&d).unwrap(),
                ExecutionContext::Simulation,
                &[],
            )
            .unwrap();
        let id = Uuid::new_v4();
        let permit = Arc::new(AtomicBool::new(true));
        let allow = permit.clone();
        let actor = Admission::open(
            path.clone(),
            Options {
                context: ExecutionContext::Simulation,
                request_ttl: Duration::from_secs(30),
                ..Default::default()
            },
            move |candidate| candidate == id && allow.load(Ordering::Acquire),
        )
        .await
        .unwrap();
        actor.synchronized(1, 1).await.unwrap();
        actor
            .initialize_variables(
                id,
                actor.task_generation().await.unwrap(),
                b.automation.unwrap().variables.into_iter().collect(),
            )
            .await
            .unwrap();
        let scheduler = SceneScheduler::new(actor.clone(), id, 16, 0).unwrap();
        let bytes = Arc::new(Mutex::new(vec![]));
        let owner = TransportOwner::new(
            actor.clone(),
            [(
                "bus".into(),
                hb_transport::Connection::for_simulation(Memory(bytes.clone()), 1),
            )]
            .into(),
        );
        Self {
            actor,
            scheduler,
            controller: id,
            path,
            bytes,
            owner,
            permit,
        }
    }
    async fn execute(&mut self) -> Result<deferred_rule::Outcome, deferred_rule::ExecutionFailure> {
        let mut chain =
            Chain::new(Uuid::new_v4().as_u128(), 0, 60000, Budget::default(), 32).unwrap();
        let root = chain.root();
        deferred_rule::execute_classified(
            &self.actor,
            self.controller,
            "synthetic-rule",
            &mut chain,
            &root,
            &mut self.scheduler,
            None,
        )
        .await
    }
    async fn settle(&mut self) {
        let now = rusqlite::Connection::open(&self.path)
            .unwrap()
            .query_row(
                "select coalesce(max(started_ms),0)+100 from scene_run",
                [],
                |r| r.get::<_, i64>(0),
            )
            .unwrap() as u64;
        for _ in 0..80 {
            let ticks = self.scheduler.tick(now).await.unwrap();
            assert!(ticks.iter().all(|t| t.result.is_ok()));
            while let Some(w) = self.owner.step().await.unwrap() {
                w.result.unwrap();
            }
        }
    }
    async fn finish(mut self) {
        self.scheduler.stop().await;
        self.actor.shutdown().await.unwrap();
        std::fs::remove_dir_all(self.path.parent().unwrap()).unwrap();
    }
}
#[tokio::test]
async fn all_four_qualified_pairs_write_only_complete_disjoint_target36_and_preserve_cause() {
    for st in [false, true] {
        for tt in [false, true] {
            let d = draft(st, tt);
            let parsed = Document::parse(&serde_json::to_vec(&d).unwrap(), &config(false)).unwrap();
            assert!(
                parsed
                    .linked_rules(&parsed.immediate_scenes, &Default::default())
                    .is_err(),
                "guarded calls require deferred owner even if target Immediate"
            );
            let mut l = Lab::new(d).await;
            let out = l.execute().await.unwrap();
            assert_eq!(out.command_ids.len(), if tt { 0 } else { 36 });
            assert_eq!(out.scene_runs.len(), usize::from(tt));
            l.settle().await;
            let expected = (4u8..40)
                .flat_map(|i| [0xae, 16 + i / 8, i % 8 + 1, 0xf2, 0xee])
                .collect::<Vec<_>>();
            assert_eq!(*l.bytes.lock().unwrap(), expected);
            let mut store = Store::open(&l.path).unwrap();
            let chain = store
                .rule_chain(l.actor.boot_id(), out.cause.correlation_id(), 0, 64)
                .unwrap();
            assert_eq!(chain.records.len(), 1);
            assert_eq!(chain.records[0].commands.len(), if tt { 0 } else { 36 });
            let records = if tt {
                let page = store
                    .query_scene(
                        1,
                        &homesense_storage::scene_run::Query::Latest {
                            definition: "control4:scene:7:11".into(),
                        },
                    )
                    .unwrap()
                    .unwrap();
                let cause = store.scene_cause(&page.boot, &page.run).unwrap().unwrap();
                assert_eq!(cause.correlation, out.cause.correlation_id());
                assert_eq!(cause.event, out.cause.event_id());
                page.commands
                    .into_iter()
                    .map(|c| (c.device, c.status, c.source))
                    .collect::<Vec<_>>()
            } else {
                chain.records[0]
                    .commands
                    .iter()
                    .map(|c| (c.device.clone(), c.status, c.source.clone().unwrap()))
                    .collect()
            };
            assert_eq!(records.len(), 36);
            for (i, (device, status, source)) in records.iter().enumerate() {
                assert_eq!(device, &format!("control4:{}", 1004 + i));
                assert_eq!(*status, Status::Written);
                assert!(source.contains("control4:scene:7:11"));
                assert!(!source.contains("control4:scene:7:50/member:"));
            }

            let sql = rusqlite::Connection::open(&l.path).unwrap();
            assert_eq!(
                sql.query_row("select count(*) from scene_run", [], |r| r.get::<_, i64>(0))
                    .unwrap(),
                i64::from(tt)
            );
            if tt {
                let source: String = sql
                    .query_row("select source from scene_cause", [], |r| r.get(0))
                    .unwrap();
                assert!(source.contains("deactivation-target:control4:scene:7:11"));
                assert_eq!(
                    sql.query_row("select definition from scene_run", [], |r| r
                        .get::<_, String>(0))
                        .unwrap(),
                    "control4:scene:7:11"
                );
            }
            drop(sql);
            drop(store);
            l.finish().await;
        }
    }
}
#[tokio::test]
async fn active_source_is_rejected_atomically_without_cancelling_or_writing_target() {
    let mut l = Lab::new(draft(true, true)).await;
    let source = l
        .scheduler
        .start_document(
            &Document::parse(
                &serde_json::to_vec(&draft(true, true)).unwrap(),
                &config(false),
            )
            .unwrap(),
            "control4:scene:7:50",
            0,
        )
        .await
        .unwrap();
    let before = Store::open(&l.path)
        .unwrap()
        .logical_variable(1, "7")
        .unwrap()
        .unwrap();
    assert!(matches!(
        l.execute().await,
        Err(deferred_rule::ExecutionFailure::BusyBeforeAcceptance)
    ));
    assert_eq!(
        l.scheduler.status(source),
        Some(homesense_core::scene::Status::Pending)
    );
    assert!(l.actor.dispatch().await.unwrap().is_none());
    assert_eq!(
        Store::open(&l.path)
            .unwrap()
            .logical_variable(1, "7")
            .unwrap()
            .unwrap()
            .version,
        before.version
    );
    assert!(l.bytes.lock().unwrap().is_empty());
    l.scheduler.cancel(source).await.unwrap();
    assert!(l.execute().await.is_ok());
    l.settle().await;
    assert_eq!(l.bytes.lock().unwrap().len(), 36 * 5);
    l.finish().await;
}
#[tokio::test]
async fn target_cancel_is_not_source_deactivation_and_restart_never_replays_pending_targets() {
    let mut l = Lab::new(draft(false, true)).await;
    let out = l.execute().await.unwrap();
    assert_eq!(l.scheduler.cancel(out.scene_runs[0]).await.unwrap(), 36);
    assert!(l.bytes.lock().unwrap().is_empty());
    let out = l.execute().await.unwrap();
    assert_eq!(out.scene_runs.len(), 1);
    let boot = l.actor.boot_id().to_string();
    l.actor.shutdown().await.unwrap();
    let store = Store::open(&l.path).unwrap();
    assert!(store.has_active_scene_runs(&boot, 1).unwrap());
    drop(store);
    let id = l.controller;
    let actor = Admission::open(
        l.path.clone(),
        Options {
            context: ExecutionContext::Simulation,
            ..Default::default()
        },
        move |c| c == id,
    )
    .await
    .unwrap();
    actor.synchronized(1, 1).await.unwrap();
    assert!(actor.dispatch().await.unwrap().is_none());
    assert!(
        !Store::open(&l.path)
            .unwrap()
            .has_active_scene_runs(&boot, 1)
            .unwrap()
    );
    assert!(l.bytes.lock().unwrap().is_empty());
    actor.shutdown().await.unwrap();
    std::fs::remove_dir_all(l.path.parent().unwrap()).unwrap();
}
#[tokio::test]
async fn revoked_caller_never_falls_back_to_owner_and_false_branch_does_not_run_guard() {
    let mut d = draft(true, true);
    d["rules"][0]["actions"][0]["action"]["value"]["value"] = 0.into();
    let mut l = Lab::new(d).await;
    let source = l
        .scheduler
        .start_document(
            &Document::parse(
                &serde_json::to_vec(&draft(true, true)).unwrap(),
                &config(false),
            )
            .unwrap(),
            "control4:scene:7:50",
            0,
        )
        .await
        .unwrap();
    let out = l.execute().await.unwrap();
    assert!(out.command_ids.is_empty() && out.scene_runs.is_empty());
    assert_eq!(
        l.scheduler.status(source),
        Some(homesense_core::scene::Status::Pending)
    );
    l.permit.store(false, Ordering::Release);
    assert!(l.execute().await.is_err());
    assert!(l.bytes.lock().unwrap().is_empty());
    l.permit.store(true, Ordering::Release);
    l.finish().await;
}
#[tokio::test]
async fn target_partial_write_is_audited_without_compensation_or_later_target_replay() {
    let mut l = Lab::new(draft(true, true)).await;
    let out = l.execute().await.unwrap();
    let sql = rusqlite::Connection::open(&l.path).unwrap();
    let now = sql
        .query_row("select started_ms+100 from scene_run", [], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap() as u64;
    drop(sql);
    l.scheduler.tick(now).await.unwrap();
    l.owner.step().await.unwrap().unwrap().result.unwrap();
    l.scheduler.tick(now).await.unwrap();
    let second = l.actor.dispatch().await.unwrap().unwrap();
    l.actor
        .finish_write(second.command.id, false)
        .await
        .unwrap();
    let _ = l.scheduler.tick(now).await;
    let sql = rusqlite::Connection::open(&l.path).unwrap();
    assert_eq!(
        sql.query_row(
            "select count(*) from command_journal where status='written'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    assert_eq!(
        sql.query_row("select count(*) from command_journal", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        2
    );
    assert_eq!(
        sql.query_row("select status from scene_run", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        "partial"
    );
    assert_eq!(l.bytes.lock().unwrap().len(), 5);
    assert_eq!(out.scene_runs.len(), 1);
    drop(sql);
    l.actor.shutdown().await.unwrap();
    let id = l.controller;
    let actor = Admission::open(
        l.path.clone(),
        Options {
            context: ExecutionContext::Simulation,
            ..Default::default()
        },
        move |c| c == id,
    )
    .await
    .unwrap();
    actor.synchronized(1, 1).await.unwrap();
    assert!(actor.dispatch().await.unwrap().is_none());
    actor.shutdown().await.unwrap();
    std::fs::remove_dir_all(l.path.parent().unwrap()).unwrap();
}
#[tokio::test]
async fn reviewed_deactivation_fade_uses_real_target_samples_and_completion_chain() {
    let root = std::env::temp_dir().join(format!("deactivation-fade-{}", Uuid::new_v4()));
    let doc = json!({"schema_version":1,"configuration_revision":1,"variables":[{"id":"done","kind":"integer","lifetime":"persistent"}],"rules":[{"id":"begin","enabled":true,"review_evidence":"synthetic source off","source":"qualified/agent/source/deactivate","trigger":{"kind":"startup_synchronized"},"actions":[{"source":"source-off","action":{"kind":"scene","id":"source","activate":false}}]},{"id":"after","enabled":true,"review_evidence":"synthetic completed target","source":"target-result","trigger":{"kind":"scene_result","scene":"target-fade"},"actions":[{"source":"done","action":{"kind":"set","variable":"done","value":{"kind":"integer","value":1}}}]}],"immediate_scenes":[{"id":"source","enabled":true,"review_evidence":"synthetic unused source vector","activate":[{"source":"source-only","action":{"kind":"command","device":"simulation:light","command":{"kind":"level","value":100,"ramp":false}}}]}],"timed_scenes":[{"id":"target-fade","enabled":true,"review_evidence":"synthetic explicit software fade","max_lateness_ms":1000,"steps":[{"source":"complete-target/member:0","offset_ms":10,"device":"simulation:light","command":{"kind":"level","value":30,"ramp":false},"fade":{"duration_ms":300,"interval_ms":100,"lateness_ms":200}}]}],"scene_deactivations":[{"scene":"source","target":"target-fade","enabled":true,"review_evidence":"synthetic explicit quiet policy","active_execution_policy":"require_quiescent_runtime"}]});
    homesensed::service::serve_automation_simulation(
        include_bytes!("../../../config/simulation.json"),
        &serde_json::to_vec(&doc).unwrap(),
        &root,
        Duration::from_secs(1),
        None,
    )
    .await
    .unwrap();
    let sql = rusqlite::Connection::open(root.join("automation-simulation.sqlite")).unwrap();
    let rows=sql.prepare("select intent,status from command_journal where substr(intent,1,2)=x'0102' order by command_id").unwrap().query_map([],|r|Ok((r.get::<_,Vec<u8>>(0)?,r.get::<_,String>(1)?))).unwrap().collect::<Result<Vec<_>,_>>().unwrap();
    assert!(rows.len() >= 2 && rows.len() <= 3);
    assert!(
        rows.iter()
            .all(|(intent, status)| intent[2] <= 30 && status == "written")
    );
    assert_eq!(rows.last().unwrap().0[2], 30);
    assert!(rows.windows(2).all(|p| p[0].0[2] < p[1].0[2]));
    assert_eq!(
        sql.query_row("select count(*) from scene_sample", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        rows.len() as i64
    );
    assert_eq!(
        sql.query_row("select definition from scene_run", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        "target-fade"
    );
    assert_eq!(
        sql.query_row(
            "select value from logical_variable where name='done'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    let source: String = sql
        .query_row("select source from scene_cause", [], |r| r.get(0))
        .unwrap();
    assert!(source.contains("deactivation-target:target-fade"));
    assert_eq!(sql.query_row("select count(*) from rule_admission child join rule_admission parent on child.boot_id=parent.boot_id and child.correlation=parent.correlation and child.parent_id=parent.event_id where child.rule_id='after' and parent.rule_id='begin'",[],|r|r.get::<_,i64>(0)).unwrap(),1);
    drop(sql);
    std::fs::remove_dir_all(root).unwrap();
}
