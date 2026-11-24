//! Burst stacking (spec §4.4 step 1). The global shutter means frames differ
//! only by a whole-image translation (hand-held or a nudged booth) plus noise
//! and whatever the guests moved, so:
//!
//! - `sharpest`: keep the frame with the highest variance of Laplacian.
//! - `average`: the sharpest frame is the reference; every other frame is
//!   aligned to it (integer translation, coarse-to-fine SAD search on a luma
//!   pyramid), rejected if it moved more than `max_shift` or is exposed
//!   differently, or if it still does not match the reference after alignment
//!   (it moved beyond the search window or the scene changed), then averaged
//!   pixel by pixel. A pixel of another frame only counts where it matches the
//!   reference within that frame's noise, so a moving guest stays sharp from
//!   the reference instead of ghosting.
//! - `off`: the first frame (the photo metered under the flash).

use rayon::prelude::*;

use crate::config::StackMode;

/// Rejected when its mean brightness differs more than this from the reference.
const MAX_BRIGHTNESS_RATIO: f64 = 0.06;
/// Per-pixel match threshold bounds (luma levels, 0..=255).
const MIN_MATCH: u8 = 10;
const MAX_MATCH: u8 = 60;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameUse {
    Reference,
    /// Averaged; `(dx, dy)`: frame pixel `(x + dx, y + dy)` shows reference pixel `(x, y)`.
    Averaged {
        dx: i32,
        dy: i32,
    },
    TooFar {
        dx: i32,
        dy: i32,
    },
    Brightness,
    /// Does not match the reference after alignment: moved beyond the search
    /// window or the scene changed.
    Mismatch,
    /// Not looked at (`sharpest` / `off`).
    Unused,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StackReport {
    pub reference: usize,
    pub frames: Vec<FrameUse>,
}

impl StackReport {
    pub fn averaged(&self) -> usize {
        self.frames.iter().filter(|f| matches!(f, FrameUse::Averaged { .. })).count()
    }
}

/// Luma pyramid of one frame: full, half and quarter resolution.
struct Pyramid {
    levels: [Plane; 3],
}

struct Plane {
    w: usize,
    h: usize,
    px: Vec<u8>,
}

impl Plane {
    fn at(&self, x: usize, y: usize) -> u8 {
        self.px[y * self.w + x]
    }

    fn half(&self) -> Plane {
        let (w, h) = (self.w / 2, self.h / 2);
        let mut px = vec![0u8; w * h];
        for y in 0..h {
            let (r0, r1) = (&self.px[2 * y * self.w..], &self.px[(2 * y + 1) * self.w..]);
            for x in 0..w {
                let s = r0[2 * x] as u16 + r0[2 * x + 1] as u16 + r1[2 * x] as u16 + r1[2 * x + 1] as u16;
                px[y * w + x] = ((s + 2) / 4) as u8;
            }
        }
        Plane { w, h, px }
    }

    fn mean(&self) -> f64 {
        self.px.iter().map(|&v| v as u64).sum::<u64>() as f64 / self.px.len().max(1) as f64
    }
}

/// Rec. 601 luma, integer weights summing to 256.
pub fn luma(rgb: &[u8], width: u32, height: u32) -> Vec<u8> {
    let n = width as usize * height as usize;
    rgb[..n * 3]
        .chunks_exact(3)
        .map(|p| ((77 * p[0] as u32 + 150 * p[1] as u32 + 29 * p[2] as u32 + 128) >> 8) as u8)
        .collect()
}

impl Pyramid {
    fn new(rgb: &[u8], width: u32, height: u32) -> Self {
        let full = Plane { w: width as usize, h: height as usize, px: luma(rgb, width, height) };
        let half = full.half();
        let quarter = half.half();
        Self { levels: [full, half, quarter] }
    }
}

/// Variance of the 4-neighbour Laplacian over the interior: higher = sharper.
pub fn sharpness(plane_px: &[u8], w: usize, h: usize) -> f64 {
    if w < 3 || h < 3 {
        return 0.0;
    }
    let (mut sum, mut sum2, mut n) = (0f64, 0f64, 0f64);
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let c = plane_px[y * w + x] as i32;
            let l = plane_px[y * w + x - 1] as i32
                + plane_px[y * w + x + 1] as i32
                + plane_px[(y - 1) * w + x] as i32
                + plane_px[(y + 1) * w + x] as i32
                - 4 * c;
            sum += l as f64;
            sum2 += (l * l) as f64;
            n += 1.0;
        }
    }
    let mean = sum / n;
    sum2 / n - mean * mean
}

/// Mean absolute difference between `a(x, y)` and `b(x + dx, y + dy)` over
/// the rectangle `margin` pixels in from every edge, sampling every `step`.
fn sad(a: &Plane, b: &Plane, dx: i32, dy: i32, margin: usize, step: usize) -> u64 {
    let mut total = 0u64;
    let mut count = 0u64;
    if a.w <= 2 * margin || a.h <= 2 * margin {
        return u64::MAX;
    }
    for y in (margin..a.h - margin).step_by(step) {
        let by = (y as i32 + dy) as usize;
        for x in (margin..a.w - margin).step_by(step) {
            let bx = (x as i32 + dx) as usize;
            total += a.at(x, y).abs_diff(b.at(bx, by)) as u64;
            count += 1;
        }
    }
    // Scaled so different sample counts never tie by accident.
    (total * 1024).checked_div(count).unwrap_or(u64::MAX)
}

fn best_around(a: &Plane, b: &Plane, (cx, cy): (i32, i32), radius: i32, margin: usize, step: usize) -> (i32, i32) {
    let mut best = ((cx, cy), u64::MAX);
    for dy in cy - radius..=cy + radius {
        for dx in cx - radius..=cx + radius {
            let s = sad(a, b, dx, dy, margin, step);
            if s < best.1 {
                best = ((dx, dy), s);
            }
        }
    }
    best.0
}

/// Translation of `frame` relative to `reference`, searched up to about
/// `max_shift` pixels: a full search at quarter resolution, refined by ±1 at
/// half and full resolution.
fn estimate_shift(reference: &Pyramid, frame: &Pyramid, max_shift: u32) -> (i32, i32) {
    let r = (max_shift as i32 + 3) / 4 + 1;
    let m = r as usize + 1;
    let q = best_around(&reference.levels[2], &frame.levels[2], (0, 0), r, m, 1);
    let h = best_around(&reference.levels[1], &frame.levels[1], (q.0 * 2, q.1 * 2), 1, 2 * m + 1, 1);
    best_around(&reference.levels[0], &frame.levels[0], (h.0 * 2, h.1 * 2), 1, 4 * m + 3, 2)
}

/// Luma difference a frame's pixel may have from the reference and still be
/// averaged: three times the noise of the difference (robust median
/// estimate), within [`MIN_MATCH`, `MAX_MATCH`].
fn match_threshold(reference: &Plane, frame: &Plane, (dx, dy): (i32, i32)) -> Option<u8> {
    let mut hist = [0u64; 256];
    let margin = dx.unsigned_abs().max(dy.unsigned_abs()) as usize + 1;
    if reference.w <= 2 * margin || reference.h <= 2 * margin {
        return Some(MIN_MATCH);
    }
    let mut n = 0u64;
    for y in (margin..reference.h - margin).step_by(4) {
        for x in (margin..reference.w - margin).step_by(4) {
            let d = reference.at(x, y).abs_diff(frame.at((x as i32 + dx) as usize, (y as i32 + dy) as usize));
            hist[d as usize] += 1;
            n += 1;
        }
    }
    let mut seen = 0;
    let median = hist
        .iter()
        .position(|&c| {
            seen += c;
            seen * 2 >= n
        })
        .unwrap_or(0);
    let sigma = 1.4826 * median as f64;
    let t = (3.0 * sigma).round();
    (t <= MAX_MATCH as f64).then(|| t.max(MIN_MATCH as f64) as u8)
}

/// Combine a burst of tightly packed RGB frames (same size) into one image.
pub fn stack(
    mut frames: Vec<Vec<u8>>,
    width: u32,
    height: u32,
    mode: StackMode,
    max_shift: u32,
) -> (Vec<u8>, StackReport) {
    assert!(!frames.is_empty(), "empty burst");
    let n = frames.len();
    let len = width as usize * height as usize * 3;
    assert!(frames.iter().all(|f| f.len() >= len), "frame shorter than {width}x{height}");
    if mode == StackMode::Off || n == 1 {
        let mut uses = vec![FrameUse::Unused; n];
        uses[0] = FrameUse::Reference;
        return (frames.swap_remove(0), StackReport { reference: 0, frames: uses });
    }

    let pyramids: Vec<Pyramid> = frames.par_iter().map(|f| Pyramid::new(f, width, height)).collect();
    let sharp: Vec<f64> =
        pyramids.par_iter().map(|p| sharpness(&p.levels[1].px, p.levels[1].w, p.levels[1].h)).collect();
    let reference = (0..n).max_by(|&a, &b| sharp[a].total_cmp(&sharp[b]).then(b.cmp(&a))).unwrap();
    let mut uses = vec![FrameUse::Unused; n];
    uses[reference] = FrameUse::Reference;
    if mode == StackMode::Sharpest {
        return (frames.swap_remove(reference), StackReport { reference, frames: uses });
    }

    let ref_pyr = &pyramids[reference];
    let ref_mean = ref_pyr.levels[0].mean().max(1.0);
    let decided: Vec<(FrameUse, u8)> = (0..n)
        .into_par_iter()
        .map(|i| {
            if i == reference {
                return (FrameUse::Reference, 0);
            }
            let p = &pyramids[i];
            if (p.levels[0].mean() / ref_mean - 1.0).abs() > MAX_BRIGHTNESS_RATIO {
                return (FrameUse::Brightness, 0);
            }
            let (dx, dy) = estimate_shift(ref_pyr, p, max_shift);
            if dx.unsigned_abs() > max_shift || dy.unsigned_abs() > max_shift {
                return (FrameUse::TooFar { dx, dy }, 0);
            }
            match match_threshold(&ref_pyr.levels[0], &p.levels[0], (dx, dy)) {
                Some(t) => (FrameUse::Averaged { dx, dy }, t),
                None => (FrameUse::Mismatch, 0),
            }
        })
        .collect();
    let mut used: Vec<(usize, i32, i32, u8)> = Vec::new();
    for (i, (u, t)) in decided.into_iter().enumerate() {
        if let FrameUse::Averaged { dx, dy } = u {
            used.push((i, dx, dy, t));
        }
        uses[i] = u;
    }
    let report = StackReport { reference, frames: uses };
    if used.is_empty() {
        return (frames.swap_remove(reference), report);
    }

    let (w, h) = (width as usize, height as usize);
    let ref_rgb = &frames[reference];
    let ref_y = &ref_pyr.levels[0];
    let mut out = vec![0u8; len];
    out.par_chunks_mut(w * 3).enumerate().for_each(|(y, row)| {
        for x in 0..w {
            let i = (y * w + x) * 3;
            let mut sum = [ref_rgb[i] as u16, ref_rgb[i + 1] as u16, ref_rgb[i + 2] as u16];
            let mut count = 1u16;
            let yr = ref_y.px[y * w + x];
            for &(k, dx, dy, thr) in &used {
                let (sx, sy) = (x as i32 + dx, y as i32 + dy);
                if sx < 0 || sy < 0 || sx >= w as i32 || sy >= h as i32 {
                    continue;
                }
                let j = sy as usize * w + sx as usize;
                if pyramids[k].levels[0].px[j].abs_diff(yr) > thr {
                    continue; // moved: keep the reference's pixel here
                }
                let f = &frames[k][j * 3..j * 3 + 3];
                sum[0] += f[0] as u16;
                sum[1] += f[1] as u16;
                sum[2] += f[2] as u16;
                count += 1;
            }
            for c in 0..3 {
                row[x * 3 + c] = ((sum[c] + count / 2) / count) as u8;
            }
        }
    });
    (out, report)
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: u32 = 160;
    const H: u32 = 120;
    const PAD: i32 = 48;

    /// Smooth random texture (value noise on an 8-px grid) plus fine detail,
    /// `PAD` pixels bigger than the frame on every side.
    fn scene(seed: u64) -> (Vec<u8>, usize, usize) {
        let mut rng = fastrand::Rng::with_seed(seed);
        let (bw, bh) = (W as usize + 2 * PAD as usize, H as usize + 2 * PAD as usize);
        let (gw, gh) = (bw / 8 + 2, bh / 8 + 2);
        let grid: Vec<f32> = (0..gw * gh).map(|_| rng.f32() * 200.0 + 20.0).collect();
        let mut img = vec![0u8; bw * bh * 3];
        for y in 0..bh {
            for x in 0..bw {
                let (gx, gy) = (x as f32 / 8.0, y as f32 / 8.0);
                let (ix, iy) = (gx as usize, gy as usize);
                let (fx, fy) = (gx - ix as f32, gy - iy as f32);
                let g = |a: usize, b: usize| grid[b * gw + a];
                let v = g(ix, iy) * (1.0 - fx) * (1.0 - fy)
                    + g(ix + 1, iy) * fx * (1.0 - fy)
                    + g(ix, iy + 1) * (1.0 - fx) * fy
                    + g(ix + 1, iy + 1) * fx * fy;
                let detail = if (x / 3 + y / 3) % 2 == 0 { 12.0 } else { -12.0 };
                let v = (v + detail).clamp(0.0, 255.0) as u8;
                img[(y * bw + x) * 3..][..3].copy_from_slice(&[v, v.saturating_sub(20), v / 2]);
            }
        }
        (img, bw, bh)
    }

    /// The frame seen when the camera moved by (sx, sy): pixel (x, y) shows
    /// scene (x + PAD + sx, y + PAD + sy), plus noise of ±`noise`.
    fn view(scene: &(Vec<u8>, usize, usize), sx: i32, sy: i32, noise: u8, rng: &mut fastrand::Rng) -> Vec<u8> {
        let (img, bw, _) = scene;
        let mut out = vec![0u8; (W * H * 3) as usize];
        for y in 0..H as i32 {
            for x in 0..W as i32 {
                let s = (((y + PAD + sy) as usize) * bw + (x + PAD + sx) as usize) * 3;
                for c in 0..3 {
                    let n = if noise == 0 { 0 } else { rng.i32(-(noise as i32)..=noise as i32) };
                    out[((y * W as i32 + x) * 3) as usize + c] = (img[s + c] as i32 + n).clamp(0, 255) as u8;
                }
            }
        }
        out
    }

    fn rmse(a: &[u8], b: &[u8]) -> f64 {
        let s: f64 = a.iter().zip(b).map(|(&x, &y)| (x as f64 - y as f64).powi(2)).sum();
        (s / a.len() as f64).sqrt()
    }

    /// Horizontal 5-tap box blur (motion blur).
    fn blur(f: &[u8]) -> Vec<u8> {
        let mut o = f.to_vec();
        let w = W as usize;
        for y in 0..H as usize {
            for x in 2..w - 2 {
                for c in 0..3 {
                    let s: u32 = (0..5).map(|d| f[(y * w + x + d - 2) * 3 + c] as u32).sum();
                    o[(y * w + x) * 3 + c] = (s / 5) as u8;
                }
            }
        }
        o
    }

    fn pyr(f: &[u8]) -> Pyramid {
        Pyramid::new(f, W, H)
    }

    #[test]
    fn finds_shifts_up_to_the_limit() {
        let sc = scene(1);
        let mut rng = fastrand::Rng::with_seed(2);
        let reference = view(&sc, 0, 0, 6, &mut rng);
        for (sx, sy) in [(0, 0), (3, -2), (-7, 5), (8, 8), (-1, 0)] {
            let f = view(&sc, sx, sy, 6, &mut rng);
            // Frame pixel (x - sx, y - sy) shows reference pixel (x, y).
            assert_eq!(estimate_shift(&pyr(&reference), &pyr(&f), 8), (-sx, -sy), "camera moved ({sx}, {sy})");
        }
    }

    #[test]
    fn averaging_aligned_frames_reduces_noise() {
        let sc = scene(3);
        let clean = view(&sc, 0, 0, 0, &mut fastrand::Rng::with_seed(0));
        let mut rng = fastrand::Rng::with_seed(4);
        let shifts = [(0, 0), (2, 1), (-3, 2), (1, -4), (4, 3), (-2, -2)];
        let frames: Vec<Vec<u8>> = shifts.iter().map(|&(x, y)| view(&sc, x, y, 30, &mut rng)).collect();
        let single = rmse(&frames[0], &clean);
        let (out, report) = stack(frames, W, H, StackMode::Average, 8);
        assert_eq!(report.averaged(), 5, "{report:?}");
        let r = report.reference;
        let (rx, ry) = shifts[r];
        // The output is in the reference frame's position.
        let clean_ref = view(&sc, rx, ry, 0, &mut fastrand::Rng::with_seed(0));
        let stacked = rmse(&out, &clean_ref);
        assert!(stacked < single / 1.8, "noise {single:.1} -> {stacked:.1}");
        for (i, u) in report.frames.iter().enumerate() {
            if let FrameUse::Averaged { dx, dy } = u {
                let (sx, sy) = shifts[i];
                assert_eq!((*dx, *dy), (rx - sx, ry - sy), "frame {i}");
            }
        }
    }

    #[test]
    fn frames_that_moved_too_far_are_rejected() {
        let sc = scene(5);
        let mut rng = fastrand::Rng::with_seed(6);
        // The far frame is slightly blurred so it is never the reference.
        let far = blur(&view(&sc, 12, 0, 4, &mut rng));
        let frames = vec![view(&sc, 0, 0, 4, &mut rng), far, view(&sc, 1, 1, 4, &mut rng)];
        let (_, report) = stack(frames, W, H, StackMode::Average, 4);
        assert_ne!(report.reference, 1);
        assert!(matches!(report.frames[1], FrameUse::TooFar { dx: -13..=-11, .. }), "{report:?}");
        assert_eq!(report.averaged(), 1);
    }

    #[test]
    fn far_moved_frames_are_not_averaged() {
        let sc = scene(13);
        let mut rng = fastrand::Rng::with_seed(14);
        let far = blur(&view(&sc, 40, 0, 4, &mut rng));
        let frames = vec![view(&sc, 0, 0, 4, &mut rng), view(&sc, 1, 0, 4, &mut rng), far];
        let (_, report) = stack(frames, W, H, StackMode::Average, 8);
        assert_ne!(report.reference, 2);
        assert!(matches!(report.frames[2], FrameUse::TooFar { .. } | FrameUse::Mismatch), "{report:?}");
        assert_eq!(report.averaged(), 1);
    }

    #[test]
    fn differently_exposed_frames_are_rejected() {
        let sc = scene(7);
        let mut rng = fastrand::Rng::with_seed(8);
        let a = view(&sc, 0, 0, 2, &mut rng);
        let bright: Vec<u8> = a.iter().map(|&v| (v as u16 * 5 / 4).min(255) as u8).collect();
        let originals = vec![a, bright];
        let (out, report) = stack(originals.clone(), W, H, StackMode::Average, 8);
        assert!(report.frames.contains(&FrameUse::Brightness), "{report:?}");
        assert_eq!(report.averaged(), 0);
        assert_eq!(out, originals[report.reference]);
    }

    #[test]
    fn moving_subject_does_not_ghost() {
        let sc = scene(9);
        let mut rng = fastrand::Rng::with_seed(10);
        let mut frames: Vec<Vec<u8>> = (0..5).map(|_| view(&sc, 0, 0, 3, &mut rng)).collect();
        // A white square that moves 20 px per frame (a waving hand).
        for (k, f) in frames.iter_mut().enumerate() {
            for y in 40..70 {
                for x in 10 + 20 * k..40 + 20 * k {
                    f[(y * W as usize + x) * 3..][..3].copy_from_slice(&[255, 255, 255]);
                }
            }
        }
        let originals = frames.clone();
        let (out, report) = stack(frames, W, H, StackMode::Average, 8);
        let r = report.reference;
        // Where the reference shows the square, the output shows it fully...
        let at = |img: &[u8], x: usize, y: usize| img[(y * W as usize + x) * 3];
        let sq_x = 10 + 20 * r + 15;
        assert!(at(&out, sq_x, 55) > 245, "square kept: {}", at(&out, sq_x, 55));
        // ...and where other frames had it, the background is not brightened.
        let mut checked = 0;
        for k in (0..5).filter(|&k| k != r) {
            let x = 10 + 20 * k + 15;
            if (10 + 20 * r..40 + 20 * r).contains(&x) {
                continue;
            }
            let diff = at(&out, x, 55).abs_diff(at(&originals[r], x, 55));
            assert!(diff < 12, "ghost of frame {k}'s square: {diff}");
            checked += 1;
        }
        assert!(checked >= 2);
    }

    #[test]
    fn sharpest_picks_the_unblurred_frame() {
        let sc = scene(11);
        let mut rng = fastrand::Rng::with_seed(12);
        let sharp = view(&sc, 0, 0, 2, &mut rng);
        let frames = vec![blur(&sharp), blur(&sharp), sharp.clone(), blur(&sharp)];
        let (out, report) = stack(frames, W, H, StackMode::Sharpest, 8);
        assert_eq!(report.reference, 2);
        assert_eq!(out, sharp);
    }

    /// `cargo test --release -- --ignored stack_timing --nocapture`
    #[test]
    #[ignore = "timing only"]
    fn stack_timing_at_sensor_size() {
        let (w, h) = crate::camera::SENSOR_SIZE;
        let mut rng = fastrand::Rng::with_seed(1);
        let base: Vec<u8> = (0..w * h * 3).map(|i| ((i / 3 % w) * 255 / w) as u8 ^ rng.u8(..32)).collect();
        let frames: Vec<Vec<u8>> =
            (0..6).map(|_| base.iter().map(|&v| v.saturating_add(rng.u8(..8))).collect()).collect();
        let t = std::time::Instant::now();
        let (_, report) = stack(frames, w, h, StackMode::Average, 8);
        eprintln!("6 frames {w}x{h}: {:?}, averaged {}", t.elapsed(), report.averaged());
    }

    #[test]
    fn off_and_single_frame_return_the_first() {
        let a = vec![1u8; (W * H * 3) as usize];
        let b = vec![2u8; (W * H * 3) as usize];
        let (out, report) = stack(vec![a.clone(), b], W, H, StackMode::Off, 8);
        assert_eq!((out, report.reference), (a.clone(), 0));
        let (out, _) = stack(vec![a.clone()], W, H, StackMode::Average, 8);
        assert_eq!(out, a);
    }
}
