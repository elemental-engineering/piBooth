//! Raspberry Pi camera via libcamera (Pi 5 PiSP pipeline, IMX296).
//!
//! libcamera objects borrow the `CameraManager` and must stay on one thread,
//! so the whole session lives on a dedicated "pi-camera" thread. `start`
//! waits for that thread to report that streaming began (or why it failed).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow, bail};
use libcamera::camera::CameraConfigurationStatus;
use libcamera::camera_manager::CameraManager;
use libcamera::control::ControlList;
use libcamera::controls::{
    AnalogueGain, AnalogueGainMode, AwbEnable, ExposureTime, ExposureTimeMode, FrameDuration, FrameDurationLimits,
    NoiseReductionMode, SensorTimestamp,
};
use libcamera::framebuffer::AsFrameBuffer;
use libcamera::framebuffer_allocator::{FrameBuffer, FrameBufferAllocator};
use libcamera::framebuffer_map::MemoryMappedFrameBuffer;
use libcamera::geometry::Size;
use libcamera::pixel_format::PixelFormat;
use libcamera::request::{RequestStatus, ReuseFlag};
use libcamera::stream::StreamRole;

use super::{Camera, CameraCommand, CameraLink, CaptureMeta, CapturedFrame, SENSOR_FPS, SENSOR_SIZE};
use crate::config::CameraCfg;
use crate::frame::{Frame, FrameMailbox, copy_packed};
use crate::stats::{FrameSample, FrameTiming};

/// libcamera `BGR888` = DRM `BG24`: bytes in memory are R, G, B, which is
/// what GL `RGB` expects. (libcamera `RGB888` is B, G, R in memory.)
const PREVIEW_FORMAT: PixelFormat = PixelFormat::new(u32::from_le_bytes(*b"BG24"), 0);
const BUFFER_COUNT: u32 = 4;

pub struct PiCamera {
    cfg: CameraCfg,
    running: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    commands: Option<mpsc::Sender<CameraCommand>>,
}

impl PiCamera {
    pub fn new(cfg: CameraCfg) -> Self {
        Self { cfg, running: Arc::new(AtomicBool::new(false)), thread: None, commands: None }
    }
}

impl Camera for PiCamera {
    fn start(&mut self, link: CameraLink) -> anyhow::Result<()> {
        if self.thread.is_some() {
            return Ok(());
        }
        if !self.cfg.tuning_file.is_empty() {
            // Read by the Raspberry Pi IPA when the camera is configured.
            // SAFETY: called before any other thread reads the environment.
            unsafe { std::env::set_var("LIBCAMERA_RPI_TUNING_FILE", &self.cfg.tuning_file) };
        }
        self.running.store(true, Ordering::SeqCst);
        let running = self.running.clone();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
        let (cmd_tx, cmd_rx) = mpsc::channel();
        self.commands = Some(cmd_tx);
        let thread = std::thread::Builder::new().name("pi-camera".into()).spawn(move || {
            let mut guard = CloseGuard { mailbox: link.mailbox.clone(), running: running.clone(), reason: None };
            if let Err(e) = session(&link, &cmd_rx, &running, &ready_tx) {
                let msg = format!("{e:#}");
                // If setup failed, `start` is still waiting for this message.
                let _ = ready_tx.send(Err(msg.clone()));
                tracing::error!("camera session ended: {msg}");
                guard.reason = Some(msg);
            }
        })?;
        self.thread = Some(thread);
        match ready_rx.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => {
                self.stop();
                Err(anyhow!(e))
            }
            Err(_) => {
                self.stop();
                bail!("camera did not start within 10 s")
            }
        }
    }

    fn command(&mut self, cmd: CameraCommand) {
        if let Some(tx) = &self.commands {
            let _ = tx.send(cmd);
        }
    }

    fn stop(&mut self) {
        self.running.store(false, Ordering::SeqCst);
        self.commands = None;
        if let Some(t) = self.thread.take()
            && t.join().is_err()
        {
            tracing::error!("camera thread panicked");
        }
    }
}

/// Closes the mailbox when the camera thread ends while it was still meant
/// to run (session error or panic), so the render loop can fail loudly.
struct CloseGuard {
    mailbox: Arc<FrameMailbox>,
    running: Arc<AtomicBool>,
    reason: Option<String>,
}

impl Drop for CloseGuard {
    fn drop(&mut self) {
        if self.running.load(Ordering::SeqCst) {
            let reason = if std::thread::panicking() {
                "camera thread panicked".to_string()
            } else {
                self.reason.take().unwrap_or_else(|| "camera session ended".to_string())
            };
            self.mailbox.close(reason);
        }
    }
}

impl Drop for PiCamera {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Control changes waiting for the next re-queued request. libcamera keeps
/// controls once set, so each change is sent once.
#[derive(Debug, Default, PartialEq)]
struct PendingControls {
    noise_reduction: Option<NoiseReductionMode>,
    /// `Some(true)`: freeze exposure, gain and white balance at their current
    /// values (switching to manual keeps the last computed value).
    /// `Some(false)`: back to automatic.
    lock: Option<bool>,
}

impl PendingControls {
    fn on_command(&mut self, cmd: CameraCommand) {
        match cmd {
            CameraCommand::PrepareCapture => self.noise_reduction = Some(NoiseReductionMode::HighQuality),
            CameraCommand::Capture { .. } => self.lock = Some(true),
            CameraCommand::ReleaseCapture => {
                self.noise_reduction = Some(NoiseReductionMode::Fast);
                self.lock = Some(false);
            }
        }
    }

    fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    fn apply(&mut self, list: &mut ControlList) -> anyhow::Result<()> {
        fn err(name: &'static str) -> impl Fn(libcamera::control::ControlError) -> anyhow::Error {
            move |e| anyhow!("{name}: {e:?}")
        }
        if let Some(nr) = self.noise_reduction.take() {
            list.set(nr).map_err(err("NoiseReductionMode"))?;
        }
        if let Some(lock) = self.lock.take() {
            let (exposure, gain) = if lock {
                (ExposureTimeMode::Manual, AnalogueGainMode::Manual)
            } else {
                (ExposureTimeMode::Auto, AnalogueGainMode::Auto)
            };
            list.set(exposure).map_err(err("ExposureTimeMode"))?;
            list.set(gain).map_err(err("AnalogueGainMode"))?;
            list.set(AwbEnable(!lock)).map_err(err("AwbEnable"))?;
        }
        Ok(())
    }
}

fn session(
    link: &CameraLink,
    commands: &mpsc::Receiver<CameraCommand>,
    running: &AtomicBool,
    ready: &mpsc::Sender<Result<(), String>>,
) -> anyhow::Result<()> {
    let mailbox = &link.mailbox;
    let mgr = CameraManager::new().context("starting libcamera")?;
    let cameras = mgr.cameras();
    let cam = cameras.get(0).ok_or_else(|| anyhow!("no camera found (is the ribbon cable seated?)"))?;
    tracing::info!(id = %cam.id(), "using camera");
    let mut cam = cam.acquire().context("camera is busy (another app using it?)")?;

    let mut cfgs = cam
        .generate_configuration(&[StreamRole::ViewFinder])
        .ok_or_else(|| anyhow!("could not generate viewfinder configuration"))?;
    {
        let mut s = cfgs.get_mut(0).unwrap();
        s.set_pixel_format(PREVIEW_FORMAT);
        s.set_size(Size { width: SENSOR_SIZE.0, height: SENSOR_SIZE.1 });
        s.set_buffer_count(BUFFER_COUNT);
    }
    match cfgs.validate() {
        CameraConfigurationStatus::Valid => {}
        CameraConfigurationStatus::Adjusted => tracing::warn!("camera adjusted configuration: {cfgs:?}"),
        CameraConfigurationStatus::Invalid => bail!("camera rejected configuration: {cfgs:?}"),
    }
    let (size, stride, format) = {
        let s = cfgs.get(0).unwrap();
        (s.get_size(), s.get_stride() as usize, s.get_pixel_format())
    };
    if format != PREVIEW_FORMAT {
        bail!("camera does not support BGR888 preview (got {format:?})");
    }
    tracing::info!(width = size.width, height = size.height, stride, "preview configured");
    cam.configure(&mut cfgs).context("configuring camera")?;

    let stream = cfgs.get(0).unwrap().stream().ok_or_else(|| anyhow!("stream missing after configure"))?;
    let mut alloc = FrameBufferAllocator::new(&cam);
    let buffers = alloc.alloc(&stream).context("allocating frame buffers")?;
    let mut requests = Vec::with_capacity(buffers.len());
    for (i, buf) in buffers.into_iter().enumerate() {
        let buf = MemoryMappedFrameBuffer::new(buf).map_err(|e| anyhow!("mmap buffer: {e:?}"))?;
        let mut req = cam.create_request(Some(i as u64)).ok_or_else(|| anyhow!("create_request failed"))?;
        req.add_buffer(&stream, buf).context("attaching buffer")?;
        requests.push(req);
    }

    let (done_tx, done_rx) = mpsc::channel();
    cam.on_request_completed(move |req| {
        let _ = done_tx.send(req);
    });

    // Fixed 60 fps: min = max frame duration (µs). Caps exposure at 16.6 ms.
    let frame_us = 1_000_000 / SENSOR_FPS as i64;
    let mut controls = ControlList::new();
    controls.set(FrameDurationLimits([frame_us, frame_us])).map_err(|e| anyhow!("FrameDurationLimits: {e:?}"))?;
    cam.start(Some(&controls)).context("starting camera stream")?;
    for req in requests {
        cam.queue_request(req).map_err(|(_, e)| e).context("queueing request")?;
    }
    let _ = ready.send(Ok(()));

    tracing::info!(frame_duration_us = frame_us, "requested fixed frame duration");

    let (w, h) = (size.width, size.height);
    let row_bytes = w as usize * 3;
    let mut timing = FrameTiming::default();
    let mut last_report = Instant::now();
    let mut pending = PendingControls::default();
    let mut capture: Option<u64> = None;
    while running.load(Ordering::SeqCst) {
        for cmd in commands.try_iter() {
            tracing::debug!(?cmd, "camera command");
            pending.on_command(cmd);
            if let CameraCommand::Capture { id } = cmd {
                capture = Some(id);
            }
        }
        let mut req = match done_rx.recv_timeout(Duration::from_secs(2)) {
            Ok(r) => r,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                tracing::warn!("no camera frame for 2 s");
                continue;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => bail!("camera callback channel closed"),
        };
        if req.status() == RequestStatus::Complete {
            let fb: &MemoryMappedFrameBuffer<FrameBuffer> =
                req.buffer(&stream).ok_or_else(|| anyhow!("request without buffer"))?;
            let seq = fb.metadata().map(|m| m.sequence() as u64).unwrap_or(0);
            let copy_start = Instant::now();
            let planes = fb.data();
            let mut data = mailbox.take_buffer(Frame::byte_len(w, h));
            copy_packed(planes[0], stride, row_bytes, h as usize, &mut data);
            let copy_us = copy_start.elapsed().as_micros() as u64;
            let md = req.metadata();
            if let Some(id) = capture.take() {
                // The next complete frame after the command: high-quality noise
                // reduction has been on since PrepareCapture (~300 ms earlier).
                let meta = CaptureMeta {
                    exposure_us: md.get::<ExposureTime>().ok().map(|v| v.0),
                    gain: md.get::<AnalogueGain>().ok().map(|v| v.0),
                };
                tracing::info!(id, seq, exposure_us = meta.exposure_us, gain = meta.gain, "frame captured");
                let frame = Frame { width: w, height: h, data: data.clone(), seq, captured_at: Instant::now() };
                let _ = link.captures.send(CapturedFrame { id, frame, meta });
            }
            mailbox.publish(Frame { width: w, height: h, data, seq, captured_at: Instant::now() });
            timing.record(FrameSample {
                seq,
                sensor_ts_ns: md.get::<SensorTimestamp>().ok().map(|v| v.0),
                frame_duration_us: md.get::<FrameDuration>().ok().map(|v| v.0),
                exposure_us: md.get::<ExposureTime>().ok().map(|v| v.0),
                gain: md.get::<AnalogueGain>().ok().map(|v| v.0),
                copy_us,
            });
        } else {
            tracing::debug!(status = ?req.status(), "request not complete");
        }
        if last_report.elapsed() >= Duration::from_secs(1) {
            last_report = Instant::now();
            let t = timing.take_summary();
            tracing::info!(
                frames = t.frames,
                dropped = t.dropped,
                sensor_fps = t.sensor_fps.map(|f| (f * 10.0).round() / 10.0),
                frame_duration_us = t.frame_duration_us.map(|d| d.round()),
                exposure_us_max = t.exposure_us_max,
                gain_mean = t.gain_mean.map(|g| (g * 100.0).round() / 100.0),
                copy_us_max = t.copy_us_max,
                "camera"
            );
        }
        req.reuse(ReuseFlag::REUSE_BUFFERS);
        if !pending.is_empty() {
            pending.apply(req.controls_mut())?;
        }
        cam.queue_request(req).map_err(|(_, e)| e).context("re-queueing request")?;
    }
    cam.stop().context("stopping camera")?;
    Ok(())
}
