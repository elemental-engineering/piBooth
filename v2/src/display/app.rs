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
                self.refresh_debug_text();
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
                    } else {
                        self.refresh_debug_text();
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
