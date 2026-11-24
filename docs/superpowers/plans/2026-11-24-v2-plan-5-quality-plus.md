# piBooth v2 — Plan 5: Burst Stacking, DNG and GPIO Strobe (Milestone 5)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Each photo becomes a burst of frames stacked into one low-noise image (or the sharpest frame), optionally saved alongside a DNG of the sensor's raw data, lit by the screen flash and/or a GPIO-driven strobe.

**Architecture:** `Capture { id, frames }` makes the camera deliver a `CapturedBurst`: the photo frame (metered under the flash), then frames taken with exposure/white balance locked (detected from `ExposureTimeMode = Manual` in frame metadata). A pure `BurstCollector` holds that rule. The photo worker stacks the burst (`pipeline::stack`, rayon) before the existing filter/encode steps and, with `capture.save_dng`, writes `raw/<name>.dng` from a second, 16-bit raw stream (`pipeline::dng`, reusing the EXIF TIFF writer). A `Strobe` thread drives the strobe's GPIO line on FlashOn/FlashOff and turns it off by itself after `flash.max_ms`.

**Tech Stack:** Rust 2024 (rustc ≥ 1.88); new dependency `rayon` 1.11. libcamera facts from the v0.7 Raspberry Pi pipeline/IPA source (`pipeline/rpi/pisp/pisp.cpp` raw stream rules, `ipa/rpi/common/ipa_base.cpp` metadata). DNG colour verified by decoding with LibRaw 0.22 (rawpy).

**Spec:** `docs/superpowers/specs/2026-11-18-rust-v2-design.md` (§4.2 camera/burst, §4.4 pipeline, §4.5 flash, §4.8 config; already updated for this milestone). Builds on Plans 1–4 on branch `v2-rust`.

## Global Constraints

- Crate `pibooth` in `v2/`; v1 files are read-only. The spec is already updated; do not edit it.
- Burst: `capture.burst_frames` (1..=12, default 6) frames; `frames[0]` is the photo metered under the flash; the rest are the first frames whose metadata reports `ExposureTimeMode = Manual`; after 8 unlocked frames the burst continues without waiting.
- Stacking (`capture.stack`): reference = highest variance of Laplacian on half-resolution luma; `average` rejects frames > 6 % mean-luma difference or shift > `capture.max_shift_px` (≤ 32), aligns by SAD (quarter-res full search, ±1 refinement at half and full res), averages per pixel only where luma is within the frame's 3σ threshold (10–60); `sharpest` = reference; `off` or 1 frame = first frame. Output stays in the reference frame's position.
- DNG: `raw/<photo file stem>.dng` (same stem as the original JPEG; `Storage::paths_for` also treats an existing `.dng` as taken); DNG 1.4 tags exactly as in `pipeline/dng.rs`; written after the photo; a DNG failure is logged and never fails the photo. Raw stream only when `capture.save_dng`; format `RG16`-family (`RG16`/`GR16`/`GB16`/`BG16`, modifier 0) else the camera fails to start with a message naming `capture.save_dng`.
- Flash: `flash.mode = screen | strobe | both | none`; light from `lead_ms` before the shot until the burst arrives, capped at `max_ms` (state machine) and independently by the strobe thread. Screen white only for `screen`/`both`. `flash.pulse_ms` still parses (Option) and is ignored with a warning. Button and strobe lines must differ when the strobe is used. `--no-gpio` disables the strobe too. Missing GPIO ⇒ warning, booth runs.
- `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` pass on x86 without the `libcamera` feature; `--features libcamera` clippy passes where libcamera ≥ 0.5 headers exist (`PKG_CONFIG_PATH` pointing at them).
- Commit messages end with the project's attribution trailer lines.

## File Structure

| File | Responsibility |
|---|---|
| `v2/src/pipeline/stack.rs` (new) | `stack`, `StackReport`, `FrameUse`, `luma`, `sharpness` |
| `v2/src/pipeline/dng.rs` (new) | `RawImage`, `RawColour`, `encode`, `colour_matrices`, `white_level`, CFA helpers |
| `v2/src/pipeline/exif.rs` | TIFF entry helpers widened to `pub(crate)`, `SRATIONAL` |
| `v2/src/flash.rs` (new) | `Output`, `GpioLine`, `Strobe` |
| `v2/src/config.rs` | `FlashMode::Both`, `Flash::{screen, strobe}`, `pulse_ms: Option`, burst/shift limits |
| `v2/src/camera/mod.rs` | `Capture { id, frames }`, `CapturedBurst`, `RawCapture`, `BurstCollector` |
| `v2/src/camera/fake.rs` | bursts, optional mosaicked raw (`with_raw`) |
| `v2/src/camera/pi.rs` | raw stream, burst collection, colour metadata |
| `v2/src/frame.rs` | `copy_u16_le` |
| `v2/src/state.rs` | `Timings::{flash, burst_frames}`, `Command::Capture { id, frames }` |
| `v2/src/pipeline/photo.rs`, `worker.rs`, `v2/src/storage.rs` | stack → filter → encode → DNG; logging; DNG name |
| `v2/src/display/controller.rs`, `v2/src/main.rs` | bursts into jobs, strobe wiring, screen-flash gating |
| `v2/README.md`, `v2/config.example.toml` | docs |

All commands run from `v2/` unless stated. Diffs below are against the branch before this plan; apply them by hand (context lines are exact).

---
### Task 1: Burst stacking

**Files:**
- Create: `v2/src/pipeline/stack.rs`
- Modify: `v2/Cargo.toml` (after `raw-window-handle = "0.6"` add `rayon = "1.11"`), `v2/src/pipeline/mod.rs` (add `pub mod stack;` after `pub mod photo;`)
- Test: inline in `stack.rs`

**Interfaces:**
- Produces: `pub enum FrameUse { Reference, Averaged { dx: i32, dy: i32 }, TooFar { dx: i32, dy: i32 }, Brightness, Unused }`; `pub struct StackReport { pub reference: usize, pub frames: Vec<FrameUse> }` with `averaged(&self) -> usize`; `pub fn luma(rgb: &[u8], width: u32, height: u32) -> Vec<u8>`; `pub fn sharpness(px: &[u8], w: usize, h: usize) -> f64`; `pub fn stack(frames: Vec<Vec<u8>>, width: u32, height: u32, mode: StackMode, max_shift: u32) -> (Vec<u8>, StackReport)` (panics on an empty burst or a short frame).

- [ ] **Step 1: Write the failing tests**

Create `stack.rs` with the Interfaces items stubbed `todo!()` (private helpers may be added as needed by the tests: `Pyramid::new`, `estimate_shift`), plus:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    const W: u32 = 160;
    const H: u32 = 120;
    const PAD: i32 = 16;

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
    fn differently_exposed_frames_are_rejected() {
        let sc = scene(7);
        let mut rng = fastrand::Rng::with_seed(8);
        let a = view(&sc, 0, 0, 2, &mut rng);
        let bright: Vec<u8> = a.iter().map(|&v| (v as u16 * 5 / 4).min(255) as u8).collect();
        let (out, report) = stack(vec![a.clone(), bright], W, H, StackMode::Average, 8);
        assert!(report.frames.contains(&FrameUse::Brightness), "{report:?}");
        assert_eq!(report.averaged(), 0);
        let reference_frame = if report.reference == 0 { &a } else { &out };
        assert_eq!(&out, reference_frame);
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
```

Run: `cargo test pipeline::stack` — Expected: FAIL (todo).

- [ ] **Step 2: Implement**

`v2/src/pipeline/stack.rs`:
```rust
//! Burst stacking (spec §4.4 step 1). The global shutter means frames differ
//! only by a whole-image translation (hand-held or a nudged booth) plus noise
//! and whatever the guests moved, so:
//!
//! - `sharpest`: keep the frame with the highest variance of Laplacian.
//! - `average`: the sharpest frame is the reference; every other frame is
//!   aligned to it (integer translation, coarse-to-fine SAD search on a luma
//!   pyramid), rejected if it moved more than `max_shift` or is exposed
//!   differently, then averaged pixel by pixel. A pixel of another frame only
//!   counts where it matches the reference within that frame's noise, so a
//!   moving guest stays sharp from the reference instead of ghosting.
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
fn match_threshold(reference: &Plane, frame: &Plane, (dx, dy): (i32, i32)) -> u8 {
    let mut hist = [0u64; 256];
    let margin = dx.unsigned_abs().max(dy.unsigned_abs()) as usize + 1;
    if reference.w <= 2 * margin || reference.h <= 2 * margin {
        return MIN_MATCH;
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
    (3.0 * sigma).round().clamp(MIN_MATCH as f64, MAX_MATCH as f64) as u8
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
            (FrameUse::Averaged { dx, dy }, match_threshold(&ref_pyr.levels[0], &p.levels[0], (dx, dy)))
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
    const PAD: i32 = 16;

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
    fn differently_exposed_frames_are_rejected() {
        let sc = scene(7);
        let mut rng = fastrand::Rng::with_seed(8);
        let a = view(&sc, 0, 0, 2, &mut rng);
        let bright: Vec<u8> = a.iter().map(|&v| (v as u16 * 5 / 4).min(255) as u8).collect();
        let (out, report) = stack(vec![a.clone(), bright], W, H, StackMode::Average, 8);
        assert!(report.frames.contains(&FrameUse::Brightness), "{report:?}");
        assert_eq!(report.averaged(), 0);
        let reference_frame = if report.reference == 0 { &a } else { &out };
        assert_eq!(&out, reference_frame);
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
```

- [ ] **Step 3: Verify**

Run: `cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check` — Expected: all pass (7 new, 1 ignored), clean.
Run: `cargo test --release --lib -- --ignored stack_timing --nocapture` — Expected: prints the time for 6 sensor-size frames (≈ 60 ms on 4 x86 cores); record it in the report.

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml Cargo.lock src/pipeline/mod.rs src/pipeline/stack.rs
git commit -m "feat(v2): burst stacking with alignment and motion rejection"
```

---

### Task 2: DNG writer

**Files:**
- Create: `v2/src/pipeline/dng.rs`
- Modify: `v2/src/pipeline/exif.rs` (helpers `pub(crate)`), `v2/src/pipeline/mod.rs` (add `pub mod dng;` before `pub mod exif;`)
- Test: inline in `dng.rs` (read back with `kamadak-exif`)

**Interfaces:**
- Consumes: `exif::{Entry, ascii, short, long, rationals, write_ifd, ifd_len, BYTE, SHORT, LONG, SRATIONAL, MAKE, MODEL, SOFTWARE, PhotoInfo}`.
- Produces: `pub struct RawImage { pub width: u32, pub height: u32, pub cfa: [u8; 4], pub data: Vec<u16> }`; `pub const RGGB: [u8; 4]`; `pub fn cfa_from_fourcc([u8; 4]) -> Option<[u8; 4]>`; `pub fn black_for_cfa([i32; 4], [u8; 4]) -> [u32; 4]`; `pub struct RawColour { pub black_levels: Option<[i32; 4]>, pub colour_gains: Option<(f32, f32)>, pub ccm: Option<[f32; 9]> }` (`Default`, `Copy`); `pub fn colour_matrices(&RawColour) -> ([f64; 9], [f64; 3])`; `pub fn white_level(&[u16]) -> u32`; `pub fn encode(&RawImage, &RawColour, &PhotoInfo) -> Vec<u8>`; `pub const UNIQUE_CAMERA_MODEL`.

- [ ] **Step 1: Widen the TIFF helpers**

Apply to `v2/src/pipeline/exif.rs`:
```diff
--- a/v2/src/pipeline/exif.rs
+++ b/v2/src/pipeline/exif.rs
@@ -19,35 +19,37 @@
     pub gps: Option<(f64, f64)>,
 }
 
-const ASCII: u16 = 2;
-const SHORT: u16 = 3;
-const LONG: u16 = 4;
-const RATIONAL: u16 = 5;
-const BYTE: u16 = 1;
-const UNDEFINED: u16 = 7;
-
-struct Entry {
-    tag: u16,
-    kind: u16,
-    count: u32,
-    data: Vec<u8>,
+pub(crate) const ASCII: u16 = 2;
+pub(crate) const SHORT: u16 = 3;
+pub(crate) const LONG: u16 = 4;
+pub(crate) const RATIONAL: u16 = 5;
+pub(crate) const BYTE: u16 = 1;
+pub(crate) const UNDEFINED: u16 = 7;
+pub(crate) const SRATIONAL: u16 = 10;
+
+/// One TIFF directory entry; `data` is the little-endian value bytes.
+pub(crate) struct Entry {
+    pub tag: u16,
+    pub kind: u16,
+    pub count: u32,
+    pub data: Vec<u8>,
 }
 
-fn ascii(tag: u16, s: &str) -> Entry {
+pub(crate) fn ascii(tag: u16, s: &str) -> Entry {
     let mut data = s.as_bytes().to_vec();
     data.push(0);
     Entry { tag, kind: ASCII, count: data.len() as u32, data }
 }
 
-fn short(tag: u16, v: u16) -> Entry {
+pub(crate) fn short(tag: u16, v: u16) -> Entry {
     Entry { tag, kind: SHORT, count: 1, data: v.to_le_bytes().to_vec() }
 }
 
-fn long(tag: u16, v: u32) -> Entry {
+pub(crate) fn long(tag: u16, v: u32) -> Entry {
     Entry { tag, kind: LONG, count: 1, data: v.to_le_bytes().to_vec() }
 }
 
-fn rationals(tag: u16, values: &[(u32, u32)]) -> Entry {
+pub(crate) fn rationals(tag: u16, values: &[(u32, u32)]) -> Entry {
     let data = values.iter().flat_map(|(n, d)| n.to_le_bytes().into_iter().chain(d.to_le_bytes())).collect();
     Entry { tag, kind: RATIONAL, count: values.len() as u32, data }
 }
@@ -64,7 +66,7 @@
 
 /// Serialise one IFD at `offset` (from the TIFF header). `next` is the
 /// following IFD's offset (0 = none). Values over 4 bytes go after the IFD.
-fn write_ifd(mut entries: Vec<Entry>, offset: u32, next: u32) -> Vec<u8> {
+pub(crate) fn write_ifd(mut entries: Vec<Entry>, offset: u32, next: u32) -> Vec<u8> {
     entries.sort_by_key(|e| e.tag);
     let n = entries.len() as u32;
     let mut extra_at = offset + 2 + n * 12 + 4;
@@ -93,7 +95,7 @@
     ifd
 }
 
-fn ifd_len(entries: &[Entry]) -> u32 {
+pub(crate) fn ifd_len(entries: &[Entry]) -> u32 {
     let extra: u32 = entries.iter().filter(|e| e.data.len() > 4).map(|e| (e.data.len() as u32 + 1) & !1).sum();
     2 + entries.len() as u32 * 12 + 4 + extra
 }
```

- [ ] **Step 2: Write the failing tests**

Create `dng.rs` with the Interfaces items stubbed `todo!()`, plus:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDateTime;
    use exif::{Context, In, Tag, Value};

    fn info() -> PhotoInfo {
        PhotoInfo {
            taken: NaiveDateTime::parse_from_str("2026-11-19 14:03:07", "%Y-%m-%d %H:%M:%S").unwrap(),
            exposure_us: Some(16_000),
            gain: Some(4.0),
            gps: None,
        }
    }

    fn colour() -> RawColour {
        RawColour {
            black_levels: Some([3840, 3840, 3840, 3840]),
            colour_gains: Some((1.8, 1.5)),
            ccm: Some([1.6, -0.4, -0.2, -0.3, 1.5, -0.2, 0.0, -0.6, 1.6]),
        }
    }

    fn raw(w: u32, h: u32) -> RawImage {
        RawImage { width: w, height: h, cfa: RGGB, data: (0..w * h).map(|i| ((i % 1024) as u16) << 6).collect() }
    }

    fn field(e: &exif::Exif, tag: u16) -> &exif::Field {
        e.get_field(Tag(Context::Tiff, tag), In::PRIMARY).unwrap_or_else(|| panic!("tag {tag} missing"))
    }

    fn uint(e: &exif::Exif, tag: u16, i: usize) -> u32 {
        field(e, tag).value.get_uint(i).unwrap()
    }

    #[test]
    fn layout_and_cfa_tags() {
        let r = raw(8, 4);
        let bytes = encode(&r, &colour(), &info());
        let e = exif::Reader::new().read_raw(bytes.clone()).expect("valid TIFF");
        assert_eq!((uint(&e, 256, 0), uint(&e, 257, 0), uint(&e, 258, 0)), (8, 4, 16));
        assert_eq!(uint(&e, 262, 0), 32803);
        assert_eq!((0..4).map(|i| uint(&e, 33422, i)).collect::<Vec<_>>(), vec![0, 1, 1, 2]);
        assert_eq!((0..4).map(|i| uint(&e, 50706, i)).collect::<Vec<_>>(), vec![1, 4, 0, 0]);
        assert_eq!(uint(&e, 50714, 0), 3840);
        assert_eq!(uint(&e, 50717, 0), 0xFFC0);
        assert_eq!(uint(&e, 50778, 0), 21);
        assert_eq!(uint(&e, 34855, 0), 400);
        let (at, len) = (uint(&e, 273, 0) as usize, uint(&e, 279, 0) as usize);
        assert_eq!(len, 8 * 4 * 2);
        assert_eq!(bytes.len(), at + len);
        let first: Vec<u16> = bytes[at..at + len].chunks_exact(2).map(|b| u16::from_le_bytes([b[0], b[1]])).collect();
        assert_eq!(first, r.data);
        assert_eq!(field(&e, 50708).display_value().to_string(), "\"Raspberry Pi IMX296\"");
    }

    #[test]
    fn colour_matrix_maps_white_to_the_as_shot_neutral() {
        let (m, neutral) = colour_matrices(&colour());
        assert!((neutral[0] - 1.0 / 1.8).abs() < 1e-6 && (neutral[2] - 1.0 / 1.5).abs() < 1e-6);
        let white = mul_vec(&m, mul_vec(&SRGB_TO_XYZ, [1.0; 3]));
        let max = white.iter().cloned().fold(f64::MIN, f64::max);
        assert!((max - 1.0).abs() < 1e-9, "{white:?}");
        // The camera's neutral is the direction of the mapped white.
        for c in 0..3 {
            assert!((white[c] / white[1] - neutral[c]).abs() < 1e-6, "{white:?} vs {neutral:?}");
        }
        let bytes = encode(&raw(4, 2), &colour(), &info());
        let e = exif::Reader::new().read_raw(bytes).unwrap();
        match &field(&e, 50721).value {
            Value::SRational(v) => {
                assert_eq!(v.len(), 9);
                assert!((v[0].to_f64() - m[0]).abs() < 1e-4);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn missing_metadata_gives_safe_defaults() {
        let (m, neutral) = colour_matrices(&RawColour::default());
        assert_eq!(neutral, [1.0; 3]);
        let white = mul_vec(&m, mul_vec(&SRGB_TO_XYZ, [1.0; 3]));
        assert!(white.iter().all(|v| (v - 1.0).abs() < 1e-6), "{white:?}");
        let e = exif::Reader::new().read_raw(encode(&raw(4, 2), &RawColour::default(), &info())).unwrap();
        assert_eq!(uint(&e, 50714, 0), 0);
    }

    #[test]
    fn cfa_orders_and_black_levels() {
        assert_eq!(cfa_from_fourcc(*b"BG16"), Some([2, 1, 1, 0]));
        assert_eq!(cfa_from_fourcc(*b"RG10"), None);
        let levels = [10, 20, 30, 40]; // R, Gr, Gb, B
        assert_eq!(black_for_cfa(levels, RGGB), [10, 20, 30, 40]);
        assert_eq!(black_for_cfa(levels, [2, 1, 1, 0]), [40, 30, 20, 10]);
        assert_eq!(black_for_cfa(levels, [1, 0, 2, 1]), [20, 10, 40, 30]);
        assert_eq!(black_for_cfa(levels, [1, 2, 0, 1]), [30, 40, 10, 20]);
        let mut r = raw(4, 2);
        r.cfa = [2, 1, 1, 0];
        let e = exif::Reader::new().read_raw(encode(&r, &colour(), &info())).unwrap();
        assert_eq!((0..4).map(|i| uint(&e, 33422, i)).collect::<Vec<_>>(), vec![2, 1, 1, 0]);
    }

    #[test]
    fn white_level_ignores_unused_low_bits() {
        assert_eq!(white_level(&[0x40, 0xFFC0]), 0xFFC0);
        assert_eq!(white_level(&[1, 1023]), 0xFFFF);
        assert_eq!(white_level(&[0x100, 0x200]), 0xFF00, "at most 8 unused bits");
        assert_eq!(white_level(&[0, 0]), 0xFFFF);
    }

    /// Writes `$DNG_OUT`: 4 colour patches (sRGB red, green, blue, grey)
    /// taken back through `colour()` to camera raw, for checking with a raw
    /// decoder. `DNG_OUT=/tmp/t.dng cargo test -- --ignored write_patch_dng`
    #[test]
    #[ignore = "writes a file for external checks"]
    fn write_patch_dng() {
        let Ok(out) = std::env::var("DNG_OUT") else { return };
        let c = colour();
        let (gr, gb) = c.colour_gains.unwrap();
        let ccm: M3 = c.ccm.unwrap().map(|v| v as f64);
        let to_cam = inverse(&ccm).unwrap();
        let lin = |v: f64| if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) };
        let patches = [[0.8, 0.1, 0.1], [0.1, 0.7, 0.1], [0.1, 0.1, 0.8], [0.5, 0.5, 0.5]].map(|p| {
            let wb = mul_vec(&to_cam, p.map(lin));
            [wb[0] / gr as f64, wb[1], wb[2] / gb as f64]
        });
        let (w, h) = (64u32, 64u32);
        let black = 3840.0;
        let data = (0..w * h)
            .map(|i| {
                let (x, y) = (i % w, i / w);
                let p = &patches[((y / 32) * 2 + x / 32) as usize];
                let ch = RGGB[((y % 2) * 2 + x % 2) as usize] as usize;
                let v = black + p[ch].clamp(0.0, 1.0) * (65472.0 - black);
                ((v.round() as u16) >> 6) << 6
            })
            .collect();
        std::fs::write(out, encode(&RawImage { width: w, height: h, cfa: RGGB, data }, &c, &info())).unwrap();
    }

    #[test]
    fn inverse_round_trips() {
        let i = inverse(&SRGB_TO_XYZ).unwrap();
        let id = mul(&SRGB_TO_XYZ, &i);
        for (k, v) in id.iter().enumerate() {
            assert!((v - if k % 4 == 0 { 1.0 } else { 0.0 }).abs() < 1e-9);
        }
        assert_eq!(inverse(&[0.0; 9]), None);
    }
}
```

Run: `cargo test pipeline::dng` — Expected: FAIL.

- [ ] **Step 3: Implement**

`v2/src/pipeline/dng.rs`:
```rust
//! DNG 1.4 writer for the IMX296 Bayer frame (spec §4.4 step 2): one
//! uncompressed 16-bit CFA strip in IFD0, black/white levels, and a colour
//! matrix built from the ISP's own white balance and colour correction so
//! raw developers render it like the JPEG.
//!
//! The Pi 5 front end delivers raw frames as unpacked 16-bit samples with the
//! sensor's 10 bits at the top; libcamera reports black levels on the same
//! 16-bit scale.

use super::exif::{
    BYTE, Entry, LONG, MAKE, MODEL, PhotoInfo, SHORT, SOFTWARE, SRATIONAL, ascii, ifd_len, long, rationals, short,
    write_ifd,
};

/// One raw Bayer frame, tightly packed (`width` samples per row).
#[derive(Debug, Clone, PartialEq)]
pub struct RawImage {
    pub width: u32,
    pub height: u32,
    /// Colour of the top-left 2×2 block, row-major: 0 = red, 1 = green, 2 = blue.
    pub cfa: [u8; 4],
    pub data: Vec<u16>,
}

pub const RGGB: [u8; 4] = [0, 1, 1, 2];

/// CFA layout of a libcamera 16-bit Bayer format (`RG16`, `GR16`, `GB16`, `BG16`).
pub fn cfa_from_fourcc(fourcc: [u8; 4]) -> Option<[u8; 4]> {
    match &fourcc {
        b"RG16" => Some(RGGB),
        b"GR16" => Some([1, 0, 2, 1]),
        b"GB16" => Some([1, 2, 0, 1]),
        b"BG16" => Some([2, 1, 1, 0]),
        _ => None,
    }
}

/// libcamera black levels (R, Gr, Gb, B) in CFA position order. A green
/// sample on a red row is Gr, on a blue row Gb.
pub fn black_for_cfa(levels: [i32; 4], cfa: [u8; 4]) -> [u32; 4] {
    let [r, gr, gb, b] = levels.map(|v| v.clamp(0, 0xFFFF) as u32);
    let mut out = [0; 4];
    for (i, &c) in cfa.iter().enumerate() {
        let row = &cfa[i / 2 * 2..i / 2 * 2 + 2];
        out[i] = match c {
            0 => r,
            2 => b,
            _ if row.contains(&0) => gr,
            _ => gb,
        };
    }
    out
}

/// Colour state the ISP used for the frame (libcamera metadata).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct RawColour {
    /// `SensorBlackLevels`: R, Gr, Gb, B on the 16-bit scale.
    pub black_levels: Option<[i32; 4]>,
    /// `ColourGains`: red and blue white-balance gains.
    pub colour_gains: Option<(f32, f32)>,
    /// `ColourCorrectionMatrix`: white-balanced camera RGB → linear sRGB, row-major.
    pub ccm: Option<[f32; 9]>,
}

pub const UNIQUE_CAMERA_MODEL: &str = "Raspberry Pi IMX296";

/// Linear sRGB → CIE XYZ (D65).
const SRGB_TO_XYZ: [f64; 9] = [0.4124, 0.3576, 0.1805, 0.2126, 0.7152, 0.0722, 0.0193, 0.1192, 0.9505];

type M3 = [f64; 9];

fn mul(a: &M3, b: &M3) -> M3 {
    let mut m = [0.0; 9];
    for r in 0..3 {
        for c in 0..3 {
            m[r * 3 + c] = (0..3).map(|k| a[r * 3 + k] * b[k * 3 + c]).sum();
        }
    }
    m
}

fn mul_vec(a: &M3, v: [f64; 3]) -> [f64; 3] {
    [0, 1, 2].map(|r| (0..3).map(|k| a[r * 3 + k] * v[k]).sum())
}

fn inverse(m: &M3) -> Option<M3> {
    let det =
        m[0] * (m[4] * m[8] - m[5] * m[7]) - m[1] * (m[3] * m[8] - m[5] * m[6]) + m[2] * (m[3] * m[7] - m[4] * m[6]);
    if det.abs() < 1e-9 {
        return None;
    }
    let adj = [
        m[4] * m[8] - m[5] * m[7],
        m[2] * m[7] - m[1] * m[8],
        m[1] * m[5] - m[2] * m[4],
        m[5] * m[6] - m[3] * m[8],
        m[0] * m[8] - m[2] * m[6],
        m[2] * m[3] - m[0] * m[5],
        m[3] * m[7] - m[4] * m[6],
        m[1] * m[6] - m[0] * m[7],
        m[0] * m[4] - m[1] * m[3],
    ];
    Some(adj.map(|v| v / det))
}

/// (ColorMatrix1: XYZ → camera, AsShotNeutral). Camera → sRGB is
/// `ccm · diag(gain_r, 1, gain_b)`, so a neutral grey (camera
/// `1/gain_r, 1, 1/gain_b`) maps to the D65 white; the matrix is scaled so
/// that white comes out with a largest component of 1 (DNG convention).
pub fn colour_matrices(colour: &RawColour) -> (M3, [f64; 3]) {
    let (gr, gb) = colour.colour_gains.filter(|(r, b)| *r > 0.0 && *b > 0.0).unwrap_or((1.0, 1.0));
    let ccm: M3 = colour.ccm.map(|c| c.map(|v| v as f64)).unwrap_or([1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]);
    let gains = [gr as f64, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, gb as f64];
    let cam_to_xyz = mul(&SRGB_TO_XYZ, &mul(&ccm, &gains));
    let neutral = [1.0 / gr as f64, 1.0, 1.0 / gb as f64];
    let Some(mut xyz_to_cam) = inverse(&cam_to_xyz) else {
        // Degenerate metadata: sRGB primaries, no white balance.
        return (inverse(&SRGB_TO_XYZ).unwrap(), [1.0; 3]);
    };
    let white = mul_vec(&xyz_to_cam, mul_vec(&SRGB_TO_XYZ, [1.0; 3]));
    let scale = white.iter().cloned().fold(f64::MIN, f64::max);
    if scale > 0.0 {
        xyz_to_cam = xyz_to_cam.map(|v| v / scale);
    }
    (xyz_to_cam, neutral)
}

/// Largest sample value the data can hold: unused low bits (always zero)
/// are excluded, so 10-bit data at the top of 16 bits gives 0xFFC0.
pub fn white_level(data: &[u16]) -> u32 {
    let bits = data.iter().fold(0u16, |a, &v| a | v);
    if bits == 0 {
        return 0xFFFF;
    }
    let unused = bits.trailing_zeros().min(8);
    (0xFFFFu32 >> unused) << unused
}

fn srationals(tag: u16, values: &[f64]) -> Entry {
    let data = values
        .iter()
        .flat_map(|v| ((v * 10_000.0).round() as i32).to_le_bytes().into_iter().chain(10_000i32.to_le_bytes()))
        .collect();
    Entry { tag, kind: SRATIONAL, count: values.len() as u32, data }
}

fn shorts(tag: u16, values: &[u16]) -> Entry {
    Entry { tag, kind: SHORT, count: values.len() as u32, data: values.iter().flat_map(|v| v.to_le_bytes()).collect() }
}

fn longs(tag: u16, values: &[u32]) -> Entry {
    Entry { tag, kind: LONG, count: values.len() as u32, data: values.iter().flat_map(|v| v.to_le_bytes()).collect() }
}

fn bytes(tag: u16, values: &[u8]) -> Entry {
    Entry { tag, kind: BYTE, count: values.len() as u32, data: values.to_vec() }
}

/// The complete `.dng` file.
pub fn encode(raw: &RawImage, colour: &RawColour, info: &PhotoInfo) -> Vec<u8> {
    let n = raw.width as usize * raw.height as usize;
    assert!(raw.data.len() >= n, "raw frame shorter than {}x{}", raw.width, raw.height);
    let samples = &raw.data[..n];
    let stamp = info.taken.format("%Y:%m:%d %H:%M:%S").to_string();
    let (matrix, neutral) = colour_matrices(colour);
    let black = black_for_cfa(colour.black_levels.unwrap_or([0; 4]), raw.cfa);
    let strip_bytes = (n * 2) as u32;

    let entries = |strip_at: u32| {
        let mut e = vec![
            long(254, 0), // NewSubfileType: main image
            long(256, raw.width),
            long(257, raw.height),
            short(258, 16),
            short(259, 1),     // uncompressed
            short(262, 32803), // CFA
            ascii(271, MAKE),
            ascii(272, MODEL),
            long(273, strip_at),
            short(274, 1),
            short(277, 1),
            long(278, raw.height),
            long(279, strip_bytes),
            short(284, 1),
            ascii(305, SOFTWARE),
            ascii(306, &stamp),
            shorts(33421, &[2, 2]),      // CFARepeatPatternDim
            bytes(33422, &raw.cfa),      // CFAPattern
            ascii(36867, &stamp),        // DateTimeOriginal
            bytes(50706, &[1, 4, 0, 0]), // DNGVersion
            bytes(50707, &[1, 1, 0, 0]), // DNGBackwardVersion
            ascii(50708, UNIQUE_CAMERA_MODEL),
            shorts(50713, &[2, 2]), // BlackLevelRepeatDim
            longs(50714, &black),   // BlackLevel, CFA position order
            long(50717, white_level(samples)),
            srationals(50721, &matrix), // ColorMatrix1
            rationals(50728, &neutral.map(|v| ((v * 1_000_000.0).round() as u32, 1_000_000))),
            short(50778, 21), // CalibrationIlluminant1: D65
        ];
        if let Some(us) = info.exposure_us.filter(|us| *us > 0) {
            e.push(rationals(33434, &[(us as u32, 1_000_000)]));
        }
        if let Some(g) = info.gain.filter(|g| *g > 0.0) {
            e.push(short(34855, (g * 100.0).round().clamp(1.0, 65535.0) as u16));
        }
        e
    };
    let ifd_at = 8u32;
    let strip_at = ifd_at + ifd_len(&entries(0));
    let mut out = Vec::with_capacity(strip_at as usize + n * 2);
    out.extend_from_slice(b"II\x2a\x00");
    out.extend_from_slice(&ifd_at.to_le_bytes());
    out.extend(write_ifd(entries(strip_at), ifd_at, 0));
    debug_assert_eq!(out.len() as u32, strip_at);
    for v in samples {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDateTime;
    use exif::{Context, In, Tag, Value};

    fn info() -> PhotoInfo {
        PhotoInfo {
            taken: NaiveDateTime::parse_from_str("2026-11-19 14:03:07", "%Y-%m-%d %H:%M:%S").unwrap(),
            exposure_us: Some(16_000),
            gain: Some(4.0),
            gps: None,
        }
    }

    fn colour() -> RawColour {
        RawColour {
            black_levels: Some([3840, 3840, 3840, 3840]),
            colour_gains: Some((1.8, 1.5)),
            ccm: Some([1.6, -0.4, -0.2, -0.3, 1.5, -0.2, 0.0, -0.6, 1.6]),
        }
    }

    fn raw(w: u32, h: u32) -> RawImage {
        RawImage { width: w, height: h, cfa: RGGB, data: (0..w * h).map(|i| ((i % 1024) as u16) << 6).collect() }
    }

    fn field(e: &exif::Exif, tag: u16) -> &exif::Field {
        e.get_field(Tag(Context::Tiff, tag), In::PRIMARY).unwrap_or_else(|| panic!("tag {tag} missing"))
    }

    fn uint(e: &exif::Exif, tag: u16, i: usize) -> u32 {
        field(e, tag).value.get_uint(i).unwrap()
    }

    #[test]
    fn layout_and_cfa_tags() {
        let r = raw(8, 4);
        let bytes = encode(&r, &colour(), &info());
        let e = exif::Reader::new().read_raw(bytes.clone()).expect("valid TIFF");
        assert_eq!((uint(&e, 256, 0), uint(&e, 257, 0), uint(&e, 258, 0)), (8, 4, 16));
        assert_eq!(uint(&e, 262, 0), 32803);
        assert_eq!((0..4).map(|i| uint(&e, 33422, i)).collect::<Vec<_>>(), vec![0, 1, 1, 2]);
        assert_eq!((0..4).map(|i| uint(&e, 50706, i)).collect::<Vec<_>>(), vec![1, 4, 0, 0]);
        assert_eq!(uint(&e, 50714, 0), 3840);
        assert_eq!(uint(&e, 50717, 0), 0xFFC0);
        assert_eq!(uint(&e, 50778, 0), 21);
        assert_eq!(uint(&e, 34855, 0), 400);
        let (at, len) = (uint(&e, 273, 0) as usize, uint(&e, 279, 0) as usize);
        assert_eq!(len, 8 * 4 * 2);
        assert_eq!(bytes.len(), at + len);
        let first: Vec<u16> = bytes[at..at + len].chunks_exact(2).map(|b| u16::from_le_bytes([b[0], b[1]])).collect();
        assert_eq!(first, r.data);
        assert_eq!(field(&e, 50708).display_value().to_string(), "\"Raspberry Pi IMX296\"");
    }

    #[test]
    fn colour_matrix_maps_white_to_the_as_shot_neutral() {
        let (m, neutral) = colour_matrices(&colour());
        assert!((neutral[0] - 1.0 / 1.8).abs() < 1e-6 && (neutral[2] - 1.0 / 1.5).abs() < 1e-6);
        let white = mul_vec(&m, mul_vec(&SRGB_TO_XYZ, [1.0; 3]));
        let max = white.iter().cloned().fold(f64::MIN, f64::max);
        assert!((max - 1.0).abs() < 1e-9, "{white:?}");
        // The camera's neutral is the direction of the mapped white.
        for c in 0..3 {
            assert!((white[c] / white[1] - neutral[c]).abs() < 1e-6, "{white:?} vs {neutral:?}");
        }
        let bytes = encode(&raw(4, 2), &colour(), &info());
        let e = exif::Reader::new().read_raw(bytes).unwrap();
        match &field(&e, 50721).value {
            Value::SRational(v) => {
                assert_eq!(v.len(), 9);
                assert!((v[0].to_f64() - m[0]).abs() < 1e-4);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn missing_metadata_gives_safe_defaults() {
        let (m, neutral) = colour_matrices(&RawColour::default());
        assert_eq!(neutral, [1.0; 3]);
        let white = mul_vec(&m, mul_vec(&SRGB_TO_XYZ, [1.0; 3]));
        assert!(white.iter().all(|v| (v - 1.0).abs() < 1e-6), "{white:?}");
        let e = exif::Reader::new().read_raw(encode(&raw(4, 2), &RawColour::default(), &info())).unwrap();
        assert_eq!(uint(&e, 50714, 0), 0);
    }

    #[test]
    fn cfa_orders_and_black_levels() {
        assert_eq!(cfa_from_fourcc(*b"BG16"), Some([2, 1, 1, 0]));
        assert_eq!(cfa_from_fourcc(*b"RG10"), None);
        let levels = [10, 20, 30, 40]; // R, Gr, Gb, B
        assert_eq!(black_for_cfa(levels, RGGB), [10, 20, 30, 40]);
        assert_eq!(black_for_cfa(levels, [2, 1, 1, 0]), [40, 30, 20, 10]);
        assert_eq!(black_for_cfa(levels, [1, 0, 2, 1]), [20, 10, 40, 30]);
        assert_eq!(black_for_cfa(levels, [1, 2, 0, 1]), [30, 40, 10, 20]);
        let mut r = raw(4, 2);
        r.cfa = [2, 1, 1, 0];
        let e = exif::Reader::new().read_raw(encode(&r, &colour(), &info())).unwrap();
        assert_eq!((0..4).map(|i| uint(&e, 33422, i)).collect::<Vec<_>>(), vec![2, 1, 1, 0]);
    }

    #[test]
    fn white_level_ignores_unused_low_bits() {
        assert_eq!(white_level(&[0x40, 0xFFC0]), 0xFFC0);
        assert_eq!(white_level(&[1, 1023]), 0xFFFF);
        assert_eq!(white_level(&[0x100, 0x200]), 0xFF00, "at most 8 unused bits");
        assert_eq!(white_level(&[0, 0]), 0xFFFF);
    }

    /// Writes `$DNG_OUT`: 4 colour patches (sRGB red, green, blue, grey)
    /// taken back through `colour()` to camera raw, for checking with a raw
    /// decoder. `DNG_OUT=/tmp/t.dng cargo test -- --ignored write_patch_dng`
    #[test]
    #[ignore = "writes a file for external checks"]
    fn write_patch_dng() {
        let Ok(out) = std::env::var("DNG_OUT") else { return };
        let c = colour();
        let (gr, gb) = c.colour_gains.unwrap();
        let ccm: M3 = c.ccm.unwrap().map(|v| v as f64);
        let to_cam = inverse(&ccm).unwrap();
        let lin = |v: f64| if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) };
        let patches = [[0.8, 0.1, 0.1], [0.1, 0.7, 0.1], [0.1, 0.1, 0.8], [0.5, 0.5, 0.5]].map(|p| {
            let wb = mul_vec(&to_cam, p.map(lin));
            [wb[0] / gr as f64, wb[1], wb[2] / gb as f64]
        });
        let (w, h) = (64u32, 64u32);
        let black = 3840.0;
        let data = (0..w * h)
            .map(|i| {
                let (x, y) = (i % w, i / w);
                let p = &patches[((y / 32) * 2 + x / 32) as usize];
                let ch = RGGB[((y % 2) * 2 + x % 2) as usize] as usize;
                let v = black + p[ch].clamp(0.0, 1.0) * (65472.0 - black);
                ((v.round() as u16) >> 6) << 6
            })
            .collect();
        std::fs::write(out, encode(&RawImage { width: w, height: h, cfa: RGGB, data }, &c, &info())).unwrap();
    }

    #[test]
    fn inverse_round_trips() {
        let i = inverse(&SRGB_TO_XYZ).unwrap();
        let id = mul(&SRGB_TO_XYZ, &i);
        for (k, v) in id.iter().enumerate() {
            assert!((v - if k % 4 == 0 { 1.0 } else { 0.0 }).abs() < 1e-9);
        }
        assert_eq!(inverse(&[0.0; 9]), None);
    }
}
```

- [ ] **Step 4: Verify**

Run: `cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check` — Expected: all pass (6 new, 1 ignored), clean.

- [ ] **Step 5: Commit**

```bash
git add src/pipeline/mod.rs src/pipeline/exif.rs src/pipeline/dng.rs
git commit -m "feat(v2): DNG writer with colour matrix from ISP metadata"
```

---

### Task 3: Flash config and strobe driver

**Files:**
- Create: `v2/src/flash.rs`
- Modify: `v2/src/config.rs`, `v2/src/lib.rs` (add `pub mod flash;` after `pub mod display;`)
- Test: inline in `flash.rs` and `config.rs`

**Interfaces:**
- Produces: `FlashMode::Both`; `Flash::screen(&self) -> bool`, `Flash::strobe(&self) -> bool`; `Flash::pulse_ms: Option<u32>`; `pub const MAX_BURST_FRAMES: u32 = 12`; `pub trait Output: Send { fn set(&mut self, on: bool) -> anyhow::Result<()>; }`; `pub struct GpioLine` with `open(&Flash) -> anyhow::Result<Self>` (implements `Output`); `pub struct Strobe` with `start(Box<dyn Output>, max_on: Duration) -> anyhow::Result<Self>`, `set(&self, on: bool)`; dropping it turns the output off.

- [ ] **Step 1: Config**

Apply to `v2/src/config.rs` (the test changes are the failing tests for this step):
```diff
--- a/v2/src/config.rs
+++ b/v2/src/config.rs
@@ -109,6 +109,8 @@
 pub enum FlashMode {
     Screen,
     Strobe,
+    /// Screen and strobe together.
+    Both,
     None,
 }
 
@@ -116,15 +118,29 @@
 #[serde(default, deny_unknown_fields)]
 pub struct Flash {
     pub mode: FlashMode,
+    /// Light on this long before the shot, so auto-exposure meters it.
     pub lead_ms: u32,
+    /// Hard limit on how long the light stays on.
     pub max_ms: u32,
+    /// BCM number of the strobe's output line.
     pub gpio_line: u32,
-    pub pulse_ms: u32,
+    /// Accepted from older config files and ignored: the strobe is lit for
+    /// the same window as the screen flash (spec §4.5).
+    pub pulse_ms: Option<u32>,
     pub active_high: bool,
 }
 impl Default for Flash {
     fn default() -> Self {
-        Self { mode: FlashMode::Screen, lead_ms: 300, max_ms: 800, gpio_line: 17, pulse_ms: 150, active_high: true }
+        Self { mode: FlashMode::Screen, lead_ms: 300, max_ms: 800, gpio_line: 17, pulse_ms: None, active_high: true }
+    }
+}
+impl Flash {
+    pub fn screen(&self) -> bool {
+        matches!(self.mode, FlashMode::Screen | FlashMode::Both)
+    }
+
+    pub fn strobe(&self) -> bool {
+        matches!(self.mode, FlashMode::Strobe | FlashMode::Both)
     }
 }
 
@@ -170,6 +186,9 @@
     }
 }
 
+/// Longest burst: 12 frames are 0.2 s at 60 fps and ~57 MB of RGB.
+pub const MAX_BURST_FRAMES: u32 = 12;
+
 fn looks_like_uuid(s: &str) -> bool {
     s.len() == 36
         && s.chars()
@@ -201,12 +220,20 @@
             Err(e) => tracing::warn!("ignoring malformed .env: {e}"),
         }
         cfg.immich_api_key = api_key_from(|k| std::env::var(k).ok());
+        if cfg.flash.pulse_ms.is_some() {
+            tracing::warn!(
+                "flash.pulse_ms is no longer used: the strobe is lit from flash.lead_ms before the shot until the burst ends"
+            );
+        }
         Ok(cfg)
     }
 
     fn validate(&self) -> anyhow::Result<()> {
-        if self.capture.burst_frames == 0 {
-            bail!("capture.burst_frames must be >= 1");
+        if !(1..=MAX_BURST_FRAMES).contains(&self.capture.burst_frames) {
+            bail!("capture.burst_frames must be 1..={MAX_BURST_FRAMES}");
+        }
+        if self.capture.max_shift_px > 32 {
+            bail!("capture.max_shift_px must be <= 32");
         }
         if !(1..=100).contains(&self.capture.jpeg_quality) {
             bail!("capture.jpeg_quality must be 1..=100");
@@ -237,7 +264,7 @@
         if self.flash.lead_ms as u64 >= self.timing.countdown_s as u64 * 1000 {
             bail!("flash.lead_ms must be shorter than the countdown (timing.countdown_s)");
         }
-        if self.button.gpio_line == self.flash.gpio_line && self.flash.mode == FlashMode::Strobe {
+        if self.button.gpio_line == self.flash.gpio_line && self.flash.strobe() {
             bail!("button.gpio_line and flash.gpio_line must differ");
         }
         Ok(())
@@ -308,6 +335,9 @@
     #[test]
     fn invalid_values_are_rejected() {
         assert!(Config::from_toml_str("[capture]\nburst_frames = 0\n").is_err());
+        assert!(Config::from_toml_str("[capture]\nburst_frames = 13\n").is_err());
+        assert!(Config::from_toml_str("[capture]\nmax_shift_px = 33\n").is_err());
+        assert!(Config::from_toml_str("[flash]\nmode = \"both\"\ngpio_line = 27\n").is_err());
         assert!(Config::from_toml_str("[capture]\njpeg_quality = 0\n").is_err());
         assert!(Config::from_toml_str("[flash]\nmode = \"strobe\"\ngpio_line = 27\n").is_err());
         assert!(Config::from_toml_str("[flash]\nlead_ms = 900\nmax_ms = 800\n").is_err());
@@ -325,6 +355,18 @@
     }
 
     #[test]
+    fn flash_modes_and_old_pulse_ms() {
+        let both = Config::from_toml_str("[flash]\nmode = \"both\"\n").unwrap().flash;
+        assert!(both.screen() && both.strobe());
+        let strobe = Config::from_toml_str("[flash]\nmode = \"strobe\"\npulse_ms = 150\n").unwrap().flash;
+        assert!(!strobe.screen() && strobe.strobe());
+        assert_eq!(strobe.pulse_ms, Some(150), "old files still load");
+        let none = Config::from_toml_str("[flash]\nmode = \"none\"\n").unwrap().flash;
+        assert!(!none.screen() && !none.strobe());
+        assert!(Config::default().flash.screen());
+    }
+
+    #[test]
     fn gps_omitted_when_zero() {
         let mut i = Immich::default();
         assert_eq!(i.gps(), None);
```

Run: `cargo test config` — Expected: PASS (the new `flash_modes_and_old_pulse_ms` and the extended `invalid_values_are_rejected`).

- [ ] **Step 2: Write the failing strobe tests**

Create `flash.rs` with the Interfaces items stubbed `todo!()`, plus:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    const MS: Duration = Duration::from_millis(1);

    #[derive(Clone, Default)]
    struct Probe(Arc<Mutex<Vec<(bool, Instant)>>>);
    impl Output for Probe {
        fn set(&mut self, on: bool) -> anyhow::Result<()> {
            self.0.lock().unwrap().push((on, Instant::now()));
            Ok(())
        }
    }
    impl Probe {
        fn states(&self) -> Vec<bool> {
            self.0.lock().unwrap().iter().map(|(on, _)| *on).collect()
        }
        fn wait_for(&self, n: usize) {
            let until = Instant::now() + Duration::from_secs(2);
            while self.0.lock().unwrap().len() < n {
                assert!(Instant::now() < until, "only {:?}", self.states());
                std::thread::sleep(MS);
            }
        }
    }

    #[test]
    fn on_then_off() {
        let p = Probe::default();
        let s = Strobe::start(Box::new(p.clone()), Duration::from_secs(5)).unwrap();
        s.set(true);
        s.set(true); // repeated "on" is not a new pulse
        s.set(false);
        s.set(false);
        p.wait_for(3);
        drop(s);
        assert_eq!(p.states(), vec![false, true, false, false], "starts off, one pulse, off at drop");
    }

    #[test]
    fn hard_limit_turns_it_off_by_itself() {
        let p = Probe::default();
        let s = Strobe::start(Box::new(p.clone()), 50 * MS).unwrap();
        s.set(true);
        p.wait_for(3);
        let log = p.0.lock().unwrap().clone();
        assert_eq!(log.iter().map(|e| e.0).collect::<Vec<_>>(), vec![false, true, false]);
        let lit = log[2].1 - log[1].1;
        assert!(lit >= 50 * MS && lit < 500 * MS, "{lit:?}");
        // A later "on" works again.
        s.set(true);
        p.wait_for(4);
        assert!(p.states()[3]);
    }

    #[test]
    fn drop_turns_it_off() {
        let p = Probe::default();
        let s = Strobe::start(Box::new(p.clone()), Duration::from_secs(5)).unwrap();
        s.set(true);
        p.wait_for(2);
        drop(s);
        assert_eq!(p.states(), vec![false, true, false]);
    }
}
```

Run: `cargo test flash` — Expected: FAIL.

- [ ] **Step 3: Implement**

`v2/src/flash.rs`:
```rust
//! GPIO strobe (spec §4.5). The light is driven from its own thread so the
//! hard limit (`flash.max_ms`) holds even if the render loop stalls: every
//! "on" carries a deadline after which the thread turns the line off by
//! itself. Dropping the [`Strobe`] turns it off and releases the line.

use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::Context;

use crate::config::Flash;
use crate::input::line_name;

/// Something that can be switched on and off (a GPIO line, or a test probe).
pub trait Output: Send {
    fn set(&mut self, on: bool) -> anyhow::Result<()>;
}

/// The strobe's output line, requested as an output that starts off.
pub struct GpioLine {
    req: gpiocdev::Request,
}

impl GpioLine {
    pub fn open(cfg: &Flash) -> anyhow::Result<Self> {
        let name = line_name(cfg.gpio_line);
        let found = gpiocdev::find_named_line(&name).with_context(|| format!("GPIO line {name} not found"))?;
        let mut builder = gpiocdev::Request::builder();
        builder.with_consumer("pibooth-strobe").with_found_line(&found).as_output(gpiocdev::line::Value::Inactive);
        if !cfg.active_high {
            builder.as_active_low();
        }
        let req = builder.request().with_context(|| format!("requesting {name}"))?;
        tracing::info!(line = %name, chip = %found.chip.display(), active_high = cfg.active_high, "strobe ready");
        Ok(Self { req })
    }
}

impl Output for GpioLine {
    fn set(&mut self, on: bool) -> anyhow::Result<()> {
        let v = if on { gpiocdev::line::Value::Active } else { gpiocdev::line::Value::Inactive };
        self.req.set_lone_value(v).context("setting strobe line")?;
        Ok(())
    }
}

pub struct Strobe {
    tx: Option<Sender<bool>>,
    thread: Option<JoinHandle<()>>,
}

impl Strobe {
    /// Drive `output` from a new thread; each "on" lasts at most `max_on`.
    pub fn start(output: Box<dyn Output>, max_on: Duration) -> anyhow::Result<Self> {
        let (tx, rx) = channel();
        let thread = std::thread::Builder::new().name("strobe".into()).spawn(move || run(output, rx, max_on))?;
        Ok(Self { tx: Some(tx), thread: Some(thread) })
    }

    /// Never blocks.
    pub fn set(&self, on: bool) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(on);
        }
    }
}

impl Drop for Strobe {
    fn drop(&mut self) {
        self.tx.take();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn run(mut output: Box<dyn Output>, rx: Receiver<bool>, max_on: Duration) {
    let mut set = |on: bool| {
        if let Err(e) = output.set(on) {
            tracing::error!("strobe: {e:#}");
        }
    };
    set(false);
    let mut deadline: Option<Instant> = None;
    loop {
        let msg = match deadline {
            Some(d) => rx.recv_timeout(d.saturating_duration_since(Instant::now())),
            None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };
        match msg {
            Ok(true) => {
                if deadline.is_none() {
                    set(true);
                    deadline = Some(Instant::now() + max_on);
                }
            }
            Ok(false) => {
                if deadline.take().is_some() {
                    set(false);
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                tracing::warn!(max_ms = max_on.as_millis() as u64, "strobe forced off after flash.max_ms");
                deadline = None;
                set(false);
            }
            Err(RecvTimeoutError::Disconnected) => {
                set(false);
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    const MS: Duration = Duration::from_millis(1);

    #[derive(Clone, Default)]
    struct Probe(Arc<Mutex<Vec<(bool, Instant)>>>);
    impl Output for Probe {
        fn set(&mut self, on: bool) -> anyhow::Result<()> {
            self.0.lock().unwrap().push((on, Instant::now()));
            Ok(())
        }
    }
    impl Probe {
        fn states(&self) -> Vec<bool> {
            self.0.lock().unwrap().iter().map(|(on, _)| *on).collect()
        }
        fn wait_for(&self, n: usize) {
            let until = Instant::now() + Duration::from_secs(2);
            while self.0.lock().unwrap().len() < n {
                assert!(Instant::now() < until, "only {:?}", self.states());
                std::thread::sleep(MS);
            }
        }
    }

    #[test]
    fn on_then_off() {
        let p = Probe::default();
        let s = Strobe::start(Box::new(p.clone()), Duration::from_secs(5)).unwrap();
        s.set(true);
        s.set(true); // repeated "on" is not a new pulse
        s.set(false);
        s.set(false);
        p.wait_for(3);
        drop(s);
        assert_eq!(p.states(), vec![false, true, false, false], "starts off, one pulse, off at drop");
    }

    #[test]
    fn hard_limit_turns_it_off_by_itself() {
        let p = Probe::default();
        let s = Strobe::start(Box::new(p.clone()), 50 * MS).unwrap();
        s.set(true);
        p.wait_for(3);
        let log = p.0.lock().unwrap().clone();
        assert_eq!(log.iter().map(|e| e.0).collect::<Vec<_>>(), vec![false, true, false]);
        let lit = log[2].1 - log[1].1;
        assert!(lit >= 50 * MS && lit < 500 * MS, "{lit:?}");
        // A later "on" works again.
        s.set(true);
        p.wait_for(4);
        assert!(p.states()[3]);
    }

    #[test]
    fn drop_turns_it_off() {
        let p = Probe::default();
        let s = Strobe::start(Box::new(p.clone()), Duration::from_secs(5)).unwrap();
        s.set(true);
        p.wait_for(2);
        drop(s);
        assert_eq!(p.states(), vec![false, true, false]);
    }
}
```

- [ ] **Step 4: Verify**

Run: `cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check` — Expected: all pass, clean.

- [ ] **Step 5: Commit**

```bash
git add src/config.rs src/lib.rs src/flash.rs
git commit -m "feat(v2): flash modes incl. both, GPIO strobe driver with hard limit"
```

---

### Task 4: Bursts from camera to photo, DNG and strobe wiring

This changes the camera→worker types, so all of these files change in one commit (the crate does not compile half-way).

**Files:**
- Modify: `v2/src/camera/mod.rs`, `v2/src/camera/fake.rs`, `v2/src/frame.rs`, `v2/src/state.rs`, `v2/src/storage.rs`, `v2/src/pipeline/photo.rs`, `v2/src/pipeline/worker.rs`, `v2/src/display/controller.rs`, `v2/src/main.rs`, `v2/src/camera/pi.rs`
- Test: inline (diffs include the updated and new tests)

**Interfaces:**
- Consumes: Tasks 1–3 (`stack`, `dng`, `Flash::screen/strobe`, `Strobe`).
- Produces: `CameraCommand::Capture { id: u64, frames: u32 }`; `pub struct CapturedBurst { pub id: u64, pub frames: Vec<Frame>, pub meta: CaptureMeta, pub raw: Option<RawCapture> }` (replaces `CapturedFrame`); `pub struct RawCapture { pub image: RawImage, pub colour: RawColour }`; `CameraLink::captures: Sender<CapturedBurst>`; `pub const MAX_UNLOCKED_FRAMES: u32 = 8`; `pub struct BurstCollector` with `new(id, frames)`, `is_first()`, `wants(locked: bool) -> bool`, `push(frame, first: Option<(CaptureMeta, Option<RawCapture>)>) -> Option<CapturedBurst>`; `FakeCamera::with_raw(bool)`, `fake::mosaic`; `frame::copy_u16_le`; `Timings { flash, burst_frames, .. }` (replaces `screen_flash`); `Command::Capture { id, frames }`; `PhotoJob { frames: Vec<Vec<u8>>, stack, max_shift_px, raw, .. }` (replaces `rgb`); `PhotoDone { dng, stack, .. }`; `Timings::{stack_ms, dng_ms}` in `photo.rs`; `Controller::set_strobe(Strobe)`; `PiCamera::new(CameraCfg, raw: bool)`.

- [ ] **Step 1: Camera types, burst collector, fake camera, raw copy helper**

```diff
--- a/v2/src/camera/mod.rs
+++ b/v2/src/camera/mod.rs
@@ -5,6 +5,7 @@
 use std::sync::mpsc::Sender;
 
 use crate::frame::{Frame, FrameMailbox};
+use crate::pipeline::dng::{RawColour, RawImage};
 
 pub mod fake;
 
@@ -47,35 +48,105 @@
 pub enum CameraCommand {
     /// Switch to high-quality noise reduction; auto-exposure keeps metering.
     PrepareCapture,
-    /// Deliver the next complete frame as capture `id` (full resolution,
-    /// never mirrored); it is metered under the flash since `PrepareCapture`.
-    /// Exposure and white balance are then locked for the frames after it
-    /// (used by milestone 5 bursts) until `ReleaseCapture`.
-    Capture { id: u64 },
+    /// Deliver a burst of `frames` (≥ 1) full-resolution, never mirrored
+    /// frames as capture `id`. The first is the next complete frame, metered
+    /// under the flash since `PrepareCapture`; exposure and white balance are
+    /// then locked, and the rest are the first `frames - 1` frames taken with
+    /// the lock in effect (spec §4.2). Locked until `ReleaseCapture`.
+    Capture { id: u64, frames: u32 },
     /// Back to preview settings: auto exposure/white balance, fast noise reduction.
     ReleaseCapture,
 }
 
-/// Exposure settings the frame was taken with (for EXIF).
+/// Exposure settings the first frame was taken with (for EXIF).
 #[derive(Debug, Clone, Copy, PartialEq, Default)]
 pub struct CaptureMeta {
     pub exposure_us: Option<i32>,
     pub gain: Option<f32>,
 }
 
+/// The raw Bayer frame matching the first burst frame (`capture.save_dng`).
+#[derive(Debug, Clone, PartialEq)]
+pub struct RawCapture {
+    pub image: RawImage,
+    pub colour: RawColour,
+}
+
 #[derive(Debug)]
-pub struct CapturedFrame {
+pub struct CapturedBurst {
     pub id: u64,
-    pub frame: Frame,
+    /// In capture order; `frames[0]` is the photo metered under the flash.
+    pub frames: Vec<Frame>,
     pub meta: CaptureMeta,
+    pub raw: Option<RawCapture>,
+}
+
+/// After the photo, frames still in flight were requested before the lock;
+/// give up waiting for a locked one after this many and take them anyway.
+pub const MAX_UNLOCKED_FRAMES: u32 = 8;
+
+/// Collects one burst from completed frames (spec §4.2): the first frame
+/// after `Capture` is the photo; the rest are taken from the first frame whose
+/// metadata shows the exposure lock in effect.
+#[derive(Debug)]
+pub struct BurstCollector {
+    burst: CapturedBurst,
+    want: usize,
+    locked: bool,
+    skipped: u32,
+}
+
+impl BurstCollector {
+    pub fn new(id: u64, frames: u32) -> Self {
+        let burst = CapturedBurst { id, frames: Vec::new(), meta: CaptureMeta::default(), raw: None };
+        Self { burst, want: frames.max(1) as usize, locked: false, skipped: 0 }
+    }
+
+    /// The next frame is the photo (record its metadata and raw frame).
+    pub fn is_first(&self) -> bool {
+        self.burst.frames.is_empty()
+    }
+
+    /// Whether to keep the next completed frame; `locked`: its metadata
+    /// reports manual exposure (the lock reached the sensor).
+    pub fn wants(&mut self, locked: bool) -> bool {
+        if self.is_first() || self.locked {
+            return true;
+        }
+        if locked {
+            self.locked = true;
+            return true;
+        }
+        self.skipped += 1;
+        if self.skipped >= MAX_UNLOCKED_FRAMES {
+            tracing::warn!(id = self.burst.id, "exposure lock not reported; continuing the burst without it");
+            self.locked = true;
+        }
+        false
+    }
+
+    /// Add a kept frame (`first` only for the photo). Returns the finished
+    /// burst once it has all its frames.
+    pub fn push(&mut self, frame: Frame, first: Option<(CaptureMeta, Option<RawCapture>)>) -> Option<CapturedBurst> {
+        if let Some((meta, raw)) = first {
+            self.burst.meta = meta;
+            self.burst.raw = raw;
+        }
+        self.burst.frames.push(frame);
+        if self.burst.frames.len() < self.want {
+            return None;
+        }
+        let empty = CapturedBurst { id: self.burst.id, frames: Vec::new(), meta: self.burst.meta, raw: None };
+        Some(std::mem::replace(&mut self.burst, empty))
+    }
 }
 
 /// Where a running camera sends its output.
 pub struct CameraLink {
     /// Latest preview frame (overwritten, never blocks the camera).
     pub mailbox: Arc<FrameMailbox>,
-    /// Captured frames, one per `Capture` command.
-    pub captures: Sender<CapturedFrame>,
+    /// Captured bursts, one per `Capture` command.
+    pub captures: Sender<CapturedBurst>,
 }
 
 /// A camera runs its own thread. It is `Send` so it can be started and
@@ -95,6 +166,43 @@
     use super::*;
     use std::time::Duration;
 
+    fn frame(seq: u64) -> Frame {
+        Frame { width: 1, height: 1, data: vec![0; 3], seq, captured_at: std::time::Instant::now() }
+    }
+
+    #[test]
+    fn burst_keeps_the_photo_then_locked_frames() {
+        let mut b = BurstCollector::new(7, 3);
+        assert!(b.is_first() && b.wants(false), "the photo is the next frame");
+        let meta = CaptureMeta { exposure_us: Some(9000), gain: Some(2.0) };
+        assert!(b.push(frame(10), Some((meta, None))).is_none());
+        assert!(!b.is_first());
+        assert!(!b.wants(false) && !b.wants(false), "in-flight unlocked frames are skipped");
+        assert!(b.wants(true));
+        assert!(b.push(frame(13), None).is_none());
+        assert!(b.wants(false), "once locked, every frame counts");
+        let done = b.push(frame(14), None).expect("complete");
+        assert_eq!((done.id, done.meta), (7, meta));
+        assert_eq!(done.frames.iter().map(|f| f.seq).collect::<Vec<_>>(), vec![10, 13, 14]);
+    }
+
+    #[test]
+    fn burst_does_not_wait_forever_for_the_lock() {
+        let mut b = BurstCollector::new(1, 2);
+        assert!(b.wants(false));
+        b.push(frame(0), Some((CaptureMeta::default(), None)));
+        let skipped = (0..20).take_while(|_| !b.wants(false)).count() as u32;
+        assert_eq!(skipped, MAX_UNLOCKED_FRAMES);
+        assert!(b.push(frame(9), None).is_some());
+    }
+
+    #[test]
+    fn single_frame_burst_is_just_the_photo() {
+        let mut b = BurstCollector::new(2, 0); // 0 is treated as 1
+        assert!(b.wants(false));
+        assert_eq!(b.push(frame(5), Some((CaptureMeta::default(), None))).unwrap().frames.len(), 1);
+    }
+
     #[test]
     fn backoff_doubles_to_max_and_resets() {
         let mut b = Backoff::new(Duration::from_secs(1), Duration::from_secs(30));
```

```diff
--- a/v2/src/camera/fake.rs
+++ b/v2/src/camera/fake.rs
@@ -7,12 +7,14 @@
 use std::thread::JoinHandle;
 use std::time::{Duration, Instant};
 
-use super::{Camera, CameraCommand, CameraLink, CaptureMeta, CapturedFrame};
+use super::{Camera, CameraCommand, CameraLink, CaptureMeta, CapturedBurst, RawCapture};
 use crate::frame::Frame;
+use crate::pipeline::dng::{RGGB, RawColour, RawImage};
 
 pub struct FakeCamera {
     size: (u32, u32),
     fps: u32,
+    raw: bool,
     running: Arc<AtomicBool>,
     thread: Option<JoinHandle<()>>,
     commands: Option<Sender<CameraCommand>>,
@@ -20,8 +22,27 @@
 
 impl FakeCamera {
     pub fn new(size: (u32, u32), fps: u32) -> Self {
-        Self { size, fps, running: Arc::new(AtomicBool::new(false)), thread: None, commands: None }
+        Self { size, fps, raw: false, running: Arc::new(AtomicBool::new(false)), thread: None, commands: None }
     }
+
+    /// Also deliver a raw Bayer frame with each burst (like `capture.save_dng`).
+    pub fn with_raw(mut self, raw: bool) -> Self {
+        self.raw = raw;
+        self
+    }
+}
+
+/// RGGB mosaic of an RGB frame, 8-bit values at the top of 16 bits.
+pub fn mosaic(rgb: &[u8], width: u32, height: u32) -> RawImage {
+    let w = width as usize;
+    let data = (0..w * height as usize)
+        .map(|i| {
+            let (x, y) = (i % w, i / w);
+            let c = RGGB[(y % 2) * 2 + x % 2] as usize;
+            (rgb[i * 3 + c] as u16) << 8
+        })
+        .collect();
+    RawImage { width, height, cfa: RGGB, data }
 }
 
 /// Grey horizontal gradient, a white vertical bar moving 8 px per frame,
@@ -62,25 +83,33 @@
         self.running.store(true, Ordering::SeqCst);
         let running = self.running.clone();
         let (w, h) = self.size;
+        let with_raw = self.raw;
         let period = Duration::from_secs_f64(1.0 / self.fps as f64);
         let (cmd_tx, cmd_rx) = channel();
         self.commands = Some(cmd_tx);
         self.thread = Some(std::thread::Builder::new().name("fake-camera".into()).spawn(move || {
             let mut next = Instant::now();
             let mut seq = 0u64;
-            let mut pending = None;
+            // The burst being collected and how many frames it still needs.
+            let mut burst: Option<(CapturedBurst, u32)> = None;
             while running.load(Ordering::SeqCst) {
                 for cmd in cmd_rx.try_iter() {
-                    if let CameraCommand::Capture { id } = cmd {
-                        pending = Some(id);
+                    if let CameraCommand::Capture { id, frames } = cmd {
+                        let meta = CaptureMeta { exposure_us: Some(10_000), gain: Some(1.0) };
+                        burst = Some((CapturedBurst { id, frames: Vec::new(), meta, raw: None }, frames.max(1)));
                     }
                 }
                 let mut data = link.mailbox.take_buffer(Frame::byte_len(w, h));
                 fill_test_pattern(&mut data, w, h, seq);
-                if let Some(id) = pending.take() {
-                    let frame = Frame { width: w, height: h, data: data.clone(), seq, captured_at: Instant::now() };
-                    let meta = CaptureMeta { exposure_us: Some(10_000), gain: Some(1.0) };
-                    let _ = link.captures.send(CapturedFrame { id, frame, meta });
+                if let Some((b, want)) = burst.as_mut() {
+                    if b.frames.is_empty() && with_raw {
+                        b.raw = Some(RawCapture { image: mosaic(&data, w, h), colour: RawColour::default() });
+                    }
+                    b.frames.push(Frame { width: w, height: h, data: data.clone(), seq, captured_at: Instant::now() });
+                    if b.frames.len() as u32 >= *want {
+                        let (done, _) = burst.take().unwrap();
+                        let _ = link.captures.send(done);
+                    }
                 }
                 link.mailbox.publish(Frame { width: w, height: h, data, seq, captured_at: Instant::now() });
                 seq += 1;
@@ -136,7 +165,7 @@
         assert_eq!(px(40, 20)[0], px(40, 20)[1], "grey");
     }
 
-    fn link() -> (CameraLink, Arc<FrameMailbox>, std::sync::mpsc::Receiver<CapturedFrame>) {
+    fn link() -> (CameraLink, Arc<FrameMailbox>, std::sync::mpsc::Receiver<CapturedBurst>) {
         let mb = FrameMailbox::new();
         let (tx, rx) = channel();
         (CameraLink { mailbox: mb.clone(), captures: tx }, mb, rx)
@@ -162,23 +191,41 @@
         cam.start(link().0).unwrap();
         cam.stop();
         cam.stop();
-        cam.command(CameraCommand::Capture { id: 1 }); // ignored when stopped
+        cam.command(CameraCommand::Capture { id: 1, frames: 1 }); // ignored when stopped
     }
 
     #[test]
-    fn capture_delivers_one_full_frame_with_its_id() {
+    fn capture_delivers_one_burst_with_its_id() {
         let (link, _mb, rx) = link();
         let mut cam = FakeCamera::new((64, 32), 100);
         cam.start(link).unwrap();
         cam.command(CameraCommand::PrepareCapture);
-        cam.command(CameraCommand::Capture { id: 42 });
+        cam.command(CameraCommand::Capture { id: 42, frames: 3 });
         let got = rx.recv_timeout(Duration::from_secs(1)).unwrap();
         assert_eq!(got.id, 42);
-        assert_eq!(got.frame.data.len(), 64 * 32 * 3);
-        assert_eq!(&got.frame.data[0..3], &[255, 0, 0], "not mirrored: marker top-left");
+        assert_eq!(got.frames.len(), 3);
+        assert!(got.frames.windows(2).all(|f| f[1].seq == f[0].seq + 1), "consecutive frames");
+        assert_eq!(got.frames[0].data.len(), 64 * 32 * 3);
+        assert_eq!(&got.frames[0].data[0..3], &[255, 0, 0], "not mirrored: marker top-left");
         assert!(got.meta.exposure_us.is_some());
+        assert!(got.raw.is_none(), "raw only when enabled");
         std::thread::sleep(Duration::from_millis(50));
-        assert!(rx.try_recv().is_err(), "exactly one frame per capture");
+        assert!(rx.try_recv().is_err(), "exactly one burst per capture");
+        cam.stop();
+    }
+
+    #[test]
+    fn raw_frame_is_the_mosaic_of_the_first_frame() {
+        let (link, _mb, rx) = link();
+        let mut cam = FakeCamera::new((64, 32), 100).with_raw(true);
+        cam.start(link).unwrap();
+        cam.command(CameraCommand::Capture { id: 1, frames: 2 });
+        let got = rx.recv_timeout(Duration::from_secs(1)).unwrap();
+        let raw = got.raw.expect("raw frame");
+        assert_eq!((raw.image.width, raw.image.height, raw.image.data.len()), (64, 32, 64 * 32));
+        assert_eq!(raw.image, mosaic(&got.frames[0].data, 64, 32));
+        // Top-left marker is red: R sample bright, the G next to it dark.
+        assert_eq!((raw.image.data[0], raw.image.data[1]), (255 << 8, 0));
         cam.stop();
     }
 }
```

```diff
--- a/v2/src/frame.rs
+++ b/v2/src/frame.rs
@@ -37,6 +37,19 @@
     }
 }
 
+/// Copy a raw Bayer plane (rows `stride` bytes apart, little-endian 16-bit
+/// samples) into tightly packed samples.
+pub fn copy_u16_le(src: &[u8], stride: usize, width: u32, height: u32) -> Vec<u16> {
+    let (w, h) = (width as usize, height as usize);
+    assert!(stride >= w * 2, "stride {stride} < row {}", w * 2);
+    assert!(h == 0 || src.len() >= stride * (h - 1) + w * 2, "src too small");
+    let mut out = Vec::with_capacity(w * h);
+    for y in 0..h {
+        out.extend(src[y * stride..y * stride + w * 2].chunks_exact(2).map(|b| u16::from_le_bytes([b[0], b[1]])));
+    }
+    out
+}
+
 /// Spare buffers kept for reuse. Writer + reader + one in flight.
 const MAX_SPARES: usize = 3;
 
@@ -143,6 +156,13 @@
     }
 
     #[test]
+    fn copy_u16_le_strips_padding_and_decodes() {
+        // 2 rows of 2 samples, stride 6 bytes (2 bytes padding).
+        let src = [0x01, 0x02, 0x03, 0x04, 0xEE, 0xEE, 0x05, 0x06, 0x07, 0x08];
+        assert_eq!(copy_u16_le(&src, 6, 2, 2), vec![0x0201, 0x0403, 0x0605, 0x0807]);
+    }
+
+    #[test]
     fn reader_gets_newest_and_only_once() {
         let mb = FrameMailbox::new();
         assert!(mb.take_latest().is_none());
```

- [ ] **Step 2: State machine**

```diff
--- a/v2/src/state.rs
+++ b/v2/src/state.rs
@@ -3,14 +3,13 @@
 //! The machine only decides; the app carries out the returned [`Command`]s.
 //!
 //! Capture flow: `lead` before the deadline the camera is prepared (high
-//! quality noise reduction; screen flash on in screen mode) so auto-exposure
-//! meters the lit scene. At the deadline the next complete frame is the
-//! photo (metered under the flash since the prepare step); the camera then
-//! locks exposure and white balance for the frames after it (used by
-//! milestone 5 bursts). Once the photo arrives the flash goes off, the lock
-//! is released and the photo is processed; review shows the result
-//! or "Oops, try again". Every capture has an id; results for another id are
-//! ignored.
+//! quality noise reduction; flash on unless `flash.mode = "none"`) so
+//! auto-exposure meters the lit scene. At the deadline the camera captures a
+//! burst: the next complete frame (metered under the flash since the prepare
+//! step), then frames with exposure and white balance locked. Once the burst
+//! arrives the flash goes off, the lock is released and the burst is
+//! processed; review shows the result or "Oops, try again". Every capture
+//! has an id; results for another id are ignored.
 
 use std::time::{Duration, Instant};
 
@@ -19,12 +18,14 @@
     pub countdown: Duration,
     pub review: Duration,
     pub idle: Duration,
-    /// Prepare the camera (and turn the screen flash on) this long before the shot.
+    /// Prepare the camera (and turn the flash on) this long before the shot.
     pub prepare_lead: Duration,
-    /// Whether the screen flash is used (`flash.mode = "screen"`).
-    pub screen_flash: bool,
-    /// Longest the screen flash may stay on.
+    /// Whether a flash (screen, strobe or both) is used.
+    pub flash: bool,
+    /// Longest the flash may stay on.
     pub flash_max: Duration,
+    /// Frames per capture (`capture.burst_frames`).
+    pub burst_frames: u32,
     /// Give up on a frame that never arrives.
     pub capture_timeout: Duration,
     /// Give up on processing that never finishes.
@@ -51,7 +52,7 @@
 pub enum Event {
     /// Button or space bar pressed (one edge = one event).
     Press,
-    /// The camera delivered the frame for capture `id`.
+    /// The camera delivered the burst for capture `id`.
     FrameCaptured {
         id: u64,
     },
@@ -71,11 +72,10 @@
     PrepareCapture,
     FlashOn,
     FlashOff,
-    /// Deliver the next complete frame as capture `id` (metered under the
-    /// flash since `PrepareCapture`); exposure/white balance are then locked
-    /// for the frames after it.
+    /// Capture a burst of `frames` as capture `id` (see the module docs).
     Capture {
         id: u64,
+        frames: u32,
     },
     /// Back to preview settings: auto exposure/white balance, fast noise reduction.
     ReleaseCapture,
@@ -152,7 +152,7 @@
             Phase::Countdown { deadline, prepared } => {
                 if !prepared && now + self.timings.prepare_lead >= deadline {
                     out.push(Command::PrepareCapture);
-                    if self.timings.screen_flash {
+                    if self.timings.flash {
                         self.flash_since = Some(now);
                         out.push(Command::FlashOn);
                     }
@@ -162,7 +162,7 @@
                     let id = self.next_id;
                     self.next_id += 1;
                     self.phase = Phase::Capturing { id, since: now };
-                    out.push(Command::Capture { id });
+                    out.push(Command::Capture { id, frames: self.timings.burst_frames });
                 }
             }
             Phase::Capturing { since, .. } => {
@@ -228,7 +228,8 @@
             review: 5 * S,
             idle: 120 * S,
             prepare_lead: 300 * MS,
-            screen_flash: true,
+            flash: true,
+            burst_frames: 6,
             flash_max: 800 * MS,
             capture_timeout: 5 * S,
             processing_timeout: 10 * S,
@@ -241,7 +242,7 @@
         b.handle(Event::Press, t0);
         b.tick(t0 + 4700 * MS);
         let cmds = b.tick(t0 + 5 * S);
-        let Some(Command::Capture { id }) = cmds.last().copied() else { panic!("{cmds:?}") };
+        let Some(Command::Capture { id, .. }) = cmds.last().copied() else { panic!("{cmds:?}") };
         (b, id)
     }
 
@@ -275,14 +276,14 @@
         assert_eq!(b.tick(t0 + 4700 * MS), vec![Command::PrepareCapture, Command::FlashOn]);
         assert!(b.view(t0 + 4700 * MS).flash);
         assert!(b.tick(t0 + 4800 * MS).is_empty(), "only once");
-        assert_eq!(b.tick(t0 + 5 * S), vec![Command::Capture { id: 1 }]);
+        assert_eq!(b.tick(t0 + 5 * S), vec![Command::Capture { id: 1, frames: 6 }]);
         assert_eq!(b.phase(), Phase::Capturing { id: 1, since: t0 + 5 * S });
     }
 
     #[test]
-    fn without_screen_flash_still_prepares() {
+    fn without_flash_still_prepares() {
         let t0 = Instant::now();
-        let mut b = Booth::new(Timings { screen_flash: false, ..timings() }, t0);
+        let mut b = Booth::new(Timings { flash: false, ..timings() }, t0);
         b.handle(Event::Press, t0);
         assert_eq!(b.tick(t0 + 4700 * MS), vec![Command::PrepareCapture]);
         assert!(!b.view(t0 + 4800 * MS).flash);
@@ -333,7 +334,7 @@
         b.tick(t0 + 10 * S);
         b.handle(Event::Press, t0 + 11 * S);
         b.tick(t0 + 15700 * MS);
-        assert_eq!(b.tick(t0 + 16 * S), vec![Command::Capture { id: first + 1 }]);
+        assert_eq!(b.tick(t0 + 16 * S), vec![Command::Capture { id: first + 1, frames: 6 }]);
     }
 
     #[test]
```

- [ ] **Step 3: Photo pipeline and storage**

```diff
--- a/v2/src/storage.rs
+++ b/v2/src/storage.rs
@@ -24,8 +24,9 @@
         Ok(Self { photos, raw })
     }
 
-    /// Paths for a shot taken at `taken`: (photo, original). A second shot in
-    /// the same second gets a `_2`, `_3`, … suffix instead of overwriting.
+    /// Paths for a shot taken at `taken`: (photo, original); its DNG is the
+    /// original with a `.dng` extension. A second shot in the same second
+    /// gets a `_2`, `_3`, … suffix instead of overwriting any of them.
     /// Single writer only: the photo worker is the sole caller, so the
     /// exists-check and the later write cannot race.
     pub fn paths_for(&self, taken: NaiveDateTime) -> (PathBuf, PathBuf) {
@@ -33,7 +34,7 @@
         for n in 1.. {
             let name = if n == 1 { format!("{stem}.jpg") } else { format!("{stem}_{n}.jpg") };
             let (p, r) = (self.photos.join(&name), self.raw.join(&name));
-            if !p.exists() && !r.exists() {
+            if !p.exists() && !r.exists() && !r.with_extension("dng").exists() {
                 return (p, r);
             }
         }
@@ -102,6 +103,17 @@
     }
 
     #[test]
+    fn a_leftover_dng_also_takes_the_name() {
+        let dir = tempfile::tempdir().unwrap();
+        let s = Storage::create(dir.path()).unwrap();
+        let t = at("2026-11-19 14:03:07");
+        let (_, r1) = s.paths_for(t);
+        write_atomic(&r1.with_extension("dng"), b"dng").unwrap();
+        let (p2, _) = s.paths_for(t);
+        assert!(p2.ends_with("booth_20261119-140307_2.jpg"), "{p2:?}");
+    }
+
+    #[test]
     fn atomic_write_leaves_no_temp_and_counts() {
         let dir = tempfile::tempdir().unwrap();
         let s = Storage::create(dir.path()).unwrap();
```

```diff
--- a/v2/src/pipeline/photo.rs
+++ b/v2/src/pipeline/photo.rs
@@ -1,5 +1,6 @@
-//! One photo, start to finish: optional unfiltered original, sepia + random
-//! effect overlay, JPEG with EXIF, atomic writes, and the review image.
+//! One photo, start to finish: burst stacking, optional unfiltered original,
+//! sepia + random effect overlay, JPEG with EXIF, atomic writes, the optional
+//! DNG, and the review image.
 
 use std::path::PathBuf;
 use std::time::Instant;
@@ -7,19 +8,28 @@
 use anyhow::Context;
 use jpeg_encoder::{ColorType, Encoder, SamplingFactor};
 
+use super::dng;
 use super::exif::{PhotoInfo, build as build_exif};
 use super::filters::{composite, sepia, to_rgba};
+use super::stack::{StackReport, stack};
+use crate::camera::RawCapture;
+use crate::config::StackMode;
 use crate::osd::Rgba;
 use crate::storage::{Storage, write_atomic};
 
-/// A captured frame plus everything needed to turn it into files.
+/// A captured burst plus everything needed to turn it into files.
 #[derive(Debug, Clone)]
 pub struct PhotoJob {
     pub id: u64,
     pub width: u32,
     pub height: u32,
-    /// Tightly packed RGB, row 0 at the top, never mirrored.
-    pub rgb: Vec<u8>,
+    /// Burst frames: tightly packed RGB, row 0 at the top, never mirrored;
+    /// `frames[0]` is the photo metered under the flash. At least one.
+    pub frames: Vec<Vec<u8>>,
+    pub stack: StackMode,
+    pub max_shift_px: u32,
+    /// Written as `raw/<name>.dng` when present (`capture.save_dng`).
+    pub raw: Option<RawCapture>,
     pub info: PhotoInfo,
     pub filter: bool,
     pub jpeg_quality: u8,
@@ -30,11 +40,13 @@
 /// not run is 0).
 #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
 pub struct Timings {
+    pub stack_ms: u64,
     pub encode_original_ms: u64,
     pub write_original_ms: u64,
     pub filter_ms: u64,
     pub encode_photo_ms: u64,
     pub write_photo_ms: u64,
+    pub dng_ms: u64,
 }
 
 #[derive(Debug, Clone, PartialEq)]
@@ -42,6 +54,8 @@
     pub id: u64,
     pub photo: PathBuf,
     pub original: Option<PathBuf>,
+    pub dng: Option<PathBuf>,
+    pub stack: StackReport,
     /// What the guests see during review: the saved photo as RGBA.
     pub review: Rgba,
     pub timings: Timings,
@@ -68,13 +82,19 @@
 ) -> anyhow::Result<PhotoDone> {
     let exif = build_exif(&job.info);
     let (photo_path, original_path) = storage.paths_for(job.info.taken);
+    let dng_path = original_path.with_extension("dng"); // raw/<photo name>.dng
 
     let mut t = Timings::default();
     let ms = |since: Instant| since.elapsed().as_millis() as u64;
 
     let clock = Instant::now();
+    let frames = std::mem::take(&mut job.frames);
+    let (mut rgb, report) = stack(frames, job.width, job.height, job.stack, job.max_shift_px);
+    t.stack_ms = ms(clock);
+
+    let clock = Instant::now();
     let plain = if job.save_original || !job.filter {
-        Some(encode_jpeg(&job.rgb, job.width, job.height, job.jpeg_quality, &exif)?)
+        Some(encode_jpeg(&rgb, job.width, job.height, job.jpeg_quality, &exif)?)
     } else {
         None
     };
@@ -91,12 +111,12 @@
 
     let clock = Instant::now();
     let photo_bytes = if job.filter {
-        sepia(&mut job.rgb);
+        sepia(&mut rgb);
         if !effects.is_empty() {
             let i = pick(effects.len()).min(effects.len() - 1);
             let effect = &effects[i];
             if (effect.width, effect.height) == (job.width, job.height) {
-                composite(&mut job.rgb, effect);
+                composite(&mut rgb, effect);
             } else {
                 tracing::warn!(
                     effect = ?(effect.width, effect.height),
@@ -107,7 +127,7 @@
         }
         t.filter_ms = ms(clock);
         let clock = Instant::now();
-        let bytes = encode_jpeg(&job.rgb, job.width, job.height, job.jpeg_quality, &exif)?;
+        let bytes = encode_jpeg(&rgb, job.width, job.height, job.jpeg_quality, &exif)?;
         t.encode_photo_ms = ms(clock);
         bytes
     } else {
@@ -117,11 +137,26 @@
     write_atomic(&photo_path, &photo_bytes)?;
     t.write_photo_ms = ms(clock);
 
+    // Last, and never fatal: the guests' photo is already safe.
+    let clock = Instant::now();
+    let dng = job.raw.as_ref().and_then(|raw| {
+        match write_atomic(&dng_path, &dng::encode(&raw.image, &raw.colour, &job.info)) {
+            Ok(()) => Some(dng_path.clone()),
+            Err(e) => {
+                tracing::error!("DNG not saved: {e:#}");
+                None
+            }
+        }
+    });
+    t.dng_ms = ms(clock);
+
     Ok(PhotoDone {
         id: job.id,
         photo: photo_path,
         original,
-        review: to_rgba(&job.rgb, job.width, job.height),
+        dng,
+        stack: report,
+        review: to_rgba(&rgb, job.width, job.height),
         timings: t,
     })
 }
@@ -137,7 +172,10 @@
             id: 7,
             width: w,
             height: h,
-            rgb: [200u8, 40, 40].repeat((w * h) as usize), // reddish
+            frames: vec![[200u8, 40, 40].repeat((w * h) as usize)], // reddish
+            stack: StackMode::Average,
+            max_shift_px: 8,
+            raw: None,
             info: PhotoInfo {
                 taken: NaiveDateTime::parse_from_str("2026-11-19 14:03:07", "%Y-%m-%d %H:%M:%S").unwrap(),
                 exposure_us: Some(10_000),
@@ -224,6 +262,55 @@
     }
 
     #[test]
+    fn burst_is_stacked_before_filtering() {
+        let dir = tempfile::tempdir().unwrap();
+        let storage = Storage::create(dir.path()).unwrap();
+        let mut j = job(false, false);
+        let second = j.frames[0].clone();
+        j.frames.push(second);
+        let done = process(j, &storage, &[], |_| 0).unwrap();
+        assert_eq!(done.stack.frames.len(), 2);
+        assert_eq!(done.stack.averaged(), 1);
+        let px = decode(&done.photo).get_pixel(16, 8).0;
+        assert!(px[0] > 180 && px[1] < 70, "same colours after stacking: {px:?}");
+    }
+
+    fn raw_capture() -> RawCapture {
+        use crate::pipeline::dng::{RGGB, RawColour, RawImage};
+        RawCapture {
+            image: RawImage { width: 32, height: 16, cfa: RGGB, data: vec![1 << 6; 32 * 16] },
+            colour: RawColour::default(),
+        }
+    }
+
+    #[test]
+    fn dng_is_saved_next_to_the_original() {
+        let dir = tempfile::tempdir().unwrap();
+        let storage = Storage::create(dir.path()).unwrap();
+        let mut j = job(true, true);
+        j.raw = Some(raw_capture());
+        let done = process(j, &storage, &[], |_| 0).unwrap();
+        let dng = done.dng.expect("dng written");
+        assert!(dng.ends_with("raw/booth_20261119-140307.dng"));
+        let bytes = std::fs::read(&dng).unwrap();
+        assert_eq!(&bytes[..4], b"II\x2a\x00");
+        assert!(bytes.len() > 32 * 16 * 2);
+        assert!(done.timings.dng_ms < 5_000);
+    }
+
+    #[test]
+    fn dng_failure_keeps_the_photo() {
+        let dir = tempfile::tempdir().unwrap();
+        let storage = Storage::create(dir.path()).unwrap();
+        std::fs::remove_dir(dir.path().join("raw")).unwrap();
+        let mut j = job(true, false);
+        j.raw = Some(raw_capture());
+        let done = process(j, &storage, &[], |_| 0).unwrap();
+        assert!(done.photo.exists());
+        assert_eq!(done.dng, None);
+    }
+
+    #[test]
     fn effect_index_is_clamped() {
         let dir = tempfile::tempdir().unwrap();
         let storage = Storage::create(dir.path()).unwrap();
```

```diff
--- a/v2/src/pipeline/worker.rs
+++ b/v2/src/pipeline/worker.rs
@@ -50,11 +50,17 @@
                         id,
                         photo = %d.photo.display(),
                         ms = started.elapsed().as_millis() as u64,
+                        frames = d.stack.frames.len(),
+                        averaged = d.stack.averaged(),
+                        reference = d.stack.reference,
+                        stack_ms = d.timings.stack_ms,
                         encode_original_ms = d.timings.encode_original_ms,
                         write_original_ms = d.timings.write_original_ms,
                         filter_ms = d.timings.filter_ms,
                         encode_photo_ms = d.timings.encode_photo_ms,
                         write_photo_ms = d.timings.write_photo_ms,
+                        dng_ms = d.timings.dng_ms,
+                        dng = d.dng.is_some(),
                         "photo saved"
                     ),
                     Err(e) => tracing::error!(id, "photo failed: {e}"),
@@ -107,7 +113,10 @@
             id,
             width: 8,
             height: 4,
-            rgb: vec![128; 8 * 4 * 3],
+            frames: vec![vec![128; 8 * 4 * 3]],
+            stack: crate::config::StackMode::Average,
+            max_shift_px: 8,
+            raw: None,
             info: PhotoInfo { taken: chrono::Local::now().naive_local(), exposure_us: None, gain: None, gps: None },
             filter: true,
             jpeg_quality: 90,
@@ -161,8 +170,7 @@
         let dir = tempfile::tempdir().unwrap();
         let w = Worker::start(Storage::create(dir.path()).unwrap(), vec![effect_8x4()], None).unwrap();
         let mut bad = job(4);
-        bad.rgb.truncate(5); // frame shorter than width*height*3: composite indexes out of bounds
-        bad.save_original = false; // skip the plain encode so the overlay step is reached
+        bad.frames[0].truncate(5); // frame shorter than width*height*3: stacking asserts
         w.submit(bad);
         let r = wait(&w);
         assert_eq!(r.id, 4);
```

- [ ] **Step 4: Controller and main**

```diff
--- a/v2/src/display/controller.rs
+++ b/v2/src/display/controller.rs
@@ -8,8 +8,9 @@
 use std::thread::JoinHandle;
 use std::time::{Duration, Instant};
 
-use crate::camera::{Backoff, Camera, CameraCommand, CameraLink, CapturedFrame};
-use crate::config::{Config, FlashMode};
+use crate::camera::{Backoff, Camera, CameraCommand, CameraLink, CapturedBurst};
+use crate::config::{Config, FlashMode, StackMode};
+use crate::flash::Strobe;
 use crate::frame::{Frame, FrameMailbox};
 use crate::input::{Action, GpioButton};
 use crate::monitor::{Monitors, Status};
@@ -61,8 +62,9 @@
         review: Duration::from_secs(cfg.timing.review_s as u64),
         idle: Duration::from_secs(cfg.timing.idle_s as u64),
         prepare_lead: Duration::from_millis(cfg.flash.lead_ms as u64),
-        screen_flash: cfg.flash.mode == FlashMode::Screen,
+        flash: cfg.flash.mode != FlashMode::None,
         flash_max: Duration::from_millis(cfg.flash.max_ms as u64),
+        burst_frames: cfg.capture.burst_frames,
         capture_timeout: CAPTURE_TIMEOUT,
         processing_timeout: PROCESSING_TIMEOUT,
     }
@@ -72,7 +74,7 @@
 struct Running {
     camera: Box<dyn Camera>,
     mailbox: Arc<FrameMailbox>,
-    captures: Receiver<CapturedFrame>,
+    captures: Receiver<CapturedBurst>,
 }
 
 fn start_camera(factory: &CameraFactory) -> anyhow::Result<Running> {
@@ -88,6 +90,10 @@
     jpeg_quality: u8,
     save_original: bool,
     gps: Option<(f64, f64)>,
+    stack: StackMode,
+    max_shift_px: u32,
+    /// Draw the white screen while the flash is on.
+    screen_flash: bool,
 }
 
 pub struct Controller {
@@ -107,6 +113,8 @@
     healthy_since: Option<Instant>,
     monitors: Monitors,
     _button: Option<GpioButton>,
+    /// Lit with the flash when `flash.mode` is strobe or both.
+    strobe: Option<Strobe>,
     button_rx: Receiver<Action>,
     worker: Worker,
     queue: Arc<Queue>,
@@ -150,9 +158,6 @@
         sup: Supervision,
         now: Instant,
     ) -> anyhow::Result<Self> {
-        if cfg.flash.mode == FlashMode::Strobe {
-            tracing::warn!("flash.mode = \"strobe\" arrives in milestone 5; running without flash");
-        }
         let storage = Storage::create(&cfg.storage.root)?;
         let photo_count = storage.photo_count();
         let effects_dir = cfg.display.assets_dir.join("effects");
@@ -199,6 +204,7 @@
             healthy_since: None,
             monitors: Monitors::start(cfg)?,
             _button: button,
+            strobe: None,
             button_rx,
             worker: Worker::start(storage, effects, Some(queue.clone()))?,
             queue,
@@ -207,6 +213,9 @@
                 jpeg_quality: cfg.capture.jpeg_quality,
                 save_original: cfg.storage.save_original,
                 gps: cfg.immich.gps(),
+                stack: cfg.capture.stack,
+                max_shift_px: cfg.capture.max_shift_px,
+                screen_flash: cfg.flash.screen(),
             },
             review_image: None,
             photo_count,
@@ -292,6 +301,11 @@
         }
     }
 
+    /// Light `strobe` together with the flash from now on.
+    pub fn set_strobe(&mut self, strobe: Strobe) {
+        self.strobe = Some(strobe);
+    }
+
     /// The processed photo to show in review, once per photo.
     pub fn take_review_image(&mut self) -> Option<Rgba> {
         self.review_image.take()
@@ -300,7 +314,11 @@
     pub fn scene_input(&self, now: Instant, screen: (u32, u32)) -> SceneInput {
         SceneInput {
             screen,
-            view: self.booth.view(now),
+            view: {
+                let mut v = self.booth.view(now);
+                v.flash &= self.photo.screen_flash;
+                v
+            },
             status: self.status(),
             debug: self.debug,
             banner: self.cam.is_none(),
@@ -311,9 +329,14 @@
         for cmd in cmds {
             let camera_cmd = match cmd {
                 // The screen flash is drawn from `View::flash`.
-                Command::FlashOn | Command::FlashOff => continue,
+                Command::FlashOn | Command::FlashOff => {
+                    if let Some(s) = &self.strobe {
+                        s.set(cmd == Command::FlashOn);
+                    }
+                    continue;
+                }
                 Command::PrepareCapture => CameraCommand::PrepareCapture,
-                Command::Capture { id } => CameraCommand::Capture { id },
+                Command::Capture { id, frames } => CameraCommand::Capture { id, frames },
                 Command::ReleaseCapture => CameraCommand::ReleaseCapture,
             };
             if let Some(c) = self.cam.as_mut() {
@@ -324,24 +347,35 @@
 
     fn collect_captures(&mut self, now: Instant) {
         let Some(c) = &self.cam else { return };
-        let frames: Vec<CapturedFrame> = c.captures.try_iter().collect();
-        for f in frames {
-            let id = f.id;
+        let bursts: Vec<CapturedBurst> = c.captures.try_iter().collect();
+        for b in bursts {
+            let id = b.id;
             if !matches!(self.booth.phase(), Phase::Capturing { id: p, .. } if p == id) {
-                tracing::warn!(id, "unexpected captured frame ignored");
+                tracing::warn!(id, "unexpected captured burst ignored");
                 continue;
             }
+            let Some((width, height)) = b.frames.first().map(|f| (f.width, f.height)) else {
+                tracing::error!(id, "empty burst ignored");
+                continue;
+            };
             let cmds = self.booth.handle(Event::FrameCaptured { id }, now);
             self.apply(cmds);
+            // Frames of another size (never expected) cannot be stacked.
+            let CapturedBurst { frames, meta, raw, .. } = b;
+            let frames: Vec<Vec<u8>> =
+                frames.into_iter().filter(|f| (f.width, f.height) == (width, height)).map(|f| f.data).collect();
             self.worker.submit(PhotoJob {
                 id,
-                width: f.frame.width,
-                height: f.frame.height,
-                rgb: f.frame.data,
+                width,
+                height,
+                frames,
+                stack: self.photo.stack,
+                max_shift_px: self.photo.max_shift_px,
+                raw,
                 info: PhotoInfo {
                     taken: chrono::Local::now().naive_local(),
-                    exposure_us: f.meta.exposure_us,
-                    gain: f.meta.gain,
+                    exposure_us: meta.exposure_us,
+                    gain: meta.gain,
                     gps: self.photo.gps,
                 },
                 filter: self.filter,
@@ -528,10 +562,13 @@
         let t = timings_from(&c);
         assert_eq!(t.countdown, Duration::from_secs(3));
         assert_eq!(t.prepare_lead, Duration::from_millis(250));
-        assert!(t.screen_flash);
+        assert!(t.flash);
+        assert_eq!(t.burst_frames, 6);
+        c.flash.mode = FlashMode::Strobe;
+        assert!(timings_from(&c).flash);
         c.flash.mode = FlashMode::None;
         let t = timings_from(&c);
-        assert!(!t.screen_flash);
+        assert!(!t.flash);
         assert_eq!(t.prepare_lead, Duration::from_millis(250), "camera still prepares without flash");
     }
 
@@ -561,10 +598,64 @@
         run_until(&mut c, |c| c.booth.phase() == Phase::Live);
     }
 
+    #[derive(Clone, Default)]
+    struct Probe(Arc<std::sync::Mutex<Vec<bool>>>);
+    impl crate::flash::Output for Probe {
+        fn set(&mut self, on: bool) -> anyhow::Result<()> {
+            self.0.lock().unwrap().push(on);
+            Ok(())
+        }
+    }
+
+    #[test]
+    fn strobe_lights_for_the_shot_without_the_screen_flash() {
+        let dir = tempfile::tempdir().unwrap();
+        let mut config = cfg(dir.path());
+        config.flash.mode = FlashMode::Strobe;
+        let (tx, rx) = mpsc::channel();
+        let f = factory(Arc::new(AtomicUsize::new(0)), usize::MAX);
+        let mut c = Controller::start_with(&config, f, None, rx, fast(), Instant::now()).unwrap();
+        let probe = Probe::default();
+        c.set_strobe(Strobe::start(Box::new(probe.clone()), Duration::from_secs(5)).unwrap());
+        tx.send(Action::Press).unwrap();
+        let mut saw_flash = false;
+        run_until(&mut c, |c| {
+            let now = Instant::now();
+            if c.booth.view(now).flash {
+                saw_flash = true;
+                assert!(!c.scene_input(now, (100, 100)).view.flash, "no white screen in strobe mode");
+            }
+            matches!(c.booth.phase(), Phase::Review { .. })
+        });
+        assert!(saw_flash);
+        drop(c); // drops the strobe: its thread has handled every message
+        assert_eq!(probe.0.lock().unwrap()[..3], [false, true, false], "off, lit for the shot, off");
+    }
+
+    #[test]
+    fn save_dng_writes_a_dng_next_to_the_original() {
+        let dir = tempfile::tempdir().unwrap();
+        let mut config = cfg(dir.path());
+        config.capture.save_dng = true;
+        let (tx, rx) = mpsc::channel();
+        let f: CameraFactory =
+            Arc::new(|| -> Box<dyn Camera> { Box::new(FakeCamera::new((64, 48), 100).with_raw(true)) });
+        let mut c = Controller::start_with(&config, f, None, rx, fast(), Instant::now()).unwrap();
+        tx.send(Action::Press).unwrap();
+        run_until(&mut c, |c| matches!(c.booth.phase(), Phase::Review { .. }));
+        assert_eq!(c.booth.view(Instant::now()).review, Some(Outcome::Photo));
+        let dngs: Vec<_> = std::fs::read_dir(dir.path().join("raw"))
+            .unwrap()
+            .filter_map(Result::ok)
+            .filter(|e| e.path().extension().is_some_and(|x| x == "dng"))
+            .collect();
+        assert_eq!(dngs.len(), 1);
+    }
+
     /// A fake camera that delivers every capture twice (same id).
     struct DuplicatingCamera {
         inner: FakeCamera,
-        captures: Option<mpsc::Sender<CapturedFrame>>,
+        captures: Option<mpsc::Sender<CapturedBurst>>,
     }
     impl Camera for DuplicatingCamera {
         fn start(&mut self, link: CameraLink) -> anyhow::Result<()> {
@@ -572,10 +663,10 @@
             self.inner.start(link)
         }
         fn command(&mut self, cmd: CameraCommand) {
-            if let (CameraCommand::Capture { id }, Some(tx)) = (cmd, &self.captures) {
+            if let (CameraCommand::Capture { id, .. }, Some(tx)) = (cmd, &self.captures) {
                 let frame =
                     Frame { width: 64, height: 48, data: vec![0; 64 * 48 * 3], seq: 0, captured_at: Instant::now() };
-                let _ = tx.send(CapturedFrame { id, frame, meta: Default::default() });
+                let _ = tx.send(CapturedBurst { id, frames: vec![frame], meta: Default::default(), raw: None });
             }
             self.inner.command(cmd);
         }
```

```diff
--- a/v2/src/main.rs
+++ b/v2/src/main.rs
@@ -8,6 +8,7 @@
 use pibooth::camera::{Camera, SENSOR_FPS, SENSOR_SIZE, fake::FakeCamera};
 use pibooth::config::Config;
 use pibooth::display::{self, CameraFactory, Controller, RunOptions};
+use pibooth::flash::{GpioLine, Strobe};
 use pibooth::input::GpioButton;
 use tracing_subscriber::EnvFilter;
 
@@ -20,7 +21,7 @@
     /// Use the animated test pattern instead of the Pi camera.
     #[arg(long)]
     fake_camera: bool,
-    /// Disable the GPIO button (keyboard only).
+    /// Disable the GPIO button and strobe (keyboard only, no strobe).
     #[arg(long)]
     no_gpio: bool,
     /// Log display/camera frame rates every second.
@@ -59,8 +60,18 @@
             }
         }
     };
-    let ctrl =
+    let mut ctrl =
         Controller::start(&cfg, camera_factory(&cli, &cfg)?, button, rx, Instant::now()).context("starting camera")?;
+    if cfg.flash.strobe() {
+        let max_on = std::time::Duration::from_millis(cfg.flash.max_ms as u64);
+        match cli.no_gpio {
+            true => tracing::warn!("--no-gpio: running without the strobe"),
+            false => match GpioLine::open(&cfg.flash).and_then(|line| Strobe::start(Box::new(line), max_on)) {
+                Ok(s) => ctrl.set_strobe(s),
+                Err(e) => tracing::warn!("strobe unavailable, running without it: {e:#}"),
+            },
+        }
+    }
     display::run(
         ctrl,
         RunOptions {
@@ -75,12 +86,15 @@
 
 fn camera_factory(cli: &Cli, cfg: &Config) -> anyhow::Result<CameraFactory> {
     if cli.fake_camera {
-        return Ok(Arc::new(|| -> Box<dyn Camera> { Box::new(FakeCamera::new(SENSOR_SIZE, SENSOR_FPS)) }));
+        let raw = cfg.capture.save_dng;
+        return Ok(Arc::new(move || -> Box<dyn Camera> {
+            Box::new(FakeCamera::new(SENSOR_SIZE, SENSOR_FPS).with_raw(raw))
+        }));
     }
     #[cfg(feature = "libcamera")]
     {
-        let cam_cfg = cfg.camera.clone();
-        Ok(Arc::new(move || -> Box<dyn Camera> { Box::new(pibooth::camera::pi::PiCamera::new(cam_cfg.clone())) }))
+        let (cam_cfg, raw) = (cfg.camera.clone(), cfg.capture.save_dng);
+        Ok(Arc::new(move || -> Box<dyn Camera> { Box::new(pibooth::camera::pi::PiCamera::new(cam_cfg.clone(), raw)) }))
     }
     #[cfg(not(feature = "libcamera"))]
     {
```

- [ ] **Step 5: Pi camera (raw stream and bursts)**

```diff
--- a/v2/src/camera/pi.rs
+++ b/v2/src/camera/pi.rs
@@ -3,6 +3,10 @@
 //! libcamera objects borrow the `CameraManager` and must stay on one thread,
 //! so the whole session lives on a dedicated "pi-camera" thread. `start`
 //! waits for that thread to report that streaming began (or why it failed).
+//!
+//! With `capture.save_dng` a second, raw stream is configured: 16-bit
+//! unpacked Bayer at the sensor size (the Pi 5 front end cannot output the
+//! sensor's packed 10-bit format; libcamera would substitute it anyway).
 
 use std::sync::Arc;
 use std::sync::atomic::{AtomicBool, Ordering};
@@ -15,8 +19,8 @@
 use libcamera::camera_manager::CameraManager;
 use libcamera::control::ControlList;
 use libcamera::controls::{
-    AnalogueGain, AnalogueGainMode, AwbEnable, ExposureTime, ExposureTimeMode, FrameDuration, FrameDurationLimits,
-    NoiseReductionMode, SensorTimestamp,
+    AnalogueGain, AnalogueGainMode, AwbEnable, ColourCorrectionMatrix, ColourGains, ExposureTime, ExposureTimeMode,
+    FrameDuration, FrameDurationLimits, NoiseReductionMode, SensorBlackLevels, SensorTimestamp,
 };
 use libcamera::framebuffer::AsFrameBuffer;
 use libcamera::framebuffer_allocator::{FrameBuffer, FrameBufferAllocator};
@@ -26,17 +30,22 @@
 use libcamera::request::{RequestStatus, ReuseFlag};
 use libcamera::stream::StreamRole;
 
-use super::{Camera, CameraCommand, CameraLink, CaptureMeta, CapturedFrame, SENSOR_FPS, SENSOR_SIZE};
+use super::{BurstCollector, Camera, CameraCommand, CameraLink, CaptureMeta, RawCapture, SENSOR_FPS, SENSOR_SIZE};
 use crate::config::CameraCfg;
-use crate::frame::{Frame, FrameMailbox, copy_packed};
+use crate::frame::{Frame, FrameMailbox, copy_packed, copy_u16_le};
+use crate::pipeline::dng::{RawColour, RawImage, cfa_from_fourcc};
 use crate::stats::{FrameSample, FrameTiming};
 
 /// libcamera `BGR888` = DRM `BG24`: bytes in memory are R, G, B, which is
 /// what GL `RGB` expects. (libcamera `RGB888` is B, G, R in memory.)
 const PREVIEW_FORMAT: PixelFormat = PixelFormat::new(u32::from_le_bytes(*b"BG24"), 0);
+/// 16-bit unpacked RGGB Bayer (`SRGGB16`); libcamera may answer with
+/// another order (`BG16`, …) if the sensor is flipped.
+const RAW_FORMAT: PixelFormat = PixelFormat::new(u32::from_le_bytes(*b"RG16"), 0);
 const BUFFER_COUNT: u32 = 4;
 
 pub struct PiCamera {
+    raw: bool,
     running: Arc<AtomicBool>,
     thread: Option<JoinHandle<()>>,
     commands: Option<mpsc::Sender<CameraCommand>>,
@@ -44,8 +53,9 @@
 
 impl PiCamera {
     /// The tuning file, if any, is applied by `main` before any thread starts.
-    pub fn new(_cfg: CameraCfg) -> Self {
-        Self { running: Arc::new(AtomicBool::new(false)), thread: None, commands: None }
+    /// `raw`: also stream raw Bayer frames for DNG output.
+    pub fn new(_cfg: CameraCfg, raw: bool) -> Self {
+        Self { raw, running: Arc::new(AtomicBool::new(false)), thread: None, commands: None }
     }
 }
 
@@ -56,12 +66,13 @@
         }
         self.running.store(true, Ordering::SeqCst);
         let running = self.running.clone();
+        let raw = self.raw;
         let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
         let (cmd_tx, cmd_rx) = mpsc::channel();
         self.commands = Some(cmd_tx);
         let thread = std::thread::Builder::new().name("pi-camera".into()).spawn(move || {
             let mut guard = CloseGuard { mailbox: link.mailbox.clone(), running: running.clone(), reason: None };
-            if let Err(e) = session(&link, &cmd_rx, &running, &ready_tx) {
+            if let Err(e) = session(&link, &cmd_rx, &running, &ready_tx, raw) {
                 let msg = format!("{e:#}");
                 // If setup failed, `start` is still waiting for this message.
                 let _ = ready_tx.send(Err(msg.clone()));
@@ -180,6 +191,7 @@
     commands: &mpsc::Receiver<CameraCommand>,
     running: &AtomicBool,
     ready: &mpsc::Sender<Result<(), String>>,
+    want_raw: bool,
 ) -> anyhow::Result<()> {
     let mailbox = &link.mailbox;
     let mgr = CameraManager::new().context("starting libcamera")?;
@@ -188,15 +200,22 @@
     tracing::info!(id = %cam.id(), "using camera");
     let mut cam = cam.acquire().context("camera is busy (another app using it?)")?;
 
-    let mut cfgs = cam
-        .generate_configuration(&[StreamRole::ViewFinder])
-        .ok_or_else(|| anyhow!("could not generate viewfinder configuration"))?;
+    let roles: &[StreamRole] =
+        if want_raw { &[StreamRole::ViewFinder, StreamRole::Raw] } else { &[StreamRole::ViewFinder] };
+    let mut cfgs =
+        cam.generate_configuration(roles).ok_or_else(|| anyhow!("could not generate camera configuration"))?;
     {
         let mut s = cfgs.get_mut(0).unwrap();
         s.set_pixel_format(PREVIEW_FORMAT);
         s.set_size(Size { width: SENSOR_SIZE.0, height: SENSOR_SIZE.1 });
         s.set_buffer_count(BUFFER_COUNT);
     }
+    if want_raw {
+        let mut s = cfgs.get_mut(1).ok_or_else(|| anyhow!("no raw stream configuration"))?;
+        s.set_pixel_format(RAW_FORMAT);
+        s.set_size(Size { width: SENSOR_SIZE.0, height: SENSOR_SIZE.1 });
+        s.set_buffer_count(BUFFER_COUNT);
+    }
     match cfgs.validate() {
         CameraConfigurationStatus::Valid => {}
         CameraConfigurationStatus::Adjusted => tracing::warn!("camera adjusted configuration: {cfgs:?}"),
@@ -210,16 +229,48 @@
         bail!("camera does not support BGR888 preview (got {format:?})");
     }
     tracing::info!(width = size.width, height = size.height, stride, "preview configured");
+    // (size, stride, CFA layout) of the raw stream.
+    let raw_layout = if want_raw {
+        let s = cfgs.get(1).unwrap();
+        let (rsize, rstride, rformat) = (s.get_size(), s.get_stride() as usize, s.get_pixel_format());
+        let fourcc = rformat.fourcc().to_le_bytes();
+        let cfa = cfa_from_fourcc(fourcc).filter(|_| rformat.modifier() == 0).ok_or_else(|| {
+            anyhow!(
+                "raw stream offered {:?}, not 16-bit Bayer; set capture.save_dng = false",
+                String::from_utf8_lossy(&fourcc)
+            )
+        })?;
+        tracing::info!(width = rsize.width, height = rsize.height, stride = rstride, format = %String::from_utf8_lossy(&fourcc), "raw stream configured");
+        Some((rsize, rstride, cfa))
+    } else {
+        None
+    };
     cam.configure(&mut cfgs).context("configuring camera")?;
 
     let stream = cfgs.get(0).unwrap().stream().ok_or_else(|| anyhow!("stream missing after configure"))?;
+    let raw_stream = match raw_layout {
+        Some(_) => Some(cfgs.get(1).unwrap().stream().ok_or_else(|| anyhow!("raw stream missing after configure"))?),
+        None => None,
+    };
     let mut alloc = FrameBufferAllocator::new(&cam);
     let buffers = alloc.alloc(&stream).context("allocating frame buffers")?;
+    let mut raw_buffers = match &raw_stream {
+        Some(rs) => alloc.alloc(rs).context("allocating raw buffers")?.into_iter().map(Some).collect(),
+        None => Vec::new(),
+    };
     let mut requests = Vec::with_capacity(buffers.len());
     for (i, buf) in buffers.into_iter().enumerate() {
         let buf = MemoryMappedFrameBuffer::new(buf).map_err(|e| anyhow!("mmap buffer: {e:?}"))?;
         let mut req = cam.create_request(Some(i as u64)).ok_or_else(|| anyhow!("create_request failed"))?;
         req.add_buffer(&stream, buf).context("attaching buffer")?;
+        if let Some(rs) = &raw_stream {
+            let rb = raw_buffers
+                .get_mut(i)
+                .and_then(Option::take)
+                .ok_or_else(|| anyhow!("fewer raw buffers than preview buffers"))?;
+            let rb = MemoryMappedFrameBuffer::new(rb).map_err(|e| anyhow!("mmap raw buffer: {e:?}"))?;
+            req.add_buffer(rs, rb).context("attaching raw buffer")?;
+        }
         requests.push(req);
     }
 
@@ -245,16 +296,16 @@
     let mut timing = FrameTiming::default();
     let mut last_report = Instant::now();
     let mut pending = PendingControls::default();
-    let mut capture: Option<u64> = None;
+    let mut burst: Option<BurstCollector> = None;
     while running.load(Ordering::SeqCst) {
         for cmd in commands.try_iter() {
             tracing::debug!(?cmd, "camera command");
             // The photo is the next completed request (already in flight), metered
             // under the flash since PrepareCapture. The lock set here applies only
-            // to requests queued after it, i.e. to frames following the photo.
+            // to requests queued after it, i.e. to the rest of the burst.
             pending.on_command(cmd);
-            if let CameraCommand::Capture { id } = cmd {
-                capture = Some(id);
+            if let CameraCommand::Capture { id, frames } = cmd {
+                burst = Some(BurstCollector::new(id, frames));
             }
         }
         let mut req = match done_rx.recv_timeout(Duration::from_secs(2)) {
@@ -275,16 +326,55 @@
             copy_packed(planes[0], stride, row_bytes, h as usize, &mut data);
             let copy_us = copy_start.elapsed().as_micros() as u64;
             let md = req.metadata();
-            if let Some(id) = capture.take() {
-                // The next complete frame after the command: high-quality noise
-                // reduction has been on since PrepareCapture (~300 ms earlier).
-                let meta = CaptureMeta {
-                    exposure_us: md.get::<ExposureTime>().ok().map(|v| v.0),
-                    gain: md.get::<AnalogueGain>().ok().map(|v| v.0),
+            let locked = md.get::<ExposureTimeMode>().is_ok_and(|m| m == ExposureTimeMode::Manual);
+            if let Some(b) = burst.as_mut()
+                && b.wants(locked)
+            {
+                let first = if b.is_first() {
+                    // High-quality noise reduction has been on since PrepareCapture.
+                    let meta = CaptureMeta {
+                        exposure_us: md.get::<ExposureTime>().ok().map(|v| v.0),
+                        gain: md.get::<AnalogueGain>().ok().map(|v| v.0),
+                    };
+                    let raw = match (&raw_stream, raw_layout) {
+                        (Some(rs), Some((rsize, rstride, cfa))) => {
+                            let rfb: &MemoryMappedFrameBuffer<FrameBuffer> =
+                                req.buffer(rs).ok_or_else(|| anyhow!("request without raw buffer"))?;
+                            let image = RawImage {
+                                width: rsize.width,
+                                height: rsize.height,
+                                cfa,
+                                data: copy_u16_le(rfb.data()[0], rstride, rsize.width, rsize.height),
+                            };
+                            let m = md.get::<ColourCorrectionMatrix>().ok().map(|c| c.0);
+                            let colour = RawColour {
+                                black_levels: md.get::<SensorBlackLevels>().ok().map(|v| v.0),
+                                colour_gains: md.get::<ColourGains>().ok().map(|v| (v.0[0], v.0[1])),
+                                ccm: m.map(|m| {
+                                    [m[0][0], m[0][1], m[0][2], m[1][0], m[1][1], m[1][2], m[2][0], m[2][1], m[2][2]]
+                                }),
+                            };
+                            Some(RawCapture { image, colour })
+                        }
+                        _ => None,
+                    };
+                    tracing::info!(
+                        seq,
+                        exposure_us = meta.exposure_us,
+                        gain = meta.gain,
+                        raw = raw.is_some(),
+                        "photo frame"
+                    );
+                    Some((meta, raw))
+                } else {
+                    None
                 };
-                tracing::info!(id, seq, exposure_us = meta.exposure_us, gain = meta.gain, "frame captured");
                 let frame = Frame { width: w, height: h, data: data.clone(), seq, captured_at: Instant::now() };
-                let _ = link.captures.send(CapturedFrame { id, frame, meta });
+                if let Some(done) = b.push(frame, first) {
+                    tracing::info!(id = done.id, frames = done.frames.len(), last_seq = seq, "burst captured");
+                    burst = None;
+                    let _ = link.captures.send(done);
+                }
             }
             mailbox.publish(Frame { width: w, height: h, data, seq, captured_at: Instant::now() });
             timing.record(FrameSample {
```

- [ ] **Step 6: Verify**

Run: `cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check` — Expected: 175 passed, 2 ignored, clean.
Run (libcamera headers available): `PKG_CONFIG_PATH=<dir with libcamera.pc> cargo clippy --all-targets --features libcamera -- -D warnings` — Expected: clean. If no headers are available, say so in the report.
Run (needs Xvfb): with a config setting `[capture] save_dng = true` and `[flash] mode = "both"`, `DISPLAY=:99 ./target/debug/pibooth --fake-camera --config <file>`, press space, wait ~12 s: the log shows `strobe unavailable` (no GPIO), then `photo saved … frames=6 averaged=… dng=true`; `raw/` holds the `.jpg` and `.dng`.

- [ ] **Step 7: Commit**

```bash
git add src
git commit -m "feat(v2): burst capture into stacked photos, DNG raw stream, strobe wiring"
```

---

### Task 5: Documentation

**Files:**
- Modify: `v2/README.md`, `v2/config.example.toml`

- [ ] **Step 1: Apply**

````diff
--- a/v2/README.md
+++ b/v2/README.md
@@ -2,11 +2,12 @@
 
 Rewrite of the Python booth (`../main.py`). Design: `../docs/superpowers/specs/2026-11-18-rust-v2-design.md`.
 
-Status: milestone 4 — live preview, button and countdown, screen flash,
-full-quality capture (high-quality noise reduction, exposure metered under the
-flash), sepia + effect overlay, JPEG with EXIF, review of the
-saved photo, status icons, camera auto-restart, and background upload to
-Immich that survives being offline and reboots.
+Status: milestone 5 — live preview, button and countdown, screen flash and/or
+GPIO strobe, full-quality capture (high-quality noise reduction, exposure
+metered under the flash) of a burst stacked into one low-noise photo, sepia +
+effect overlay, JPEG with EXIF, optional DNG raw, review of the saved photo,
+status icons, camera auto-restart, and background upload to Immich that
+survives being offline and reboots.
 
 ## Photos
 
@@ -16,6 +17,24 @@
   overlay from `effects/` when the filter is on, plain otherwise).
 - `raw/booth_YYYYMMDD-HHMMSS.jpg` — the unfiltered original
   (`storage.save_original = true`).
+- `raw/booth_YYYYMMDD-HHMMSS.dng` — the sensor's raw data for the first frame
+  (`capture.save_dng = true`; 3.2 MB each, kept on the Pi, not uploaded). Opens
+  in darktable, RawTherapee or Lightroom with the colours the booth used.
+
+Each photo is a burst of `capture.burst_frames` (6) frames taken in about
+0.15 s with exposure and white balance locked:
+
+- `stack = "average"` (default): the frames are lined up with the sharpest one
+  and averaged, which removes most of the grain in dim light. Frames that moved
+  more than `max_shift_px` or came out brighter/darker are left out, and where
+  someone moved between frames the sharpest frame's pixels are kept, so moving
+  hands do not leave ghosts.
+- `stack = "sharpest"`: keep the sharpest frame (rescues motion blur, no
+  noise reduction).
+- `stack = "off"`: the first frame only. With `burst_frames = 1` the camera
+  takes just one frame.
+
+The log line `photo saved … frames=6 averaged=5` tells how many frames were used.
 
 Photos are never mirrored, are 1456×1088 (the sensor's full resolution), JPEG
 quality `capture.jpeg_quality` (95) with full colour detail, and carry EXIF date,
@@ -92,6 +111,17 @@
 the internal pull-down and listens for debounced press edges. For a button to
 ground instead, set `[button] active_low = true` (internal pull-up).
 
+## Strobe
+
+`[flash] mode = "strobe"` (or `"both"` with the white screen) drives GPIO 17
+(BCM) high from `flash.lead_ms` before the shot until the burst is captured
+(about 0.5 s), never longer than `flash.max_ms`. The line can only switch a
+light through a driver (a MOSFET or relay module for an LED panel, or an
+opto-coupler for a flash unit), never power it directly. `active_high = false`
+inverts the line for drivers that switch on low. Add a pull-down (pull-up when
+inverted) resistor on the driver input so the light stays off while the booth
+is not running. `--no-gpio` disables the strobe and the button.
+
 ## Build on the Raspberry Pi 5
 
 ```bash
````

```diff
--- a/v2/config.example.toml
+++ b/v2/config.example.toml
@@ -11,11 +11,10 @@
 tuning_file = ""      # empty = libcamera default (imx296.json)
 
 [capture]
-# Milestone 3 uses one frame per photo; burst_frames/stack/save_dng arrive in milestone 5.
-burst_frames = 6
-stack = "average"     # average | sharpest | off
-max_shift_px = 8
-save_dng = false
+burst_frames = 6      # frames per photo (1..=12), ~0.15 s at 60 fps
+stack = "average"     # average: less noise | sharpest: least blur | off: first frame only
+max_shift_px = 8      # frames that moved more than this are left out of the average
+save_dng = false      # also save raw/booth_<ts>.dng (3.2 MB each, not uploaded)
 jpeg_quality = 95
 
 [timing]
@@ -31,12 +30,11 @@
 enabled = true
 
 [flash]
-mode = "screen"       # screen | strobe | none
-lead_ms = 300
-max_ms = 800
-gpio_line = 17
-pulse_ms = 150
-active_high = true
+mode = "screen"       # screen | strobe | both | none
+lead_ms = 300         # light on this long before the shot (exposure is metered under it)
+max_ms = 800          # hard limit on the light; it goes off when the burst is in
+gpio_line = 17        # strobe output (BCM), for strobe | both
+active_high = true    # false: the strobe is on when the line is low
 
 [button]
 gpio_line = 27
```

- [ ] **Step 2: Verify**

Run: `cargo test config` — Expected: PASS (`example_config_parses`). `grep -n pulse_ms README.md config.example.toml` — Expected: nothing.

- [ ] **Step 3: Commit**

```bash
git add README.md config.example.toml
git commit -m "docs(v2): burst stacking, DNG and strobe"
```
