//! The booth's behaviour as a pure state machine (spec §4.1). Time is passed
//! in by the caller, so every transition is deterministic and testable.
//! The machine only decides; the app carries out the returned [`Command`]s.
//!
//! Capture flow: `lead` before the deadline the camera is prepared (high
//! quality noise reduction; screen flash on in screen mode) so auto-exposure
//! meters the lit scene. At the deadline the next frame is captured with
//! exposure and white balance locked; once it arrives the flash goes off,
//! the lock is released and the photo is processed; review shows the result
//! or "Oops, try again". Every capture has an id; results for another id are
//! ignored.

use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Timings {
    pub countdown: Duration,
    pub review: Duration,
    pub idle: Duration,
    /// Prepare the camera (and turn the screen flash on) this long before the shot.
    pub prepare_lead: Duration,
    /// Whether the screen flash is used (`flash.mode = "screen"`).
    pub screen_flash: bool,
    /// Longest the screen flash may stay on.
    pub flash_max: Duration,
    /// Give up on a frame that never arrives.
    pub capture_timeout: Duration,
    /// Give up on processing that never finishes.
    pub processing_timeout: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Photo,
    Oops,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Phase {
    Idle,
    Live,
    Countdown { deadline: Instant, prepared: bool },
    Capturing { id: u64, since: Instant },
    Processing { id: u64, since: Instant },
    Review { until: Instant, outcome: Outcome },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// Button or space bar pressed (one edge = one event).
    Press,
    /// The camera delivered the frame for capture `id`.
    FrameCaptured {
        id: u64,
    },
    PhotoReady {
        id: u64,
    },
    PhotoFailed {
        id: u64,
    },
    /// The camera stopped; the app is restarting it.
    CameraLost,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// High-quality noise reduction on; auto-exposure keeps metering.
    PrepareCapture,
    FlashOn,
    FlashOff,
    /// Lock exposure/white balance and deliver the next frame as capture `id`.
    Capture {
        id: u64,
    },
    /// Back to preview settings: auto exposure/white balance, fast noise reduction.
    ReleaseCapture,
}

/// What the screen should show, derived from the state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct View {
    pub title: bool,
    pub countdown: Option<u32>,
    pub flash: bool,
    pub review: Option<Outcome>,
}

pub struct Booth {
    timings: Timings,
    phase: Phase,
    flash_since: Option<Instant>,
    last_activity: Instant,
    next_id: u64,
}

impl Booth {
    pub fn new(timings: Timings, now: Instant) -> Self {
        Self { timings, phase: Phase::Idle, flash_since: None, last_activity: now, next_id: 1 }
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    pub fn handle(&mut self, event: Event, now: Instant) -> Vec<Command> {
        let mut out = Vec::new();
        match (self.phase, event) {
            (Phase::Idle | Phase::Live, Event::Press) => {
                self.phase = Phase::Countdown { deadline: now + self.timings.countdown, prepared: false };
                self.last_activity = now;
            }
            (Phase::Capturing { id, .. }, Event::FrameCaptured { id: got }) if got == id => {
                self.flash_off(&mut out);
                out.push(Command::ReleaseCapture);
                self.phase = Phase::Processing { id, since: now };
            }
            (Phase::Processing { id, .. }, Event::PhotoReady { id: got }) if got == id => {
                self.review(Outcome::Photo, now);
            }
            (Phase::Processing { id, .. }, Event::PhotoFailed { id: got }) if got == id => {
                self.review(Outcome::Oops, now);
            }
            (phase, Event::CameraLost) => {
                self.flash_off(&mut out);
                if matches!(phase, Phase::Countdown { prepared: true, .. } | Phase::Capturing { .. }) {
                    out.push(Command::ReleaseCapture);
                }
                self.phase = Phase::Idle;
                self.last_activity = now;
            }
            // Presses during a shot, and results for other captures, are ignored.
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
            Phase::Countdown { deadline, prepared } => {
                if !prepared && now + self.timings.prepare_lead >= deadline {
                    out.push(Command::PrepareCapture);
                    if self.timings.screen_flash {
                        self.flash_since = Some(now);
                        out.push(Command::FlashOn);
                    }
                    self.phase = Phase::Countdown { deadline, prepared: true };
                }
                if now >= deadline {
                    let id = self.next_id;
                    self.next_id += 1;
                    self.phase = Phase::Capturing { id, since: now };
                    out.push(Command::Capture { id });
                }
            }
            Phase::Capturing { since, .. } => {
                if self.flash_since.is_some_and(|t| now.duration_since(t) >= self.timings.flash_max) {
                    self.flash_off(&mut out);
                }
                if now.duration_since(since) >= self.timings.capture_timeout {
                    self.flash_off(&mut out);
                    out.push(Command::ReleaseCapture);
                    self.review(Outcome::Oops, now);
                }
            }
            Phase::Processing { since, .. } => {
                if now.duration_since(since) >= self.timings.processing_timeout {
                    self.review(Outcome::Oops, now);
                }
            }
            Phase::Review { until, .. } => {
                if now >= until {
                    self.phase = Phase::Live;
                    self.last_activity = now;
                }
            }
        }
        out
    }

    pub fn view(&self, now: Instant) -> View {
        let countdown = match self.phase {
            Phase::Countdown { deadline, .. } if deadline > now => {
                Some(deadline.duration_since(now).as_nanos().div_ceil(1_000_000_000) as u32)
            }
            _ => None,
        };
        let review = match self.phase {
            Phase::Review { outcome, .. } => Some(outcome),
            _ => None,
        };
        View { title: self.phase == Phase::Idle, countdown, flash: self.flash_since.is_some(), review }
    }

    fn flash_off(&mut self, out: &mut Vec<Command>) {
        if self.flash_since.take().is_some() {
            out.push(Command::FlashOff);
        }
    }

    fn review(&mut self, outcome: Outcome, now: Instant) {
        self.phase = Phase::Review { until: now + self.timings.review, outcome };
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
            prepare_lead: 300 * MS,
            screen_flash: true,
            flash_max: 800 * MS,
            capture_timeout: 5 * S,
            processing_timeout: 10 * S,
        }
    }

    /// Press at t0 and run to the capture; returns (booth, capture id).
    fn to_capture(t0: Instant) -> (Booth, u64) {
        let mut b = Booth::new(timings(), t0);
        b.handle(Event::Press, t0);
        b.tick(t0 + 4700 * MS);
        let cmds = b.tick(t0 + 5 * S);
        let Some(Command::Capture { id }) = cmds.last().copied() else { panic!("{cmds:?}") };
        (b, id)
    }

    #[test]
    fn starts_idle_with_title() {
        let t0 = Instant::now();
        let b = Booth::new(timings(), t0);
        assert_eq!(b.phase(), Phase::Idle);
        assert_eq!(b.view(t0), View { title: true, countdown: None, flash: false, review: None });
    }

    #[test]
    fn press_starts_countdown_showing_5_to_1() {
        let t0 = Instant::now();
        let mut b = Booth::new(timings(), t0);
        assert!(b.handle(Event::Press, t0).is_empty());
        assert_eq!(b.view(t0).countdown, Some(5));
        assert!(!b.view(t0).title);
        assert_eq!(b.view(t0 + 1 * S).countdown, Some(4));
        assert_eq!(b.view(t0 + 4500 * MS).countdown, Some(1));
        assert_eq!(b.view(t0 + 4 * S + 1 * MS).countdown, Some(1), "no millisecond truncation");
        assert_eq!(b.view(t0 + 5 * S).countdown, None);
    }

    #[test]
    fn prepare_and_flash_lead_the_capture_once() {
        let t0 = Instant::now();
        let mut b = Booth::new(timings(), t0);
        b.handle(Event::Press, t0);
        assert!(b.tick(t0 + 4600 * MS).is_empty());
        assert_eq!(b.tick(t0 + 4700 * MS), vec![Command::PrepareCapture, Command::FlashOn]);
        assert!(b.view(t0 + 4700 * MS).flash);
        assert!(b.tick(t0 + 4800 * MS).is_empty(), "only once");
        assert_eq!(b.tick(t0 + 5 * S), vec![Command::Capture { id: 1 }]);
        assert_eq!(b.phase(), Phase::Capturing { id: 1, since: t0 + 5 * S });
    }

    #[test]
    fn without_screen_flash_still_prepares() {
        let t0 = Instant::now();
        let mut b = Booth::new(Timings { screen_flash: false, ..timings() }, t0);
        b.handle(Event::Press, t0);
        assert_eq!(b.tick(t0 + 4700 * MS), vec![Command::PrepareCapture]);
        assert!(!b.view(t0 + 4800 * MS).flash);
    }

    #[test]
    fn full_shot_to_photo_review_then_live() {
        let t0 = Instant::now();
        let (mut b, id) = to_capture(t0);
        let t1 = t0 + 5050 * MS;
        assert_eq!(b.handle(Event::FrameCaptured { id }, t1), vec![Command::FlashOff, Command::ReleaseCapture]);
        assert_eq!(b.phase(), Phase::Processing { id, since: t1 });
        assert!(!b.view(t1).flash);
        let t2 = t1 + 200 * MS;
        assert!(b.handle(Event::PhotoReady { id }, t2).is_empty());
        assert_eq!(b.view(t2).review, Some(Outcome::Photo));
        assert!(b.tick(t2 + 4 * S).is_empty());
        b.tick(t2 + 5 * S);
        assert_eq!(b.phase(), Phase::Live);
    }

    #[test]
    fn processing_failure_shows_oops() {
        let t0 = Instant::now();
        let (mut b, id) = to_capture(t0);
        b.handle(Event::FrameCaptured { id }, t0 + 5 * S);
        b.handle(Event::PhotoFailed { id }, t0 + 6 * S);
        assert_eq!(b.view(t0 + 6 * S).review, Some(Outcome::Oops));
    }

    #[test]
    fn results_for_other_ids_are_ignored() {
        let t0 = Instant::now();
        let (mut b, id) = to_capture(t0);
        assert!(b.handle(Event::FrameCaptured { id: id + 1 }, t0 + 5 * S).is_empty());
        assert!(matches!(b.phase(), Phase::Capturing { .. }));
        b.handle(Event::FrameCaptured { id }, t0 + 5 * S);
        b.handle(Event::PhotoReady { id: id + 7 }, t0 + 5 * S);
        assert!(matches!(b.phase(), Phase::Processing { .. }));
    }

    #[test]
    fn ids_increase_per_shot() {
        let t0 = Instant::now();
        let (mut b, first) = to_capture(t0);
        b.handle(Event::FrameCaptured { id: first }, t0 + 5 * S);
        b.handle(Event::PhotoReady { id: first }, t0 + 5 * S);
        b.tick(t0 + 10 * S);
        b.handle(Event::Press, t0 + 11 * S);
        b.tick(t0 + 15700 * MS);
        assert_eq!(b.tick(t0 + 16 * S), vec![Command::Capture { id: first + 1 }]);
    }

    #[test]
    fn presses_ignored_during_a_shot() {
        let t0 = Instant::now();
        let mut b = Booth::new(timings(), t0);
        b.handle(Event::Press, t0);
        b.handle(Event::Press, t0 + 1 * S);
        assert!(matches!(b.phase(), Phase::Countdown { deadline, .. } if deadline == t0 + 5 * S));
        let (mut b, id) = to_capture(t0);
        b.handle(Event::Press, t0 + 5 * S);
        assert!(matches!(b.phase(), Phase::Capturing { .. }));
        b.handle(Event::FrameCaptured { id }, t0 + 5 * S);
        b.handle(Event::Press, t0 + 5 * S);
        assert!(matches!(b.phase(), Phase::Processing { .. }));
        b.handle(Event::PhotoReady { id }, t0 + 5 * S);
        b.handle(Event::Press, t0 + 6 * S);
        assert!(matches!(b.phase(), Phase::Review { .. }));
    }

    #[test]
    fn live_returns_to_idle_after_timeout() {
        let t0 = Instant::now();
        let (mut b, id) = to_capture(t0);
        b.handle(Event::FrameCaptured { id }, t0 + 5 * S);
        b.handle(Event::PhotoReady { id }, t0 + 5 * S);
        let live_at = t0 + 10 * S;
        b.tick(live_at);
        assert_eq!(b.phase(), Phase::Live);
        b.tick(live_at + 119 * S);
        assert_eq!(b.phase(), Phase::Live);
        b.tick(live_at + 120 * S);
        assert!(b.view(live_at + 120 * S).title);
    }

    #[test]
    fn flash_is_capped_and_capture_timeout_shows_oops() {
        let t0 = Instant::now();
        let (mut b, _) = to_capture(t0); // flash on at 4.7 s
        assert_eq!(b.tick(t0 + 5500 * MS), vec![Command::FlashOff], "flash_max = 800 ms");
        assert!(b.tick(t0 + 9 * S).is_empty());
        assert!(matches!(b.phase(), Phase::Capturing { .. }));
        assert_eq!(b.tick(t0 + 10 * S), vec![Command::ReleaseCapture], "capture timed out after 5 s");
        assert_eq!(b.view(t0 + 10 * S).review, Some(Outcome::Oops));
    }

    #[test]
    fn processing_timeout_shows_oops() {
        let t0 = Instant::now();
        let (mut b, id) = to_capture(t0);
        b.handle(Event::FrameCaptured { id }, t0 + 5 * S);
        b.tick(t0 + 14 * S);
        assert!(matches!(b.phase(), Phase::Processing { .. }));
        b.tick(t0 + 15 * S);
        assert_eq!(b.view(t0 + 15 * S).review, Some(Outcome::Oops));
    }

    #[test]
    fn camera_lost_releases_and_goes_idle() {
        let t0 = Instant::now();
        let mut b = Booth::new(timings(), t0);
        b.handle(Event::Press, t0);
        assert!(b.handle(Event::CameraLost, t0 + 1 * S).is_empty(), "nothing prepared yet");
        b.handle(Event::Press, t0 + 2 * S);
        b.tick(t0 + 6700 * MS);
        assert_eq!(b.handle(Event::CameraLost, t0 + 6800 * MS), vec![Command::FlashOff, Command::ReleaseCapture]);
        assert_eq!(b.phase(), Phase::Idle);
        let (mut b, id) = to_capture(t0);
        b.handle(Event::FrameCaptured { id }, t0 + 5 * S);
        assert!(b.handle(Event::CameraLost, t0 + 5 * S).is_empty());
        assert!(b.handle(Event::PhotoReady { id }, t0 + 6 * S).is_empty());
        assert_eq!(b.phase(), Phase::Idle, "late result after camera loss is ignored");
    }
}
