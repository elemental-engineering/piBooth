//! Pixel operations on tightly packed RGB8 images.

use std::path::Path;

use anyhow::Context;
use image::imageops::FilterType;

use crate::osd::Rgba;

/// Standard sepia matrix (rows = output R, G, B; columns = input R, G, B),
/// clamped to 255. v1 applied it to BGR data and rescaled by the image
/// maximum, which swapped red/blue weights and made brightness depend on
/// the scene (spec §2).
pub fn sepia(rgb: &mut [u8]) {
    for px in rgb.chunks_exact_mut(3) {
        let (r, g, b) = (px[0] as f32, px[1] as f32, px[2] as f32);
        px[0] = (0.393 * r + 0.769 * g + 0.189 * b).min(255.0) as u8;
        px[1] = (0.349 * r + 0.686 * g + 0.168 * b).min(255.0) as u8;
        px[2] = (0.272 * r + 0.534 * g + 0.131 * b).min(255.0) as u8;
    }
}

/// Alpha-composite `overlay` (same size) over the RGB image.
pub fn composite(rgb: &mut [u8], overlay: &Rgba) {
    assert_eq!(rgb.len() / 3, overlay.data.len() / 4, "overlay size must match the photo");
    for (px, o) in rgb.chunks_exact_mut(3).zip(overlay.data.chunks_exact(4)) {
        let a = o[3] as u32;
        if a == 0 {
            continue;
        }
        for c in 0..3 {
            px[c] = ((o[c] as u32 * a + px[c] as u32 * (255 - a) + 127) / 255) as u8;
        }
    }
}

/// RGB → opaque RGBA (for showing the photo as an OSD sprite).
pub fn to_rgba(rgb: &[u8], width: u32, height: u32) -> Rgba {
    let mut data = Vec::with_capacity(rgb.len() / 3 * 4);
    for px in rgb.chunks_exact(3) {
        data.extend_from_slice(&[px[0], px[1], px[2], 255]);
    }
    Rgba { width, height, data }
}

/// Every `*.png` in `dir`, sorted by name, resized to `size` if needed.
pub fn load_effects(dir: &Path, size: (u32, u32)) -> anyhow::Result<Vec<Rgba>> {
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .with_context(|| format!("reading effects folder {}", dir.display()))?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("png")))
        .collect();
    files.sort();
    files
        .iter()
        .map(|p| {
            let img = image::open(p).with_context(|| format!("loading effect {}", p.display()))?.to_rgba8();
            let img = if img.dimensions() == size {
                img
            } else {
                image::imageops::resize(&img, size.0, size.1, FilterType::Lanczos3)
            };
            Ok(Rgba { width: size.0, height: size.1, data: img.into_raw() })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sepia_uses_rgb_order_and_clamps() {
        let mut px = [255, 0, 0, 0, 0, 255, 255, 255, 255];
        sepia(&mut px);
        assert_eq!(&px[0..3], &[100, 88, 69], "pure red");
        assert_eq!(&px[3..6], &[48, 42, 33], "pure blue weighs less than red");
        assert_eq!(&px[6..9], &[255, 255, 238], "white clamps, no rescale");
    }

    #[test]
    fn sepia_is_independent_of_other_pixels() {
        let mut a = [10, 20, 30];
        let mut b = [10, 20, 30, 255, 255, 255];
        sepia(&mut a);
        sepia(&mut b);
        assert_eq!(&a, &b[0..3], "v1 rescaled by the image max; we must not");
    }

    #[test]
    fn composite_respects_alpha() {
        let mut rgb = [100, 100, 100, 100, 100, 100, 100, 100, 100];
        let overlay = Rgba { width: 3, height: 1, data: vec![255, 0, 0, 0, 255, 0, 0, 255, 0, 0, 255, 128] };
        composite(&mut rgb, &overlay);
        assert_eq!(&rgb[0..3], &[100, 100, 100], "transparent");
        assert_eq!(&rgb[3..6], &[255, 0, 0], "opaque");
        assert_eq!(&rgb[6..9], &[50, 50, 178], "half: (255*128 + 100*127 + 127) / 255");
    }

    #[test]
    fn to_rgba_is_opaque() {
        let r = to_rgba(&[1, 2, 3, 4, 5, 6], 2, 1);
        assert_eq!(r.data, vec![1, 2, 3, 255, 4, 5, 6, 255]);
    }

    #[test]
    fn loads_v1_effects_at_sensor_size() {
        let dir = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../effects"));
        let effects = load_effects(dir, (1456, 1088)).unwrap();
        assert_eq!(effects.len(), 4);
        assert!(effects.iter().all(|e| e.data.len() == 1456 * 1088 * 4));
        let small = load_effects(dir, (64, 48)).unwrap();
        assert_eq!(small[0].data.len(), 64 * 48 * 4, "resized when sizes differ");
        assert!(load_effects(Path::new("/nonexistent"), (1, 1)).is_err());
    }
}
