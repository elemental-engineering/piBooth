//! Animated test pattern at the sensor's size and rate, for development
//! without a Pi and for CI smoke tests.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::{Camera, CameraCommand, CameraLink, CaptureMeta, CapturedFrame};
use crate::frame::Frame;

pub struct FakeCamera {
    size: (u32, u32),
    fps: u32,
    running: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    commands: Option<Sender<CameraCommand>>,
}

impl FakeCamera {
    pub fn new(size: (u32, u32), fps: u32) -> Self {
        Self { size, fps, running: Arc::new(AtomicBool::new(false)), thread: None, commands: None }
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
    fn start(&mut self, link: CameraLink) -> anyhow::Result<()> {
        if self.thread.is_some() {
            return Ok(());
        }
        self.running.store(true, Ordering::SeqCst);
        let running = self.running.clone();
        let (w, h) = self.size;
        let period = Duration::from_secs_f64(1.0 / self.fps as f64);
        let (cmd_tx, cmd_rx) = channel();
        self.commands = Some(cmd_tx);
        self.thread = Some(std::thread::Builder::new().name("fake-camera".into()).spawn(move || {
            let mut next = Instant::now();
            let mut seq = 0u64;
            let mut pending = None;
            while running.load(Ordering::SeqCst) {
                for cmd in cmd_rx.try_iter() {
                    if let CameraCommand::Capture { id } = cmd {
                        pending = Some(id);
                    }
                }
                let mut data = link.mailbox.take_buffer(Frame::byte_len(w, h));
                fill_test_pattern(&mut data, w, h, seq);
                if let Some(id) = pending.take() {
                    let frame = Frame { width: w, height: h, data: data.clone(), seq, captured_at: Instant::now() };
                    let meta = CaptureMeta { exposure_us: Some(10_000), gain: Some(1.0) };
                    let _ = link.captures.send(CapturedFrame { id, frame, meta });
                }
                link.mailbox.publish(Frame { width: w, height: h, data, seq, captured_at: Instant::now() });
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

impl Drop for FakeCamera {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::FrameMailbox;

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

    fn link() -> (CameraLink, Arc<FrameMailbox>, std::sync::mpsc::Receiver<CapturedFrame>) {
        let mb = FrameMailbox::new();
        let (tx, rx) = channel();
        (CameraLink { mailbox: mb.clone(), captures: tx }, mb, rx)
    }

    #[test]
    fn streams_at_roughly_requested_rate() {
        let (link, mb, _rx) = link();
        let mut cam = FakeCamera::new((64, 32), 100);
        cam.start(link).unwrap();
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
        cam.start(link().0).unwrap();
        cam.stop();
        cam.stop();
        cam.command(CameraCommand::Capture { id: 1 }); // ignored when stopped
    }

    #[test]
    fn capture_delivers_one_full_frame_with_its_id() {
        let (link, _mb, rx) = link();
        let mut cam = FakeCamera::new((64, 32), 100);
        cam.start(link).unwrap();
        cam.command(CameraCommand::PrepareCapture);
        cam.command(CameraCommand::Capture { id: 42 });
        let got = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(got.id, 42);
        assert_eq!(got.frame.data.len(), 64 * 32 * 3);
        assert_eq!(&got.frame.data[0..3], &[255, 0, 0], "not mirrored: marker top-left");
        assert!(got.meta.exposure_us.is_some());
        std::thread::sleep(Duration::from_millis(50));
        assert!(rx.try_recv().is_err(), "exactly one frame per capture");
        cam.stop();
    }
}
