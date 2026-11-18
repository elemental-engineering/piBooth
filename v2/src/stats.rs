//! Rolling rate meter for `--bench` reporting.

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

#[cfg(test)]
mod tests {
    use super::*;

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
