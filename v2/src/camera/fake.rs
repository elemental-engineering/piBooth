//! Animated test pattern at the sensor's size and rate, for development
//! without a Pi and for CI smoke tests.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::Camera;
use crate::frame::{Frame, FrameMailbox};

pub struct FakeCamera {
    size: (u32, u32),
    fps: u32,
    running: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl FakeCamera {
    pub fn new(size: (u32, u32), fps: u32) -> Self {
        Self { size, fps, running: Arc::new(AtomicBool::new(false)), thread: None }
    }
}

/// Grey horizontal gradient, a white vertical bar moving 8 px per frame,
/// and a red square in the top-left corner so mirroring is visible.
pub fn fill_test_pattern(buf: &mut [u8], width: u32, height: u32, seq: u64) {
    let (w, h) = (width as usize, height as usize);
    assert_eq!(buf.len(), w * h * 3);
    let stride = w * 3;
    let denom = (w - 1).max(1);
    // Build one gradient row, then copy it down (fast even in debug builds).
    for x in 0..w {
        let g = (x * 255 / denom) as u8;
        buf[x * 3..x * 3 + 3].copy_from_slice(&[g, g, g]);
    }
    let (first, rest) = buf.split_at_mut(stride);
    for row in rest.chunks_exact_mut(stride) {
        row.copy_from_slice(first);
    }
    let bar_x = (seq as usize * 8) % w;
    let marker = (h / 8).max(1).min(w);
    for (y, row) in buf.chunks_exact_mut(stride).enumerate() {
        for x in bar_x.saturating_sub(3)..(bar_x + 4).min(w) {
            row[x * 3..x * 3 + 3].copy_from_slice(&[255, 255, 255]);
        }
        if y < marker {
            for px in row[..marker * 3].chunks_exact_mut(3) {
                px.copy_from_slice(&[255, 0, 0]);
            }
        }
    }
}

impl Camera for FakeCamera {
    fn start(&mut self, mailbox: Arc<FrameMailbox>) -> anyhow::Result<()> {
        if self.thread.is_some() {
            return Ok(());
        }
        self.running.store(true, Ordering::SeqCst);
        let running = self.running.clone();
        let (w, h) = self.size;
        let period = Duration::from_secs_f64(1.0 / self.fps as f64);
        self.thread = Some(std::thread::Builder::new().name("fake-camera".into()).spawn(move || {
            let mut next = Instant::now();
            let mut seq = 0u64;
            while running.load(Ordering::SeqCst) {
                let mut data = mailbox.take_buffer(Frame::byte_len(w, h));
                fill_test_pattern(&mut data, w, h, seq);
                mailbox.publish(Frame { width: w, height: h, data, seq, captured_at: Instant::now() });
                seq += 1;
                next += period;
                let now = Instant::now();
                if next > now {
                    std::thread::sleep(next - now);
                } else {
                    next = now; // fell behind; don't try to catch up
                }
            }
        })?);
        Ok(())
    }

    fn stop(&mut self) {
        self.running.store(false, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            if t.join().is_err() {
                tracing::error!("camera thread panicked");
            }
        }
    }
}

impl Drop for FakeCamera {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pattern_has_marker_and_bar() {
        let (w, h) = (64u32, 32u32);
        let mut buf = vec![0; Frame::byte_len(w, h)];
        fill_test_pattern(&mut buf, w, h, 2); // bar at x=16
        assert_eq!(&buf[0..3], &[255, 0, 0], "top-left marker");
        let px = |x: usize, y: usize| &buf[(y * w as usize + x) * 3..][..3];
        assert_eq!(px(16, 20), &[255, 255, 255], "bar");
        assert_eq!(px(63, 0)[0], 255, "gradient right edge");
        assert_eq!(px(40, 20)[0], px(40, 20)[1], "grey");
    }

    #[test]
    fn streams_at_roughly_requested_rate() {
        let mb = FrameMailbox::new();
        let mut cam = FakeCamera::new((64, 32), 100);
        cam.start(mb.clone()).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        cam.stop();
        let (published, _) = mb.counters();
        assert!((15..=40).contains(&published), "published {published}");
        let f = mb.take_latest().unwrap();
        assert_eq!((f.width, f.height, f.data.len()), (64, 32, 64 * 32 * 3));
    }

    #[test]
    fn stop_is_idempotent() {
        let mut cam = FakeCamera::new((8, 8), 30);
        cam.stop();
        cam.start(FrameMailbox::new()).unwrap();
        cam.stop();
        cam.stop();
    }
}
