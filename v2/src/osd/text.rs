//! Text rasterisation with an optional outline (v1 drew titles with a 1 px
//! black stroke so they stay readable over any scene).

use fontdue::Font;

use super::Rgba;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextStyle {
    pub px: f32,
    pub fill: [u8; 4],
    /// Outline colour and radius in pixels.
    pub outline: Option<([u8; 4], u32)>,
}

/// Rendered text plus where its baseline sits inside the image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextImage {
    pub image: Rgba,
    /// Distance from the image top to the ascender line (= outline padding).
    pub ascender_y: i32,
    /// Distance from the image top to the baseline.
    pub baseline_y: i32,
    /// Distance from the image top to the descender line.
    pub descender_y: i32,
}

pub fn load_font(bytes: Vec<u8>) -> anyhow::Result<Font> {
    Font::from_bytes(bytes, fontdue::FontSettings::default()).map_err(|e| anyhow::anyhow!("invalid font: {e}"))
}

/// Rasterise one line of text. The image spans the font's ascender to
/// descender lines (so all strings of one style share a baseline), padded
/// by the outline radius on every side.
pub fn render_text(font: &Font, text: &str, style: TextStyle) -> TextImage {
    let lm = font.horizontal_line_metrics(style.px);
    let ascent = lm.map_or(style.px, |m| m.ascent).ceil() as i32;
    let descent = lm.map_or(0.0, |m| m.descent).floor() as i32; // negative
    let pad = style.outline.map_or(0, |(_, r)| r) as i32;

    // Lay out glyphs on the baseline.
    let mut glyphs = Vec::new();
    let mut pen = 0.0f32;
    let (mut min_x, mut max_x) = (0i32, 0i32);
    for c in text.chars() {
        let (m, bitmap) = font.rasterize(c, style.px);
        let left = (pen + m.xmin as f32).round() as i32;
        let top = -(m.ymin + m.height as i32); // relative to baseline, y down
        min_x = min_x.min(left);
        max_x = max_x.max(left + m.width as i32);
        glyphs.push((left, top, m.width, m.height, bitmap));
        pen += m.advance_width;
    }
    max_x = max_x.max(pen.ceil() as i32);

    let text_w = (max_x - min_x).max(1);
    let text_h = (ascent - descent).max(1);
    let (w, h) = ((text_w + 2 * pad) as u32, (text_h + 2 * pad) as u32);
    let baseline_y = pad + ascent;

    // Coverage mask of the fill.
    let mut mask = vec![0u8; (w * h) as usize];
    for (left, top, gw, gh, bitmap) in &glyphs {
        let ox = left - min_x + pad;
        let oy = baseline_y + top;
        for gy in 0..*gh {
            let y = oy + gy as i32;
            if y < 0 || y >= h as i32 {
                continue;
            }
            for gx in 0..*gw {
                let x = ox + gx as i32;
                if x < 0 || x >= w as i32 {
                    continue;
                }
                let i = (y as u32 * w + x as u32) as usize;
                mask[i] = mask[i].max(bitmap[gy * gw + gx]);
            }
        }
    }

    let outline = style.outline.map(|(_, r)| dilate(&mask, w, h, r));
    let mut image = Rgba::new(w, h);
    for i in 0..(w * h) as usize {
        let fa = mask[i] as f32 / 255.0 * style.fill[3] as f32 / 255.0;
        let (oc, oa) = match (&outline, style.outline) {
            (Some(o), Some((c, _))) => (c, o[i] as f32 / 255.0 * c[3] as f32 / 255.0 * (1.0 - fa)),
            _ => ([0u8; 4], 0.0),
        };
        let a = fa + oa;
        if a <= 0.0 {
            continue;
        }
        let px = &mut image.data[i * 4..i * 4 + 4];
        for ch in 0..3 {
            px[ch] = ((style.fill[ch] as f32 * fa + oc[ch] as f32 * oa) / a).round() as u8;
        }
        px[3] = (a * 255.0).round() as u8;
    }
    TextImage { image, ascender_y: pad, baseline_y, descender_y: pad + ascent - descent }
}

/// Max filter over a disc of radius `r` (outline mask).
fn dilate(mask: &[u8], w: u32, h: u32, r: u32) -> Vec<u8> {
    let (w, h, r) = (w as i32, h as i32, r as i32);
    let offsets: Vec<(i32, i32)> = (-r..=r)
        .flat_map(|dy| (-r..=r).map(move |dx| (dx, dy)))
        .filter(|(dx, dy)| dx * dx + dy * dy <= r * r)
        .collect();
    let mut out = vec![0u8; mask.len()];
    for y in 0..h {
        for x in 0..w {
            let mut m = 0u8;
            for (dx, dy) in &offsets {
                let (sx, sy) = (x + dx, y + dy);
                if sx >= 0 && sy >= 0 && sx < w && sy < h {
                    m = m.max(mask[(sy * w + sx) as usize]);
                }
            }
            out[(y * w + x) as usize] = m;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arial() -> Font {
        load_font(include_bytes!("../../../fonts/arial.ttf").to_vec()).unwrap()
    }

    fn style(px: f32, outline: Option<([u8; 4], u32)>) -> TextStyle {
        TextStyle { px, fill: [255, 252, 226, 255], outline }
    }

    fn alpha_bbox(img: &Rgba) -> Option<(u32, u32, u32, u32)> {
        let mut bb: Option<(u32, u32, u32, u32)> = None;
        for y in 0..img.height {
            for x in 0..img.width {
                if img.pixel(x, y)[3] > 0 {
                    bb = Some(match bb {
                        None => (x, y, x, y),
                        Some((a, b, c, d)) => (a.min(x), b.min(y), c.max(x), d.max(y)),
                    });
                }
            }
        }
        bb
    }

    #[test]
    fn same_style_shares_height_and_baseline() {
        let f = arial();
        let a = render_text(&f, "Photo", style(40.0, None));
        let b = render_text(&f, "gy", style(40.0, None));
        assert_eq!(a.image.height, b.image.height);
        assert_eq!(a.baseline_y, b.baseline_y);
        assert!(a.ascender_y < a.baseline_y && a.baseline_y < a.descender_y);
        assert_eq!(a.descender_y, a.image.height as i32);
    }

    #[test]
    fn wider_text_is_wider_and_ink_sits_on_baseline() {
        let f = arial();
        let one = render_text(&f, "1", style(100.0, None));
        let many = render_text(&f, "1111", style(100.0, None));
        assert!(many.image.width > 3 * one.image.width);
        let (_, top, _, bottom) = alpha_bbox(&one.image).unwrap();
        assert!(bottom as i32 <= one.baseline_y, "'1' has no descender");
        assert!((one.baseline_y - top as i32) as f32 > 60.0, "digit height ~0.7 em");
    }

    #[test]
    fn fill_colour_is_used() {
        let f = arial();
        let t = render_text(&f, "I", style(60.0, None));
        let (x0, y0, x1, y1) = alpha_bbox(&t.image).unwrap();
        let p = t.image.pixel((x0 + x1) / 2, (y0 + y1) / 2);
        assert_eq!(p, [255, 252, 226, 255]);
    }

    #[test]
    fn outline_pads_and_draws_dark_edge() {
        let f = arial();
        let plain = render_text(&f, "I", style(60.0, None));
        let out = render_text(&f, "I", style(60.0, Some(([0, 0, 0, 255], 3))));
        assert_eq!(out.image.width, plain.image.width + 6);
        assert_eq!(out.image.height, plain.image.height + 6);
        assert_eq!(out.baseline_y, plain.baseline_y + 3);
        // Just left of the stem: outline (black), fully opaque.
        let (x0, y0, x1, y1) = alpha_bbox(&out.image).unwrap();
        let mid_y = (y0 + y1) / 2;
        let edge = out.image.pixel(x0 + 1, mid_y);
        assert_eq!(edge[3], 255);
        assert!(edge[0] < 40, "outline is dark: {edge:?}");
        let centre = out.image.pixel((x0 + x1) / 2, mid_y);
        assert_eq!(centre, [255, 252, 226, 255]);
    }

    #[test]
    fn empty_text_is_a_valid_image() {
        let t = render_text(&arial(), "", style(20.0, None));
        assert!(t.image.width >= 1 && t.image.height >= 1);
        assert_eq!(t.image.data.len(), (t.image.width * t.image.height * 4) as usize);
    }
}
