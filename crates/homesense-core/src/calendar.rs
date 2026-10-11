//! Bounded daily/weekly schedules for contemporary Asia/Shanghai (UTC+08:00).
//! Unix timestamps before 2000 are rejected: historical DST is not modelled.
//! Callers must persist the returned watermark before executing occurrences if
//! duplicate prevention must survive a restart during a wall-clock rollback.
use std::collections::BTreeSet;
const DAY: i64 = 86_400_000;
const OFFSET: i64 = 8 * 3_600_000;
const MIN_TIME: i64 = 946_684_800_000;
#[derive(Debug, Clone)]
pub struct Schedule {
    pub id: String,
    /// Monday through Sunday. At least one must be enabled.
    pub weekdays: [bool; 7],
    pub hour: u8,
    pub minute: u8,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Occurrence {
    pub schedule: String,
    pub due_unix_ms: i64,
}
#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    InvalidConfiguration,
    InvalidClock,
    Stopped,
}
#[derive(Debug)]
pub struct Poll {
    pub occurrences: Vec<Occurrence>,
    pub watermark_unix_ms: i64,
    pub clock_jump: bool,
}
pub struct Calendar {
    schedules: Vec<Schedule>,
    wall: i64,
    monotonic: u64,
    watermark: i64,
    lateness: i64,
    stopped: bool,
}
impl Calendar {
    pub fn new(
        schedules: Vec<Schedule>,
        wall: i64,
        monotonic: u64,
        persisted_watermark: Option<i64>,
        lateness_ms: u32,
    ) -> Result<Self, Error> {
        if !(MIN_TIME..=i64::MAX - DAY - OFFSET).contains(&wall) {
            return Err(Error::InvalidClock);
        }
        let mut ids = BTreeSet::new();
        if schedules.len() > 256
            || !(1..=60_000).contains(&lateness_ms)
            || schedules.iter().any(|s| {
                s.id.is_empty()
                    || !ids.insert(s.id.clone())
                    || s.hour > 23
                    || s.minute > 59
                    || !s.weekdays.iter().any(|v| *v)
            })
        {
            return Err(Error::InvalidConfiguration);
        }
        if persisted_watermark.is_some_and(|v| !(MIN_TIME..=i64::MAX - DAY - OFFSET).contains(&v)) {
            return Err(Error::InvalidClock);
        }
        Ok(Self {
            schedules,
            wall,
            monotonic,
            watermark: persisted_watermark.unwrap_or(wall).max(wall),
            lateness: lateness_ms.into(),
            stopped: false,
        })
    }
    pub fn stop(&mut self) {
        self.stopped = true;
    }
    /// At most one occurrence per schedule. No missed-day replay. A wall clock
    /// jump greater than one second relative to monotonic time skips that poll.
    pub fn poll(&mut self, wall: i64, monotonic: u64) -> Result<Poll, Error> {
        if self.stopped {
            return Err(Error::Stopped);
        }
        if !(MIN_TIME..=i64::MAX - DAY - OFFSET).contains(&wall) || monotonic < self.monotonic {
            self.stopped = true;
            return Err(Error::InvalidClock);
        }
        let elapsed = i128::from(monotonic - self.monotonic);
        let wall_elapsed = i128::from(wall) - i128::from(self.wall);
        let clock_jump = (wall_elapsed - elapsed).abs() > 1000;
        self.wall = wall;
        self.monotonic = monotonic;
        let previous = self.watermark;
        self.watermark = self.watermark.max(wall);
        let mut occurrences = vec![];
        if !clock_jump && wall > previous {
            // Yesterday is needed for a normal poll crossing local midnight.
            let today = (wall + OFFSET).div_euclid(DAY);
            for schedule in &self.schedules {
                for day in [today - 1, today] {
                    let weekday = (day + 3).rem_euclid(7) as usize;
                    let due = day * DAY - OFFSET
                        + i64::from(schedule.hour) * 3_600_000
                        + i64::from(schedule.minute) * 60_000;
                    if schedule.weekdays[weekday]
                        && due > previous
                        && due <= wall
                        && wall - due <= self.lateness
                    {
                        occurrences.push(Occurrence {
                            schedule: schedule.id.clone(),
                            due_unix_ms: due,
                        });
                    }
                }
            }
            // Stable sort preserves configured order when times coincide.
            occurrences.sort_by_key(|o| o.due_unix_ms);
        }
        Ok(Poll {
            occurrences,
            watermark_unix_ms: self.watermark,
            clock_jump,
        })
    }
}
