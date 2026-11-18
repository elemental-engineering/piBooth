# piBooth v2 — Plan 2: Booth Loop (Milestone 2)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn the milestone 1 preview into a working booth loop: GPIO button and keyboard input, the Idle → Countdown → Capture → Review → Live state machine, a screen flash, the on-screen display (title, countdown digits, status icons, debug overlay, error banner), background health monitors, and automatic camera restart. The "photo" is the frozen preview frame (not saved); milestone 3 adds real capture.

**Architecture:** A pure `state::Booth` machine takes events and the current time and returns commands. A `display::Controller` (no GL) owns the machine, the camera (re-created through a factory after a failure, with exponential backoff), the monitors and the button channel, and produces a `SceneInput` each frame. The OSD is rendered on the CPU once into RGBA images (`osd::text`, `osd::assets`), uploaded as GPU sprites by `display::Hud`, laid out by the pure `osd::scene::compose`, and alpha-blended by a second GLES program in the renderer. Monitors and the button run on plain threads and never block a frame.

**Tech Stack:** Rust 2024 (rustc ≥ 1.85), existing winit 0.30 / glutin 0.32 / glow 0.18 / libcamera 0.8; new: gpiocdev 0.8 (GPIO character device), fontdue 0.9 (font rasterisation), image 0.25 (PNG decode + resize, `png` feature only), nix 0.30 (`statvfs`).

**Spec:** `docs/superpowers/specs/2026-11-18-rust-v2-design.md` (§4.1 state machine, §4.3 OSD, §4.5 screen flash, §4.7 monitors, §5 camera restart; this plan covers §8 milestone 2). Builds on Plan 1 (`docs/superpowers/plans/2026-11-18-v2-plan-1-preview.md`), already on branch `v2-rust`.

## Global Constraints

- Crate `pibooth` in `v2/`; v1 (`main.py`, `fonts/`, `images/`, `effects/`) is read-only and is the asset source.
- Target: Raspberry Pi 5, IMX296 colour 1456×1088 @ 60 fps, 1920×1080 HDMI, `cage` kiosk. Preview must stay ≥ 58 fps with the OSD on (milestone 1 measured 60.1).
- No frame may block on I/O: monitors, network checks and the button run on their own threads; the render thread only reads snapshots and channels.
- State machine timings (spec §4.1): countdown `timing.countdown_s` (default 5, digits 5…1), review `timing.review_s` (5), idle `timing.idle_s` (120); screen flash on `flash.lead_ms` (300) before the shot, capped at `flash.max_ms` (800); capture timeout 5 s.
- Presses during Countdown, Capturing and Review are ignored; one physical press = one event; key auto-repeat is ignored; `Q` works like `q`.
- OSD look follows v1: title "Photo Shoot" 250 px and subtitle "Press to start" 120 px in `fonts/Carnevalee Freakshow.ttf`, colour (255,252,226) with a black outline; countdown digits 1000 px; debug text `fonts/arial.ttf` 20 px red, right-aligned 2 % from the right edge on baselines 70 + 30·i; icons 3 % of screen height at x = 1 % width + i·4 % height, order hdd_warning, hdd_critical, network, temp_warning, temp_critical. All sizes are for a 1080-line screen and scale with screen height.
- Monitor thresholds (v1): warning strictly above `*_warn`, critical at or above `*_crit`; temp every 1 s from `/sys/class/thermal/thermal_zone0/temp`; disk every 5 min via statvfs on `storage.root`; Immich reachability every 60 s (10 s while offline), 5 s timeout, only when `immich.base_url` is set.
- Button default matches v1 wiring: active-high on GPIO 27 with internal pull-down (`button.active_low = false`); `true` selects active-low with pull-up. Lines are looked up by name (`GPIO27`), not by chip number.
- Camera failure after startup: error banner, state → Idle, restart with backoff 1 s doubling to 30 s; failure at startup still exits non-zero (spec §5).
- `flash.mode = "strobe"` logs a warning and runs without flash until milestone 5.
- `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and `cargo test` pass on x86 without the `libcamera` feature; `cargo clippy --all-targets --features libcamera -- -D warnings` passes where libcamera ≥ 0.4 headers exist.
- Commit messages end with the project's attribution trailer lines.

## File Structure

| File | Responsibility |
|---|---|
| `v2/src/state.rs` (new) | Pure booth state machine: `Booth`, `Phase`, `Event`, `Command`, `View`, `Timings` |
| `v2/src/input.rs` (new) | `Action`, `map_key`, `line_name`, `GpioButton` (gpiocdev edge events) |
| `v2/src/monitor.rs` (new) | `Level`, `Network`, `Status`, `classify`, `parse_millidegrees`, `disk_used_pct`, `server_addr`, `reachable`, `Monitors` |
| `v2/src/osd/mod.rs` (new) | `Rgba`, `SpriteId`, `Overlay`, `WHITE` |
| `v2/src/osd/text.rs` (new) | `TextStyle`, `TextImage`, `load_font`, `render_text` (outline by dilation) |
| `v2/src/osd/assets.rs` (new) | `Icon`, `Assets::load`, `load_icon` |
| `v2/src/osd/scene.rs` (new) | `TextSprite`, `Sprites`, `SceneInput`, `Metrics`, `DebugInfo`, `debug_text`, `visible_icons`, `compose` |
| `v2/src/camera/mod.rs` | add `Backoff` |
| `v2/src/display/controller.rs` (new) | `Controller`, `CameraFactory`, `timings_from` |
| `v2/src/display/hud.rs` (new) | `Hud`: OSD images → renderer sprites, debug text refresh, resize |
| `v2/src/display/renderer.rs` | add overlay program, sprites, `draw(&[Overlay])` |
| `v2/src/display/app.rs` | drive controller + HUD; keys via `map_key` |
| `v2/src/display/mod.rs` | module list and re-exports |
| `v2/src/main.rs` | button setup, camera factory, controller |
| `v2/src/lib.rs` | module list |
| `v2/src/config.rs`, `v2/config.example.toml`, `v2/config.dev.toml` (new) | `display.assets_dir`; button default active-high |
| `v2/Cargo.toml` | new dependencies |
| `v2/README.md`, spec | docs |

All commands run from `v2/` unless stated. Tests that read v1 assets use `CARGO_MANIFEST_DIR/..` (the repo root).

---
### Task 1: Booth state machine

**Files:**
- Create: `v2/src/state.rs`
- Modify: `v2/src/lib.rs` (add `pub mod state;`, keeping modules in alphabetical order)
- Test: inline in `v2/src/state.rs`

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `pub struct Timings { pub countdown: Duration, pub review: Duration, pub idle: Duration, pub screen_flash_lead: Option<Duration>, pub flash_max: Duration, pub capture_timeout: Duration }` (`Debug, Clone, Copy, PartialEq`)
  - `pub enum Phase { Idle, Live, Countdown { deadline: Instant }, Capturing { since: Instant }, Review { until: Instant } }`
  - `pub enum Event { Press, CaptureDone, CaptureFailed, CameraLost }`, `pub enum Command { FlashOn, FlashOff, Capture, FreezePreview, ResumePreview }`
  - `pub struct View { pub title: bool, pub countdown: Option<u32>, pub flash: bool }`
  - `Booth::new(Timings, now: Instant)`, `phase(&self) -> Phase`, `handle(&mut self, Event, now) -> Vec<Command>`, `tick(&mut self, now) -> Vec<Command>`, `view(&self, now) -> View`

- [ ] **Step 1: Write the failing tests**

Create `v2/src/state.rs` with the Interfaces types and `Booth` methods stubbed `todo!()` (keep the private fields shown in Step 3), plus this test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const S: Duration = Duration::from_secs(1);
    const MS: Duration = Duration::from_millis(1);

    fn timings() -> Timings {
        Timings {
            countdown: 5 * S,
            review: 5 * S,
            idle: 120 * S,
            screen_flash_lead: Some(300 * MS),
            flash_max: 800 * MS,
            capture_timeout: 5 * S,
        }
    }

    #[test]
    fn starts_idle_with_title() {
        let t0 = Instant::now();
        let b = Booth::new(timings(), t0);
        assert_eq!(b.phase(), Phase::Idle);
        assert_eq!(b.view(t0), View { title: true, countdown: None, flash: false });
    }

    #[test]
    fn press_starts_countdown_showing_5_to_1() {
        let t0 = Instant::now();
        let mut b = Booth::new(timings(), t0);
        assert!(b.handle(Event::Press, t0).is_empty());
        assert_eq!(b.phase(), Phase::Countdown { deadline: t0 + 5 * S });
        assert_eq!(b.view(t0).countdown, Some(5));
        assert!(!b.view(t0).title);
        assert_eq!(b.view(t0 + 1 * S).countdown, Some(4));
        assert_eq!(b.view(t0 + 4500 * MS).countdown, Some(1));
        assert_eq!(b.view(t0 + 5 * S).countdown, None);
    }

    #[test]
    fn screen_flash_leads_the_capture() {
        let t0 = Instant::now();
        let mut b = Booth::new(timings(), t0);
        b.handle(Event::Press, t0);
        assert!(b.tick(t0 + 4600 * MS).is_empty());
        assert_eq!(b.tick(t0 + 4700 * MS), vec![Command::FlashOn]);
        assert!(b.view(t0 + 4700 * MS).flash);
        assert!(b.tick(t0 + 4800 * MS).is_empty(), "flash turns on once");
        assert_eq!(b.tick(t0 + 5 * S), vec![Command::Capture]);
        assert_eq!(b.phase(), Phase::Capturing { since: t0 + 5 * S });
    }

    #[test]
    fn no_flash_when_disabled() {
        let t0 = Instant::now();
        let mut b = Booth::new(Timings { screen_flash_lead: None, ..timings() }, t0);
        b.handle(Event::Press, t0);
        assert!(b.tick(t0 + 4900 * MS).is_empty());
        assert_eq!(b.tick(t0 + 5 * S), vec![Command::Capture]);
    }

    #[test]
    fn capture_done_goes_to_review_then_live() {
        let t0 = Instant::now();
        let mut b = Booth::new(timings(), t0);
        b.handle(Event::Press, t0);
        b.tick(t0 + 4700 * MS);
        b.tick(t0 + 5 * S);
        let t1 = t0 + 5100 * MS;
        assert_eq!(b.handle(Event::CaptureDone, t1), vec![Command::FlashOff, Command::FreezePreview]);
        assert_eq!(b.phase(), Phase::Review { until: t1 + 5 * S });
        assert!(b.tick(t1 + 4 * S).is_empty());
        assert_eq!(b.tick(t1 + 5 * S), vec![Command::ResumePreview]);
        assert_eq!(b.phase(), Phase::Live);
    }

    #[test]
    fn presses_ignored_outside_idle_and_live() {
        let t0 = Instant::now();
        let mut b = Booth::new(timings(), t0);
        b.handle(Event::Press, t0);
        b.handle(Event::Press, t0 + 1 * S);
        assert_eq!(b.phase(), Phase::Countdown { deadline: t0 + 5 * S }, "countdown not restarted");
        b.tick(t0 + 5 * S);
        assert!(b.handle(Event::Press, t0 + 5 * S).is_empty());
        let t1 = t0 + 5 * S;
        b.handle(Event::CaptureDone, t1);
        b.handle(Event::Press, t1 + 1 * S);
        assert!(matches!(b.phase(), Phase::Review { .. }));
    }

    #[test]
    fn press_in_live_starts_new_countdown() {
        let t0 = Instant::now();
        let mut b = Booth::new(timings(), t0);
        b.handle(Event::Press, t0);
        b.tick(t0 + 5 * S);
        b.handle(Event::CaptureDone, t0 + 5 * S);
        b.tick(t0 + 10 * S);
        assert_eq!(b.phase(), Phase::Live);
        b.handle(Event::Press, t0 + 11 * S);
        assert!(matches!(b.phase(), Phase::Countdown { .. }));
    }

    #[test]
    fn live_returns_to_idle_after_timeout() {
        let t0 = Instant::now();
        let mut b = Booth::new(timings(), t0);
        b.handle(Event::Press, t0);
        b.tick(t0 + 5 * S);
        b.handle(Event::CaptureDone, t0 + 5 * S);
        let live_at = t0 + 10 * S;
        b.tick(live_at);
        b.tick(live_at + 119 * S);
        assert_eq!(b.phase(), Phase::Live);
        b.tick(live_at + 120 * S);
        assert_eq!(b.phase(), Phase::Idle);
        assert!(b.view(live_at + 120 * S).title);
    }

    #[test]
    fn flash_capped_and_capture_timeout_returns_go_live() {
        let t0 = Instant::now();
        let mut b = Booth::new(timings(), t0);
        b.handle(Event::Press, t0);
        b.tick(t0 + 4700 * MS); // flash on at 4.7 s
        b.tick(t0 + 5 * S); // capture requested
        assert_eq!(b.tick(t0 + 5500 * MS), vec![Command::FlashOff], "flash_max = 800 ms");
        assert!(b.tick(t0 + 9 * S).is_empty());
        assert!(b.tick(t0 + 10 * S).is_empty(), "already off, no duplicate");
        assert_eq!(b.phase(), Phase::Live, "capture timed out after 5 s");
    }

    #[test]
    fn capture_failed_turns_flash_off_and_goes_live() {
        let t0 = Instant::now();
        let mut b = Booth::new(timings(), t0);
        b.handle(Event::Press, t0);
        b.tick(t0 + 4700 * MS);
        b.tick(t0 + 5 * S);
        assert_eq!(b.handle(Event::CaptureFailed, t0 + 5 * S), vec![Command::FlashOff]);
        assert_eq!(b.phase(), Phase::Live);
    }

    #[test]
    fn camera_lost_resets_to_idle_from_any_phase() {
        let t0 = Instant::now();
        let mut b = Booth::new(timings(), t0);
        b.handle(Event::Press, t0);
        b.tick(t0 + 4700 * MS);
        assert_eq!(b.handle(Event::CameraLost, t0 + 4800 * MS), vec![Command::FlashOff]);
        assert_eq!(b.phase(), Phase::Idle);

        b.handle(Event::Press, t0 + 10 * S);
        b.tick(t0 + 15 * S);
        b.handle(Event::CaptureDone, t0 + 15 * S);
        assert_eq!(b.handle(Event::CameraLost, t0 + 16 * S), vec![Command::ResumePreview]);
        assert_eq!(b.phase(), Phase::Idle);
    }
}
```

Add `pub mod state;` to `v2/src/lib.rs`.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test state::`
Expected: FAIL — `not yet implemented`.

- [ ] **Step 3: Write the implementation**

`v2/src/state.rs`:
```rust
//! The booth's behaviour as a pure state machine (spec §4.1). Time is passed
//! in by the caller, so every transition is deterministic and testable.
//! The machine only decides; the app carries out the returned [`Command`]s.

use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Timings {
    pub countdown: Duration,
    pub review: Duration,
    pub idle: Duration,
    /// `Some(lead)`: screen flash turns on `lead` before the shot.
    pub screen_flash_lead: Option<Duration>,
    /// Longest the screen flash may stay on.
    pub flash_max: Duration,
    /// Give up on a capture that never completes.
    pub capture_timeout: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Phase {
    Idle,
    Live,
    Countdown { deadline: Instant },
    Capturing { since: Instant },
    Review { until: Instant },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// Button or space bar pressed (one edge = one event).
    Press,
    CaptureDone,
    CaptureFailed,
    /// The camera stopped; the app is restarting it.
    CameraLost,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    FlashOn,
    FlashOff,
    /// Take the photo now.
    Capture,
    /// Stop showing new frames (review shows the captured image).
    FreezePreview,
    ResumePreview,
}

/// What the screen should show, derived from the state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct View {
    pub title: bool,
    pub countdown: Option<u32>,
    pub flash: bool,
}

pub struct Booth {
    timings: Timings,
    phase: Phase,
    flash_on: bool,
    flash_since: Option<Instant>,
    last_activity: Instant,
}

impl Booth {
    pub fn new(timings: Timings, now: Instant) -> Self {
        Self { timings, phase: Phase::Idle, flash_on: false, flash_since: None, last_activity: now }
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    pub fn handle(&mut self, event: Event, now: Instant) -> Vec<Command> {
        let mut out = Vec::new();
        match (self.phase, event) {
            (Phase::Idle | Phase::Live, Event::Press) => {
                self.phase = Phase::Countdown { deadline: now + self.timings.countdown };
                self.last_activity = now;
            }
            (Phase::Capturing { .. }, Event::CaptureDone) => {
                self.flash_off(&mut out);
                out.push(Command::FreezePreview);
                self.phase = Phase::Review { until: now + self.timings.review };
            }
            (Phase::Capturing { .. }, Event::CaptureFailed) => {
                self.flash_off(&mut out);
                self.go_live(now);
            }
            (_, Event::CameraLost) => {
                self.flash_off(&mut out);
                if matches!(self.phase, Phase::Review { .. }) {
                    out.push(Command::ResumePreview);
                }
                self.phase = Phase::Idle;
                self.last_activity = now;
            }
            // Presses during countdown/capture/review, and stray capture
            // results, are ignored.
            _ => {}
        }
        out
    }

    /// Advance time-driven transitions. Call every frame.
    pub fn tick(&mut self, now: Instant) -> Vec<Command> {
        let mut out = Vec::new();
        match self.phase {
            Phase::Idle => {}
            Phase::Live => {
                if now.duration_since(self.last_activity) >= self.timings.idle {
                    self.phase = Phase::Idle;
                }
            }
            Phase::Countdown { deadline } => {
                if let Some(lead) = self.timings.screen_flash_lead
                    && !self.flash_on
                    && now + lead >= deadline
                {
                    self.flash_on = true;
                    self.flash_since = Some(now);
                    out.push(Command::FlashOn);
                }
                if now >= deadline {
                    self.phase = Phase::Capturing { since: now };
                    out.push(Command::Capture);
                }
            }
            Phase::Capturing { since } => {
                if self.flash_on && self.flash_since.is_some_and(|t| now.duration_since(t) >= self.timings.flash_max) {
                    self.flash_off(&mut out);
                }
                if now.duration_since(since) >= self.timings.capture_timeout {
                    self.flash_off(&mut out);
                    self.go_live(now);
                }
            }
            Phase::Review { until } => {
                if now >= until {
                    out.push(Command::ResumePreview);
                    self.go_live(now);
                }
            }
        }
        out
    }

    pub fn view(&self, now: Instant) -> View {
        let countdown = match self.phase {
            Phase::Countdown { deadline } if deadline > now => {
                let ms = deadline.duration_since(now).as_millis() as u64;
                Some(ms.div_ceil(1000) as u32)
            }
            _ => None,
        };
        View { title: self.phase == Phase::Idle, countdown, flash: self.flash_on }
    }

    fn flash_off(&mut self, out: &mut Vec<Command>) {
        if self.flash_on {
            self.flash_on = false;
            self.flash_since = None;
            out.push(Command::FlashOff);
        }
    }

    fn go_live(&mut self, now: Instant) {
        self.phase = Phase::Live;
        self.last_activity = now;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: Duration = Duration::from_secs(1);
    const MS: Duration = Duration::from_millis(1);

    fn timings() -> Timings {
        Timings {
            countdown: 5 * S,
            review: 5 * S,
            idle: 120 * S,
            screen_flash_lead: Some(300 * MS),
            flash_max: 800 * MS,
            capture_timeout: 5 * S,
        }
    }

    #[test]
    fn starts_idle_with_title() {
        let t0 = Instant::now();
        let b = Booth::new(timings(), t0);
        assert_eq!(b.phase(), Phase::Idle);
        assert_eq!(b.view(t0), View { title: true, countdown: None, flash: false });
    }

    #[test]
    fn press_starts_countdown_showing_5_to_1() {
        let t0 = Instant::now();
        let mut b = Booth::new(timings(), t0);
        assert!(b.handle(Event::Press, t0).is_empty());
        assert_eq!(b.phase(), Phase::Countdown { deadline: t0 + 5 * S });
        assert_eq!(b.view(t0).countdown, Some(5));
        assert!(!b.view(t0).title);
        assert_eq!(b.view(t0 + 1 * S).countdown, Some(4));
        assert_eq!(b.view(t0 + 4500 * MS).countdown, Some(1));
        assert_eq!(b.view(t0 + 5 * S).countdown, None);
    }

    #[test]
    fn screen_flash_leads_the_capture() {
        let t0 = Instant::now();
        let mut b = Booth::new(timings(), t0);
        b.handle(Event::Press, t0);
        assert!(b.tick(t0 + 4600 * MS).is_empty());
        assert_eq!(b.tick(t0 + 4700 * MS), vec![Command::FlashOn]);
        assert!(b.view(t0 + 4700 * MS).flash);
        assert!(b.tick(t0 + 4800 * MS).is_empty(), "flash turns on once");
        assert_eq!(b.tick(t0 + 5 * S), vec![Command::Capture]);
        assert_eq!(b.phase(), Phase::Capturing { since: t0 + 5 * S });
    }

    #[test]
    fn no_flash_when_disabled() {
        let t0 = Instant::now();
        let mut b = Booth::new(Timings { screen_flash_lead: None, ..timings() }, t0);
        b.handle(Event::Press, t0);
        assert!(b.tick(t0 + 4900 * MS).is_empty());
        assert_eq!(b.tick(t0 + 5 * S), vec![Command::Capture]);
    }

    #[test]
    fn capture_done_goes_to_review_then_live() {
        let t0 = Instant::now();
        let mut b = Booth::new(timings(), t0);
        b.handle(Event::Press, t0);
        b.tick(t0 + 4700 * MS);
        b.tick(t0 + 5 * S);
        let t1 = t0 + 5100 * MS;
        assert_eq!(b.handle(Event::CaptureDone, t1), vec![Command::FlashOff, Command::FreezePreview]);
        assert_eq!(b.phase(), Phase::Review { until: t1 + 5 * S });
        assert!(b.tick(t1 + 4 * S).is_empty());
        assert_eq!(b.tick(t1 + 5 * S), vec![Command::ResumePreview]);
        assert_eq!(b.phase(), Phase::Live);
    }

    #[test]
    fn presses_ignored_outside_idle_and_live() {
        let t0 = Instant::now();
        let mut b = Booth::new(timings(), t0);
        b.handle(Event::Press, t0);
        b.handle(Event::Press, t0 + 1 * S);
        assert_eq!(b.phase(), Phase::Countdown { deadline: t0 + 5 * S }, "countdown not restarted");
        b.tick(t0 + 5 * S);
        assert!(b.handle(Event::Press, t0 + 5 * S).is_empty());
        let t1 = t0 + 5 * S;
        b.handle(Event::CaptureDone, t1);
        b.handle(Event::Press, t1 + 1 * S);
        assert!(matches!(b.phase(), Phase::Review { .. }));
    }

    #[test]
    fn press_in_live_starts_new_countdown() {
        let t0 = Instant::now();
        let mut b = Booth::new(timings(), t0);
        b.handle(Event::Press, t0);
        b.tick(t0 + 5 * S);
        b.handle(Event::CaptureDone, t0 + 5 * S);
        b.tick(t0 + 10 * S);
        assert_eq!(b.phase(), Phase::Live);
        b.handle(Event::Press, t0 + 11 * S);
        assert!(matches!(b.phase(), Phase::Countdown { .. }));
    }

    #[test]
    fn live_returns_to_idle_after_timeout() {
        let t0 = Instant::now();
        let mut b = Booth::new(timings(), t0);
        b.handle(Event::Press, t0);
        b.tick(t0 + 5 * S);
        b.handle(Event::CaptureDone, t0 + 5 * S);
        let live_at = t0 + 10 * S;
        b.tick(live_at);
        b.tick(live_at + 119 * S);
        assert_eq!(b.phase(), Phase::Live);
        b.tick(live_at + 120 * S);
        assert_eq!(b.phase(), Phase::Idle);
        assert!(b.view(live_at + 120 * S).title);
    }

    #[test]
    fn flash_capped_and_capture_timeout_returns_go_live() {
        let t0 = Instant::now();
        let mut b = Booth::new(timings(), t0);
        b.handle(Event::Press, t0);
        b.tick(t0 + 4700 * MS); // flash on at 4.7 s
        b.tick(t0 + 5 * S); // capture requested
        assert_eq!(b.tick(t0 + 5500 * MS), vec![Command::FlashOff], "flash_max = 800 ms");
        assert!(b.tick(t0 + 9 * S).is_empty());
        assert!(b.tick(t0 + 10 * S).is_empty(), "already off, no duplicate");
        assert_eq!(b.phase(), Phase::Live, "capture timed out after 5 s");
    }

    #[test]
    fn capture_failed_turns_flash_off_and_goes_live() {
        let t0 = Instant::now();
        let mut b = Booth::new(timings(), t0);
        b.handle(Event::Press, t0);
        b.tick(t0 + 4700 * MS);
        b.tick(t0 + 5 * S);
        assert_eq!(b.handle(Event::CaptureFailed, t0 + 5 * S), vec![Command::FlashOff]);
        assert_eq!(b.phase(), Phase::Live);
    }

    #[test]
    fn camera_lost_resets_to_idle_from_any_phase() {
        let t0 = Instant::now();
        let mut b = Booth::new(timings(), t0);
        b.handle(Event::Press, t0);
        b.tick(t0 + 4700 * MS);
        assert_eq!(b.handle(Event::CameraLost, t0 + 4800 * MS), vec![Command::FlashOff]);
        assert_eq!(b.phase(), Phase::Idle);

        b.handle(Event::Press, t0 + 10 * S);
        b.tick(t0 + 15 * S);
        b.handle(Event::CaptureDone, t0 + 15 * S);
        assert_eq!(b.handle(Event::CameraLost, t0 + 16 * S), vec![Command::ResumePreview]);
        assert_eq!(b.phase(), Phase::Idle);
    }
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test state:: && cargo clippy --all-targets -- -D warnings && cargo fmt --check`
Expected: `11 passed`, clean.

- [ ] **Step 5: Commit**

```bash
git add src/state.rs src/lib.rs
git commit -m "feat(v2): pure booth state machine"
```

### Task 2: Input (keys + GPIO button) and config changes

**Files:**
- Create: `v2/src/input.rs`, `v2/config.dev.toml`
- Modify: `v2/Cargo.toml`, `v2/src/lib.rs`, `v2/src/config.rs`, `v2/config.example.toml`
- Test: inline in `v2/src/input.rs` and `v2/src/config.rs`

**Interfaces:**
- Consumes: `config::Button { gpio_line, active_low, debounce_ms }`.
- Produces:
  - `pub enum Action { Quit, Press, ToggleDebug, ToggleMirror, ToggleFilter }` (`Debug, Clone, Copy, PartialEq, Eq`)
  - `pub fn map_key(key: &winit::keyboard::Key, repeat: bool) -> Option<Action>`
  - `pub fn line_name(bcm: u32) -> String`
  - `pub struct GpioButton` with `start(cfg: &Button, tx: std::sync::mpsc::Sender<Action>) -> anyhow::Result<Self>`; dropping it stops and joins its thread.
  - `config::Display` gains `pub assets_dir: PathBuf` (default `"."`); `Button::default().active_low` becomes `false`.

- [ ] **Step 1: Dependencies and config**

In `v2/Cargo.toml` `[dependencies]`, after `glutin-winit = "0.5"`, add:
```toml
gpiocdev = "0.8"
```

In `v2/src/config.rs`, replace the `Display` struct and its `Default` impl with:
```rust
pub struct Display {
    pub mirror_preview: bool,
    /// Folder holding v1's `fonts/` and `images/`.
    pub assets_dir: PathBuf,
}
impl Default for Display {
    fn default() -> Self {
        Self { mirror_preview: true, assets_dir: PathBuf::from(".") }
    }
}
```

and in `impl Default for Button` change `active_low: true` to `active_low: false`.

In `v2/config.example.toml`, under `[display]` after `mirror_preview`, add:
```toml
assets_dir = "."      # folder containing fonts/ and images/ (".." when running from v2/)
```

and under `[button]` replace `active_low = true` with:
```toml
active_low = false    # false: button to 3.3 V, internal pull-down (v1 wiring)
```

Create `v2/config.dev.toml`:
```toml
# Development settings. From v2/:
#   cargo run -- --config config.dev.toml --fake-camera --windowed --no-gpio
[display]
assets_dir = ".."   # v1 fonts/ and images/ live at the repo root
```

- [ ] **Step 2: Write the failing tests**

Add these tests to the `tests` module of `v2/src/config.rs` (before `unknown_key_is_rejected`):
```rust
    #[test]
    fn dev_config_points_assets_at_repo_root() {
        let cfg = Config::from_toml_str(include_str!("../config.dev.toml")).unwrap();
        assert_eq!(cfg.display.assets_dir, PathBuf::from(".."));
    }

    #[test]
    fn button_defaults_match_v1_wiring() {
        let b = Config::default().button;
        assert_eq!((b.gpio_line, b.active_low, b.debounce_ms), (27, false, 30));
    }
```

Create `v2/src/input.rs` with the Interfaces items stubbed `todo!()` and this test module:
```rust
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
```

Add `pub mod input;` to `v2/src/lib.rs`.

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test input::` then `cargo test config::`
Expected: input tests FAIL with `not yet implemented`. The two new config tests already pass because Step 1 changed the config (`button_defaults_match_v1_wiring` guards that change).

- [ ] **Step 4: Write the implementation**

`v2/src/input.rs`:
```rust
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
```

- [ ] **Step 5: Run tests**

Run: `cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check`
Expected: all pass (4 input + 2 new config tests).

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock src/input.rs src/lib.rs src/config.rs config.example.toml config.dev.toml
git commit -m "feat(v2): keyboard mapping, GPIO button and assets_dir config"
```

### Task 3: Health monitors

**Files:**
- Create: `v2/src/monitor.rs`
- Modify: `v2/Cargo.toml`, `v2/src/lib.rs`
- Test: inline in `v2/src/monitor.rs`

**Interfaces:**
- Consumes: `Config` (`monitor`, `storage.root`, `immich.base_url`).
- Produces:
  - `pub enum Level { Ok, Warning, Critical }`, `pub enum Network { Disabled, Unknown, Online, Offline }` (both `Default`: `Ok` / `Disabled`)
  - `pub struct Status { pub temp_c: Option<f32>, pub temp: Level, pub disk_pct: Option<f32>, pub disk: Level, pub network: Network }` (`Copy, Default`)
  - `classify(value, warn, crit) -> Level`, `parse_millidegrees(&str) -> Option<f32>`, `disk_used_pct(&Path) -> io::Result<f32>`, `server_addr(&str) -> Option<(String, u16)>`, `reachable(host, port, timeout) -> bool`
  - `pub struct Monitors` with `start(&Config) -> anyhow::Result<Self>` and `snapshot(&self) -> Status`; drop stops it (the network thread is detached so a pending connect never delays quitting).

- [ ] **Step 1: Dependency**

In `v2/Cargo.toml` `[dependencies]`, after `libcamera = ...`, add:
```toml
nix = { version = "0.30", features = ["fs"] }
```

- [ ] **Step 2: Write the failing tests**

Create `v2/src/monitor.rs` with the Interfaces items stubbed `todo!()` and this test module:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_matches_v1_thresholds() {
        assert_eq!(classify(55.0, 60.0, 65.0), Level::Ok);
        assert_eq!(classify(60.0, 60.0, 65.0), Level::Ok, "warning is strictly above");
        assert_eq!(classify(60.1, 60.0, 65.0), Level::Warning);
        assert_eq!(classify(65.0, 60.0, 65.0), Level::Critical);
        assert_eq!(classify(91.0, 70.0, 90.0), Level::Critical);
    }

    #[test]
    fn parses_thermal_zone() {
        assert_eq!(parse_millidegrees("48312\n"), Some(48.312));
        assert_eq!(parse_millidegrees("garbage"), None);
    }

    #[test]
    fn disk_usage_of_current_dir_is_a_percentage() {
        let p = disk_used_pct(Path::new(".")).unwrap();
        assert!((0.0..=100.0).contains(&p), "{p}");
        assert!(disk_used_pct(Path::new("/nonexistent/dir")).is_err());
    }

    #[test]
    fn server_addr_from_urls() {
        assert_eq!(server_addr("https://immich.example/api"), Some(("immich.example".into(), 443)));
        assert_eq!(server_addr("http://192.168.1.10:2283/api"), Some(("192.168.1.10".into(), 2283)));
        assert_eq!(server_addr("HTTPS://user:pw@host.lan"), Some(("host.lan".into(), 443)));
        assert_eq!(server_addr("https://[fd00::1]:8443/api"), Some(("fd00::1".into(), 8443)));
        assert_eq!(server_addr("https://[fd00::1]/api"), Some(("fd00::1".into(), 443)));
        assert_eq!(server_addr(""), None);
        assert_eq!(server_addr("immich.example/api"), None);
        assert_eq!(server_addr("ftp://x"), None);
        assert_eq!(server_addr("https://host:notaport"), None);
    }

    #[test]
    fn reachability_against_local_listener() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(reachable("127.0.0.1", port, Duration::from_secs(1)));
        drop(listener);
        assert!(!reachable("127.0.0.1", port, Duration::from_millis(500)));
    }

    #[test]
    fn monitors_publish_disk_and_disabled_network() {
        let cfg = Config::default();
        let m = Monitors::start(&cfg).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while m.snapshot().disk_pct.is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        let s = m.snapshot();
        assert!(s.disk_pct.is_some());
        assert_eq!(s.network, Network::Disabled);
    }
}
```

Add `pub mod monitor;` to `v2/src/lib.rs`.

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test monitor::`
Expected: FAIL — `not yet implemented`.

- [ ] **Step 4: Write the implementation**

`v2/src/monitor.rs`:
```rust
//! Background health checks (spec §4.7): CPU temperature, disk usage and
//! Immich server reachability. They run on their own threads and only
//! publish a [`Status`] snapshot, so a slow check can never stall a frame.

use std::net::{TcpStream, ToSocketAddrs};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::config::Config;

const TEMP_PATH: &str = "/sys/class/thermal/thermal_zone0/temp";
const TEMP_INTERVAL: Duration = Duration::from_secs(1);
const DISK_INTERVAL: Duration = Duration::from_secs(5 * 60);
const NET_INTERVAL_ONLINE: Duration = Duration::from_secs(60);
const NET_INTERVAL_OFFLINE: Duration = Duration::from_secs(10);
const NET_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Level {
    #[default]
    Ok,
    Warning,
    Critical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Network {
    /// No Immich server configured: nothing to check, no icon.
    #[default]
    Disabled,
    Unknown,
    Online,
    Offline,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Status {
    pub temp_c: Option<f32>,
    pub temp: Level,
    pub disk_pct: Option<f32>,
    pub disk: Level,
    pub network: Network,
}

/// v1 thresholds semantics: above `warn` is a warning, `crit` and above is critical.
pub fn classify(value: f32, warn: f32, crit: f32) -> Level {
    if value >= crit {
        Level::Critical
    } else if value > warn {
        Level::Warning
    } else {
        Level::Ok
    }
}

/// Parse the kernel's thermal zone file (millidegrees Celsius).
pub fn parse_millidegrees(text: &str) -> Option<f32> {
    text.trim().parse::<i64>().ok().map(|m| m as f32 / 1000.0)
}

/// Used space as a percentage, computed like v1 (`(blocks - bfree) / blocks`).
pub fn disk_used_pct(path: &Path) -> std::io::Result<f32> {
    let st = nix::sys::statvfs::statvfs(path).map_err(std::io::Error::from)?;
    let blocks = st.blocks() as f64;
    if blocks == 0.0 {
        return Ok(0.0);
    }
    Ok(((blocks - st.blocks_free() as f64) * 100.0 / blocks) as f32)
}

/// Host and port to probe for an Immich base URL such as
/// `https://immich.example/api` → `("immich.example", 443)`.
pub fn server_addr(base_url: &str) -> Option<(String, u16)> {
    let (scheme, rest) = base_url.trim().split_once("://")?;
    let default_port = match scheme.to_ascii_lowercase().as_str() {
        "https" => 443,
        "http" => 80,
        _ => return None,
    };
    let authority = rest.split(['/', '?', '#']).next()?;
    let authority = authority.rsplit('@').next()?; // drop userinfo
    if authority.is_empty() {
        return None;
    }
    if let Some(v6) = authority.strip_prefix('[') {
        let (host, after) = v6.split_once(']')?;
        let port = match after.strip_prefix(':') {
            Some(p) => p.parse().ok()?,
            None if after.is_empty() => default_port,
            None => return None,
        };
        return Some((host.to_string(), port));
    }
    match authority.rsplit_once(':') {
        Some((host, port)) => Some((host.to_string(), port.parse().ok()?)),
        None => Some((authority.to_string(), default_port)),
    }
}

/// TCP reachability of `host:port` within `timeout`.
pub fn reachable(host: &str, port: u16, timeout: Duration) -> bool {
    let Ok(addrs) = (host, port).to_socket_addrs() else { return false };
    addrs.into_iter().any(|a| TcpStream::connect_timeout(&a, timeout).is_ok())
}

pub struct Monitors {
    status: Arc<Mutex<Status>>,
    running: Arc<AtomicBool>,
    /// The local-checks thread; joined on drop (it wakes every 100 ms).
    local: Option<JoinHandle<()>>,
}

impl Monitors {
    pub fn start(cfg: &Config) -> anyhow::Result<Self> {
        let status = Arc::new(Mutex::new(Status::default()));
        let running = Arc::new(AtomicBool::new(true));
        let (st, run, mon, root) = (status.clone(), running.clone(), cfg.monitor.clone(), cfg.storage.root.clone());
        let local = std::thread::Builder::new().name("monitor".into()).spawn(move || {
            local_loop(&st, &run, &mon, &root);
        })?;

        if let Some((host, port)) = server_addr(&cfg.immich.base_url) {
            status.lock().unwrap().network = Network::Unknown;
            let (st, run) = (status.clone(), running.clone());
            // Detached: a connect can block for NET_TIMEOUT, which must not delay quitting.
            std::thread::Builder::new().name("net-monitor".into()).spawn(move || {
                net_loop(&st, &run, &host, port);
            })?;
        } else if !cfg.immich.base_url.is_empty() {
            tracing::warn!(url = %cfg.immich.base_url, "cannot parse immich.base_url; network check disabled");
        }
        Ok(Self { status, running, local: Some(local) })
    }

    pub fn snapshot(&self) -> Status {
        *self.status.lock().unwrap()
    }
}

impl Drop for Monitors {
    fn drop(&mut self) {
        self.running.store(false, Ordering::SeqCst);
        if let Some(t) = self.local.take() {
            let _ = t.join();
        }
    }
}

/// Sleep up to `d`, waking early when `running` clears. Returns false to stop.
fn nap(running: &AtomicBool, d: Duration) -> bool {
    let end = Instant::now() + d;
    while running.load(Ordering::SeqCst) {
        let now = Instant::now();
        if now >= end {
            return true;
        }
        std::thread::sleep((end - now).min(Duration::from_millis(100)));
    }
    false
}

fn local_loop(status: &Mutex<Status>, running: &AtomicBool, mon: &crate::config::Monitor, root: &Path) {
    let mut next_disk = Instant::now();
    loop {
        let temp_c = std::fs::read_to_string(TEMP_PATH).ok().and_then(|t| parse_millidegrees(&t));
        let disk = if Instant::now() >= next_disk {
            next_disk = Instant::now() + DISK_INTERVAL;
            match disk_used_pct(root) {
                Ok(p) => Some(p),
                Err(e) => {
                    tracing::warn!("disk usage of {}: {e}", root.display());
                    None
                }
            }
        } else {
            None
        };
        {
            let mut s = status.lock().unwrap();
            s.temp_c = temp_c;
            s.temp = temp_c.map_or(Level::Ok, |t| classify(t, mon.temp_warn_c, mon.temp_crit_c));
            if let Some(p) = disk {
                s.disk_pct = Some(p);
                s.disk = classify(p, mon.disk_warn_pct, mon.disk_crit_pct);
            }
        }
        if !nap(running, TEMP_INTERVAL) {
            return;
        }
    }
}

fn net_loop(status: &Mutex<Status>, running: &AtomicBool, host: &str, port: u16) {
    loop {
        let online = reachable(host, port, NET_TIMEOUT);
        let previous = std::mem::replace(
            &mut status.lock().unwrap().network,
            if online { Network::Online } else { Network::Offline },
        );
        if previous != Network::Online && online {
            tracing::info!(host, port, "Immich server reachable");
        } else if previous != Network::Offline && !online {
            tracing::warn!(host, port, "Immich server unreachable");
        }
        if !nap(running, if online { NET_INTERVAL_ONLINE } else { NET_INTERVAL_OFFLINE }) {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_matches_v1_thresholds() {
        assert_eq!(classify(55.0, 60.0, 65.0), Level::Ok);
        assert_eq!(classify(60.0, 60.0, 65.0), Level::Ok, "warning is strictly above");
        assert_eq!(classify(60.1, 60.0, 65.0), Level::Warning);
        assert_eq!(classify(65.0, 60.0, 65.0), Level::Critical);
        assert_eq!(classify(91.0, 70.0, 90.0), Level::Critical);
    }

    #[test]
    fn parses_thermal_zone() {
        assert_eq!(parse_millidegrees("48312\n"), Some(48.312));
        assert_eq!(parse_millidegrees("garbage"), None);
    }

    #[test]
    fn disk_usage_of_current_dir_is_a_percentage() {
        let p = disk_used_pct(Path::new(".")).unwrap();
        assert!((0.0..=100.0).contains(&p), "{p}");
        assert!(disk_used_pct(Path::new("/nonexistent/dir")).is_err());
    }

    #[test]
    fn server_addr_from_urls() {
        assert_eq!(server_addr("https://immich.example/api"), Some(("immich.example".into(), 443)));
        assert_eq!(server_addr("http://192.168.1.10:2283/api"), Some(("192.168.1.10".into(), 2283)));
        assert_eq!(server_addr("HTTPS://user:pw@host.lan"), Some(("host.lan".into(), 443)));
        assert_eq!(server_addr("https://[fd00::1]:8443/api"), Some(("fd00::1".into(), 8443)));
        assert_eq!(server_addr("https://[fd00::1]/api"), Some(("fd00::1".into(), 443)));
        assert_eq!(server_addr(""), None);
        assert_eq!(server_addr("immich.example/api"), None);
        assert_eq!(server_addr("ftp://x"), None);
        assert_eq!(server_addr("https://host:notaport"), None);
    }

    #[test]
    fn reachability_against_local_listener() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(reachable("127.0.0.1", port, Duration::from_secs(1)));
        drop(listener);
        assert!(!reachable("127.0.0.1", port, Duration::from_millis(500)));
    }

    #[test]
    fn monitors_publish_disk_and_disabled_network() {
        let cfg = Config::default();
        let m = Monitors::start(&cfg).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while m.snapshot().disk_pct.is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        let s = m.snapshot();
        assert!(s.disk_pct.is_some());
        assert_eq!(s.network, Network::Disabled);
    }
}
```

- [ ] **Step 5: Run tests**

Run: `cargo test monitor:: && cargo clippy --all-targets -- -D warnings && cargo fmt --check`
Expected: `6 passed`, clean.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock src/monitor.rs src/lib.rs
git commit -m "feat(v2): temperature, disk and Immich reachability monitors"
```

### Task 4: OSD core and text rendering

**Files:**
- Create: `v2/src/osd/mod.rs`, `v2/src/osd/text.rs`
- Modify: `v2/Cargo.toml`, `v2/src/lib.rs`
- Test: inline in `v2/src/osd/text.rs` (reads `fonts/arial.ttf` from the repo root via `include_bytes!`)

**Interfaces:**
- Consumes: `geometry::Rect`.
- Produces:
  - `pub struct Rgba { pub width: u32, pub height: u32, pub data: Vec<u8> }` with `new(w, h)` (transparent) and `pixel(x, y) -> [u8; 4]`
  - `pub struct SpriteId(pub usize)`, `pub struct Overlay { pub sprite: SpriteId, pub rect: Rect, pub tint: [f32; 4] }`, `pub const WHITE: [f32; 4]`
  - `pub struct TextStyle { pub px: f32, pub fill: [u8; 4], pub outline: Option<([u8; 4], u32)> }`
  - `pub struct TextImage { pub image: Rgba, pub ascender_y: i32, pub baseline_y: i32, pub descender_y: i32 }`
  - `load_font(Vec<u8>) -> anyhow::Result<fontdue::Font>`, `render_text(&Font, &str, TextStyle) -> TextImage`

- [ ] **Step 1: Dependency and module skeleton**

In `v2/Cargo.toml` `[dependencies]`, after `dotenvy = "0.15"`, add:
```toml
fontdue = "0.9"
```

`v2/src/osd/mod.rs` — final content except that `pub mod assets;` and `pub mod scene;` are added in Task 5, so omit those two lines now:
```rust
//! On-screen display: text and icons rasterised on the CPU once (or once a
//! second for debug text), then drawn by the GPU as alpha-blended quads.
//! Nothing here touches GL; [`scene::compose`] outputs [`Overlay`]s that
//! the renderer draws.

pub mod text;

use crate::geometry::Rect;

/// Straight-alpha RGBA8 image, row 0 at the top.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rgba {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

impl Rgba {
    pub fn new(width: u32, height: u32) -> Self {
        Self { width, height, data: vec![0; width as usize * height as usize * 4] }
    }

    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let i = (y as usize * self.width as usize + x as usize) * 4;
        [self.data[i], self.data[i + 1], self.data[i + 2], self.data[i + 3]]
    }
}

/// Handle to a GPU texture owned by the renderer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SpriteId(pub usize);

/// One textured quad to draw over the camera image, in screen pixels.
/// The texture colour is multiplied by `tint` (RGBA, 0..1).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Overlay {
    pub sprite: SpriteId,
    pub rect: Rect,
    pub tint: [f32; 4],
}

pub const WHITE: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
```

Add `pub mod osd;` to `v2/src/lib.rs`.

- [ ] **Step 2: Write the failing tests**

Create `v2/src/osd/text.rs` with `TextStyle`, `TextImage`, `load_font` (implemented — it is one line) and `render_text` stubbed `todo!()`, plus:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn arial() -> Font {
        load_font(include_bytes!("../../../fonts/arial.ttf").to_vec()).unwrap()
    }

    fn style(px: f32, outline: Option<([u8; 4], u32)>) -> TextStyle {
        TextStyle { px, fill: [255, 252, 226, 255], outline }
    }

    fn alpha_bbox(img: &Rgba) -> Option<(u32, u32, u32, u32)> {
        let mut bb: Option<(u32, u32, u32, u32)> = None;
        for y in 0..img.height {
            for x in 0..img.width {
                if img.pixel(x, y)[3] > 0 {
                    bb = Some(match bb {
                        None => (x, y, x, y),
                        Some((a, b, c, d)) => (a.min(x), b.min(y), c.max(x), d.max(y)),
                    });
                }
            }
        }
        bb
    }

    #[test]
    fn same_style_shares_height_and_baseline() {
        let f = arial();
        let a = render_text(&f, "Photo", style(40.0, None));
        let b = render_text(&f, "gy", style(40.0, None));
        assert_eq!(a.image.height, b.image.height);
        assert_eq!(a.baseline_y, b.baseline_y);
        assert!(a.ascender_y < a.baseline_y && a.baseline_y < a.descender_y);
        assert_eq!(a.descender_y, a.image.height as i32);
    }

    #[test]
    fn wider_text_is_wider_and_ink_sits_on_baseline() {
        let f = arial();
        let one = render_text(&f, "1", style(100.0, None));
        let many = render_text(&f, "1111", style(100.0, None));
        assert!(many.image.width > 3 * one.image.width);
        let (_, top, _, bottom) = alpha_bbox(&one.image).unwrap();
        assert!(bottom as i32 <= one.baseline_y, "'1' has no descender");
        assert!((one.baseline_y - top as i32) as f32 > 60.0, "digit height ~0.7 em");
    }

    #[test]
    fn fill_colour_is_used() {
        let f = arial();
        let t = render_text(&f, "I", style(60.0, None));
        let (x0, y0, x1, y1) = alpha_bbox(&t.image).unwrap();
        let p = t.image.pixel((x0 + x1) / 2, (y0 + y1) / 2);
        assert_eq!(p, [255, 252, 226, 255]);
    }

    #[test]
    fn outline_pads_and_draws_dark_edge() {
        let f = arial();
        let plain = render_text(&f, "I", style(60.0, None));
        let out = render_text(&f, "I", style(60.0, Some(([0, 0, 0, 255], 3))));
        assert_eq!(out.image.width, plain.image.width + 6);
        assert_eq!(out.image.height, plain.image.height + 6);
        assert_eq!(out.baseline_y, plain.baseline_y + 3);
        // Just left of the stem: outline (black), fully opaque.
        let (x0, y0, x1, y1) = alpha_bbox(&out.image).unwrap();
        let mid_y = (y0 + y1) / 2;
        let edge = out.image.pixel(x0 + 1, mid_y);
        assert_eq!(edge[3], 255);
        assert!(edge[0] < 40, "outline is dark: {edge:?}");
        let centre = out.image.pixel((x0 + x1) / 2, mid_y);
        assert_eq!(centre, [255, 252, 226, 255]);
    }

    #[test]
    fn empty_text_is_a_valid_image() {
        let t = render_text(&arial(), "", style(20.0, None));
        assert!(t.image.width >= 1 && t.image.height >= 1);
        assert_eq!(t.image.data.len(), (t.image.width * t.image.height * 4) as usize);
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test osd::text`
Expected: FAIL — `not yet implemented`.

- [ ] **Step 4: Write the implementation**

`v2/src/osd/text.rs`:
```rust
//! Text rasterisation with an optional outline (v1 drew titles with a 1 px
//! black stroke so they stay readable over any scene).

use fontdue::Font;

use super::Rgba;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextStyle {
    pub px: f32,
    pub fill: [u8; 4],
    /// Outline colour and radius in pixels.
    pub outline: Option<([u8; 4], u32)>,
}

/// Rendered text plus where its baseline sits inside the image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextImage {
    pub image: Rgba,
    /// Distance from the image top to the ascender line (= outline padding).
    pub ascender_y: i32,
    /// Distance from the image top to the baseline.
    pub baseline_y: i32,
    /// Distance from the image top to the descender line.
    pub descender_y: i32,
}

pub fn load_font(bytes: Vec<u8>) -> anyhow::Result<Font> {
    Font::from_bytes(bytes, fontdue::FontSettings::default()).map_err(|e| anyhow::anyhow!("invalid font: {e}"))
}

/// Rasterise one line of text. The image spans the font's ascender to
/// descender lines (so all strings of one style share a baseline), padded
/// by the outline radius on every side.
pub fn render_text(font: &Font, text: &str, style: TextStyle) -> TextImage {
    let lm = font.horizontal_line_metrics(style.px);
    let ascent = lm.map_or(style.px, |m| m.ascent).ceil() as i32;
    let descent = lm.map_or(0.0, |m| m.descent).floor() as i32; // negative
    let pad = style.outline.map_or(0, |(_, r)| r) as i32;

    // Lay out glyphs on the baseline.
    let mut glyphs = Vec::new();
    let mut pen = 0.0f32;
    let (mut min_x, mut max_x) = (0i32, 0i32);
    for c in text.chars() {
        let (m, bitmap) = font.rasterize(c, style.px);
        let left = (pen + m.xmin as f32).round() as i32;
        let top = -(m.ymin + m.height as i32); // relative to baseline, y down
        min_x = min_x.min(left);
        max_x = max_x.max(left + m.width as i32);
        glyphs.push((left, top, m.width, m.height, bitmap));
        pen += m.advance_width;
    }
    max_x = max_x.max(pen.ceil() as i32);

    let text_w = (max_x - min_x).max(1);
    let text_h = (ascent - descent).max(1);
    let (w, h) = ((text_w + 2 * pad) as u32, (text_h + 2 * pad) as u32);
    let baseline_y = pad + ascent;

    // Coverage mask of the fill.
    let mut mask = vec![0u8; (w * h) as usize];
    for (left, top, gw, gh, bitmap) in &glyphs {
        let ox = left - min_x + pad;
        let oy = baseline_y + top;
        for gy in 0..*gh {
            let y = oy + gy as i32;
            if y < 0 || y >= h as i32 {
                continue;
            }
            for gx in 0..*gw {
                let x = ox + gx as i32;
                if x < 0 || x >= w as i32 {
                    continue;
                }
                let i = (y as u32 * w + x as u32) as usize;
                mask[i] = mask[i].max(bitmap[gy * gw + gx]);
            }
        }
    }

    let outline = style.outline.map(|(_, r)| dilate(&mask, w, h, r));
    let mut image = Rgba::new(w, h);
    for i in 0..(w * h) as usize {
        let fa = mask[i] as f32 / 255.0 * style.fill[3] as f32 / 255.0;
        let (oc, oa) = match (&outline, style.outline) {
            (Some(o), Some((c, _))) => (c, o[i] as f32 / 255.0 * c[3] as f32 / 255.0 * (1.0 - fa)),
            _ => ([0u8; 4], 0.0),
        };
        let a = fa + oa;
        if a <= 0.0 {
            continue;
        }
        let px = &mut image.data[i * 4..i * 4 + 4];
        for ch in 0..3 {
            px[ch] = ((style.fill[ch] as f32 * fa + oc[ch] as f32 * oa) / a).round() as u8;
        }
        px[3] = (a * 255.0).round() as u8;
    }
    TextImage { image, ascender_y: pad, baseline_y, descender_y: pad + ascent - descent }
}

/// Max filter over a disc of radius `r` (outline mask).
fn dilate(mask: &[u8], w: u32, h: u32, r: u32) -> Vec<u8> {
    let (w, h, r) = (w as i32, h as i32, r as i32);
    let offsets: Vec<(i32, i32)> = (-r..=r)
        .flat_map(|dy| (-r..=r).map(move |dx| (dx, dy)))
        .filter(|(dx, dy)| dx * dx + dy * dy <= r * r)
        .collect();
    let mut out = vec![0u8; mask.len()];
    for y in 0..h {
        for x in 0..w {
            let mut m = 0u8;
            for (dx, dy) in &offsets {
                let (sx, sy) = (x + dx, y + dy);
                if sx >= 0 && sy >= 0 && sx < w && sy < h {
                    m = m.max(mask[(sy * w + sx) as usize]);
                }
            }
            out[(y * w + x) as usize] = m;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arial() -> Font {
        load_font(include_bytes!("../../../fonts/arial.ttf").to_vec()).unwrap()
    }

    fn style(px: f32, outline: Option<([u8; 4], u32)>) -> TextStyle {
        TextStyle { px, fill: [255, 252, 226, 255], outline }
    }

    fn alpha_bbox(img: &Rgba) -> Option<(u32, u32, u32, u32)> {
        let mut bb: Option<(u32, u32, u32, u32)> = None;
        for y in 0..img.height {
            for x in 0..img.width {
                if img.pixel(x, y)[3] > 0 {
                    bb = Some(match bb {
                        None => (x, y, x, y),
                        Some((a, b, c, d)) => (a.min(x), b.min(y), c.max(x), d.max(y)),
                    });
                }
            }
        }
        bb
    }

    #[test]
    fn same_style_shares_height_and_baseline() {
        let f = arial();
        let a = render_text(&f, "Photo", style(40.0, None));
        let b = render_text(&f, "gy", style(40.0, None));
        assert_eq!(a.image.height, b.image.height);
        assert_eq!(a.baseline_y, b.baseline_y);
        assert!(a.ascender_y < a.baseline_y && a.baseline_y < a.descender_y);
        assert_eq!(a.descender_y, a.image.height as i32);
    }

    #[test]
    fn wider_text_is_wider_and_ink_sits_on_baseline() {
        let f = arial();
        let one = render_text(&f, "1", style(100.0, None));
        let many = render_text(&f, "1111", style(100.0, None));
        assert!(many.image.width > 3 * one.image.width);
        let (_, top, _, bottom) = alpha_bbox(&one.image).unwrap();
        assert!(bottom as i32 <= one.baseline_y, "'1' has no descender");
        assert!((one.baseline_y - top as i32) as f32 > 60.0, "digit height ~0.7 em");
    }

    #[test]
    fn fill_colour_is_used() {
        let f = arial();
        let t = render_text(&f, "I", style(60.0, None));
        let (x0, y0, x1, y1) = alpha_bbox(&t.image).unwrap();
        let p = t.image.pixel((x0 + x1) / 2, (y0 + y1) / 2);
        assert_eq!(p, [255, 252, 226, 255]);
    }

    #[test]
    fn outline_pads_and_draws_dark_edge() {
        let f = arial();
        let plain = render_text(&f, "I", style(60.0, None));
        let out = render_text(&f, "I", style(60.0, Some(([0, 0, 0, 255], 3))));
        assert_eq!(out.image.width, plain.image.width + 6);
        assert_eq!(out.image.height, plain.image.height + 6);
        assert_eq!(out.baseline_y, plain.baseline_y + 3);
        // Just left of the stem: outline (black), fully opaque.
        let (x0, y0, x1, y1) = alpha_bbox(&out.image).unwrap();
        let mid_y = (y0 + y1) / 2;
        let edge = out.image.pixel(x0 + 1, mid_y);
        assert_eq!(edge[3], 255);
        assert!(edge[0] < 40, "outline is dark: {edge:?}");
        let centre = out.image.pixel((x0 + x1) / 2, mid_y);
        assert_eq!(centre, [255, 252, 226, 255]);
    }

    #[test]
    fn empty_text_is_a_valid_image() {
        let t = render_text(&arial(), "", style(20.0, None));
        assert!(t.image.width >= 1 && t.image.height >= 1);
        assert_eq!(t.image.data.len(), (t.image.width * t.image.height * 4) as usize);
    }
}
```

- [ ] **Step 5: Run tests**

Run: `cargo test osd:: && cargo clippy --all-targets -- -D warnings && cargo fmt --check`
Expected: `5 passed`, clean.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock src/osd src/lib.rs
git commit -m "feat(v2): OSD image types and outlined text rendering"
```

### Task 5: OSD assets and scene layout

**Files:**
- Create: `v2/src/osd/assets.rs`, `v2/src/osd/scene.rs`
- Modify: `v2/Cargo.toml`, `v2/src/osd/mod.rs` (add `pub mod assets;` and `pub mod scene;` before `pub mod text;`)
- Test: inline in both files

**Interfaces:**
- Consumes: `Rgba`, `SpriteId`, `Overlay`, `WHITE`, `load_font` (Task 4); `Status`, `Level`, `Network` (Task 3); `View` (Task 1); `Rect`.
- Produces:
  - `pub enum Icon { HddWarning, HddCritical, NoNetwork, TempWarning, TempCritical }` with `ALL` (v1 order) and `file()`; `pub struct Assets { pub title_font, pub text_font, pub icons: Vec<Rgba> }` with `load(dir: &Path, icon_px: u32) -> anyhow::Result<Self>` and `icon(Icon) -> &Rgba`; `load_icon(&Path, px) -> anyhow::Result<Rgba>`
  - `pub struct TextSprite { pub id: SpriteId, pub w: u32, pub h: u32, pub ascender_y: i32, pub baseline_y: i32, pub descender_y: i32 }`
  - `pub struct Sprites { pub white, pub title, pub subtitle, pub digits: Vec<TextSprite>, pub banner, pub icons: Vec<SpriteId>, pub debug_lines: Vec<TextSprite> }`
  - `pub struct SceneInput { pub screen: (u32, u32), pub view: View, pub status: Status, pub debug: bool, pub banner: bool }`
  - `pub struct Metrics { pub scale: f32, pub icon_px: u32, pub icon_pad: i32, pub icon_step: i32 }` with `for_screen((w, h))`
  - `pub struct DebugInfo { display_fps: f64, camera_fps: f64, mirror: bool, filter: bool, temp_c: Option<f32>, disk_pct: Option<f32> }` (all `pub`), `debug_text(&DebugInfo) -> Vec<String>`
  - `visible_icons(&Status) -> Vec<Icon>`, `compose(&Sprites, &SceneInput) -> Vec<Overlay>`

- [ ] **Step 1: Dependency**

In `v2/Cargo.toml` `[dependencies]`, after `gpiocdev = "0.8"`, add:
```toml
image = { version = "0.25", default-features = false, features = ["png"] }
```

- [ ] **Step 2: Write the failing tests**

Create both files with the Interfaces items stubbed `todo!()` (constants `TITLE_FONT`/`TEXT_FONT` and `Icon::ALL`/`file()` may be written in full), and these test modules.

`v2/src/osd/assets.rs` tests:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn repo_assets() -> &'static Path {
        Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/.."))
    }

    #[test]
    fn loads_v1_assets_at_requested_icon_size() {
        let a = Assets::load(repo_assets(), 32).unwrap();
        assert_eq!(a.icons.len(), 5);
        for icon in &a.icons {
            assert_eq!((icon.width, icon.height, icon.data.len()), (32, 32, 32 * 32 * 4));
        }
        assert!(a.icon(Icon::NoNetwork).data.chunks(4).any(|p| p[3] > 0), "icon has visible pixels");
        assert!(a.title_font.horizontal_line_metrics(100.0).is_some());
    }

    #[test]
    fn missing_assets_name_the_path() {
        let err = Assets::load(Path::new("/nonexistent"), 32).err().unwrap();
        assert!(format!("{err:#}").contains("/nonexistent/"), "{err:#}");
    }
}
```

`v2/src/osd/scene.rs` tests:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn ts(id: usize, w: u32, h: u32) -> TextSprite {
        TextSprite { id: SpriteId(id), w, h, ascender_y: 0, baseline_y: h as i32 * 3 / 4, descender_y: h as i32 }
    }

    fn sprites() -> Sprites {
        Sprites {
            white: SpriteId(0),
            title: ts(1, 800, 300),
            subtitle: ts(2, 600, 140),
            digits: (1..=5).map(|n| ts(10 + n, 500, 1000)).collect(),
            banner: ts(3, 700, 60),
            icons: (20..25).map(SpriteId).collect(),
            debug_lines: vec![ts(30, 150, 24), ts(31, 200, 24)],
        }
    }

    fn input(view: View) -> SceneInput {
        SceneInput { screen: (1920, 1080), view, status: Status::default(), debug: false, banner: false }
    }

    const NONE: View = View { title: false, countdown: None, flash: false };

    #[test]
    fn plain_live_view_draws_nothing() {
        assert!(compose(&sprites(), &input(NONE)).is_empty());
    }

    #[test]
    fn title_above_and_subtitle_below_centre() {
        let out = compose(&sprites(), &input(View { title: true, ..NONE }));
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].rect, Rect { x: 960 - 400, y: 540 - 300, w: 800, h: 300 });
        assert_eq!(out[1].rect, Rect { x: 960 - 300, y: 540, w: 600, h: 140 });
    }

    #[test]
    fn countdown_digit_is_centred() {
        let out = compose(&sprites(), &input(View { countdown: Some(3), ..NONE }));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].sprite, SpriteId(13));
        assert_eq!(out[0].rect, Rect { x: 960 - 250, y: 540 - 500, w: 500, h: 1000 });
    }

    #[test]
    fn out_of_range_digit_is_skipped() {
        assert!(compose(&sprites(), &input(View { countdown: Some(9), ..NONE })).is_empty());
    }

    #[test]
    fn flash_is_full_screen_and_last() {
        let mut i = input(View { title: true, flash: true, ..NONE });
        i.banner = true;
        let out = compose(&sprites(), &i);
        let last = out.last().unwrap();
        assert_eq!((last.sprite, last.rect, last.tint), (SpriteId(0), Rect { x: 0, y: 0, w: 1920, h: 1080 }, WHITE));
    }

    #[test]
    fn icons_follow_status_in_v1_order() {
        let status =
            Status { disk: Level::Critical, network: Network::Offline, temp: Level::Warning, ..Status::default() };
        assert_eq!(visible_icons(&status), vec![Icon::HddCritical, Icon::NoNetwork, Icon::TempWarning]);
        let mut i = input(NONE);
        i.status = status;
        let out = compose(&sprites(), &i);
        let m = Metrics::for_screen((1920, 1080));
        assert_eq!((m.icon_px, m.icon_pad, m.icon_step), (32, 19, 43));
        assert_eq!(out[0].sprite, SpriteId(21));
        assert_eq!(out[0].rect, Rect { x: 19, y: 19, w: 32, h: 32 });
        assert_eq!(out[1].rect.x, 19 + 43);
        assert_eq!(out[2].sprite, SpriteId(23));
        assert!(visible_icons(&Status { network: Network::Unknown, ..Status::default() }).is_empty());
    }

    #[test]
    fn debug_lines_right_aligned_on_v1_baselines() {
        let mut i = input(NONE);
        i.debug = true;
        let out = compose(&sprites(), &i);
        assert_eq!(out.len(), 2);
        let right = 1920 - 38;
        assert_eq!(out[0].rect.x + out[0].rect.w as i32, right);
        assert_eq!(out[1].rect.x + out[1].rect.w as i32, right);
        assert_eq!(out[0].rect.y, 70 - 18);
        assert_eq!(out[1].rect.y, 100 - 18);
    }

    #[test]
    fn debug_text_formats_values_and_missing() {
        let d = DebugInfo {
            display_fps: 59.97,
            camera_fps: 60.1,
            mirror: true,
            filter: false,
            temp_c: Some(48.31),
            disk_pct: None,
        };
        assert_eq!(
            debug_text(&d),
            vec![
                "fps: 60",
                "Camera fps: 60",
                "Mirrored: true",
                "Filtered: false",
                "Temperature: 48.3°C",
                "Disk used: --"
            ]
        );
    }

    #[test]
    fn banner_has_dark_box_behind_text() {
        let mut i = input(NONE);
        i.banner = true;
        let out = compose(&sprites(), &i);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].sprite, SpriteId(0));
        assert_eq!(out[0].tint, [0.0, 0.0, 0.0, 0.65]);
        assert!(out[0].rect.w > out[1].rect.w && out[0].rect.h > out[1].rect.h);
        assert_eq!(out[1].sprite, SpriteId(3));
    }
}
```

Add `pub mod assets;` and `pub mod scene;` to `v2/src/osd/mod.rs`.

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test osd::`
Expected: the new tests FAIL with `not yet implemented`; Task 4's text tests still pass.

- [ ] **Step 4: Write the implementation**

`v2/src/osd/assets.rs`:
```rust
//! Fonts and status icons loaded from the assets directory (v1's
//! `fonts/` and `images/` folders).

use std::path::Path;

use anyhow::Context;
use fontdue::Font;
use image::imageops::FilterType;

use super::Rgba;
use super::text::load_font;

pub const TITLE_FONT: &str = "fonts/Carnevalee Freakshow.ttf";
pub const TEXT_FONT: &str = "fonts/arial.ttf";

/// Status icons in v1's left-to-right order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    HddWarning,
    HddCritical,
    NoNetwork,
    TempWarning,
    TempCritical,
}

impl Icon {
    pub const ALL: [Icon; 5] =
        [Icon::HddWarning, Icon::HddCritical, Icon::NoNetwork, Icon::TempWarning, Icon::TempCritical];

    pub fn file(self) -> &'static str {
        match self {
            Icon::HddWarning => "images/hdd_warning.png",
            Icon::HddCritical => "images/hdd_critical.png",
            Icon::NoNetwork => "images/network.png",
            Icon::TempWarning => "images/temp_warning.png",
            Icon::TempCritical => "images/temp_critical.png",
        }
    }
}

pub struct Assets {
    pub title_font: Font,
    pub text_font: Font,
    /// Indexed like [`Icon::ALL`], each `icon_px` square.
    pub icons: Vec<Rgba>,
}

impl Assets {
    pub fn load(dir: &Path, icon_px: u32) -> anyhow::Result<Self> {
        let font = |rel: &str| -> anyhow::Result<Font> {
            let p = dir.join(rel);
            let bytes = std::fs::read(&p).with_context(|| format!("reading font {}", p.display()))?;
            load_font(bytes).with_context(|| format!("loading font {}", p.display()))
        };
        let icons = Icon::ALL
            .iter()
            .map(|icon| load_icon(&dir.join(icon.file()), icon_px))
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok(Self { title_font: font(TITLE_FONT)?, text_font: font(TEXT_FONT)?, icons })
    }

    pub fn icon(&self, icon: Icon) -> &Rgba {
        &self.icons[Icon::ALL.iter().position(|i| *i == icon).unwrap()]
    }
}

/// Decode a PNG and scale it to `px` × `px`.
pub fn load_icon(path: &Path, px: u32) -> anyhow::Result<Rgba> {
    let img = image::open(path).with_context(|| format!("loading icon {}", path.display()))?.to_rgba8();
    let img = image::imageops::resize(&img, px.max(1), px.max(1), FilterType::Lanczos3);
    Ok(Rgba { width: img.width(), height: img.height(), data: img.into_raw() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo_assets() -> &'static Path {
        Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/.."))
    }

    #[test]
    fn loads_v1_assets_at_requested_icon_size() {
        let a = Assets::load(repo_assets(), 32).unwrap();
        assert_eq!(a.icons.len(), 5);
        for icon in &a.icons {
            assert_eq!((icon.width, icon.height, icon.data.len()), (32, 32, 32 * 32 * 4));
        }
        assert!(a.icon(Icon::NoNetwork).data.chunks(4).any(|p| p[3] > 0), "icon has visible pixels");
        assert!(a.title_font.horizontal_line_metrics(100.0).is_some());
    }

    #[test]
    fn missing_assets_name_the_path() {
        let err = Assets::load(Path::new("/nonexistent"), 32).err().unwrap();
        assert!(format!("{err:#}").contains("/nonexistent/"), "{err:#}");
    }
}
```

`v2/src/osd/scene.rs`:
```rust
//! Pure layout: given the booth view, status and pre-rendered sprites,
//! decide which overlays to draw and where (v1 positions, scaled to the
//! screen). Draw order: icons, debug text, title, countdown, banner, flash.

use super::assets::Icon;
use super::{Overlay, SpriteId, WHITE};
use crate::geometry::Rect;
use crate::monitor::{Level, Network, Status};
use crate::state::View;

/// A rendered text sprite and its vertical metrics (see `TextImage`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextSprite {
    pub id: SpriteId,
    pub w: u32,
    pub h: u32,
    pub ascender_y: i32,
    pub baseline_y: i32,
    pub descender_y: i32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Sprites {
    /// 1×1 white texture, tinted for solid boxes and the flash.
    pub white: SpriteId,
    pub title: TextSprite,
    pub subtitle: TextSprite,
    /// `digits[n - 1]` shows the number `n`.
    pub digits: Vec<TextSprite>,
    pub banner: TextSprite,
    /// Indexed like [`Icon::ALL`]; all `icon_px` square.
    pub icons: Vec<SpriteId>,
    pub debug_lines: Vec<TextSprite>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SceneInput {
    pub screen: (u32, u32),
    pub view: View,
    pub status: Status,
    pub debug: bool,
    pub banner: bool,
}

/// Screen-relative sizes, from v1's values on a 1080-line display.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Metrics {
    pub scale: f32,
    pub icon_px: u32,
    pub icon_pad: i32,
    pub icon_step: i32,
}

impl Metrics {
    pub fn for_screen(screen: (u32, u32)) -> Self {
        let (w, h) = (screen.0 as f32, screen.1 as f32);
        Self {
            scale: h / 1080.0,
            icon_px: ((h * 0.03) as u32).max(1),
            icon_pad: (w * 0.01) as i32,
            icon_step: (h * 0.04) as i32,
        }
    }
}

/// Values shown by the debug overlay (`d` key).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DebugInfo {
    pub display_fps: f64,
    pub camera_fps: f64,
    pub mirror: bool,
    pub filter: bool,
    pub temp_c: Option<f32>,
    pub disk_pct: Option<f32>,
}

/// Debug overlay lines, top to bottom (v1's list plus camera fps).
pub fn debug_text(d: &DebugInfo) -> Vec<String> {
    let opt = |v: Option<f32>, unit: &str| v.map_or("--".to_string(), |v| format!("{v:.1}{unit}"));
    vec![
        format!("fps: {:.0}", d.display_fps),
        format!("Camera fps: {:.0}", d.camera_fps),
        format!("Mirrored: {}", d.mirror),
        format!("Filtered: {}", d.filter),
        format!("Temperature: {}", opt(d.temp_c, "°C")),
        format!("Disk used: {}", opt(d.disk_pct, "%")),
    ]
}

/// Icons to show for a status, in v1's order.
pub fn visible_icons(s: &Status) -> Vec<Icon> {
    Icon::ALL
        .into_iter()
        .filter(|i| match i {
            Icon::HddWarning => s.disk == Level::Warning,
            Icon::HddCritical => s.disk == Level::Critical,
            Icon::NoNetwork => s.network == Network::Offline,
            Icon::TempWarning => s.temp == Level::Warning,
            Icon::TempCritical => s.temp == Level::Critical,
        })
        .collect()
}

fn text_at(t: &TextSprite, x: i32, y: i32) -> Overlay {
    Overlay { sprite: t.id, rect: Rect { x, y, w: t.w, h: t.h }, tint: WHITE }
}

pub fn compose(sprites: &Sprites, input: &SceneInput) -> Vec<Overlay> {
    let (sw, sh) = input.screen;
    let m = Metrics::for_screen(input.screen);
    let (cx, cy) = ((sw / 2) as i32, (sh / 2) as i32);
    let mut out = Vec::new();

    for (i, icon) in visible_icons(&input.status).into_iter().enumerate() {
        let idx = Icon::ALL.iter().position(|a| *a == icon).unwrap();
        out.push(Overlay {
            sprite: sprites.icons[idx],
            rect: Rect { x: m.icon_pad + i as i32 * m.icon_step, y: m.icon_pad, w: m.icon_px, h: m.icon_px },
            tint: WHITE,
        });
    }

    if input.debug {
        // Right-aligned on the baseline, 2 % from the right edge (v1 anchor "rs").
        let right = sw as i32 - (sw as f32 * 0.02) as i32;
        for (i, line) in sprites.debug_lines.iter().enumerate() {
            let baseline = ((70.0 + 30.0 * i as f32) * m.scale) as i32;
            out.push(text_at(line, right - line.w as i32, baseline - line.baseline_y));
        }
    }

    if input.view.title {
        // Title sits on the centre line (anchor "md"), subtitle hangs below it ("ma").
        let t = &sprites.title;
        out.push(text_at(t, cx - t.w as i32 / 2, cy - t.descender_y));
        let s = &sprites.subtitle;
        out.push(text_at(s, cx - s.w as i32 / 2, cy - s.ascender_y));
    }

    if let Some(n) = input.view.countdown
        && let Some(d) = sprites.digits.get(n as usize - 1)
    {
        // Centred between ascender and descender (anchor "mm").
        let mid = (d.ascender_y + d.descender_y) / 2;
        out.push(text_at(d, cx - d.w as i32 / 2, cy - mid));
    }

    if input.banner {
        let b = &sprites.banner;
        let pad = (20.0 * m.scale) as i32;
        let y = sh as i32 - (sh as f32 * 0.15) as i32;
        let x = cx - b.w as i32 / 2;
        out.push(Overlay {
            sprite: sprites.white,
            rect: Rect { x: x - pad, y: y - pad, w: b.w + 2 * pad as u32, h: b.h + 2 * pad as u32 },
            tint: [0.0, 0.0, 0.0, 0.65],
        });
        out.push(text_at(b, x, y));
    }

    if input.view.flash {
        out.push(Overlay { sprite: sprites.white, rect: Rect { x: 0, y: 0, w: sw, h: sh }, tint: WHITE });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(id: usize, w: u32, h: u32) -> TextSprite {
        TextSprite { id: SpriteId(id), w, h, ascender_y: 0, baseline_y: h as i32 * 3 / 4, descender_y: h as i32 }
    }

    fn sprites() -> Sprites {
        Sprites {
            white: SpriteId(0),
            title: ts(1, 800, 300),
            subtitle: ts(2, 600, 140),
            digits: (1..=5).map(|n| ts(10 + n, 500, 1000)).collect(),
            banner: ts(3, 700, 60),
            icons: (20..25).map(SpriteId).collect(),
            debug_lines: vec![ts(30, 150, 24), ts(31, 200, 24)],
        }
    }

    fn input(view: View) -> SceneInput {
        SceneInput { screen: (1920, 1080), view, status: Status::default(), debug: false, banner: false }
    }

    const NONE: View = View { title: false, countdown: None, flash: false };

    #[test]
    fn plain_live_view_draws_nothing() {
        assert!(compose(&sprites(), &input(NONE)).is_empty());
    }

    #[test]
    fn title_above_and_subtitle_below_centre() {
        let out = compose(&sprites(), &input(View { title: true, ..NONE }));
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].rect, Rect { x: 960 - 400, y: 540 - 300, w: 800, h: 300 });
        assert_eq!(out[1].rect, Rect { x: 960 - 300, y: 540, w: 600, h: 140 });
    }

    #[test]
    fn countdown_digit_is_centred() {
        let out = compose(&sprites(), &input(View { countdown: Some(3), ..NONE }));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].sprite, SpriteId(13));
        assert_eq!(out[0].rect, Rect { x: 960 - 250, y: 540 - 500, w: 500, h: 1000 });
    }

    #[test]
    fn out_of_range_digit_is_skipped() {
        assert!(compose(&sprites(), &input(View { countdown: Some(9), ..NONE })).is_empty());
    }

    #[test]
    fn flash_is_full_screen_and_last() {
        let mut i = input(View { title: true, flash: true, ..NONE });
        i.banner = true;
        let out = compose(&sprites(), &i);
        let last = out.last().unwrap();
        assert_eq!((last.sprite, last.rect, last.tint), (SpriteId(0), Rect { x: 0, y: 0, w: 1920, h: 1080 }, WHITE));
    }

    #[test]
    fn icons_follow_status_in_v1_order() {
        let status =
            Status { disk: Level::Critical, network: Network::Offline, temp: Level::Warning, ..Status::default() };
        assert_eq!(visible_icons(&status), vec![Icon::HddCritical, Icon::NoNetwork, Icon::TempWarning]);
        let mut i = input(NONE);
        i.status = status;
        let out = compose(&sprites(), &i);
        let m = Metrics::for_screen((1920, 1080));
        assert_eq!((m.icon_px, m.icon_pad, m.icon_step), (32, 19, 43));
        assert_eq!(out[0].sprite, SpriteId(21));
        assert_eq!(out[0].rect, Rect { x: 19, y: 19, w: 32, h: 32 });
        assert_eq!(out[1].rect.x, 19 + 43);
        assert_eq!(out[2].sprite, SpriteId(23));
        assert!(visible_icons(&Status { network: Network::Unknown, ..Status::default() }).is_empty());
    }

    #[test]
    fn debug_lines_right_aligned_on_v1_baselines() {
        let mut i = input(NONE);
        i.debug = true;
        let out = compose(&sprites(), &i);
        assert_eq!(out.len(), 2);
        let right = 1920 - 38;
        assert_eq!(out[0].rect.x + out[0].rect.w as i32, right);
        assert_eq!(out[1].rect.x + out[1].rect.w as i32, right);
        assert_eq!(out[0].rect.y, 70 - 18);
        assert_eq!(out[1].rect.y, 100 - 18);
    }

    #[test]
    fn debug_text_formats_values_and_missing() {
        let d = DebugInfo {
            display_fps: 59.97,
            camera_fps: 60.1,
            mirror: true,
            filter: false,
            temp_c: Some(48.31),
            disk_pct: None,
        };
        assert_eq!(
            debug_text(&d),
            vec![
                "fps: 60",
                "Camera fps: 60",
                "Mirrored: true",
                "Filtered: false",
                "Temperature: 48.3°C",
                "Disk used: --"
            ]
        );
    }

    #[test]
    fn banner_has_dark_box_behind_text() {
        let mut i = input(NONE);
        i.banner = true;
        let out = compose(&sprites(), &i);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].sprite, SpriteId(0));
        assert_eq!(out[0].tint, [0.0, 0.0, 0.0, 0.65]);
        assert!(out[0].rect.w > out[1].rect.w && out[0].rect.h > out[1].rect.h);
        assert_eq!(out[1].sprite, SpriteId(3));
    }
}
```

- [ ] **Step 5: Run tests**

Run: `cargo test osd:: && cargo clippy --all-targets -- -D warnings && cargo fmt --check`
Expected: `16 passed` under `osd::` (5 text + 2 assets + 9 scene), clean.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock src/osd
git commit -m "feat(v2): OSD assets and scene layout"
```

### Task 6: Camera restart backoff and booth controller

**Files:**
- Create: `v2/src/display/controller.rs`
- Modify: `v2/src/camera/mod.rs` (add `Backoff` and a test module), `v2/src/display/mod.rs` (add `pub mod controller;` and `pub use controller::{CameraFactory, Controller};`)
- Test: inline in both files

**Interfaces:**
- Consumes: `Booth`, `Timings`, `Event`, `Command` (Task 1); `Action`, `GpioButton` (Task 2); `Monitors`, `Status` (Task 3); `SceneInput` (Task 5); `Camera`, `FakeCamera`, `FrameMailbox` (Plan 1).
- Produces:
  - `camera::Backoff` with `new(initial, max)`, `next_delay(&mut self) -> Duration`, `reset(&mut self)`
  - `pub type CameraFactory = Box<dyn FnMut() -> Box<dyn Camera>>;`
  - `pub fn timings_from(&Config) -> Timings`
  - `pub struct Controller` with `start(&Config, CameraFactory, Option<GpioButton>, Receiver<Action>, now) -> anyhow::Result<Self>`, `on_action(&mut self, Action, now) -> Option<Action>` (returns `Quit`/`ToggleMirror` for the caller), `update(&mut self, now)`, `take_frame(&self) -> Option<Frame>`, `recycle(&self, Frame)`, `camera_counters(&self) -> (u64, u64)`, `status(&self) -> Status`, `scene_input(&self, now, screen) -> SceneInput`; public fields `debug: bool`, `filter: bool`

- [ ] **Step 1: Write the failing tests**

In `v2/src/camera/mod.rs`, add `Backoff` with `todo!()` bodies above `pub trait Camera`, and append:
```rust
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
```

Create `v2/src/display/controller.rs` with the Interfaces items stubbed `todo!()` (keep the struct fields shown in Step 2) and this test module:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::camera::fake::FakeCamera;
    use crate::state::Phase;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;

    /// A camera whose stream can be killed from the test, like a dead sensor.
    struct DyingCamera {
        inner: FakeCamera,
        mailbox: Option<Arc<FrameMailbox>>,
        fail_start: bool,
    }

    impl Camera for DyingCamera {
        fn start(&mut self, mailbox: Arc<FrameMailbox>) -> anyhow::Result<()> {
            if self.fail_start {
                anyhow::bail!("sensor unplugged");
            }
            self.mailbox = Some(mailbox.clone());
            self.inner.start(mailbox)
        }
        fn stop(&mut self) {
            self.inner.stop();
        }
    }

    fn cfg() -> Config {
        let mut c = Config::default();
        c.storage.root = std::env::temp_dir();
        c
    }

    fn controller(starts: Arc<AtomicUsize>, fail_after_first: bool) -> (Controller, mpsc::Sender<Action>) {
        let (tx, rx) = mpsc::channel();
        let factory: CameraFactory = Box::new(move || {
            let n = starts.fetch_add(1, Ordering::SeqCst);
            Box::new(DyingCamera {
                inner: FakeCamera::new((8, 8), 100),
                mailbox: None,
                fail_start: fail_after_first && n > 0,
            })
        });
        (Controller::start(&cfg(), factory, None, rx, Instant::now()).unwrap(), tx)
    }

    #[test]
    fn timings_follow_config() {
        let mut c = Config::default();
        c.timing.countdown_s = 3;
        c.flash.lead_ms = 250;
        let t = timings_from(&c);
        assert_eq!(t.countdown, Duration::from_secs(3));
        assert_eq!(t.screen_flash_lead, Some(Duration::from_millis(250)));
        c.flash.mode = FlashMode::None;
        assert_eq!(timings_from(&c).screen_flash_lead, None);
    }

    #[test]
    fn button_press_runs_a_full_shot_and_freezes_preview() {
        let (mut c, tx) = controller(Arc::new(AtomicUsize::new(0)), false);
        let t0 = Instant::now();
        tx.send(Action::Press).unwrap();
        c.update(t0);
        assert!(matches!(c.booth.phase(), Phase::Countdown { .. }));
        assert!(c.scene_input(t0, (100, 100)).view.countdown.is_some());
        c.update(t0 + Duration::from_millis(4800));
        assert!(c.scene_input(t0 + Duration::from_millis(4800), (100, 100)).view.flash);
        c.update(t0 + Duration::from_secs(5));
        assert!(matches!(c.booth.phase(), Phase::Review { .. }));
        assert!(c.frozen);
        std::thread::sleep(Duration::from_millis(50));
        assert!(c.take_frame().is_none(), "frozen preview yields no frames");
        c.update(t0 + Duration::from_secs(10));
        assert_eq!(c.booth.phase(), Phase::Live);
        std::thread::sleep(Duration::from_millis(50));
        assert!(c.take_frame().is_some());
    }

    #[test]
    fn toggles_and_passthrough_actions() {
        let (mut c, _tx) = controller(Arc::new(AtomicUsize::new(0)), false);
        let now = Instant::now();
        assert_eq!(c.on_action(Action::ToggleDebug, now), None);
        assert!(c.debug);
        let f = c.filter;
        c.on_action(Action::ToggleFilter, now);
        assert_eq!(c.filter, !f);
        assert_eq!(c.on_action(Action::Quit, now), Some(Action::Quit));
        assert_eq!(c.on_action(Action::ToggleMirror, now), Some(Action::ToggleMirror));
    }

    #[test]
    fn dead_camera_shows_banner_and_restarts() {
        let starts = Arc::new(AtomicUsize::new(0));
        let (mut c, _tx) = controller(starts.clone(), false);
        let t0 = Instant::now();
        c.mailbox.close("sensor gone");
        c.update(t0);
        assert!(c.scene_input(t0, (100, 100)).banner);
        assert_eq!(c.booth.phase(), Phase::Idle);
        c.update(t0 + Duration::from_millis(500));
        assert_eq!(starts.load(Ordering::SeqCst), 1, "waits for the 1 s backoff");
        c.update(t0 + Duration::from_secs(1));
        assert_eq!(starts.load(Ordering::SeqCst), 2);
        assert!(!c.scene_input(t0, (100, 100)).banner);
        std::thread::sleep(Duration::from_millis(50));
        assert!(c.take_frame().is_some(), "new camera streams into the new mailbox");
    }

    #[test]
    fn failed_restart_backs_off() {
        let starts = Arc::new(AtomicUsize::new(0));
        let (mut c, _tx) = controller(starts.clone(), true);
        let t0 = Instant::now();
        c.mailbox.close("sensor gone");
        c.update(t0);
        c.update(t0 + Duration::from_secs(1));
        assert_eq!(starts.load(Ordering::SeqCst), 2);
        assert!(c.scene_input(t0, (100, 100)).banner, "still down");
        assert!(c.restart_at.unwrap() > Instant::now() + Duration::from_millis(1500), "next wait is 2 s");
    }
}
```

Register the module in `v2/src/display/mod.rs` as described above.

Run: `cargo test camera::tests display::controller` (or each filter separately)
Expected: FAIL — `not yet implemented`.

- [ ] **Step 2: Write the implementation**

`Backoff` in `v2/src/camera/mod.rs`:
```rust
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
```

`v2/src/display/controller.rs`:
```rust
//! Runs the booth: feeds input and time into the state machine, carries
//! out its commands, supervises the camera (restart with backoff, spec §5)
//! and gathers what the screen should show. Owns no GL state.

use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use crate::camera::{Backoff, Camera};
use crate::config::{Config, FlashMode};
use crate::frame::{Frame, FrameMailbox};
use crate::input::{Action, GpioButton};
use crate::monitor::{Monitors, Status};
use crate::osd::scene::SceneInput;
use crate::state::{Booth, Command, Event, Timings};

pub type CameraFactory = Box<dyn FnMut() -> Box<dyn Camera>>;

const CAPTURE_TIMEOUT: Duration = Duration::from_secs(5);

/// State-machine timings from the configuration.
pub fn timings_from(cfg: &Config) -> Timings {
    Timings {
        countdown: Duration::from_secs(cfg.timing.countdown_s as u64),
        review: Duration::from_secs(cfg.timing.review_s as u64),
        idle: Duration::from_secs(cfg.timing.idle_s as u64),
        screen_flash_lead: (cfg.flash.mode == FlashMode::Screen)
            .then(|| Duration::from_millis(cfg.flash.lead_ms as u64)),
        flash_max: Duration::from_millis(cfg.flash.max_ms as u64),
        capture_timeout: CAPTURE_TIMEOUT,
    }
}

pub struct Controller {
    booth: Booth,
    make_camera: CameraFactory,
    camera: Option<Box<dyn Camera>>,
    mailbox: Arc<FrameMailbox>,
    backoff: Backoff,
    restart_at: Option<Instant>,
    monitors: Monitors,
    _button: Option<GpioButton>,
    button_rx: Receiver<Action>,
    frozen: bool,
    pub debug: bool,
    pub filter: bool,
}

impl Controller {
    /// Start the camera and monitors. A camera that fails here is a startup
    /// error (spec §5): the process exits and systemd retries.
    pub fn start(
        cfg: &Config,
        mut make_camera: CameraFactory,
        button: Option<GpioButton>,
        button_rx: Receiver<Action>,
        now: Instant,
    ) -> anyhow::Result<Self> {
        if cfg.flash.mode == FlashMode::Strobe {
            tracing::warn!("flash.mode = \"strobe\" arrives in milestone 5; running without flash");
        }
        let mailbox = FrameMailbox::new();
        let mut camera = make_camera();
        camera.start(mailbox.clone())?;
        Ok(Self {
            booth: Booth::new(timings_from(cfg), now),
            make_camera,
            camera: Some(camera),
            mailbox,
            backoff: Backoff::new(Duration::from_secs(1), Duration::from_secs(30)),
            restart_at: None,
            monitors: Monitors::start(cfg)?,
            _button: button,
            button_rx,
            frozen: false,
            debug: false,
            filter: cfg.filter.enabled,
        })
    }

    /// Handle a user action. Returns actions the controller does not own
    /// (`Quit`, `ToggleMirror`) for the caller.
    pub fn on_action(&mut self, action: Action, now: Instant) -> Option<Action> {
        match action {
            Action::Press => {
                let cmds = self.booth.handle(Event::Press, now);
                self.apply(cmds, now);
                None
            }
            Action::ToggleDebug => {
                self.debug = !self.debug;
                None
            }
            Action::ToggleFilter => {
                self.filter = !self.filter;
                tracing::info!(filter = self.filter, "filter toggled");
                None
            }
            Action::Quit | Action::ToggleMirror => Some(action),
        }
    }

    /// Per-frame housekeeping: button presses, camera health, timed transitions.
    pub fn update(&mut self, now: Instant) {
        // An empty or disconnected (no button) channel both end the drain.
        while let Ok(a) = self.button_rx.try_recv() {
            let _ = self.on_action(a, now);
        }
        self.supervise_camera(now);
        let cmds = self.booth.tick(now);
        self.apply(cmds, now);
    }

    /// The newest camera frame, unless the preview is frozen for review.
    pub fn take_frame(&self) -> Option<Frame> {
        if self.frozen { None } else { self.mailbox.take_latest() }
    }

    pub fn recycle(&self, frame: Frame) {
        self.mailbox.recycle(frame);
    }

    /// (frames published, frames overwritten) of the current camera.
    pub fn camera_counters(&self) -> (u64, u64) {
        self.mailbox.counters()
    }

    pub fn status(&self) -> Status {
        self.monitors.snapshot()
    }

    pub fn scene_input(&self, now: Instant, screen: (u32, u32)) -> SceneInput {
        SceneInput {
            screen,
            view: self.booth.view(now),
            status: self.status(),
            debug: self.debug,
            banner: self.camera.is_none(),
        }
    }

    fn apply(&mut self, cmds: Vec<Command>, now: Instant) {
        for cmd in cmds {
            match cmd {
                // The screen flash is drawn from `View::flash`.
                Command::FlashOn | Command::FlashOff => {}
                Command::Capture => {
                    // Milestone 2: the "photo" is the preview frame on screen,
                    // frozen for review and not saved. Milestone 3 captures for real.
                    tracing::info!("capture (preview frame, not saved)");
                    let more = self.booth.handle(Event::CaptureDone, now);
                    self.apply(more, now);
                }
                Command::FreezePreview => self.frozen = true,
                Command::ResumePreview => self.frozen = false,
            }
        }
    }

    fn supervise_camera(&mut self, now: Instant) {
        if self.camera.is_some()
            && let Some(reason) = self.mailbox.closed_reason()
        {
            tracing::error!("camera stopped: {reason}");
            if let Some(mut cam) = self.camera.take() {
                cam.stop();
            }
            let cmds = self.booth.handle(Event::CameraLost, now);
            self.apply(cmds, now);
            self.restart_at = Some(now + self.backoff.next_delay());
        }
        if self.camera.is_none() && self.restart_at.is_some_and(|t| now >= t) {
            let mailbox = FrameMailbox::new();
            let mut cam = (self.make_camera)();
            match cam.start(mailbox.clone()) {
                Ok(()) => {
                    tracing::info!("camera restarted");
                    self.mailbox = mailbox;
                    self.camera = Some(cam);
                    self.backoff.reset();
                    self.restart_at = None;
                }
                Err(e) => {
                    let delay = self.backoff.next_delay();
                    tracing::error!("camera restart failed, retrying in {delay:?}: {e:#}");
                    self.restart_at = Some(Instant::now() + delay);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::camera::fake::FakeCamera;
    use crate::state::Phase;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;

    /// A camera whose stream can be killed from the test, like a dead sensor.
    struct DyingCamera {
        inner: FakeCamera,
        mailbox: Option<Arc<FrameMailbox>>,
        fail_start: bool,
    }

    impl Camera for DyingCamera {
        fn start(&mut self, mailbox: Arc<FrameMailbox>) -> anyhow::Result<()> {
            if self.fail_start {
                anyhow::bail!("sensor unplugged");
            }
            self.mailbox = Some(mailbox.clone());
            self.inner.start(mailbox)
        }
        fn stop(&mut self) {
            self.inner.stop();
        }
    }

    fn cfg() -> Config {
        let mut c = Config::default();
        c.storage.root = std::env::temp_dir();
        c
    }

    fn controller(starts: Arc<AtomicUsize>, fail_after_first: bool) -> (Controller, mpsc::Sender<Action>) {
        let (tx, rx) = mpsc::channel();
        let factory: CameraFactory = Box::new(move || {
            let n = starts.fetch_add(1, Ordering::SeqCst);
            Box::new(DyingCamera {
                inner: FakeCamera::new((8, 8), 100),
                mailbox: None,
                fail_start: fail_after_first && n > 0,
            })
        });
        (Controller::start(&cfg(), factory, None, rx, Instant::now()).unwrap(), tx)
    }

    #[test]
    fn timings_follow_config() {
        let mut c = Config::default();
        c.timing.countdown_s = 3;
        c.flash.lead_ms = 250;
        let t = timings_from(&c);
        assert_eq!(t.countdown, Duration::from_secs(3));
        assert_eq!(t.screen_flash_lead, Some(Duration::from_millis(250)));
        c.flash.mode = FlashMode::None;
        assert_eq!(timings_from(&c).screen_flash_lead, None);
    }

    #[test]
    fn button_press_runs_a_full_shot_and_freezes_preview() {
        let (mut c, tx) = controller(Arc::new(AtomicUsize::new(0)), false);
        let t0 = Instant::now();
        tx.send(Action::Press).unwrap();
        c.update(t0);
        assert!(matches!(c.booth.phase(), Phase::Countdown { .. }));
        assert!(c.scene_input(t0, (100, 100)).view.countdown.is_some());
        c.update(t0 + Duration::from_millis(4800));
        assert!(c.scene_input(t0 + Duration::from_millis(4800), (100, 100)).view.flash);
        c.update(t0 + Duration::from_secs(5));
        assert!(matches!(c.booth.phase(), Phase::Review { .. }));
        assert!(c.frozen);
        std::thread::sleep(Duration::from_millis(50));
        assert!(c.take_frame().is_none(), "frozen preview yields no frames");
        c.update(t0 + Duration::from_secs(10));
        assert_eq!(c.booth.phase(), Phase::Live);
        std::thread::sleep(Duration::from_millis(50));
        assert!(c.take_frame().is_some());
    }

    #[test]
    fn toggles_and_passthrough_actions() {
        let (mut c, _tx) = controller(Arc::new(AtomicUsize::new(0)), false);
        let now = Instant::now();
        assert_eq!(c.on_action(Action::ToggleDebug, now), None);
        assert!(c.debug);
        let f = c.filter;
        c.on_action(Action::ToggleFilter, now);
        assert_eq!(c.filter, !f);
        assert_eq!(c.on_action(Action::Quit, now), Some(Action::Quit));
        assert_eq!(c.on_action(Action::ToggleMirror, now), Some(Action::ToggleMirror));
    }

    #[test]
    fn dead_camera_shows_banner_and_restarts() {
        let starts = Arc::new(AtomicUsize::new(0));
        let (mut c, _tx) = controller(starts.clone(), false);
        let t0 = Instant::now();
        c.mailbox.close("sensor gone");
        c.update(t0);
        assert!(c.scene_input(t0, (100, 100)).banner);
        assert_eq!(c.booth.phase(), Phase::Idle);
        c.update(t0 + Duration::from_millis(500));
        assert_eq!(starts.load(Ordering::SeqCst), 1, "waits for the 1 s backoff");
        c.update(t0 + Duration::from_secs(1));
        assert_eq!(starts.load(Ordering::SeqCst), 2);
        assert!(!c.scene_input(t0, (100, 100)).banner);
        std::thread::sleep(Duration::from_millis(50));
        assert!(c.take_frame().is_some(), "new camera streams into the new mailbox");
    }

    #[test]
    fn failed_restart_backs_off() {
        let starts = Arc::new(AtomicUsize::new(0));
        let (mut c, _tx) = controller(starts.clone(), true);
        let t0 = Instant::now();
        c.mailbox.close("sensor gone");
        c.update(t0);
        c.update(t0 + Duration::from_secs(1));
        assert_eq!(starts.load(Ordering::SeqCst), 2);
        assert!(c.scene_input(t0, (100, 100)).banner, "still down");
        assert!(c.restart_at.unwrap() > Instant::now() + Duration::from_millis(1500), "next wait is 2 s");
    }
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check`
Expected: all pass (1 backoff + 5 controller tests new), clean. The controller tests run a real `FakeCamera` thread; they take well under a second.

- [ ] **Step 4: Commit**

```bash
git add src/camera/mod.rs src/display/controller.rs src/display/mod.rs
git commit -m "feat(v2): booth controller with camera restart backoff"
```

### Task 7: Overlay rendering, HUD and app wiring

GL and windowing are verified by running on a (virtual) display; their pure inputs are tested in Tasks 1–6.

**Files:**
- Create: `v2/src/display/hud.rs`
- Modify (replace whole files): `v2/src/display/renderer.rs`, `v2/src/display/app.rs`, `v2/src/display/mod.rs`, `v2/src/main.rs`

**Interfaces:**
- Consumes: everything above.
- Produces:
  - `Renderer::create_sprite(&Rgba) -> anyhow::Result<SpriteId>`, `update_sprite(SpriteId, &Rgba)`, `delete_sprite(SpriteId)`, `screen() -> (u32, u32)`, `draw(&[Overlay])` (camera quad, then blended overlays in order)
  - `Hud::new(&mut Renderer, assets_dir: &Path, countdown_max: u32)`, `sprites() -> &Sprites`, `resize(&mut Renderer)`, `set_debug_text(&mut Renderer, &[String])`
  - `RunOptions { mirror, bench, windowed, assets_dir: PathBuf, countdown_max: u32 }`, `run(Controller, RunOptions) -> anyhow::Result<()>`
  - CLI unchanged; `--no-gpio` now skips the button.

- [ ] **Step 1: Renderer with overlay pass**

`v2/src/display/renderer.rs`:
```rust
//! OpenGL ES 3 renderer. Pass 1 draws the streamed RGB camera texture as a
//! letterboxed, optionally mirrored quad. Pass 2 alpha-blends OSD sprites
//! (pre-rendered RGBA textures) on top. All GL calls happen on the render
//! thread.

use anyhow::{Context, anyhow};
use glow::HasContext;

use crate::frame::Frame;
use crate::geometry::{letterbox, quad_vertices};
use crate::osd::{Overlay, Rgba, SpriteId};

const VERTEX_SHADER: &str = r#"#version 300 es
layout(location = 0) in vec2 a_pos;
layout(location = 1) in vec2 a_uv;
out vec2 v_uv;
void main() {
    v_uv = a_uv;
    gl_Position = vec4(a_pos, 0.0, 1.0);
}
"#;

const CAMERA_FRAGMENT_SHADER: &str = r#"#version 300 es
precision highp float;
in vec2 v_uv;
uniform sampler2D u_tex;
out vec4 o_color;
void main() {
    o_color = vec4(texture(u_tex, v_uv).rgb, 1.0);
}
"#;

const OVERLAY_FRAGMENT_SHADER: &str = r#"#version 300 es
precision highp float;
in vec2 v_uv;
uniform sampler2D u_tex;
uniform vec4 u_tint;
out vec4 o_color;
void main() {
    o_color = texture(u_tex, v_uv) * u_tint;
}
"#;

struct Sprite {
    texture: glow::Texture,
    size: (u32, u32),
}

pub struct Renderer {
    gl: glow::Context,
    camera_program: glow::Program,
    overlay_program: glow::Program,
    tint_loc: Option<glow::UniformLocation>,
    camera_vao: glow::VertexArray,
    camera_vbo: glow::Buffer,
    overlay_vao: glow::VertexArray,
    overlay_vbo: glow::Buffer,
    texture: glow::Texture,
    tex_size: Option<(u32, u32)>,
    sprites: Vec<Option<Sprite>>,
    screen: (u32, u32),
    mirror: bool,
    quad_dirty: bool,
}

impl Renderer {
    /// # Safety
    /// `gl` must be current on this thread for the renderer's whole life.
    pub unsafe fn new(gl: glow::Context, screen: (u32, u32), mirror: bool) -> anyhow::Result<Self> {
        unsafe {
            let camera_program = link_program(&gl, VERTEX_SHADER, CAMERA_FRAGMENT_SHADER)?;
            let overlay_program = link_program(&gl, VERTEX_SHADER, OVERLAY_FRAGMENT_SHADER)?;
            let (camera_vao, camera_vbo) = quad_buffers(&gl)?;
            let (overlay_vao, overlay_vbo) = quad_buffers(&gl)?;
            let texture = new_texture(&gl)?;
            // Camera rows are tightly packed RGB; 1456*3 is not a multiple of 4.
            gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);

            for program in [camera_program, overlay_program] {
                gl.use_program(Some(program));
                let loc = gl.get_uniform_location(program, "u_tex");
                gl.uniform_1_i32(loc.as_ref(), 0);
            }
            let tint_loc = gl.get_uniform_location(overlay_program, "u_tint");
            gl.clear_color(0.0, 0.0, 0.0, 1.0);
            gl.blend_func(glow::SRC_ALPHA, glow::ONE_MINUS_SRC_ALPHA);

            Ok(Self {
                gl,
                camera_program,
                overlay_program,
                tint_loc,
                camera_vao,
                camera_vbo,
                overlay_vao,
                overlay_vbo,
                texture,
                tex_size: None,
                sprites: Vec::new(),
                screen,
                mirror,
                quad_dirty: true,
            })
        }
    }

    pub fn resize(&mut self, screen: (u32, u32)) {
        if screen != self.screen {
            self.screen = screen;
            self.quad_dirty = true;
        }
    }

    pub fn screen(&self) -> (u32, u32) {
        self.screen
    }

    pub fn set_mirror(&mut self, mirror: bool) {
        if mirror != self.mirror {
            self.mirror = mirror;
            self.quad_dirty = true;
        }
    }

    pub fn mirror(&self) -> bool {
        self.mirror
    }

    /// Upload a new camera frame into the streamed texture.
    pub fn upload(&mut self, frame: &Frame) {
        assert_eq!(
            frame.data.len(),
            Frame::byte_len(frame.width, frame.height),
            "frame buffer length does not match its size"
        );
        let gl = &self.gl;
        let size = (frame.width, frame.height);
        unsafe {
            gl.bind_texture(glow::TEXTURE_2D, Some(self.texture));
            let pixels = glow::PixelUnpackData::Slice(Some(&frame.data));
            if self.tex_size != Some(size) {
                gl.tex_image_2d(
                    glow::TEXTURE_2D,
                    0,
                    glow::RGB8 as i32,
                    size.0 as i32,
                    size.1 as i32,
                    0,
                    glow::RGB,
                    glow::UNSIGNED_BYTE,
                    pixels,
                );
                self.tex_size = Some(size);
                self.quad_dirty = true;
            } else {
                gl.tex_sub_image_2d(
                    glow::TEXTURE_2D,
                    0,
                    0,
                    0,
                    size.0 as i32,
                    size.1 as i32,
                    glow::RGB,
                    glow::UNSIGNED_BYTE,
                    pixels,
                );
            }
        }
    }

    /// Create a GPU texture from an RGBA image; draw it via [`Overlay`]s.
    pub fn create_sprite(&mut self, image: &Rgba) -> anyhow::Result<SpriteId> {
        let texture = unsafe { new_texture(&self.gl)? };
        let id = SpriteId(self.sprites.len());
        self.sprites.push(Some(Sprite { texture, size: (0, 0) }));
        self.update_sprite(id, image);
        Ok(id)
    }

    /// Replace a sprite's pixels (e.g. the debug text once a second).
    pub fn update_sprite(&mut self, id: SpriteId, image: &Rgba) {
        assert_eq!(image.data.len(), image.width as usize * image.height as usize * 4, "RGBA length mismatch");
        let Some(Some(sprite)) = self.sprites.get_mut(id.0) else { return };
        unsafe {
            self.gl.bind_texture(glow::TEXTURE_2D, Some(sprite.texture));
            self.gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                glow::RGBA8 as i32,
                image.width as i32,
                image.height as i32,
                0,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(Some(&image.data)),
            );
        }
        sprite.size = (image.width, image.height);
    }

    pub fn delete_sprite(&mut self, id: SpriteId) {
        if let Some(slot) = self.sprites.get_mut(id.0)
            && let Some(sprite) = slot.take()
        {
            unsafe { self.gl.delete_texture(sprite.texture) };
        }
    }

    pub fn draw(&mut self, overlays: &[Overlay]) {
        let gl = &self.gl;
        unsafe {
            gl.viewport(0, 0, self.screen.0 as i32, self.screen.1 as i32);
            gl.clear(glow::COLOR_BUFFER_BIT);
            gl.active_texture(glow::TEXTURE0);
            if let Some(tex_size) = self.tex_size {
                gl.use_program(Some(self.camera_program));
                gl.bind_vertex_array(Some(self.camera_vao));
                if self.quad_dirty {
                    let rect = letterbox(tex_size, self.screen);
                    let verts = quad_vertices(rect, self.screen, self.mirror);
                    gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.camera_vbo));
                    gl.buffer_sub_data_u8_slice(glow::ARRAY_BUFFER, 0, as_bytes(&verts));
                    self.quad_dirty = false;
                }
                gl.bind_texture(glow::TEXTURE_2D, Some(self.texture));
                gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
            }
            if overlays.is_empty() {
                return;
            }
            gl.enable(glow::BLEND);
            gl.use_program(Some(self.overlay_program));
            gl.bind_vertex_array(Some(self.overlay_vao));
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.overlay_vbo));
            for o in overlays {
                let Some(Some(sprite)) = self.sprites.get(o.sprite.0) else { continue };
                let verts = quad_vertices(o.rect, self.screen, false);
                gl.buffer_sub_data_u8_slice(glow::ARRAY_BUFFER, 0, as_bytes(&verts));
                gl.uniform_4_f32(self.tint_loc.as_ref(), o.tint[0], o.tint[1], o.tint[2], o.tint[3]);
                gl.bind_texture(glow::TEXTURE_2D, Some(sprite.texture));
                gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
            }
            gl.disable(glow::BLEND);
        }
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        unsafe {
            for sprite in self.sprites.drain(..).flatten() {
                self.gl.delete_texture(sprite.texture);
            }
            self.gl.delete_texture(self.texture);
            self.gl.delete_buffer(self.camera_vbo);
            self.gl.delete_vertex_array(self.camera_vao);
            self.gl.delete_buffer(self.overlay_vbo);
            self.gl.delete_vertex_array(self.overlay_vao);
            self.gl.delete_program(self.camera_program);
            self.gl.delete_program(self.overlay_program);
        }
    }
}

/// VAO + VBO for one 4-vertex `[x, y, u, v]` triangle strip.
unsafe fn quad_buffers(gl: &glow::Context) -> anyhow::Result<(glow::VertexArray, glow::Buffer)> {
    unsafe {
        let vao = gl.create_vertex_array().map_err(|e| anyhow!(e))?;
        let vbo = gl.create_buffer().map_err(|e| anyhow!(e))?;
        gl.bind_vertex_array(Some(vao));
        gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
        gl.buffer_data_size(glow::ARRAY_BUFFER, 16 * 4, glow::DYNAMIC_DRAW);
        gl.enable_vertex_attrib_array(0);
        gl.vertex_attrib_pointer_f32(0, 2, glow::FLOAT, false, 16, 0);
        gl.enable_vertex_attrib_array(1);
        gl.vertex_attrib_pointer_f32(1, 2, glow::FLOAT, false, 16, 8);
        Ok((vao, vbo))
    }
}

unsafe fn new_texture(gl: &glow::Context) -> anyhow::Result<glow::Texture> {
    unsafe {
        let texture = gl.create_texture().map_err(|e| anyhow!(e))?;
        gl.bind_texture(glow::TEXTURE_2D, Some(texture));
        for (k, v) in [
            (glow::TEXTURE_MIN_FILTER, glow::LINEAR),
            (glow::TEXTURE_MAG_FILTER, glow::LINEAR),
            (glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE),
            (glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE),
        ] {
            gl.tex_parameter_i32(glow::TEXTURE_2D, k, v as i32);
        }
        Ok(texture)
    }
}

fn as_bytes(v: &[f32]) -> &[u8] {
    // SAFETY: f32 has no padding and any byte pattern is a valid u8.
    unsafe { std::slice::from_raw_parts(v.as_ptr().cast(), std::mem::size_of_val(v)) }
}

unsafe fn link_program(gl: &glow::Context, vs: &str, fs: &str) -> anyhow::Result<glow::Program> {
    unsafe {
        let program = gl.create_program().map_err(|e| anyhow!(e))?;
        let mut shaders = Vec::new();
        for (kind, src) in [(glow::VERTEX_SHADER, vs), (glow::FRAGMENT_SHADER, fs)] {
            let s = gl.create_shader(kind).map_err(|e| anyhow!(e))?;
            gl.shader_source(s, src);
            gl.compile_shader(s);
            if !gl.get_shader_compile_status(s) {
                return Err(anyhow!(gl.get_shader_info_log(s))).context("shader compile failed");
            }
            gl.attach_shader(program, s);
            shaders.push(s);
        }
        gl.link_program(program);
        if !gl.get_program_link_status(program) {
            return Err(anyhow!(gl.get_program_info_log(program))).context("program link failed");
        }
        for s in shaders {
            gl.detach_shader(program, s);
            gl.delete_shader(s);
        }
        Ok(program)
    }
}
```

- [ ] **Step 2: HUD**

`v2/src/display/hud.rs`:
```rust
//! Turns the OSD's CPU-rendered text and icons into renderer sprites, sized
//! for the current screen. Static text is rendered once; debug lines are
//! re-rendered when their content changes.

use std::path::{Path, PathBuf};

use super::renderer::Renderer;
use crate::osd::assets::Assets;
use crate::osd::scene::{Metrics, Sprites, TextSprite};
use crate::osd::text::{TextImage, TextStyle, render_text};
use crate::osd::{Rgba, SpriteId};

const CREAM: [u8; 4] = [255, 252, 226, 255];
const BLACK: [u8; 4] = [0, 0, 0, 255];
const WHITE: [u8; 4] = [255, 255, 255, 255];
const RED: [u8; 4] = [255, 0, 0, 255];
pub const TITLE: &str = "Photo Shoot";
pub const SUBTITLE: &str = "Press to start";
pub const CAMERA_ERROR: &str = "Camera problem, restarting…";

pub struct Hud {
    assets_dir: PathBuf,
    assets: Assets,
    countdown_max: u32,
    screen: (u32, u32),
    sprites: Sprites,
    debug_text: Vec<String>,
}

impl Hud {
    pub fn new(r: &mut Renderer, assets_dir: &Path, countdown_max: u32) -> anyhow::Result<Self> {
        let screen = r.screen();
        let assets = Assets::load(assets_dir, Metrics::for_screen(screen).icon_px)?;
        let sprites = build(r, &assets, screen, countdown_max)?;
        Ok(Self {
            assets_dir: assets_dir.to_path_buf(),
            assets,
            countdown_max,
            screen,
            sprites,
            debug_text: Vec::new(),
        })
    }

    pub fn sprites(&self) -> &Sprites {
        &self.sprites
    }

    /// Re-render everything if the screen size changed (icons and text scale with it).
    pub fn resize(&mut self, r: &mut Renderer) -> anyhow::Result<()> {
        let screen = r.screen();
        if screen == self.screen {
            return Ok(());
        }
        for id in all_ids(&self.sprites) {
            r.delete_sprite(id);
        }
        self.assets = Assets::load(&self.assets_dir, Metrics::for_screen(screen).icon_px)?;
        self.sprites = build(r, &self.assets, screen, self.countdown_max)?;
        self.screen = screen;
        self.debug_text.clear();
        Ok(())
    }

    /// Update the debug overlay lines; re-renders only lines that changed.
    pub fn set_debug_text(&mut self, r: &mut Renderer, lines: &[String]) -> anyhow::Result<()> {
        let style = style(20.0, Metrics::for_screen(self.screen).scale, RED, false);
        for (i, line) in lines.iter().enumerate() {
            if self.debug_text.get(i) == Some(line) {
                continue;
            }
            let t = render_text(&self.assets.text_font, line, style);
            match self.sprites.debug_lines.get_mut(i) {
                Some(existing) => {
                    r.update_sprite(existing.id, &t.image);
                    *existing = text_sprite(existing.id, &t);
                }
                None => {
                    let id = r.create_sprite(&t.image)?;
                    self.sprites.debug_lines.push(text_sprite(id, &t));
                }
            }
        }
        for extra in self.sprites.debug_lines.drain(lines.len().min(self.sprites.debug_lines.len())..) {
            r.delete_sprite(extra.id);
        }
        self.debug_text = lines.to_vec();
        Ok(())
    }
}

fn style(px_at_1080: f32, scale: f32, fill: [u8; 4], outline: bool) -> TextStyle {
    let radius = (2.0 * scale).round().max(1.0) as u32;
    TextStyle { px: px_at_1080 * scale, fill, outline: outline.then_some((BLACK, radius)) }
}

fn text_sprite(id: SpriteId, t: &TextImage) -> TextSprite {
    TextSprite {
        id,
        w: t.image.width,
        h: t.image.height,
        ascender_y: t.ascender_y,
        baseline_y: t.baseline_y,
        descender_y: t.descender_y,
    }
}

fn build(r: &mut Renderer, a: &Assets, screen: (u32, u32), countdown_max: u32) -> anyhow::Result<Sprites> {
    let scale = Metrics::for_screen(screen).scale;
    let mut text = |font: &fontdue::Font, s: &str, st: TextStyle| -> anyhow::Result<TextSprite> {
        let t = render_text(font, s, st);
        Ok(text_sprite(r.create_sprite(&t.image)?, &t))
    };
    // v1 sizes on a 1080-line screen: title 250, subtitle 120, digits 1000.
    let title = text(&a.title_font, TITLE, style(250.0, scale, CREAM, true))?;
    let subtitle = text(&a.title_font, SUBTITLE, style(120.0, scale, CREAM, true))?;
    let digits = (1..=countdown_max)
        .map(|n| text(&a.title_font, &n.to_string(), style(1000.0, scale, CREAM, true)))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let banner = text(&a.text_font, CAMERA_ERROR, style(48.0, scale, WHITE, false))?;
    let white = r.create_sprite(&Rgba { width: 1, height: 1, data: vec![255; 4] })?;
    let icons = a.icons.iter().map(|img| r.create_sprite(img)).collect::<anyhow::Result<Vec<_>>>()?;
    Ok(Sprites { white, title, subtitle, digits, banner, icons, debug_lines: Vec::new() })
}

fn all_ids(s: &Sprites) -> Vec<SpriteId> {
    let mut ids = vec![s.white, s.title.id, s.subtitle.id, s.banner.id];
    ids.extend(s.digits.iter().map(|d| d.id));
    ids.extend(s.icons.iter().copied());
    ids.extend(s.debug_lines.iter().map(|d| d.id));
    ids
}
```

- [ ] **Step 3: App, module list and main**

`v2/src/display/app.rs`:
```rust
//! winit application: creates the fullscreen GLES window and, on every
//! redraw, advances the booth, uploads the newest camera frame and draws it
//! with the OSD on top, presenting with vsync.

use std::ffi::CString;
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow};
use glutin::config::{Config as GlConfig, ConfigTemplateBuilder, GlConfig as _};
use glutin::context::{ContextApi, ContextAttributesBuilder, PossiblyCurrentContext, Version};
use glutin::display::{GetGlDisplay, GlDisplay};
use glutin::prelude::NotCurrentGlContext;
use glutin::surface::{GlSurface, Surface, SwapInterval, WindowSurface};
use glutin_winit::{DisplayBuilder, GlWindow};
use raw_window_handle::HasWindowHandle;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Fullscreen, Window, WindowId};

use super::controller::Controller;
use super::hud::Hud;
use super::renderer::Renderer;
use crate::input::{Action, map_key};
use crate::osd::scene::{DebugInfo, compose, debug_text};
use crate::stats::RateMeter;

pub struct RunOptions {
    pub mirror: bool,
    pub bench: bool,
    /// Windowed 1280x720 instead of fullscreen (development on a desktop).
    pub windowed: bool,
    pub assets_dir: PathBuf,
    /// Highest countdown digit to pre-render (`timing.countdown_s`).
    pub countdown_max: u32,
}

struct Gfx {
    // Field order matters: HUD sprites and renderer (GL objects) drop before the context.
    hud: Hud,
    renderer: Renderer,
    surface: Surface<WindowSurface>,
    context: PossiblyCurrentContext,
    window: Window,
}

struct App {
    opts: RunOptions,
    ctrl: Controller,
    gfx: Option<Gfx>,
    error: Option<anyhow::Error>,
    display_rate: RateMeter,
    upload_rate: RateMeter,
    last_counters: (u64, u64),
    display_fps: f64,
    camera_fps: f64,
}

/// Run the render loop on the calling (main) thread until Esc/q or close.
pub fn run(ctrl: Controller, opts: RunOptions) -> anyhow::Result<()> {
    let event_loop = EventLoop::new().context("creating event loop")?;
    event_loop.set_control_flow(ControlFlow::Wait);
    let now = Instant::now();
    let mut app = App {
        opts,
        ctrl,
        gfx: None,
        error: None,
        display_rate: RateMeter::new(Duration::from_secs(1), now),
        upload_rate: RateMeter::new(Duration::from_secs(1), now),
        last_counters: (0, 0),
        display_fps: 0.0,
        camera_fps: 0.0,
    };
    event_loop.run_app(&mut app).context("event loop")?;
    match app.error {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

impl App {
    fn init(&mut self, event_loop: &ActiveEventLoop) -> anyhow::Result<Gfx> {
        let mut attrs = Window::default_attributes().with_title("piBooth");
        attrs = if self.opts.windowed {
            attrs.with_inner_size(winit::dpi::LogicalSize::new(1280.0, 720.0))
        } else {
            attrs.with_fullscreen(Some(Fullscreen::Borderless(None)))
        };
        let template = ConfigTemplateBuilder::new().with_alpha_size(0);
        let (window, gl_config) = DisplayBuilder::new()
            .with_window_attributes(Some(attrs))
            .build(event_loop, template, pick_config)
            .map_err(|e| anyhow!("{e}"))
            .context("creating GL display")?;
        let window = window.ok_or_else(|| anyhow!("no window created"))?;
        window.set_cursor_visible(false);

        let raw = window.window_handle()?.as_raw();
        let display = gl_config.display();
        let ctx_attrs = ContextAttributesBuilder::new()
            .with_context_api(ContextApi::Gles(Some(Version::new(3, 0))))
            .build(Some(raw));
        let not_current =
            unsafe { display.create_context(&gl_config, &ctx_attrs) }.context("creating GLES 3.0 context")?;
        let surface_attrs = window.build_surface_attributes(Default::default())?;
        let surface = unsafe { display.create_window_surface(&gl_config, &surface_attrs) }?;
        let context = not_current.make_current(&surface)?;
        // vsync: present at most once per display refresh.
        if let Err(e) = surface.set_swap_interval(&context, SwapInterval::Wait(NonZeroU32::MIN)) {
            tracing::warn!("could not enable vsync: {e}");
        }

        let gl = unsafe {
            glow::Context::from_loader_function(|s| {
                let s = CString::new(s).unwrap();
                display.get_proc_address(&s).cast()
            })
        };
        let size = window.inner_size();
        let mut renderer = unsafe { Renderer::new(gl, (size.width, size.height), self.opts.mirror) }?;
        let hud = Hud::new(&mut renderer, &self.opts.assets_dir, self.opts.countdown_max)?;
        tracing::info!(width = size.width, height = size.height, "display ready");
        Ok(Gfx { hud, renderer, surface, context, window })
    }

    fn redraw(&mut self) {
        let now = Instant::now();
        self.ctrl.update(now);
        let Some(gfx) = self.gfx.as_mut() else { return };
        if let Some(frame) = self.ctrl.take_frame() {
            gfx.renderer.upload(&frame);
            self.ctrl.recycle(frame);
            self.upload_rate.tick();
        }
        let overlays = compose(gfx.hud.sprites(), &self.ctrl.scene_input(now, gfx.renderer.screen()));
        gfx.renderer.draw(&overlays);
        gfx.window.pre_present_notify();
        if let Err(e) = gfx.surface.swap_buffers(&gfx.context) {
            tracing::error!("swap_buffers: {e}");
        }
        self.display_rate.tick();
        self.report();
    }

    /// Once a second: frame-rate figures for `--bench` and the debug overlay.
    fn report(&mut self) {
        let now = Instant::now();
        let Some(display_fps) = self.display_rate.poll(now) else { return };
        let upload_fps = self.upload_rate.poll(now).unwrap_or(0.0);
        let (published, overwritten) = self.ctrl.camera_counters();
        // Counters restart from zero when the camera is restarted.
        let camera_frames = published.saturating_sub(self.last_counters.0);
        let skipped = overwritten.saturating_sub(self.last_counters.1);
        self.last_counters = (published, overwritten);
        self.display_fps = display_fps;
        self.camera_fps = camera_frames as f64;
        if self.opts.bench {
            tracing::info!(display_fps, upload_fps, camera_frames, skipped, "bench");
        } else {
            tracing::debug!(display_fps, upload_fps, camera_frames, skipped, "bench");
        }
        self.refresh_debug_text();
    }

    fn refresh_debug_text(&mut self) {
        let Some(gfx) = self.gfx.as_mut() else { return };
        if !self.ctrl.debug {
            return;
        }
        let status = self.ctrl.status();
        let lines = debug_text(&DebugInfo {
            display_fps: self.display_fps,
            camera_fps: self.camera_fps,
            mirror: gfx.renderer.mirror(),
            filter: self.ctrl.filter,
            temp_c: status.temp_c,
            disk_pct: status.disk_pct,
        });
        if let Err(e) = gfx.hud.set_debug_text(&mut gfx.renderer, &lines) {
            tracing::warn!("debug overlay: {e:#}");
        }
    }

    fn on_action(&mut self, action: Action, event_loop: &ActiveEventLoop) {
        match self.ctrl.on_action(action, Instant::now()) {
            Some(Action::Quit) => event_loop.exit(),
            Some(Action::ToggleMirror) => {
                if let Some(gfx) = self.gfx.as_mut() {
                    let m = !gfx.renderer.mirror();
                    gfx.renderer.set_mirror(m);
                    tracing::info!(mirror = m, "mirror toggled");
                }
            }
            Some(_) | None => {}
        }
        if action == Action::ToggleDebug {
            self.refresh_debug_text();
        }
    }
}

fn pick_config(configs: Box<dyn Iterator<Item = GlConfig> + '_>) -> GlConfig {
    // Prefer a config without multisampling; we only draw one textured quad.
    configs.min_by_key(|c| c.num_samples()).expect("no GL configs available")
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.gfx.is_some() {
            return;
        }
        match self.init(event_loop) {
            Ok(g) => self.gfx = Some(g),
            Err(e) => {
                self.error = Some(e);
                event_loop.exit();
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let (Some(gfx), Some(w), Some(h)) =
                    (self.gfx.as_mut(), NonZeroU32::new(size.width), NonZeroU32::new(size.height))
                {
                    gfx.surface.resize(&gfx.context, w, h);
                    gfx.renderer.resize((size.width, size.height));
                    if let Err(e) = gfx.hud.resize(&mut gfx.renderer) {
                        self.error = Some(e.context("re-rendering the OSD"));
                        event_loop.exit();
                    }
                }
            }
            WindowEvent::KeyboardInput {
                event: KeyEvent { logical_key, state: ElementState::Pressed, repeat, .. },
                ..
            } => {
                if let Some(action) = map_key(&logical_key, repeat) {
                    self.on_action(action, event_loop);
                }
            }
            WindowEvent::RedrawRequested => self.redraw(),
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        // ControlFlow::Wait does not spin: on Wayland the frame callback
        // scheduled by `pre_present_notify` wakes the loop; on X11 the swap
        // itself waits for vsync. Requesting a redraw keeps frames coming.
        if let Some(gfx) = self.gfx.as_ref() {
            gfx.window.request_redraw();
        }
    }
}
```

`v2/src/display/mod.rs`:
```rust
//! Fullscreen window + GL context (winit/glutin), the render loop, and the
//! controller that runs the booth.

mod app;
pub mod controller;
mod hud;
mod renderer;

pub use app::{RunOptions, run};
pub use controller::{CameraFactory, Controller};
```

`v2/src/main.rs`:
```rust
use std::path::PathBuf;

use anyhow::Context;
use clap::Parser;
use std::time::Instant;

use pibooth::camera::{SENSOR_FPS, SENSOR_SIZE, fake::FakeCamera};
use pibooth::config::Config;
use pibooth::display::{self, CameraFactory, Controller, RunOptions};
use pibooth::input::GpioButton;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(version, about = "Raspberry Pi 5 photo booth")]
struct Cli {
    /// Path to config.toml (defaults are used when omitted).
    #[arg(long)]
    config: Option<PathBuf>,
    /// Use the animated test pattern instead of the Pi camera.
    #[arg(long)]
    fake_camera: bool,
    /// Disable the GPIO button (keyboard only).
    #[arg(long)]
    no_gpio: bool,
    /// Log display/camera frame rates every second.
    #[arg(long)]
    bench: bool,
    /// Run in a 1280x720 window instead of fullscreen.
    #[arg(long)]
    windowed: bool,
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .init();
    let cli = Cli::parse();
    let cfg = Config::load(cli.config.as_deref()).context("loading configuration")?;
    tracing::info!(upload = cfg.upload_enabled(), "configuration loaded");

    let (tx, rx) = std::sync::mpsc::channel();
    let button = if cli.no_gpio {
        None
    } else {
        match GpioButton::start(&cfg.button, tx) {
            Ok(b) => Some(b),
            Err(e) => {
                tracing::warn!("button unavailable, keyboard only: {e:#}");
                None
            }
        }
    };
    let ctrl =
        Controller::start(&cfg, camera_factory(&cli, &cfg)?, button, rx, Instant::now()).context("starting camera")?;
    display::run(
        ctrl,
        RunOptions {
            mirror: cfg.display.mirror_preview,
            bench: cli.bench,
            windowed: cli.windowed,
            assets_dir: cfg.display.assets_dir.clone(),
            countdown_max: cfg.timing.countdown_s,
        },
    )
}

fn camera_factory(cli: &Cli, cfg: &Config) -> anyhow::Result<CameraFactory> {
    if cli.fake_camera {
        return Ok(Box::new(|| Box::new(FakeCamera::new(SENSOR_SIZE, SENSOR_FPS))));
    }
    #[cfg(feature = "libcamera")]
    {
        let cam_cfg = cfg.camera.clone();
        Ok(Box::new(move || Box::new(pibooth::camera::pi::PiCamera::new(cam_cfg.clone()))))
    }
    #[cfg(not(feature = "libcamera"))]
    {
        let _ = cfg;
        anyhow::bail!("built without the `libcamera` feature; use --fake-camera or rebuild with --features libcamera")
    }
}
```

`v2/src/lib.rs` is now:
```rust
//! piBooth v2 library crate. The binary in `main.rs` wires these together.

pub mod camera;
pub mod config;
pub mod display;
pub mod frame;
pub mod geometry;
pub mod input;
pub mod monitor;
pub mod osd;
pub mod state;
pub mod stats;
```

- [ ] **Step 4: Build, lint, test (both feature sets)**

Run: `cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check`
Expected: all tests pass (78 total), clean.
Where libcamera ≥ 0.4 headers exist (the Pi, or a local build with `PKG_CONFIG_PATH` set): `cargo clippy --all-targets --features libcamera -- -D warnings` is clean.

- [ ] **Step 5: Run it and check each screen**

Headless (Linux with Xvfb, Mesa, ImageMagick `import`, `xdotool`):
```bash
Xvfb :99 -screen 0 1920x1080x24 &
DISPLAY=:99 timeout 20 cargo run -- --config config.dev.toml --fake-camera --no-gpio > run.log 2>&1 &
sleep 4 && DISPLAY=:99 import -window root idle.png
W=$(DISPLAY=:99 xdotool search --name piBooth | head -1)
DISPLAY=:99 xdotool windowfocus $W key d && sleep 1.5 && DISPLAY=:99 xdotool key space
sleep 2.2 && DISPLAY=:99 import -window root countdown.png
sleep 2.6 && DISPLAY=:99 import -window root flash.png
sleep 1 && DISPLAY=:99 import -window root review.png
DISPLAY=:99 xdotool key Escape
```
Expected (Xvfb has no window manager, so the window is 800×600 at the top-left; judge within it):
- `idle.png`: "Photo Shoot" above the centre line and "Press to start" below it, cream with a black outline.
- `countdown.png`: one large centred digit (3); debug lines in red at the top right (`Filtered: true`, `Temperature: --` on machines without a thermal zone, `Disk used: …%`).
- `flash.png`: the whole window white.
- `review.png`: the test pattern frozen (the white bar does not move between two captures 0.5 s apart).
- `run.log` contains `capture (preview frame, not saved)` and the process exits 0 on `Esc`.

- [ ] **Step 6: Commit**

```bash
git add src/display src/main.rs src/lib.rs
git commit -m "feat(v2): OSD overlay rendering and booth loop wiring"
```

### Task 8: Docs and Pi verification

**Files:**
- Modify: `v2/README.md` (replace), `docs/superpowers/specs/2026-11-18-rust-v2-design.md`

**Interfaces:**
- Consumes: the finished milestone 2 binary.
- Produces: up-to-date README and spec.

- [ ] **Step 1: README**

Replace `v2/README.md` with:
````markdown
# piBooth v2 (Rust)

Rewrite of the Python booth (`../main.py`). Design: `../docs/superpowers/specs/2026-11-18-rust-v2-design.md`.

Status: milestone 2 — live preview, button and countdown, screen flash, on-screen
display, status icons and camera auto-restart. The "photo" is still the frozen
preview frame and is not saved (capture arrives in milestone 3).

## Develop on a laptop (no Pi needed)

```bash
cargo test
cargo run -- --config config.dev.toml --fake-camera --windowed --no-gpio --bench
```

`config.dev.toml` points `display.assets_dir` at the repo root, where v1's
`fonts/` and `images/` live.

Keys: `Space` take a photo, `d` debug overlay, `r` mirror preview, `f` filter
on/off, `Esc`/`q` quit. The red square marks the image's top-left corner; with
mirroring on it appears top-right.

## Button

Same wiring as v1: a push button between GPIO 27 (BCM) and 3.3 V. v2 enables
the internal pull-down and listens for debounced press edges. For a button to
ground instead, set `[button] active_low = true` (internal pull-up).

## Build on the Raspberry Pi 5

```bash
sudo apt install -y libcamera-dev libclang-dev clang pkg-config cage
pkg-config --modversion libcamera   # must be 0.4.x .. 0.7.x for libcamera crate 0.8
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
. "$HOME/.cargo/env"
cargo build --release --features libcamera
```

Run from a text console (no desktop), from the repo root so `fonts/` and
`images/` are found:

```bash
cd .. && NO_COLOR=1 cage -s -- v2/target/release/pibooth --bench > bench.log 2>&1
```

Check `top` while it runs: pibooth should be well under one full core.

## Install as a service

```bash
sudo useradd -m -G video,render,input,gpio booth
sudo mkdir -p /opt/pibooth && sudo cp -r target/release/pibooth config.example.toml ../fonts ../effects ../images /opt/pibooth/
sudo cp /opt/pibooth/config.example.toml /opt/pibooth/config.toml
sudo chown -R booth: /opt/pibooth
sudo touch /opt/pibooth/.env && sudo chown booth: /opt/pibooth/.env && sudo chmod 600 /opt/pibooth/.env
sudo cp deploy/pibooth.service /etc/systemd/system/
sudo systemctl daemon-reload && sudo systemctl enable --now pibooth
```

The `gpio` group exists on Raspberry Pi OS; elsewhere create it first with `sudo groupadd gpio`.

The unit takes over tty1 (it stops `getty@tty1` via `Conflicts=`); use another VT, e.g. Ctrl+Alt+F2, for a console.

The Immich key goes in `/opt/pibooth/.env` as `PIBOOTH_IMMICH_API_KEY=...`
(or v1's `API_KEY=...`).
````

- [ ] **Step 2: Spec updates (record milestone 2 decisions)**

Edit `docs/superpowers/specs/2026-11-18-rust-v2-design.md`:

1. §3, the "Threads:" paragraph: replace "a tokio runtime (2 workers) for upload and monitors, and a GPIO event thread" with "plain threads for the monitors and the GPIO button (milestone 2), and a tokio runtime for upload (milestone 4)".
2. §3.1 table: `flash` row → "`Flash` trait; `ScreenFlash`, `GpioStrobe` (milestone 5; the screen flash is a view flag until then)"; `monitor` row dependencies → "std threads, nix (statvfs)"; add rows `state` ("Pure booth state machine", "none"), `osd` ("Text/icon rasterisation and scene layout, no GL", "fontdue, image").
3. §4.1, maintenance keys: remove "`b` bench overlay" (the debug overlay shows frame rates); add "Key auto-repeat is ignored; `Q` works like `q`."
4. §4.7: replace "On tokio, each publishing `StatusUpdate` to the main thread:" with "On their own threads, each updating a shared `Status` snapshot read by the render loop:"; Network row source → "TCP connect to the `immich.base_url` host:port, 5 s timeout".
5. §4.8 example: under `[display]` add `assets_dir = "."`; under `[button]` change `active_low = true` to `active_low = false   # v1 wiring: button to 3.3 V, pull-down`.

- [ ] **Step 3: Commit**

```bash
git add README.md ../docs/superpowers/specs/2026-11-18-rust-v2-design.md
git commit -m "docs(v2): milestone 2 README and spec updates"
```

- [ ] **Step 4: Verify on the Pi (user)**

From the repo root on the Pi, after `cargo build --release --features libcamera` in `v2/`:
```bash
NO_COLOR=1 cage -s -- v2/target/release/pibooth --bench > bench.log 2>&1
```
Expected:
- Log line `button ready line=GPIO27 …`; pressing the real button starts the countdown; holding it does not retrigger; a quick double press starts one countdown.
- Title, countdown digits, white flash about 0.3 s before the shot, 5 s frozen review, then live preview; after 120 s without a press the title returns.
- `d` shows the debug overlay with real temperature and disk figures.
- `bench` lines still show `camera_frames` ≥ 58 and `display_fps` ≈ 60 with the OSD visible.
- Unplugging Wi-Fi (with `immich.base_url` set) shows the network icon within ~60 s; it clears within ~10 s of reconnecting.
---

## Spec coverage (milestone 2)

| Spec item | Task |
|---|---|
| §4.1 state machine, ignored presses, idle timeout, hold doesn't retrigger | 1, 2 |
| §4.1 maintenance keys | 2, 7 |
| §4.3 OSD layers: icons, debug, countdown, title; screen flash quad | 4, 5, 7 |
| §4.5 screen flash timing (`lead_ms`, `max_ms`) | 1, 6 |
| §4.7 monitors and icons | 3, 5 |
| §4.8 config (`assets_dir`, button default) | 2 |
| §5 runtime camera error → banner, restart with backoff, state → Idle | 6, 7 |
| §2 v1 defects: gpiod v1 API / `gpiochip4`, level-polled button, blocking HTTP in loop, `vcgencmd` fork | 2, 3 |
| Milestone 1 deferred items: key repeat, uppercase `Q`, exit-on-camera-death replaced by restart | 2, 6 |

Deferred to later plans: real capture, AE/AWB lock, filters, JPEG/EXIF, saved review image (M3); upload queue and tokio (M4); stacking, DNG, GPIO strobe (M5); dmabuf, hardware trigger, bare KMS (M6).
