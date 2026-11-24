//! Runs the booth: feeds input and time into the state machine, carries
//! out its commands (camera capture, photo processing), supervises the
//! camera (restart with backoff on a background thread, spec §5) and
//! gathers what the screen should show. Owns no GL state.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::camera::{Backoff, Camera, CameraCommand, CameraLink, CapturedFrame};
use crate::config::{Config, FlashMode};
use crate::frame::{Frame, FrameMailbox};
use crate::input::{Action, GpioButton};
use crate::monitor::{Monitors, Status};
use crate::osd::Rgba;
use crate::osd::scene::SceneInput;
use crate::pipeline::exif::PhotoInfo;
use crate::pipeline::filters::load_effects;
use crate::pipeline::photo::PhotoJob;
use crate::pipeline::worker::Worker;
use crate::state::{Booth, Command, Event, Phase, Timings};
use crate::storage::Storage;
use crate::upload::immich::Immich;
use crate::upload::queue::Queue;
use crate::upload::uploader::{Pacing, UploadStatus, Uploader};

/// Builds a fresh, not yet started camera. Called on a background thread
/// when the camera is restarted.
pub type CameraFactory = Arc<dyn Fn() -> Box<dyn Camera> + Send + Sync>;

const CAPTURE_TIMEOUT: Duration = Duration::from_secs(5);
const PROCESSING_TIMEOUT: Duration = Duration::from_secs(10);

/// Camera supervision timings (spec §5). Tests shorten them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Supervision {
    /// No new preview frame for this long counts as a lost camera.
    pub stall: Duration,
    /// A restarted camera must stream this long before the backoff resets.
    pub healthy_after: Duration,
    pub backoff_initial: Duration,
    pub backoff_max: Duration,
}

impl Default for Supervision {
    fn default() -> Self {
        Self {
            stall: Duration::from_secs(3),
            healthy_after: Duration::from_secs(5),
            backoff_initial: Duration::from_secs(1),
            backoff_max: Duration::from_secs(30),
        }
    }
}

/// State-machine timings from the configuration.
pub fn timings_from(cfg: &Config) -> Timings {
    Timings {
        countdown: Duration::from_secs(cfg.timing.countdown_s as u64),
        review: Duration::from_secs(cfg.timing.review_s as u64),
        idle: Duration::from_secs(cfg.timing.idle_s as u64),
        prepare_lead: Duration::from_millis(cfg.flash.lead_ms as u64),
        screen_flash: cfg.flash.mode == FlashMode::Screen,
        flash_max: Duration::from_millis(cfg.flash.max_ms as u64),
        capture_timeout: CAPTURE_TIMEOUT,
        processing_timeout: PROCESSING_TIMEOUT,
    }
}

/// A started camera and the channels it feeds.
struct Running {
    camera: Box<dyn Camera>,
    mailbox: Arc<FrameMailbox>,
    captures: Receiver<CapturedFrame>,
}

fn start_camera(factory: &CameraFactory) -> anyhow::Result<Running> {
    let mailbox = FrameMailbox::new();
    let (tx, captures) = channel();
    let mut camera = factory();
    camera.start(CameraLink { mailbox: mailbox.clone(), captures: tx })?;
    Ok(Running { camera, mailbox, captures })
}

/// Photo settings copied from the configuration.
struct PhotoSettings {
    jpeg_quality: u8,
    save_original: bool,
    gps: Option<(f64, f64)>,
}

pub struct Controller {
    booth: Booth,
    factory: CameraFactory,
    cam: Option<Running>,
    /// Background stop of a lost camera; the next start waits for it
    /// (libcamera allows one camera manager at a time).
    stopping: Option<JoinHandle<()>>,
    /// Pending background restart.
    restarting: Option<Receiver<anyhow::Result<Running>>>,
    sup: Supervision,
    backoff: Backoff,
    restart_at: Option<Instant>,
    last_published: u64,
    last_progress: Instant,
    healthy_since: Option<Instant>,
    monitors: Monitors,
    _button: Option<GpioButton>,
    button_rx: Receiver<Action>,
    worker: Worker,
    queue: Arc<Queue>,
    uploader: Option<Uploader>,
    photo: PhotoSettings,
    review_image: Option<Rgba>,
    photo_count: usize,
    pub debug: bool,
    pub filter: bool,
}

impl Drop for Controller {
    /// Wait for a pending camera stop so libcamera isn't torn down mid-stop
    /// at quit. A pending restart thread is not waited for.
    fn drop(&mut self) {
        if let Some(h) = self.stopping.take() {
            let _ = h.join();
        }
    }
}

impl Controller {
    /// Start the camera, storage, photo worker and monitors. A camera that
    /// fails here is a startup error (spec §5): the process exits and systemd
    /// retries.
    pub fn start(
        cfg: &Config,
        factory: CameraFactory,
        button: Option<GpioButton>,
        button_rx: Receiver<Action>,
        now: Instant,
    ) -> anyhow::Result<Self> {
        Self::start_with(cfg, factory, button, button_rx, Supervision::default(), now)
    }

    pub fn start_with(
        cfg: &Config,
        factory: CameraFactory,
        button: Option<GpioButton>,
        button_rx: Receiver<Action>,
        sup: Supervision,
        now: Instant,
    ) -> anyhow::Result<Self> {
        if cfg.flash.mode == FlashMode::Strobe {
            tracing::warn!("flash.mode = \"strobe\" arrives in milestone 5; running without flash");
        }
        let storage = Storage::create(&cfg.storage.root)?;
        let photo_count = storage.photo_count();
        let effects_dir = cfg.display.assets_dir.join("effects");
        let effects = match load_effects(&effects_dir, crate::camera::SENSOR_SIZE) {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!("no effect overlays: {e:#}");
                Vec::new()
            }
        };
        tracing::info!(photos = photo_count, effects = effects.len(), "storage ready");
        // Photos are queued even when upload is not configured, so they go
        // up once it is (spec §4.8).
        let queue = Arc::new(Queue::open(&cfg.storage.root)?);
        let uploader = match (&cfg.immich_api_key, cfg.upload_enabled()) {
            (Some(key), true) => {
                let album = Some(cfg.immich.album_id.as_str());
                Some(Uploader::start(queue.clone(), Immich::new(&cfg.immich.base_url, key, album), Pacing::default())?)
            }
            _ => {
                tracing::info!(queued = queue.len(), "upload off (no Immich key or base_url); photos stay queued");
                None
            }
        };
        let cam = start_camera(&factory)?;
        Ok(Self {
            booth: Booth::new(timings_from(cfg), now),
            factory,
            cam: Some(cam),
            stopping: None,
            restarting: None,
            sup,
            backoff: Backoff::new(sup.backoff_initial, sup.backoff_max),
            restart_at: None,
            last_published: 0,
            last_progress: now,
            healthy_since: None,
            monitors: Monitors::start(cfg)?,
            _button: button,
            button_rx,
            worker: Worker::start(storage, effects, Some(queue.clone()))?,
            queue,
            uploader,
            photo: PhotoSettings {
                jpeg_quality: cfg.capture.jpeg_quality,
                save_original: cfg.storage.save_original,
                gps: cfg.immich.gps(),
            },
            review_image: None,
            photo_count,
            debug: false,
            filter: cfg.filter.enabled,
        })
    }

    /// Handle a user action. Returns actions the controller does not own
    /// (`Quit`, `ToggleMirror`) for the caller.
    pub fn on_action(&mut self, action: Action, now: Instant) -> Option<Action> {
        match action {
            Action::Press if self.cam.is_none() => {
                tracing::debug!("press ignored: camera is down");
                None
            }
            Action::Press => {
                let cmds = self.booth.handle(Event::Press, now);
                self.apply(cmds);
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

    /// Per-frame housekeeping: button presses, camera health, captured
    /// frames, finished photos and timed transitions. Never blocks.
    pub fn update(&mut self, now: Instant) {
        // An empty or disconnected (no button) channel both end the drain.
        while let Ok(a) = self.button_rx.try_recv() {
            let _ = self.on_action(a, now);
        }
        self.supervise_camera(now);
        self.collect_captures(now);
        self.collect_photos(now);
        let cmds = self.booth.tick(now);
        self.apply(cmds);
    }

    /// The newest camera frame, if one arrived since the last call.
    pub fn take_frame(&self) -> Option<Frame> {
        self.cam.as_ref().and_then(|c| c.mailbox.take_latest())
    }

    pub fn recycle(&self, frame: Frame) {
        if let Some(c) = &self.cam {
            c.mailbox.recycle(frame);
        }
    }

    /// (frames published, frames overwritten) of the current camera.
    pub fn camera_counters(&self) -> (u64, u64) {
        self.cam.as_ref().map_or((0, 0), |c| c.mailbox.counters())
    }

    pub fn status(&self) -> Status {
        self.monitors.snapshot()
    }

    pub fn photo_count(&self) -> usize {
        self.photo_count
    }

    /// Upload state for the debug overlay: `None` when upload is off, with
    /// the number of photos waiting either way.
    pub fn upload_status(&self) -> (Option<UploadStatus>, usize) {
        match &self.uploader {
            Some(u) => {
                let s = u.status();
                let n = s.pending;
                (Some(s), n)
            }
            None => (None, self.queue.len()),
        }
    }

    /// The processed photo to show in review, once per photo.
    pub fn take_review_image(&mut self) -> Option<Rgba> {
        self.review_image.take()
    }

    pub fn scene_input(&self, now: Instant, screen: (u32, u32)) -> SceneInput {
        SceneInput {
            screen,
            view: self.booth.view(now),
            status: self.status(),
            debug: self.debug,
            banner: self.cam.is_none(),
        }
    }

    fn apply(&mut self, cmds: Vec<Command>) {
        for cmd in cmds {
            let camera_cmd = match cmd {
                // The screen flash is drawn from `View::flash`.
                Command::FlashOn | Command::FlashOff => continue,
                Command::PrepareCapture => CameraCommand::PrepareCapture,
                Command::Capture { id } => CameraCommand::Capture { id },
                Command::ReleaseCapture => CameraCommand::ReleaseCapture,
            };
            if let Some(c) = self.cam.as_mut() {
                c.camera.command(camera_cmd);
            }
        }
    }

    fn collect_captures(&mut self, now: Instant) {
        let Some(c) = &self.cam else { return };
        let frames: Vec<CapturedFrame> = c.captures.try_iter().collect();
        for f in frames {
            let id = f.id;
            if !matches!(self.booth.phase(), Phase::Capturing { id: p, .. } if p == id) {
                tracing::warn!(id, "unexpected captured frame ignored");
                continue;
            }
            let cmds = self.booth.handle(Event::FrameCaptured { id }, now);
            self.apply(cmds);
            self.worker.submit(PhotoJob {
                id,
                width: f.frame.width,
                height: f.frame.height,
                rgb: f.frame.data,
                info: PhotoInfo {
                    taken: chrono::Local::now().naive_local(),
                    exposure_us: f.meta.exposure_us,
                    gain: f.meta.gain,
                    gps: self.photo.gps,
                },
                filter: self.filter,
                jpeg_quality: self.photo.jpeg_quality,
                save_original: self.photo.save_original,
            });
        }
    }

    fn collect_photos(&mut self, now: Instant) {
        while let Some(result) = self.worker.try_recv() {
            let event = match result.outcome {
                Ok(done) => {
                    self.photo_count += 1;
                    self.monitors.recheck_disk();
                    // Only the photo being processed may reach the review screen.
                    if matches!(self.booth.phase(), Phase::Processing { id, .. } if id == result.id) {
                        self.review_image = Some(done.review);
                    }
                    if let Some(u) = &self.uploader {
                        u.wake(); // the worker queued it just before reporting
                    }
                    Event::PhotoReady { id: result.id }
                }
                Err(_) => Event::PhotoFailed { id: result.id },
            };
            let cmds = self.booth.handle(event, now);
            self.apply(cmds);
        }
    }

    fn supervise_camera(&mut self, now: Instant) {
        if let Some(c) = &self.cam {
            let published = c.mailbox.counters().0;
            if published != self.last_published {
                self.last_published = published;
                self.last_progress = now;
            }
            let reason = c.mailbox.closed_reason().or_else(|| {
                (now.duration_since(self.last_progress) >= self.sup.stall)
                    .then(|| format!("no camera frames for {:?}", self.sup.stall))
            });
            if let Some(reason) = reason {
                tracing::error!("camera stopped: {reason}");
                let mut lost = self.cam.take().expect("checked above");
                // Stopping joins the camera thread; do it off the render thread.
                self.stopping = std::thread::Builder::new()
                    .name("camera-stop".into())
                    .spawn(move || lost.camera.stop())
                    .map_err(|e| tracing::error!("cannot spawn camera stop: {e}"))
                    .ok();
                self.healthy_since = None;
                let cmds = self.booth.handle(Event::CameraLost, now);
                self.apply(cmds);
                self.restart_at = Some(now + self.backoff.next_delay());
            } else if self.healthy_since.is_some_and(|t| now.duration_since(t) >= self.sup.healthy_after)
                && self.last_published > 0
            {
                self.backoff.reset();
                self.healthy_since = None;
            }
        }

        if self.cam.is_none() && self.restarting.is_none() && self.restart_at.is_some_and(|t| now >= t) {
            let (tx, rx) = channel();
            let factory = self.factory.clone();
            let stopping = self.stopping.take();
            let spawned = std::thread::Builder::new().name("camera-start".into()).spawn(move || {
                if let Some(h) = stopping {
                    let _ = h.join();
                }
                let _ = tx.send(start_camera(&factory));
            });
            match spawned {
                Ok(_) => self.restarting = Some(rx),
                Err(e) => {
                    tracing::error!("cannot spawn camera restart: {e}");
                    self.restart_at = Some(now + self.backoff.next_delay());
                }
            }
        }

        if let Some(rx) = &self.restarting {
            let result = match rx.try_recv() {
                Err(TryRecvError::Empty) => return,
                Ok(r) => r,
                Err(TryRecvError::Disconnected) => Err(anyhow::anyhow!("camera restart thread died")),
            };
            self.restarting = None;
            match result {
                Ok(cam) => {
                    tracing::info!("camera restarted");
                    self.cam = Some(cam);
                    self.restart_at = None;
                    self.healthy_since = Some(now);
                    // Baseline taken when the camera is actually running.
                    self.last_published = 0;
                    self.last_progress = now;
                }
                Err(e) => {
                    let delay = self.backoff.next_delay();
                    tracing::error!("camera restart failed, retrying in {delay:?}: {e:#}");
                    self.restart_at = Some(now + delay);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::camera::fake::FakeCamera;
    use crate::state::Outcome;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;

    const MS: Duration = Duration::from_millis(1);

    /// Short supervision timings so tests run in well under a second each.
    fn fast() -> Supervision {
        Supervision { stall: 300 * MS, healthy_after: 300 * MS, backoff_initial: 50 * MS, backoff_max: 400 * MS }
    }

    fn cfg(root: &std::path::Path) -> Config {
        let mut c = Config::default();
        c.storage.root = root.to_path_buf();
        c.display.assets_dir = root.to_path_buf(); // no effects/ here: sepia only
        c.timing.countdown_s = 1;
        c.timing.review_s = 1;
        c
    }

    /// A camera that starts fine but never publishes a frame.
    struct SilentCamera;
    impl Camera for SilentCamera {
        fn start(&mut self, _link: CameraLink) -> anyhow::Result<()> {
            Ok(())
        }
        fn command(&mut self, _cmd: CameraCommand) {}
        fn stop(&mut self) {}
    }

    /// Fake cameras; `fail_after` = number of successful starts before failing.
    fn factory(starts: Arc<AtomicUsize>, fail_after: usize) -> CameraFactory {
        Arc::new(move || -> Box<dyn Camera> {
            let n = starts.fetch_add(1, Ordering::SeqCst);
            if n >= fail_after {
                struct Broken;
                impl Camera for Broken {
                    fn start(&mut self, _l: CameraLink) -> anyhow::Result<()> {
                        anyhow::bail!("sensor unplugged")
                    }
                    fn command(&mut self, _c: CameraCommand) {}
                    fn stop(&mut self) {}
                }
                Box::new(Broken)
            } else {
                Box::new(FakeCamera::new((64, 48), 100))
            }
        })
    }

    fn controller(dir: &tempfile::TempDir, f: CameraFactory, now: Instant) -> (Controller, mpsc::Sender<Action>) {
        let (tx, rx) = mpsc::channel();
        (Controller::start_with(&cfg(dir.path()), f, None, rx, fast(), now).unwrap(), tx)
    }

    /// Run `update(Instant::now())` until `done` holds (real time, ≤ 5 s).
    fn run_until(c: &mut Controller, mut done: impl FnMut(&Controller) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !done(c) {
            assert!(Instant::now() < deadline, "condition not reached; phase {:?}", c.booth.phase());
            c.update(Instant::now());
            std::thread::sleep(5 * MS);
        }
    }

    #[test]
    fn timings_follow_config() {
        let mut c = Config::default();
        c.timing.countdown_s = 3;
        c.flash.lead_ms = 250;
        let t = timings_from(&c);
        assert_eq!(t.countdown, Duration::from_secs(3));
        assert_eq!(t.prepare_lead, Duration::from_millis(250));
        assert!(t.screen_flash);
        c.flash.mode = FlashMode::None;
        let t = timings_from(&c);
        assert!(!t.screen_flash);
        assert_eq!(t.prepare_lead, Duration::from_millis(250), "camera still prepares without flash");
    }

    #[test]
    fn creates_storage_folders_at_start() {
        let dir = tempfile::tempdir().unwrap();
        let (_c, _tx) = controller(&dir, factory(Arc::new(AtomicUsize::new(0)), usize::MAX), Instant::now());
        assert!(dir.path().join("photos").is_dir() && dir.path().join("raw").is_dir());
    }

    #[test]
    fn button_press_takes_saves_and_reviews_a_photo() {
        let dir = tempfile::tempdir().unwrap();
        let (mut c, tx) = controller(&dir, factory(Arc::new(AtomicUsize::new(0)), usize::MAX), Instant::now());
        tx.send(Action::Press).unwrap();
        c.update(Instant::now());
        assert!(matches!(c.booth.phase(), Phase::Countdown { .. }));
        run_until(&mut c, |c| matches!(c.booth.phase(), Phase::Review { .. }));
        assert_eq!(c.booth.view(Instant::now()).review, Some(Outcome::Photo));
        let review = c.take_review_image().expect("review image");
        assert_eq!((review.width, review.height), (64, 48));
        assert!(c.take_review_image().is_none(), "handed out once");
        assert_eq!(c.photo_count(), 1);
        let photos: Vec<_> = std::fs::read_dir(dir.path().join("photos")).unwrap().collect();
        let raws: Vec<_> = std::fs::read_dir(dir.path().join("raw")).unwrap().collect();
        assert_eq!((photos.len(), raws.len()), (1, 1));
        run_until(&mut c, |c| c.booth.phase() == Phase::Live);
    }

    /// A fake camera that delivers every capture twice (same id).
    struct DuplicatingCamera {
        inner: FakeCamera,
        captures: Option<mpsc::Sender<CapturedFrame>>,
    }
    impl Camera for DuplicatingCamera {
        fn start(&mut self, link: CameraLink) -> anyhow::Result<()> {
            self.captures = Some(link.captures.clone());
            self.inner.start(link)
        }
        fn command(&mut self, cmd: CameraCommand) {
            if let (CameraCommand::Capture { id }, Some(tx)) = (cmd, &self.captures) {
                let frame =
                    Frame { width: 64, height: 48, data: vec![0; 64 * 48 * 3], seq: 0, captured_at: Instant::now() };
                let _ = tx.send(CapturedFrame { id, frame, meta: Default::default() });
            }
            self.inner.command(cmd);
        }
        fn stop(&mut self) {
            self.inner.stop();
        }
    }

    #[test]
    fn duplicate_captured_frame_is_processed_once() {
        let dir = tempfile::tempdir().unwrap();
        let f: CameraFactory = Arc::new(|| -> Box<dyn Camera> {
            Box::new(DuplicatingCamera { inner: FakeCamera::new((64, 48), 100), captures: None })
        });
        let (mut c, tx) = controller(&dir, f, Instant::now());
        tx.send(Action::Press).unwrap();
        run_until(&mut c, |c| matches!(c.booth.phase(), Phase::Review { .. }));
        // Let any (wrongly) submitted second job finish and be collected.
        for _ in 0..40 {
            c.update(Instant::now());
            std::thread::sleep(5 * MS);
        }
        assert_eq!(c.photo_count(), 1);
        let photos: Vec<_> = std::fs::read_dir(dir.path().join("photos")).unwrap().collect();
        assert_eq!(photos.len(), 1);
    }

    #[test]
    fn toggles_and_passthrough_actions() {
        let dir = tempfile::tempdir().unwrap();
        let (mut c, _tx) = controller(&dir, factory(Arc::new(AtomicUsize::new(0)), usize::MAX), Instant::now());
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
    fn stalled_camera_shows_banner() {
        let dir = tempfile::tempdir().unwrap();
        let t0 = Instant::now();
        let (mut c, _tx) = controller(&dir, Arc::new(|| -> Box<dyn Camera> { Box::new(SilentCamera) }), t0);
        c.update(t0 + 299 * MS);
        assert!(!c.scene_input(t0, (100, 100)).banner);
        c.update(t0 + 300 * MS);
        assert!(c.scene_input(t0, (100, 100)).banner);
        assert_eq!(c.booth.phase(), Phase::Idle);
    }

    #[test]
    fn press_ignored_while_camera_down() {
        let dir = tempfile::tempdir().unwrap();
        let (mut c, _tx) = controller(&dir, factory(Arc::new(AtomicUsize::new(0)), usize::MAX), Instant::now());
        c.cam.as_ref().unwrap().mailbox.close("gone");
        c.update(Instant::now());
        assert!(c.cam.is_none());
        c.on_action(Action::Press, Instant::now());
        assert_eq!(c.booth.phase(), Phase::Idle);
    }

    #[test]
    fn dead_camera_restarts_in_background() {
        let starts = Arc::new(AtomicUsize::new(0));
        let dir = tempfile::tempdir().unwrap();
        let (mut c, _tx) = controller(&dir, factory(starts.clone(), usize::MAX), Instant::now());
        c.cam.as_ref().unwrap().mailbox.close("sensor gone");
        c.update(Instant::now());
        assert!(c.scene_input(Instant::now(), (100, 100)).banner);
        run_until(&mut c, |c| c.cam.is_some());
        assert_eq!(starts.load(Ordering::SeqCst), 2);
        assert!(!c.scene_input(Instant::now(), (100, 100)).banner);
        run_until(&mut c, |c| c.take_frame().is_some());
    }

    #[test]
    fn failed_restarts_back_off_and_healthy_camera_resets() {
        let starts = Arc::new(AtomicUsize::new(0));
        let dir = tempfile::tempdir().unwrap();
        // First start (at startup) works, the next two fail, then cameras work again.
        let n = starts.clone();
        let f: CameraFactory = Arc::new(move || -> Box<dyn Camera> {
            let i = n.fetch_add(1, Ordering::SeqCst);
            let inner = factory(Arc::new(AtomicUsize::new(0)), if i == 1 || i == 2 { 0 } else { usize::MAX });
            inner()
        });
        let (mut c, _tx) = controller(&dir, f, Instant::now());
        c.cam.as_ref().unwrap().mailbox.close("sensor gone");
        c.update(Instant::now());
        assert!(c.cam.is_none(), "loss detected");
        run_until(&mut c, |c| c.cam.is_some());
        assert_eq!(starts.load(Ordering::SeqCst), 4, "two failures then success");
        // Delays used so far: 50 ms (loss), 100 ms, 200 ms (two failures); next would be 400 ms.
        assert_eq!(c.backoff.clone().next_delay(), 400 * MS);
        run_until(&mut c, |c| c.healthy_since.is_none());
        assert_eq!(c.backoff.clone().next_delay(), 50 * MS, "reset after a healthy period");
    }
}
