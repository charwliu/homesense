//! A transport phase collects its bounded tail without one actor round trip per frame.
use super::*;

#[tokio::test]
async fn batch_tail_collection_preserves_frame_budget_order_and_independent_phase() {
    let mut r = rules();
    r["rules"] = json!([]);
    let actions = (0..23)
        .map(|n| {
            command(
                &format!("batch/{n}"),
                "relay",
                json!({"kind":"switch","on":n%2==0}),
            )
        })
        .collect::<Vec<_>>();
    r["immediate_scenes"] = json!([
        {"id":"act", "enabled":true, "review_evidence":"reviewed bounded lighting phase", "activate":actions},
        {"id":"other", "enabled":true, "review_evidence":"independent phase", "activate":[
            command("other/marker", "marker", json!({"kind":"switch","on":true}))
        ]}
    ]);
    let mut h = Harness::new(r).await;
    for scene in ["act", "other"] {
        h.request(Action::SceneAction {
            controller: h.controller,
            revision: 1,
            scene: scene.into(),
            execute: true,
        })
        .await
        .unwrap();
    }
    let first = h.actor.dispatch().await.unwrap().unwrap();
    let anchor = first.command.id;
    let mut phase = vec![first];
    let tail = h.actor.extend_write_batch(anchor, 7).await.unwrap();
    assert_eq!(
        tail.len(),
        7,
        "one collection honors the remaining frame budget"
    );
    phase.extend(tail);
    phase.extend(
        h.actor
            .extend_write_batch(anchor, hb_transport::MAX_BATCH_FRAMES)
            .await
            .unwrap(),
    );
    assert_eq!(
        phase.len(),
        23,
        "an independent phase must not enter this batch"
    );
    assert!(
        h.actor
            .extend_write_batch(anchor, hb_transport::MAX_BATCH_FRAMES)
            .await
            .unwrap()
            .is_empty()
    );
    for (n, ticket) in phase.iter().enumerate() {
        assert_eq!(ticket.command.device_id.as_str(), "relay");
        assert_eq!(
            ticket.command.command,
            homesense_core::Command::Switch(n % 2 == 0)
        );
        h.actor.finish_write(ticket.command.id, true).await.unwrap();
    }
    assert_eq!(h.wire().await, [0xae, 3, 2, 0xf1, 0xee]);
    assert_eq!(h.count("command_journal"), 24);
    h.finish().await;
}
