//! User input: keyboard keys and the GPIO push button, both turned into
//! [`Action`]s. One physical press yields exactly one `Press`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::Context;
use winit::keyboard::{Key, NamedKey};

use crate::config::Button;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Quit,
    Press,
    ToggleDebug,
    ToggleMirror,
    ToggleFilter,
}

/// Maintenance keys (spec §4.1). Auto-repeat is ignored so holding a key
/// acts once, like the button.
pub fn map_key(key: &Key, repeat: bool) -> Option<Action> {
    if repeat {
        return None;
    }
    match key {
        Key::Named(NamedKey::Escape) => Some(Action::Quit),
        Key::Named(NamedKey::Space) => Some(Action::Press),
        Key::Character(c) => match c.to_lowercase().as_str() {
            "q" => Some(Action::Quit),
            " " => Some(Action::Press),
            "d" => Some(Action::ToggleDebug),
            "r" => Some(Action::ToggleMirror),
            "f" => Some(Action::ToggleFilter),
            _ => None,
        },
        _ => None,
    }
}

/// Kernel line name of a BCM GPIO on Raspberry Pi (e.g. 27 → "GPIO27").
/// Looking lines up by name survives chip renumbering (v1 hard-coded
/// `gpiochip4`, which became `gpiochip0` on newer Pi 5 kernels).
pub fn line_name(bcm: u32) -> String {
    format!("GPIO{bcm}")
}

/// Watches the button line on its own thread and sends `Action::Press` on
/// every debounced press edge.
pub struct GpioButton {
    running: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl GpioButton {
    pub fn start(cfg: &Button, tx: Sender<Action>) -> anyhow::Result<Self> {
        let name = line_name(cfg.gpio_line);
        let found = gpiocdev::find_named_line(&name).with_context(|| format!("GPIO line {name} not found"))?;
        let mut builder = gpiocdev::Request::builder();
        builder
            .with_consumer("pibooth-button")
            .with_found_line(&found)
            .as_input()
            .with_edge_detection(gpiocdev::line::EdgeDetection::RisingEdge)
            .with_debounce_period(Duration::from_millis(cfg.debounce_ms as u64));
        if cfg.active_low {
            builder.as_active_low().with_bias(gpiocdev::line::Bias::PullUp);
        } else {
            builder.with_bias(gpiocdev::line::Bias::PullDown);
        }
        let req = builder.request().with_context(|| format!("requesting {name}"))?;
        tracing::info!(line = %name, chip = %found.chip.display(), active_low = cfg.active_low, "button ready");

        let running = Arc::new(AtomicBool::new(true));
        let flag = running.clone();
        let thread = std::thread::Builder::new().name("gpio-button".into()).spawn(move || {
            while flag.load(Ordering::SeqCst) {
                match req.wait_edge_event(Duration::from_millis(200)) {
                    Ok(false) => continue,
                    Ok(true) => match req.read_edge_event() {
                        // Rising = inactive→active, already corrected for active_low.
                        Ok(ev) if ev.kind == gpiocdev::line::EdgeKind::Rising => {
                            if tx.send(Action::Press).is_err() {
                                break;
                            }
                        }
                        Ok(_) => {}
                        Err(e) => tracing::warn!("button read: {e}"),
                    },
                    Err(e) => {
                        tracing::error!("button wait failed, button disabled: {e}");
                        break;
                    }
                }
            }
        })?;
        Ok(Self { running, thread: Some(thread) })
    }
}

impl Drop for GpioButton {
    fn drop(&mut self) {
        self.running.store(false, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::keyboard::SmolStr;

    fn ch(s: &str) -> Key {
        Key::Character(SmolStr::new(s))
    }

    #[test]
    fn maps_maintenance_keys() {
        assert_eq!(map_key(&Key::Named(NamedKey::Escape), false), Some(Action::Quit));
        assert_eq!(map_key(&Key::Named(NamedKey::Space), false), Some(Action::Press));
        assert_eq!(map_key(&ch(" "), false), Some(Action::Press));
        assert_eq!(map_key(&ch("q"), false), Some(Action::Quit));
        assert_eq!(map_key(&ch("d"), false), Some(Action::ToggleDebug));
        assert_eq!(map_key(&ch("r"), false), Some(Action::ToggleMirror));
        assert_eq!(map_key(&ch("f"), false), Some(Action::ToggleFilter));
        assert_eq!(map_key(&ch("x"), false), None);
    }

    #[test]
    fn uppercase_maps_like_lowercase() {
        assert_eq!(map_key(&ch("Q"), false), Some(Action::Quit));
        assert_eq!(map_key(&ch("R"), false), Some(Action::ToggleMirror));
    }

    #[test]
    fn auto_repeat_is_ignored() {
        assert_eq!(map_key(&ch("r"), true), None);
        assert_eq!(map_key(&Key::Named(NamedKey::Space), true), None);
    }

    #[test]
    fn pi_line_names() {
        assert_eq!(line_name(27), "GPIO27");
    }
}
