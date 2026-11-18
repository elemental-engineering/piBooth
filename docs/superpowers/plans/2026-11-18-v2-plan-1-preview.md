# piBooth v2 — Plan 1: Live Preview (Milestone 1)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A Rust binary that shows the IMX296 camera live, letterboxed and mirrored, fullscreen on a 1080p HDMI display under `cage`, at the sensor's 60 fps, with a `--fake-camera` mode for development and a `--bench` frame-rate report.

**Architecture:** A camera thread (libcamera, or a synthetic test pattern) copies each preview frame into a recycled buffer and publishes it to a lock-protected latest-frame mailbox. The main thread runs a winit/glutin GLES 3.0 window; on every vsync it takes the newest frame, uploads it into one streamed texture, and draws a letterboxed quad whose U coordinate is flipped for mirroring. Pure logic (config, geometry, mailbox, rate meter, test pattern) is unit-tested on x86; GL and libcamera are verified by running.

**Tech Stack:** Rust 2024 edition (rustc ≥ 1.85), libcamera 0.8 (optional `libcamera` feature), winit 0.30, glutin 0.32, glutin-winit 0.5, glow 0.18, clap 4.6, serde + toml 1.2, dotenvy 0.15, tracing 0.1, anyhow 1.

**Spec:** `docs/superpowers/specs/2026-11-18-rust-v2-design.md` (this plan covers §8 milestone 1; later milestones get their own plans).

## Global Constraints

- Crate `pibooth` lives in `v2/` at the repo root; v1 (`main.py`) is not modified.
- Target: Raspberry Pi 5, IMX296 colour, 1456×1088, 60 fps; 1920×1080 HDMI; Wayland kiosk `cage`.
- Preview mirroring happens only in the shader (spec §4.3); pixel data is never flipped.
- Letterbox uses `min(w ratio, h ratio)` (spec §2, fixes `main.py:175`).
- The render loop never blocks on the camera: the camera thread overwrites unread frames (spec §3).
- Camera preview pixel format: libcamera `BGR888` (fourcc `BG24`, memory order R,G,B), uploaded as GL `RGB`.
- Frame duration fixed to 1/60 s via `FrameDurationLimits` (spec §4.2; fixes `main.py:169`).
- API key from `PIBOOTH_IMMICH_API_KEY`, falling back to `API_KEY` in `.env` (spec §4.8).
- `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and `cargo test` pass on x86 **without** the `libcamera` feature.
- Commit messages end with the project's attribution trailer lines.

## File Structure

| File | Responsibility |
|---|---|
| `v2/Cargo.toml` | Crate manifest; `libcamera` optional feature |
| `v2/rustfmt.toml` | Formatting (120 cols) |
| `v2/config.example.toml` | Documented default configuration (spec §4.8) |
| `v2/src/lib.rs` | Module list |
| `v2/src/main.rs` | CLI, logging, wiring camera → display |
| `v2/src/config.rs` | `Config` schema, defaults, validation, API key lookup |
| `v2/src/geometry.rs` | `letterbox`, `quad_vertices` |
| `v2/src/frame.rs` | `Frame`, `FrameMailbox`, `copy_packed` |
| `v2/src/stats.rs` | `RateMeter` for `--bench` |
| `v2/src/camera/mod.rs` | `Camera` trait, sensor constants |
| `v2/src/camera/fake.rs` | `FakeCamera` test pattern |
| `v2/src/camera/pi.rs` | `PiCamera` (libcamera), feature-gated |
| `v2/src/display/mod.rs` | Public `run`/`RunOptions` |
| `v2/src/display/renderer.rs` | GLES 3 texture + quad renderer |
| `v2/src/display/app.rs` | winit `ApplicationHandler`, GL context, vsync loop, keys, bench log |
| `v2/deploy/pibooth.service` | systemd unit running `cage` |
| `v2/README.md` | Dev, Pi build, install instructions |
| `.github/workflows/v2.yml` | CI: fmt, clippy, test |
| `.gitignore` | add `v2/target/` |

All commands below run from `v2/` unless stated.

---
### Task 1: Crate scaffold, configuration and CI

**Files:**
- Create: `v2/Cargo.toml`, `v2/rustfmt.toml`, `v2/config.example.toml`, `v2/src/lib.rs`, `v2/src/main.rs`, `v2/src/config.rs`, `.github/workflows/v2.yml`
- Modify: `.gitignore` (append `v2/target/`)
- Test: inline `#[cfg(test)] mod tests` in `v2/src/config.rs`

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `pub struct Config { storage: Storage, camera: CameraCfg, capture: Capture, timing: Timing, display: Display, filter: Filter, flash: Flash, button: Button, immich: Immich, monitor: Monitor, immich_api_key: Option<String> }` (all `pub`, `Clone + PartialEq + Debug + Default`)
  - `pub struct CameraCfg { pub tuning_file: String }`, `pub struct Display { pub mirror_preview: bool }`
  - `Config::from_toml_str(&str) -> anyhow::Result<Config>`, `Config::load(Option<&Path>) -> anyhow::Result<Config>`, `Config::upload_enabled(&self) -> bool`
  - `Immich::gps(&self) -> Option<(f64, f64)>`, `pub fn api_key_from(get: impl Fn(&str) -> Option<String>) -> Option<String>`
  - enums `StackMode { Average, Sharpest, Off }`, `FlashMode { Screen, Strobe, None }` (lowercase in TOML)

- [ ] **Step 1: Create the manifest, formatter config, gitignore entry and CI workflow**

`v2/Cargo.toml`:
```toml
[package]
name = "pibooth"
version = "2.0.0-alpha.1"
edition = "2024"
rust-version = "1.85"
description = "Raspberry Pi 5 photo booth, v2"

[features]
default = []
# Real Pi camera support. Requires libcamera-dev (>= 0.4) at build time.
libcamera = ["dep:libcamera"]

[dependencies]
anyhow = "1.0"
clap = { version = "4.6", features = ["derive"] }
dotenvy = "0.15"
glow = "0.18"
glutin = "0.32"
glutin-winit = "0.5"
libcamera = { version = "0.8", optional = true }
raw-window-handle = "0.6"
serde = { version = "1.0", features = ["derive"] }
toml = "1.2"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
winit = "0.30"

[dev-dependencies]
tempfile = "3"

[profile.release]
lto = "thin"
codegen-units = 1
```

`v2/rustfmt.toml`:
```toml
max_width = 120
use_small_heuristics = "Max"
```

Append to the repo-root `.gitignore`:
```text
v2/target/
```

`.github/workflows/v2.yml`:
```yaml
name: v2

on:
  push:
    paths: ["v2/**", ".github/workflows/v2.yml"]
  pull_request:
    paths: ["v2/**", ".github/workflows/v2.yml"]

jobs:
  test:
    runs-on: ubuntu-latest
    defaults:
      run:
        working-directory: v2
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: clippy, rustfmt
      - run: cargo fmt --check
      - run: cargo clippy --all-targets -- -D warnings
      - run: cargo test
```

`v2/config.example.toml`:
```toml
# piBooth v2 configuration. Every key is optional; values shown are defaults.
# The Immich API key is NOT stored here: set PIBOOTH_IMMICH_API_KEY, or
# API_KEY in a .env file next to the binary (v1 compatible).

[storage]
root = "."            # photos/, raw/, queue/ are created under here
save_original = true  # keep the unfiltered photo in raw/

[camera]
tuning_file = ""      # empty = libcamera default (imx296.json)

[capture]
burst_frames = 6
stack = "average"     # average | sharpest | off
max_shift_px = 8
save_dng = false
jpeg_quality = 95

[timing]
countdown_s = 5
review_s = 5
idle_s = 120

[display]
mirror_preview = true # preview only; saved photos are never mirrored

[filter]
enabled = true

[flash]
mode = "screen"       # screen | strobe | none
lead_ms = 300
max_ms = 800
gpio_line = 17
pulse_ms = 150
active_high = true

[button]
gpio_line = 27
active_low = true
debounce_ms = 30

[immich]
base_url = ""         # e.g. "https://immich.example/api"
album_id = ""
latitude = 0.0        # GPS omitted when both are 0.0
longitude = 0.0

[monitor]
temp_warn_c = 60
temp_crit_c = 65
disk_warn_pct = 70
disk_crit_pct = 90
```

`v2/src/lib.rs`:
```rust
//! piBooth v2 library crate. The binary in `main.rs` wires these together.

pub mod config;
```

`v2/src/main.rs` (temporary; Task 5 replaces it):
```rust
use std::path::PathBuf;

use anyhow::Context;
use clap::Parser;
use pibooth::config::Config;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(version, about = "Raspberry Pi 5 photo booth")]
struct Cli {
    /// Path to config.toml (defaults are used when omitted).
    #[arg(long)]
    config: Option<PathBuf>,
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .init();
    let cli = Cli::parse();
    let cfg = Config::load(cli.config.as_deref()).context("loading configuration")?;
    tracing::info!(upload = cfg.upload_enabled(), "configuration loaded");
    Ok(())
}
```

- [ ] **Step 2: Write the failing tests**

Create `v2/src/config.rs` containing the type definitions from the Interfaces block with every function body `todo!()` (and `Default` impls returning the defaults from `config.example.toml`), followed by this test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_file_gives_spec_defaults() {
        let cfg = Config::from_toml_str("").unwrap();
        assert_eq!(cfg, Config::default());
        assert_eq!(cfg.capture.burst_frames, 6);
        assert_eq!(cfg.capture.stack, StackMode::Average);
        assert_eq!(cfg.timing.countdown_s, 5);
        assert_eq!(cfg.flash.mode, FlashMode::Screen);
        assert_eq!(cfg.button.gpio_line, 27);
        assert!(cfg.display.mirror_preview);
    }

    #[test]
    fn partial_section_keeps_other_defaults() {
        let cfg = Config::from_toml_str("[capture]\nburst_frames = 3\nstack = \"sharpest\"\n").unwrap();
        assert_eq!(cfg.capture.burst_frames, 3);
        assert_eq!(cfg.capture.stack, StackMode::Sharpest);
        assert_eq!(cfg.capture.jpeg_quality, 95);
    }

    #[test]
    fn example_config_parses() {
        let text = include_str!("../config.example.toml");
        Config::from_toml_str(text).unwrap();
    }

    #[test]
    fn unknown_key_is_rejected() {
        assert!(Config::from_toml_str("[capture]\nburst = 3\n").is_err());
    }

    #[test]
    fn invalid_values_are_rejected() {
        assert!(Config::from_toml_str("[capture]\nburst_frames = 0\n").is_err());
        assert!(Config::from_toml_str("[capture]\njpeg_quality = 0\n").is_err());
        assert!(Config::from_toml_str("[flash]\nmode = \"strobe\"\ngpio_line = 27\n").is_err());
    }

    #[test]
    fn gps_omitted_when_zero() {
        let mut i = Immich::default();
        assert_eq!(i.gps(), None);
        i.latitude = 48.85;
        i.longitude = 2.35;
        assert_eq!(i.gps(), Some((48.85, 2.35)));
    }

    #[test]
    fn api_key_precedence() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| v.to_string())
        };
        assert_eq!(api_key_from(env(&[("API_KEY", "v1")])), Some("v1".into()));
        assert_eq!(api_key_from(env(&[("API_KEY", "v1"), ("PIBOOTH_IMMICH_API_KEY", "v2")])), Some("v2".into()));
        assert_eq!(api_key_from(env(&[("PIBOOTH_IMMICH_API_KEY", "  "), ("API_KEY", "v1")])), Some("v1".into()));
        assert_eq!(api_key_from(env(&[])), None);
    }

    #[test]
    fn load_missing_path_errors_and_none_gives_defaults() {
        assert!(Config::load(Some(Path::new("/nonexistent/config.toml"))).is_err());
        let cfg = Config::load(None).unwrap();
        assert_eq!(cfg.capture, Capture::default());
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test config::`
Expected: FAIL — panics with `not yet implemented`.

- [ ] **Step 4: Write the implementation**

Replace `v2/src/config.rs` with the complete module:

```rust
//! Booth configuration: `config.toml` plus the Immich API key from the
//! environment (`PIBOOTH_IMMICH_API_KEY`, or `API_KEY` in `.env` for v1
//! compatibility).

use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub storage: Storage,
    pub camera: CameraCfg,
    pub capture: Capture,
    pub timing: Timing,
    pub display: Display,
    pub filter: Filter,
    pub flash: Flash,
    pub button: Button,
    pub immich: Immich,
    pub monitor: Monitor,
    /// Never read from the TOML file; filled from the environment.
    #[serde(skip)]
    pub immich_api_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Storage {
    pub root: PathBuf,
    pub save_original: bool,
}
impl Default for Storage {
    fn default() -> Self {
        Self { root: PathBuf::from("."), save_original: true }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct CameraCfg {
    /// Empty = libcamera default tuning file.
    pub tuning_file: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StackMode {
    Average,
    Sharpest,
    Off,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Capture {
    pub burst_frames: u32,
    pub stack: StackMode,
    pub max_shift_px: u32,
    pub save_dng: bool,
    pub jpeg_quality: u8,
}
impl Default for Capture {
    fn default() -> Self {
        Self { burst_frames: 6, stack: StackMode::Average, max_shift_px: 8, save_dng: false, jpeg_quality: 95 }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Timing {
    pub countdown_s: u32,
    pub review_s: u32,
    pub idle_s: u32,
}
impl Default for Timing {
    fn default() -> Self {
        Self { countdown_s: 5, review_s: 5, idle_s: 120 }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Display {
    pub mirror_preview: bool,
}
impl Default for Display {
    fn default() -> Self {
        Self { mirror_preview: true }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Filter {
    pub enabled: bool,
}
impl Default for Filter {
    fn default() -> Self {
        Self { enabled: true }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FlashMode {
    Screen,
    Strobe,
    None,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Flash {
    pub mode: FlashMode,
    pub lead_ms: u32,
    pub max_ms: u32,
    pub gpio_line: u32,
    pub pulse_ms: u32,
    pub active_high: bool,
}
impl Default for Flash {
    fn default() -> Self {
        Self { mode: FlashMode::Screen, lead_ms: 300, max_ms: 800, gpio_line: 17, pulse_ms: 150, active_high: true }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Button {
    pub gpio_line: u32,
    pub active_low: bool,
    pub debounce_ms: u32,
}
impl Default for Button {
    fn default() -> Self {
        Self { gpio_line: 27, active_low: true, debounce_ms: 30 }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct Immich {
    pub base_url: String,
    pub album_id: String,
    pub latitude: f64,
    pub longitude: f64,
}
impl Immich {
    /// GPS is omitted when both coordinates are 0.0 (spec §4.8).
    pub fn gps(&self) -> Option<(f64, f64)> {
        (self.latitude != 0.0 || self.longitude != 0.0).then_some((self.latitude, self.longitude))
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Monitor {
    pub temp_warn_c: f32,
    pub temp_crit_c: f32,
    pub disk_warn_pct: f32,
    pub disk_crit_pct: f32,
}
impl Default for Monitor {
    fn default() -> Self {
        Self { temp_warn_c: 60.0, temp_crit_c: 65.0, disk_warn_pct: 70.0, disk_crit_pct: 90.0 }
    }
}

impl Config {
    /// Parse TOML text and validate. Does not touch the environment.
    pub fn from_toml_str(text: &str) -> anyhow::Result<Self> {
        let cfg: Config = toml::from_str(text).context("invalid config.toml")?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Load `path` (missing file = all defaults), then the API key from the
    /// environment, reading `.env` from the current directory if present.
    pub fn load(path: Option<&Path>) -> anyhow::Result<Self> {
        let mut cfg = match path {
            Some(p) => {
                let text = std::fs::read_to_string(p).with_context(|| format!("reading {}", p.display()))?;
                Self::from_toml_str(&text)?
            }
            None => Self::default(),
        };
        let _ = dotenvy::dotenv();
        cfg.immich_api_key = api_key_from(|k| std::env::var(k).ok());
        Ok(cfg)
    }

    fn validate(&self) -> anyhow::Result<()> {
        if self.capture.burst_frames == 0 {
            bail!("capture.burst_frames must be >= 1");
        }
        if !(1..=100).contains(&self.capture.jpeg_quality) {
            bail!("capture.jpeg_quality must be 1..=100");
        }
        if self.timing.countdown_s == 0 {
            bail!("timing.countdown_s must be >= 1");
        }
        if self.button.gpio_line == self.flash.gpio_line && self.flash.mode == FlashMode::Strobe {
            bail!("button.gpio_line and flash.gpio_line must differ");
        }
        Ok(())
    }

    /// Upload is possible only with both a key and a server URL.
    pub fn upload_enabled(&self) -> bool {
        self.immich_api_key.is_some() && !self.immich.base_url.is_empty()
    }
}

/// `PIBOOTH_IMMICH_API_KEY` wins over v1's `API_KEY`; empty values count as unset.
pub fn api_key_from(get: impl Fn(&str) -> Option<String>) -> Option<String> {
    ["PIBOOTH_IMMICH_API_KEY", "API_KEY"]
        .iter()
        .filter_map(|k| get(k))
        .map(|v| v.trim().to_string())
        .find(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_file_gives_spec_defaults() {
        let cfg = Config::from_toml_str("").unwrap();
        assert_eq!(cfg, Config::default());
        assert_eq!(cfg.capture.burst_frames, 6);
        assert_eq!(cfg.capture.stack, StackMode::Average);
        assert_eq!(cfg.timing.countdown_s, 5);
        assert_eq!(cfg.flash.mode, FlashMode::Screen);
        assert_eq!(cfg.button.gpio_line, 27);
        assert!(cfg.display.mirror_preview);
    }

    #[test]
    fn partial_section_keeps_other_defaults() {
        let cfg = Config::from_toml_str("[capture]\nburst_frames = 3\nstack = \"sharpest\"\n").unwrap();
        assert_eq!(cfg.capture.burst_frames, 3);
        assert_eq!(cfg.capture.stack, StackMode::Sharpest);
        assert_eq!(cfg.capture.jpeg_quality, 95);
    }

    #[test]
    fn example_config_parses() {
        let text = include_str!("../config.example.toml");
        Config::from_toml_str(text).unwrap();
    }

    #[test]
    fn unknown_key_is_rejected() {
        assert!(Config::from_toml_str("[capture]\nburst = 3\n").is_err());
    }

    #[test]
    fn invalid_values_are_rejected() {
        assert!(Config::from_toml_str("[capture]\nburst_frames = 0\n").is_err());
        assert!(Config::from_toml_str("[capture]\njpeg_quality = 0\n").is_err());
        assert!(Config::from_toml_str("[flash]\nmode = \"strobe\"\ngpio_line = 27\n").is_err());
    }

    #[test]
    fn gps_omitted_when_zero() {
        let mut i = Immich::default();
        assert_eq!(i.gps(), None);
        i.latitude = 48.85;
        i.longitude = 2.35;
        assert_eq!(i.gps(), Some((48.85, 2.35)));
    }

    #[test]
    fn api_key_precedence() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| v.to_string())
        };
        assert_eq!(api_key_from(env(&[("API_KEY", "v1")])), Some("v1".into()));
        assert_eq!(api_key_from(env(&[("API_KEY", "v1"), ("PIBOOTH_IMMICH_API_KEY", "v2")])), Some("v2".into()));
        assert_eq!(api_key_from(env(&[("PIBOOTH_IMMICH_API_KEY", "  "), ("API_KEY", "v1")])), Some("v1".into()));
        assert_eq!(api_key_from(env(&[])), None);
    }

    #[test]
    fn load_missing_path_errors_and_none_gives_defaults() {
        assert!(Config::load(Some(Path::new("/nonexistent/config.toml"))).is_err());
        let cfg = Config::load(None).unwrap();
        assert_eq!(cfg.capture, Capture::default());
    }
}
```

- [ ] **Step 5: Run tests, lint, format check**

Run: `cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check && cargo run -- --config config.example.toml`
Expected: `8 passed`; no clippy warnings; run logs `configuration loaded upload=false` and exits 0.

- [ ] **Step 6: Commit**

```bash
git -C .. add .gitignore .github/workflows/v2.yml v2/Cargo.toml v2/Cargo.lock v2/rustfmt.toml v2/config.example.toml v2/src
git commit -m "feat(v2): scaffold Rust crate with configuration and CI"
```

### Task 2: Letterbox and quad geometry

**Files:**
- Create: `v2/src/geometry.rs`
- Modify: `v2/src/lib.rs` (add `pub mod geometry;`)
- Test: inline in `v2/src/geometry.rs`

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub struct Rect { pub x: i32, pub y: i32, pub w: u32, pub h: u32 }`
  - `pub fn letterbox(src: (u32, u32), screen: (u32, u32)) -> Rect`
  - `pub fn quad_vertices(rect: Rect, screen: (u32, u32), mirror: bool) -> [f32; 16]` — triangle strip `[x, y, u, v]` × 4: top-left, bottom-left, top-right, bottom-right; v=0 at the top.

- [ ] **Step 1: Write the failing tests**

Create `v2/src/geometry.rs` with `Rect` and the two functions as `todo!()`, plus:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imx296_on_1080p_is_pillarboxed() {
        let r = letterbox((1456, 1088), (1920, 1080));
        assert_eq!(r, Rect { x: 237, y: 0, w: 1445, h: 1080 });
    }

    #[test]
    fn small_screen_never_overflows() {
        // v1 overflowed here (fit-by-width gave 800x597 on a 480px screen).
        let r = letterbox((1456, 1088), (800, 480));
        assert!(r.w <= 800 && r.h <= 480, "{r:?}");
        assert_eq!(r.h, 480);
    }

    #[test]
    fn portrait_screen_is_letterboxed() {
        let r = letterbox((1456, 1088), (1080, 1920));
        assert_eq!(r.w, 1080);
        assert!(r.h < 1920);
        assert_eq!(r.x, 0);
        assert_eq!(r.y as u32, (1920 - r.h) / 2);
    }

    #[test]
    fn same_aspect_fills_screen() {
        assert_eq!(letterbox((640, 480), (1280, 960)), Rect { x: 0, y: 0, w: 1280, h: 960 });
    }

    #[test]
    fn full_screen_quad_spans_clip_space() {
        let v = quad_vertices(Rect { x: 0, y: 0, w: 100, h: 50 }, (100, 50), false);
        assert_eq!(&v[0..4], &[-1.0, 1.0, 0.0, 0.0]);
        assert_eq!(&v[12..16], &[1.0, -1.0, 1.0, 1.0]);
    }

    #[test]
    fn mirror_swaps_u_only() {
        let r = Rect { x: 10, y: 0, w: 80, h: 50 };
        let a = quad_vertices(r, (100, 50), false);
        let b = quad_vertices(r, (100, 50), true);
        for i in 0..4 {
            assert_eq!(a[i * 4], b[i * 4], "x unchanged");
            assert_eq!(a[i * 4 + 1], b[i * 4 + 1], "y unchanged");
            assert_eq!(a[i * 4 + 2], 1.0 - b[i * 4 + 2], "u flipped");
            assert_eq!(a[i * 4 + 3], b[i * 4 + 3], "v unchanged");
        }
    }
}
```

Add `pub mod geometry;` to `v2/src/lib.rs`.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test geometry::`
Expected: FAIL — `not yet implemented`.

- [ ] **Step 3: Write the implementation**

Replace `v2/src/geometry.rs` with:

```rust
//! Screen geometry: letterboxing the camera image and building the quad the
//! renderer draws. Pure functions, no GL.

/// Integer pixel rectangle, origin top-left.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

/// Largest rectangle with the source aspect ratio that fits in the screen,
/// centred. Uses ratios, not pixel deltas (fixes v1 `main.py:175`).
pub fn letterbox(src: (u32, u32), screen: (u32, u32)) -> Rect {
    let (sw, sh) = (src.0 as f64, src.1 as f64);
    let (dw, dh) = (screen.0 as f64, screen.1 as f64);
    let scale = (dw / sw).min(dh / sh);
    let w = (sw * scale).round() as u32;
    let h = (sh * scale).round() as u32;
    let w = w.min(screen.0);
    let h = h.min(screen.1);
    Rect { x: ((screen.0 - w) / 2) as i32, y: ((screen.1 - h) / 2) as i32, w, h }
}

/// Triangle-strip vertices `[x, y, u, v] * 4` in GL clip space for `rect`
/// on a `screen`-sized viewport. Order: top-left, bottom-left, top-right,
/// bottom-right. Texture row 0 is the top of the image, so v=0 at the top.
/// `mirror` flips U, giving a selfie view without touching pixel data.
pub fn quad_vertices(rect: Rect, screen: (u32, u32), mirror: bool) -> [f32; 16] {
    let (sw, sh) = (screen.0 as f32, screen.1 as f32);
    let x0 = rect.x as f32 / sw * 2.0 - 1.0;
    let x1 = (rect.x as f32 + rect.w as f32) / sw * 2.0 - 1.0;
    let y0 = 1.0 - rect.y as f32 / sh * 2.0; // top
    let y1 = 1.0 - (rect.y as f32 + rect.h as f32) / sh * 2.0; // bottom
    let (u0, u1) = if mirror { (1.0, 0.0) } else { (0.0, 1.0) };
    [
        x0, y0, u0, 0.0, //
        x0, y1, u0, 1.0, //
        x1, y0, u1, 0.0, //
        x1, y1, u1, 1.0,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imx296_on_1080p_is_pillarboxed() {
        let r = letterbox((1456, 1088), (1920, 1080));
        assert_eq!(r, Rect { x: 237, y: 0, w: 1445, h: 1080 });
    }

    #[test]
    fn small_screen_never_overflows() {
        // v1 overflowed here (fit-by-width gave 800x597 on a 480px screen).
        let r = letterbox((1456, 1088), (800, 480));
        assert!(r.w <= 800 && r.h <= 480, "{r:?}");
        assert_eq!(r.h, 480);
    }

    #[test]
    fn portrait_screen_is_letterboxed() {
        let r = letterbox((1456, 1088), (1080, 1920));
        assert_eq!(r.w, 1080);
        assert!(r.h < 1920);
        assert_eq!(r.x, 0);
        assert_eq!(r.y as u32, (1920 - r.h) / 2);
    }

    #[test]
    fn same_aspect_fills_screen() {
        assert_eq!(letterbox((640, 480), (1280, 960)), Rect { x: 0, y: 0, w: 1280, h: 960 });
    }

    #[test]
    fn full_screen_quad_spans_clip_space() {
        let v = quad_vertices(Rect { x: 0, y: 0, w: 100, h: 50 }, (100, 50), false);
        assert_eq!(&v[0..4], &[-1.0, 1.0, 0.0, 0.0]);
        assert_eq!(&v[12..16], &[1.0, -1.0, 1.0, 1.0]);
    }

    #[test]
    fn mirror_swaps_u_only() {
        let r = Rect { x: 10, y: 0, w: 80, h: 50 };
        let a = quad_vertices(r, (100, 50), false);
        let b = quad_vertices(r, (100, 50), true);
        for i in 0..4 {
            assert_eq!(a[i * 4], b[i * 4], "x unchanged");
            assert_eq!(a[i * 4 + 1], b[i * 4 + 1], "y unchanged");
            assert_eq!(a[i * 4 + 2], 1.0 - b[i * 4 + 2], "u flipped");
            assert_eq!(a[i * 4 + 3], b[i * 4 + 3], "v unchanged");
        }
    }
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test geometry:: && cargo clippy --all-targets -- -D warnings`
Expected: `6 passed`. Note `imx296_on_1080p_is_pillarboxed` pins the 1080p layout to 1445×1080 at x=237.

- [ ] **Step 5: Commit**

```bash
git add src/geometry.rs src/lib.rs
git commit -m "feat(v2): letterbox and mirrored quad geometry"
```

### Task 3: Frame mailbox, row packing and rate meter

**Files:**
- Create: `v2/src/frame.rs`, `v2/src/stats.rs`
- Modify: `v2/src/lib.rs` (add `pub mod frame;` and `pub mod stats;`)
- Test: inline in both files

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `pub struct Frame { pub width: u32, pub height: u32, pub data: Vec<u8>, pub seq: u64, pub captured_at: Instant }` — tightly packed RGB, row 0 at top; `Frame::byte_len(w, h) -> usize`
  - `pub struct FrameMailbox` with `new() -> Arc<Self>`, `take_buffer(&self, len) -> Vec<u8>`, `publish(&self, Frame)`, `take_latest(&self) -> Option<Frame>`, `recycle(&self, Frame)`, `counters(&self) -> (u64 published, u64 overwritten)`
  - `pub fn copy_packed(src: &[u8], stride: usize, row_bytes: usize, height: usize, dst: &mut [u8])`
  - `pub struct RateMeter` with `new(window: Duration, now: Instant)`, `tick(&mut self)`, `poll(&mut self, now: Instant) -> Option<f64>`

- [ ] **Step 1: Write the failing tests**

Create both files with the Interfaces items stubbed with `todo!()` (for `FrameMailbox`, keep the private `Inner` struct and `MAX_SPARES` const from Step 3 since the tests read them), and these test modules.

`v2/src/frame.rs` tests:
```rust
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
```

`v2/src/stats.rs` tests:
```rust
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
```

Add `pub mod frame;` and `pub mod stats;` to `v2/src/lib.rs`.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test frame:: stats::` (run each filter separately if your cargo rejects two)
Expected: FAIL — `not yet implemented`.

- [ ] **Step 3: Write the implementation**

`v2/src/frame.rs`:
```rust
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
```

`v2/src/stats.rs`:
```rust
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
```

- [ ] **Step 4: Run tests**

Run: `cargo test && cargo clippy --all-targets -- -D warnings`
Expected: all pass (7 new tests). `writer_and_reader_on_different_threads` must finish in well under a second.

- [ ] **Step 5: Commit**

```bash
git add src/frame.rs src/stats.rs src/lib.rs
git commit -m "feat(v2): latest-frame mailbox, stride packing and rate meter"
```

### Task 4: Camera trait and fake test-pattern camera

**Files:**
- Create: `v2/src/camera/mod.rs`, `v2/src/camera/fake.rs`
- Modify: `v2/src/lib.rs` (add `pub mod camera;`)
- Test: inline in `v2/src/camera/fake.rs`

**Interfaces:**
- Consumes: `Frame`, `FrameMailbox` (Task 3).
- Produces:
  - `pub const SENSOR_SIZE: (u32, u32) = (1456, 1088); pub const SENSOR_FPS: u32 = 60;`
  - `pub trait Camera { fn start(&mut self, mailbox: Arc<FrameMailbox>) -> anyhow::Result<()>; fn stop(&mut self); }` — `stop` joins the thread and is idempotent
  - `pub struct FakeCamera` with `new(size: (u32, u32), fps: u32)`, implementing `Camera` and `Drop`
  - `pub fn fill_test_pattern(buf: &mut [u8], width: u32, height: u32, seq: u64)` — grey gradient, white bar at `x = seq*8 % width` (±3 px), red square of side `height/8` at top-left

- [ ] **Step 1: Write `camera/mod.rs` and the failing tests**

`v2/src/camera/mod.rs` (Task 6 adds the feature-gated `pi` module):
```rust
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
```

Create `v2/src/camera/fake.rs` with `FakeCamera`/`fill_test_pattern` stubbed as `todo!()` and these tests:

```rust
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
```

Add `pub mod camera;` to `v2/src/lib.rs`.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test camera::`
Expected: FAIL — `not yet implemented`.

- [ ] **Step 3: Write the implementation**

`v2/src/camera/fake.rs`:
```rust
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
            let _ = t.join();
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
```

- [ ] **Step 4: Run tests**

Run: `cargo test && cargo clippy --all-targets -- -D warnings`
Expected: all pass (3 new tests).

- [ ] **Step 5: Commit**

```bash
git add src/camera src/lib.rs
git commit -m "feat(v2): camera trait and fake test-pattern camera"
```

### Task 5: GL display, render loop and CLI wiring

GL and windowing cannot be unit-tested headlessly; the pure parts they rely on (geometry, mailbox, rate meter) are already tested. This task is verified by running against a real or virtual display.

**Files:**
- Create: `v2/src/display/mod.rs`, `v2/src/display/renderer.rs`, `v2/src/display/app.rs`
- Modify: `v2/src/lib.rs` (add `pub mod display;`), replace `v2/src/main.rs`

**Interfaces:**
- Consumes: `letterbox`, `quad_vertices` (Task 2); `Frame`, `FrameMailbox`, `RateMeter` (Task 3); `Camera`, `FakeCamera`, `SENSOR_SIZE`, `SENSOR_FPS` (Task 4); `Config` (Task 1).
- Produces:
  - `pub struct RunOptions { pub mirror: bool, pub bench: bool, pub windowed: bool }`
  - `pub fn run(mailbox: Arc<FrameMailbox>, opts: RunOptions) -> anyhow::Result<()>` — blocks on the main thread until Esc/`q`/close
  - CLI: `pibooth [--config PATH] [--fake-camera] [--no-gpio] [--bench] [--windowed]`
  - Bench log line every second: `bench display_fps=… upload_fps=… camera_frames=… skipped=…`

- [ ] **Step 1: Write the renderer**

`v2/src/display/renderer.rs`:
```rust
//! OpenGL ES 3 renderer: one streamed RGB texture drawn as a letterboxed,
//! optionally mirrored quad. All GL calls happen on the render thread.

use anyhow::{Context, anyhow};
use glow::HasContext;

use crate::frame::Frame;
use crate::geometry::{letterbox, quad_vertices};

const VERTEX_SHADER: &str = r#"#version 300 es
layout(location = 0) in vec2 a_pos;
layout(location = 1) in vec2 a_uv;
out vec2 v_uv;
void main() {
    v_uv = a_uv;
    gl_Position = vec4(a_pos, 0.0, 1.0);
}
"#;

const FRAGMENT_SHADER: &str = r#"#version 300 es
precision mediump float;
in vec2 v_uv;
uniform sampler2D u_tex;
out vec4 o_color;
void main() {
    o_color = vec4(texture(u_tex, v_uv).rgb, 1.0);
}
"#;

pub struct Renderer {
    gl: glow::Context,
    program: glow::Program,
    vao: glow::VertexArray,
    vbo: glow::Buffer,
    texture: glow::Texture,
    tex_size: Option<(u32, u32)>,
    screen: (u32, u32),
    mirror: bool,
    quad_dirty: bool,
}

impl Renderer {
    /// # Safety
    /// `gl` must be current on this thread for the renderer's whole life.
    pub unsafe fn new(gl: glow::Context, screen: (u32, u32), mirror: bool) -> anyhow::Result<Self> {
        unsafe {
            let program = link_program(&gl, VERTEX_SHADER, FRAGMENT_SHADER)?;
            let vao = gl.create_vertex_array().map_err(|e| anyhow!(e))?;
            let vbo = gl.create_buffer().map_err(|e| anyhow!(e))?;
            gl.bind_vertex_array(Some(vao));
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
            gl.buffer_data_size(glow::ARRAY_BUFFER, 16 * 4, glow::DYNAMIC_DRAW);
            gl.enable_vertex_attrib_array(0);
            gl.vertex_attrib_pointer_f32(0, 2, glow::FLOAT, false, 16, 0);
            gl.enable_vertex_attrib_array(1);
            gl.vertex_attrib_pointer_f32(1, 2, glow::FLOAT, false, 16, 8);

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
            // Rows are tightly packed RGB; 1456*3 is not a multiple of 4.
            gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);

            gl.use_program(Some(program));
            let loc = gl.get_uniform_location(program, "u_tex");
            gl.uniform_1_i32(loc.as_ref(), 0);
            gl.clear_color(0.0, 0.0, 0.0, 1.0);

            Ok(Self { gl, program, vao, vbo, texture, tex_size: None, screen, mirror, quad_dirty: true })
        }
    }

    pub fn resize(&mut self, screen: (u32, u32)) {
        if screen != self.screen {
            self.screen = screen;
            self.quad_dirty = true;
        }
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

    pub fn draw(&mut self) {
        let gl = &self.gl;
        unsafe {
            gl.viewport(0, 0, self.screen.0 as i32, self.screen.1 as i32);
            gl.clear(glow::COLOR_BUFFER_BIT);
            let Some(tex_size) = self.tex_size else { return };
            gl.use_program(Some(self.program));
            gl.bind_vertex_array(Some(self.vao));
            if self.quad_dirty {
                let rect = letterbox(tex_size, self.screen);
                let verts = quad_vertices(rect, self.screen, self.mirror);
                gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.vbo));
                gl.buffer_sub_data_u8_slice(glow::ARRAY_BUFFER, 0, as_bytes(&verts));
                self.quad_dirty = false;
            }
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(self.texture));
            gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
        }
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        unsafe {
            self.gl.delete_texture(self.texture);
            self.gl.delete_buffer(self.vbo);
            self.gl.delete_vertex_array(self.vao);
            self.gl.delete_program(self.program);
        }
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

- [ ] **Step 2: Write the window/app loop**

`v2/src/display/mod.rs`:
```rust
//! Fullscreen window + GL context (winit/glutin) and the render loop.

mod app;
mod renderer;

pub use app::{RunOptions, run};
```

`v2/src/display/app.rs`:
```rust
//! winit application: creates the fullscreen GLES window, and on every
//! redraw uploads the newest camera frame and presents it with vsync.

use std::ffi::CString;
use std::num::NonZeroU32;
use std::sync::Arc;
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
use winit::keyboard::{Key, NamedKey};
use winit::window::{Fullscreen, Window, WindowId};

use super::renderer::Renderer;
use crate::frame::FrameMailbox;
use crate::stats::RateMeter;

pub struct RunOptions {
    pub mirror: bool,
    pub bench: bool,
    /// Windowed 1280x720 instead of fullscreen (development on a desktop).
    pub windowed: bool,
}

struct Gfx {
    // Field order matters: renderer (GL objects) drops before the context.
    renderer: Renderer,
    surface: Surface<WindowSurface>,
    context: PossiblyCurrentContext,
    window: Window,
}

struct App {
    opts: RunOptions,
    mailbox: Arc<FrameMailbox>,
    gfx: Option<Gfx>,
    error: Option<anyhow::Error>,
    display_rate: RateMeter,
    upload_rate: RateMeter,
    last_counters: (u64, u64),
}

/// Run the render loop on the calling (main) thread until Esc/q or close.
pub fn run(mailbox: Arc<FrameMailbox>, opts: RunOptions) -> anyhow::Result<()> {
    let event_loop = EventLoop::new().context("creating event loop")?;
    event_loop.set_control_flow(ControlFlow::Poll);
    let now = Instant::now();
    let mut app = App {
        opts,
        mailbox,
        gfx: None,
        error: None,
        display_rate: RateMeter::new(Duration::from_secs(1), now),
        upload_rate: RateMeter::new(Duration::from_secs(1), now),
        last_counters: (0, 0),
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
        let renderer = unsafe { Renderer::new(gl, (size.width, size.height), self.opts.mirror) }?;
        tracing::info!(width = size.width, height = size.height, "display ready");
        Ok(Gfx { renderer, surface, context, window })
    }

    fn redraw(&mut self) {
        let Some(gfx) = self.gfx.as_mut() else { return };
        if let Some(frame) = self.mailbox.take_latest() {
            gfx.renderer.upload(&frame);
            self.mailbox.recycle(frame);
            self.upload_rate.tick();
        }
        gfx.renderer.draw();
        gfx.window.pre_present_notify();
        if let Err(e) = gfx.surface.swap_buffers(&gfx.context) {
            tracing::error!("swap_buffers: {e}");
        }
        self.display_rate.tick();
        self.report();
    }

    fn report(&mut self) {
        let now = Instant::now();
        let Some(display_fps) = self.display_rate.poll(now) else { return };
        let upload_fps = self.upload_rate.poll(now).unwrap_or(0.0);
        let (published, overwritten) = self.mailbox.counters();
        let camera_frames = published - self.last_counters.0;
        let skipped = overwritten - self.last_counters.1;
        self.last_counters = (published, overwritten);
        if self.opts.bench {
            tracing::info!(display_fps, upload_fps, camera_frames, skipped, "bench");
        } else {
            tracing::debug!(display_fps, upload_fps, camera_frames, skipped, "bench");
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
                }
            }
            WindowEvent::KeyboardInput {
                event: KeyEvent { logical_key, state: ElementState::Pressed, .. }, ..
            } => match logical_key.as_ref() {
                Key::Named(NamedKey::Escape) | Key::Character("q") => event_loop.exit(),
                Key::Character("r") => {
                    if let Some(gfx) = self.gfx.as_mut() {
                        let m = !gfx.renderer.mirror();
                        gfx.renderer.set_mirror(m);
                        tracing::info!(mirror = m, "mirror toggled");
                    }
                }
                _ => {}
            },
            WindowEvent::RedrawRequested => self.redraw(),
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        // Swap blocks on vsync, so requesting continuously paces us at the
        // display refresh rate without busy-looping.
        if let Some(gfx) = self.gfx.as_ref() {
            gfx.window.request_redraw();
        }
    }
}
```

Add `pub mod display;` to `v2/src/lib.rs`; the file is now:
```rust
//! piBooth v2 library crate. The binary in `main.rs` wires these together.

pub mod camera;
pub mod config;
pub mod display;
pub mod frame;
pub mod geometry;
pub mod stats;
```

- [ ] **Step 3: Replace `main.rs`**

`v2/src/main.rs`:
```rust
use std::path::PathBuf;

use anyhow::Context;
use clap::Parser;
use pibooth::camera::{Camera, SENSOR_FPS, SENSOR_SIZE, fake::FakeCamera};
use pibooth::config::Config;
use pibooth::display::{self, RunOptions};
use pibooth::frame::FrameMailbox;
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
    /// Disable GPIO (keyboard only). Accepted now, used from milestone 2.
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

    let mailbox = FrameMailbox::new();
    let mut camera = make_camera(&cli, &cfg)?;
    camera.start(mailbox.clone()).context("starting camera")?;

    let result = display::run(
        mailbox,
        RunOptions { mirror: cfg.display.mirror_preview, bench: cli.bench, windowed: cli.windowed },
    );
    camera.stop();
    result
}

fn make_camera(cli: &Cli, cfg: &Config) -> anyhow::Result<Box<dyn Camera>> {
    if cli.fake_camera {
        return Ok(Box::new(FakeCamera::new(SENSOR_SIZE, SENSOR_FPS)));
    }
    #[cfg(feature = "libcamera")]
    {
        Ok(Box::new(pibooth::camera::pi::PiCamera::new(cfg.camera.clone())))
    }
    #[cfg(not(feature = "libcamera"))]
    {
        let _ = cfg;
        anyhow::bail!("built without the `libcamera` feature; use --fake-camera or rebuild with --features libcamera")
    }
}
```

- [ ] **Step 4: Build, lint, test**

Run: `cargo build && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: builds with no warnings (the `pi` module is not referenced without the feature); all tests pass.

- [ ] **Step 5: Run against a display and check it visually**

On a desktop: `cargo run -- --fake-camera --windowed --bench`
Headless (Linux with Xvfb and Mesa):
```bash
Xvfb :99 -screen 0 1920x1080x24 &
DISPLAY=:99 timeout 5 cargo run -- --fake-camera --bench
```
Expected:
- Log `display ready`, then once per second `bench … upload_fps≈60 camera_frames≈60 skipped=0` (`display_fps` may be far above 60 where the server ignores vsync, e.g. Xvfb).
- The image is letterboxed (black bars, aspect 1456:1088), the white bar moves, and the **red square is top-right** (mirrored). Press `r`: it moves top-left. `Esc` exits with status 0.
- With Xvfb, capture to check: `DISPLAY=:99 import -window root shot.png`.

- [ ] **Step 6: Commit**

```bash
git add src/display src/lib.rs src/main.rs
git commit -m "feat(v2): GLES preview renderer, vsync loop and CLI"
```

### Task 6: libcamera preview for the Pi camera

Compiles on any Linux with libcamera 0.4–0.7 headers; streaming is verified on the Pi 5.

**Files:**
- Create: `v2/src/camera/pi.rs`
- Modify: `v2/src/camera/mod.rs` (add the feature-gated module)

**Interfaces:**
- Consumes: `Camera`, `SENSOR_SIZE`, `SENSOR_FPS` (Task 4); `Frame`, `FrameMailbox`, `copy_packed` (Task 3); `CameraCfg` (Task 1).
- Produces: `pub struct PiCamera` with `new(cfg: CameraCfg)`, implementing `Camera` and `Drop`; `start` returns setup errors (no camera, busy, format rejected) or times out after 10 s.

- [ ] **Step 1: Check the Pi's libcamera version**

On the Pi:
```bash
sudo apt install -y libcamera-dev libclang-dev clang pkg-config
pkg-config --modversion libcamera
```
Expected: `0.4.x`–`0.7.x` (what libcamera crate 0.8 supports). If it is newer, stop and report: the crate needs a release supporting it (check crates.io for `libcamera` > 0.8) before continuing.

- [ ] **Step 2: Register the module**

In `v2/src/camera/mod.rs`, after `pub mod fake;`, add:
```rust
#[cfg(feature = "libcamera")]
pub mod pi;
```

- [ ] **Step 3: Write the camera session**

`v2/src/camera/pi.rs`:
```rust
//! Raspberry Pi camera via libcamera (Pi 5 PiSP pipeline, IMX296).
//!
//! libcamera objects borrow the `CameraManager` and must stay on one thread,
//! so the whole session lives on a dedicated "pi-camera" thread. `start`
//! waits for that thread to report that streaming began (or why it failed).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow, bail};
use libcamera::camera::CameraConfigurationStatus;
use libcamera::camera_manager::CameraManager;
use libcamera::control::ControlList;
use libcamera::controls::FrameDurationLimits;
use libcamera::framebuffer::AsFrameBuffer;
use libcamera::framebuffer_allocator::{FrameBuffer, FrameBufferAllocator};
use libcamera::framebuffer_map::MemoryMappedFrameBuffer;
use libcamera::geometry::Size;
use libcamera::pixel_format::PixelFormat;
use libcamera::request::{RequestStatus, ReuseFlag};
use libcamera::stream::StreamRole;

use super::{Camera, SENSOR_FPS, SENSOR_SIZE};
use crate::config::CameraCfg;
use crate::frame::{Frame, FrameMailbox, copy_packed};

/// libcamera `BGR888` = DRM `BG24`: bytes in memory are R, G, B, which is
/// what GL `RGB` expects. (libcamera `RGB888` is B, G, R in memory.)
const PREVIEW_FORMAT: PixelFormat = PixelFormat::new(u32::from_le_bytes(*b"BG24"), 0);
const BUFFER_COUNT: u32 = 4;

pub struct PiCamera {
    cfg: CameraCfg,
    running: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl PiCamera {
    pub fn new(cfg: CameraCfg) -> Self {
        Self { cfg, running: Arc::new(AtomicBool::new(false)), thread: None }
    }
}

impl Camera for PiCamera {
    fn start(&mut self, mailbox: Arc<FrameMailbox>) -> anyhow::Result<()> {
        if self.thread.is_some() {
            return Ok(());
        }
        if !self.cfg.tuning_file.is_empty() {
            // Read by the Raspberry Pi IPA when the camera is configured.
            // SAFETY: called before any other thread reads the environment.
            unsafe { std::env::set_var("LIBCAMERA_RPI_TUNING_FILE", &self.cfg.tuning_file) };
        }
        self.running.store(true, Ordering::SeqCst);
        let running = self.running.clone();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
        let thread = std::thread::Builder::new().name("pi-camera".into()).spawn(move || {
            if let Err(e) = session(&mailbox, &running, &ready_tx) {
                // If setup failed, `start` is still waiting for this message.
                let _ = ready_tx.send(Err(format!("{e:#}")));
                tracing::error!("camera session ended: {e:#}");
            }
        })?;
        self.thread = Some(thread);
        match ready_rx.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => {
                self.stop();
                Err(anyhow!(e))
            }
            Err(_) => {
                self.stop();
                bail!("camera did not start within 10 s")
            }
        }
    }

    fn stop(&mut self) {
        self.running.store(false, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for PiCamera {
    fn drop(&mut self) {
        self.stop();
    }
}

fn session(
    mailbox: &FrameMailbox,
    running: &AtomicBool,
    ready: &mpsc::Sender<Result<(), String>>,
) -> anyhow::Result<()> {
    let mgr = CameraManager::new().context("starting libcamera")?;
    let cameras = mgr.cameras();
    let cam = cameras.get(0).ok_or_else(|| anyhow!("no camera found (is the ribbon cable seated?)"))?;
    tracing::info!(id = %cam.id(), "using camera");
    let mut cam = cam.acquire().context("camera is busy (another app using it?)")?;

    let mut cfgs = cam
        .generate_configuration(&[StreamRole::ViewFinder])
        .ok_or_else(|| anyhow!("could not generate viewfinder configuration"))?;
    {
        let mut s = cfgs.get_mut(0).unwrap();
        s.set_pixel_format(PREVIEW_FORMAT);
        s.set_size(Size { width: SENSOR_SIZE.0, height: SENSOR_SIZE.1 });
        s.set_buffer_count(BUFFER_COUNT);
    }
    match cfgs.validate() {
        CameraConfigurationStatus::Valid => {}
        CameraConfigurationStatus::Adjusted => tracing::warn!("camera adjusted configuration: {cfgs:?}"),
        CameraConfigurationStatus::Invalid => bail!("camera rejected configuration: {cfgs:?}"),
    }
    let (size, stride, format) = {
        let s = cfgs.get(0).unwrap();
        (s.get_size(), s.get_stride() as usize, s.get_pixel_format())
    };
    if format != PREVIEW_FORMAT {
        bail!("camera does not support BGR888 preview (got {format:?})");
    }
    tracing::info!(width = size.width, height = size.height, stride, "preview configured");
    cam.configure(&mut cfgs).context("configuring camera")?;

    let stream = cfgs.get(0).unwrap().stream().ok_or_else(|| anyhow!("stream missing after configure"))?;
    let mut alloc = FrameBufferAllocator::new(&cam);
    let buffers = alloc.alloc(&stream).context("allocating frame buffers")?;
    let mut requests = Vec::with_capacity(buffers.len());
    for (i, buf) in buffers.into_iter().enumerate() {
        let buf = MemoryMappedFrameBuffer::new(buf).map_err(|e| anyhow!("mmap buffer: {e:?}"))?;
        let mut req = cam.create_request(Some(i as u64)).ok_or_else(|| anyhow!("create_request failed"))?;
        req.add_buffer(&stream, buf).context("attaching buffer")?;
        requests.push(req);
    }

    let (done_tx, done_rx) = mpsc::channel();
    cam.on_request_completed(move |req| {
        let _ = done_tx.send(req);
    });

    // Fixed 60 fps: min = max frame duration (µs). Caps exposure at 16.6 ms.
    let frame_us = 1_000_000 / SENSOR_FPS as i64;
    let mut controls = ControlList::new();
    controls.set(FrameDurationLimits([frame_us, frame_us])).map_err(|e| anyhow!("FrameDurationLimits: {e:?}"))?;
    cam.start(Some(&controls)).context("starting camera stream")?;
    for req in requests {
        cam.queue_request(req).map_err(|(_, e)| e).context("queueing request")?;
    }
    let _ = ready.send(Ok(()));

    let (w, h) = (size.width, size.height);
    let row_bytes = w as usize * 3;
    while running.load(Ordering::SeqCst) {
        let mut req = match done_rx.recv_timeout(Duration::from_secs(2)) {
            Ok(r) => r,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                tracing::warn!("no camera frame for 2 s");
                continue;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => bail!("camera callback channel closed"),
        };
        if req.status() == RequestStatus::Complete {
            let fb: &MemoryMappedFrameBuffer<FrameBuffer> =
                req.buffer(&stream).ok_or_else(|| anyhow!("request without buffer"))?;
            let seq = fb.metadata().map(|m| m.sequence() as u64).unwrap_or(0);
            let planes = fb.data();
            let mut data = mailbox.take_buffer(Frame::byte_len(w, h));
            copy_packed(planes[0], stride, row_bytes, h as usize, &mut data);
            mailbox.publish(Frame { width: w, height: h, data, seq, captured_at: Instant::now() });
        }
        req.reuse(ReuseFlag::REUSE_BUFFERS);
        cam.queue_request(req).map_err(|(_, e)| e).context("re-queueing request")?;
    }
    cam.stop().context("stopping camera")?;
    Ok(())
}
```

- [ ] **Step 4: Build and lint both ways**

Run (x86 or Pi): `cargo clippy --all-targets -- -D warnings && cargo clippy --all-targets --features libcamera -- -D warnings && cargo test`
Expected: clean. On a machine without a camera, `cargo run --features libcamera` exits non-zero with `Caused by: starting libcamera: No such device` or `no camera found` — never a panic.

- [ ] **Step 5: Verify on the Pi 5**

From a text console (Ctrl+Alt+F2, desktop not running on that VT):
```bash
cargo build --release --features libcamera
sudo apt install -y cage
cage -s -- ./target/release/pibooth --bench
```
Expected:
- Logs `using camera`, `preview configured width=1456 height=1088 stride=…`, with no `adjusted configuration` warning. If it warns, record the adjusted config in the commit message.
- Live, mirrored, pillarboxed preview fullscreen at 1920×1080; colours natural (if red/blue are swapped, the format constant is wrong: report it, do not patch around it in the shader).
- Bench lines show `display_fps` ≈ 60 and `camera_frames` ≥ 58 sustained for 60 s (spec success criterion 1).

- [ ] **Step 6: Commit**

```bash
git add src/camera
git commit -m "feat(v2): libcamera 60 fps BGR888 preview for the Pi camera"
```

### Task 7: Pi deployment and milestone acceptance

**Files:**
- Create: `v2/deploy/pibooth.service`, `v2/README.md`

**Interfaces:**
- Consumes: the release binary from Task 6.
- Produces: an installable service that starts the booth on boot under `cage` on tty1.

- [ ] **Step 1: Write the systemd unit**

`v2/deploy/pibooth.service`:
```ini
[Unit]
Description=piBooth photo booth (v2)
After=network-online.target systemd-user-sessions.service
Wants=network-online.target

[Service]
User=booth
PAMName=login
TTYPath=/dev/tty1
WorkingDirectory=/opt/pibooth
Environment=RUST_LOG=info
EnvironmentFile=-/opt/pibooth/.env
ExecStart=/usr/bin/cage -s -- /opt/pibooth/pibooth --config /opt/pibooth/config.toml
Restart=always
RestartSec=3

[Install]
WantedBy=multi-user.target
```

- [ ] **Step 2: Write the README**

`v2/README.md`:
````markdown
# piBooth v2 (Rust)

Rewrite of the Python booth (`../main.py`). Design: `../docs/superpowers/specs/2026-11-18-rust-v2-design.md`.

Status: milestone 1 — live preview at sensor rate. No capture yet.

## Develop on a laptop (no Pi needed)

```bash
cargo test
cargo run -- --fake-camera --windowed --bench
```

Keys: `Esc`/`q` quit, `r` toggle preview mirror. The red square marks the
image's top-left corner; with mirroring on it appears top-right.

## Build on the Raspberry Pi 5

```bash
sudo apt install -y libcamera-dev libclang-dev clang pkg-config cage
pkg-config --modversion libcamera   # must be 0.4.x .. 0.7.x for libcamera crate 0.8
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
cargo build --release --features libcamera
```

Run from a text console (no desktop):

```bash
cage -s -- ./target/release/pibooth --bench
```

## Install as a service

```bash
sudo useradd -m -G video,render,input,gpio booth
sudo mkdir -p /opt/pibooth && sudo cp -r target/release/pibooth config.example.toml ../fonts ../effects ../images /opt/pibooth/
sudo cp /opt/pibooth/config.example.toml /opt/pibooth/config.toml
sudo chown -R booth: /opt/pibooth
sudo cp deploy/pibooth.service /etc/systemd/system/
sudo systemctl daemon-reload && sudo systemctl enable --now pibooth
```

The Immich key goes in `/opt/pibooth/.env` as `PIBOOTH_IMMICH_API_KEY=...`
(or v1's `API_KEY=...`).
````
- [ ] **Step 3: Install and verify on the Pi**

Follow `v2/README.md` "Install as a service", then `sudo reboot`.
Expected after boot: the preview appears without login; `journalctl -u pibooth -f` shows `display ready`; `sudo systemctl stop pibooth` releases the camera and screen; `start` brings it back. Kill the process (`sudo pkill pibooth`) and confirm systemd restarts it within ~3 s.

- [ ] **Step 4: Record milestone 1 acceptance**

Temporarily set `Environment=RUST_LOG=info` and add `--bench` to `ExecStart` (or run manually as in Task 6), and confirm over 5 minutes: `camera_frames` ≥ 58 every second, `skipped` stays small (≤ 2/s), CPU temperature (`vcgencmd measure_temp`) stable. Append the measured numbers to the README under a "Milestone 1 results" heading.

- [ ] **Step 5: Commit**

```bash
git add deploy README.md
git commit -m "docs(v2): Pi service, install guide and milestone 1 results"
```
---

## Spec coverage (milestone 1)

| Spec item | Task |
|---|---|
| §3 crate layout, threads, mailbox | 1, 3, 4, 5, 6 |
| §4.2 preview stream 1456×1088, fixed 60 fps, tuning file override | 6 |
| §4.2 `FakeCamera` test pattern | 4 |
| §4.3 fullscreen, vsync, letterbox, shader mirror, texture upload | 2, 5 |
| §4.8 config schema, example file, API key precedence, CLI flags | 1, 5 |
| §5 startup errors exit non-zero with context | 5, 6 |
| §6 unit tests for config/geometry; `--bench` | 1, 2, 3, 5 |
| §9 deployment (cage, systemd, native Pi build) | 7 |
| Success criterion 1 (≥ 58 fps sustained) | 6, 7 |

Deferred to later plans (by milestone): state machine, GPIO button, OSD, monitors, screen flash (M2); capture stream, AE/AWB lock, filters, JPEG/EXIF, review (M3); upload queue (M4); stacking, DNG, strobe (M5); dmabuf, hardware trigger, bare KMS (M6). `--no-gpio` is accepted now and used from M2.
