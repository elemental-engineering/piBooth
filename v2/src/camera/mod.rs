//! Camera sources. Each implementation runs its own thread and publishes
//! preview frames into a [`FrameMailbox`].

use std::sync::Arc;

use crate::frame::FrameMailbox;

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

pub trait Camera {
    /// Start streaming. Returns once the camera thread is running; errors
    /// that happen during setup are returned here.
    fn start(&mut self, mailbox: Arc<FrameMailbox>) -> anyhow::Result<()>;
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
