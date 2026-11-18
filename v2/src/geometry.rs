//! Screen geometry: letterboxing the camera image and building the quad the
//! renderer draws. Pure functions, no GL.

/// Integer pixel rectangle, origin top-left.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

/// Largest rectangle with the source aspect ratio that fits in the screen,
/// centred. Uses ratios, not pixel deltas (fixes v1 `main.py:175`).
pub fn letterbox(src: (u32, u32), screen: (u32, u32)) -> Rect {
    let (sw, sh) = (src.0 as f64, src.1 as f64);
    let (dw, dh) = (screen.0 as f64, screen.1 as f64);
    let scale = (dw / sw).min(dh / sh);
    let w = (sw * scale).round() as u32;
    let h = (sh * scale).round() as u32;
    let w = w.min(screen.0);
    let h = h.min(screen.1);
    Rect { x: ((screen.0 - w) / 2) as i32, y: ((screen.1 - h) / 2) as i32, w, h }
}

/// Triangle-strip vertices `[x, y, u, v] * 4` in GL clip space for `rect`
/// on a `screen`-sized viewport. Order: top-left, bottom-left, top-right,
/// bottom-right. Texture row 0 is the top of the image, so v=0 at the top.
/// `mirror` flips U, giving a selfie view without touching pixel data.
pub fn quad_vertices(rect: Rect, screen: (u32, u32), mirror: bool) -> [f32; 16] {
    let (sw, sh) = (screen.0 as f32, screen.1 as f32);
    let x0 = rect.x as f32 / sw * 2.0 - 1.0;
    let x1 = (rect.x as f32 + rect.w as f32) / sw * 2.0 - 1.0;
    let y0 = 1.0 - rect.y as f32 / sh * 2.0; // top
    let y1 = 1.0 - (rect.y as f32 + rect.h as f32) / sh * 2.0; // bottom
    let (u0, u1) = if mirror { (1.0, 0.0) } else { (0.0, 1.0) };
    [
        x0, y0, u0, 0.0, //
        x0, y1, u0, 1.0, //
        x1, y0, u1, 0.0, //
        x1, y1, u1, 1.0,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imx296_on_1080p_is_pillarboxed() {
        let r = letterbox((1456, 1088), (1920, 1080));
        assert_eq!(r, Rect { x: 237, y: 0, w: 1445, h: 1080 });
    }

    #[test]
    fn small_screen_never_overflows() {
        // v1 overflowed here (fit-by-width gave 800x597 on a 480px screen).
        let r = letterbox((1456, 1088), (800, 480));
        assert!(r.w <= 800 && r.h <= 480, "{r:?}");
        assert_eq!(r.h, 480);
    }

    #[test]
    fn portrait_screen_is_letterboxed() {
        let r = letterbox((1456, 1088), (1080, 1920));
        assert_eq!(r.w, 1080);
        assert!(r.h < 1920);
        assert_eq!(r.x, 0);
        assert_eq!(r.y as u32, (1920 - r.h) / 2);
    }

    #[test]
    fn same_aspect_fills_screen() {
        assert_eq!(letterbox((640, 480), (1280, 960)), Rect { x: 0, y: 0, w: 1280, h: 960 });
    }

    #[test]
    fn full_screen_quad_spans_clip_space() {
        let v = quad_vertices(Rect { x: 0, y: 0, w: 100, h: 50 }, (100, 50), false);
        assert_eq!(&v[0..4], &[-1.0, 1.0, 0.0, 0.0]);
        assert_eq!(&v[12..16], &[1.0, -1.0, 1.0, 1.0]);
    }

    #[test]
    fn mirror_swaps_u_only() {
        let r = Rect { x: 10, y: 0, w: 80, h: 50 };
        let a = quad_vertices(r, (100, 50), false);
        let b = quad_vertices(r, (100, 50), true);
        for i in 0..4 {
            assert_eq!(a[i * 4], b[i * 4], "x unchanged");
            assert_eq!(a[i * 4 + 1], b[i * 4 + 1], "y unchanged");
            assert_eq!(a[i * 4 + 2], 1.0 - b[i * 4 + 2], "u flipped");
            assert_eq!(a[i * 4 + 3], b[i * 4 + 3], "v unchanged");
        }
    }
}
