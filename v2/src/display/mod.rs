//! Fullscreen window + GL context (winit/glutin), the render loop, and the
//! controller that runs the booth.

mod app;
pub mod controller;
mod hud;
mod renderer;

pub use app::{RunOptions, run};
pub use controller::{CameraFactory, Controller};
