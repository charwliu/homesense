use homesense_core::gesture::{Error, Event::*, KeyGesture, Settings};
fn key(double: bool) -> KeyGesture {
    KeyGesture::new(
        Settings {
            double_click_enabled: double,
            ..Settings::default()
        },
        0,
    )
    .unwrap()
}
#[test]
fn short_release_is_not_lost_to_debounce_and_duplicates_do_not_repeat() {
    let mut k = key(false);
    assert_eq!(k.edge(true, 100).unwrap(), vec![Pressed]);
    assert!(k.edge(true, 110).unwrap().is_empty());
    assert_eq!(k.edge(false, 120).unwrap(), vec![Released, Click]);
    assert!(k.edge(false, 125).unwrap().is_empty());
    assert!(k.edge(true, 140).unwrap().is_empty());
    assert!(k.edge(false, 145).unwrap().is_empty());
    assert_eq!(k.edge(true, 150).unwrap(), vec![Pressed]);
}
#[test]
fn double_click_suppresses_single_and_exact_window_boundary_is_single() {
    let mut k = key(true);
    k.edge(true, 0).unwrap();
    assert_eq!(k.edge(false, 50).unwrap(), vec![Released]);
    assert!(k.advance(349).unwrap().is_empty());
    assert_eq!(k.edge(true, 350).unwrap(), vec![Click, Pressed]);
    k.edge(false, 400).unwrap();
    k.edge(true, 450).unwrap();
    assert_eq!(k.edge(false, 500).unwrap(), vec![Released, DoubleClick]);
    assert!(k.advance(1000).unwrap().is_empty());
}
#[test]
fn long_press_starts_once_and_release_ends_without_click() {
    let mut k = key(true);
    k.edge(true, 0).unwrap();
    assert!(k.advance(599).unwrap().is_empty());
    assert_eq!(k.advance(600).unwrap(), vec![LongStart]);
    assert!(k.edge(true, 800).unwrap().is_empty());
    assert_eq!(k.edge(false, 1000).unwrap(), vec![Released, LongEnd]);
    assert!(k.advance(2000).unwrap().is_empty());
}
#[test]
fn missing_release_and_stalled_scheduler_cancel_without_late_start() {
    let mut k = key(false);
    k.edge(true, 0).unwrap();
    assert_eq!(k.advance(10000).unwrap(), vec![Cancelled]);
    assert!(k.edge(false, 10001).unwrap().is_empty());
    assert!(k.advance(20000).unwrap().is_empty());
    k.edge(true, 21000).unwrap();
    k.advance(21600).unwrap();
    assert_eq!(k.advance(31000).unwrap(), vec![Cancelled]);
}
#[test]
fn disconnect_cancels_pending_click_or_hold_and_rejects_old_generation() {
    for released in [false, true] {
        let mut k = key(true);
        k.edge(true, 0).unwrap();
        if released {
            k.edge(false, 50).unwrap();
        }
        assert_eq!(k.disconnect(), vec![Cancelled]);
        assert!(k.disconnect().is_empty());
        assert_eq!(k.advance(1000), Err(Error::Disconnected));
    }
}
#[test]
fn second_press_long_preserves_first_click_and_does_not_emit_double() {
    let mut k = key(true);
    k.edge(true, 0).unwrap();
    k.edge(false, 50).unwrap();
    k.edge(true, 100).unwrap();
    assert!(k.advance(350).unwrap().is_empty());
    assert_eq!(k.advance(700).unwrap(), vec![Click, LongStart]);
    assert_eq!(k.edge(false, 800).unwrap(), vec![Released, LongEnd]);
}
#[test]
fn invalid_clock_and_settings_do_not_mutate_state() {
    let mut k = key(false);
    k.edge(true, 100).unwrap();
    assert_eq!(k.advance(99), Err(Error::ClockWentBackwards));
    assert_eq!(k.advance(u64::MAX), Err(Error::ClockOverflow));
    assert_eq!(k.edge(false, 200).unwrap(), vec![Released, Click]);
    assert!(
        KeyGesture::new(
            Settings {
                max_hold_ms: 600,
                ..Settings::default()
            },
            0
        )
        .is_err()
    );
}
