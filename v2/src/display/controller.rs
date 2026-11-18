//! Runs the booth: feeds input and time into the state machine, carries
//! out its commands, supervises the camera (restart with backoff, spec §5)
//! and gathers what the screen should show. Owns no GL state.

use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use crate::camera::{Backoff, Camera};
use crate::config::{Config, FlashMode};
use crate::frame::{Frame, FrameMailbox};
use crate::input::{Action, GpioButton};
use crate::monitor::{Monitors, Status};
use crate::osd::scene::SceneInput;
use crate::state::{Booth, Command, Event, Timings};

pub type CameraFactory = Box<dyn FnMut() -> Box<dyn Camera>>;

const CAPTURE_TIMEOUT: Duration = Duration::from_secs(5);

/// State-machine timings from the configuration.
pub fn timings_from(cfg: &Config) -> Timings {
    Timings {
        countdown: Duration::from_secs(cfg.timing.countdown_s as u64),
        review: Duration::from_secs(cfg.timing.review_s as u64),
        idle: Duration::from_secs(cfg.timing.idle_s as u64),
        screen_flash_lead: (cfg.flash.mode == FlashMode::Screen)
            .then(|| Duration::from_millis(cfg.flash.lead_ms as u64)),
        flash_max: Duration::from_millis(cfg.flash.max_ms as u64),
        capture_timeout: CAPTURE_TIMEOUT,
    }
}

pub struct Controller {
    booth: Booth,
    make_camera: CameraFactory,
    camera: Option<Box<dyn Camera>>,
    mailbox: Arc<FrameMailbox>,
    backoff: Backoff,
    restart_at: Option<Instant>,
    monitors: Monitors,
    _button: Option<GpioButton>,
    button_rx: Receiver<Action>,
    frozen: bool,
    pub debug: bool,
    pub filter: bool,
}

impl Controller {
    /// Start the camera and monitors. A camera that fails here is a startup
    /// error (spec §5): the process exits and systemd retries.
    pub fn start(
        cfg: &Config,
        mut make_camera: CameraFactory,
        button: Option<GpioButton>,
        button_rx: Receiver<Action>,
        now: Instant,
    ) -> anyhow::Result<Self> {
        if cfg.flash.mode == FlashMode::Strobe {
            tracing::warn!("flash.mode = \"strobe\" arrives in milestone 5; running without flash");
        }
        let mailbox = FrameMailbox::new();
        let mut camera = make_camera();
        camera.start(mailbox.clone())?;
        Ok(Self {
            booth: Booth::new(timings_from(cfg), now),
            make_camera,
            camera: Some(camera),
            mailbox,
            backoff: Backoff::new(Duration::from_secs(1), Duration::from_secs(30)),
            restart_at: None,
            monitors: Monitors::start(cfg)?,
            _button: button,
            button_rx,
            frozen: false,
            debug: false,
            filter: cfg.filter.enabled,
        })
    }

    /// Handle a user action. Returns actions the controller does not own
    /// (`Quit`, `ToggleMirror`) for the caller.
    pub fn on_action(&mut self, action: Action, now: Instant) -> Option<Action> {
        match action {
            Action::Press => {
                let cmds = self.booth.handle(Event::Press, now);
                self.apply(cmds, now);
                None
            }
            Action::ToggleDebug => {
                self.debug = !self.debug;
                None
            }
            Action::ToggleFilter => {
                self.filter = !self.filter;
                tracing::info!(filter = self.filter, "filter toggled");
                None
            }
            Action::Quit | Action::ToggleMirror => Some(action),
        }
    }

    /// Per-frame housekeeping: button presses, camera health, timed transitions.
    pub fn update(&mut self, now: Instant) {
        // An empty or disconnected (no button) channel both end the drain.
        while let Ok(a) = self.button_rx.try_recv() {
            let _ = self.on_action(a, now);
        }
        self.supervise_camera(now);
        let cmds = self.booth.tick(now);
        self.apply(cmds, now);
    }

    /// The newest camera frame, unless the preview is frozen for review.
    pub fn take_frame(&self) -> Option<Frame> {
        if self.frozen { None } else { self.mailbox.take_latest() }
    }

    pub fn recycle(&self, frame: Frame) {
        self.mailbox.recycle(frame);
    }

    /// (frames published, frames overwritten) of the current camera.
    pub fn camera_counters(&self) -> (u64, u64) {
        self.mailbox.counters()
    }

    pub fn status(&self) -> Status {
        self.monitors.snapshot()
    }

    pub fn scene_input(&self, now: Instant, screen: (u32, u32)) -> SceneInput {
        SceneInput {
            screen,
            view: self.booth.view(now),
            status: self.status(),
            debug: self.debug,
            banner: self.camera.is_none(),
        }
    }

    fn apply(&mut self, cmds: Vec<Command>, now: Instant) {
        for cmd in cmds {
            match cmd {
                // The screen flash is drawn from `View::flash`.
                Command::FlashOn | Command::FlashOff => {}
                Command::Capture => {
                    // Milestone 2: the "photo" is the preview frame on screen,
                    // frozen for review and not saved. Milestone 3 captures for real.
                    tracing::info!("capture (preview frame, not saved)");
                    let more = self.booth.handle(Event::CaptureDone, now);
                    self.apply(more, now);
                }
                Command::FreezePreview => self.frozen = true,
                Command::ResumePreview => self.frozen = false,
            }
        }
    }

    fn supervise_camera(&mut self, now: Instant) {
        if self.camera.is_some()
            && let Some(reason) = self.mailbox.closed_reason()
        {
            tracing::error!("camera stopped: {reason}");
            if let Some(mut cam) = self.camera.take() {
                cam.stop();
            }
            let cmds = self.booth.handle(Event::CameraLost, now);
            self.apply(cmds, now);
            self.restart_at = Some(now + self.backoff.next_delay());
        }
        if self.camera.is_none() && self.restart_at.is_some_and(|t| now >= t) {
            let mailbox = FrameMailbox::new();
            let mut cam = (self.make_camera)();
            match cam.start(mailbox.clone()) {
                Ok(()) => {
                    tracing::info!("camera restarted");
                    self.mailbox = mailbox;
                    self.camera = Some(cam);
                    self.backoff.reset();
                    self.restart_at = None;
                }
                Err(e) => {
                    let delay = self.backoff.next_delay();
                    tracing::error!("camera restart failed, retrying in {delay:?}: {e:#}");
                    self.restart_at = Some(Instant::now() + delay);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::camera::fake::FakeCamera;
    use crate::state::Phase;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;

    /// A camera whose stream can be killed from the test, like a dead sensor.
    struct DyingCamera {
        inner: FakeCamera,
        mailbox: Option<Arc<FrameMailbox>>,
        fail_start: bool,
    }

    impl Camera for DyingCamera {
        fn start(&mut self, mailbox: Arc<FrameMailbox>) -> anyhow::Result<()> {
            if self.fail_start {
                anyhow::bail!("sensor unplugged");
            }
            self.mailbox = Some(mailbox.clone());
            self.inner.start(mailbox)
        }
        fn stop(&mut self) {
            self.inner.stop();
        }
    }

    fn cfg() -> Config {
        let mut c = Config::default();
        c.storage.root = std::env::temp_dir();
        c
    }

    fn controller(starts: Arc<AtomicUsize>, fail_after_first: bool) -> (Controller, mpsc::Sender<Action>) {
        let (tx, rx) = mpsc::channel();
        let factory: CameraFactory = Box::new(move || {
            let n = starts.fetch_add(1, Ordering::SeqCst);
            Box::new(DyingCamera {
                inner: FakeCamera::new((8, 8), 100),
                mailbox: None,
                fail_start: fail_after_first && n > 0,
            })
        });
        (Controller::start(&cfg(), factory, None, rx, Instant::now()).unwrap(), tx)
    }

    #[test]
    fn timings_follow_config() {
        let mut c = Config::default();
        c.timing.countdown_s = 3;
        c.flash.lead_ms = 250;
        let t = timings_from(&c);
        assert_eq!(t.countdown, Duration::from_secs(3));
        assert_eq!(t.screen_flash_lead, Some(Duration::from_millis(250)));
        c.flash.mode = FlashMode::None;
        assert_eq!(timings_from(&c).screen_flash_lead, None);
    }

    #[test]
    fn button_press_runs_a_full_shot_and_freezes_preview() {
        let (mut c, tx) = controller(Arc::new(AtomicUsize::new(0)), false);
        let t0 = Instant::now();
        tx.send(Action::Press).unwrap();
        c.update(t0);
        assert!(matches!(c.booth.phase(), Phase::Countdown { .. }));
        assert!(c.scene_input(t0, (100, 100)).view.countdown.is_some());
        c.update(t0 + Duration::from_millis(4800));
        assert!(c.scene_input(t0 + Duration::from_millis(4800), (100, 100)).view.flash);
        c.update(t0 + Duration::from_secs(5));
        assert!(matches!(c.booth.phase(), Phase::Review { .. }));
        assert!(c.frozen);
        std::thread::sleep(Duration::from_millis(50));
        assert!(c.take_frame().is_none(), "frozen preview yields no frames");
        c.update(t0 + Duration::from_secs(10));
        assert_eq!(c.booth.phase(), Phase::Live);
        std::thread::sleep(Duration::from_millis(50));
        assert!(c.take_frame().is_some());
    }

    #[test]
    fn toggles_and_passthrough_actions() {
        let (mut c, _tx) = controller(Arc::new(AtomicUsize::new(0)), false);
        let now = Instant::now();
        assert_eq!(c.on_action(Action::ToggleDebug, now), None);
        assert!(c.debug);
        let f = c.filter;
        c.on_action(Action::ToggleFilter, now);
        assert_eq!(c.filter, !f);
        assert_eq!(c.on_action(Action::Quit, now), Some(Action::Quit));
        assert_eq!(c.on_action(Action::ToggleMirror, now), Some(Action::ToggleMirror));
    }

    #[test]
    fn dead_camera_shows_banner_and_restarts() {
        let starts = Arc::new(AtomicUsize::new(0));
        let (mut c, _tx) = controller(starts.clone(), false);
        let t0 = Instant::now();
        c.mailbox.close("sensor gone");
        c.update(t0);
        assert!(c.scene_input(t0, (100, 100)).banner);
        assert_eq!(c.booth.phase(), Phase::Idle);
        c.update(t0 + Duration::from_millis(500));
        assert_eq!(starts.load(Ordering::SeqCst), 1, "waits for the 1 s backoff");
        c.update(t0 + Duration::from_secs(1));
        assert_eq!(starts.load(Ordering::SeqCst), 2);
        assert!(!c.scene_input(t0, (100, 100)).banner);
        std::thread::sleep(Duration::from_millis(50));
        assert!(c.take_frame().is_some(), "new camera streams into the new mailbox");
    }

    #[test]
    fn failed_restart_backs_off() {
        let starts = Arc::new(AtomicUsize::new(0));
        let (mut c, _tx) = controller(starts.clone(), true);
        let t0 = Instant::now();
        c.mailbox.close("sensor gone");
        c.update(t0);
        c.update(t0 + Duration::from_secs(1));
        assert_eq!(starts.load(Ordering::SeqCst), 2);
        assert!(c.scene_input(t0, (100, 100)).banner, "still down");
        assert!(c.restart_at.unwrap() > Instant::now() + Duration::from_millis(1500), "next wait is 2 s");
    }
}
