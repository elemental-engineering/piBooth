//! Rate meter and camera frame-timing statistics for `--bench` reporting.

use std::time::{Duration, Instant};

pub struct RateMeter {
    window: Duration,
    started: Instant,
    count: u64,
}

impl RateMeter {
    pub fn new(window: Duration, now: Instant) -> Self {
        Self { window, started: now, count: 0 }
    }

    pub fn tick(&mut self) {
        self.count += 1;
    }

    /// Events per second once `window` has elapsed since the last report,
    /// then restarts the window.
    pub fn poll(&mut self, now: Instant) -> Option<f64> {
        let elapsed = now.duration_since(self.started);
        if elapsed < self.window {
            return None;
        }
        let rate = self.count as f64 / elapsed.as_secs_f64();
        self.started = now;
        self.count = 0;
        Some(rate)
    }
}

/// Metadata of one completed camera frame, as reported by libcamera.
#[derive(Debug, Clone, Copy, Default)]
pub struct FrameSample {
    /// Sensor frame sequence number; gaps mean frames dropped before us.
    pub seq: u64,
    /// Start-of-exposure timestamp, nanoseconds (libcamera `SensorTimestamp`).
    pub sensor_ts_ns: Option<i64>,
    /// libcamera `FrameDuration` metadata, µs.
    pub frame_duration_us: Option<i64>,
    pub exposure_us: Option<i32>,
    pub gain: Option<f32>,
    /// Time spent copying the frame out of the camera buffer, µs.
    pub copy_us: u64,
}

/// Summary of the frames recorded since the previous summary.
#[derive(Debug, Clone, PartialEq)]
pub struct TimingSummary {
    pub frames: u64,
    pub dropped: u64,
    /// Rate measured from sensor timestamps (true sensor cadence).
    pub sensor_fps: Option<f64>,
    pub frame_duration_us: Option<f64>,
    pub exposure_us_max: Option<i32>,
    pub gain_mean: Option<f32>,
    pub copy_us_max: u64,
}

/// Accumulates [`FrameSample`]s; [`take_summary`](Self::take_summary)
/// resets the window but keeps the previous frame so gaps and intervals
/// spanning two windows are still counted.
#[derive(Default)]
pub struct FrameTiming {
    frames: u64,
    dropped: u64,
    last_seq: Option<u64>,
    last_ts: Option<i64>,
    interval_ns: i64,
    intervals: u64,
    duration_sum: i64,
    durations: u64,
    exposure_max: Option<i32>,
    gain_sum: f32,
    gains: u32,
    copy_max: u64,
}

impl FrameTiming {
    pub fn record(&mut self, s: FrameSample) {
        self.frames += 1;
        if let Some(last) = self.last_seq
            && s.seq > last + 1
        {
            self.dropped += s.seq - last - 1;
        }
        self.last_seq = Some(s.seq);
        if let Some(ts) = s.sensor_ts_ns {
            if let Some(last) = self.last_ts
                && ts > last
            {
                self.interval_ns += ts - last;
                // Count the sensor frames covered, so drops don't inflate the rate.
                self.intervals += 1;
            }
            self.last_ts = Some(ts);
        }
        if let Some(d) = s.frame_duration_us {
            self.duration_sum += d;
            self.durations += 1;
        }
        if let Some(e) = s.exposure_us {
            self.exposure_max = Some(self.exposure_max.map_or(e, |m| m.max(e)));
        }
        if let Some(g) = s.gain {
            self.gain_sum += g;
            self.gains += 1;
        }
        self.copy_max = self.copy_max.max(s.copy_us);
    }

    pub fn take_summary(&mut self) -> TimingSummary {
        let sensor_fps = (self.intervals > 0 && self.interval_ns > 0).then(|| {
            // Each interval may span dropped frames; use the frame count implied by the sequence.
            let frames_spanned = self.intervals + self.dropped;
            frames_spanned as f64 / (self.interval_ns as f64 / 1e9)
        });
        let summary = TimingSummary {
            frames: self.frames,
            dropped: self.dropped,
            sensor_fps,
            frame_duration_us: (self.durations > 0).then(|| self.duration_sum as f64 / self.durations as f64),
            exposure_us_max: self.exposure_max,
            gain_mean: (self.gains > 0).then(|| self.gain_sum / self.gains as f32),
            copy_us_max: self.copy_max,
        };
        *self = Self { last_seq: self.last_seq, last_ts: self.last_ts, ..Self::default() };
        summary
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(seq: u64, ts_ms: f64) -> FrameSample {
        FrameSample {
            seq,
            sensor_ts_ns: Some((ts_ms * 1e6) as i64),
            frame_duration_us: Some(16_667),
            exposure_us: Some(10_000),
            gain: Some(2.0),
            copy_us: 1_500,
        }
    }

    #[test]
    fn steady_60fps_without_drops() {
        let mut t = FrameTiming::default();
        for i in 0..61 {
            t.record(sample(i, i as f64 * 16.667));
        }
        let s = t.take_summary();
        assert_eq!(s.frames, 61);
        assert_eq!(s.dropped, 0);
        assert!((s.sensor_fps.unwrap() - 60.0).abs() < 0.01, "{s:?}");
        assert_eq!(s.frame_duration_us, Some(16_667.0));
        assert_eq!(s.exposure_us_max, Some(10_000));
        assert_eq!(s.gain_mean, Some(2.0));
        assert_eq!(s.copy_us_max, 1_500);
    }

    #[test]
    fn sequence_gaps_count_as_dropped() {
        let mut t = FrameTiming::default();
        for seq in [10, 11, 14, 15, 20] {
            t.record(sample(seq, seq as f64 * 16.667));
        }
        let s = t.take_summary();
        assert_eq!(s.frames, 5);
        assert_eq!(s.dropped, 2 + 4);
        // Sensor rate comes from timestamps, so it stays ~60 despite drops.
        assert!((s.sensor_fps.unwrap() - 60.0).abs() < 0.01, "{s:?}");
    }

    #[test]
    fn slow_sensor_shows_in_rate_and_duration() {
        let mut t = FrameTiming::default();
        for i in 0..10 {
            let mut s = sample(i, i as f64 * 18.0);
            s.frame_duration_us = Some(18_000);
            s.exposure_us = Some(17_900);
            t.record(s);
        }
        let s = t.take_summary();
        assert!((s.sensor_fps.unwrap() - 55.555).abs() < 0.01, "{s:?}");
        assert_eq!(s.frame_duration_us, Some(18_000.0));
        assert_eq!(s.exposure_us_max, Some(17_900));
    }

    #[test]
    fn windows_are_continuous_and_reset() {
        let mut t = FrameTiming::default();
        t.record(sample(0, 0.0));
        t.record(sample(1, 16.667));
        let _ = t.take_summary();
        // Next window: the gap and interval to the previous frame still count.
        t.record(sample(3, 50.0));
        let s = t.take_summary();
        assert_eq!(s.frames, 1);
        assert_eq!(s.dropped, 1);
        assert!(s.sensor_fps.is_some());
        let empty = t.take_summary();
        assert_eq!((empty.frames, empty.dropped, empty.sensor_fps), (0, 0, None));
    }

    #[test]
    fn missing_metadata_gives_none() {
        let mut t = FrameTiming::default();
        t.record(FrameSample { seq: 1, ..Default::default() });
        let s = t.take_summary();
        assert_eq!((s.sensor_fps, s.frame_duration_us, s.exposure_us_max, s.gain_mean), (None, None, None, None));
    }

    #[test]
    fn reports_after_window_then_resets() {
        let t0 = Instant::now();
        let mut m = RateMeter::new(Duration::from_secs(1), t0);
        for _ in 0..60 {
            m.tick();
        }
        assert_eq!(m.poll(t0 + Duration::from_millis(500)), None);
        let r = m.poll(t0 + Duration::from_secs(1)).unwrap();
        assert!((r - 60.0).abs() < 1e-9);
        assert_eq!(m.poll(t0 + Duration::from_secs(2)), Some(0.0));
    }
}
