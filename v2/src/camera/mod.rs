//! Camera sources. Each implementation runs its own thread and publishes
//! preview frames into a [`FrameMailbox`].

use std::sync::Arc;
use std::sync::mpsc::Sender;

use crate::frame::{Frame, FrameMailbox};

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
    /// Deliver the next complete frame as capture `id` (full resolution,
    /// never mirrored); it is metered under the flash since `PrepareCapture`.
    /// Exposure and white balance are then locked for the frames after it
    /// (used by milestone 5 bursts) until `ReleaseCapture`.
    Capture { id: u64 },
    /// Back to preview settings: auto exposure/white balance, fast noise reduction.
    ReleaseCapture,
}

/// Exposure settings the frame was taken with (for EXIF).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct CaptureMeta {
    pub exposure_us: Option<i32>,
    pub gain: Option<f32>,
}

#[derive(Debug)]
pub struct CapturedFrame {
    pub id: u64,
    pub frame: Frame,
    pub meta: CaptureMeta,
}

/// Where a running camera sends its output.
pub struct CameraLink {
    /// Latest preview frame (overwritten, never blocks the camera).
    pub mailbox: Arc<FrameMailbox>,
    /// Captured frames, one per `Capture` command.
    pub captures: Sender<CapturedFrame>,
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

    #[test]
    fn backoff_doubles_to_max_and_resets() {
        let mut b = Backoff::new(Duration::from_secs(1), Duration::from_secs(30));
        let got: Vec<u64> = (0..7).map(|_| b.next_delay().as_secs()).collect();
        assert_eq!(got, vec![1, 2, 4, 8, 16, 30, 30]);
        b.reset();
        assert_eq!(b.next_delay(), Duration::from_secs(1));
    }
}
