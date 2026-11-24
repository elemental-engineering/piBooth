//! Camera sources. Each implementation runs its own thread and publishes
//! preview frames into a [`FrameMailbox`].

use std::sync::Arc;
use std::sync::mpsc::Sender;

use crate::frame::{Frame, FrameMailbox};
use crate::pipeline::dng::{RawColour, RawImage};

pub mod fake;

#[cfg(feature = "libcamera")]
pub mod pi;

/// IMX296 native resolution (spec §1).
pub const SENSOR_SIZE: (u32, u32) = (1456, 1088);
/// Sensor maximum frame rate at full resolution.
pub const SENSOR_FPS: u32 = 60;

/// Delay before each camera restart attempt: doubles from `initial` up to
/// `max`, and resets after a successful start (spec §5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Backoff {
    initial: std::time::Duration,
    max: std::time::Duration,
    next: std::time::Duration,
}

impl Backoff {
    pub fn new(initial: std::time::Duration, max: std::time::Duration) -> Self {
        Self { initial, max, next: initial }
    }

    /// The delay to wait now; the following call returns twice as much.
    pub fn next_delay(&mut self) -> std::time::Duration {
        let d = self.next;
        self.next = (self.next * 2).min(self.max);
        d
    }

    pub fn reset(&mut self) {
        self.next = self.initial;
    }
}

/// Capture-related requests from the booth to the camera thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CameraCommand {
    /// Switch to high-quality noise reduction; auto-exposure keeps metering.
    PrepareCapture,
    /// Deliver a burst of `frames` (≥ 1) full-resolution, never mirrored
    /// frames as capture `id`. The first is the next complete frame, metered
    /// under the flash since `PrepareCapture`; exposure and white balance are
    /// then locked, and the rest are the first `frames - 1` frames taken with
    /// the lock in effect (spec §4.2). Locked until `ReleaseCapture`.
    Capture { id: u64, frames: u32 },
    /// Back to preview settings: auto exposure/white balance, fast noise reduction.
    ReleaseCapture,
}

/// Exposure settings the first frame was taken with (for EXIF).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct CaptureMeta {
    pub exposure_us: Option<i32>,
    pub gain: Option<f32>,
}

/// The raw Bayer frame matching the first burst frame (`capture.save_dng`).
#[derive(Debug, Clone, PartialEq)]
pub struct RawCapture {
    pub image: RawImage,
    pub colour: RawColour,
}

#[derive(Debug)]
pub struct CapturedBurst {
    pub id: u64,
    /// In capture order; `frames[0]` is the photo metered under the flash.
    pub frames: Vec<Frame>,
    pub meta: CaptureMeta,
    pub raw: Option<RawCapture>,
}

/// After the photo, frames still in flight were requested before the lock;
/// give up waiting for a locked one after this many and take them anyway.
pub const MAX_UNLOCKED_FRAMES: u32 = 8;

/// Collects one burst from completed frames (spec §4.2): the first frame
/// after `Capture` is the photo; the rest are taken from the first frame whose
/// metadata shows the exposure lock in effect.
#[derive(Debug)]
pub struct BurstCollector {
    burst: CapturedBurst,
    want: usize,
    locked: bool,
    skipped: u32,
}

impl BurstCollector {
    pub fn new(id: u64, frames: u32) -> Self {
        let burst = CapturedBurst { id, frames: Vec::new(), meta: CaptureMeta::default(), raw: None };
        Self { burst, want: frames.max(1) as usize, locked: false, skipped: 0 }
    }

    /// The next frame is the photo (record its metadata and raw frame).
    pub fn is_first(&self) -> bool {
        self.burst.frames.is_empty()
    }

    /// Whether to keep the next completed frame; `locked`: its metadata
    /// reports manual exposure (the lock reached the sensor).
    pub fn wants(&mut self, locked: bool) -> bool {
        if self.is_first() || self.locked {
            return true;
        }
        if locked {
            self.locked = true;
            return true;
        }
        self.skipped += 1;
        if self.skipped >= MAX_UNLOCKED_FRAMES {
            tracing::warn!(id = self.burst.id, "exposure lock not reported; continuing the burst without it");
            self.locked = true;
        }
        false
    }

    /// Add a kept frame (`first` only for the photo). Returns the finished
    /// burst once it has all its frames.
    pub fn push(&mut self, frame: Frame, first: Option<(CaptureMeta, Option<RawCapture>)>) -> Option<CapturedBurst> {
        if let Some((meta, raw)) = first {
            self.burst.meta = meta;
            self.burst.raw = raw;
        }
        self.burst.frames.push(frame);
        if self.burst.frames.len() < self.want {
            return None;
        }
        let empty = CapturedBurst { id: self.burst.id, frames: Vec::new(), meta: self.burst.meta, raw: None };
        Some(std::mem::replace(&mut self.burst, empty))
    }
}

/// Where a running camera sends its output.
pub struct CameraLink {
    /// Latest preview frame (overwritten, never blocks the camera).
    pub mailbox: Arc<FrameMailbox>,
    /// Captured bursts, one per `Capture` command.
    pub captures: Sender<CapturedBurst>,
}

/// A camera runs its own thread. It is `Send` so it can be started and
/// stopped off the render thread.
pub trait Camera: Send {
    /// Start streaming. Returns once the camera thread is running; errors
    /// that happen during setup are returned here.
    fn start(&mut self, link: CameraLink) -> anyhow::Result<()>;
    /// Queue a capture command; ignored if the camera is not running.
    fn command(&mut self, cmd: CameraCommand);
    /// Stop streaming and join the camera thread. Idempotent.
    fn stop(&mut self);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn frame(seq: u64) -> Frame {
        Frame { width: 1, height: 1, data: vec![0; 3], seq, captured_at: std::time::Instant::now() }
    }

    #[test]
    fn burst_keeps_the_photo_then_locked_frames() {
        let mut b = BurstCollector::new(7, 3);
        assert!(b.is_first() && b.wants(false), "the photo is the next frame");
        let meta = CaptureMeta { exposure_us: Some(9000), gain: Some(2.0) };
        assert!(b.push(frame(10), Some((meta, None))).is_none());
        assert!(!b.is_first());
        assert!(!b.wants(false) && !b.wants(false), "in-flight unlocked frames are skipped");
        assert!(b.wants(true));
        assert!(b.push(frame(13), None).is_none());
        assert!(b.wants(false), "once locked, every frame counts");
        let done = b.push(frame(14), None).expect("complete");
        assert_eq!((done.id, done.meta), (7, meta));
        assert_eq!(done.frames.iter().map(|f| f.seq).collect::<Vec<_>>(), vec![10, 13, 14]);
    }

    #[test]
    fn burst_does_not_wait_forever_for_the_lock() {
        let mut b = BurstCollector::new(1, 2);
        assert!(b.wants(false));
        b.push(frame(0), Some((CaptureMeta::default(), None)));
        let skipped = (0..20).take_while(|_| !b.wants(false)).count() as u32;
        assert_eq!(skipped, MAX_UNLOCKED_FRAMES);
        assert!(b.push(frame(9), None).is_some());
    }

    #[test]
    fn single_frame_burst_is_just_the_photo() {
        let mut b = BurstCollector::new(2, 0); // 0 is treated as 1
        assert!(b.wants(false));
        assert_eq!(b.push(frame(5), Some((CaptureMeta::default(), None))).unwrap().frames.len(), 1);
    }

    #[test]
    fn backoff_doubles_to_max_and_resets() {
        let mut b = Backoff::new(Duration::from_secs(1), Duration::from_secs(30));
        let got: Vec<u64> = (0..7).map(|_| b.next_delay().as_secs()).collect();
        assert_eq!(got, vec![1, 2, 4, 8, 16, 30, 30]);
        b.reset();
        assert_eq!(b.next_delay(), Duration::from_secs(1));
    }
}
