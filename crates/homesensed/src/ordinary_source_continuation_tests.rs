// Synthetic continuation probes included in the exact HTTP harness module.
// No timing sleeps, external IO, private project data or fallback credentials.
impl Harness {
    fn assert_response_written(&self, response: &Value) {
        let ids = response["commands"]
            .as_array()
            .expect("fresh control must return its own command IDs");
        assert!(
            !ids.is_empty(),
            "fresh control returned no accepted IDs: {response}"
        );
        let sql = rusqlite::Connection::open(self.directory.join("runtime.sqlite")).unwrap();
        for id in ids {
            let id = id
                .as_u64()
                .or_else(|| id.as_str().and_then(|s| s.parse().ok()))
                .unwrap();
            let (device,status):(String,String)=sql.query_row("SELECT device_id,status FROM command_journal WHERE boot_id=?1 AND command_id=?2",rusqlite::params![self.actor.boot_id(),i64::try_from(id).expect("command ID must fit SQLite INTEGER")],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
            assert_eq!(
                device, "switch",
                "fresh source ID belongs to a different operation"
            );
            assert_eq!(
                status, "written",
                "fresh source's own command was not written"
            );
        }
        assert!(
            self.bytes
                .lock()
                .unwrap()
                .as_chunks::<5>().0.iter().copied()
                .any(|b| b == [0xae, 0x03, 0x01, 0xf1, 0xee]),
            "fresh source expected switch-on frame is absent"
        );
    }
    async fn fresh_root_proof(&mut self) {
        self.bytes.lock().unwrap().clear();
        let task = self.call_root();
        let (status, body) = self.finish_http(task).await;
        assert!(
            [200, 202].contains(&status),
            "fresh same-controller root rejected: {body}"
        );
        self.drain().await;
        self.assert_response_written(&body);
    }
    fn count_written(&self) -> i64 {
        rusqlite::Connection::open(self.directory.join("runtime.sqlite"))
            .unwrap()
            .query_row(
                "SELECT count(*) FROM command_journal WHERE status='written'",
                [],
                |r| r.get(0),
            )
            .unwrap()
    }
    fn assert_no_pending_commands(&self) {
        let count: i64 = rusqlite::Connection::open(self.directory.join("runtime.sqlite"))
            .unwrap()
            .query_row(
                "SELECT count(*) FROM command_journal WHERE status IN ('accepted','writing')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 0, "invalid source left writable queue entries");
    }
    async fn at(&mut self, now: u64) {
        self.clock.store(now, Ordering::Release);
        // Receiver ticks native relative phases even when the HTTP mailbox is empty.
        self.receiver
            .poll_causal(
                &self.actor,
                &self.document,
                &mut self.scheduler,
                &self.feedback,
                5000,
                None,
                Some(&mut self.router),
            )
            .await;
        self.router
            .poll(&mut self.scheduler, Some((&self.feedback, 5000)))
            .await
            .unwrap();
    }
    async fn send_one(&mut self) -> Option<u64> {
        let ticket = self.actor.dispatch().await.unwrap()?;
        let id = ticket.command.id;
        let mut connection = hb_transport::Connection::for_simulation(
            Wire(self.bytes.clone()),
            ticket.command.epochs.transport,
        );
        crate::transport_owner::send_ticket(&ticket, &mut connection, || self.actor.monotonic_ms())
            .unwrap();
        self.actor.finish_write(id, true).await.unwrap();
        Some(id)
    }
    async fn drain(&mut self) {
        for _ in 0..32 {
            if self.send_one().await.is_none() {
                return;
            }
        }
        panic!("unbounded synthetic queue");
    }
    fn restrict_scene(&mut self, id: &str) {
        let scenes = [id.to_owned()].into();
        self.tokens
            .update_grant(
                self.source.id,
                CredentialUpdate {
                    expected: self.source.grant.clone(),
                    role: self.source.grant.role.clone(),
                    devices: BTreeSet::new(),
                    scenes,
                },
                Some(&format!("Bearer {ROOT}")),
                None,
                None,
            )
            .unwrap();
        self.source.grant.scenes = [id.to_owned()].into();
        self.source.grant.devices.clear();
        if self.session.is_some() {
            self.session = Some(
                self.tokens
                    .login(&format!("Bearer {}", self.source.secret))
                    .unwrap(),
            );
        }
    }
}
#[tokio::test]
async fn ordinary_http_ticket_rechecks_exact_source_at_the_actual_write() {
    let mut records = vec![];
    for case in [
        Case::Switch,
        Case::Dimmer,
        Case::Combined,
        Case::MotorOpen,
        Case::MotorStop,
        Case::ModuleSwitch,
        Case::ModuleMotor,
        Case::Immediate,
        Case::Later,
        Case::Relative,
    ] {
        for change in ["role", "scope", "revoke", "logout"] {
            let mut h = Harness::new(case, change == "logout").await;
            let task = h.call(case);
            let (status, body) = h.finish_http(task).await;
            assert!([200, 202].contains(&status), "{case:?}: {body}");
            h.at(200).await;
            let ticket = h
                .actor
                .dispatch()
                .await
                .unwrap()
                .expect("positive accepted route produced no ticket");
            h.change(change);
            let mut connection = hb_transport::Connection::for_simulation(Wire(h.bytes.clone()), 1);
            let error = crate::transport_owner::send_ticket(&ticket, &mut connection, || {
                h.actor.monotonic_ms()
            })
            .unwrap_err();
            assert_eq!(error.bytes_written, 0);
            assert_eq!(
                error.reason,
                hb_transport::SendFailure::Io(io::ErrorKind::PermissionDenied)
            );
            assert!(h.bytes.lock().unwrap().is_empty());
            h.actor
                .finish_write(ticket.command.id, false)
                .await
                .unwrap();
            assert_eq!(h.count_written(), 0);
            // Preserve the existing conservative late-write failure contract:
            // explicit owner recovery, never retry the refused command.
            h.actor.recover_synchronized(2, 1).await.unwrap();
            let generation = h.actor.task_generation().await.unwrap();
            let controller = h.source.grant.controller;
            (h.control, h.receiver) = super::channel(controller, generation);
            h.api.runtime.write().unwrap().as_mut().unwrap().control = h.control.clone();
            h.scheduler = SceneScheduler::new(h.actor.clone(), controller, 16, 200).unwrap();
            h.router = Router::new(h.actor.clone(), controller, &h.scheduler, 16).unwrap();
            h.fresh_root_proof().await;
            records.push(json!({"case":format!("{case:?}"),"change":change,"invalid_bytes":0,"fresh_root_own_ids_written_after_explicit_recovery":true}));
            h.actor.clone().shutdown().await.unwrap();
        }
    }
    evidence("ordinary-source-after-ticket.json", &records);
}
fn two_stage(_: &mut Value, rules: &mut Value) {
    rules["timed_scenes"][0]["steps"] = json!([
        {"source":"first legitimate stage","offset_ms":0,"device":"switch","command":{"kind":"switch","on":true}},
        {"source":"delayed stage","offset_ms":100,"device":"switch","command":{"kind":"switch","on":false}}]);
}
#[tokio::test]
async fn source_loss_after_first_written_cancels_delayed_stages_without_blocking_another_source() {
    let mut records = vec![];
    for change in [
        None,
        Some("role"),
        Some("scope"),
        Some("revoke"),
        Some("logout"),
    ] {
        let mut h = Harness::configured(Case::Later, change == Some("logout"), two_stage).await;
        h.restrict_scene("later");
        let task = h.call(Case::Later);
        let (status, body) = h.finish_http(task).await;
        assert!([200, 202].contains(&status), "{body}");
        h.at(100).await;
        h.drain().await;
        assert_eq!(h.count_written(), 1);
        assert_eq!(*h.bytes.lock().unwrap(), [0xae, 3, 1, 0xf1, 0xee]);
        h.bytes.lock().unwrap().clear();
        if let Some(kind) = change {
            h.change(kind);
        }
        h.at(300).await;
        h.drain().await;
        h.at(301).await;
        let wire = h.bytes.lock().unwrap().clone();
        if change.is_some() {
            assert!(wire.is_empty());
            assert_eq!(h.count_written(), 1);
        } else {
            assert_eq!(wire, [0xae, 3, 1, 0xf2, 0xee]);
            assert_eq!(h.count_written(), 2);
        }
        h.assert_no_pending_commands();
        h.fresh_root_proof().await;
        records.push(
            json!({"change":change,"wire_after_barrier":wire,"fresh_root_own_ids_written":true}),
        );
        h.actor.clone().shutdown().await.unwrap();
    }
    evidence("ordinary-source-delayed.json", &records);
}
fn nested_result(_: &mut Value, rules: &mut Value) {
    rules["immediate_scenes"][0]["activate"] = json!([{ "source":"nested reviewed immediate","action":{"kind":"scene","id":"nested","activate":true}}]);
    rules["immediate_scenes"].as_array_mut().unwrap().push(json!({"id":"nested","enabled":true,"review_evidence":"Synthetic nested scene","activate":[
        {"source":"root command","action":{"kind":"command","device":"switch","command":{"kind":"switch","on":true}}},
        {"source":"start delayed followup","action":{"kind":"set","variable":"start","value":{"kind":"integer","value":1}}}]}));
    rules["timed_scenes"][0]["steps"][0]["offset_ms"] = json!(100);
    rules["timed_scenes"][0]["steps"][0]["command"]["on"] = json!(false);
    for id in ["start", "cascade"] {
        rules["variables"]
            .as_array_mut()
            .unwrap()
            .push(json!({"id":id,"kind":"integer","lifetime":"persistent"}));
    }
    rules["rules"] = json!([
        {"id":"start-later","source":"synthetic nested continuation","enabled":true,"review_evidence":"Synthetic","trigger":{"kind":"variable_changed","variable":"start"},"actions":[{"source":"delayed receipt","action":{"kind":"scene","id":"later","activate":true}}]},
        {"id":"result","source":"synthetic scene result","enabled":true,"review_evidence":"Synthetic","trigger":{"kind":"scene_result","scene":"later"},"actions":[{"source":"result variable","action":{"kind":"set","variable":"cascade","value":{"kind":"integer","value":5}}}]},
        {"id":"variable","source":"synthetic variable cascade","enabled":true,"review_evidence":"Synthetic","trigger":{"kind":"variable_changed","variable":"cascade"},"actions":[{"source":"variable output","action":{"kind":"command","device":"switch","command":{"kind":"switch","on":true}}}]}]);
}
#[tokio::test]
async fn immediate_nested_result_and_variable_cascade_keep_the_root_scene_only_source() {
    let mut records = vec![];
    // Barrier 0: after initial command before nested delay. Barrier 1: nested
    // output already Written, but its scene-result/variable rules not resumed.
    for barrier in [0, 1] {
        for change in [
            None,
            Some("role"),
            Some("scope"),
            Some("revoke"),
            Some("logout"),
        ] {
            let mut h =
                Harness::configured(Case::Immediate, change == Some("logout"), nested_result).await;
            h.restrict_scene("instant"); // no grant to nested scene or any device
            let task = h.call(Case::Immediate);
            let (status, body) = h.finish_http(task).await;
            assert!([200, 202].contains(&status), "{body}");
            h.drain().await;
            assert_eq!(h.count_written(), 1);
            if barrier == 1 {
                h.at(300).await;
                h.drain().await;
                assert_eq!(h.count_written(), 2);
            }
            let before = h.count_written();
            h.bytes.lock().unwrap().clear();
            if let Some(kind) = change {
                h.change(kind);
            }
            for now in 301..306 {
                h.at(now).await;
                h.drain().await;
            }
            let wire = h.bytes.lock().unwrap().clone();
            let generation = h.actor.task_generation().await.unwrap();
            let values = h
                .actor
                .variable_snapshot(
                    h.source.grant.controller,
                    generation,
                    vec!["cascade".into()],
                )
                .await
                .unwrap();
            if change.is_some() {
                assert!(wire.is_empty());
                assert_eq!(h.count_written(), before);
                assert_eq!(
                    values["cascade"].value, None,
                    "revoked result modified selection variable"
                );
            } else {
                assert!(!wire.is_empty());
                assert_eq!(h.count_written(), 3);
                assert_eq!(
                    values["cascade"].value,
                    Some(homesense_core::rule::Value::Integer(5))
                );
            }
            h.assert_no_pending_commands();
            h.fresh_root_proof().await;
            records.push(json!({"barrier":barrier,"change":change,"wire_after_barrier":wire,"fresh_root_own_ids_written":true}));
            h.actor.clone().shutdown().await.unwrap();
        }
    }
    evidence("ordinary-source-causal.json", &records);
}
fn configured_fade(config: &mut Value, rules: &mut Value) {
    config["gateways"][0]["modules"][1]["profile"] = json!("gt_d8_rl_v2_2_6_2");
    rules["timed_scenes"][0]["steps"] = json!([{ "source":"configured synthetic fade","offset_ms":0,"device":"dimmer","command":{"kind":"level","value":0,"ramp":false},"fade":{"duration_ms":200,"interval_ms":50,"lateness_ms":100}}]);
}
#[tokio::test]
async fn configured_fade_stops_future_samples_after_source_loss_and_releases_budget() {
    let mut records = vec![];
    for change in [
        None,
        Some("role"),
        Some("scope"),
        Some("revoke"),
        Some("logout"),
    ] {
        let mut h =
            Harness::configured(Case::Later, change == Some("logout"), configured_fade).await;
        h.restrict_scene("later");
        h.feedback
            .apply(&crate::transport_owner::GatewayRead {
                gateway_id: "bus".into(),
                transport_epoch: 1,
                received_at_ms: 100,
                result: hb_transport::Receive::Frames {
                    transport_epoch: 1,
                    frames: vec![hb_protocol::RawFrame {
                        address: 0x15,
                        command: 0xd1,
                        value: 100,
                    }],
                },
            })
            .unwrap();
        let task = h.call(Case::Later);
        let (status, body) = h.finish_http(task).await;
        assert!([200, 202].contains(&status), "{body}");
        h.at(100).await;
        h.at(150).await;
        h.drain().await;
        assert_eq!(h.count_written(), 1, "first legitimate fade sample missing");
        h.bytes.lock().unwrap().clear();
        if let Some(kind) = change {
            h.change(kind);
        }
        for now in [200, 250, 300, 350, 400] {
            h.at(now).await;
            h.drain().await;
        }
        let wire = h.bytes.lock().unwrap().clone();
        if change.is_some() {
            assert!(wire.is_empty());
            assert_eq!(h.count_written(), 1);
        } else {
            assert_eq!(h.count_written(), 4);
            assert_eq!(wire.len(), 15);
        }
        h.assert_no_pending_commands();
        // Full reservation proves revoked fade released its allocation.
        let reservation = h.actor.fade_budget.reserve(1024, 20).unwrap();
        drop(reservation);
        h.fresh_root_proof().await;
        records.push(json!({"change":change,"wire_after_first_sample":wire,"budget_released":true,"fresh_root_own_ids_written":true}));
        h.actor.clone().shutdown().await.unwrap();
    }
    evidence("ordinary-source-fade.json", &records);
}
#[tokio::test]
async fn relative_scene_does_not_start_a_later_query_phase_under_another_same_controller_grant() {
    let mut records = vec![];
    for change in [
        None,
        Some("role"),
        Some("scope"),
        Some("revoke"),
        Some("logout"),
    ] {
        let mut h = Harness::new(Case::Relative, change == Some("logout")).await;
        let task = h.call(Case::Relative);
        let (status, body) = h.finish_http(task).await;
        assert!([200, 202].contains(&status), "{body}");
        h.drain().await;
        assert!(h.count_written() > 0);
        let before = h.count_written();
        h.bytes.lock().unwrap().clear();
        if let Some(kind) = change {
            h.change(kind);
        }
        h.at(200).await;
        h.drain().await;
        let wire = h.bytes.lock().unwrap().clone();
        if change.is_some() {
            assert!(wire.is_empty());
            assert_eq!(h.count_written(), before);
            h.assert_no_pending_commands();
        } else {
            assert!(
                !wire.is_empty(),
                "unchanged source lost its next native query phase"
            );
            assert!(h.count_written() > before);
        }
        h.fresh_root_proof().await;
        records.push(
            json!({"change":change,"next_phase_wire":wire,"fresh_root_own_ids_written":true}),
        );
        h.actor.clone().shutdown().await.unwrap();
    }
    evidence("ordinary-source-native-phase.json", &records);
}
#[tokio::test]
async fn administrative_query_queue_is_bound_to_the_exact_http_source() {
    let mut records = vec![];
    for ticket_barrier in [false, true] {
        for change in [
            None,
            Some("role"),
            Some("scope"),
            Some("revoke"),
            Some("logout"),
        ] {
            let mut h = Harness::new(Case::ModuleSwitch, change == Some("logout")).await;
            let body = json!({"module_id":"relay","scope":{"kind":"channel","channel":1},"revision":"1","request_id":Uuid::new_v4().to_string()});
            let mut request = Request::builder()
                .method("POST")
                .uri("/api/v1/module-queries")
                .header("host", "127.0.0.1:8088")
                .header("origin", &h.api.origin)
                .header("content-type", "application/json");
            if let Some(session) = &h.session {
                request = request
                    .header("cookie", format!("homesense_session={}", session.cookie))
                    .header("x-csrf-token", &session.csrf);
            } else {
                request = request.header("authorization", format!("Bearer {}", h.source.secret));
            }
            let request = request.body(Bytes::from(body.to_string())).unwrap();
            let api = h.api.clone();
            let task = tokio::spawn(async move { api.handle(request).await });
            let (status, response) = h.finish_http(task).await;
            assert!([200, 202].contains(&status), "{response}");
            assert_eq!(response["outcome"], "accepted");
            let ticket = if ticket_barrier {
                Some(h.actor.dispatch().await.unwrap().unwrap())
            } else {
                None
            };
            if let Some(kind) = change {
                h.change(kind);
            }
            if let Some(ticket) = ticket {
                let mut connection =
                    hb_transport::Connection::for_simulation(Wire(h.bytes.clone()), 1);
                let result = crate::transport_owner::send_ticket(&ticket, &mut connection, || {
                    h.actor.monotonic_ms()
                });
                if change.is_some() {
                    assert_eq!(result.unwrap_err().bytes_written, 0);
                    h.actor
                        .finish_write(ticket.command.id, false)
                        .await
                        .unwrap();
                    h.actor.recover_synchronized(2, 1).await.unwrap();
                    let generation = h.actor.task_generation().await.unwrap();
                    (h.control, h.receiver) = super::channel(h.source.grant.controller, generation);
                    h.api.runtime.write().unwrap().as_mut().unwrap().control = h.control.clone();
                } else {
                    result.unwrap();
                    h.actor.finish_write(ticket.command.id, true).await.unwrap();
                }
            } else {
                h.drain().await;
            }
            let bytes = h.bytes.lock().unwrap().clone();
            if change.is_some() {
                assert!(bytes.is_empty());
                assert_eq!(h.count_written(), 0);
            } else {
                assert_eq!(bytes, [0xae, 3, 0xa1, 0xf2, 0xee]);
                assert_eq!(h.count_written(), 1);
            }
            h.assert_no_pending_commands();
            h.fresh_root_proof().await;
            records.push(json!({"ticket_barrier":ticket_barrier,"change":change,"query_wire":bytes,"fresh_root_own_ids_written":true}));
            h.actor.clone().shutdown().await.unwrap();
        }
    }
    evidence("ordinary-source-module-query.json", &records);
}
fn inferred_motor_leds(config: &mut Value, _: &mut Value) {
    config["gateways"][0]["modules"].as_array_mut().unwrap().push(json!({"id":"panel","address":"41","profile":"ts6_86_rs485_v2_6_5_6key","verification":{"status":"simulated"},"led_mode":"manual"}));
    config["devices"].as_array_mut().unwrap().extend((1..=3).map(|n|json!({"id":format!("motor-led:{n}"),"name":format!("Synthetic LED {n}"),"module_id":"panel","channel":n,"enabled":true})));
    config["devices"][4]["curtain_state"] = json!({"open_travel_ms":1000,"close_travel_ms":1000,"stop_led_idle_ms":200,"evidence":"Synthetic review","reviewed_at_unix_ms":1,"leds":{"open":["motor-led:1"],"stop":["motor-led:2"],"close":["motor-led:3"]}});
}
#[tokio::test]
async fn inferred_curtain_leds_retain_motor_source_after_the_motor_write_has_finished() {
    let mut records = vec![];
    for change in [
        None,
        Some("role"),
        Some("scope"),
        Some("revoke"),
        Some("logout"),
    ] {
        let mut h = Harness::configured(
            Case::MotorOpen,
            change == Some("logout"),
            inferred_motor_leds,
        )
        .await;
        assert!(
            !h.source.grant.devices.contains("motor-led:1"),
            "logical motor grant need not include configured derived LEDs"
        );
        let task = h.call(Case::MotorOpen);
        let (status, body) = h.finish_http(task).await;
        assert!([200, 202].contains(&status), "{body}");
        let first = h.send_one().await.unwrap();
        assert_eq!(*h.bytes.lock().unwrap(), [0xae, 0x22, 0x01, 0xf1, 0xee]);
        // Flush any already accepted immediate indicators while the source is
        // valid; only a later endpoint/idle effect is tested after this barrier.
        h.drain().await;
        let before = h.count_written();
        h.bytes.lock().unwrap().clear();
        if let Some(kind) = change {
            h.change(kind);
        }
        h.clock.store(1400, Ordering::Release);
        // Receive-boundary audit advances the model; the following actor
        // iteration collects effects before handling the next dispatch.
        h.actor.task_generation().await.unwrap();
        h.drain().await;
        let bytes = h.bytes.lock().unwrap().clone();
        if change.is_some() {
            assert!(bytes.is_empty());
            assert_eq!(h.count_written(), before);
        } else {
            assert!(
                !bytes.is_empty(),
                "unchanged motor source lost its delayed indicator effects"
            );
            assert!(
                bytes.as_chunks::<5>().0.iter().copied().all(|frame| frame[1] == 0x41),
                "a motor command was replayed"
            );
        }
        h.assert_no_pending_commands();
        h.fresh_root_proof().await;
        records.push(json!({"change":change,"motor_written_id":first,"later_led_wire":bytes,"fresh_root_own_ids_written":true}));
        h.actor.clone().shutdown().await.unwrap();
    }
    evidence("ordinary-source-curtain-leds.json", &records);
}
#[tokio::test]
async fn source_generation_is_captured_before_module_preflight_and_never_recaptured() {
    let mut h = Harness::new(Case::ModuleSwitch, false).await;
    let task = h.call(Case::ModuleSwitch);
    h.wait_queued().await; // read-only ModuleBinding, before final control request
    h.tokens
        .update_grant(
            h.source.id,
            CredentialUpdate {
                expected: h.source.grant.clone(),
                role: h.source.grant.role.clone(),
                devices: h.source.grant.devices.clone(),
                scenes: h.source.grant.scenes.clone(),
            },
            Some(&format!("Bearer {ROOT}")),
            None,
            None,
        )
        .unwrap();
    let (_, old) = h.finish_http(task).await;
    h.drain().await;
    assert!(h.bytes.lock().unwrap().is_empty());
    assert_eq!(h.count_written(), 0);
    // A new request may authenticate using the replacement generation and the
    // retained scope. The old request cannot borrow that replacement.
    let new = h.call(Case::ModuleSwitch);
    let (status, body) = h.finish_http(new).await;
    assert!(
        [200, 202].contains(&status),
        "replacement generation request failed: {body}"
    );
    h.drain().await;
    h.assert_response_written(&body);
    evidence(
        "ordinary-source-preflight.json",
        &[json!({"old_response":old,"fresh_same_secret_new_generation_own_ids_written":true})],
    );
    h.actor.clone().shutdown().await.unwrap();
}
#[tokio::test]
async fn combined_http_partial_write_keeps_the_written_dimmer_and_rejects_unwritten_hidden_supply_after_source_loss()
 {
    let mut records = vec![];
    for change in [
        None,
        Some("role"),
        Some("scope"),
        Some("revoke"),
        Some("logout"),
    ] {
        let mut h = Harness::new(Case::Combined, change == Some("logout")).await;
        assert!(!h.source.grant.devices.contains("supply"));
        let task = h.call(Case::Combined);
        let (status, response) = h.finish_http(task).await;
        assert!([200, 202].contains(&status), "{response}");
        let ids = response["commands"].as_array().unwrap();
        assert_eq!(ids.len(), 2);
        let first = ids[0].as_u64().unwrap();
        let second = ids[1].as_u64().unwrap();
        assert_eq!(h.send_one().await, Some(first));
        assert_eq!(
            *h.bytes.lock().unwrap(),
            [0xae, 0x15, 0xb2, 30, 0xee],
            "ordinary combined control writes dimmer before supply by design"
        );
        h.bytes.lock().unwrap().clear();
        if let Some(kind) = change {
            h.change(kind);
        }
        h.drain().await;
        let wire = h.bytes.lock().unwrap().clone();
        let sql = rusqlite::Connection::open(h.directory.join("runtime.sqlite")).unwrap();
        let status_of = |id: u64| -> String {
            sql.query_row(
                "SELECT status FROM command_journal WHERE boot_id=?1 AND command_id=?2",
                rusqlite::params![h.actor.boot_id(), i64::try_from(id).unwrap()],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(
            status_of(first),
            "written",
            "already written first frame must not be recalled or replayed"
        );
        if change.is_some() {
            assert!(wire.is_empty());
            assert_eq!(status_of(second), "cancelled");
            assert_eq!(h.count_written(), 1);
        } else {
            assert_eq!(wire, [0xae, 3, 2, 0xf1, 0xee]);
            assert_eq!(status_of(second), "written");
            assert_eq!(h.count_written(), 2);
        }
        drop(sql);
        h.assert_no_pending_commands();
        h.fresh_root_proof().await;
        records.push(json!({"change":change,"written_first_command_id":first,"remaining_command_id":second,"remaining_wire":wire,"physical_order":"dimmer_then_hidden_supply","fresh_root_own_ids_written":true}));
        h.actor.clone().shutdown().await.unwrap();
    }
    evidence("ordinary-source-combined-partial.json", &records);
}
fn native_combined(_: &mut Value, rules: &mut Value) {
    rules["timed_scenes"][1]["steps"] = json!([{ "source":"native combined target","offset_ms":0,"device":"combined","command":{"kind":"level","value":30,"ramp":false}}]);
    rules["timed_scenes"][3]["steps"] = json!([{ "source":"distinct noncombined target","offset_ms":0,"device":"switch","command":{"kind":"switch","on":true}}]);
}
#[tokio::test]
async fn native_combined_written_supply_does_not_authorize_later_dimmer_under_a_revoked_scene_only_source()
 {
    let mut records = vec![];
    for change in [
        None,
        Some("role"),
        Some("scope"),
        Some("revoke"),
        Some("logout"),
    ] {
        let mut h =
            Harness::configured(Case::Relative, change == Some("logout"), native_combined).await;
        assert!(
            h.source.grant.devices.is_empty(),
            "scene-only grant must not require hidden supply or dimmer IDs"
        );
        let task = h.call(Case::Relative);
        let (status, response) = h.finish_http(task).await;
        assert!([200, 202].contains(&status), "{response}");
        h.drain().await;
        assert!(
            h.bytes
                .lock()
                .unwrap()
                .as_chunks::<5>().0.iter().copied()
                .any(|b| b == [0xae, 3, 0xa2, 0xf1, 0xee]),
            "unknown supply did not receive its native Toggle"
        );
        assert!(
            !h.bytes
                .lock()
                .unwrap()
                .as_chunks::<5>().0.iter().copied()
                .any(|b| b[1] == 0x15),
            "conditional dimmer must wait for fresh supply query"
        );
        h.at(200).await;
        h.drain().await;
        h.clock.store(201, Ordering::Release);
        h.feedback
            .apply(&crate::transport_owner::GatewayRead {
                gateway_id: "bus".into(),
                transport_epoch: 1,
                received_at_ms: 201,
                result: hb_transport::Receive::Frames {
                    transport_epoch: 1,
                    frames: vec![hb_protocol::RawFrame {
                        address: 3,
                        command: 0xd2,
                        value: 0xf1,
                    }],
                },
            })
            .unwrap();
        let before = h.count_written();
        h.bytes.lock().unwrap().clear();
        if let Some(kind) = change {
            h.change(kind);
        }
        h.at(201).await;
        h.drain().await;
        let wire = h.bytes.lock().unwrap().clone();
        if change.is_some() {
            assert!(wire.is_empty());
            assert_eq!(h.count_written(), before);
        } else {
            assert_eq!(wire, [0xae, 0x15, 0xb2, 30, 0xee]);
            assert_eq!(h.count_written(), before + 1);
        }
        let sql = rusqlite::Connection::open(h.directory.join("runtime.sqlite")).unwrap();
        let written_supply:i64=sql.query_row("SELECT count(*) FROM command_journal WHERE device_id='supply' AND status='written' AND intent=x'01070000'",[],|r|r.get(0)).unwrap();
        assert_eq!(
            written_supply, 1,
            "written supply Toggle must survive and must never be replayed"
        );
        drop(sql);
        h.assert_no_pending_commands();
        h.fresh_root_proof().await;
        records.push(json!({"change":change,"written_supply_toggles":written_supply,"later_dimmer_wire":wire,"scene_only_scope":true,"fresh_root_own_ids_written":true}));
        h.actor.clone().shutdown().await.unwrap();
    }
    evidence("ordinary-source-native-combined-partial.json", &records);
}
