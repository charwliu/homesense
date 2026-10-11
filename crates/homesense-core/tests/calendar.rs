use homesense_core::calendar::{Calendar, Error, Schedule};
// 2000-01-03 Monday, 00:00:00 Asia/Shanghai.
const MONDAY: i64 = 946_828_800_000;
fn daily(id: &str, hour: u8, minute: u8) -> Schedule {
    Schedule {
        id: id.into(),
        hour,
        minute,
        weekdays: [true; 7],
    }
}
#[test]
fn local_weekday_and_same_time_order_with_no_startup_replay() {
    let mut monday = daily("monday", 8, 0);
    monday.weekdays = [true, false, false, false, false, false, false];
    let due = MONDAY + 8 * 3_600_000;
    let mut c = Calendar::new(
        vec![monday, daily("daily", 8, 0)],
        due - 1000,
        0,
        None,
        2000,
    )
    .unwrap();
    let p = c.poll(due, 1000).unwrap();
    assert_eq!(
        p.occurrences
            .iter()
            .map(|o| o.schedule.as_str())
            .collect::<Vec<_>>(),
        ["monday", "daily"]
    );
    assert!(p.occurrences.iter().all(|o| o.due_unix_ms == due));
    assert!(c.poll(due, 1000).unwrap().occurrences.is_empty());
    let p = c.poll(due + 86_400_000, 86_401_000).unwrap();
    assert_eq!(p.occurrences.len(), 1);
    assert_eq!(p.occurrences[0].schedule, "daily");
    let mut restarted = Calendar::new(vec![daily("daily", 8, 0)], due, 0, None, 2000).unwrap();
    assert!(
        restarted
            .poll(due + 1000, 1000)
            .unwrap()
            .occurrences
            .is_empty()
    );
}
#[test]
fn wall_jumps_skip_and_rollback_does_not_repeat_even_with_restored_watermark() {
    let due = MONDAY + 60_000;
    let mut c = Calendar::new(vec![daily("a", 0, 1)], due - 1000, 0, None, 2000).unwrap();
    let saved = c.poll(due, 1000).unwrap().watermark_unix_ms;
    assert!(c.poll(due - 60_000, 2000).unwrap().clock_jump);
    assert!(c.poll(due, 62_000).unwrap().occurrences.is_empty());
    let mut restored =
        Calendar::new(vec![daily("a", 0, 1)], due - 1000, 0, Some(saved), 2000).unwrap();
    assert!(restored.poll(due, 1000).unwrap().occurrences.is_empty());
    let p = c.poll(due + 86_400_000, 63_000).unwrap();
    assert!(p.clock_jump);
    assert!(p.occurrences.is_empty());
}
#[test]
fn midnight_normal_delay_and_long_suspend_have_no_backlog() {
    let mut c = Calendar::new(vec![daily("midnight", 0, 0)], MONDAY - 500, 0, None, 2000).unwrap();
    assert_eq!(c.poll(MONDAY + 500, 1000).unwrap().occurrences.len(), 1);
    assert!(
        c.poll(MONDAY + 3 * 86_400_000 + 60_000, 3 * 86_400_000 + 60_500)
            .unwrap()
            .occurrences
            .is_empty()
    );
}
#[test]
fn invalid_times_stop_and_invalid_schedule_is_rejected() {
    assert!(matches!(
        Calendar::new(vec![daily("x", 24, 0)], MONDAY, 0, None, 1000),
        Err(Error::InvalidConfiguration)
    ));
    let mut c = Calendar::new(vec![], MONDAY, 10, None, 1000).unwrap();
    assert!(matches!(c.poll(MONDAY, 9), Err(Error::InvalidClock)));
    assert!(matches!(c.poll(MONDAY, 11), Err(Error::Stopped)));
}
