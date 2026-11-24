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
    /// Folder holding v1's `fonts/` and `images/`.
    pub assets_dir: PathBuf,
}
impl Default for Display {
    fn default() -> Self {
        Self { mirror_preview: true, assets_dir: PathBuf::from(".") }
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
    /// Screen and strobe together.
    Both,
    None,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Flash {
    pub mode: FlashMode,
    /// Light on this long before the shot, so auto-exposure meters it.
    pub lead_ms: u32,
    /// Hard limit on how long the light stays on.
    pub max_ms: u32,
    /// BCM number of the strobe's output line.
    pub gpio_line: u32,
    /// Accepted from older config files and ignored: the strobe is lit for
    /// the same window as the screen flash (spec §4.5).
    pub pulse_ms: Option<u32>,
    pub active_high: bool,
}
impl Default for Flash {
    fn default() -> Self {
        Self { mode: FlashMode::Screen, lead_ms: 300, max_ms: 800, gpio_line: 17, pulse_ms: None, active_high: true }
    }
}
impl Flash {
    pub fn screen(&self) -> bool {
        matches!(self.mode, FlashMode::Screen | FlashMode::Both)
    }

    pub fn strobe(&self) -> bool {
        matches!(self.mode, FlashMode::Strobe | FlashMode::Both)
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
        Self { gpio_line: 27, active_low: false, debounce_ms: 30 }
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

/// Longest burst: 12 frames are 0.2 s at 60 fps and ~57 MB of RGB.
pub const MAX_BURST_FRAMES: u32 = 12;

fn looks_like_uuid(s: &str) -> bool {
    s.len() == 36
        && s.chars()
            .enumerate()
            .all(|(i, c)| if matches!(i, 8 | 13 | 18 | 23) { c == '-' } else { c.is_ascii_hexdigit() })
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
        match dotenvy::from_filename(".env") {
            Ok(_) => {}
            Err(e) if e.not_found() => {}
            Err(e) => tracing::warn!("ignoring malformed .env: {e}"),
        }
        cfg.immich_api_key = api_key_from(|k| std::env::var(k).ok());
        if cfg.flash.pulse_ms.is_some() {
            tracing::warn!(
                "flash.pulse_ms is no longer used: the strobe is lit from flash.lead_ms before the shot until the burst ends"
            );
        }
        Ok(cfg)
    }

    fn validate(&self) -> anyhow::Result<()> {
        if !(1..=MAX_BURST_FRAMES).contains(&self.capture.burst_frames) {
            bail!("capture.burst_frames must be 1..={MAX_BURST_FRAMES}");
        }
        if self.capture.max_shift_px > 32 {
            bail!("capture.max_shift_px must be <= 32");
        }
        if !(1..=100).contains(&self.capture.jpeg_quality) {
            bail!("capture.jpeg_quality must be 1..=100");
        }
        if self.timing.countdown_s == 0 {
            bail!("timing.countdown_s must be >= 1");
        }
        if self.timing.countdown_s > 30 {
            bail!("timing.countdown_s must be <= 30");
        }
        if self.flash.lead_ms < 100 {
            bail!("flash.lead_ms must be at least 100 ms so high-quality noise reduction reaches the photo");
        }
        let (lat, lon) = (self.immich.latitude, self.immich.longitude);
        if !lat.is_finite() || !lon.is_finite() || lat.abs() > 90.0 || lon.abs() > 180.0 {
            bail!("immich.latitude must be within +-90 and immich.longitude within +-180");
        }
        let url = &self.immich.base_url;
        if !url.is_empty() && !url.starts_with("http://") && !url.starts_with("https://") {
            bail!("immich.base_url must start with http:// or https://");
        }
        if !self.immich.album_id.is_empty() && !looks_like_uuid(&self.immich.album_id) {
            bail!("immich.album_id must be a UUID like 4c1d8f6e-0b2a-4f6e-9a7c-2d4e6f8a0b1c");
        }
        if self.flash.lead_ms > self.flash.max_ms {
            bail!("flash.lead_ms must be <= flash.max_ms");
        }
        if self.flash.lead_ms as u64 >= self.timing.countdown_s as u64 * 1000 {
            bail!("flash.lead_ms must be shorter than the countdown (timing.countdown_s)");
        }
        if self.button.gpio_line == self.flash.gpio_line && self.flash.strobe() {
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
    fn dev_config_points_assets_at_repo_root() {
        let cfg = Config::from_toml_str(include_str!("../config.dev.toml")).unwrap();
        assert_eq!(cfg.display.assets_dir, PathBuf::from(".."));
    }

    #[test]
    fn button_defaults_match_v1_wiring() {
        let b = Config::default().button;
        assert_eq!((b.gpio_line, b.active_low, b.debounce_ms), (27, false, 30));
    }

    #[test]
    fn unknown_key_is_rejected() {
        assert!(Config::from_toml_str("[capture]\nburst = 3\n").is_err());
    }

    #[test]
    fn invalid_values_are_rejected() {
        assert!(Config::from_toml_str("[capture]\nburst_frames = 0\n").is_err());
        assert!(Config::from_toml_str("[capture]\nburst_frames = 13\n").is_err());
        assert!(Config::from_toml_str("[capture]\nmax_shift_px = 33\n").is_err());
        assert!(Config::from_toml_str("[flash]\nmode = \"both\"\ngpio_line = 27\n").is_err());
        assert!(Config::from_toml_str("[capture]\njpeg_quality = 0\n").is_err());
        assert!(Config::from_toml_str("[flash]\nmode = \"strobe\"\ngpio_line = 27\n").is_err());
        assert!(Config::from_toml_str("[flash]\nlead_ms = 900\nmax_ms = 800\n").is_err());
        assert!(Config::from_toml_str("[timing]\ncountdown_s = 1\n[flash]\nlead_ms = 1000\n").is_err());
        assert!(Config::from_toml_str("[timing]\ncountdown_s = 31\n").is_err());
        assert!(Config::from_toml_str("[flash]\nlead_ms = 99\n").is_err());
        assert!(Config::from_toml_str("[immich]\nlatitude = nan\n").is_err());
        assert!(Config::from_toml_str("[immich]\nlatitude = 90.5\n").is_err());
        assert!(Config::from_toml_str("[immich]\nlongitude = -180.5\n").is_err());
        assert!(Config::from_toml_str("[immich]\nbase_url = \"immich.example/api\"\n").is_err());
        assert!(Config::from_toml_str("[immich]\nbase_url = \"https://immich.example/api\"\n").is_ok());
        assert!(Config::from_toml_str("[immich]\nalbum_id = \"my-album\"\n").is_err());
        assert!(Config::from_toml_str("[immich]\nalbum_id = \"4c1d8f6e-0b2a-4f6e-9a7c-2d4e6f8a0b1g\"\n").is_err());
        assert!(Config::from_toml_str("[immich]\nalbum_id = \"4c1d8f6e-0b2a-4f6e-9a7c-2d4e6f8a0b1c\"\n").is_ok());
    }

    #[test]
    fn flash_modes_and_old_pulse_ms() {
        let both = Config::from_toml_str("[flash]\nmode = \"both\"\n").unwrap().flash;
        assert!(both.screen() && both.strobe());
        let strobe = Config::from_toml_str("[flash]\nmode = \"strobe\"\npulse_ms = 150\n").unwrap().flash;
        assert!(!strobe.screen() && strobe.strobe());
        assert_eq!(strobe.pulse_ms, Some(150), "old files still load");
        let none = Config::from_toml_str("[flash]\nmode = \"none\"\n").unwrap().flash;
        assert!(!none.screen() && !none.strobe());
        assert!(Config::default().flash.screen());
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
