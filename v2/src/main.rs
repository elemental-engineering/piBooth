use std::path::PathBuf;

use anyhow::Context;
use clap::Parser;
use std::sync::Arc;
use std::time::Instant;

use pibooth::camera::{Camera, SENSOR_FPS, SENSOR_SIZE, fake::FakeCamera};
use pibooth::config::Config;
use pibooth::display::{self, CameraFactory, Controller, RunOptions};
use pibooth::flash::{GpioLine, Strobe};
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
    /// Disable the GPIO button and strobe (keyboard only, no strobe).
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
    #[cfg(feature = "libcamera")]
    if !cfg.camera.tuning_file.is_empty() {
        // Read by the Raspberry Pi IPA when the camera is configured.
        // SAFETY: no other thread exists yet (the button, camera, monitor and
        // display threads are all spawned below), so nothing reads the
        // environment concurrently.
        unsafe { std::env::set_var("LIBCAMERA_RPI_TUNING_FILE", &cfg.camera.tuning_file) };
    }

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
    let mut ctrl =
        Controller::start(&cfg, camera_factory(&cli, &cfg)?, button, rx, Instant::now()).context("starting camera")?;
    if cfg.flash.strobe() {
        let max_on = std::time::Duration::from_millis(cfg.flash.max_ms as u64);
        match cli.no_gpio {
            true => tracing::warn!("--no-gpio: running without the strobe"),
            false => match GpioLine::open(&cfg.flash).and_then(|line| Strobe::start(Box::new(line), max_on)) {
                Ok(s) => ctrl.set_strobe(s),
                Err(e) => tracing::warn!("strobe unavailable, running without it: {e:#}"),
            },
        }
    }
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
        let raw = cfg.capture.save_dng;
        return Ok(Arc::new(move || -> Box<dyn Camera> {
            Box::new(FakeCamera::new(SENSOR_SIZE, SENSOR_FPS).with_raw(raw))
        }));
    }
    #[cfg(feature = "libcamera")]
    {
        let (cam_cfg, raw) = (cfg.camera.clone(), cfg.capture.save_dng);
        Ok(Arc::new(move || -> Box<dyn Camera> { Box::new(pibooth::camera::pi::PiCamera::new(cam_cfg.clone(), raw)) }))
    }
    #[cfg(not(feature = "libcamera"))]
    {
        let _ = cfg;
        anyhow::bail!("built without the `libcamera` feature; use --fake-camera or rebuild with --features libcamera")
    }
}
