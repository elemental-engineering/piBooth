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
