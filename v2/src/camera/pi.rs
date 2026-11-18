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
use libcamera::controls::FrameDurationLimits;
use libcamera::framebuffer::AsFrameBuffer;
use libcamera::framebuffer_allocator::{FrameBuffer, FrameBufferAllocator};
use libcamera::framebuffer_map::MemoryMappedFrameBuffer;
use libcamera::geometry::Size;
use libcamera::pixel_format::PixelFormat;
use libcamera::request::{RequestStatus, ReuseFlag};
use libcamera::stream::StreamRole;

use super::{Camera, SENSOR_FPS, SENSOR_SIZE};
use crate::config::CameraCfg;
use crate::frame::{Frame, FrameMailbox, copy_packed};

/// libcamera `BGR888` = DRM `BG24`: bytes in memory are R, G, B, which is
/// what GL `RGB` expects. (libcamera `RGB888` is B, G, R in memory.)
const PREVIEW_FORMAT: PixelFormat = PixelFormat::new(u32::from_le_bytes(*b"BG24"), 0);
const BUFFER_COUNT: u32 = 4;

pub struct PiCamera {
    cfg: CameraCfg,
    running: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl PiCamera {
    pub fn new(cfg: CameraCfg) -> Self {
        Self { cfg, running: Arc::new(AtomicBool::new(false)), thread: None }
    }
}

impl Camera for PiCamera {
    fn start(&mut self, mailbox: Arc<FrameMailbox>) -> anyhow::Result<()> {
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
        let thread = std::thread::Builder::new().name("pi-camera".into()).spawn(move || {
            if let Err(e) = session(&mailbox, &running, &ready_tx) {
                // If setup failed, `start` is still waiting for this message.
                let _ = ready_tx.send(Err(format!("{e:#}")));
                tracing::error!("camera session ended: {e:#}");
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

    fn stop(&mut self) {
        self.running.store(false, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for PiCamera {
    fn drop(&mut self) {
        self.stop();
    }
}

fn session(
    mailbox: &FrameMailbox,
    running: &AtomicBool,
    ready: &mpsc::Sender<Result<(), String>>,
) -> anyhow::Result<()> {
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

    let (w, h) = (size.width, size.height);
    let row_bytes = w as usize * 3;
    while running.load(Ordering::SeqCst) {
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
            let planes = fb.data();
            let mut data = mailbox.take_buffer(Frame::byte_len(w, h));
            copy_packed(planes[0], stride, row_bytes, h as usize, &mut data);
            mailbox.publish(Frame { width: w, height: h, data, seq, captured_at: Instant::now() });
        }
        req.reuse(ReuseFlag::REUSE_BUFFERS);
        cam.queue_request(req).map_err(|(_, e)| e).context("re-queueing request")?;
    }
    cam.stop().context("stopping camera")?;
    Ok(())
}
