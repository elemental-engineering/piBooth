//! Fullscreen window + GL context (winit/glutin) and the render loop.

mod app;
pub mod controller;
mod renderer;

pub use app::{RunOptions, run};
pub use controller::{CameraFactory, Controller};
