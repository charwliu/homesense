use homesense_core::{DeviceId, away_execution::*};
use std::collections::{BTreeMap, BTreeSet};
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
    let mut o = Operation::new(scope(12), Policy::default(), 100).unwrap();
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
fn retry_only_failed_module_after_one_second_and_install_new_receipts() {
    let mut o = Operation::new(scope(12), Policy::default(), 0).unwrap();
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
    let mut o = Operation::new(scope(1), Policy::default(), 0).unwrap();
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
    let mut o = Operation::new(scope(1), Policy::default(), 0).unwrap();
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
    let mut o=Operation::new(scope(2),Policy::default(),0).unwrap();
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
        let mut o=Operation::new(scope(2),Policy{max_rounds:1,..Policy::default()},0).unwrap();
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
    let mut o=Operation::new(scope(1),Policy{max_rounds:1,..Policy::default()},0).unwrap();
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
        let mut o = Operation::new(scope(1), Policy::default(), 0).unwrap();
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
        let mut o = Operation::new(scope(1), Policy::default(), 0).unwrap();
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
        let mut o = Operation::new(scope(1), Policy::default(), 0).unwrap();
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
    let mut o = Operation::new(scope(2), Policy::default(), 0).unwrap();
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
    let policy = Policy::default();
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
    let mut o = Operation::new(scope(2), Policy::default(), 0).unwrap();
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
    let mut o = Operation::new(scope(2), Policy::default(), 0).unwrap();
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
    let mut o = Operation::new(scope(2), Policy::default(), 0).unwrap();
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
    assert!(matches!(
        o.poll(30, Owner::Current, &e).unwrap(),
        Decision::Read(_)
    ));
    assert_eq!(o.poll(7000, Owner::Current, &e).unwrap(), Decision::Wait);
    let b = attempt(&mut o, 8000, &e);
    assert_eq!(b.modules, BTreeSet::from(["m1".into()]));
    let r = accept(&mut o, &b, 10);
    written(&r, 8010, &mut e);
    off(0, 8020, 3, &mut e);
    off(1, 8020, 3, &mut e);
    assert_eq!(
        o.poll(8030, Owner::Current, &e).unwrap(),
        Decision::Finished(End::Confirmed)
    );
}
#[test]
fn repeated_poll_and_duplicate_acceptance_cannot_create_another_attempt() {
    let mut o = Operation::new(scope(1), Policy::default(), 0).unwrap();
    let e = Evidence::default();
    let a = attempt(&mut o, 0, &e);
    assert_eq!(o.poll(1, Owner::Current, &e).unwrap(), Decision::Wait);
    let r = receipts(&a, 1);
    o.accepted(a.proposal, r.clone(), 2).unwrap();
    assert_eq!(o.accepted(a.proposal, r, 3), Err(Error::InvalidReceipt));
    assert_eq!(o.rounds(), 1);
    assert_eq!(o.poll(4, Owner::Current, &e).unwrap(), Decision::Wait);
}
