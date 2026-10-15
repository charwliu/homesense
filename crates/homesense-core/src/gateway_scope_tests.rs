use super::*;

fn table() -> GatewayTable {
    GatewayTable::new(
        Epochs {
            transport: 7,
            configuration: 3,
            control: 9,
        },
        ["bus-a".into(), "bus-b".into()],
    )
    .unwrap()
}
fn requirements() -> Vec<ReadRequirement> {
    (1..=8)
        .map(|channel| ReadRequirement {
            port: Port {
                address: 0x11,
                channel,
            },
            before_count: 4,
            sent_at_ms: 100,
            deadline_ms: 200,
        })
        .collect()
}
fn ready(table: &mut GatewayTable, gateway: &str) -> WireScope {
    let scope = table.begin_attempt(gateway).unwrap();
    table.begin_synchronization(&scope, requirements()).unwrap();
    for channel in 1..=8 {
        assert_eq!(
            table
                .received(
                    &scope,
                    Port {
                        address: 0x11,
                        channel
                    },
                    5,
                    120
                )
                .unwrap(),
            channel == 8
        );
    }
    scope
}

#[test]
fn identical_addresses_on_independent_buses_never_share_sync_progress() {
    let mut table = table();
    let a = table.begin_attempt("bus-a").unwrap();
    let b = table.begin_attempt("bus-b").unwrap();
    table.begin_synchronization(&a, requirements()).unwrap();
    table.begin_synchronization(&b, requirements()).unwrap();
    for channel in 1..=8 {
        table
            .received(
                &b,
                Port {
                    address: 0x11,
                    channel,
                },
                5,
                120,
            )
            .unwrap();
    }
    assert!(table.ready(&b));
    assert!(!table.ready(&a));
    assert!(
        !table
            .received(
                &a,
                Port {
                    address: 0x11,
                    channel: 1
                },
                5,
                120
            )
            .unwrap()
    );
    assert!(!table.ready(&a), "one of eight A ports cannot open A");
    assert!(table.capture(["bus-b".into()]).unwrap().current(&table));
    assert_eq!(table.capture(["bus-a".into()]), Err(Error::Unavailable));
}

#[test]
fn local_failure_preserves_healthy_scope_but_invalidates_cross_bus_dependencies() {
    let mut table = table();
    let a = ready(&mut table, "bus-a");
    let b = ready(&mut table, "bus-b");
    let only_b = table.capture(["bus-b".into()]).unwrap();
    let both = table.capture(["bus-a".into(), "bus-b".into()]).unwrap();
    let original_global = table.global();
    table.fault(&a).unwrap();
    assert!(table.ready(&b));
    assert!(only_b.current(&table));
    assert!(!both.current(&table));
    assert_eq!(table.global(), original_global);
    assert_eq!(only_b.wires(), &[b]);
}

#[test]
fn fresh_local_recovery_does_not_recapture_or_replay_old_dependencies() {
    let mut table = table();
    let a = ready(&mut table, "bus-a");
    let b = ready(&mut table, "bus-b");
    let old_a = table.capture(["bus-a".into()]).unwrap();
    let old_b = table.capture(["bus-b".into()]).unwrap();
    table.fault(&a).unwrap();
    let new_a = ready(&mut table, "bus-a");
    assert_eq!(new_a.epoch(), a.epoch() + 1);
    assert!(!old_a.current(&table));
    assert!(old_b.current(&table));
    assert_eq!(old_b.wires(), &[b]);
    assert!(table.capture(["bus-a".into()]).unwrap().current(&table));
    assert_eq!(table.fault(&a), Err(Error::StaleScope));
    assert!(table.ready(&new_a));
}

#[test]
fn synchronization_requires_actual_new_counts_and_bounded_current_epoch_samples() {
    let mut table = table();
    let a = table.begin_attempt("bus-a").unwrap();
    table.begin_synchronization(&a, requirements()).unwrap();
    let port = Port {
        address: 0x11,
        channel: 1,
    };
    for (count, at) in [(4, 120), (5, 99), (5, 201)] {
        assert!(!table.received(&a, port, count, at).unwrap());
    }
    table.fault(&a).unwrap();
    let new_a = table.begin_attempt("bus-a").unwrap();
    table.begin_synchronization(&new_a, requirements()).unwrap();
    assert_eq!(table.received(&a, port, 6, 120), Err(Error::StaleScope));
    assert!(!table.ready(&new_a));
}

#[test]
fn each_failed_attempt_consumes_unique_epoch_and_overflow_never_wraps() {
    let mut table = table();
    let a = table.begin_attempt("bus-a").unwrap();
    assert_eq!(table.begin_attempt("bus-a"), Err(Error::Busy));
    table.fault(&a).unwrap();
    let next = table.begin_attempt("bus-a").unwrap();
    assert_eq!(next.epoch(), a.epoch() + 1);
    table.fault(&next).unwrap();
    table.gateways.get_mut("bus-a").unwrap().epoch = u64::MAX;
    assert_eq!(table.begin_attempt("bus-a"), Err(Error::CounterExhausted));
    assert_eq!(table.phase("bus-a").unwrap(), Phase::Offline);
}

#[test]
fn invalid_sync_plan_leaves_connecting_bus_unready() {
    let mut table = table();
    let a = table.begin_attempt("bus-a").unwrap();
    let duplicate = requirements()[0];
    assert_eq!(
        table.begin_synchronization(&a, [duplicate, duplicate]),
        Err(Error::DuplicatePort)
    );
    assert_eq!(table.phase("bus-a").unwrap(), Phase::Connecting);
    assert!(!table.ready(&a));
    assert_eq!(table.begin_attempt("missing"), Err(Error::UnknownGateway));
}

#[test]
fn deduplicated_requirement_sets_do_not_invent_additional_scope_or_authority() {
    let mut table = table();
    let b = ready(&mut table, "bus-b");
    let read = table.capture(["bus-b".into(), "bus-b".into()]).unwrap();
    assert_eq!(read.wires(), &[b]);
    assert!(!read.depends_on("bus-a"));
    assert!(read.depends_on("bus-b"));
    assert!(table.capture([]).unwrap().current(&table));
}
