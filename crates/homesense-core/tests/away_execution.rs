use homesense_core::{DeviceId, away_execution::*};
use std::collections::{BTreeMap, BTreeSet};
// The historical ageing/override regressions exercise observations older than
// five seconds. Their explicit extended budget is independent of the production
// ten-second default, covered by the response tests below.
fn extended_policy() -> Policy {
    Policy { delay_ms: 1000, feedback_ms: 5000, total_ms: 30_000, ..Policy::default() }
}
fn id(module: usize, ch: usize) -> DeviceId {
    DeviceId::new(format!("m{module}:{ch}")).unwrap()
}
fn scope(count: usize) -> Vec<Module> {
    (0..count)
        .map(|n| Module {
            id: format!("m{n}"),
            outputs: (1..=8).map(|ch| id(n, ch)).collect(),
        })
        .collect()
}

#[test]
fn serialized_off_waits_for_all_eight_and_rx_turnaround_with_fresh_future_leases() {
    let mut operation=Operation::new_serialized(scope(12),Policy::default(),0).unwrap();
    let mut evidence=Evidence::default();
    let mut now=0;
    let mut seen=BTreeSet::new();
    for command in 1..=12 {
        let a=attempt(&mut operation,now,&evidence);
        assert_eq!(a.modules.len(),1);
        assert_eq!(a.round,1,"each module is in the same round");
        assert_eq!(a.send_before_ms,now+2000,"future tickets are minted only when selected");
        let module=a.modules.first().unwrap();
        assert!(seen.insert(module.clone()));
        let n:usize=module[1..].parse().unwrap();
        let receipts=accept(&mut operation,&a,command);
        evidence.commands.insert(command,CommandState::Accepted);
        assert_eq!(operation.poll(now,Owner::Current,&evidence).unwrap(),Decision::Wait);
        now+=1;written(&receipts,now,&mut evidence);
        off(n,now+1,2,&mut evidence);
        // Seven replies cannot release the next module.
        evidence.physical.remove(&id(n,8));
        assert_eq!(operation.poll(now+2,Owner::Current,&evidence).unwrap(),Decision::Wait);
        evidence.physical.insert(id(n,8),Sample{actual:Physical::Off,received_ms:now+3,count:2});
        assert_eq!(operation.poll(now+4,Owner::Waiting,&evidence).unwrap(),Decision::Wait);
        // Stay within the original10s, but accumulate >2s before later modules.
        now+=300;
        if command==12 {
            assert_eq!(operation.poll(now,Owner::Current,&evidence).unwrap(),Decision::Finished(End::Confirmed));
        }
    }
    assert_eq!(seen.len(),12);
    assert!(now>2000);
}

#[test]
fn serialized_off_failed_first_module_yields_to_all_healthy_modules_before_retry() {
    let mut operation=Operation::new_serialized(scope(3),Policy::default(),0).unwrap();
    let mut evidence=Evidence::default();
    let mut now=0;
    let mut writes=vec![];
    for command in 1..=5 {
        let a=attempt(&mut operation,now,&evidence);
        let module=a.modules.first().unwrap().clone();
        let n:usize=module[1..].parse().unwrap();
        writes.push((module,a.round));
        let receipts=accept(&mut operation,&a,command);
        now+=1;written(&receipts,now,&mut evidence);
        if n!=0 {off(n,now+1,2,&mut evidence);now+=30;}
        else {
            assert_eq!(operation.poll(now+100,Owner::Current,&evidence).unwrap(),Decision::Read(["m0".into()].into()));
            now+=1000;
        }
        if command==3 || command==4 {
            assert_eq!(operation.poll(now,Owner::Current,&evidence).unwrap(),Decision::Wait);
            now+=200;
        } else if command==5 {
            assert_eq!(operation.poll(now,Owner::Current,&evidence).unwrap(),Decision::Finished(End::Exhausted));
        }
    }
    assert_eq!(writes,vec![("m0".into(),1),("m1".into(),1),("m2".into(),1),("m0".into(),2),("m0".into(),3)]);
    assert_eq!(operation.terminal_missing().unwrap().keys().cloned().collect::<Vec<_>>(),vec!["m0"]);
}

#[test]
fn serialized_profile_window_accepts_delayed_passive_reply_without_early_query() {
    let mut operation = Operation::new_serialized_with_reply_windows(
        scope(1), Policy::default(), 0, [("m0".into(), 500)].into(),
    ).unwrap();
    let mut evidence = Evidence::default();
    let a = attempt(&mut operation, 0, &evidence);
    let receipts = accept(&mut operation, &a, 1);
    written(&receipts, 1, &mut evidence);
    for at in [101, 251, 399] {
        assert_eq!(operation.poll(at, Owner::Current, &evidence).unwrap(), Decision::Wait,
            "do not query over the module's delayed automatic response");
    }
    off(0, 400, 2, &mut evidence);
    assert_eq!(operation.poll(401, Owner::Current, &evidence).unwrap(), Decision::Finished(End::Confirmed));
    assert_eq!(operation.deadline_ms(), 10_000);
}

#[test]
fn serialized_profile_window_does_not_delay_fast_complete_replies_or_extend_missing_reply_budget() {
    for complete in [true, false] {
        let mut operation = Operation::new_serialized_with_reply_windows(
            scope(1), Policy::default(), 0, [("m0".into(), 500)].into(),
        ).unwrap();
        let mut evidence = Evidence::default();
        let a = attempt(&mut operation, 0, &evidence);
        let receipts = accept(&mut operation, &a, 1);
        written(&receipts, 1, &mut evidence);
        off(0, 20, 2, &mut evidence);
        if complete {
            assert_eq!(operation.poll(21, Owner::Current, &evidence).unwrap(), Decision::Finished(End::Confirmed));
        } else {
            evidence.physical.remove(&id(0, 8));
            assert_eq!(operation.poll(500, Owner::Current, &evidence).unwrap(), Decision::Wait);
            assert_eq!(operation.poll(501, Owner::Current, &evidence).unwrap(), Decision::Read(["m0".into()].into()));
            assert_eq!(operation.deadline_ms(), 10_000);
            assert_eq!(operation.poll(10_000, Owner::Current, &evidence).unwrap(), Decision::Finished(End::Deadline));
        }
    }
}

#[test]
fn serialized_profile_window_rejects_unknown_scope_and_unbounded_waits() {
    assert!(matches!(Operation::new_serialized_with_reply_windows(
        scope(1), Policy::default(), 0, [("unknown".into(), 500)].into(),
    ), Err(Error::InvalidScope)));
    for wait in [0, 1000, 1001] {
        assert!(matches!(Operation::new_serialized_with_reply_windows(
            scope(1), Policy::default(), 0, [("m0".into(), wait)].into(),
        ), Err(Error::InvalidPolicy)));
    }
}

#[test]
fn serialized_profile_window_is_module_scoped_and_leaves_unconfigured_query_timing_unchanged() {
    let mut operation = Operation::new_serialized_with_reply_windows(
        scope(2), Policy::default(), 0, [("m0".into(), 500)].into(),
    ).unwrap();
    let mut evidence = Evidence::default();
    let first = attempt(&mut operation, 0, &evidence);
    let receipts = accept(&mut operation, &first, 1);
    written(&receipts, 1, &mut evidence);
    off(0, 20, 2, &mut evidence);
    let second = attempt(&mut operation, 21, &evidence);
    assert_eq!(second.modules, ["m1".into()].into());
    let receipts = accept(&mut operation, &second, 2);
    written(&receipts, 22, &mut evidence);
    assert_eq!(operation.poll(121, Owner::Current, &evidence).unwrap(), Decision::Wait);
    assert_eq!(operation.poll(122, Owner::Current, &evidence).unwrap(), Decision::Read(["m1".into()].into()));
}

#[test]
fn serialized_profile_window_yields_complete_failure_but_not_a_partial_reply_burst() {
    for complete in [false, true] {
        let mut operation = Operation::new_serialized_with_reply_windows(
            scope(2), Policy::default(), 0, [("m0".into(), 500)].into(),
        ).unwrap();
        let mut evidence = Evidence::default();
        let first = attempt(&mut operation, 0, &evidence);
        let receipts = accept(&mut operation, &first, 1);
        written(&receipts, 1, &mut evidence);
        off(0, 20, 2, &mut evidence);
        evidence.physical.get_mut(&id(0, 1)).unwrap().actual = Physical::On;
        if complete {
            let next = attempt(&mut operation, 21, &evidence);
            assert_eq!(next.modules, ["m1".into()].into());
            assert!(operation.missing(&evidence, 21).unwrap().contains_key("m0"));
        } else {
            evidence.physical.remove(&id(0, 8));
            assert_eq!(operation.poll(21, Owner::Current, &evidence).unwrap(), Decision::Wait);
            assert_eq!(operation.poll(500, Owner::Current, &evidence).unwrap(), Decision::Wait);
        }
    }
}

#[test]
fn serialized_off_defer_does_not_skip_a_module_and_retirement_stops_future_proposals() {
    for cause in [Cancel::AuthorityChanged,Cancel::ConfigurationChanged,Cancel::TransportChanged,Cancel::ManualOverride] {
        let mut operation=Operation::new_serialized(scope(2),Policy::default(),0).unwrap();
        let evidence=Evidence::default();
        let first=attempt(&mut operation,0,&evidence);
        operation.defer(first.proposal,1).unwrap();
        let repeated=attempt(&mut operation,1,&evidence);
        assert_eq!(first.modules,repeated.modules);
        assert_eq!(repeated.round,1);
        assert_ne!(first.proposal,repeated.proposal);
        assert_eq!(operation.poll(2,Owner::Retired(cause),&evidence).unwrap(),Decision::Finished(End::Cancelled(cause)));
        assert_eq!(operation.poll(3,Owner::Current,&evidence).unwrap(),Decision::Finished(End::Cancelled(cause)));
    }
}

#[test]
fn serialized_off_completed_failed_read_advances_but_never_certifies_shutdown() {
    let mut operation=Operation::new_serialized(scope(2),Policy::default(),0).unwrap();
    let mut evidence=Evidence {queries:Some(Default::default()),..Evidence::default()};
    let a=attempt(&mut operation,0,&evidence);
    let receipts=accept(&mut operation,&a,1);
    written(&receipts,1,&mut evidence);
    evidence.queries.as_mut().unwrap().insert(id(0,1),ReadTurn{written_ms:101,finished_ms:351});
    assert_eq!(operation.poll(352,Owner::Waiting,&evidence).unwrap(),Decision::Wait);
    let b=attempt(&mut operation,372,&evidence);
    assert_eq!(b.modules,["m1".into()].into());
    assert_eq!(b.round,1);
    assert!(operation.missing(&evidence,372).unwrap().contains_key("m0"));
}

#[test]
fn serialized_off_late_confirmation_removes_only_that_module_from_the_pending_retry_round() {
    let mut operation=Operation::new_serialized(scope(3),Policy::default(),0).unwrap();
    let mut evidence=Evidence::default();
    let mut now=0;
    for command in 1..=3 {
        let a=attempt(&mut operation,now,&evidence);
        let receipts=accept(&mut operation,&a,command);
        written(&receipts,now+1,&mut evidence);now+=1001;
    }
    assert_eq!(operation.poll(now,Owner::Current,&evidence).unwrap(),Decision::Wait);
    now+=200;
    let retry=attempt(&mut operation,now,&evidence);
    assert_eq!(retry.modules,["m0".into()].into());assert_eq!(retry.round,2);
    let receipts=accept(&mut operation,&retry,4);
    written(&receipts,now+1,&mut evidence);
    off(0,now+2,2,&mut evidence);
    // m1's genuine first-round reply arrives before its second-round turn.
    off(1,now+3,2,&mut evidence);
    let remaining=attempt(&mut operation,now+25,&evidence);
    assert_eq!(remaining.modules,["m2".into()].into(),"confirmed m1 must not be written again");
    assert_eq!(remaining.round,2);
}
fn attempt(operation: &mut Operation, now: u64, evidence: &Evidence) -> Attempt {
    match operation.poll(now, Owner::Current, evidence).unwrap() {
        Decision::Submit(a) => a,
        other => panic!("expected bounded attempt, got {other:?}"),
    }
}
fn receipts(a: &Attempt, start: u64) -> Vec<Receipt> {
    a.modules
        .iter()
        .enumerate()
        .map(|(index, module)| {
            let n: usize = module[1..].parse().unwrap();
            Receipt {
                module: module.clone(),
                command: start + index as u64,
                accepted_ms: a.proposed_ms,
                send_before_ms: a.send_before_ms,
                before_counts: (1..=8).map(|ch| (id(n, ch), 1)).collect(),
            }
        })
        .collect()
}
fn accept(operation: &mut Operation, a: &Attempt, start: u64) -> Vec<Receipt> {
    let r = receipts(a, start);
    operation
        .accepted(a.proposal, r.clone(), a.proposed_ms)
        .unwrap();
    r
}
fn written(r: &[Receipt], at: u64, e: &mut Evidence) {
    for receipt in r {
        e.commands
            .insert(receipt.command, CommandState::Written { at_ms: at });
    }
}
fn off(module: usize, at: u64, count: u64, e: &mut Evidence) {
    for ch in 1..=8 {
        e.physical.insert(
            id(module, ch),
            Sample {
                actual: Physical::Off,
                received_ms: at,
                count,
            },
        );
    }
}
#[test]
fn complete_twelve_modules_requires_all96_postwrite_observations_and_no_second_attempt() {
    let mut o = Operation::new(scope(12), extended_policy(), 100).unwrap();
    let mut e = Evidence::default();
    let a = attempt(&mut o, 100, &e);
    assert_eq!(a.modules.len(), 12);
    let r = accept(&mut o, &a, 1);
    written(&r, 200, &mut e);
    for n in 0..12 {
        off(n, 190, 2, &mut e);
    }
    assert!(
        matches!(o.poll(210, Owner::Current, &e).unwrap(), Decision::Read(_)),
        "newer count but pre-write timestamps do not confirm closure"
    );
    for n in 0..12 {
        off(n, 220, 3, &mut e);
    }
    e.physical.remove(&id(11, 8));
    assert!(
        matches!(o.poll(230, Owner::Current, &e).unwrap(), Decision::Read(_)),
        "95 ports cannot confirm96"
    );
    e.physical.insert(
        id(11, 8),
        Sample {
            actual: Physical::Off,
            received_ms: 220,
            count: 3,
        },
    );
    assert_eq!(
        o.poll(240, Owner::Current, &e).unwrap(),
        Decision::Finished(End::Confirmed)
    );
    assert_eq!(o.rounds(), 1);
    assert_eq!(
        o.poll(250, Owner::Current, &e).unwrap(),
        Decision::Finished(End::Confirmed)
    );
}
#[test]
fn millisecond_replies_confirm_immediately_without_waiting_for_timeout() {
    let mut operation = Operation::new(scope(12), Policy::default(), 0).unwrap();
    let mut evidence = Evidence::default();
    let first = attempt(&mut operation, 0, &evidence);
    let receipts = accept(&mut operation, &first, 1);
    written(&receipts, 10, &mut evidence);
    for module in 0..12 {
        off(module, 33, 2, &mut evidence);
    }
    assert_eq!(operation.poll(34, Owner::Current, &evidence).unwrap(),
        Decision::Finished(End::Confirmed));
    assert_eq!(operation.rounds(), 1);
}
#[test]
fn silent_module_times_out_one_second_after_write_and_only_it_is_retried() {
    let mut operation = Operation::new(scope(12), Policy::default(), 0).unwrap();
    let mut evidence = Evidence::default();
    let first = attempt(&mut operation, 0, &evidence);
    let receipts = accept(&mut operation, &first, 1);
    written(&receipts, 10, &mut evidence);
    for module in 0..11 {
        off(module, 33, 2, &mut evidence);
    }
    assert!(matches!(operation.poll(34, Owner::Current, &evidence).unwrap(), Decision::Read(_)));
    assert!(matches!(operation.poll(1009, Owner::Current, &evidence).unwrap(), Decision::Read(_)));
    // The one-second window starts at Written(10), not at the send lease's
    // deadline(2000), and not at the ten-second operation ceiling.
    assert_eq!(operation.poll(1010, Owner::Current, &evidence).unwrap(), Decision::Wait);
    assert_eq!(operation.poll(1209, Owner::Current, &evidence).unwrap(), Decision::Wait);
    let retry = attempt(&mut operation, 1210, &evidence);
    assert_eq!(retry.modules, BTreeSet::from(["m11".into()]));
    assert_eq!(retry.round, 2);
    assert_eq!(operation.deadline_ms(), 10_000);
    assert_eq!(operation.missing(&evidence, 1210).unwrap(),
        BTreeMap::from([("m11".into(), (1..=8).map(|ch| id(11, ch)).collect())]));
}
#[test]
fn queued_write_time_is_not_charged_to_the_device_response_window() {
    let mut operation = Operation::new(scope(1), Policy::default(), 0).unwrap();
    let mut evidence = Evidence::default();
    let first = attempt(&mut operation, 0, &evidence);
    let receipts = accept(&mut operation, &first, 1);
    written(&receipts, 1900, &mut evidence);
    assert!(matches!(operation.poll(2899, Owner::Current, &evidence).unwrap(), Decision::Read(_)));
    assert_eq!(operation.poll(2900, Owner::Current, &evidence).unwrap(), Decision::Wait);
    assert_eq!(operation.poll(3099, Owner::Current, &evidence).unwrap(), Decision::Wait);
    assert_eq!(attempt(&mut operation, 3100, &evidence).round, 2);
}
#[test]
fn silent_channel_exhausts_three_one_second_windows_without_waiting_thirty_seconds() {
    let mut operation = Operation::new(scope(12), Policy::default(), 0).unwrap();
    let mut evidence = Evidence::default();
    for (round, now) in [(1, 0), (2, 1210), (3, 2420)] {
        let next = attempt(&mut operation, now, &evidence);
        assert_eq!(next.round, round);
        assert_eq!(next.modules.len(), if round == 1 { 12 } else { 1 });
        if round > 1 { assert_eq!(next.modules, BTreeSet::from(["m11".into()])); }
        let receipts = accept(&mut operation, &next, u64::from(round) * 20);
        written(&receipts, now + 10, &mut evidence);
        for module in 0..12 { off(module, now + 33, u64::from(round) + 1, &mut evidence); }
        evidence.physical.remove(&id(11, 8));
        assert!(matches!(operation.poll(now + 34, Owner::Current, &evidence).unwrap(), Decision::Read(_)));
        let decision = operation.poll(now + 1010, Owner::Current, &evidence).unwrap();
        assert_eq!(decision, if round == 3 { Decision::Finished(End::Exhausted) } else { Decision::Wait });
    }
    assert_eq!(operation.terminal_missing(), Some(&BTreeMap::from([
        ("m11".into(), BTreeSet::from([id(11, 8)]))
    ])));
    assert_eq!(operation.deadline_ms(), 10_000);
}
#[test]
fn queued_serial_reads_are_not_device_timeouts_and_do_not_manufacture_confirmation() {
    let mut operation = Operation::new(scope(12), Policy::default(), 0).unwrap();
    let mut evidence = Evidence { queries: Some(BTreeMap::new()), ..Evidence::default() };
    let first = attempt(&mut operation, 0, &evidence);
    let receipts = accept(&mut operation, &first, 1);
    written(&receipts, 10, &mut evidence);
    // The gateway is serial: later modules have not yet had a query written.
    // Waiting in that queue must not cause redundant module-Off retries.
    for module in 0..12 {
        let at = 50 + module as u64 * 200;
        for channel in 1..=8 {
            evidence.queries.as_mut().unwrap().insert(id(module, channel), ReadTurn { written_ms: at, finished_ms: at + 23 });
        }
        // Query completion alone is never proof that an output is Off.
        if module == 11 {
            assert_eq!(operation.poll(at + 23, Owner::Current, &evidence).unwrap(), Decision::Wait);
            assert_eq!(operation.rounds(), 1);
        }
        off(module, at + 23, 2, &mut evidence);
        let decision = operation.poll(at + 24, Owner::Current, &evidence).unwrap();
        assert_eq!(operation.rounds(), 1);
        if module == 11 { assert_eq!(decision, Decision::Finished(End::Confirmed)); }
        else { assert!(matches!(decision, Decision::Read(_))); }
    }
}
#[test]
fn whole_house_serial_confirmation_can_finish_after_five_seconds_without_extending_module_timeouts() {
    let policy=Policy::default();
    assert_eq!((policy.total_ms,policy.feedback_ms),(10_000,10_000));
    assert_eq!((policy.response_ms,policy.send_ms,policy.delay_ms,policy.max_rounds),(1000,2000,200,3));
    let mut operation=Operation::new(scope(12),policy,0).unwrap();
    let mut evidence=Evidence{queries:Some(BTreeMap::new()),..Evidence::default()};
    let first=attempt(&mut operation,0,&evidence);let receipts=accept(&mut operation,&first,1);
    written(&receipts,10,&mut evidence);
    for module in 0..12 {
        let at=50+module as u64*500;
        for channel in 1..=8 {
            evidence.queries.as_mut().unwrap().insert(id(module,channel),ReadTurn{written_ms:at,finished_ms:at+20});
        }
        off(module,at+20,2,&mut evidence);
        let result=operation.poll(at+21,Owner::Current,&evidence).unwrap();
        if module==11 {assert_eq!(result,Decision::Finished(End::Confirmed));}
        else {assert!(matches!(result,Decision::Read(_)));}
        assert_eq!(operation.rounds(),1,"queued reads must not invent a failed device or redundant write");
        assert_eq!(operation.deadline_ms(),10_000,"progress never renews the operation ceiling");
    }
}
#[test]
fn five_second_deadline_reports_only_silent_output_even_if_query_queue_never_drains() {
    let mut operation = Operation::new(scope(12), Policy {total_ms:5000,..Policy::default()}, 0).unwrap();
    let mut evidence = Evidence { queries: Some(BTreeMap::new()), ..Evidence::default() };
    let first = attempt(&mut operation, 0, &evidence);
    let receipts = accept(&mut operation, &first, 1);
    written(&receipts, 10, &mut evidence);
    for module in 0..12 { off(module, 33, 2, &mut evidence); }
    evidence.physical.remove(&id(11, 8));
    assert!(matches!(operation.poll(4999, Owner::Current, &evidence).unwrap(), Decision::Read(_)));
    assert_eq!(operation.poll(5000, Owner::Current, &evidence).unwrap(), Decision::Finished(End::Deadline));
    assert_eq!(operation.terminal_missing(), Some(&BTreeMap::from([("m11".into(), BTreeSet::from([id(11, 8)]))])));
}
#[test]
fn completed_missing_read_does_not_wait_for_a_second_unsolicited_reply_timeout() {
    let mut operation = Operation::new(scope(1), Policy::default(), 0).unwrap();
    let mut evidence = Evidence { queries: Some(BTreeMap::new()), ..Evidence::default() };
    let first = attempt(&mut operation, 0, &evidence);
    let receipts = accept(&mut operation, &first, 1);
    written(&receipts, 10, &mut evidence);
    off(0, 33, 2, &mut evidence);
    evidence.physical.remove(&id(0, 8));
    assert!(matches!(operation.poll(200, Owner::Current, &evidence).unwrap(), Decision::Read(_)));
    evidence.queries.as_mut().unwrap().insert(id(0, 8), ReadTurn { written_ms: 50, finished_ms: 550 });
    assert_eq!(operation.poll(550, Owner::Current, &evidence).unwrap(), Decision::Wait);
    assert_eq!(operation.poll(749, Owner::Current, &evidence).unwrap(), Decision::Wait);
    assert_eq!(attempt(&mut operation, 750, &evidence).modules, BTreeSet::from(["m0".into()]));
    assert_eq!(operation.missing(&evidence, 750).unwrap(), BTreeMap::from([("m0".into(), BTreeSet::from([id(0, 8)]))]));
}
#[test]
fn a_silent_module_retries_after_its_actual_failed_query_without_inventing_seven_reads() {
    let mut operation = Operation::new(scope(2), Policy::default(), 0).unwrap();
    let mut evidence = Evidence { queries: Some(BTreeMap::new()), ..Evidence::default() };
    let first = attempt(&mut operation, 0, &evidence);
    let receipts = accept(&mut operation, &first, 1);
    written(&receipts, 10, &mut evidence);
    off(0, 33, 2, &mut evidence);
    evidence.queries.as_mut().unwrap().insert(id(1, 1), ReadTurn { written_ms: 50, finished_ms: 550 });
    assert_eq!(operation.poll(550, Owner::Current, &evidence).unwrap(), Decision::Wait);
    assert_eq!(attempt(&mut operation, 750, &evidence).modules, BTreeSet::from(["m1".into()]));
    assert_eq!(evidence.queries.as_ref().unwrap().len(), 1);
    assert_eq!(operation.missing(&evidence, 750).unwrap(), BTreeMap::from([("m1".into(), (1..=8).map(|ch| id(1, ch)).collect())]));
}
#[test]
fn retry_only_failed_module_after_one_second_and_install_new_receipts() {
    let mut o = Operation::new(scope(12), extended_policy(), 0).unwrap();
    let mut e = Evidence::default();
    let a = attempt(&mut o, 0, &e);
    let r = accept(&mut o, &a, 1);
    written(&r, 10, &mut e);
    for n in 0..12 {
        off(n, 20, 2, &mut e);
    }
    e.physical.get_mut(&id(7, 8)).unwrap().actual = Physical::On;
    assert!(matches!(
        o.poll(30, Owner::Current, &e).unwrap(),
        Decision::Read(_)
    ));
    assert_eq!(o.poll(7000, Owner::Current, &e).unwrap(), Decision::Wait);
    // Refresh successful modules without another write. Existing proof/count
    // remains tied to the first attempt; only m7 belongs to the next attempt.
    for n in 0..12 {
        if n != 7 {
            off(n, 7500, 3, &mut e);
        }
    }
    assert_eq!(o.poll(7999, Owner::Current, &e).unwrap(), Decision::Wait);
    let b = attempt(&mut o, 8000, &e);
    assert_eq!(b.modules, BTreeSet::from(["m7".into()]));
    assert_eq!(b.round, 2);
    let mut reused = receipts(&b, 1);
    reused[0].command = r.iter().find(|r| r.module == "m7").unwrap().command;
    assert_eq!(
        o.accepted(b.proposal, reused, 8000),
        Err(Error::InvalidReceipt)
    );
    let second = accept(&mut o, &b, 100);
    written(&second, 8010, &mut e);
    off(7, 8020, 4, &mut e);
    assert_eq!(
        o.poll(8030, Owner::Current, &e).unwrap(),
        Decision::Finished(End::Confirmed)
    );
    assert_eq!(o.rounds(), 2);
}
#[test]
fn three_rounds_then_precise_missing_channel_failure() {
    let mut o = Operation::new(scope(1), extended_policy(), 0).unwrap();
    let mut e = Evidence::default();
    for (round, now) in [(1, 0), (2, 8000), (3, 16000)] {
        let a = attempt(&mut o, now, &e);
        assert_eq!(a.round, round);
        let r = accept(&mut o, &a, round as u64);
        written(&r, now + 10, &mut e);
        off(0, now + 20, round as u64 + 2, &mut e);
        e.physical.remove(&id(0, 8));
        if round == 3 {
            for ch in 1..=7 {
                e.physical.insert(
                    id(0, ch),
                    Sample {
                        actual: Physical::Off,
                        received_ms: now + 6999,
                        count: 9,
                    },
                );
            }
        }
        let d = o.poll(now + 7000, Owner::Current, &e).unwrap();
        if round < 3 {
            assert_eq!(d, Decision::Wait);
        } else {
            assert_eq!(d, Decision::Finished(End::Exhausted));
            assert_eq!(
                o.terminal_missing(),
                Some(&BTreeMap::from([("m0".into(), BTreeSet::from([id(0, 8)]))]))
            );
            off(0, now + 7000, 10, &mut e);
            assert!(o.missing(&e, now + 7000).unwrap().is_empty());
            assert_eq!(
                o.terminal_missing(),
                Some(&BTreeMap::from([("m0".into(), BTreeSet::from([id(0, 8)]))])),
                "new observations cannot rewrite terminal evidence"
            );
        }
    }
    assert_eq!(o.rounds(), 3);
    assert_eq!(
        o.poll(24000, Owner::Current, &e).unwrap(),
        Decision::Finished(End::Exhausted)
    );
}
#[test]
fn waiting_for_query_or_definite_busy_does_not_consume_rounds_or_renew_a_ticket() {
    let mut o = Operation::new(scope(1), extended_policy(), 0).unwrap();
    let e = Evidence::default();
    for now in [0, 1000, 2000, 5000] {
        assert_eq!(o.poll(now, Owner::Waiting, &e).unwrap(), Decision::Wait);
        assert_eq!(o.rounds(), 0);
        assert!(o.needs_admission(now));
    }
    let a = attempt(&mut o, 5500, &e);
    assert_eq!(a.send_before_ms, 7500);
    o.defer(a.proposal, 5510).unwrap();
    let b = attempt(&mut o, 5600, &e);
    assert_ne!(a.proposal, b.proposal);
    assert_eq!(b.round, 1);
    assert_eq!(
        o.accepted(a.proposal, receipts(&a, 1), 5600),
        Err(Error::InvalidReceipt)
    );
    accept(&mut o, &b, 2);
    assert_eq!(o.rounds(), 1);
    assert_eq!(o.deadline_ms(), 30000);
}
#[test]
fn exhausted_writes_refresh_only_aged_healthy_modules_before_freezing_failure_scope() {
    let mut o=Operation::new(scope(2),extended_policy(),0).unwrap();
    let mut e=Evidence::default();
    for (round,now) in [(1,0),(2,8000),(3,16000)] {
        let a=attempt(&mut o,now,&e);
        assert_eq!(a.modules,if round==1{BTreeSet::from(["m0".into(),"m1".into()])}else{BTreeSet::from(["m1".into()])});
        let r=accept(&mut o,&a,round*10);written(&r,now+10,&mut e);
        if round==1 {off(0,20,2,&mut e);}
        assert!(matches!(o.poll(now+30,Owner::Current,&e).unwrap(),Decision::Read(_)));
        if round<3 {assert_eq!(o.poll(now+7000,Owner::Current,&e).unwrap(),Decision::Wait);}
    }
    let deadline=o.deadline_ms();
    assert_eq!(o.poll(23000,Owner::Current,&e).unwrap(),Decision::Read(BTreeSet::from(["m0".into()])));
    assert_eq!(o.rounds(),3);assert!(!o.needs_admission(23000));assert_eq!(o.deadline_ms(),deadline);
    assert!(o.terminal_missing().is_none());
    off(0,23100,3,&mut e);
    assert_eq!(o.poll(23101,Owner::Current,&e).unwrap(),Decision::Finished(End::Exhausted));
    assert_eq!(o.terminal_missing(),Some(&BTreeMap::from([("m1".into(),(1..=8).map(|ch|id(1,ch)).collect())])));
}
#[test]
fn final_read_phase_never_renews_budget_or_suppresses_override_and_late_recovery() {
    for exit in 0..3 {
        let mut o=Operation::new(scope(2),Policy{max_rounds:1,..extended_policy()},0).unwrap();
        let mut e=Evidence::default();let a=attempt(&mut o,0,&e);let r=accept(&mut o,&a,1);written(&r,10,&mut e);
        off(0,20,2,&mut e);assert!(matches!(o.poll(30,Owner::Current,&e).unwrap(),Decision::Read(_)));
        assert_eq!(o.poll(7000,Owner::Current,&e).unwrap(),Decision::Read(BTreeSet::from(["m0".into()])));
        let (now,owner,end)=match exit {
            0=>(30000,Owner::Current,End::Deadline),
            1=>(7100,Owner::Retired(Cancel::ManualOverride),End::Cancelled(Cancel::ManualOverride)),
            _=>{off(0,7100,3,&mut e);off(1,7100,2,&mut e);(7101,Owner::Current,End::Confirmed)},
        };
        assert_eq!(o.poll(now,owner,&e).unwrap(),Decision::Finished(end));assert_eq!(o.rounds(),1);assert_eq!(o.deadline_ms(),30000);
    }
}
#[test]
fn exhausted_partial_module_refreshes_its_aged_off_ports_instead_of_reporting_all_eight_missing() {
    let mut o=Operation::new(scope(1),Policy{max_rounds:1,..extended_policy()},0).unwrap();
    let mut e=Evidence::default();let a=attempt(&mut o,0,&e);let r=accept(&mut o,&a,1);written(&r,10,&mut e);
    off(0,20,2,&mut e);e.physical.remove(&id(0,8));
    assert!(matches!(o.poll(30,Owner::Current,&e).unwrap(),Decision::Read(_)));
    assert_eq!(o.poll(7000,Owner::Current,&e).unwrap(),Decision::Read(BTreeSet::from(["m0".into()])));
    off(0,7100,3,&mut e);e.physical.remove(&id(0,8));
    assert_eq!(o.poll(7101,Owner::Current,&e).unwrap(),Decision::Finished(End::Exhausted));
    assert_eq!(o.terminal_missing(),Some(&BTreeMap::from([("m0".into(),BTreeSet::from([id(0,8)]))])));
    assert_eq!(o.rounds(),1);assert_eq!(o.deadline_ms(),30000);
}
#[test]
fn accepted_or_writing_ticket_cannot_be_replayed_after_its_send_deadline() {
    for state in [CommandState::Accepted, CommandState::Writing] {
        let mut o = Operation::new(scope(1), extended_policy(), 0).unwrap();
        let mut e = Evidence::default();
        let a = attempt(&mut o, 0, &e);
        let r = accept(&mut o, &a, 1);
        e.commands.insert(r[0].command, state);
        for now in [2000, 7000, 15000, 29999] {
            assert_eq!(o.poll(now, Owner::Current, &e).unwrap(), Decision::Wait);
            assert_eq!(o.rounds(), 1);
        }
        assert_eq!(
            o.poll(30000, Owner::Current, &e).unwrap(),
            Decision::Finished(End::Deadline)
        );
    }
}
#[test]
fn no_feedback_or_unconfirmed_actual_is_never_a_successful_write() {
    for physical in [None, Some(Physical::Unknown), Some(Physical::On)] {
        let mut o = Operation::new(scope(1), extended_policy(), 0).unwrap();
        let mut e = Evidence::default();
        let a = attempt(&mut o, 0, &e);
        let r = accept(&mut o, &a, 1);
        written(&r, 10, &mut e);
        if let Some(actual) = physical {
            for ch in 1..=8 {
                e.physical.insert(
                    id(0, ch),
                    Sample {
                        actual,
                        received_ms: 20,
                        count: 2,
                    },
                );
            }
        }
        assert!(matches!(
            o.poll(30, Owner::Current, &e).unwrap(),
            Decision::Read(_)
        ));
        assert_eq!(o.poll(7000, Owner::Current, &e).unwrap(), Decision::Wait);
        assert_eq!(
            attempt(&mut o, 8000, &e).modules,
            BTreeSet::from(["m0".into()])
        );
    }
}
#[test]
fn source_configuration_transport_and_manual_invalidation_cancel_without_retry() {
    for cause in [
        Cancel::AuthorityChanged,
        Cancel::ConfigurationChanged,
        Cancel::TransportChanged,
        Cancel::ManualOverride,
        Cancel::Stopped,
        Cancel::AdmissionUnknown,
    ] {
        let mut o = Operation::new(scope(1), extended_policy(), 0).unwrap();
        let mut e = Evidence::default();
        let a = attempt(&mut o, 0, &e);
        let r = accept(&mut o, &a, 1);
        written(&r, 10, &mut e);
        assert_eq!(o.poll(7000, Owner::Current, &e).unwrap(), Decision::Wait);
        assert_eq!(
            o.poll(7500, Owner::Retired(cause), &e).unwrap(),
            Decision::Finished(End::Cancelled(cause))
        );
        assert_eq!(
            o.poll(8000, Owner::Current, &e).unwrap(),
            Decision::Finished(End::Cancelled(cause))
        );
    }
}
#[test]
fn ageing_a_confirmed_off_requests_only_read_without_redundant_write() {
    let mut o = Operation::new(scope(2), extended_policy(), 0).unwrap();
    let mut e = Evidence::default();
    let a = attempt(&mut o, 0, &e);
    let r = accept(&mut o, &a, 1);
    written(&r, 10, &mut e);
    off(0, 20, 2, &mut e);
    assert!(matches!(
        o.poll(30, Owner::Current, &e).unwrap(),
        Decision::Read(_)
    ));
    assert_eq!(o.poll(7000, Owner::Current, &e).unwrap(), Decision::Wait);
    let b = attempt(&mut o, 8000, &e);
    assert_eq!(b.modules, BTreeSet::from(["m1".into()]));
    let r = accept(&mut o, &b, 10);
    written(&r, 8010, &mut e);
    off(1, 8020, 2, &mut e);
    assert_eq!(
        o.poll(8030, Owner::Current, &e).unwrap(),
        Decision::Read(BTreeSet::from(["m0".into()]))
    );
    off(0, 8040, 3, &mut e);
    assert_eq!(
        o.poll(8050, Owner::Waiting, &e).unwrap(),
        Decision::Wait,
        "unavailable owner cannot declare completion from cached truth"
    );
    assert_eq!(
        o.poll(8060, Owner::Current, &e).unwrap(),
        Decision::Finished(End::Confirmed)
    );
}
#[test]
fn malformed_scope_receipts_clock_or_late_write_cannot_establish_success() {
    let policy = extended_policy();
    assert_eq!(
        Operation::new(vec![], policy, 0).unwrap_err(),
        Error::InvalidScope
    );
    let mut duplicate = scope(2);
    duplicate[1].outputs = duplicate[0].outputs.clone();
    assert_eq!(
        Operation::new(duplicate, policy, 0).unwrap_err(),
        Error::InvalidScope
    );
    let mut o = Operation::new(scope(1), policy, 10).unwrap();
    let mut e = Evidence::default();
    assert_eq!(o.poll(9, Owner::Current, &e), Err(Error::ClockRegression));
    let a = attempt(&mut o, 10, &e);
    let mut bad = receipts(&a, 1);
    bad[0].before_counts.remove(&id(0, 8));
    assert_eq!(o.accepted(a.proposal, bad, 10), Err(Error::InvalidReceipt));
    let r = accept(&mut o, &a, 1);
    written(&r, a.send_before_ms, &mut e);
    off(0, a.send_before_ms + 1, 2, &mut e);
    assert_eq!(
        o.poll(a.send_before_ms + 2, Owner::Current, &e),
        Err(Error::InvalidEvidence)
    );
}

#[test]
fn late_confirmation_during_delay_releases_admission_fence_for_read_only_completion() {
    let mut o = Operation::new(scope(2), extended_policy(), 0).unwrap();
    let mut e = Evidence::default();
    let a = attempt(&mut o, 0, &e);
    let r = accept(&mut o, &a, 1);
    written(&r, 10, &mut e);
    off(0, 20, 2, &mut e);
    o.poll(30, Owner::Current, &e).unwrap();
    assert_eq!(o.poll(7000, Owner::Current, &e).unwrap(), Decision::Wait);
    off(1, 7500, 2, &mut e);
    assert_eq!(
        o.poll(8000, Owner::Current, &e).unwrap(),
        Decision::Read(BTreeSet::from(["m0".into()]))
    );
    assert!(
        !o.needs_admission(8000),
        "read-only confirmation cannot be fenced as if another Off were needed"
    );
    off(0, 8010, 3, &mut e);
    assert_eq!(
        o.poll(8020, Owner::Current, &e).unwrap(),
        Decision::Finished(End::Confirmed)
    );
    assert_eq!(o.rounds(), 1);
}
#[test]
fn newer_on_invalidates_previous_confirmation_while_other_module_is_pending() {
    let mut o = Operation::new(scope(2), extended_policy(), 0).unwrap();
    let mut e = Evidence::default();
    let a = attempt(&mut o, 0, &e);
    let r = accept(&mut o, &a, 1);
    written(&r, 10, &mut e);
    off(0, 20, 2, &mut e);
    o.poll(30, Owner::Current, &e).unwrap();
    e.physical.insert(
        id(0, 1),
        Sample {
            actual: Physical::On,
            received_ms: 100,
            count: 3,
        },
    );
    assert_eq!(o.poll(7000, Owner::Current, &e).unwrap(), Decision::Wait);
    assert_eq!(
        attempt(&mut o, 8000, &e).modules,
        BTreeSet::from(["m0".into(), "m1".into()])
    );
}
#[test]
fn a_definitively_retired_failed_ticket_only_retries_its_original_module() {
    let mut o = Operation::new(scope(2), extended_policy(), 0).unwrap();
    let mut e = Evidence::default();
    let a = attempt(&mut o, 0, &e);
    let r = accept(&mut o, &a, 1);
    written(&r, 10, &mut e);
    off(0, 20, 2, &mut e);
    e.commands.insert(
        r.iter().find(|r| r.module == "m1").unwrap().command,
        CommandState::Retired,
    );
    // Even a matching actual Off cannot certify a ticket that failed its full
    // write. The next attempt is explicit Off with a new command receipt.
    off(1, 20, 2, &mut e);
    assert_eq!(o.poll(30, Owner::Current, &e).unwrap(), Decision::Wait);
    assert_eq!(o.poll(1029, Owner::Current, &e).unwrap(), Decision::Wait);
    let b = attempt(&mut o, 1030, &e);
    assert_eq!(b.modules, BTreeSet::from(["m1".into()]));
    let r = accept(&mut o, &b, 10);
    written(&r, 1040, &mut e);
    off(0, 1050, 3, &mut e);
    off(1, 1050, 3, &mut e);
    assert_eq!(
        o.poll(1060, Owner::Current, &e).unwrap(),
        Decision::Finished(End::Confirmed)
    );
}
#[test]
fn repeated_poll_and_duplicate_acceptance_cannot_create_another_attempt() {
    let mut o = Operation::new(scope(1), extended_policy(), 0).unwrap();
    let e = Evidence::default();
    let a = attempt(&mut o, 0, &e);
    assert_eq!(o.poll(1, Owner::Current, &e).unwrap(), Decision::Wait);
    let r = receipts(&a, 1);
    o.accepted(a.proposal, r.clone(), 2).unwrap();
    assert_eq!(o.accepted(a.proposal, r, 3), Err(Error::InvalidReceipt));
    assert_eq!(o.rounds(), 1);
    assert_eq!(o.poll(4, Owner::Current, &e).unwrap(), Decision::Wait);
}
