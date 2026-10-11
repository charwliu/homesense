//! Bounded software fade sampling. Samples are targets, never physical feedback.
//! The caller owns fresh starting feedback, output admission, generation fencing,
//! cancellation, bus-wide budgeting and actual completion observation.
use crate::Percent;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    InvalidBounds,
    BudgetExceeded,
    ClockReversed,
    DeadlineMissed,
}
#[derive(Debug)]
pub struct Fade {
    start: u64,
    end: u64,
    expires: u64,
    interval: u64,
    next: u64,
    last_time: u64,
    from: Percent,
    target: Percent,
    last_level: Percent,
    finished: bool,
    budget: u64,
}
impl Fade {
    /// Minimum interval 50 ms bounds one fade to at most 20 targets/second.
    /// Duration is capped at one minute. Budget is reserved by the caller before
    /// construction succeeds; duplicate levels and missed samples use fewer slots.
    pub fn new(
        from: Percent,
        target: Percent,
        start: u64,
        duration: u64,
        interval: u64,
        lateness: u64,
        budget: u64,
    ) -> Result<Self, Error> {
        if duration == 0
            || duration > 60_000
            || !(50..=5_000).contains(&interval)
            || lateness > 5_000
        {
            return Err(Error::InvalidBounds);
        }
        let end = start.checked_add(duration).ok_or(Error::InvalidBounds)?;
        let expires = end.checked_add(lateness).ok_or(Error::InvalidBounds)?;
        let required = if from == target {
            0
        } else {
            duration.div_ceil(interval)
        };
        if required > budget {
            return Err(Error::BudgetExceeded);
        }
        Ok(Self {
            start,
            end,
            expires,
            interval,
            next: start + interval.min(duration),
            last_time: start,
            from,
            target,
            last_level: from,
            finished: from == target,
            budget: required,
        })
    }
    pub fn reserved_samples(&self) -> u64 {
        self.budget
    }
    pub fn finished(&self) -> bool {
        self.finished
    }
    pub fn cancel(&mut self) {
        self.finished = true;
    }
    /// Coalesce late ticks to one interpolated target, with no catch-up burst.
    /// The final target is emitted once only within the allowed deadline.
    pub fn sample(&mut self, now: u64) -> Result<Option<Percent>, Error> {
        if self.finished {
            return Ok(None);
        }
        if now < self.last_time {
            self.finished = true;
            return Err(Error::ClockReversed);
        }
        self.last_time = now;
        if now > self.expires {
            self.finished = true;
            return Err(Error::DeadlineMissed);
        }
        if now < self.next {
            return Ok(None);
        }
        // Reserve the endpoint slot rather than emitting two targets closer
        // than the configured interval after a late penultimate tick.
        if now < self.end && self.end - now < self.interval {
            self.next = self.end;
            return Ok(None);
        }
        let elapsed = (now - self.start).min(self.end - self.start);
        let delta = i64::from(self.target.get()) - i64::from(self.from.get());
        let level =
            i64::from(self.from.get()) + delta * elapsed as i64 / (self.end - self.start) as i64;
        let level =
            Percent::new(level as u8).expect("interpolation stays between validated endpoints");
        if now >= self.end {
            self.finished = true;
        } else {
            // Advance relative to this emission, so late samples cannot increase
            // the reserved count. The endpoint is still scheduled at its deadline.
            self.next = now.saturating_add(self.interval).min(self.end);
        }
        if level == self.last_level {
            return Ok(None);
        }
        self.last_level = level;
        Ok(Some(level))
    }
}
