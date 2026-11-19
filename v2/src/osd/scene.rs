//! Pure layout: given the booth view, status and pre-rendered sprites,
//! decide which overlays to draw and where (v1 positions, scaled to the
//! screen). Draw order: icons, debug text, title, countdown, banner, flash.

use super::assets::Icon;
use super::{Overlay, SpriteId, WHITE};
use crate::geometry::Rect;
use crate::monitor::{Level, Network, Status};
use crate::state::View;

/// A rendered text sprite and its vertical metrics (see `TextImage`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextSprite {
    pub id: SpriteId,
    pub w: u32,
    pub h: u32,
    pub ascender_y: i32,
    pub baseline_y: i32,
    pub descender_y: i32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Sprites {
    /// 1×1 white texture, tinted for solid boxes and the flash.
    pub white: SpriteId,
    pub title: TextSprite,
    pub subtitle: TextSprite,
    /// `digits[n - 1]` shows the number `n`.
    pub digits: Vec<TextSprite>,
    pub banner: TextSprite,
    /// Indexed like [`Icon::ALL`]; all `icon_px` square.
    pub icons: Vec<SpriteId>,
    pub debug_lines: Vec<TextSprite>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SceneInput {
    pub screen: (u32, u32),
    pub view: View,
    pub status: Status,
    pub debug: bool,
    pub banner: bool,
}

/// Screen-relative sizes, from v1's values on a 1080-line display.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Metrics {
    pub scale: f32,
    pub icon_px: u32,
    pub icon_pad: i32,
    pub icon_step: i32,
}

impl Metrics {
    pub fn for_screen(screen: (u32, u32)) -> Self {
        let (w, h) = (screen.0 as f32, screen.1 as f32);
        Self {
            scale: h / 1080.0,
            icon_px: ((h * 0.03) as u32).max(1),
            icon_pad: (w * 0.01) as i32,
            icon_step: (h * 0.04) as i32,
        }
    }
}

/// Values shown by the debug overlay (`d` key).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DebugInfo {
    pub display_fps: f64,
    pub camera_fps: f64,
    pub mirror: bool,
    pub filter: bool,
    pub temp_c: Option<f32>,
    pub disk_pct: Option<f32>,
}

/// Debug overlay lines, top to bottom (v1's list plus camera fps).
pub fn debug_text(d: &DebugInfo) -> Vec<String> {
    let opt = |v: Option<f32>, unit: &str| v.map_or("--".to_string(), |v| format!("{v:.1}{unit}"));
    vec![
        format!("fps: {:.0}", d.display_fps),
        format!("Camera fps: {:.0}", d.camera_fps),
        format!("Mirrored: {}", d.mirror),
        format!("Filtered: {}", d.filter),
        format!("Temperature: {}", opt(d.temp_c, "°C")),
        format!("Disk used: {}", opt(d.disk_pct, "%")),
    ]
}

/// Icons to show for a status, in v1's order.
pub fn visible_icons(s: &Status) -> Vec<Icon> {
    Icon::ALL
        .into_iter()
        .filter(|i| match i {
            Icon::HddWarning => s.disk == Level::Warning,
            Icon::HddCritical => s.disk == Level::Critical,
            Icon::NoNetwork => s.network == Network::Offline,
            Icon::TempWarning => s.temp == Level::Warning,
            Icon::TempCritical => s.temp == Level::Critical,
        })
        .collect()
}

fn text_at(t: &TextSprite, x: i32, y: i32) -> Overlay {
    Overlay { sprite: t.id, rect: Rect { x, y, w: t.w, h: t.h }, tint: WHITE }
}

pub fn compose(sprites: &Sprites, input: &SceneInput) -> Vec<Overlay> {
    let (sw, sh) = input.screen;
    let m = Metrics::for_screen(input.screen);
    let (cx, cy) = ((sw / 2) as i32, (sh / 2) as i32);
    let mut out = Vec::new();

    for (i, icon) in visible_icons(&input.status).into_iter().enumerate() {
        let idx = Icon::ALL.iter().position(|a| *a == icon).unwrap();
        out.push(Overlay {
            sprite: sprites.icons[idx],
            rect: Rect { x: m.icon_pad + i as i32 * m.icon_step, y: m.icon_pad, w: m.icon_px, h: m.icon_px },
            tint: WHITE,
        });
    }

    if input.debug {
        // Right-aligned on the baseline, 2 % from the right edge (v1 anchor "rs").
        let right = sw as i32 - (sw as f32 * 0.02) as i32;
        for (i, line) in sprites.debug_lines.iter().enumerate() {
            let baseline = ((70.0 + 30.0 * i as f32) * m.scale) as i32;
            out.push(text_at(line, right - line.w as i32, baseline - line.baseline_y));
        }
    }

    if input.view.title {
        // Title sits on the centre line (anchor "md"), subtitle hangs below it ("ma").
        let t = &sprites.title;
        out.push(text_at(t, cx - t.w as i32 / 2, cy - t.descender_y));
        let s = &sprites.subtitle;
        out.push(text_at(s, cx - s.w as i32 / 2, cy - s.ascender_y));
    }

    if let Some(n) = input.view.countdown
        && let Some(d) = n.checked_sub(1).and_then(|i| sprites.digits.get(i as usize))
    {
        // Centred between ascender and descender (anchor "mm").
        let mid = (d.ascender_y + d.descender_y) / 2;
        out.push(text_at(d, cx - d.w as i32 / 2, cy - mid));
    }

    if input.banner {
        let b = &sprites.banner;
        let pad = (20.0 * m.scale) as i32;
        let y = sh as i32 - (sh as f32 * 0.15) as i32;
        let x = cx - b.w as i32 / 2;
        out.push(Overlay {
            sprite: sprites.white,
            rect: Rect { x: x - pad, y: y - pad, w: b.w + 2 * pad as u32, h: b.h + 2 * pad as u32 },
            tint: [0.0, 0.0, 0.0, 0.65],
        });
        out.push(text_at(b, x, y));
    }

    if input.view.flash {
        out.push(Overlay { sprite: sprites.white, rect: Rect { x: 0, y: 0, w: sw, h: sh }, tint: WHITE });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(id: usize, w: u32, h: u32) -> TextSprite {
        TextSprite { id: SpriteId(id), w, h, ascender_y: 0, baseline_y: h as i32 * 3 / 4, descender_y: h as i32 }
    }

    fn sprites() -> Sprites {
        Sprites {
            white: SpriteId(0),
            title: ts(1, 800, 300),
            subtitle: ts(2, 600, 140),
            digits: (1..=5).map(|n| ts(10 + n, 500, 1000)).collect(),
            banner: ts(3, 700, 60),
            icons: (20..25).map(SpriteId).collect(),
            debug_lines: vec![ts(30, 150, 24), ts(31, 200, 24)],
        }
    }

    fn input(view: View) -> SceneInput {
        SceneInput { screen: (1920, 1080), view, status: Status::default(), debug: false, banner: false }
    }

    const NONE: View = View { title: false, countdown: None, flash: false, review: None };

    #[test]
    fn plain_live_view_draws_nothing() {
        assert!(compose(&sprites(), &input(NONE)).is_empty());
    }

    #[test]
    fn title_above_and_subtitle_below_centre() {
        let out = compose(&sprites(), &input(View { title: true, ..NONE }));
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].rect, Rect { x: 960 - 400, y: 540 - 300, w: 800, h: 300 });
        assert_eq!(out[1].rect, Rect { x: 960 - 300, y: 540, w: 600, h: 140 });
    }

    #[test]
    fn countdown_digit_is_centred() {
        let out = compose(&sprites(), &input(View { countdown: Some(3), ..NONE }));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].sprite, SpriteId(13));
        assert_eq!(out[0].rect, Rect { x: 960 - 250, y: 540 - 500, w: 500, h: 1000 });
    }

    #[test]
    fn out_of_range_digit_is_skipped() {
        assert!(compose(&sprites(), &input(View { countdown: Some(9), ..NONE })).is_empty());
    }

    #[test]
    fn zero_digit_is_skipped() {
        assert!(compose(&sprites(), &input(View { countdown: Some(0), ..NONE })).is_empty());
    }

    #[test]
    fn flash_is_full_screen_and_last() {
        let mut i = input(View { title: true, flash: true, ..NONE });
        i.banner = true;
        let out = compose(&sprites(), &i);
        let last = out.last().unwrap();
        assert_eq!((last.sprite, last.rect, last.tint), (SpriteId(0), Rect { x: 0, y: 0, w: 1920, h: 1080 }, WHITE));
    }

    #[test]
    fn icons_follow_status_in_v1_order() {
        let status =
            Status { disk: Level::Critical, network: Network::Offline, temp: Level::Warning, ..Status::default() };
        assert_eq!(visible_icons(&status), vec![Icon::HddCritical, Icon::NoNetwork, Icon::TempWarning]);
        let mut i = input(NONE);
        i.status = status;
        let out = compose(&sprites(), &i);
        let m = Metrics::for_screen((1920, 1080));
        assert_eq!((m.icon_px, m.icon_pad, m.icon_step), (32, 19, 43));
        assert_eq!(out[0].sprite, SpriteId(21));
        assert_eq!(out[0].rect, Rect { x: 19, y: 19, w: 32, h: 32 });
        assert_eq!(out[1].rect.x, 19 + 43);
        assert_eq!(out[2].sprite, SpriteId(23));
        assert!(visible_icons(&Status { network: Network::Unknown, ..Status::default() }).is_empty());
    }

    #[test]
    fn debug_lines_right_aligned_on_v1_baselines() {
        let mut i = input(NONE);
        i.debug = true;
        let out = compose(&sprites(), &i);
        assert_eq!(out.len(), 2);
        let right = 1920 - 38;
        assert_eq!(out[0].rect.x + out[0].rect.w as i32, right);
        assert_eq!(out[1].rect.x + out[1].rect.w as i32, right);
        assert_eq!(out[0].rect.y, 70 - 18);
        assert_eq!(out[1].rect.y, 100 - 18);
    }

    #[test]
    fn debug_text_formats_values_and_missing() {
        let d = DebugInfo {
            display_fps: 59.97,
            camera_fps: 60.1,
            mirror: true,
            filter: false,
            temp_c: Some(48.31),
            disk_pct: None,
        };
        assert_eq!(
            debug_text(&d),
            vec![
                "fps: 60",
                "Camera fps: 60",
                "Mirrored: true",
                "Filtered: false",
                "Temperature: 48.3°C",
                "Disk used: --"
            ]
        );
    }

    #[test]
    fn banner_has_dark_box_behind_text() {
        let mut i = input(NONE);
        i.banner = true;
        let out = compose(&sprites(), &i);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].sprite, SpriteId(0));
        assert_eq!(out[0].tint, [0.0, 0.0, 0.0, 0.65]);
        assert!(out[0].rect.w > out[1].rect.w && out[0].rect.h > out[1].rect.h);
        assert_eq!(out[1].sprite, SpriteId(3));
    }
}
