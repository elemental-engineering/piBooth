//! Preview frames and the latest-frame mailbox between the camera thread
//! (writer) and the render loop (reader). The writer never blocks on the
//! reader: an unread frame is simply replaced, and its buffer recycled.

use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Tightly packed 8-bit RGB (R, G, B byte order), row 0 at the top.
#[derive(Debug)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
    /// Camera sequence number; gaps mean the sensor dropped frames.
    pub seq: u64,
    pub captured_at: Instant,
}

impl Frame {
    pub fn byte_len(width: u32, height: u32) -> usize {
        width as usize * height as usize * 3
    }
}

/// Copy `height` rows of `row_bytes` from `src` (rows `stride` bytes apart,
/// as delivered by the ISP) into tightly packed `dst`.
pub fn copy_packed(src: &[u8], stride: usize, row_bytes: usize, height: usize, dst: &mut [u8]) {
    assert!(stride >= row_bytes, "stride {stride} < row {row_bytes}");
    assert!(dst.len() >= row_bytes * height, "dst too small");
    assert!(src.len() >= stride * (height - 1) + row_bytes, "src too small");
    if stride == row_bytes {
        dst[..row_bytes * height].copy_from_slice(&src[..row_bytes * height]);
        return;
    }
    for (y, out) in dst.chunks_exact_mut(row_bytes).take(height).enumerate() {
        out.copy_from_slice(&src[y * stride..y * stride + row_bytes]);
    }
}

/// Spare buffers kept for reuse. Writer + reader + one in flight.
const MAX_SPARES: usize = 3;

#[derive(Default)]
struct Inner {
    latest: Option<Frame>,
    spares: Vec<Vec<u8>>,
    published: u64,
    overwritten: u64,
    closed: Option<String>,
}

#[derive(Default)]
pub struct FrameMailbox {
    inner: Mutex<Inner>,
}

impl FrameMailbox {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// A buffer of exactly `len` bytes, reusing a recycled one when possible.
    /// Contents are unspecified; the writer overwrites all of it.
    pub fn take_buffer(&self, len: usize) -> Vec<u8> {
        let spare = self.inner.lock().unwrap().spares.pop();
        let mut buf = spare.unwrap_or_default();
        buf.resize(len, 0);
        buf
    }

    /// Make `frame` the latest. An unread previous frame is dropped
    /// (counted in `overwritten`) and its buffer recycled.
    pub fn publish(&self, frame: Frame) {
        let mut g = self.inner.lock().unwrap();
        g.published += 1;
        if let Some(old) = g.latest.replace(frame) {
            g.overwritten += 1;
            if g.spares.len() < MAX_SPARES {
                g.spares.push(old.data);
            }
        }
    }

    /// Take the newest frame, if one arrived since the last call.
    pub fn take_latest(&self) -> Option<Frame> {
        self.inner.lock().unwrap().latest.take()
    }

    /// Return a frame's buffer after the reader is done with it.
    pub fn recycle(&self, frame: Frame) {
        let mut g = self.inner.lock().unwrap();
        if g.spares.len() < MAX_SPARES {
            g.spares.push(frame.data);
        }
    }

    /// Mark the producer as gone. The first reason wins.
    pub fn close(&self, reason: impl Into<String>) {
        let mut g = self.inner.lock().unwrap();
        if g.closed.is_none() {
            g.closed = Some(reason.into());
        }
    }

    /// Why the producer stopped, if it did.
    pub fn closed_reason(&self) -> Option<String> {
        self.inner.lock().unwrap().closed.clone()
    }

    /// (frames published, frames overwritten before being read)
    pub fn counters(&self) -> (u64, u64) {
        let g = self.inner.lock().unwrap();
        (g.published, g.overwritten)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(mb: &FrameMailbox, seq: u64) -> Frame {
        Frame { width: 2, height: 1, data: mb.take_buffer(6), seq, captured_at: Instant::now() }
    }

    #[test]
    fn copy_packed_strips_row_padding() {
        // 2 rows of 3 bytes, stride 5 (2 bytes padding per row).
        let src = [1, 2, 3, 0, 0, 4, 5, 6, 0, 0];
        let mut dst = [9; 6];
        copy_packed(&src, 5, 3, 2, &mut dst);
        assert_eq!(dst, [1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn copy_packed_fast_path_and_short_last_row() {
        let mut dst = [0; 6];
        copy_packed(&[1, 2, 3, 4, 5, 6], 3, 3, 2, &mut dst);
        assert_eq!(dst, [1, 2, 3, 4, 5, 6]);
        // Last row need not include trailing padding.
        let mut dst = [0; 4];
        copy_packed(&[1, 2, 0, 3, 4], 3, 2, 2, &mut dst);
        assert_eq!(dst, [1, 2, 3, 4]);
    }

    #[test]
    fn reader_gets_newest_and_only_once() {
        let mb = FrameMailbox::new();
        assert!(mb.take_latest().is_none());
        mb.publish(frame(&mb, 1));
        mb.publish(frame(&mb, 2));
        assert_eq!(mb.take_latest().unwrap().seq, 2);
        assert!(mb.take_latest().is_none());
        assert_eq!(mb.counters(), (2, 1));
    }

    #[test]
    fn not_closed_initially() {
        assert_eq!(FrameMailbox::new().closed_reason(), None);
    }

    #[test]
    fn close_sets_reason() {
        let mb = FrameMailbox::new();
        mb.close("boom");
        assert_eq!(mb.closed_reason().as_deref(), Some("boom"));
    }

    #[test]
    fn first_close_reason_wins() {
        let mb = FrameMailbox::new();
        mb.close("first");
        mb.close("second");
        assert_eq!(mb.closed_reason().as_deref(), Some("first"));
    }

    #[test]
    fn publish_and_take_work_after_close() {
        let mb = FrameMailbox::new();
        mb.close("gone");
        mb.publish(frame(&mb, 7));
        assert_eq!(mb.take_latest().unwrap().seq, 7);
    }

    #[test]
    fn buffers_are_recycled() {
        let mb = FrameMailbox::new();
        let f = frame(&mb, 1);
        let ptr = f.data.as_ptr();
        mb.recycle(f);
        let buf = mb.take_buffer(6);
        assert_eq!(buf.as_ptr(), ptr);
        assert_eq!(buf.len(), 6);
    }

    #[test]
    fn spare_pool_is_bounded() {
        let mb = FrameMailbox::new();
        for seq in 0..10 {
            mb.recycle(frame(&mb, seq));
            let extra = Frame { width: 2, height: 1, data: vec![0; 6], seq, captured_at: Instant::now() };
            mb.recycle(extra);
        }
        assert!(mb.inner.lock().unwrap().spares.len() <= MAX_SPARES);
    }

    #[test]
    fn writer_and_reader_on_different_threads() {
        let mb = FrameMailbox::new();
        let w = mb.clone();
        let t = std::thread::spawn(move || {
            for seq in 0..1000 {
                let f = Frame { width: 2, height: 1, data: w.take_buffer(6), seq, captured_at: Instant::now() };
                w.publish(f);
            }
        });
        let mut last = None;
        while last != Some(999) {
            if let Some(f) = mb.take_latest() {
                assert!(last.is_none_or(|l| f.seq > l), "seq must increase");
                last = Some(f.seq);
                mb.recycle(f);
            }
        }
        t.join().unwrap();
        assert_eq!(mb.counters().0, 1000);
    }
}
