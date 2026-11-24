//! GPIO strobe (spec §4.5). The light is driven from its own thread so the
//! hard limit (`flash.max_ms`) holds even if the render loop stalls: every
//! "on" carries a deadline after which the thread turns the line off by
//! itself. Dropping the [`Strobe`] turns it off and releases the line.

use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::Context;

use crate::config::Flash;
use crate::input::line_name;

/// Something that can be switched on and off (a GPIO line, or a test probe).
pub trait Output: Send {
    fn set(&mut self, on: bool) -> anyhow::Result<()>;
}

/// The strobe's output line, requested as an output that starts off.
pub struct GpioLine {
    req: gpiocdev::Request,
}

impl GpioLine {
    pub fn open(cfg: &Flash) -> anyhow::Result<Self> {
        let name = line_name(cfg.gpio_line);
        let found = gpiocdev::find_named_line(&name).with_context(|| format!("GPIO line {name} not found"))?;
        let mut builder = gpiocdev::Request::builder();
        builder.with_consumer("pibooth-strobe").with_found_line(&found).as_output(gpiocdev::line::Value::Inactive);
        if !cfg.active_high {
            builder.as_active_low();
        }
        let req = builder.request().with_context(|| format!("requesting {name}"))?;
        tracing::info!(line = %name, chip = %found.chip.display(), active_high = cfg.active_high, "strobe ready");
        Ok(Self { req })
    }
}

impl Output for GpioLine {
    fn set(&mut self, on: bool) -> anyhow::Result<()> {
        let v = if on { gpiocdev::line::Value::Active } else { gpiocdev::line::Value::Inactive };
        self.req.set_lone_value(v).context("setting strobe line")?;
        Ok(())
    }
}

pub struct Strobe {
    tx: Option<Sender<bool>>,
    thread: Option<JoinHandle<()>>,
}

impl Strobe {
    /// Drive `output` from a new thread; each "on" lasts at most `max_on`.
    pub fn start(output: Box<dyn Output>, max_on: Duration) -> anyhow::Result<Self> {
        let (tx, rx) = channel();
        let thread = std::thread::Builder::new().name("strobe".into()).spawn(move || run(output, rx, max_on))?;
        Ok(Self { tx: Some(tx), thread: Some(thread) })
    }

    /// Never blocks.
    pub fn set(&self, on: bool) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(on);
        }
    }
}

impl Drop for Strobe {
    fn drop(&mut self) {
        self.tx.take();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn run(mut output: Box<dyn Output>, rx: Receiver<bool>, max_on: Duration) {
    let mut set = |on: bool| {
        if let Err(e) = output.set(on) {
            tracing::error!("strobe: {e:#}");
        }
    };
    set(false);
    let mut deadline: Option<Instant> = None;
    loop {
        let msg = match deadline {
            Some(d) => rx.recv_timeout(d.saturating_duration_since(Instant::now())),
            None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };
        match msg {
            Ok(true) => {
                if deadline.is_none() {
                    set(true);
                    deadline = Some(Instant::now() + max_on);
                }
            }
            Ok(false) => {
                if deadline.take().is_some() {
                    set(false);
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                tracing::warn!(max_ms = max_on.as_millis() as u64, "strobe forced off after flash.max_ms");
                deadline = None;
                set(false);
            }
            Err(RecvTimeoutError::Disconnected) => {
                set(false);
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    const MS: Duration = Duration::from_millis(1);

    #[derive(Clone, Default)]
    struct Probe(Arc<Mutex<Vec<(bool, Instant)>>>);
    impl Output for Probe {
        fn set(&mut self, on: bool) -> anyhow::Result<()> {
            self.0.lock().unwrap().push((on, Instant::now()));
            Ok(())
        }
    }
    impl Probe {
        fn states(&self) -> Vec<bool> {
            self.0.lock().unwrap().iter().map(|(on, _)| *on).collect()
        }
        fn wait_for(&self, n: usize) {
            let until = Instant::now() + Duration::from_secs(2);
            while self.0.lock().unwrap().len() < n {
                assert!(Instant::now() < until, "only {:?}", self.states());
                std::thread::sleep(MS);
            }
        }
    }

    #[test]
    fn on_then_off() {
        let p = Probe::default();
        let s = Strobe::start(Box::new(p.clone()), Duration::from_secs(5)).unwrap();
        s.set(true);
        s.set(true); // repeated "on" is not a new pulse
        s.set(false);
        s.set(false);
        p.wait_for(3);
        drop(s);
        assert_eq!(p.states(), vec![false, true, false, false], "starts off, one pulse, off at drop");
    }

    #[test]
    fn hard_limit_turns_it_off_by_itself() {
        let p = Probe::default();
        let s = Strobe::start(Box::new(p.clone()), 50 * MS).unwrap();
        s.set(true);
        p.wait_for(3);
        let log = p.0.lock().unwrap().clone();
        assert_eq!(log.iter().map(|e| e.0).collect::<Vec<_>>(), vec![false, true, false]);
        let lit = log[2].1 - log[1].1;
        assert!(lit >= 50 * MS && lit < 500 * MS, "{lit:?}");
        // A later "on" works again.
        s.set(true);
        p.wait_for(4);
        assert!(p.states()[3]);
    }

    #[test]
    fn drop_turns_it_off() {
        let p = Probe::default();
        let s = Strobe::start(Box::new(p.clone()), Duration::from_secs(5)).unwrap();
        s.set(true);
        p.wait_for(2);
        drop(s);
        assert_eq!(p.states(), vec![false, true, false]);
    }
}
