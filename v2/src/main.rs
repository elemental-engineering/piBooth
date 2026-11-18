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
