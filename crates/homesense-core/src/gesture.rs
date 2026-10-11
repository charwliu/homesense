//! One bounded state machine per configured key. Call advance from a monotonic
//! scheduler; no wall clock, detached task, device writes, or startup replay.
#[derive(Debug, Clone, Copy)]
pub struct Settings {
    pub debounce_ms: u64,
    pub long_press_ms: u64,
    pub double_click_ms: u64,
    pub max_hold_ms: u64,
    pub double_click_enabled: bool,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            debounce_ms: 30,
            long_press_ms: 600,
            double_click_ms: 300,
            max_hold_ms: 10_000,
            double_click_enabled: false,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Pressed,
    Released,
    Click,
    DoubleClick,
    LongStart,
    LongEnd,
    Cancelled,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    InvalidSettings,
    ClockWentBackwards,
    Disconnected,
    ClockOverflow,
}
pub struct KeyGesture {
    settings: Settings,
    now: u64,
    down: Option<u64>,
    long: bool,
    last_release: Option<u64>,
    click_deadline: Option<u64>,
    second_click: bool,
    connected: bool,
}
impl KeyGesture {
    pub fn new(settings: Settings, now: u64) -> Result<Self, Error> {
        if settings.debounce_ms == 0
            || settings.long_press_ms <= settings.debounce_ms
            || settings.double_click_ms <= settings.debounce_ms
            || settings.max_hold_ms <= settings.long_press_ms
            || settings.max_hold_ms > 60_000
        {
            return Err(Error::InvalidSettings);
        }
        Ok(Self {
            settings,
            now,
            down: None,
            long: false,
            last_release: None,
            click_deadline: None,
            second_click: false,
            connected: true,
        })
    }
    fn check(&self, now: u64) -> Result<(), Error> {
        if !self.connected {
            return Err(Error::Disconnected);
        }
        if now < self.now {
            return Err(Error::ClockWentBackwards);
        }
        if now
            .checked_add(self.settings.max_hold_ms.max(self.settings.double_click_ms))
            .is_none()
        {
            return Err(Error::ClockOverflow);
        }
        Ok(())
    }
    pub fn advance(&mut self, now: u64) -> Result<Vec<Event>, Error> {
        self.check(now)?;
        self.now = now;
        let mut events = Vec::new();
        if let Some(start) = self.down {
            let held = now - start;
            // A stalled scheduler must not start a long action after its deadline.
            if held >= self.settings.max_hold_ms {
                self.down = None;
                self.long = false;
                self.second_click = false;
                self.click_deadline = None;
                self.last_release = Some(now);
                events.push(Event::Cancelled);
            } else if !self.long && held >= self.settings.long_press_ms {
                self.long = true;
                // A second press held long is not a double click. Preserve the
                // previous short click, then start the independent long gesture.
                if self.second_click {
                    events.push(Event::Click);
                }
                self.second_click = false;
                self.click_deadline = None;
                events.push(Event::LongStart);
            }
        }
        if !self.second_click && self.click_deadline.is_some_and(|due| now >= due) {
            self.click_deadline = None;
            events.push(Event::Click);
        }
        Ok(events)
    }
    pub fn edge(&mut self, pressed: bool, now: u64) -> Result<Vec<Event>, Error> {
        let mut events = self.advance(now)?;
        if pressed {
            // Repeated down feedback never restarts the hold timer. A new down
            // immediately after release is treated as contact bounce.
            if self.down.is_some()
                || self
                    .last_release
                    .is_some_and(|t| now - t < self.settings.debounce_ms)
            {
                return Ok(events);
            }
            self.second_click = self.click_deadline.is_some();
            self.down = Some(now);
            self.long = false;
            events.push(Event::Pressed);
        } else if self.down.take().is_some() {
            self.last_release = Some(now);
            events.push(Event::Released);
            if self.long {
                self.long = false;
                events.push(Event::LongEnd);
            } else if self.second_click {
                self.second_click = false;
                self.click_deadline = None;
                events.push(Event::DoubleClick);
            } else if self.settings.double_click_enabled {
                self.click_deadline = Some(now + self.settings.double_click_ms);
            } else {
                events.push(Event::Click);
            }
        }
        Ok(events)
    }
    /// Terminal for this connection/configuration generation. No delayed click
    /// survives a disconnect or configuration replacement. Construct a new key
    /// machine only after the new generation is synchronized.
    pub fn disconnect(&mut self) -> Vec<Event> {
        if !self.connected {
            return Vec::new();
        }
        self.connected = false;
        let active = self.down.take().is_some() || self.click_deadline.take().is_some();
        self.long = false;
        self.second_click = false;
        if active {
            vec![Event::Cancelled]
        } else {
            Vec::new()
        }
    }
}
