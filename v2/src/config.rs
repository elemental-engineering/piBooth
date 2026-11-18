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
        match dotenvy::from_filename(".env") {
            Ok(_) => {}
            Err(e) if e.not_found() => {}
            Err(e) => tracing::warn!("ignoring malformed .env: {e}"),
        }
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
