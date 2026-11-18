//! The booth's behaviour as a pure state machine (spec §4.1). Time is passed
//! in by the caller, so every transition is deterministic and testable.
//! The machine only decides; the app carries out the returned [`Command`]s.

use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Timings {
    pub countdown: Duration,
    pub review: Duration,
    pub idle: Duration,
    /// `Some(lead)`: screen flash turns on `lead` before the shot.
    pub screen_flash_lead: Option<Duration>,
    /// Longest the screen flash may stay on.
    pub flash_max: Duration,
    /// Give up on a capture that never completes.
    pub capture_timeout: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Phase {
    Idle,
    Live,
    Countdown { deadline: Instant },
    Capturing { since: Instant },
    Review { until: Instant },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// Button or space bar pressed (one edge = one event).
    Press,
    CaptureDone,
    CaptureFailed,
    /// The camera stopped; the app is restarting it.
    CameraLost,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    FlashOn,
    FlashOff,
    /// Take the photo now.
    Capture,
    /// Stop showing new frames (review shows the captured image).
    FreezePreview,
    ResumePreview,
}

/// What the screen should show, derived from the state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct View {
    pub title: bool,
    pub countdown: Option<u32>,
    pub flash: bool,
}

pub struct Booth {
    timings: Timings,
    phase: Phase,
    flash_on: bool,
    flash_since: Option<Instant>,
    last_activity: Instant,
}

impl Booth {
    pub fn new(timings: Timings, now: Instant) -> Self {
        Self { timings, phase: Phase::Idle, flash_on: false, flash_since: None, last_activity: now }
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    pub fn handle(&mut self, event: Event, now: Instant) -> Vec<Command> {
        let mut out = Vec::new();
        match (self.phase, event) {
            (Phase::Idle | Phase::Live, Event::Press) => {
                self.phase = Phase::Countdown { deadline: now + self.timings.countdown };
                self.last_activity = now;
            }
            (Phase::Capturing { .. }, Event::CaptureDone) => {
                self.flash_off(&mut out);
                out.push(Command::FreezePreview);
                self.phase = Phase::Review { until: now + self.timings.review };
            }
            (Phase::Capturing { .. }, Event::CaptureFailed) => {
                self.flash_off(&mut out);
                self.go_live(now);
            }
            (_, Event::CameraLost) => {
                self.flash_off(&mut out);
                if matches!(self.phase, Phase::Review { .. }) {
                    out.push(Command::ResumePreview);
                }
                self.phase = Phase::Idle;
                self.last_activity = now;
            }
            // Presses during countdown/capture/review, and stray capture
            // results, are ignored.
            _ => {}
        }
        out
    }

    /// Advance time-driven transitions. Call every frame.
    pub fn tick(&mut self, now: Instant) -> Vec<Command> {
        let mut out = Vec::new();
        match self.phase {
            Phase::Idle => {}
            Phase::Live => {
                if now.duration_since(self.last_activity) >= self.timings.idle {
                    self.phase = Phase::Idle;
                }
            }
            Phase::Countdown { deadline } => {
                if let Some(lead) = self.timings.screen_flash_lead
                    && !self.flash_on
                    && now + lead >= deadline
                {
                    self.flash_on = true;
                    self.flash_since = Some(now);
                    out.push(Command::FlashOn);
                }
                if now >= deadline {
                    self.phase = Phase::Capturing { since: now };
                    out.push(Command::Capture);
                }
            }
            Phase::Capturing { since } => {
                if self.flash_on && self.flash_since.is_some_and(|t| now.duration_since(t) >= self.timings.flash_max) {
                    self.flash_off(&mut out);
                }
                if now.duration_since(since) >= self.timings.capture_timeout {
                    self.flash_off(&mut out);
                    self.go_live(now);
                }
            }
            Phase::Review { until } => {
                if now >= until {
                    out.push(Command::ResumePreview);
                    self.go_live(now);
                }
            }
        }
        out
    }

    pub fn view(&self, now: Instant) -> View {
        let countdown = match self.phase {
            Phase::Countdown { deadline } if deadline > now => {
                let ms = deadline.duration_since(now).as_millis() as u64;
                Some(ms.div_ceil(1000) as u32)
            }
            _ => None,
        };
        View { title: self.phase == Phase::Idle, countdown, flash: self.flash_on }
    }

    fn flash_off(&mut self, out: &mut Vec<Command>) {
        if self.flash_on {
            self.flash_on = false;
            self.flash_since = None;
            out.push(Command::FlashOff);
        }
    }

    fn go_live(&mut self, now: Instant) {
        self.phase = Phase::Live;
        self.last_activity = now;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: Duration = Duration::from_secs(1);
    const MS: Duration = Duration::from_millis(1);

    fn timings() -> Timings {
        Timings {
            countdown: 5 * S,
            review: 5 * S,
            idle: 120 * S,
            screen_flash_lead: Some(300 * MS),
            flash_max: 800 * MS,
            capture_timeout: 5 * S,
        }
    }

    #[test]
    fn starts_idle_with_title() {
        let t0 = Instant::now();
        let b = Booth::new(timings(), t0);
        assert_eq!(b.phase(), Phase::Idle);
        assert_eq!(b.view(t0), View { title: true, countdown: None, flash: false });
    }

    #[test]
    fn press_starts_countdown_showing_5_to_1() {
        let t0 = Instant::now();
        let mut b = Booth::new(timings(), t0);
        assert!(b.handle(Event::Press, t0).is_empty());
        assert_eq!(b.phase(), Phase::Countdown { deadline: t0 + 5 * S });
        assert_eq!(b.view(t0).countdown, Some(5));
        assert!(!b.view(t0).title);
        assert_eq!(b.view(t0 + 1 * S).countdown, Some(4));
        assert_eq!(b.view(t0 + 4500 * MS).countdown, Some(1));
        assert_eq!(b.view(t0 + 5 * S).countdown, None);
    }

    #[test]
    fn screen_flash_leads_the_capture() {
        let t0 = Instant::now();
        let mut b = Booth::new(timings(), t0);
        b.handle(Event::Press, t0);
        assert!(b.tick(t0 + 4600 * MS).is_empty());
        assert_eq!(b.tick(t0 + 4700 * MS), vec![Command::FlashOn]);
        assert!(b.view(t0 + 4700 * MS).flash);
        assert!(b.tick(t0 + 4800 * MS).is_empty(), "flash turns on once");
        assert_eq!(b.tick(t0 + 5 * S), vec![Command::Capture]);
        assert_eq!(b.phase(), Phase::Capturing { since: t0 + 5 * S });
    }

    #[test]
    fn no_flash_when_disabled() {
        let t0 = Instant::now();
        let mut b = Booth::new(Timings { screen_flash_lead: None, ..timings() }, t0);
        b.handle(Event::Press, t0);
        assert!(b.tick(t0 + 4900 * MS).is_empty());
        assert_eq!(b.tick(t0 + 5 * S), vec![Command::Capture]);
    }

    #[test]
    fn capture_done_goes_to_review_then_live() {
        let t0 = Instant::now();
        let mut b = Booth::new(timings(), t0);
        b.handle(Event::Press, t0);
        b.tick(t0 + 4700 * MS);
        b.tick(t0 + 5 * S);
        let t1 = t0 + 5100 * MS;
        assert_eq!(b.handle(Event::CaptureDone, t1), vec![Command::FlashOff, Command::FreezePreview]);
        assert_eq!(b.phase(), Phase::Review { until: t1 + 5 * S });
        assert!(b.tick(t1 + 4 * S).is_empty());
        assert_eq!(b.tick(t1 + 5 * S), vec![Command::ResumePreview]);
        assert_eq!(b.phase(), Phase::Live);
    }

    #[test]
    fn presses_ignored_outside_idle_and_live() {
        let t0 = Instant::now();
        let mut b = Booth::new(timings(), t0);
        b.handle(Event::Press, t0);
        b.handle(Event::Press, t0 + 1 * S);
        assert_eq!(b.phase(), Phase::Countdown { deadline: t0 + 5 * S }, "countdown not restarted");
        b.tick(t0 + 5 * S);
        assert!(b.handle(Event::Press, t0 + 5 * S).is_empty());
        let t1 = t0 + 5 * S;
        b.handle(Event::CaptureDone, t1);
        b.handle(Event::Press, t1 + 1 * S);
        assert!(matches!(b.phase(), Phase::Review { .. }));
    }

    #[test]
    fn press_in_live_starts_new_countdown() {
        let t0 = Instant::now();
        let mut b = Booth::new(timings(), t0);
        b.handle(Event::Press, t0);
        b.tick(t0 + 5 * S);
        b.handle(Event::CaptureDone, t0 + 5 * S);
        b.tick(t0 + 10 * S);
        assert_eq!(b.phase(), Phase::Live);
        b.handle(Event::Press, t0 + 11 * S);
        assert!(matches!(b.phase(), Phase::Countdown { .. }));
    }

    #[test]
    fn live_returns_to_idle_after_timeout() {
        let t0 = Instant::now();
        let mut b = Booth::new(timings(), t0);
        b.handle(Event::Press, t0);
        b.tick(t0 + 5 * S);
        b.handle(Event::CaptureDone, t0 + 5 * S);
        let live_at = t0 + 10 * S;
        b.tick(live_at);
        b.tick(live_at + 119 * S);
        assert_eq!(b.phase(), Phase::Live);
        b.tick(live_at + 120 * S);
        assert_eq!(b.phase(), Phase::Idle);
        assert!(b.view(live_at + 120 * S).title);
    }

    #[test]
    fn flash_capped_and_capture_timeout_returns_go_live() {
        let t0 = Instant::now();
        let mut b = Booth::new(timings(), t0);
        b.handle(Event::Press, t0);
        b.tick(t0 + 4700 * MS); // flash on at 4.7 s
        b.tick(t0 + 5 * S); // capture requested
        assert_eq!(b.tick(t0 + 5500 * MS), vec![Command::FlashOff], "flash_max = 800 ms");
        assert!(b.tick(t0 + 9 * S).is_empty());
        assert!(b.tick(t0 + 10 * S).is_empty(), "already off, no duplicate");
        assert_eq!(b.phase(), Phase::Live, "capture timed out after 5 s");
    }

    #[test]
    fn capture_failed_turns_flash_off_and_goes_live() {
        let t0 = Instant::now();
        let mut b = Booth::new(timings(), t0);
        b.handle(Event::Press, t0);
        b.tick(t0 + 4700 * MS);
        b.tick(t0 + 5 * S);
        assert_eq!(b.handle(Event::CaptureFailed, t0 + 5 * S), vec![Command::FlashOff]);
        assert_eq!(b.phase(), Phase::Live);
    }

    #[test]
    fn camera_lost_resets_to_idle_from_any_phase() {
        let t0 = Instant::now();
        let mut b = Booth::new(timings(), t0);
        b.handle(Event::Press, t0);
        b.tick(t0 + 4700 * MS);
        assert_eq!(b.handle(Event::CameraLost, t0 + 4800 * MS), vec![Command::FlashOff]);
        assert_eq!(b.phase(), Phase::Idle);

        b.handle(Event::Press, t0 + 10 * S);
        b.tick(t0 + 15 * S);
        b.handle(Event::CaptureDone, t0 + 15 * S);
        assert_eq!(b.handle(Event::CameraLost, t0 + 16 * S), vec![Command::ResumePreview]);
        assert_eq!(b.phase(), Phase::Idle);
    }
}
