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
pub const OOPS: &str = "Oops, try again";

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

    /// Show `image` as the review photo (re-uses one GPU texture).
    pub fn set_photo(&mut self, r: &mut Renderer, image: &Rgba) -> anyhow::Result<()> {
        match self.sprites.photo {
            Some((id, _, _)) => r.update_sprite(id, image),
            None => {
                let id = r.create_sprite(image)?;
                self.sprites.photo = Some((id, image.width, image.height));
            }
        }
        if let Some(p) = self.sprites.photo.as_mut() {
            (p.1, p.2) = (image.width, image.height);
        }
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
    let oops = text(&a.title_font, OOPS, style(160.0, scale, CREAM, true))?;
    let white = r.create_sprite(&Rgba { width: 1, height: 1, data: vec![255; 4] })?;
    let icons = a.icons.iter().map(|img| r.create_sprite(img)).collect::<anyhow::Result<Vec<_>>>()?;
    Ok(Sprites { white, title, subtitle, digits, banner, oops, icons, debug_lines: Vec::new(), photo: None })
}

fn all_ids(s: &Sprites) -> Vec<SpriteId> {
    let mut ids = vec![s.white, s.title.id, s.subtitle.id, s.banner.id, s.oops.id];
    ids.extend(s.photo.map(|(id, _, _)| id));
    ids.extend(s.digits.iter().map(|d| d.id));
    ids.extend(s.icons.iter().copied());
    ids.extend(s.debug_lines.iter().map(|d| d.id));
    ids
}
