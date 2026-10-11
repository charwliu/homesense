use homesense_core::{
    Percent,
    fade::{Error, Fade},
};
fn level(value: u8) -> Percent {
    Percent::new(value).unwrap()
}
#[test]
fn timed_fade_coalesces_jitter_and_emits_endpoint_once() {
    let mut fade = Fade::new(level(100), level(0), 100, 1000, 100, 50, 10).unwrap();
    assert_eq!(fade.reserved_samples(), 10);
    assert_eq!(fade.sample(100).unwrap(), None);
    assert_eq!(fade.sample(200).unwrap(), Some(level(90)));
    assert_eq!(fade.sample(550).unwrap(), Some(level(55)));
    assert_eq!(fade.sample(550).unwrap(), None);
    assert_eq!(fade.sample(1100).unwrap(), Some(level(0)));
    assert!(fade.finished());
    assert_eq!(fade.sample(1101).unwrap(), None);
}
#[test]
fn late_cancelled_and_reversed_clocks_never_replay_targets() {
    for mode in 0..3 {
        let mut fade = Fade::new(level(0), level(100), 10, 1000, 50, 20, 20).unwrap();
        match mode {
            0 => {
                fade.cancel();
                assert_eq!(fade.sample(500), Ok(None));
            }
            1 => assert_eq!(fade.sample(1031), Err(Error::DeadlineMissed)),
            _ => {
                fade.sample(100).unwrap();
                assert_eq!(fade.sample(99), Err(Error::ClockReversed));
            }
        }
        assert!(fade.finished());
        assert_eq!(fade.sample(1010), Ok(None));
    }
}
#[test]
fn bounded_budget_and_rounding_work_in_both_directions() {
    assert!(matches!(
        Fade::new(level(0), level(100), 0, 1000, 50, 0, 19),
        Err(Error::BudgetExceeded)
    ));
    assert!(matches!(
        Fade::new(level(0), level(100), u64::MAX, 1000, 50, 0, 20),
        Err(Error::InvalidBounds)
    ));
    for (from, to) in [(0, 1), (1, 0), (33, 77), (77, 33)] {
        let mut fade = Fade::new(level(from), level(to), 0, 975, 100, 20, 10).unwrap();
        let mut values = vec![];
        for now in (0..=1000).step_by(13) {
            if let Some(v) = fade.sample(now).unwrap() {
                values.push(v.get());
            }
        }
        assert!(values.len() <= 10);
        assert_eq!(values.last(), Some(&to));
        assert!(
            values
                .windows(2)
                .all(|w| if from < to { w[0] < w[1] } else { w[0] > w[1] })
        );
    }
}

#[test]
fn delayed_penultimate_tick_reserves_endpoint_bus_slot() {
    let mut fade = Fade::new(level(0), level(100), 0, 1000, 100, 0, 10).unwrap();
    assert_eq!(fade.sample(850), Ok(Some(level(85))));
    assert_eq!(fade.sample(970), Ok(None));
    assert_eq!(fade.sample(1000), Ok(Some(level(100))));
}
