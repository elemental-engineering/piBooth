//! Camera sources. Each implementation runs its own thread and publishes
//! preview frames into a [`FrameMailbox`].

use std::sync::Arc;

use crate::frame::FrameMailbox;

pub mod fake;

/// IMX296 native resolution (spec §1).
pub const SENSOR_SIZE: (u32, u32) = (1456, 1088);
/// Sensor maximum frame rate at full resolution.
pub const SENSOR_FPS: u32 = 60;

pub trait Camera {
    /// Start streaming. Returns once the camera thread is running; errors
    /// that happen during setup are returned here.
    fn start(&mut self, mailbox: Arc<FrameMailbox>) -> anyhow::Result<()>;
    /// Stop streaming and join the camera thread. Idempotent.
    fn stop(&mut self);
}
