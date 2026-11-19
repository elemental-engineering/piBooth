# piBooth v2 — Plan 3: Quality Capture (Milestone 3)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn the milestone 2 booth loop into a real photo booth: capture a full-quality frame at the end of the countdown (high-quality noise reduction, exposure and white balance locked), apply the colour-correct sepia and a random v1 effect overlay, save `photos/` and `raw/` JPEGs with EXIF, and show the saved photo (or "Oops, try again") during review.

**Architecture:** The 1456×1088 preview stream *is* the sensor's full resolution, so captures come from it: `PrepareCapture` (≈`flash.lead_ms` before the shot) switches noise reduction to HighQuality while auto-exposure meters the flash-lit scene; `Capture{id}` locks exposure/gain/white balance and hands the next complete frame to the controller over a channel; `ReleaseCapture` restores preview settings. A background `Worker` thread turns the frame into files (pure functions in `pipeline::{filters, photo, exif}` and `storage`) and returns the review image. The state machine gains capture ids and `Processing`/`Review{Photo|Oops}` phases. The camera restart moves to a background thread.

**Tech Stack:** Rust 2024 (rustc ≥ 1.88); existing crates plus `chrono` (local time), `jpeg-encoder` 0.7 (pure-Rust JPEG, 4:4:4), `fastrand` 2 (effect choice), libcamera feature `vendor_draft` (NoiseReductionMode); dev: `kamadak-exif` 0.6 and `image` with `jpeg` (tests read files back).

**Spec:** `docs/superpowers/specs/2026-11-18-rust-v2-design.md` (§4.1, §4.2, §4.4 steps 2–6 without stacking/DNG, §5 "Oops"; this plan covers §8 milestone 3). Builds on Plans 1–2, already on branch `v2-rust`.

## Global Constraints

- Crate `pibooth` in `v2/`; v1 files (`main.py`, `fonts/`, `images/`, `effects/`) are read-only and are the asset source.
- Photos are never mirrored; preview mirroring stays shader-only. Photo size = capture size (1456×1088 on the Pi).
- Files (v1 layout, under `storage.root`): filtered `photos/booth_YYYYMMDD-HHMMSS.jpg`, unfiltered `raw/booth_YYYYMMDD-HHMMSS.jpg` when `storage.save_original` (default true); a second shot in the same second gets `_2`, `_3`…; writes go to `*.jpg.part` then rename; `photos/` and `raw/` are created at startup.
- Sepia uses the standard RGB matrix (R' = .393R+.769G+.189B, G' = .349R+.686G+.168B, B' = .272R+.534G+.131B), clamped to 255, no rescaling (fixes v1 `main.py:76`). Effect = random PNG from `<display.assets_dir>/effects/`, resized once at startup to the capture size, alpha-composited. Filter off (`f`) ⇒ `photos/` gets the plain image.
- JPEG: quality `capture.jpeg_quality` (95), 4:4:4 sampling, EXIF APP1 with Make "Raspberry Pi", Model "Global Shutter Camera (IMX296)", Orientation 1, Software, DateTime/DateTimeOriginal/DateTimeDigitized (local time), ExposureTime, ISO = gain×100, GPS when `immich.gps()` is Some.
- Capture timing: `PrepareCapture` at `deadline - flash.lead_ms` in every flash mode; screen flash on at the same moment in screen mode; `Capture{id}` at the deadline; the next complete frame is the photo; flash off and `ReleaseCapture` when it arrives; capture timeout 5 s and processing timeout 10 s ⇒ review "Oops, try again". Results for another id are ignored.
- Camera controls (libcamera): prepare ⇒ `NoiseReductionMode::HighQuality`; capture ⇒ `ExposureTimeMode::Manual`, `AnalogueGainMode::Manual`, `AwbEnable(false)`; release ⇒ Auto, Auto, `AwbEnable(true)`, `NoiseReductionMode::Fast`. Controls are sent once, on the next re-queued request.
- The render loop never blocks: photo processing runs on the worker thread; camera stop and restart run on background threads (supervision: 3 s stall, backoff 1 s→30 s, reset after 5 s healthy, baseline taken when the restarted camera is running).
- Presses are ignored while the camera is down and during a shot.
- `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and `cargo test` pass on x86 without the `libcamera` feature; `cargo clippy --all-targets --features libcamera -- -D warnings` passes where libcamera ≥ 0.4 headers exist.
- Commit messages end with the project's attribution trailer lines.

## File Structure

| File | Responsibility |
|---|---|
| `v2/src/pipeline/mod.rs` (new) | module list |
| `v2/src/pipeline/exif.rs` (new) | `PhotoInfo`, `build` (TIFF bytes for APP1) |
| `v2/src/storage.rs` (new) | `Storage` (folders, names, photo count), `write_atomic` |
| `v2/src/pipeline/filters.rs` (new) | `sepia`, `composite`, `to_rgba`, `load_effects` |
| `v2/src/pipeline/photo.rs` (new) | `PhotoJob`, `PhotoDone`, `encode_jpeg`, `process` |
| `v2/src/pipeline/worker.rs` (new) | `Worker` thread, `PhotoResult` |
| `v2/src/state.rs` | capture ids, `Processing`, `Review{Photo|Oops}`, new commands |
| `v2/src/camera/mod.rs`, `fake.rs`, `pi.rs` | `CameraCommand`, `CameraLink`, `CapturedFrame`; fake + libcamera capture |
| `v2/src/monitor.rs` | `recheck_disk` |
| `v2/src/display/controller.rs` | capture flow, worker, background restart, `Supervision` |
| `v2/src/main.rs` | `Arc` camera factory |
| `v2/src/osd/scene.rs`, `v2/src/display/hud.rs`, `v2/src/display/app.rs` | review photo, "Oops", photo count |
| `v2/Cargo.toml`, `v2/.gitignore` (new), `v2/README.md`, `v2/config.example.toml`, spec | deps, ignores, docs |

All commands run from `v2/` unless stated. Tests that read v1 assets use `CARGO_MANIFEST_DIR/..`.

---
### Task 1: EXIF writer

**Files:**
- Create: `v2/src/pipeline/mod.rs`, `v2/src/pipeline/exif.rs`
- Modify: `v2/Cargo.toml`, `v2/src/lib.rs` (add `pub mod pipeline;` after `pub mod osd;`)
- Test: inline in `exif.rs` (parses the output with `kamadak-exif`)

**Interfaces:**
- Produces: `pub struct PhotoInfo { pub taken: chrono::NaiveDateTime, pub exposure_us: Option<i32>, pub gain: Option<f32>, pub gps: Option<(f64, f64)> }` (`Debug, Clone, PartialEq`); `pub fn build(&PhotoInfo) -> Vec<u8>` (little-endian TIFF, no `Exif\0\0` prefix); consts `MAKE`, `MODEL`, `SOFTWARE`.

- [ ] **Step 1: Dependencies and module**

In `v2/Cargo.toml` `[dependencies]` after `anyhow`, add:
```toml
chrono = { version = "0.4", default-features = false, features = ["clock", "std"] }
```

In `[dev-dependencies]` before `tempfile`, add:
```toml
kamadak-exif = "0.6"
```

`v2/src/pipeline/mod.rs` (Tasks 3–4 add modules):
```rust
//! Photo processing: filters, encoding, EXIF and the worker thread.

pub mod exif;
```

- [ ] **Step 2: Write the failing tests**

Create `v2/src/pipeline/exif.rs` with `PhotoInfo`, the consts and `build` stubbed `todo!()`, plus:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use exif::{In, Tag, Value};

    fn info() -> PhotoInfo {
        PhotoInfo {
            taken: NaiveDateTime::parse_from_str("2026-11-19 14:03:07", "%Y-%m-%d %H:%M:%S").unwrap(),
            exposure_us: Some(16_592),
            gain: Some(15.5),
            gps: Some((48.858_37, -2.294_48)),
        }
    }

    fn parse(tiff: Vec<u8>) -> exif::Exif {
        exif::Reader::new().read_raw(tiff).expect("valid TIFF/EXIF")
    }

    fn text(e: &exif::Exif, tag: Tag) -> String {
        e.get_field(tag, In::PRIMARY).unwrap().display_value().to_string()
    }

    #[test]
    fn camera_and_dates_round_trip() {
        let e = parse(build(&info()));
        assert_eq!(text(&e, Tag::Make), "\"Raspberry Pi\"");
        assert_eq!(text(&e, Tag::Model), "\"Global Shutter Camera (IMX296)\"");
        assert_eq!(text(&e, Tag::DateTimeOriginal), "2026-11-19 14:03:07");
        assert_eq!(text(&e, Tag::DateTime), "2026-11-19 14:03:07");
        assert_eq!(e.get_field(Tag::Orientation, In::PRIMARY).unwrap().value.get_uint(0), Some(1));
    }

    #[test]
    fn exposure_and_iso() {
        let e = parse(build(&info()));
        match &e.get_field(Tag::ExposureTime, In::PRIMARY).unwrap().value {
            Value::Rational(v) => assert!((v[0].to_f64() - 0.016_592).abs() < 1e-9),
            other => panic!("{other:?}"),
        }
        let iso = e.get_field(Tag::PhotographicSensitivity, In::PRIMARY).unwrap();
        assert_eq!(iso.value.get_uint(0), Some(1550));
    }

    #[test]
    fn gps_round_trips_with_hemispheres() {
        let e = parse(build(&info()));
        assert_eq!(text(&e, Tag::GPSLatitudeRef), "N");
        assert_eq!(text(&e, Tag::GPSLongitudeRef), "W");
        let deg = |tag| match &e.get_field(tag, In::PRIMARY).unwrap().value {
            Value::Rational(v) => v[0].to_f64() + v[1].to_f64() / 60.0 + v[2].to_f64() / 3600.0,
            other => panic!("{other:?}"),
        };
        assert!((deg(Tag::GPSLatitude) - 48.858_37).abs() < 1e-6);
        assert!((deg(Tag::GPSLongitude) - 2.294_48).abs() < 1e-6);
    }

    #[test]
    fn optional_fields_are_omitted() {
        let e = parse(build(&PhotoInfo { exposure_us: None, gain: None, gps: None, ..info() }));
        assert!(e.get_field(Tag::ExposureTime, In::PRIMARY).is_none());
        assert!(e.get_field(Tag::PhotographicSensitivity, In::PRIMARY).is_none());
        assert!(e.get_field(Tag::GPSLatitude, In::PRIMARY).is_none());
        assert!(e.get_field(Tag::DateTimeOriginal, In::PRIMARY).is_some());
    }
}
```

Run: `cargo test pipeline::exif` — Expected: FAIL (`not yet implemented`).

- [ ] **Step 3: Implement**

`v2/src/pipeline/exif.rs`:
```rust
//! Minimal EXIF writer: a little-endian TIFF block with the fields a photo
//! booth needs (date, camera, exposure, ISO, GPS). Only writes; tests read
//! the result back with a standard EXIF parser.

use chrono::NaiveDateTime;

pub const MAKE: &str = "Raspberry Pi";
pub const MODEL: &str = "Global Shutter Camera (IMX296)";
pub const SOFTWARE: &str = concat!("piBooth ", env!("CARGO_PKG_VERSION"));

#[derive(Debug, Clone, PartialEq)]
pub struct PhotoInfo {
    /// Local wall-clock time of the shot.
    pub taken: NaiveDateTime,
    pub exposure_us: Option<i32>,
    /// Analogue gain; reported as ISO = gain × 100.
    pub gain: Option<f32>,
    /// (latitude, longitude) in decimal degrees.
    pub gps: Option<(f64, f64)>,
}

const ASCII: u16 = 2;
const SHORT: u16 = 3;
const LONG: u16 = 4;
const RATIONAL: u16 = 5;
const BYTE: u16 = 1;

struct Entry {
    tag: u16,
    kind: u16,
    count: u32,
    data: Vec<u8>,
}

fn ascii(tag: u16, s: &str) -> Entry {
    let mut data = s.as_bytes().to_vec();
    data.push(0);
    Entry { tag, kind: ASCII, count: data.len() as u32, data }
}

fn short(tag: u16, v: u16) -> Entry {
    Entry { tag, kind: SHORT, count: 1, data: v.to_le_bytes().to_vec() }
}

fn long(tag: u16, v: u32) -> Entry {
    Entry { tag, kind: LONG, count: 1, data: v.to_le_bytes().to_vec() }
}

fn rationals(tag: u16, values: &[(u32, u32)]) -> Entry {
    let data = values.iter().flat_map(|(n, d)| n.to_le_bytes().into_iter().chain(d.to_le_bytes())).collect();
    Entry { tag, kind: RATIONAL, count: values.len() as u32, data }
}

/// Degrees → (deg, min, sec×1000/1000) rationals.
fn dms(value: f64) -> [(u32, u32); 3] {
    let v = value.abs();
    let deg = v.trunc();
    let min_f = (v - deg) * 60.0;
    let min = min_f.trunc();
    let sec_milli = ((min_f - min) * 60.0 * 1000.0).round();
    [(deg as u32, 1), (min as u32, 1), (sec_milli as u32, 1000)]
}

/// Serialise one IFD at `offset` (from the TIFF header). `next` is the
/// following IFD's offset (0 = none). Values over 4 bytes go after the IFD.
fn write_ifd(mut entries: Vec<Entry>, offset: u32, next: u32) -> Vec<u8> {
    entries.sort_by_key(|e| e.tag);
    let n = entries.len() as u32;
    let mut extra_at = offset + 2 + n * 12 + 4;
    let mut ifd = Vec::new();
    let mut extra = Vec::new();
    ifd.extend_from_slice(&(n as u16).to_le_bytes());
    for e in &entries {
        ifd.extend_from_slice(&e.tag.to_le_bytes());
        ifd.extend_from_slice(&e.kind.to_le_bytes());
        ifd.extend_from_slice(&e.count.to_le_bytes());
        if e.data.len() <= 4 {
            let mut v = e.data.clone();
            v.resize(4, 0);
            ifd.extend_from_slice(&v);
        } else {
            ifd.extend_from_slice(&extra_at.to_le_bytes());
            extra.extend_from_slice(&e.data);
            if extra.len() % 2 == 1 {
                extra.push(0); // keep offsets word-aligned
            }
            extra_at = offset + 2 + n * 12 + 4 + extra.len() as u32;
        }
    }
    ifd.extend_from_slice(&next.to_le_bytes());
    ifd.extend_from_slice(&extra);
    ifd
}

fn ifd_len(entries: &[Entry]) -> u32 {
    let extra: u32 = entries.iter().filter(|e| e.data.len() > 4).map(|e| (e.data.len() as u32 + 1) & !1).sum();
    2 + entries.len() as u32 * 12 + 4 + extra
}

/// TIFF bytes for a JPEG APP1 "Exif" segment (without the `Exif\0\0` header).
pub fn build(info: &PhotoInfo) -> Vec<u8> {
    let stamp = info.taken.format("%Y:%m:%d %H:%M:%S").to_string();

    let mut exif = vec![ascii(0x9003, &stamp), ascii(0x9004, &stamp)];
    if let Some(us) = info.exposure_us.filter(|us| *us > 0) {
        exif.push(rationals(0x829A, &[(us as u32, 1_000_000)]));
    }
    if let Some(g) = info.gain.filter(|g| *g > 0.0) {
        exif.push(short(0x8827, (g * 100.0).round().clamp(1.0, 65535.0) as u16));
    }

    let gps = info.gps.map(|(lat, lon)| {
        vec![
            Entry { tag: 0x0000, kind: BYTE, count: 4, data: vec![2, 3, 0, 0] },
            ascii(0x0001, if lat >= 0.0 { "N" } else { "S" }),
            rationals(0x0002, &dms(lat)),
            ascii(0x0003, if lon >= 0.0 { "E" } else { "W" }),
            rationals(0x0004, &dms(lon)),
        ]
    });

    // IFD0 entries; the pointer values are patched once sizes are known.
    let ifd0_base = |exif_at: u32, gps_at: Option<u32>| {
        let mut e = vec![
            ascii(0x010F, MAKE),
            ascii(0x0110, MODEL),
            short(0x0112, 1), // orientation: top-left (photos are never mirrored)
            ascii(0x0131, SOFTWARE),
            ascii(0x0132, &stamp),
            long(0x8769, exif_at),
        ];
        if let Some(g) = gps_at {
            e.push(long(0x8825, g));
        }
        e
    };

    let ifd0_at = 8u32;
    let ifd0_size = ifd_len(&ifd0_base(0, gps.as_ref().map(|_| 0)));
    let exif_at = ifd0_at + ifd0_size;
    let gps_at = gps.as_ref().map(|_| exif_at + ifd_len(&exif));

    let mut out = b"II\x2a\x00".to_vec();
    out.extend_from_slice(&ifd0_at.to_le_bytes());
    out.extend(write_ifd(ifd0_base(exif_at, gps_at), ifd0_at, 0));
    out.extend(write_ifd(exif, exif_at, 0));
    if let (Some(g), Some(at)) = (gps, gps_at) {
        out.extend(write_ifd(g, at, 0));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use exif::{In, Tag, Value};

    fn info() -> PhotoInfo {
        PhotoInfo {
            taken: NaiveDateTime::parse_from_str("2026-11-19 14:03:07", "%Y-%m-%d %H:%M:%S").unwrap(),
            exposure_us: Some(16_592),
            gain: Some(15.5),
            gps: Some((48.858_37, -2.294_48)),
        }
    }

    fn parse(tiff: Vec<u8>) -> exif::Exif {
        exif::Reader::new().read_raw(tiff).expect("valid TIFF/EXIF")
    }

    fn text(e: &exif::Exif, tag: Tag) -> String {
        e.get_field(tag, In::PRIMARY).unwrap().display_value().to_string()
    }

    #[test]
    fn camera_and_dates_round_trip() {
        let e = parse(build(&info()));
        assert_eq!(text(&e, Tag::Make), "\"Raspberry Pi\"");
        assert_eq!(text(&e, Tag::Model), "\"Global Shutter Camera (IMX296)\"");
        assert_eq!(text(&e, Tag::DateTimeOriginal), "2026-11-19 14:03:07");
        assert_eq!(text(&e, Tag::DateTime), "2026-11-19 14:03:07");
        assert_eq!(e.get_field(Tag::Orientation, In::PRIMARY).unwrap().value.get_uint(0), Some(1));
    }

    #[test]
    fn exposure_and_iso() {
        let e = parse(build(&info()));
        match &e.get_field(Tag::ExposureTime, In::PRIMARY).unwrap().value {
            Value::Rational(v) => assert!((v[0].to_f64() - 0.016_592).abs() < 1e-9),
            other => panic!("{other:?}"),
        }
        let iso = e.get_field(Tag::PhotographicSensitivity, In::PRIMARY).unwrap();
        assert_eq!(iso.value.get_uint(0), Some(1550));
    }

    #[test]
    fn gps_round_trips_with_hemispheres() {
        let e = parse(build(&info()));
        assert_eq!(text(&e, Tag::GPSLatitudeRef), "N");
        assert_eq!(text(&e, Tag::GPSLongitudeRef), "W");
        let deg = |tag| match &e.get_field(tag, In::PRIMARY).unwrap().value {
            Value::Rational(v) => v[0].to_f64() + v[1].to_f64() / 60.0 + v[2].to_f64() / 3600.0,
            other => panic!("{other:?}"),
        };
        assert!((deg(Tag::GPSLatitude) - 48.858_37).abs() < 1e-6);
        assert!((deg(Tag::GPSLongitude) - 2.294_48).abs() < 1e-6);
    }

    #[test]
    fn optional_fields_are_omitted() {
        let e = parse(build(&PhotoInfo { exposure_us: None, gain: None, gps: None, ..info() }));
        assert!(e.get_field(Tag::ExposureTime, In::PRIMARY).is_none());
        assert!(e.get_field(Tag::PhotographicSensitivity, In::PRIMARY).is_none());
        assert!(e.get_field(Tag::GPSLatitude, In::PRIMARY).is_none());
        assert!(e.get_field(Tag::DateTimeOriginal, In::PRIMARY).is_some());
    }
}
```

- [ ] **Step 4: Verify**

Run: `cargo test pipeline::exif && cargo clippy --all-targets -- -D warnings && cargo fmt --check` — Expected: `4 passed`, clean.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock src/lib.rs src/pipeline
git commit -m "feat(v2): EXIF writer for photo metadata"
```

### Task 2: Photo storage

**Files:**
- Create: `v2/src/storage.rs`, `v2/.gitignore`
- Modify: `v2/src/lib.rs` (add `pub mod storage;` after `pub mod stats;`)
- Test: inline in `storage.rs`

**Interfaces:**
- Produces: `pub struct Storage` with `create(root: &Path) -> anyhow::Result<Self>`, `paths_for(&self, NaiveDateTime) -> (PathBuf photo, PathBuf original)`, `photo_count(&self) -> usize`; `pub fn write_atomic(&Path, &[u8]) -> anyhow::Result<()>`.

- [ ] **Step 1: Ignore local photos**

`v2/.gitignore`:
```text
photos/
raw/
```

- [ ] **Step 2: Write the failing tests**

Create `v2/src/storage.rs` with the Interfaces items stubbed `todo!()` (keep the `photos`/`raw` fields), plus:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S").unwrap()
    }

    #[test]
    fn creates_folders_and_names_like_v1() {
        let dir = tempfile::tempdir().unwrap();
        let s = Storage::create(dir.path()).unwrap();
        assert!(dir.path().join("photos").is_dir() && dir.path().join("raw").is_dir());
        let (p, r) = s.paths_for(at("2026-11-19 14:03:07"));
        assert_eq!(p, dir.path().join("photos/booth_20261119-140307.jpg"));
        assert_eq!(r, dir.path().join("raw/booth_20261119-140307.jpg"));
    }

    #[test]
    fn same_second_gets_a_suffix() {
        let dir = tempfile::tempdir().unwrap();
        let s = Storage::create(dir.path()).unwrap();
        let t = at("2026-11-19 14:03:07");
        let (p1, _) = s.paths_for(t);
        write_atomic(&p1, b"x").unwrap();
        let (p2, r2) = s.paths_for(t);
        assert!(p2.ends_with("booth_20261119-140307_2.jpg"), "{p2:?}");
        assert!(r2.ends_with("booth_20261119-140307_2.jpg"));
    }

    #[test]
    fn atomic_write_leaves_no_temp_and_counts() {
        let dir = tempfile::tempdir().unwrap();
        let s = Storage::create(dir.path()).unwrap();
        assert_eq!(s.photo_count(), 0);
        let (p, _) = s.paths_for(at("2026-11-19 14:03:07"));
        write_atomic(&p, b"jpeg").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"jpeg");
        let names: Vec<_> =
            std::fs::read_dir(dir.path().join("photos")).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names.len(), 1, "no .part file left: {names:?}");
        assert_eq!(s.photo_count(), 1);
    }

    #[test]
    fn unwritable_root_is_an_error() {
        assert!(Storage::create(Path::new("/proc/pibooth-test")).is_err());
    }
}
```

Add `pub mod storage;` to `lib.rs`. Run: `cargo test storage::` — Expected: FAIL.

- [ ] **Step 3: Implement**

`v2/src/storage.rs`:
```rust
//! Where photos go (v1 layout): filtered photos in `photos/`, unfiltered
//! originals in `raw/`, named `booth_YYYYMMDD-HHMMSS.jpg`. Files are written
//! to a temporary name and renamed, so a crash never leaves half a JPEG.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::Context;
use chrono::NaiveDateTime;

pub struct Storage {
    photos: PathBuf,
    raw: PathBuf,
}

impl Storage {
    /// Create `photos/` and `raw/` under `root` if missing.
    pub fn create(root: &Path) -> anyhow::Result<Self> {
        let photos = root.join("photos");
        let raw = root.join("raw");
        for dir in [&photos, &raw] {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        Ok(Self { photos, raw })
    }

    /// Paths for a shot taken at `taken`: (photo, original). A second shot in
    /// the same second gets a `_2`, `_3`, … suffix instead of overwriting.
    pub fn paths_for(&self, taken: NaiveDateTime) -> (PathBuf, PathBuf) {
        let stem = format!("booth_{}", taken.format("%Y%m%d-%H%M%S"));
        for n in 1.. {
            let name = if n == 1 { format!("{stem}.jpg") } else { format!("{stem}_{n}.jpg") };
            let (p, r) = (self.photos.join(&name), self.raw.join(&name));
            if !p.exists() && !r.exists() {
                return (p, r);
            }
        }
        unreachable!()
    }

    /// Number of `.jpg` files in `photos/` (shown in the debug overlay).
    pub fn photo_count(&self) -> usize {
        std::fs::read_dir(&self.photos)
            .map(|d| d.filter_map(Result::ok).filter(|e| e.path().extension().is_some_and(|x| x == "jpg")).count())
            .unwrap_or(0)
    }
}

/// Write `bytes` to `path` via a temporary file in the same folder + rename.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let tmp = path.with_extension("jpg.part");
    let mut f = std::fs::File::create(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
    f.write_all(bytes).and_then(|_| f.sync_all()).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("renaming to {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S").unwrap()
    }

    #[test]
    fn creates_folders_and_names_like_v1() {
        let dir = tempfile::tempdir().unwrap();
        let s = Storage::create(dir.path()).unwrap();
        assert!(dir.path().join("photos").is_dir() && dir.path().join("raw").is_dir());
        let (p, r) = s.paths_for(at("2026-11-19 14:03:07"));
        assert_eq!(p, dir.path().join("photos/booth_20261119-140307.jpg"));
        assert_eq!(r, dir.path().join("raw/booth_20261119-140307.jpg"));
    }

    #[test]
    fn same_second_gets_a_suffix() {
        let dir = tempfile::tempdir().unwrap();
        let s = Storage::create(dir.path()).unwrap();
        let t = at("2026-11-19 14:03:07");
        let (p1, _) = s.paths_for(t);
        write_atomic(&p1, b"x").unwrap();
        let (p2, r2) = s.paths_for(t);
        assert!(p2.ends_with("booth_20261119-140307_2.jpg"), "{p2:?}");
        assert!(r2.ends_with("booth_20261119-140307_2.jpg"));
    }

    #[test]
    fn atomic_write_leaves_no_temp_and_counts() {
        let dir = tempfile::tempdir().unwrap();
        let s = Storage::create(dir.path()).unwrap();
        assert_eq!(s.photo_count(), 0);
        let (p, _) = s.paths_for(at("2026-11-19 14:03:07"));
        write_atomic(&p, b"jpeg").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"jpeg");
        let names: Vec<_> =
            std::fs::read_dir(dir.path().join("photos")).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names.len(), 1, "no .part file left: {names:?}");
        assert_eq!(s.photo_count(), 1);
    }

    #[test]
    fn unwritable_root_is_an_error() {
        assert!(Storage::create(Path::new("/proc/pibooth-test")).is_err());
    }
}
```

- [ ] **Step 4: Verify**

Run: `cargo test storage:: && cargo clippy --all-targets -- -D warnings && cargo fmt --check` — Expected: `4 passed`, clean.

- [ ] **Step 5: Commit**

```bash
git add .gitignore src/storage.rs src/lib.rs
git commit -m "feat(v2): photo storage with v1 names and atomic writes"
```

### Task 3: Filters and effect overlays

**Files:**
- Create: `v2/src/pipeline/filters.rs`
- Modify: `v2/src/pipeline/mod.rs` (add `pub mod filters;`)
- Test: inline (reads v1 `effects/` from the repo root)

**Interfaces:**
- Consumes: `osd::Rgba`.
- Produces: `sepia(&mut [u8])`, `composite(&mut [u8], &Rgba)`, `to_rgba(&[u8], w, h) -> Rgba`, `load_effects(&Path, (u32, u32)) -> anyhow::Result<Vec<Rgba>>`.

- [ ] **Step 1: Write the failing tests**

Create `filters.rs` with the four functions stubbed `todo!()`, plus:
```rust
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
```

Register the module. Run: `cargo test pipeline::filters` — Expected: FAIL.

- [ ] **Step 2: Implement**

`v2/src/pipeline/filters.rs`:
```rust
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
```

- [ ] **Step 3: Verify**

Run: `cargo test pipeline::filters && cargo clippy --all-targets -- -D warnings && cargo fmt --check` — Expected: `5 passed`, clean.

- [ ] **Step 4: Commit**

```bash
git add src/pipeline
git commit -m "feat(v2): colour-correct sepia and effect overlays"
```

### Task 4: Photo processing and worker thread

**Files:**
- Create: `v2/src/pipeline/photo.rs`, `v2/src/pipeline/worker.rs`
- Modify: `v2/Cargo.toml`, `v2/src/pipeline/mod.rs`
- Test: inline in both

**Interfaces:**
- Consumes: `PhotoInfo`, `build` (Task 1); `Storage`, `write_atomic` (Task 2); filters (Task 3); `Rgba`.
- Produces: `PhotoJob { id, width, height, rgb, info, filter, jpeg_quality, save_original }`; `PhotoDone { id, photo: PathBuf, original: Option<PathBuf>, review: Rgba }`; `encode_jpeg(&[u8], w, h, quality, exif) -> anyhow::Result<Vec<u8>>`; `process(PhotoJob, &Storage, &[Rgba], pick: impl FnOnce(usize) -> usize) -> anyhow::Result<PhotoDone>`; `PhotoResult { id, outcome: Result<PhotoDone, String> }`; `Worker::start(Storage, Vec<Rgba>) -> anyhow::Result<Worker>`, `submit(&self, PhotoJob)`, `try_recv(&self) -> Option<PhotoResult>`; dropping the worker finishes the current job.

- [ ] **Step 1: Dependencies and module list**

`[dependencies]`: after `dotenvy` add `fastrand = "2"`; after `image = …` add `jpeg-encoder = "0.7"`.
`[dev-dependencies]`: before `kamadak-exif` add:
```toml
image = { version = "0.25", default-features = false, features = ["png", "jpeg"] }
```

`v2/src/pipeline/mod.rs` final content:
```rust
//! Photo processing: filters, encoding, EXIF and the worker thread.

pub mod exif;
pub mod filters;
pub mod photo;
pub mod worker;
```

- [ ] **Step 2: Write the failing tests**

Create both files with the Interfaces items stubbed `todo!()`, plus these tests.

`photo.rs` tests:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDateTime;

    fn job(filter: bool, save_original: bool) -> PhotoJob {
        let (w, h) = (32u32, 16u32);
        PhotoJob {
            id: 7,
            width: w,
            height: h,
            rgb: [200u8, 40, 40].repeat((w * h) as usize), // reddish
            info: PhotoInfo {
                taken: NaiveDateTime::parse_from_str("2026-11-19 14:03:07", "%Y-%m-%d %H:%M:%S").unwrap(),
                exposure_us: Some(10_000),
                gain: Some(2.0),
                gps: Some((48.85, 2.35)),
            },
            filter,
            jpeg_quality: 95,
            save_original,
        }
    }

    fn decode(path: &std::path::Path) -> image::RgbImage {
        image::open(path).unwrap().to_rgb8()
    }

    fn effect(w: u32, h: u32) -> Rgba {
        // Opaque blue left column, transparent elsewhere.
        let mut data = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            let i = (y * w * 4) as usize;
            data[i..i + 4].copy_from_slice(&[0, 0, 255, 255]);
        }
        Rgba { width: w, height: h, data }
    }

    #[test]
    fn filtered_photo_and_plain_original() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::create(dir.path()).unwrap();
        let done = process(job(true, true), &storage, &[effect(32, 16)], |_| 0).unwrap();

        assert_eq!(done.id, 7);
        assert!(done.photo.ends_with("photos/booth_20261119-140307.jpg"));
        let original = done.original.clone().unwrap();
        assert!(original.ends_with("raw/booth_20261119-140307.jpg"));

        let photo = decode(&done.photo);
        assert_eq!(photo.dimensions(), (32, 16));
        let mid = photo.get_pixel(16, 8).0;
        assert!(mid[0] > mid[1] && mid[1] > mid[2], "sepia tone: {mid:?}");
        let edge = photo.get_pixel(0, 8).0;
        assert!(edge[2] > 200 && edge[0] < 60, "effect overlay drawn: {edge:?}");

        let raw = decode(&original).get_pixel(16, 8).0;
        assert!(raw[0] > 180 && raw[1] < 70, "original stays unfiltered: {raw:?}");

        assert_eq!((done.review.width, done.review.height), (32, 16));
        assert_eq!(done.review.pixel(0, 8)[2], 255, "review shows the filtered photo");
    }

    #[test]
    fn exif_is_embedded() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::create(dir.path()).unwrap();
        let done = process(job(true, false), &storage, &[], |_| 0).unwrap();
        let file = std::fs::File::open(&done.photo).unwrap();
        let exif = exif::Reader::new().read_from_container(&mut std::io::BufReader::new(file)).unwrap();
        let dt = exif.get_field(exif::Tag::DateTimeOriginal, exif::In::PRIMARY).unwrap();
        assert_eq!(dt.display_value().to_string(), "2026-11-19 14:03:07");
        assert!(exif.get_field(exif::Tag::GPSLatitude, exif::In::PRIMARY).is_some());
    }

    #[test]
    fn filter_off_saves_plain_photo_and_no_original_when_disabled() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::create(dir.path()).unwrap();
        let done = process(job(false, false), &storage, &[effect(32, 16)], |_| 0).unwrap();
        assert_eq!(done.original, None);
        assert_eq!(std::fs::read_dir(dir.path().join("raw")).unwrap().count(), 0);
        let px = decode(&done.photo).get_pixel(0, 8).0;
        assert!(px[0] > 180 && px[2] < 70, "no sepia, no effect: {px:?}");
    }

    #[test]
    fn effect_index_is_clamped() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::create(dir.path()).unwrap();
        assert!(process(job(true, false), &storage, &[effect(32, 16)], |_| 99).is_ok());
    }
}
```

`worker.rs` tests:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::exif::PhotoInfo;
    use std::time::{Duration, Instant};

    fn job(id: u64) -> PhotoJob {
        PhotoJob {
            id,
            width: 8,
            height: 4,
            rgb: vec![128; 8 * 4 * 3],
            info: PhotoInfo { taken: chrono::Local::now().naive_local(), exposure_us: None, gain: None, gps: None },
            filter: true,
            jpeg_quality: 90,
            save_original: true,
        }
    }

    fn wait(w: &Worker) -> PhotoResult {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(r) = w.try_recv() {
                return r;
            }
            assert!(Instant::now() < deadline, "worker produced no result");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn processes_jobs_in_order_off_thread() {
        let dir = tempfile::tempdir().unwrap();
        let w = Worker::start(Storage::create(dir.path()).unwrap(), Vec::new()).unwrap();
        assert!(w.try_recv().is_none());
        w.submit(job(1));
        w.submit(job(2));
        let a = wait(&w);
        let b = wait(&w);
        assert_eq!((a.id, b.id), (1, 2));
        assert!(a.outcome.is_ok() && b.outcome.is_ok());
        assert_ne!(a.outcome.unwrap().photo, b.outcome.unwrap().photo, "same-second shots get distinct names");
    }

    #[test]
    fn failures_are_reported_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::create(dir.path()).unwrap();
        std::fs::remove_dir(dir.path().join("photos")).unwrap(); // make the write fail
        let w = Worker::start(storage, Vec::new()).unwrap();
        w.submit(job(3));
        let r = wait(&w);
        assert_eq!(r.id, 3);
        assert!(r.outcome.unwrap_err().contains("photos"));
    }
}
```

Run: `cargo test pipeline::` — Expected: the new tests FAIL.

- [ ] **Step 3: Implement**

`v2/src/pipeline/photo.rs`:
```rust
//! One photo, start to finish: optional unfiltered original, sepia + random
//! effect overlay, JPEG with EXIF, atomic writes, and the review image.

use std::path::PathBuf;

use anyhow::Context;
use jpeg_encoder::{ColorType, Encoder, SamplingFactor};

use super::exif::{PhotoInfo, build as build_exif};
use super::filters::{composite, sepia, to_rgba};
use crate::osd::Rgba;
use crate::storage::{Storage, write_atomic};

/// A captured frame plus everything needed to turn it into files.
#[derive(Debug, Clone)]
pub struct PhotoJob {
    pub id: u64,
    pub width: u32,
    pub height: u32,
    /// Tightly packed RGB, row 0 at the top, never mirrored.
    pub rgb: Vec<u8>,
    pub info: PhotoInfo,
    pub filter: bool,
    pub jpeg_quality: u8,
    pub save_original: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PhotoDone {
    pub id: u64,
    pub photo: PathBuf,
    pub original: Option<PathBuf>,
    /// What the guests see during review: the saved photo as RGBA.
    pub review: Rgba,
}

/// Baseline JPEG, 4:4:4 chroma (full colour detail), EXIF in APP1.
pub fn encode_jpeg(rgb: &[u8], width: u32, height: u32, quality: u8, exif: &[u8]) -> anyhow::Result<Vec<u8>> {
    let (w, h) = (u16::try_from(width)?, u16::try_from(height)?);
    let mut out = Vec::with_capacity(rgb.len() / 4);
    let mut enc = Encoder::new(&mut out, quality);
    enc.set_sampling_factor(SamplingFactor::R_4_4_4);
    enc.add_exif_metadata(exif).context("adding EXIF")?;
    enc.encode(rgb, w, h, ColorType::Rgb).context("encoding JPEG")?;
    Ok(out)
}

/// Process `job` and write its files. `pick(n)` chooses which of `n`
/// effects to overlay (random in production, fixed in tests).
pub fn process(
    mut job: PhotoJob,
    storage: &Storage,
    effects: &[Rgba],
    pick: impl FnOnce(usize) -> usize,
) -> anyhow::Result<PhotoDone> {
    let exif = build_exif(&job.info);
    let (photo_path, original_path) = storage.paths_for(job.info.taken);

    let plain = if job.save_original || !job.filter {
        Some(encode_jpeg(&job.rgb, job.width, job.height, job.jpeg_quality, &exif)?)
    } else {
        None
    };
    let original = match (&plain, job.save_original) {
        (Some(bytes), true) => {
            write_atomic(&original_path, bytes)?;
            Some(original_path)
        }
        _ => None,
    };

    let photo_bytes = if job.filter {
        sepia(&mut job.rgb);
        if !effects.is_empty() {
            let i = pick(effects.len()).min(effects.len() - 1);
            composite(&mut job.rgb, &effects[i]);
        }
        encode_jpeg(&job.rgb, job.width, job.height, job.jpeg_quality, &exif)?
    } else {
        plain.expect("plain JPEG exists when the filter is off")
    };
    write_atomic(&photo_path, &photo_bytes)?;

    Ok(PhotoDone { id: job.id, photo: photo_path, original, review: to_rgba(&job.rgb, job.width, job.height) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDateTime;

    fn job(filter: bool, save_original: bool) -> PhotoJob {
        let (w, h) = (32u32, 16u32);
        PhotoJob {
            id: 7,
            width: w,
            height: h,
            rgb: [200u8, 40, 40].repeat((w * h) as usize), // reddish
            info: PhotoInfo {
                taken: NaiveDateTime::parse_from_str("2026-11-19 14:03:07", "%Y-%m-%d %H:%M:%S").unwrap(),
                exposure_us: Some(10_000),
                gain: Some(2.0),
                gps: Some((48.85, 2.35)),
            },
            filter,
            jpeg_quality: 95,
            save_original,
        }
    }

    fn decode(path: &std::path::Path) -> image::RgbImage {
        image::open(path).unwrap().to_rgb8()
    }

    fn effect(w: u32, h: u32) -> Rgba {
        // Opaque blue left column, transparent elsewhere.
        let mut data = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            let i = (y * w * 4) as usize;
            data[i..i + 4].copy_from_slice(&[0, 0, 255, 255]);
        }
        Rgba { width: w, height: h, data }
    }

    #[test]
    fn filtered_photo_and_plain_original() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::create(dir.path()).unwrap();
        let done = process(job(true, true), &storage, &[effect(32, 16)], |_| 0).unwrap();

        assert_eq!(done.id, 7);
        assert!(done.photo.ends_with("photos/booth_20261119-140307.jpg"));
        let original = done.original.clone().unwrap();
        assert!(original.ends_with("raw/booth_20261119-140307.jpg"));

        let photo = decode(&done.photo);
        assert_eq!(photo.dimensions(), (32, 16));
        let mid = photo.get_pixel(16, 8).0;
        assert!(mid[0] > mid[1] && mid[1] > mid[2], "sepia tone: {mid:?}");
        let edge = photo.get_pixel(0, 8).0;
        assert!(edge[2] > 200 && edge[0] < 60, "effect overlay drawn: {edge:?}");

        let raw = decode(&original).get_pixel(16, 8).0;
        assert!(raw[0] > 180 && raw[1] < 70, "original stays unfiltered: {raw:?}");

        assert_eq!((done.review.width, done.review.height), (32, 16));
        assert_eq!(done.review.pixel(0, 8)[2], 255, "review shows the filtered photo");
    }

    #[test]
    fn exif_is_embedded() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::create(dir.path()).unwrap();
        let done = process(job(true, false), &storage, &[], |_| 0).unwrap();
        let file = std::fs::File::open(&done.photo).unwrap();
        let exif = exif::Reader::new().read_from_container(&mut std::io::BufReader::new(file)).unwrap();
        let dt = exif.get_field(exif::Tag::DateTimeOriginal, exif::In::PRIMARY).unwrap();
        assert_eq!(dt.display_value().to_string(), "2026-11-19 14:03:07");
        assert!(exif.get_field(exif::Tag::GPSLatitude, exif::In::PRIMARY).is_some());
    }

    #[test]
    fn filter_off_saves_plain_photo_and_no_original_when_disabled() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::create(dir.path()).unwrap();
        let done = process(job(false, false), &storage, &[effect(32, 16)], |_| 0).unwrap();
        assert_eq!(done.original, None);
        assert_eq!(std::fs::read_dir(dir.path().join("raw")).unwrap().count(), 0);
        let px = decode(&done.photo).get_pixel(0, 8).0;
        assert!(px[0] > 180 && px[2] < 70, "no sepia, no effect: {px:?}");
    }

    #[test]
    fn effect_index_is_clamped() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::create(dir.path()).unwrap();
        assert!(process(job(true, false), &storage, &[effect(32, 16)], |_| 99).is_ok());
    }
}
```

`v2/src/pipeline/worker.rs`:
```rust
//! Runs photo jobs on a background thread so the preview never stutters.

use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::JoinHandle;

use super::photo::{PhotoDone, PhotoJob, process};
use crate::osd::Rgba;
use crate::storage::Storage;

#[derive(Debug)]
pub struct PhotoResult {
    pub id: u64,
    pub outcome: Result<PhotoDone, String>,
}

pub struct Worker {
    jobs: Option<Sender<PhotoJob>>,
    results: Receiver<PhotoResult>,
    thread: Option<JoinHandle<()>>,
}

impl Worker {
    pub fn start(storage: Storage, effects: Vec<Rgba>) -> anyhow::Result<Self> {
        let (job_tx, job_rx) = channel::<PhotoJob>();
        let (res_tx, res_rx) = channel();
        let thread = std::thread::Builder::new().name("photo-worker".into()).spawn(move || {
            for job in job_rx {
                let id = job.id;
                let started = std::time::Instant::now();
                let outcome = process(job, &storage, &effects, |n| fastrand::usize(..n)).map_err(|e| format!("{e:#}"));
                match &outcome {
                    Ok(d) => tracing::info!(id, photo = %d.photo.display(), ms = started.elapsed().as_millis() as u64, "photo saved"),
                    Err(e) => tracing::error!(id, "photo failed: {e}"),
                }
                if res_tx.send(PhotoResult { id, outcome }).is_err() {
                    break;
                }
            }
        })?;
        Ok(Self { jobs: Some(job_tx), results: res_rx, thread: Some(thread) })
    }

    pub fn submit(&self, job: PhotoJob) {
        if let Some(tx) = &self.jobs
            && tx.send(job).is_err()
        {
            tracing::error!("photo worker is gone; job dropped");
        }
    }

    pub fn try_recv(&self) -> Option<PhotoResult> {
        // Empty and disconnected (worker gone) both mean "nothing now".
        self.results.try_recv().ok()
    }
}

impl Drop for Worker {
    /// Finishes the job in progress (a half-saved photo is worse than a short wait).
    fn drop(&mut self) {
        self.jobs.take();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::exif::PhotoInfo;
    use std::time::{Duration, Instant};

    fn job(id: u64) -> PhotoJob {
        PhotoJob {
            id,
            width: 8,
            height: 4,
            rgb: vec![128; 8 * 4 * 3],
            info: PhotoInfo { taken: chrono::Local::now().naive_local(), exposure_us: None, gain: None, gps: None },
            filter: true,
            jpeg_quality: 90,
            save_original: true,
        }
    }

    fn wait(w: &Worker) -> PhotoResult {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(r) = w.try_recv() {
                return r;
            }
            assert!(Instant::now() < deadline, "worker produced no result");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn processes_jobs_in_order_off_thread() {
        let dir = tempfile::tempdir().unwrap();
        let w = Worker::start(Storage::create(dir.path()).unwrap(), Vec::new()).unwrap();
        assert!(w.try_recv().is_none());
        w.submit(job(1));
        w.submit(job(2));
        let a = wait(&w);
        let b = wait(&w);
        assert_eq!((a.id, b.id), (1, 2));
        assert!(a.outcome.is_ok() && b.outcome.is_ok());
        assert_ne!(a.outcome.unwrap().photo, b.outcome.unwrap().photo, "same-second shots get distinct names");
    }

    #[test]
    fn failures_are_reported_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::create(dir.path()).unwrap();
        std::fs::remove_dir(dir.path().join("photos")).unwrap(); // make the write fail
        let w = Worker::start(storage, Vec::new()).unwrap();
        w.submit(job(3));
        let r = wait(&w);
        assert_eq!(r.id, 3);
        assert!(r.outcome.unwrap_err().contains("photos"));
    }
}
```

- [ ] **Step 4: Verify**

Run: `cargo test pipeline:: && cargo clippy --all-targets -- -D warnings && cargo fmt --check` — Expected: 15 under `pipeline::` (4 exif + 5 filters + 4 photo + 2 worker), clean.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock src/pipeline
git commit -m "feat(v2): JPEG photo processing on a worker thread"
```

### Task 5: Capture flow (state machine, cameras, controller)

This task changes interfaces shared by several files, so they change together to keep the crate compiling. Pure logic is test-first; `pi.rs` is compile/lint-checked here and verified on the Pi in Task 7.

**Files (replace whole files unless noted):**
- `v2/src/state.rs`, `v2/src/camera/mod.rs`, `v2/src/camera/fake.rs`, `v2/src/camera/pi.rs`, `v2/src/monitor.rs`, `v2/src/display/controller.rs`, `v2/src/main.rs`
- Modify: `v2/Cargo.toml` (libcamera feature), `v2/src/osd/scene.rs` (one test constant)

**Interfaces:**
- Consumes: Tasks 1–4; existing `FrameMailbox`, `Backoff`, `Monitors`, `GpioButton`, `SceneInput`.
- Produces:
  - `state`: `Timings { countdown, review, idle, prepare_lead, screen_flash: bool, flash_max, capture_timeout, processing_timeout }`; `Outcome { Photo, Oops }`; `Phase { Idle, Live, Countdown { deadline, prepared }, Capturing { id, since }, Processing { id, since }, Review { until, outcome } }`; `Event { Press, FrameCaptured { id }, PhotoReady { id }, PhotoFailed { id }, CameraLost }`; `Command { PrepareCapture, FlashOn, FlashOff, Capture { id }, ReleaseCapture }`; `View { title, countdown, flash, review: Option<Outcome> }`.
  - `camera`: `CameraCommand { PrepareCapture, Capture { id }, ReleaseCapture }`; `CaptureMeta { exposure_us, gain }`; `CapturedFrame { id, frame: Frame, meta }`; `CameraLink { mailbox: Arc<FrameMailbox>, captures: Sender<CapturedFrame> }`; `trait Camera: Send { start(&mut self, CameraLink), command(&mut self, CameraCommand), stop(&mut self) }`.
  - `Monitors::recheck_disk(&self)`.
  - `controller`: `pub type CameraFactory = Arc<dyn Fn() -> Box<dyn Camera> + Send + Sync>`; `Supervision { stall, healthy_after, backoff_initial, backoff_max }` (`Default` = 3 s, 5 s, 1 s, 30 s); `Controller::start(cfg, factory, button, rx, now)` and `start_with(…, Supervision, now)`; existing methods plus `photo_count() -> usize`, `take_review_image() -> Option<Rgba>`.

- [ ] **Step 1: Enable the draft controls**

In `v2/Cargo.toml` change the libcamera line to:
```toml
libcamera = { version = "0.8", optional = true, features = ["vendor_draft"] }
```

- [ ] **Step 2: State machine (test-first)**

Changing these types breaks the controller until Step 6, so this task's tests first run in Step 7. Write the test module first so the implementation is checked against it. Tests for `v2/src/state.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    const S: Duration = Duration::from_secs(1);
    const MS: Duration = Duration::from_millis(1);

    fn timings() -> Timings {
        Timings {
            countdown: 5 * S,
            review: 5 * S,
            idle: 120 * S,
            prepare_lead: 300 * MS,
            screen_flash: true,
            flash_max: 800 * MS,
            capture_timeout: 5 * S,
            processing_timeout: 10 * S,
        }
    }

    /// Press at t0 and run to the capture; returns (booth, capture id).
    fn to_capture(t0: Instant) -> (Booth, u64) {
        let mut b = Booth::new(timings(), t0);
        b.handle(Event::Press, t0);
        b.tick(t0 + 4700 * MS);
        let cmds = b.tick(t0 + 5 * S);
        let Some(Command::Capture { id }) = cmds.last().copied() else { panic!("{cmds:?}") };
        (b, id)
    }

    #[test]
    fn starts_idle_with_title() {
        let t0 = Instant::now();
        let b = Booth::new(timings(), t0);
        assert_eq!(b.phase(), Phase::Idle);
        assert_eq!(b.view(t0), View { title: true, countdown: None, flash: false, review: None });
    }

    #[test]
    fn press_starts_countdown_showing_5_to_1() {
        let t0 = Instant::now();
        let mut b = Booth::new(timings(), t0);
        assert!(b.handle(Event::Press, t0).is_empty());
        assert_eq!(b.view(t0).countdown, Some(5));
        assert!(!b.view(t0).title);
        assert_eq!(b.view(t0 + 1 * S).countdown, Some(4));
        assert_eq!(b.view(t0 + 4500 * MS).countdown, Some(1));
        assert_eq!(b.view(t0 + 4 * S + 1 * MS).countdown, Some(1), "no millisecond truncation");
        assert_eq!(b.view(t0 + 5 * S).countdown, None);
    }

    #[test]
    fn prepare_and_flash_lead_the_capture_once() {
        let t0 = Instant::now();
        let mut b = Booth::new(timings(), t0);
        b.handle(Event::Press, t0);
        assert!(b.tick(t0 + 4600 * MS).is_empty());
        assert_eq!(b.tick(t0 + 4700 * MS), vec![Command::PrepareCapture, Command::FlashOn]);
        assert!(b.view(t0 + 4700 * MS).flash);
        assert!(b.tick(t0 + 4800 * MS).is_empty(), "only once");
        assert_eq!(b.tick(t0 + 5 * S), vec![Command::Capture { id: 1 }]);
        assert_eq!(b.phase(), Phase::Capturing { id: 1, since: t0 + 5 * S });
    }

    #[test]
    fn without_screen_flash_still_prepares() {
        let t0 = Instant::now();
        let mut b = Booth::new(Timings { screen_flash: false, ..timings() }, t0);
        b.handle(Event::Press, t0);
        assert_eq!(b.tick(t0 + 4700 * MS), vec![Command::PrepareCapture]);
        assert!(!b.view(t0 + 4800 * MS).flash);
    }

    #[test]
    fn full_shot_to_photo_review_then_live() {
        let t0 = Instant::now();
        let (mut b, id) = to_capture(t0);
        let t1 = t0 + 5050 * MS;
        assert_eq!(b.handle(Event::FrameCaptured { id }, t1), vec![Command::FlashOff, Command::ReleaseCapture]);
        assert_eq!(b.phase(), Phase::Processing { id, since: t1 });
        assert!(!b.view(t1).flash);
        let t2 = t1 + 200 * MS;
        assert!(b.handle(Event::PhotoReady { id }, t2).is_empty());
        assert_eq!(b.view(t2).review, Some(Outcome::Photo));
        assert!(b.tick(t2 + 4 * S).is_empty());
        b.tick(t2 + 5 * S);
        assert_eq!(b.phase(), Phase::Live);
    }

    #[test]
    fn processing_failure_shows_oops() {
        let t0 = Instant::now();
        let (mut b, id) = to_capture(t0);
        b.handle(Event::FrameCaptured { id }, t0 + 5 * S);
        b.handle(Event::PhotoFailed { id }, t0 + 6 * S);
        assert_eq!(b.view(t0 + 6 * S).review, Some(Outcome::Oops));
    }

    #[test]
    fn results_for_other_ids_are_ignored() {
        let t0 = Instant::now();
        let (mut b, id) = to_capture(t0);
        assert!(b.handle(Event::FrameCaptured { id: id + 1 }, t0 + 5 * S).is_empty());
        assert!(matches!(b.phase(), Phase::Capturing { .. }));
        b.handle(Event::FrameCaptured { id }, t0 + 5 * S);
        b.handle(Event::PhotoReady { id: id + 7 }, t0 + 5 * S);
        assert!(matches!(b.phase(), Phase::Processing { .. }));
    }

    #[test]
    fn ids_increase_per_shot() {
        let t0 = Instant::now();
        let (mut b, first) = to_capture(t0);
        b.handle(Event::FrameCaptured { id: first }, t0 + 5 * S);
        b.handle(Event::PhotoReady { id: first }, t0 + 5 * S);
        b.tick(t0 + 10 * S);
        b.handle(Event::Press, t0 + 11 * S);
        b.tick(t0 + 15700 * MS);
        assert_eq!(b.tick(t0 + 16 * S), vec![Command::Capture { id: first + 1 }]);
    }

    #[test]
    fn presses_ignored_during_a_shot() {
        let t0 = Instant::now();
        let mut b = Booth::new(timings(), t0);
        b.handle(Event::Press, t0);
        b.handle(Event::Press, t0 + 1 * S);
        assert!(matches!(b.phase(), Phase::Countdown { deadline, .. } if deadline == t0 + 5 * S));
        let (mut b, id) = to_capture(t0);
        b.handle(Event::Press, t0 + 5 * S);
        assert!(matches!(b.phase(), Phase::Capturing { .. }));
        b.handle(Event::FrameCaptured { id }, t0 + 5 * S);
        b.handle(Event::Press, t0 + 5 * S);
        assert!(matches!(b.phase(), Phase::Processing { .. }));
        b.handle(Event::PhotoReady { id }, t0 + 5 * S);
        b.handle(Event::Press, t0 + 6 * S);
        assert!(matches!(b.phase(), Phase::Review { .. }));
    }

    #[test]
    fn live_returns_to_idle_after_timeout() {
        let t0 = Instant::now();
        let (mut b, id) = to_capture(t0);
        b.handle(Event::FrameCaptured { id }, t0 + 5 * S);
        b.handle(Event::PhotoReady { id }, t0 + 5 * S);
        let live_at = t0 + 10 * S;
        b.tick(live_at);
        assert_eq!(b.phase(), Phase::Live);
        b.tick(live_at + 119 * S);
        assert_eq!(b.phase(), Phase::Live);
        b.tick(live_at + 120 * S);
        assert!(b.view(live_at + 120 * S).title);
    }

    #[test]
    fn flash_is_capped_and_capture_timeout_shows_oops() {
        let t0 = Instant::now();
        let (mut b, _) = to_capture(t0); // flash on at 4.7 s
        assert_eq!(b.tick(t0 + 5500 * MS), vec![Command::FlashOff], "flash_max = 800 ms");
        assert!(b.tick(t0 + 9 * S).is_empty());
        assert!(matches!(b.phase(), Phase::Capturing { .. }));
        assert_eq!(b.tick(t0 + 10 * S), vec![Command::ReleaseCapture], "capture timed out after 5 s");
        assert_eq!(b.view(t0 + 10 * S).review, Some(Outcome::Oops));
    }

    #[test]
    fn processing_timeout_shows_oops() {
        let t0 = Instant::now();
        let (mut b, id) = to_capture(t0);
        b.handle(Event::FrameCaptured { id }, t0 + 5 * S);
        b.tick(t0 + 14 * S);
        assert!(matches!(b.phase(), Phase::Processing { .. }));
        b.tick(t0 + 15 * S);
        assert_eq!(b.view(t0 + 15 * S).review, Some(Outcome::Oops));
    }

    #[test]
    fn camera_lost_releases_and_goes_idle() {
        let t0 = Instant::now();
        let mut b = Booth::new(timings(), t0);
        b.handle(Event::Press, t0);
        assert!(b.handle(Event::CameraLost, t0 + 1 * S).is_empty(), "nothing prepared yet");
        b.handle(Event::Press, t0 + 2 * S);
        b.tick(t0 + 6700 * MS);
        assert_eq!(b.handle(Event::CameraLost, t0 + 6800 * MS), vec![Command::FlashOff, Command::ReleaseCapture]);
        assert_eq!(b.phase(), Phase::Idle);
        let (mut b, id) = to_capture(t0);
        b.handle(Event::FrameCaptured { id }, t0 + 5 * S);
        assert!(b.handle(Event::CameraLost, t0 + 5 * S).is_empty());
        assert!(b.handle(Event::PhotoReady { id }, t0 + 6 * S).is_empty());
        assert_eq!(b.phase(), Phase::Idle, "late result after camera loss is ignored");
    }
}
```

Implementation — `v2/src/state.rs` (includes the tests above):
```rust
//! The booth's behaviour as a pure state machine (spec §4.1). Time is passed
//! in by the caller, so every transition is deterministic and testable.
//! The machine only decides; the app carries out the returned [`Command`]s.
//!
//! Capture flow: `lead` before the deadline the camera is prepared (high
//! quality noise reduction; screen flash on in screen mode) so auto-exposure
//! meters the lit scene. At the deadline the next frame is captured with
//! exposure and white balance locked; once it arrives the flash goes off,
//! the lock is released and the photo is processed; review shows the result
//! or "Oops, try again". Every capture has an id; results for another id are
//! ignored.

use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Timings {
    pub countdown: Duration,
    pub review: Duration,
    pub idle: Duration,
    /// Prepare the camera (and turn the screen flash on) this long before the shot.
    pub prepare_lead: Duration,
    /// Whether the screen flash is used (`flash.mode = "screen"`).
    pub screen_flash: bool,
    /// Longest the screen flash may stay on.
    pub flash_max: Duration,
    /// Give up on a frame that never arrives.
    pub capture_timeout: Duration,
    /// Give up on processing that never finishes.
    pub processing_timeout: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Photo,
    Oops,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Phase {
    Idle,
    Live,
    Countdown { deadline: Instant, prepared: bool },
    Capturing { id: u64, since: Instant },
    Processing { id: u64, since: Instant },
    Review { until: Instant, outcome: Outcome },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// Button or space bar pressed (one edge = one event).
    Press,
    /// The camera delivered the frame for capture `id`.
    FrameCaptured {
        id: u64,
    },
    PhotoReady {
        id: u64,
    },
    PhotoFailed {
        id: u64,
    },
    /// The camera stopped; the app is restarting it.
    CameraLost,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// High-quality noise reduction on; auto-exposure keeps metering.
    PrepareCapture,
    FlashOn,
    FlashOff,
    /// Lock exposure/white balance and deliver the next frame as capture `id`.
    Capture {
        id: u64,
    },
    /// Back to preview settings: auto exposure/white balance, fast noise reduction.
    ReleaseCapture,
}

/// What the screen should show, derived from the state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct View {
    pub title: bool,
    pub countdown: Option<u32>,
    pub flash: bool,
    pub review: Option<Outcome>,
}

pub struct Booth {
    timings: Timings,
    phase: Phase,
    flash_since: Option<Instant>,
    last_activity: Instant,
    next_id: u64,
}

impl Booth {
    pub fn new(timings: Timings, now: Instant) -> Self {
        Self { timings, phase: Phase::Idle, flash_since: None, last_activity: now, next_id: 1 }
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    pub fn handle(&mut self, event: Event, now: Instant) -> Vec<Command> {
        let mut out = Vec::new();
        match (self.phase, event) {
            (Phase::Idle | Phase::Live, Event::Press) => {
                self.phase = Phase::Countdown { deadline: now + self.timings.countdown, prepared: false };
                self.last_activity = now;
            }
            (Phase::Capturing { id, .. }, Event::FrameCaptured { id: got }) if got == id => {
                self.flash_off(&mut out);
                out.push(Command::ReleaseCapture);
                self.phase = Phase::Processing { id, since: now };
            }
            (Phase::Processing { id, .. }, Event::PhotoReady { id: got }) if got == id => {
                self.review(Outcome::Photo, now);
            }
            (Phase::Processing { id, .. }, Event::PhotoFailed { id: got }) if got == id => {
                self.review(Outcome::Oops, now);
            }
            (phase, Event::CameraLost) => {
                self.flash_off(&mut out);
                if matches!(phase, Phase::Countdown { prepared: true, .. } | Phase::Capturing { .. }) {
                    out.push(Command::ReleaseCapture);
                }
                self.phase = Phase::Idle;
                self.last_activity = now;
            }
            // Presses during a shot, and results for other captures, are ignored.
            _ => {}
        }
        out
    }

    /// Advance time-driven transitions. Call every frame.
    pub fn tick(&mut self, now: Instant) -> Vec<Command> {
        let mut out = Vec::new();
        match self.phase {
            Phase::Idle => {}
            Phase::Live => {
                if now.duration_since(self.last_activity) >= self.timings.idle {
                    self.phase = Phase::Idle;
                }
            }
            Phase::Countdown { deadline, prepared } => {
                if !prepared && now + self.timings.prepare_lead >= deadline {
                    out.push(Command::PrepareCapture);
                    if self.timings.screen_flash {
                        self.flash_since = Some(now);
                        out.push(Command::FlashOn);
                    }
                    self.phase = Phase::Countdown { deadline, prepared: true };
                }
                if now >= deadline {
                    let id = self.next_id;
                    self.next_id += 1;
                    self.phase = Phase::Capturing { id, since: now };
                    out.push(Command::Capture { id });
                }
            }
            Phase::Capturing { since, .. } => {
                if self.flash_since.is_some_and(|t| now.duration_since(t) >= self.timings.flash_max) {
                    self.flash_off(&mut out);
                }
                if now.duration_since(since) >= self.timings.capture_timeout {
                    self.flash_off(&mut out);
                    out.push(Command::ReleaseCapture);
                    self.review(Outcome::Oops, now);
                }
            }
            Phase::Processing { since, .. } => {
                if now.duration_since(since) >= self.timings.processing_timeout {
                    self.review(Outcome::Oops, now);
                }
            }
            Phase::Review { until, .. } => {
                if now >= until {
                    self.phase = Phase::Live;
                    self.last_activity = now;
                }
            }
        }
        out
    }

    pub fn view(&self, now: Instant) -> View {
        let countdown = match self.phase {
            Phase::Countdown { deadline, .. } if deadline > now => {
                Some(deadline.duration_since(now).as_nanos().div_ceil(1_000_000_000) as u32)
            }
            _ => None,
        };
        let review = match self.phase {
            Phase::Review { outcome, .. } => Some(outcome),
            _ => None,
        };
        View { title: self.phase == Phase::Idle, countdown, flash: self.flash_since.is_some(), review }
    }

    fn flash_off(&mut self, out: &mut Vec<Command>) {
        if self.flash_since.take().is_some() {
            out.push(Command::FlashOff);
        }
    }

    fn review(&mut self, outcome: Outcome, now: Instant) {
        self.phase = Phase::Review { until: now + self.timings.review, outcome };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: Duration = Duration::from_secs(1);
    const MS: Duration = Duration::from_millis(1);

    fn timings() -> Timings {
        Timings {
            countdown: 5 * S,
            review: 5 * S,
            idle: 120 * S,
            prepare_lead: 300 * MS,
            screen_flash: true,
            flash_max: 800 * MS,
            capture_timeout: 5 * S,
            processing_timeout: 10 * S,
        }
    }

    /// Press at t0 and run to the capture; returns (booth, capture id).
    fn to_capture(t0: Instant) -> (Booth, u64) {
        let mut b = Booth::new(timings(), t0);
        b.handle(Event::Press, t0);
        b.tick(t0 + 4700 * MS);
        let cmds = b.tick(t0 + 5 * S);
        let Some(Command::Capture { id }) = cmds.last().copied() else { panic!("{cmds:?}") };
        (b, id)
    }

    #[test]
    fn starts_idle_with_title() {
        let t0 = Instant::now();
        let b = Booth::new(timings(), t0);
        assert_eq!(b.phase(), Phase::Idle);
        assert_eq!(b.view(t0), View { title: true, countdown: None, flash: false, review: None });
    }

    #[test]
    fn press_starts_countdown_showing_5_to_1() {
        let t0 = Instant::now();
        let mut b = Booth::new(timings(), t0);
        assert!(b.handle(Event::Press, t0).is_empty());
        assert_eq!(b.view(t0).countdown, Some(5));
        assert!(!b.view(t0).title);
        assert_eq!(b.view(t0 + 1 * S).countdown, Some(4));
        assert_eq!(b.view(t0 + 4500 * MS).countdown, Some(1));
        assert_eq!(b.view(t0 + 4 * S + 1 * MS).countdown, Some(1), "no millisecond truncation");
        assert_eq!(b.view(t0 + 5 * S).countdown, None);
    }

    #[test]
    fn prepare_and_flash_lead_the_capture_once() {
        let t0 = Instant::now();
        let mut b = Booth::new(timings(), t0);
        b.handle(Event::Press, t0);
        assert!(b.tick(t0 + 4600 * MS).is_empty());
        assert_eq!(b.tick(t0 + 4700 * MS), vec![Command::PrepareCapture, Command::FlashOn]);
        assert!(b.view(t0 + 4700 * MS).flash);
        assert!(b.tick(t0 + 4800 * MS).is_empty(), "only once");
        assert_eq!(b.tick(t0 + 5 * S), vec![Command::Capture { id: 1 }]);
        assert_eq!(b.phase(), Phase::Capturing { id: 1, since: t0 + 5 * S });
    }

    #[test]
    fn without_screen_flash_still_prepares() {
        let t0 = Instant::now();
        let mut b = Booth::new(Timings { screen_flash: false, ..timings() }, t0);
        b.handle(Event::Press, t0);
        assert_eq!(b.tick(t0 + 4700 * MS), vec![Command::PrepareCapture]);
        assert!(!b.view(t0 + 4800 * MS).flash);
    }

    #[test]
    fn full_shot_to_photo_review_then_live() {
        let t0 = Instant::now();
        let (mut b, id) = to_capture(t0);
        let t1 = t0 + 5050 * MS;
        assert_eq!(b.handle(Event::FrameCaptured { id }, t1), vec![Command::FlashOff, Command::ReleaseCapture]);
        assert_eq!(b.phase(), Phase::Processing { id, since: t1 });
        assert!(!b.view(t1).flash);
        let t2 = t1 + 200 * MS;
        assert!(b.handle(Event::PhotoReady { id }, t2).is_empty());
        assert_eq!(b.view(t2).review, Some(Outcome::Photo));
        assert!(b.tick(t2 + 4 * S).is_empty());
        b.tick(t2 + 5 * S);
        assert_eq!(b.phase(), Phase::Live);
    }

    #[test]
    fn processing_failure_shows_oops() {
        let t0 = Instant::now();
        let (mut b, id) = to_capture(t0);
        b.handle(Event::FrameCaptured { id }, t0 + 5 * S);
        b.handle(Event::PhotoFailed { id }, t0 + 6 * S);
        assert_eq!(b.view(t0 + 6 * S).review, Some(Outcome::Oops));
    }

    #[test]
    fn results_for_other_ids_are_ignored() {
        let t0 = Instant::now();
        let (mut b, id) = to_capture(t0);
        assert!(b.handle(Event::FrameCaptured { id: id + 1 }, t0 + 5 * S).is_empty());
        assert!(matches!(b.phase(), Phase::Capturing { .. }));
        b.handle(Event::FrameCaptured { id }, t0 + 5 * S);
        b.handle(Event::PhotoReady { id: id + 7 }, t0 + 5 * S);
        assert!(matches!(b.phase(), Phase::Processing { .. }));
    }

    #[test]
    fn ids_increase_per_shot() {
        let t0 = Instant::now();
        let (mut b, first) = to_capture(t0);
        b.handle(Event::FrameCaptured { id: first }, t0 + 5 * S);
        b.handle(Event::PhotoReady { id: first }, t0 + 5 * S);
        b.tick(t0 + 10 * S);
        b.handle(Event::Press, t0 + 11 * S);
        b.tick(t0 + 15700 * MS);
        assert_eq!(b.tick(t0 + 16 * S), vec![Command::Capture { id: first + 1 }]);
    }

    #[test]
    fn presses_ignored_during_a_shot() {
        let t0 = Instant::now();
        let mut b = Booth::new(timings(), t0);
        b.handle(Event::Press, t0);
        b.handle(Event::Press, t0 + 1 * S);
        assert!(matches!(b.phase(), Phase::Countdown { deadline, .. } if deadline == t0 + 5 * S));
        let (mut b, id) = to_capture(t0);
        b.handle(Event::Press, t0 + 5 * S);
        assert!(matches!(b.phase(), Phase::Capturing { .. }));
        b.handle(Event::FrameCaptured { id }, t0 + 5 * S);
        b.handle(Event::Press, t0 + 5 * S);
        assert!(matches!(b.phase(), Phase::Processing { .. }));
        b.handle(Event::PhotoReady { id }, t0 + 5 * S);
        b.handle(Event::Press, t0 + 6 * S);
        assert!(matches!(b.phase(), Phase::Review { .. }));
    }

    #[test]
    fn live_returns_to_idle_after_timeout() {
        let t0 = Instant::now();
        let (mut b, id) = to_capture(t0);
        b.handle(Event::FrameCaptured { id }, t0 + 5 * S);
        b.handle(Event::PhotoReady { id }, t0 + 5 * S);
        let live_at = t0 + 10 * S;
        b.tick(live_at);
        assert_eq!(b.phase(), Phase::Live);
        b.tick(live_at + 119 * S);
        assert_eq!(b.phase(), Phase::Live);
        b.tick(live_at + 120 * S);
        assert!(b.view(live_at + 120 * S).title);
    }

    #[test]
    fn flash_is_capped_and_capture_timeout_shows_oops() {
        let t0 = Instant::now();
        let (mut b, _) = to_capture(t0); // flash on at 4.7 s
        assert_eq!(b.tick(t0 + 5500 * MS), vec![Command::FlashOff], "flash_max = 800 ms");
        assert!(b.tick(t0 + 9 * S).is_empty());
        assert!(matches!(b.phase(), Phase::Capturing { .. }));
        assert_eq!(b.tick(t0 + 10 * S), vec![Command::ReleaseCapture], "capture timed out after 5 s");
        assert_eq!(b.view(t0 + 10 * S).review, Some(Outcome::Oops));
    }

    #[test]
    fn processing_timeout_shows_oops() {
        let t0 = Instant::now();
        let (mut b, id) = to_capture(t0);
        b.handle(Event::FrameCaptured { id }, t0 + 5 * S);
        b.tick(t0 + 14 * S);
        assert!(matches!(b.phase(), Phase::Processing { .. }));
        b.tick(t0 + 15 * S);
        assert_eq!(b.view(t0 + 15 * S).review, Some(Outcome::Oops));
    }

    #[test]
    fn camera_lost_releases_and_goes_idle() {
        let t0 = Instant::now();
        let mut b = Booth::new(timings(), t0);
        b.handle(Event::Press, t0);
        assert!(b.handle(Event::CameraLost, t0 + 1 * S).is_empty(), "nothing prepared yet");
        b.handle(Event::Press, t0 + 2 * S);
        b.tick(t0 + 6700 * MS);
        assert_eq!(b.handle(Event::CameraLost, t0 + 6800 * MS), vec![Command::FlashOff, Command::ReleaseCapture]);
        assert_eq!(b.phase(), Phase::Idle);
        let (mut b, id) = to_capture(t0);
        b.handle(Event::FrameCaptured { id }, t0 + 5 * S);
        assert!(b.handle(Event::CameraLost, t0 + 5 * S).is_empty());
        assert!(b.handle(Event::PhotoReady { id }, t0 + 6 * S).is_empty());
        assert_eq!(b.phase(), Phase::Idle, "late result after camera loss is ignored");
    }
}
```

- [ ] **Step 3: Camera interface and fake camera**

`v2/src/camera/mod.rs`:
```rust
//! Camera sources. Each implementation runs its own thread and publishes
//! preview frames into a [`FrameMailbox`].

use std::sync::Arc;
use std::sync::mpsc::Sender;

use crate::frame::{Frame, FrameMailbox};

pub mod fake;

#[cfg(feature = "libcamera")]
pub mod pi;

/// IMX296 native resolution (spec §1).
pub const SENSOR_SIZE: (u32, u32) = (1456, 1088);
/// Sensor maximum frame rate at full resolution.
pub const SENSOR_FPS: u32 = 60;

/// Delay before each camera restart attempt: doubles from `initial` up to
/// `max`, and resets after a successful start (spec §5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Backoff {
    initial: std::time::Duration,
    max: std::time::Duration,
    next: std::time::Duration,
}

impl Backoff {
    pub fn new(initial: std::time::Duration, max: std::time::Duration) -> Self {
        Self { initial, max, next: initial }
    }

    /// The delay to wait now; the following call returns twice as much.
    pub fn next_delay(&mut self) -> std::time::Duration {
        let d = self.next;
        self.next = (self.next * 2).min(self.max);
        d
    }

    pub fn reset(&mut self) {
        self.next = self.initial;
    }
}

/// Capture-related requests from the booth to the camera thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CameraCommand {
    /// Switch to high-quality noise reduction; auto-exposure keeps metering.
    PrepareCapture,
    /// Lock exposure and white balance, and deliver the next complete frame
    /// as capture `id` (full resolution, never mirrored).
    Capture { id: u64 },
    /// Back to preview settings: auto exposure/white balance, fast noise reduction.
    ReleaseCapture,
}

/// Exposure settings the frame was taken with (for EXIF).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct CaptureMeta {
    pub exposure_us: Option<i32>,
    pub gain: Option<f32>,
}

#[derive(Debug)]
pub struct CapturedFrame {
    pub id: u64,
    pub frame: Frame,
    pub meta: CaptureMeta,
}

/// Where a running camera sends its output.
pub struct CameraLink {
    /// Latest preview frame (overwritten, never blocks the camera).
    pub mailbox: Arc<FrameMailbox>,
    /// Captured frames, one per `Capture` command.
    pub captures: Sender<CapturedFrame>,
}

/// A camera runs its own thread. It is `Send` so it can be started and
/// stopped off the render thread.
pub trait Camera: Send {
    /// Start streaming. Returns once the camera thread is running; errors
    /// that happen during setup are returned here.
    fn start(&mut self, link: CameraLink) -> anyhow::Result<()>;
    /// Queue a capture command; ignored if the camera is not running.
    fn command(&mut self, cmd: CameraCommand);
    /// Stop streaming and join the camera thread. Idempotent.
    fn stop(&mut self);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn backoff_doubles_to_max_and_resets() {
        let mut b = Backoff::new(Duration::from_secs(1), Duration::from_secs(30));
        let got: Vec<u64> = (0..7).map(|_| b.next_delay().as_secs()).collect();
        assert_eq!(got, vec![1, 2, 4, 8, 16, 30, 30]);
        b.reset();
        assert_eq!(b.next_delay(), Duration::from_secs(1));
    }
}
```

`v2/src/camera/fake.rs`:
```rust
//! Animated test pattern at the sensor's size and rate, for development
//! without a Pi and for CI smoke tests.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::{Camera, CameraCommand, CameraLink, CaptureMeta, CapturedFrame};
use crate::frame::Frame;

pub struct FakeCamera {
    size: (u32, u32),
    fps: u32,
    running: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    commands: Option<Sender<CameraCommand>>,
}

impl FakeCamera {
    pub fn new(size: (u32, u32), fps: u32) -> Self {
        Self { size, fps, running: Arc::new(AtomicBool::new(false)), thread: None, commands: None }
    }
}

/// Grey horizontal gradient, a white vertical bar moving 8 px per frame,
/// and a red square in the top-left corner so mirroring is visible.
pub fn fill_test_pattern(buf: &mut [u8], width: u32, height: u32, seq: u64) {
    let (w, h) = (width as usize, height as usize);
    assert_eq!(buf.len(), w * h * 3);
    let stride = w * 3;
    let denom = (w - 1).max(1);
    // Build one gradient row, then copy it down (fast even in debug builds).
    for x in 0..w {
        let g = (x * 255 / denom) as u8;
        buf[x * 3..x * 3 + 3].copy_from_slice(&[g, g, g]);
    }
    let (first, rest) = buf.split_at_mut(stride);
    for row in rest.chunks_exact_mut(stride) {
        row.copy_from_slice(first);
    }
    let bar_x = (seq as usize * 8) % w;
    let marker = (h / 8).max(1).min(w);
    for (y, row) in buf.chunks_exact_mut(stride).enumerate() {
        for x in bar_x.saturating_sub(3)..(bar_x + 4).min(w) {
            row[x * 3..x * 3 + 3].copy_from_slice(&[255, 255, 255]);
        }
        if y < marker {
            for px in row[..marker * 3].chunks_exact_mut(3) {
                px.copy_from_slice(&[255, 0, 0]);
            }
        }
    }
}

impl Camera for FakeCamera {
    fn start(&mut self, link: CameraLink) -> anyhow::Result<()> {
        if self.thread.is_some() {
            return Ok(());
        }
        self.running.store(true, Ordering::SeqCst);
        let running = self.running.clone();
        let (w, h) = self.size;
        let period = Duration::from_secs_f64(1.0 / self.fps as f64);
        let (cmd_tx, cmd_rx) = channel();
        self.commands = Some(cmd_tx);
        self.thread = Some(std::thread::Builder::new().name("fake-camera".into()).spawn(move || {
            let mut next = Instant::now();
            let mut seq = 0u64;
            let mut pending = None;
            while running.load(Ordering::SeqCst) {
                for cmd in cmd_rx.try_iter() {
                    if let CameraCommand::Capture { id } = cmd {
                        pending = Some(id);
                    }
                }
                let mut data = link.mailbox.take_buffer(Frame::byte_len(w, h));
                fill_test_pattern(&mut data, w, h, seq);
                if let Some(id) = pending.take() {
                    let frame = Frame { width: w, height: h, data: data.clone(), seq, captured_at: Instant::now() };
                    let meta = CaptureMeta { exposure_us: Some(10_000), gain: Some(1.0) };
                    let _ = link.captures.send(CapturedFrame { id, frame, meta });
                }
                link.mailbox.publish(Frame { width: w, height: h, data, seq, captured_at: Instant::now() });
                seq += 1;
                next += period;
                let now = Instant::now();
                if next > now {
                    std::thread::sleep(next - now);
                } else {
                    next = now; // fell behind; don't try to catch up
                }
            }
        })?);
        Ok(())
    }

    fn command(&mut self, cmd: CameraCommand) {
        if let Some(tx) = &self.commands {
            let _ = tx.send(cmd);
        }
    }

    fn stop(&mut self) {
        self.running.store(false, Ordering::SeqCst);
        self.commands = None;
        if let Some(t) = self.thread.take()
            && t.join().is_err()
        {
            tracing::error!("camera thread panicked");
        }
    }
}

impl Drop for FakeCamera {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::FrameMailbox;

    #[test]
    fn pattern_has_marker_and_bar() {
        let (w, h) = (64u32, 32u32);
        let mut buf = vec![0; Frame::byte_len(w, h)];
        fill_test_pattern(&mut buf, w, h, 2); // bar at x=16
        assert_eq!(&buf[0..3], &[255, 0, 0], "top-left marker");
        let px = |x: usize, y: usize| &buf[(y * w as usize + x) * 3..][..3];
        assert_eq!(px(16, 20), &[255, 255, 255], "bar");
        assert_eq!(px(63, 0)[0], 255, "gradient right edge");
        assert_eq!(px(40, 20)[0], px(40, 20)[1], "grey");
    }

    fn link() -> (CameraLink, Arc<FrameMailbox>, std::sync::mpsc::Receiver<CapturedFrame>) {
        let mb = FrameMailbox::new();
        let (tx, rx) = channel();
        (CameraLink { mailbox: mb.clone(), captures: tx }, mb, rx)
    }

    #[test]
    fn streams_at_roughly_requested_rate() {
        let (link, mb, _rx) = link();
        let mut cam = FakeCamera::new((64, 32), 100);
        cam.start(link).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        cam.stop();
        let (published, _) = mb.counters();
        assert!((15..=40).contains(&published), "published {published}");
        let f = mb.take_latest().unwrap();
        assert_eq!((f.width, f.height, f.data.len()), (64, 32, 64 * 32 * 3));
    }

    #[test]
    fn stop_is_idempotent() {
        let mut cam = FakeCamera::new((8, 8), 30);
        cam.stop();
        cam.start(link().0).unwrap();
        cam.stop();
        cam.stop();
        cam.command(CameraCommand::Capture { id: 1 }); // ignored when stopped
    }

    #[test]
    fn capture_delivers_one_full_frame_with_its_id() {
        let (link, _mb, rx) = link();
        let mut cam = FakeCamera::new((64, 32), 100);
        cam.start(link).unwrap();
        cam.command(CameraCommand::PrepareCapture);
        cam.command(CameraCommand::Capture { id: 42 });
        let got = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(got.id, 42);
        assert_eq!(got.frame.data.len(), 64 * 32 * 3);
        assert_eq!(&got.frame.data[0..3], &[255, 0, 0], "not mirrored: marker top-left");
        assert!(got.meta.exposure_us.is_some());
        std::thread::sleep(Duration::from_millis(50));
        assert!(rx.try_recv().is_err(), "exactly one frame per capture");
        cam.stop();
    }
}
```

- [ ] **Step 4: Pi camera capture controls**

`v2/src/camera/pi.rs`:
```rust
//! Raspberry Pi camera via libcamera (Pi 5 PiSP pipeline, IMX296).
//!
//! libcamera objects borrow the `CameraManager` and must stay on one thread,
//! so the whole session lives on a dedicated "pi-camera" thread. `start`
//! waits for that thread to report that streaming began (or why it failed).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow, bail};
use libcamera::camera::CameraConfigurationStatus;
use libcamera::camera_manager::CameraManager;
use libcamera::control::ControlList;
use libcamera::controls::{
    AnalogueGain, AnalogueGainMode, AwbEnable, ExposureTime, ExposureTimeMode, FrameDuration, FrameDurationLimits,
    NoiseReductionMode, SensorTimestamp,
};
use libcamera::framebuffer::AsFrameBuffer;
use libcamera::framebuffer_allocator::{FrameBuffer, FrameBufferAllocator};
use libcamera::framebuffer_map::MemoryMappedFrameBuffer;
use libcamera::geometry::Size;
use libcamera::pixel_format::PixelFormat;
use libcamera::request::{RequestStatus, ReuseFlag};
use libcamera::stream::StreamRole;

use super::{Camera, CameraCommand, CameraLink, CaptureMeta, CapturedFrame, SENSOR_FPS, SENSOR_SIZE};
use crate::config::CameraCfg;
use crate::frame::{Frame, FrameMailbox, copy_packed};
use crate::stats::{FrameSample, FrameTiming};

/// libcamera `BGR888` = DRM `BG24`: bytes in memory are R, G, B, which is
/// what GL `RGB` expects. (libcamera `RGB888` is B, G, R in memory.)
const PREVIEW_FORMAT: PixelFormat = PixelFormat::new(u32::from_le_bytes(*b"BG24"), 0);
const BUFFER_COUNT: u32 = 4;

pub struct PiCamera {
    cfg: CameraCfg,
    running: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    commands: Option<mpsc::Sender<CameraCommand>>,
}

impl PiCamera {
    pub fn new(cfg: CameraCfg) -> Self {
        Self { cfg, running: Arc::new(AtomicBool::new(false)), thread: None, commands: None }
    }
}

impl Camera for PiCamera {
    fn start(&mut self, link: CameraLink) -> anyhow::Result<()> {
        if self.thread.is_some() {
            return Ok(());
        }
        if !self.cfg.tuning_file.is_empty() {
            // Read by the Raspberry Pi IPA when the camera is configured.
            // SAFETY: called before any other thread reads the environment.
            unsafe { std::env::set_var("LIBCAMERA_RPI_TUNING_FILE", &self.cfg.tuning_file) };
        }
        self.running.store(true, Ordering::SeqCst);
        let running = self.running.clone();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
        let (cmd_tx, cmd_rx) = mpsc::channel();
        self.commands = Some(cmd_tx);
        let thread = std::thread::Builder::new().name("pi-camera".into()).spawn(move || {
            let mut guard = CloseGuard { mailbox: link.mailbox.clone(), running: running.clone(), reason: None };
            if let Err(e) = session(&link, &cmd_rx, &running, &ready_tx) {
                let msg = format!("{e:#}");
                // If setup failed, `start` is still waiting for this message.
                let _ = ready_tx.send(Err(msg.clone()));
                tracing::error!("camera session ended: {msg}");
                guard.reason = Some(msg);
            }
        })?;
        self.thread = Some(thread);
        match ready_rx.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => {
                self.stop();
                Err(anyhow!(e))
            }
            Err(_) => {
                self.stop();
                bail!("camera did not start within 10 s")
            }
        }
    }

    fn command(&mut self, cmd: CameraCommand) {
        if let Some(tx) = &self.commands {
            let _ = tx.send(cmd);
        }
    }

    fn stop(&mut self) {
        self.running.store(false, Ordering::SeqCst);
        self.commands = None;
        if let Some(t) = self.thread.take()
            && t.join().is_err()
        {
            tracing::error!("camera thread panicked");
        }
    }
}

/// Closes the mailbox when the camera thread ends while it was still meant
/// to run (session error or panic), so the render loop can fail loudly.
struct CloseGuard {
    mailbox: Arc<FrameMailbox>,
    running: Arc<AtomicBool>,
    reason: Option<String>,
}

impl Drop for CloseGuard {
    fn drop(&mut self) {
        if self.running.load(Ordering::SeqCst) {
            let reason = if std::thread::panicking() {
                "camera thread panicked".to_string()
            } else {
                self.reason.take().unwrap_or_else(|| "camera session ended".to_string())
            };
            self.mailbox.close(reason);
        }
    }
}

impl Drop for PiCamera {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Control changes waiting for the next re-queued request. libcamera keeps
/// controls once set, so each change is sent once.
#[derive(Debug, Default, PartialEq)]
struct PendingControls {
    noise_reduction: Option<NoiseReductionMode>,
    /// `Some(true)`: freeze exposure, gain and white balance at their current
    /// values (switching to manual keeps the last computed value).
    /// `Some(false)`: back to automatic.
    lock: Option<bool>,
}

impl PendingControls {
    fn on_command(&mut self, cmd: CameraCommand) {
        match cmd {
            CameraCommand::PrepareCapture => self.noise_reduction = Some(NoiseReductionMode::HighQuality),
            CameraCommand::Capture { .. } => self.lock = Some(true),
            CameraCommand::ReleaseCapture => {
                self.noise_reduction = Some(NoiseReductionMode::Fast);
                self.lock = Some(false);
            }
        }
    }

    fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    fn apply(&mut self, list: &mut ControlList) -> anyhow::Result<()> {
        fn err(name: &'static str) -> impl Fn(libcamera::control::ControlError) -> anyhow::Error {
            move |e| anyhow!("{name}: {e:?}")
        }
        if let Some(nr) = self.noise_reduction.take() {
            list.set(nr).map_err(err("NoiseReductionMode"))?;
        }
        if let Some(lock) = self.lock.take() {
            let (exposure, gain) = if lock {
                (ExposureTimeMode::Manual, AnalogueGainMode::Manual)
            } else {
                (ExposureTimeMode::Auto, AnalogueGainMode::Auto)
            };
            list.set(exposure).map_err(err("ExposureTimeMode"))?;
            list.set(gain).map_err(err("AnalogueGainMode"))?;
            list.set(AwbEnable(!lock)).map_err(err("AwbEnable"))?;
        }
        Ok(())
    }
}

fn session(
    link: &CameraLink,
    commands: &mpsc::Receiver<CameraCommand>,
    running: &AtomicBool,
    ready: &mpsc::Sender<Result<(), String>>,
) -> anyhow::Result<()> {
    let mailbox = &link.mailbox;
    let mgr = CameraManager::new().context("starting libcamera")?;
    let cameras = mgr.cameras();
    let cam = cameras.get(0).ok_or_else(|| anyhow!("no camera found (is the ribbon cable seated?)"))?;
    tracing::info!(id = %cam.id(), "using camera");
    let mut cam = cam.acquire().context("camera is busy (another app using it?)")?;

    let mut cfgs = cam
        .generate_configuration(&[StreamRole::ViewFinder])
        .ok_or_else(|| anyhow!("could not generate viewfinder configuration"))?;
    {
        let mut s = cfgs.get_mut(0).unwrap();
        s.set_pixel_format(PREVIEW_FORMAT);
        s.set_size(Size { width: SENSOR_SIZE.0, height: SENSOR_SIZE.1 });
        s.set_buffer_count(BUFFER_COUNT);
    }
    match cfgs.validate() {
        CameraConfigurationStatus::Valid => {}
        CameraConfigurationStatus::Adjusted => tracing::warn!("camera adjusted configuration: {cfgs:?}"),
        CameraConfigurationStatus::Invalid => bail!("camera rejected configuration: {cfgs:?}"),
    }
    let (size, stride, format) = {
        let s = cfgs.get(0).unwrap();
        (s.get_size(), s.get_stride() as usize, s.get_pixel_format())
    };
    if format != PREVIEW_FORMAT {
        bail!("camera does not support BGR888 preview (got {format:?})");
    }
    tracing::info!(width = size.width, height = size.height, stride, "preview configured");
    cam.configure(&mut cfgs).context("configuring camera")?;

    let stream = cfgs.get(0).unwrap().stream().ok_or_else(|| anyhow!("stream missing after configure"))?;
    let mut alloc = FrameBufferAllocator::new(&cam);
    let buffers = alloc.alloc(&stream).context("allocating frame buffers")?;
    let mut requests = Vec::with_capacity(buffers.len());
    for (i, buf) in buffers.into_iter().enumerate() {
        let buf = MemoryMappedFrameBuffer::new(buf).map_err(|e| anyhow!("mmap buffer: {e:?}"))?;
        let mut req = cam.create_request(Some(i as u64)).ok_or_else(|| anyhow!("create_request failed"))?;
        req.add_buffer(&stream, buf).context("attaching buffer")?;
        requests.push(req);
    }

    let (done_tx, done_rx) = mpsc::channel();
    cam.on_request_completed(move |req| {
        let _ = done_tx.send(req);
    });

    // Fixed 60 fps: min = max frame duration (µs). Caps exposure at 16.6 ms.
    let frame_us = 1_000_000 / SENSOR_FPS as i64;
    let mut controls = ControlList::new();
    controls.set(FrameDurationLimits([frame_us, frame_us])).map_err(|e| anyhow!("FrameDurationLimits: {e:?}"))?;
    cam.start(Some(&controls)).context("starting camera stream")?;
    for req in requests {
        cam.queue_request(req).map_err(|(_, e)| e).context("queueing request")?;
    }
    let _ = ready.send(Ok(()));

    tracing::info!(frame_duration_us = frame_us, "requested fixed frame duration");

    let (w, h) = (size.width, size.height);
    let row_bytes = w as usize * 3;
    let mut timing = FrameTiming::default();
    let mut last_report = Instant::now();
    let mut pending = PendingControls::default();
    let mut capture: Option<u64> = None;
    while running.load(Ordering::SeqCst) {
        for cmd in commands.try_iter() {
            tracing::debug!(?cmd, "camera command");
            pending.on_command(cmd);
            if let CameraCommand::Capture { id } = cmd {
                capture = Some(id);
            }
        }
        let mut req = match done_rx.recv_timeout(Duration::from_secs(2)) {
            Ok(r) => r,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                tracing::warn!("no camera frame for 2 s");
                continue;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => bail!("camera callback channel closed"),
        };
        if req.status() == RequestStatus::Complete {
            let fb: &MemoryMappedFrameBuffer<FrameBuffer> =
                req.buffer(&stream).ok_or_else(|| anyhow!("request without buffer"))?;
            let seq = fb.metadata().map(|m| m.sequence() as u64).unwrap_or(0);
            let copy_start = Instant::now();
            let planes = fb.data();
            let mut data = mailbox.take_buffer(Frame::byte_len(w, h));
            copy_packed(planes[0], stride, row_bytes, h as usize, &mut data);
            let copy_us = copy_start.elapsed().as_micros() as u64;
            let md = req.metadata();
            if let Some(id) = capture.take() {
                // The next complete frame after the command: high-quality noise
                // reduction has been on since PrepareCapture (~300 ms earlier).
                let meta = CaptureMeta {
                    exposure_us: md.get::<ExposureTime>().ok().map(|v| v.0),
                    gain: md.get::<AnalogueGain>().ok().map(|v| v.0),
                };
                tracing::info!(id, seq, exposure_us = meta.exposure_us, gain = meta.gain, "frame captured");
                let frame = Frame { width: w, height: h, data: data.clone(), seq, captured_at: Instant::now() };
                let _ = link.captures.send(CapturedFrame { id, frame, meta });
            }
            mailbox.publish(Frame { width: w, height: h, data, seq, captured_at: Instant::now() });
            timing.record(FrameSample {
                seq,
                sensor_ts_ns: md.get::<SensorTimestamp>().ok().map(|v| v.0),
                frame_duration_us: md.get::<FrameDuration>().ok().map(|v| v.0),
                exposure_us: md.get::<ExposureTime>().ok().map(|v| v.0),
                gain: md.get::<AnalogueGain>().ok().map(|v| v.0),
                copy_us,
            });
        } else {
            tracing::debug!(status = ?req.status(), "request not complete");
        }
        if last_report.elapsed() >= Duration::from_secs(1) {
            last_report = Instant::now();
            let t = timing.take_summary();
            tracing::info!(
                frames = t.frames,
                dropped = t.dropped,
                sensor_fps = t.sensor_fps.map(|f| (f * 10.0).round() / 10.0),
                frame_duration_us = t.frame_duration_us.map(|d| d.round()),
                exposure_us_max = t.exposure_us_max,
                gain_mean = t.gain_mean.map(|g| (g * 100.0).round() / 100.0),
                copy_us_max = t.copy_us_max,
                "camera"
            );
        }
        req.reuse(ReuseFlag::REUSE_BUFFERS);
        if !pending.is_empty() {
            pending.apply(req.controls_mut())?;
        }
        cam.queue_request(req).map_err(|(_, e)| e).context("re-queueing request")?;
    }
    cam.stop().context("stopping camera")?;
    Ok(())
}
```

- [ ] **Step 5: Disk re-check after a save**

`v2/src/monitor.rs`:
```rust
//! Background health checks (spec §4.7): CPU temperature, disk usage and
//! Immich server reachability. They run on their own threads and only
//! publish a [`Status`] snapshot, so a slow check can never stall a frame.

use std::net::{TcpStream, ToSocketAddrs};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::config::Config;

const TEMP_PATH: &str = "/sys/class/thermal/thermal_zone0/temp";
const TEMP_INTERVAL: Duration = Duration::from_secs(1);
const DISK_INTERVAL: Duration = Duration::from_secs(5 * 60);
const NET_INTERVAL_ONLINE: Duration = Duration::from_secs(60);
const NET_INTERVAL_OFFLINE: Duration = Duration::from_secs(10);
const NET_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Level {
    #[default]
    Ok,
    Warning,
    Critical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Network {
    /// No Immich server configured: nothing to check, no icon.
    #[default]
    Disabled,
    Unknown,
    Online,
    Offline,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Status {
    pub temp_c: Option<f32>,
    pub temp: Level,
    pub disk_pct: Option<f32>,
    pub disk: Level,
    pub network: Network,
}

/// v1 thresholds semantics: above `warn` is a warning, `crit` and above is critical.
pub fn classify(value: f32, warn: f32, crit: f32) -> Level {
    if value >= crit {
        Level::Critical
    } else if value > warn {
        Level::Warning
    } else {
        Level::Ok
    }
}

/// Parse the kernel's thermal zone file (millidegrees Celsius).
pub fn parse_millidegrees(text: &str) -> Option<f32> {
    text.trim().parse::<i64>().ok().map(|m| m as f32 / 1000.0)
}

/// Used space as a percentage, computed like v1 (`(blocks - bfree) / blocks`).
pub fn disk_used_pct(path: &Path) -> std::io::Result<f32> {
    let st = nix::sys::statvfs::statvfs(path).map_err(std::io::Error::from)?;
    let blocks = st.blocks() as f64;
    if blocks == 0.0 {
        return Ok(0.0);
    }
    Ok(((blocks - st.blocks_free() as f64) * 100.0 / blocks) as f32)
}

/// Host and port to probe for an Immich base URL such as
/// `https://immich.example/api` → `("immich.example", 443)`.
pub fn server_addr(base_url: &str) -> Option<(String, u16)> {
    let (scheme, rest) = base_url.trim().split_once("://")?;
    let default_port = match scheme.to_ascii_lowercase().as_str() {
        "https" => 443,
        "http" => 80,
        _ => return None,
    };
    let authority = rest.split(['/', '?', '#']).next()?;
    let authority = authority.rsplit('@').next()?; // drop userinfo
    if authority.is_empty() {
        return None;
    }
    if let Some(v6) = authority.strip_prefix('[') {
        let (host, after) = v6.split_once(']')?;
        let port = match after.strip_prefix(':') {
            Some(p) => p.parse().ok()?,
            None if after.is_empty() => default_port,
            None => return None,
        };
        return Some((host.to_string(), port));
    }
    match authority.rsplit_once(':') {
        Some((host, port)) => Some((host.to_string(), port.parse().ok()?)),
        None => Some((authority.to_string(), default_port)),
    }
}

/// TCP reachability of `host:port` within `timeout`.
pub fn reachable(host: &str, port: u16, timeout: Duration) -> bool {
    let Ok(addrs) = (host, port).to_socket_addrs() else { return false };
    addrs.into_iter().any(|a| TcpStream::connect_timeout(&a, timeout).is_ok())
}

pub struct Monitors {
    status: Arc<Mutex<Status>>,
    running: Arc<AtomicBool>,
    /// Set to re-check disk usage on the next pass (after a photo is saved).
    disk_now: Arc<AtomicBool>,
    /// The local-checks thread; joined on drop (it wakes every 100 ms).
    local: Option<JoinHandle<()>>,
}

impl Monitors {
    pub fn start(cfg: &Config) -> anyhow::Result<Self> {
        let status = Arc::new(Mutex::new(Status::default()));
        let running = Arc::new(AtomicBool::new(true));
        let disk_now = Arc::new(AtomicBool::new(false));
        let (st, run, mon, root) = (status.clone(), running.clone(), cfg.monitor.clone(), cfg.storage.root.clone());
        let dn = disk_now.clone();
        let local = std::thread::Builder::new().name("monitor".into()).spawn(move || {
            local_loop(&st, &run, &dn, &mon, &root);
        })?;

        if let Some((host, port)) = server_addr(&cfg.immich.base_url) {
            status.lock().unwrap().network = Network::Unknown;
            let (st, run) = (status.clone(), running.clone());
            // Detached: a connect can block for NET_TIMEOUT, which must not delay quitting.
            std::thread::Builder::new().name("net-monitor".into()).spawn(move || {
                net_loop(&st, &run, &host, port);
            })?;
        } else if !cfg.immich.base_url.is_empty() {
            tracing::warn!(url = %cfg.immich.base_url, "cannot parse immich.base_url; network check disabled");
        }
        Ok(Self { status, running, disk_now, local: Some(local) })
    }

    pub fn snapshot(&self) -> Status {
        *self.status.lock().unwrap()
    }

    /// Re-check disk usage within about a second (spec §4.7: after each save).
    pub fn recheck_disk(&self) {
        self.disk_now.store(true, Ordering::SeqCst);
    }
}

impl Drop for Monitors {
    fn drop(&mut self) {
        self.running.store(false, Ordering::SeqCst);
        if let Some(t) = self.local.take() {
            let _ = t.join();
        }
    }
}

/// Sleep up to `d`, waking early when `running` clears. Returns false to stop.
fn nap(running: &AtomicBool, d: Duration) -> bool {
    let end = Instant::now() + d;
    while running.load(Ordering::SeqCst) {
        let now = Instant::now();
        if now >= end {
            return true;
        }
        std::thread::sleep((end - now).min(Duration::from_millis(100)));
    }
    false
}

fn local_loop(
    status: &Mutex<Status>,
    running: &AtomicBool,
    disk_now: &AtomicBool,
    mon: &crate::config::Monitor,
    root: &Path,
) {
    let mut next_disk = Instant::now();
    loop {
        let temp_c = std::fs::read_to_string(TEMP_PATH).ok().and_then(|t| parse_millidegrees(&t));
        let disk = if disk_now.swap(false, Ordering::SeqCst) || Instant::now() >= next_disk {
            next_disk = Instant::now() + DISK_INTERVAL;
            match disk_used_pct(root) {
                Ok(p) => Some(p),
                Err(e) => {
                    tracing::warn!("disk usage of {}: {e}", root.display());
                    None
                }
            }
        } else {
            None
        };
        {
            let mut s = status.lock().unwrap();
            s.temp_c = temp_c;
            s.temp = temp_c.map_or(Level::Ok, |t| classify(t, mon.temp_warn_c, mon.temp_crit_c));
            if let Some(p) = disk {
                s.disk_pct = Some(p);
                s.disk = classify(p, mon.disk_warn_pct, mon.disk_crit_pct);
            }
        }
        if !nap(running, TEMP_INTERVAL) {
            return;
        }
    }
}

fn net_loop(status: &Mutex<Status>, running: &AtomicBool, host: &str, port: u16) {
    loop {
        let online = reachable(host, port, NET_TIMEOUT);
        let previous = std::mem::replace(
            &mut status.lock().unwrap().network,
            if online { Network::Online } else { Network::Offline },
        );
        if previous != Network::Online && online {
            tracing::info!(host, port, "Immich server reachable");
        } else if previous != Network::Offline && !online {
            tracing::warn!(host, port, "Immich server unreachable");
        }
        if !nap(running, if online { NET_INTERVAL_ONLINE } else { NET_INTERVAL_OFFLINE }) {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_matches_v1_thresholds() {
        assert_eq!(classify(55.0, 60.0, 65.0), Level::Ok);
        assert_eq!(classify(60.0, 60.0, 65.0), Level::Ok, "warning is strictly above");
        assert_eq!(classify(60.1, 60.0, 65.0), Level::Warning);
        assert_eq!(classify(65.0, 60.0, 65.0), Level::Critical);
        assert_eq!(classify(91.0, 70.0, 90.0), Level::Critical);
    }

    #[test]
    fn parses_thermal_zone() {
        assert_eq!(parse_millidegrees("48312\n"), Some(48.312));
        assert_eq!(parse_millidegrees("garbage"), None);
    }

    #[test]
    fn disk_usage_of_current_dir_is_a_percentage() {
        let p = disk_used_pct(Path::new(".")).unwrap();
        assert!((0.0..=100.0).contains(&p), "{p}");
        assert!(disk_used_pct(Path::new("/nonexistent/dir")).is_err());
    }

    #[test]
    fn server_addr_from_urls() {
        assert_eq!(server_addr("https://immich.example/api"), Some(("immich.example".into(), 443)));
        assert_eq!(server_addr("http://192.168.1.10:2283/api"), Some(("192.168.1.10".into(), 2283)));
        assert_eq!(server_addr("HTTPS://user:pw@host.lan"), Some(("host.lan".into(), 443)));
        assert_eq!(server_addr("https://[fd00::1]:8443/api"), Some(("fd00::1".into(), 8443)));
        assert_eq!(server_addr("https://[fd00::1]/api"), Some(("fd00::1".into(), 443)));
        assert_eq!(server_addr(""), None);
        assert_eq!(server_addr("immich.example/api"), None);
        assert_eq!(server_addr("ftp://x"), None);
        assert_eq!(server_addr("https://host:notaport"), None);
    }

    #[test]
    fn reachability_against_local_listener() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(reachable("127.0.0.1", port, Duration::from_secs(1)));
        drop(listener);
        assert!(!reachable("127.0.0.1", port, Duration::from_millis(500)));
    }

    #[test]
    fn monitors_publish_disk_and_disabled_network() {
        let cfg = Config::default();
        let m = Monitors::start(&cfg).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while m.snapshot().disk_pct.is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        let s = m.snapshot();
        assert!(s.disk_pct.is_some());
        assert_eq!(s.network, Network::Disabled);
    }

    #[test]
    fn recheck_disk_refreshes_within_a_second() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = Config::default();
        cfg.storage.root = dir.path().to_path_buf();
        let m = Monitors::start(&cfg).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while m.snapshot().disk_pct.is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        m.status.lock().unwrap().disk_pct = None; // forget the first reading
        m.recheck_disk();
        let deadline = Instant::now() + Duration::from_millis(1500);
        while m.snapshot().disk_pct.is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(m.snapshot().disk_pct.is_some(), "re-checked before the 5 min interval");
    }
}
```

- [ ] **Step 6: Controller, main, scene test constant**

`v2/src/display/controller.rs`:
```rust
//! Runs the booth: feeds input and time into the state machine, carries
//! out its commands (camera capture, photo processing), supervises the
//! camera (restart with backoff on a background thread, spec §5) and
//! gathers what the screen should show. Owns no GL state.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::camera::{Backoff, Camera, CameraCommand, CameraLink, CapturedFrame};
use crate::config::{Config, FlashMode};
use crate::frame::{Frame, FrameMailbox};
use crate::input::{Action, GpioButton};
use crate::monitor::{Monitors, Status};
use crate::osd::Rgba;
use crate::osd::scene::SceneInput;
use crate::pipeline::exif::PhotoInfo;
use crate::pipeline::filters::load_effects;
use crate::pipeline::photo::PhotoJob;
use crate::pipeline::worker::Worker;
use crate::state::{Booth, Command, Event, Phase, Timings};
use crate::storage::Storage;

/// Builds a fresh, not yet started camera. Called on a background thread
/// when the camera is restarted.
pub type CameraFactory = Arc<dyn Fn() -> Box<dyn Camera> + Send + Sync>;

const CAPTURE_TIMEOUT: Duration = Duration::from_secs(5);
const PROCESSING_TIMEOUT: Duration = Duration::from_secs(10);

/// Camera supervision timings (spec §5). Tests shorten them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Supervision {
    /// No new preview frame for this long counts as a lost camera.
    pub stall: Duration,
    /// A restarted camera must stream this long before the backoff resets.
    pub healthy_after: Duration,
    pub backoff_initial: Duration,
    pub backoff_max: Duration,
}

impl Default for Supervision {
    fn default() -> Self {
        Self {
            stall: Duration::from_secs(3),
            healthy_after: Duration::from_secs(5),
            backoff_initial: Duration::from_secs(1),
            backoff_max: Duration::from_secs(30),
        }
    }
}

/// State-machine timings from the configuration.
pub fn timings_from(cfg: &Config) -> Timings {
    Timings {
        countdown: Duration::from_secs(cfg.timing.countdown_s as u64),
        review: Duration::from_secs(cfg.timing.review_s as u64),
        idle: Duration::from_secs(cfg.timing.idle_s as u64),
        prepare_lead: Duration::from_millis(cfg.flash.lead_ms as u64),
        screen_flash: cfg.flash.mode == FlashMode::Screen,
        flash_max: Duration::from_millis(cfg.flash.max_ms as u64),
        capture_timeout: CAPTURE_TIMEOUT,
        processing_timeout: PROCESSING_TIMEOUT,
    }
}

/// A started camera and the channels it feeds.
struct Running {
    camera: Box<dyn Camera>,
    mailbox: Arc<FrameMailbox>,
    captures: Receiver<CapturedFrame>,
}

fn start_camera(factory: &CameraFactory) -> anyhow::Result<Running> {
    let mailbox = FrameMailbox::new();
    let (tx, captures) = channel();
    let mut camera = factory();
    camera.start(CameraLink { mailbox: mailbox.clone(), captures: tx })?;
    Ok(Running { camera, mailbox, captures })
}

/// Photo settings copied from the configuration.
struct PhotoSettings {
    jpeg_quality: u8,
    save_original: bool,
    gps: Option<(f64, f64)>,
}

pub struct Controller {
    booth: Booth,
    factory: CameraFactory,
    cam: Option<Running>,
    /// Background stop of a lost camera; the next start waits for it
    /// (libcamera allows one camera manager at a time).
    stopping: Option<JoinHandle<()>>,
    /// Pending background restart.
    restarting: Option<Receiver<anyhow::Result<Running>>>,
    sup: Supervision,
    backoff: Backoff,
    restart_at: Option<Instant>,
    last_published: u64,
    last_progress: Instant,
    healthy_since: Option<Instant>,
    monitors: Monitors,
    _button: Option<GpioButton>,
    button_rx: Receiver<Action>,
    worker: Worker,
    photo: PhotoSettings,
    review_image: Option<Rgba>,
    photo_count: usize,
    pub debug: bool,
    pub filter: bool,
}

impl Controller {
    /// Start the camera, storage, photo worker and monitors. A camera that
    /// fails here is a startup error (spec §5): the process exits and systemd
    /// retries.
    pub fn start(
        cfg: &Config,
        factory: CameraFactory,
        button: Option<GpioButton>,
        button_rx: Receiver<Action>,
        now: Instant,
    ) -> anyhow::Result<Self> {
        Self::start_with(cfg, factory, button, button_rx, Supervision::default(), now)
    }

    pub fn start_with(
        cfg: &Config,
        factory: CameraFactory,
        button: Option<GpioButton>,
        button_rx: Receiver<Action>,
        sup: Supervision,
        now: Instant,
    ) -> anyhow::Result<Self> {
        if cfg.flash.mode == FlashMode::Strobe {
            tracing::warn!("flash.mode = \"strobe\" arrives in milestone 5; running without flash");
        }
        let storage = Storage::create(&cfg.storage.root)?;
        let photo_count = storage.photo_count();
        let effects_dir = cfg.display.assets_dir.join("effects");
        let effects = match load_effects(&effects_dir, crate::camera::SENSOR_SIZE) {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!("no effect overlays: {e:#}");
                Vec::new()
            }
        };
        tracing::info!(photos = photo_count, effects = effects.len(), "storage ready");
        let cam = start_camera(&factory)?;
        Ok(Self {
            booth: Booth::new(timings_from(cfg), now),
            factory,
            cam: Some(cam),
            stopping: None,
            restarting: None,
            sup,
            backoff: Backoff::new(sup.backoff_initial, sup.backoff_max),
            restart_at: None,
            last_published: 0,
            last_progress: now,
            healthy_since: None,
            monitors: Monitors::start(cfg)?,
            _button: button,
            button_rx,
            worker: Worker::start(storage, effects)?,
            photo: PhotoSettings {
                jpeg_quality: cfg.capture.jpeg_quality,
                save_original: cfg.storage.save_original,
                gps: cfg.immich.gps(),
            },
            review_image: None,
            photo_count,
            debug: false,
            filter: cfg.filter.enabled,
        })
    }

    /// Handle a user action. Returns actions the controller does not own
    /// (`Quit`, `ToggleMirror`) for the caller.
    pub fn on_action(&mut self, action: Action, now: Instant) -> Option<Action> {
        match action {
            Action::Press if self.cam.is_none() => {
                tracing::debug!("press ignored: camera is down");
                None
            }
            Action::Press => {
                let cmds = self.booth.handle(Event::Press, now);
                self.apply(cmds);
                None
            }
            Action::ToggleDebug => {
                self.debug = !self.debug;
                None
            }
            Action::ToggleFilter => {
                self.filter = !self.filter;
                tracing::info!(filter = self.filter, "filter toggled");
                None
            }
            Action::Quit | Action::ToggleMirror => Some(action),
        }
    }

    /// Per-frame housekeeping: button presses, camera health, captured
    /// frames, finished photos and timed transitions. Never blocks.
    pub fn update(&mut self, now: Instant) {
        // An empty or disconnected (no button) channel both end the drain.
        while let Ok(a) = self.button_rx.try_recv() {
            let _ = self.on_action(a, now);
        }
        self.supervise_camera(now);
        self.collect_captures(now);
        self.collect_photos(now);
        let cmds = self.booth.tick(now);
        self.apply(cmds);
    }

    /// The newest camera frame, if one arrived since the last call.
    pub fn take_frame(&self) -> Option<Frame> {
        self.cam.as_ref().and_then(|c| c.mailbox.take_latest())
    }

    pub fn recycle(&self, frame: Frame) {
        if let Some(c) = &self.cam {
            c.mailbox.recycle(frame);
        }
    }

    /// (frames published, frames overwritten) of the current camera.
    pub fn camera_counters(&self) -> (u64, u64) {
        self.cam.as_ref().map_or((0, 0), |c| c.mailbox.counters())
    }

    pub fn status(&self) -> Status {
        self.monitors.snapshot()
    }

    pub fn photo_count(&self) -> usize {
        self.photo_count
    }

    /// The processed photo to show in review, once per photo.
    pub fn take_review_image(&mut self) -> Option<Rgba> {
        self.review_image.take()
    }

    pub fn scene_input(&self, now: Instant, screen: (u32, u32)) -> SceneInput {
        SceneInput {
            screen,
            view: self.booth.view(now),
            status: self.status(),
            debug: self.debug,
            banner: self.cam.is_none(),
        }
    }

    fn apply(&mut self, cmds: Vec<Command>) {
        for cmd in cmds {
            let camera_cmd = match cmd {
                // The screen flash is drawn from `View::flash`.
                Command::FlashOn | Command::FlashOff => continue,
                Command::PrepareCapture => CameraCommand::PrepareCapture,
                Command::Capture { id } => CameraCommand::Capture { id },
                Command::ReleaseCapture => CameraCommand::ReleaseCapture,
            };
            if let Some(c) = self.cam.as_mut() {
                c.camera.command(camera_cmd);
            }
        }
    }

    fn collect_captures(&mut self, now: Instant) {
        let Some(c) = &self.cam else { return };
        let frames: Vec<CapturedFrame> = c.captures.try_iter().collect();
        for f in frames {
            let id = f.id;
            let cmds = self.booth.handle(Event::FrameCaptured { id }, now);
            if !matches!(self.booth.phase(), Phase::Processing { id: p, .. } if p == id) {
                tracing::warn!(id, "late captured frame ignored");
                continue;
            }
            self.apply(cmds);
            self.worker.submit(PhotoJob {
                id,
                width: f.frame.width,
                height: f.frame.height,
                rgb: f.frame.data,
                info: PhotoInfo {
                    taken: chrono::Local::now().naive_local(),
                    exposure_us: f.meta.exposure_us,
                    gain: f.meta.gain,
                    gps: self.photo.gps,
                },
                filter: self.filter,
                jpeg_quality: self.photo.jpeg_quality,
                save_original: self.photo.save_original,
            });
        }
    }

    fn collect_photos(&mut self, now: Instant) {
        while let Some(result) = self.worker.try_recv() {
            let event = match result.outcome {
                Ok(done) => {
                    self.photo_count += 1;
                    self.monitors.recheck_disk();
                    self.review_image = Some(done.review);
                    Event::PhotoReady { id: result.id }
                }
                Err(_) => Event::PhotoFailed { id: result.id },
            };
            let cmds = self.booth.handle(event, now);
            self.apply(cmds);
        }
    }

    fn supervise_camera(&mut self, now: Instant) {
        if let Some(c) = &self.cam {
            let published = c.mailbox.counters().0;
            if published != self.last_published {
                self.last_published = published;
                self.last_progress = now;
            }
            let reason = c.mailbox.closed_reason().or_else(|| {
                (now.duration_since(self.last_progress) >= self.sup.stall)
                    .then(|| format!("no camera frames for {:?}", self.sup.stall))
            });
            if let Some(reason) = reason {
                tracing::error!("camera stopped: {reason}");
                let mut lost = self.cam.take().expect("checked above");
                // Stopping joins the camera thread; do it off the render thread.
                self.stopping = std::thread::Builder::new()
                    .name("camera-stop".into())
                    .spawn(move || lost.camera.stop())
                    .map_err(|e| tracing::error!("cannot spawn camera stop: {e}"))
                    .ok();
                self.healthy_since = None;
                let cmds = self.booth.handle(Event::CameraLost, now);
                self.apply(cmds);
                self.restart_at = Some(now + self.backoff.next_delay());
            } else if self.healthy_since.is_some_and(|t| now.duration_since(t) >= self.sup.healthy_after)
                && self.last_published > 0
            {
                self.backoff.reset();
                self.healthy_since = None;
            }
        }

        if self.cam.is_none() && self.restarting.is_none() && self.restart_at.is_some_and(|t| now >= t) {
            let (tx, rx) = channel();
            let factory = self.factory.clone();
            let stopping = self.stopping.take();
            let spawned = std::thread::Builder::new().name("camera-start".into()).spawn(move || {
                if let Some(h) = stopping {
                    let _ = h.join();
                }
                let _ = tx.send(start_camera(&factory));
            });
            match spawned {
                Ok(_) => self.restarting = Some(rx),
                Err(e) => {
                    tracing::error!("cannot spawn camera restart: {e}");
                    self.restart_at = Some(now + self.backoff.next_delay());
                }
            }
        }

        if let Some(rx) = &self.restarting {
            let result = match rx.try_recv() {
                Err(TryRecvError::Empty) => return,
                Ok(r) => r,
                Err(TryRecvError::Disconnected) => Err(anyhow::anyhow!("camera restart thread died")),
            };
            self.restarting = None;
            match result {
                Ok(cam) => {
                    tracing::info!("camera restarted");
                    self.cam = Some(cam);
                    self.restart_at = None;
                    self.healthy_since = Some(now);
                    // Baseline taken when the camera is actually running.
                    self.last_published = 0;
                    self.last_progress = now;
                }
                Err(e) => {
                    let delay = self.backoff.next_delay();
                    tracing::error!("camera restart failed, retrying in {delay:?}: {e:#}");
                    self.restart_at = Some(now + delay);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::camera::fake::FakeCamera;
    use crate::state::Outcome;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;

    const MS: Duration = Duration::from_millis(1);

    /// Short supervision timings so tests run in well under a second each.
    fn fast() -> Supervision {
        Supervision { stall: 300 * MS, healthy_after: 300 * MS, backoff_initial: 50 * MS, backoff_max: 400 * MS }
    }

    fn cfg(root: &std::path::Path) -> Config {
        let mut c = Config::default();
        c.storage.root = root.to_path_buf();
        c.display.assets_dir = root.to_path_buf(); // no effects/ here: sepia only
        c.timing.countdown_s = 1;
        c.timing.review_s = 1;
        c
    }

    /// A camera that starts fine but never publishes a frame.
    struct SilentCamera;
    impl Camera for SilentCamera {
        fn start(&mut self, _link: CameraLink) -> anyhow::Result<()> {
            Ok(())
        }
        fn command(&mut self, _cmd: CameraCommand) {}
        fn stop(&mut self) {}
    }

    /// Fake cameras; `fail_after` = number of successful starts before failing.
    fn factory(starts: Arc<AtomicUsize>, fail_after: usize) -> CameraFactory {
        Arc::new(move || -> Box<dyn Camera> {
            let n = starts.fetch_add(1, Ordering::SeqCst);
            if n >= fail_after {
                struct Broken;
                impl Camera for Broken {
                    fn start(&mut self, _l: CameraLink) -> anyhow::Result<()> {
                        anyhow::bail!("sensor unplugged")
                    }
                    fn command(&mut self, _c: CameraCommand) {}
                    fn stop(&mut self) {}
                }
                Box::new(Broken)
            } else {
                Box::new(FakeCamera::new((64, 48), 100))
            }
        })
    }

    fn controller(dir: &tempfile::TempDir, f: CameraFactory) -> (Controller, mpsc::Sender<Action>) {
        let (tx, rx) = mpsc::channel();
        (Controller::start_with(&cfg(dir.path()), f, None, rx, fast(), Instant::now()).unwrap(), tx)
    }

    /// Run `update(Instant::now())` until `done` holds (real time, ≤ 5 s).
    fn run_until(c: &mut Controller, mut done: impl FnMut(&Controller) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !done(c) {
            assert!(Instant::now() < deadline, "condition not reached; phase {:?}", c.booth.phase());
            c.update(Instant::now());
            std::thread::sleep(5 * MS);
        }
    }

    #[test]
    fn timings_follow_config() {
        let mut c = Config::default();
        c.timing.countdown_s = 3;
        c.flash.lead_ms = 250;
        let t = timings_from(&c);
        assert_eq!(t.countdown, Duration::from_secs(3));
        assert_eq!(t.prepare_lead, Duration::from_millis(250));
        assert!(t.screen_flash);
        c.flash.mode = FlashMode::None;
        let t = timings_from(&c);
        assert!(!t.screen_flash);
        assert_eq!(t.prepare_lead, Duration::from_millis(250), "camera still prepares without flash");
    }

    #[test]
    fn creates_storage_folders_at_start() {
        let dir = tempfile::tempdir().unwrap();
        let (_c, _tx) = controller(&dir, factory(Arc::new(AtomicUsize::new(0)), usize::MAX));
        assert!(dir.path().join("photos").is_dir() && dir.path().join("raw").is_dir());
    }

    #[test]
    fn button_press_takes_saves_and_reviews_a_photo() {
        let dir = tempfile::tempdir().unwrap();
        let (mut c, tx) = controller(&dir, factory(Arc::new(AtomicUsize::new(0)), usize::MAX));
        tx.send(Action::Press).unwrap();
        c.update(Instant::now());
        assert!(matches!(c.booth.phase(), Phase::Countdown { .. }));
        run_until(&mut c, |c| matches!(c.booth.phase(), Phase::Review { .. }));
        assert_eq!(c.booth.view(Instant::now()).review, Some(Outcome::Photo));
        let review = c.take_review_image().expect("review image");
        assert_eq!((review.width, review.height), (64, 48));
        assert!(c.take_review_image().is_none(), "handed out once");
        assert_eq!(c.photo_count(), 1);
        let photos: Vec<_> = std::fs::read_dir(dir.path().join("photos")).unwrap().collect();
        let raws: Vec<_> = std::fs::read_dir(dir.path().join("raw")).unwrap().collect();
        assert_eq!((photos.len(), raws.len()), (1, 1));
        run_until(&mut c, |c| c.booth.phase() == Phase::Live);
    }

    #[test]
    fn toggles_and_passthrough_actions() {
        let dir = tempfile::tempdir().unwrap();
        let (mut c, _tx) = controller(&dir, factory(Arc::new(AtomicUsize::new(0)), usize::MAX));
        let now = Instant::now();
        assert_eq!(c.on_action(Action::ToggleDebug, now), None);
        assert!(c.debug);
        let f = c.filter;
        c.on_action(Action::ToggleFilter, now);
        assert_eq!(c.filter, !f);
        assert_eq!(c.on_action(Action::Quit, now), Some(Action::Quit));
        assert_eq!(c.on_action(Action::ToggleMirror, now), Some(Action::ToggleMirror));
    }

    #[test]
    fn stalled_camera_shows_banner_without_blocking() {
        let dir = tempfile::tempdir().unwrap();
        let (mut c, _tx) = controller(&dir, Arc::new(|| -> Box<dyn Camera> { Box::new(SilentCamera) }));
        let t0 = Instant::now();
        c.update(t0 + 299 * MS);
        assert!(!c.scene_input(t0, (100, 100)).banner);
        c.update(t0 + 300 * MS);
        assert!(c.scene_input(t0, (100, 100)).banner);
        assert_eq!(c.booth.phase(), Phase::Idle);
    }

    #[test]
    fn press_ignored_while_camera_down() {
        let dir = tempfile::tempdir().unwrap();
        let (mut c, _tx) = controller(&dir, factory(Arc::new(AtomicUsize::new(0)), usize::MAX));
        c.cam.as_ref().unwrap().mailbox.close("gone");
        c.update(Instant::now());
        assert!(c.cam.is_none());
        c.on_action(Action::Press, Instant::now());
        assert_eq!(c.booth.phase(), Phase::Idle);
    }

    #[test]
    fn dead_camera_restarts_in_background() {
        let starts = Arc::new(AtomicUsize::new(0));
        let dir = tempfile::tempdir().unwrap();
        let (mut c, _tx) = controller(&dir, factory(starts.clone(), usize::MAX));
        c.cam.as_ref().unwrap().mailbox.close("sensor gone");
        c.update(Instant::now());
        assert!(c.scene_input(Instant::now(), (100, 100)).banner);
        run_until(&mut c, |c| c.cam.is_some());
        assert_eq!(starts.load(Ordering::SeqCst), 2);
        assert!(!c.scene_input(Instant::now(), (100, 100)).banner);
        run_until(&mut c, |c| c.take_frame().is_some());
    }

    #[test]
    fn failed_restarts_back_off_and_healthy_camera_resets() {
        let starts = Arc::new(AtomicUsize::new(0));
        let dir = tempfile::tempdir().unwrap();
        // First start (at startup) works, the next two fail, then cameras work again.
        let n = starts.clone();
        let f: CameraFactory = Arc::new(move || -> Box<dyn Camera> {
            let i = n.fetch_add(1, Ordering::SeqCst);
            let inner = factory(Arc::new(AtomicUsize::new(0)), if i == 1 || i == 2 { 0 } else { usize::MAX });
            inner()
        });
        let (mut c, _tx) = controller(&dir, f);
        c.cam.as_ref().unwrap().mailbox.close("sensor gone");
        c.update(Instant::now());
        assert!(c.cam.is_none(), "loss detected");
        run_until(&mut c, |c| c.cam.is_some());
        assert_eq!(starts.load(Ordering::SeqCst), 4, "two failures then success");
        // Delays used so far: 50 ms (loss), 100 ms, 200 ms (two failures); next would be 400 ms.
        assert_eq!(c.backoff.clone().next_delay(), 400 * MS);
        run_until(&mut c, |c| c.healthy_since.is_none());
        assert_eq!(c.backoff.clone().next_delay(), 50 * MS, "reset after a healthy period");
    }
}
```

`v2/src/main.rs`:
```rust
use std::path::PathBuf;

use anyhow::Context;
use clap::Parser;
use std::sync::Arc;
use std::time::Instant;

use pibooth::camera::{Camera, SENSOR_FPS, SENSOR_SIZE, fake::FakeCamera};
use pibooth::config::Config;
use pibooth::display::{self, CameraFactory, Controller, RunOptions};
use pibooth::input::GpioButton;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(version, about = "Raspberry Pi 5 photo booth")]
struct Cli {
    /// Path to config.toml (defaults are used when omitted).
    #[arg(long)]
    config: Option<PathBuf>,
    /// Use the animated test pattern instead of the Pi camera.
    #[arg(long)]
    fake_camera: bool,
    /// Disable the GPIO button (keyboard only).
    #[arg(long)]
    no_gpio: bool,
    /// Log display/camera frame rates every second.
    #[arg(long)]
    bench: bool,
    /// Run in a 1280x720 window instead of fullscreen.
    #[arg(long)]
    windowed: bool,
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .init();
    let cli = Cli::parse();
    let cfg = Config::load(cli.config.as_deref()).context("loading configuration")?;
    tracing::info!(upload = cfg.upload_enabled(), "configuration loaded");

    let (tx, rx) = std::sync::mpsc::channel();
    let button = if cli.no_gpio {
        None
    } else {
        match GpioButton::start(&cfg.button, tx) {
            Ok(b) => Some(b),
            Err(e) => {
                tracing::warn!("button unavailable, keyboard only: {e:#}");
                None
            }
        }
    };
    let ctrl =
        Controller::start(&cfg, camera_factory(&cli, &cfg)?, button, rx, Instant::now()).context("starting camera")?;
    display::run(
        ctrl,
        RunOptions {
            mirror: cfg.display.mirror_preview,
            bench: cli.bench,
            windowed: cli.windowed,
            assets_dir: cfg.display.assets_dir.clone(),
            countdown_max: cfg.timing.countdown_s,
        },
    )
}

fn camera_factory(cli: &Cli, cfg: &Config) -> anyhow::Result<CameraFactory> {
    if cli.fake_camera {
        return Ok(Arc::new(|| -> Box<dyn Camera> { Box::new(FakeCamera::new(SENSOR_SIZE, SENSOR_FPS)) }));
    }
    #[cfg(feature = "libcamera")]
    {
        let cam_cfg = cfg.camera.clone();
        Ok(Arc::new(move || -> Box<dyn Camera> { Box::new(pibooth::camera::pi::PiCamera::new(cam_cfg.clone())) }))
    }
    #[cfg(not(feature = "libcamera"))]
    {
        let _ = cfg;
        anyhow::bail!("built without the `libcamera` feature; use --fake-camera or rebuild with --features libcamera")
    }
}
```

In `v2/src/osd/scene.rs` tests, change the `NONE` constant to:
```rust
    const NONE: View = View { title: false, countdown: None, flash: false, review: None };
```

- [ ] **Step 7: Verify (run the suite three times — controller tests use real threads)**

Run: `cargo fmt --check && cargo clippy --all-targets -- -D warnings && for i in 1 2 3; do cargo test || break; done`
Expected: all pass three times (≈105 tests).
Where libcamera ≥ 0.4 headers exist: `cargo clippy --all-targets --features libcamera -- -D warnings` clean.

- [ ] **Step 8: Commit**

```bash
git add Cargo.toml Cargo.lock src/state.rs src/camera src/monitor.rs src/display/controller.rs src/main.rs src/osd/scene.rs
git commit -m "feat(v2): full-quality capture flow with background camera restart"
```

### Task 6: Review display

**Files (replace whole files):** `v2/src/osd/scene.rs`, `v2/src/display/hud.rs`, `v2/src/display/app.rs`

**Interfaces:**
- Consumes: `View.review`, `Controller::take_review_image`, `Controller::photo_count`.
- Produces: `Sprites` gains `oops: TextSprite` and `photo: Option<(SpriteId, u32, u32)>`; `DebugInfo` gains `photo_count: usize` (line "Photo count: N" after "Camera fps"); `Hud::set_photo(&mut Renderer, &Rgba)`; draw order: review photo (letterboxed, unmirrored), icons, debug, title, countdown, "Oops, try again" on a dark box, camera banner, flash.

- [ ] **Step 1: Scene (test-first)**

Add the new scene tests from the file below first (`review_shows_photo_letterboxed_and_unmirrored_first`, `review_without_photo_sprite_draws_nothing`, `oops_is_centred_on_a_dark_box`, and the updated `debug_text_formats_values_and_missing`); see them fail; then replace `v2/src/osd/scene.rs` with:
```rust
//! Pure layout: given the booth view, status and pre-rendered sprites,
//! decide which overlays to draw and where (v1 positions, scaled to the
//! screen). Draw order: review photo, icons, debug text, title, countdown,
//! "oops" message, camera banner, flash.

use super::assets::Icon;
use super::{Overlay, SpriteId, WHITE};
use crate::geometry::{Rect, letterbox};
use crate::monitor::{Level, Network, Status};
use crate::state::{Outcome, View};

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
    /// "Oops, try again" (shown when a photo failed).
    pub oops: TextSprite,
    /// Indexed like [`Icon::ALL`]; all `icon_px` square.
    pub icons: Vec<SpriteId>,
    pub debug_lines: Vec<TextSprite>,
    /// The last processed photo and its size, shown during review.
    pub photo: Option<(SpriteId, u32, u32)>,
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
    pub photo_count: usize,
}

/// Debug overlay lines, top to bottom (v1's list plus camera fps).
pub fn debug_text(d: &DebugInfo) -> Vec<String> {
    let opt = |v: Option<f32>, unit: &str| v.map_or("--".to_string(), |v| format!("{v:.1}{unit}"));
    vec![
        format!("fps: {:.0}", d.display_fps),
        format!("Camera fps: {:.0}", d.camera_fps),
        format!("Photo count: {}", d.photo_count),
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

/// `t` with its top-left at (`cx - w/2`, `y`) on a translucent dark box.
fn boxed_text(sprites: &Sprites, t: &TextSprite, cx: i32, y: i32, scale: f32) -> [Overlay; 2] {
    let pad = (20.0 * scale) as i32;
    let x = cx - t.w as i32 / 2;
    [
        Overlay {
            sprite: sprites.white,
            rect: Rect { x: x - pad, y: y - pad, w: t.w + 2 * pad as u32, h: t.h + 2 * pad as u32 },
            tint: [0.0, 0.0, 0.0, 0.65],
        },
        text_at(t, x, y),
    ]
}

pub fn compose(sprites: &Sprites, input: &SceneInput) -> Vec<Overlay> {
    let (sw, sh) = input.screen;
    let m = Metrics::for_screen(input.screen);
    let (cx, cy) = ((sw / 2) as i32, (sh / 2) as i32);
    let mut out = Vec::new();

    if input.view.review == Some(Outcome::Photo)
        && let Some((id, w, h)) = sprites.photo
    {
        // The saved photo, letterboxed like the preview but never mirrored.
        out.push(Overlay { sprite: id, rect: letterbox((w, h), input.screen), tint: WHITE });
    }

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

    if input.view.review == Some(Outcome::Oops) {
        out.extend(boxed_text(sprites, &sprites.oops, cx, cy - sprites.oops.h as i32 / 2, m.scale));
    }

    if input.banner {
        let y = sh as i32 - (sh as f32 * 0.15) as i32;
        out.extend(boxed_text(sprites, &sprites.banner, cx, y, m.scale));
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
            oops: ts(4, 900, 160),
            icons: (20..25).map(SpriteId).collect(),
            debug_lines: vec![ts(30, 150, 24), ts(31, 200, 24)],
            photo: Some((SpriteId(40), 1456, 1088)),
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
            photo_count: 12,
        };
        assert_eq!(
            debug_text(&d),
            vec![
                "fps: 60",
                "Camera fps: 60",
                "Photo count: 12",
                "Mirrored: true",
                "Filtered: false",
                "Temperature: 48.3°C",
                "Disk used: --"
            ]
        );
    }

    #[test]
    fn review_shows_photo_letterboxed_and_unmirrored_first() {
        let mut i = input(View { review: Some(Outcome::Photo), ..NONE });
        i.status = Status { temp: Level::Warning, ..Status::default() };
        let out = compose(&sprites(), &i);
        assert_eq!(out[0].sprite, SpriteId(40), "photo under the icons");
        assert_eq!(out[0].rect, Rect { x: 237, y: 0, w: 1445, h: 1080 });
        assert_eq!(out[0].tint, WHITE);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn review_without_photo_sprite_draws_nothing() {
        let mut s = sprites();
        s.photo = None;
        assert!(compose(&s, &input(View { review: Some(Outcome::Photo), ..NONE })).is_empty());
    }

    #[test]
    fn oops_is_centred_on_a_dark_box() {
        let out = compose(&sprites(), &input(View { review: Some(Outcome::Oops), ..NONE }));
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].tint, [0.0, 0.0, 0.0, 0.65]);
        assert_eq!(out[1].sprite, SpriteId(4));
        assert_eq!(out[1].rect, Rect { x: 960 - 450, y: 540 - 80, w: 900, h: 160 });
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
```

- [ ] **Step 2: HUD and app**

`v2/src/display/hud.rs`:
```rust
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
```

`v2/src/display/app.rs`:
```rust
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
        if let Some(photo) = self.ctrl.take_review_image()
            && let Err(e) = gfx.hud.set_photo(&mut gfx.renderer, &photo)
        {
            tracing::error!("showing review photo: {e:#}");
        }
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
            photo_count: self.ctrl.photo_count(),
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
```

- [ ] **Step 3: Verify**

Run: `cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test` — Expected: all pass (108 total).

- [ ] **Step 4: End-to-end on a virtual display**

From the repo root (assets live there):
```bash
mkdir -p /tmp/pb-e2e && printf '[storage]\nroot = "/tmp/pb-e2e"\n[immich]\nlatitude = 48.8584\nlongitude = 2.2945\n' > /tmp/pb-e2e/config.toml
Xvfb :99 -screen 0 1920x1080x24 &
(cd v2 && cargo build)
DISPLAY=:99 timeout 25 v2/target/debug/pibooth --config /tmp/pb-e2e/config.toml --fake-camera --no-gpio > /tmp/pb-e2e/run.log 2>&1 &
sleep 5; W=$(DISPLAY=:99 xdotool search --name piBooth | head -1); DISPLAY=:99 xdotool windowfocus $W key space
sleep 4.85; DISPLAY=:99 import -window root /tmp/pb-e2e/flash.png
sleep 1.2;  DISPLAY=:99 import -window root /tmp/pb-e2e/review.png
sleep 6.5;  DISPLAY=:99 import -window root /tmp/pb-e2e/live.png
DISPLAY=:99 xdotool key Escape
```
Expected:
- `run.log`: `storage ready photos=0 effects=4`, then `photo saved id=1 photo=…/photos/booth_….jpg`; exit 0.
- `/tmp/pb-e2e/photos/` and `/raw/` each hold one 1456×1088 JPEG; the raw one's top-left pixel is red (≈255,0,0) and top-right is light (unmirrored); the photo is sepia-toned with an overlay.
- EXIF (e.g. `python3 -c "from PIL import Image; e=Image.open(p).getexif(); print(e[271], e[272], e.get_ifd(0x8769)[36867], e.get_ifd(0x8825))"`): Make "Raspberry Pi", Model "Global Shutter Camera (IMX296)", DateTimeOriginal, GPS N/E.
- `flash.png` all white; `review.png` shows the sepia photo with the red-ish marker at the top-left (unmirrored); `live.png` shows the mirrored preview again (marker top-right).

- [ ] **Step 5: Commit**

```bash
git add src/osd/scene.rs src/display/hud.rs src/display/app.rs
git commit -m "feat(v2): show the saved photo or \"Oops\" in review"
```

### Task 7: Docs and Pi verification

**Files:**
- Modify: `v2/README.md` (replace), `v2/config.example.toml` (two comment lines), `docs/superpowers/specs/2026-11-18-rust-v2-design.md`

- [ ] **Step 1: README**

Replace `v2/README.md` with:
````markdown
# piBooth v2 (Rust)

Rewrite of the Python booth (`../main.py`). Design: `../docs/superpowers/specs/2026-11-18-rust-v2-design.md`.

Status: milestone 3 — live preview, button and countdown, screen flash,
full-quality capture (high-quality noise reduction, exposure and white balance
locked for the shot), sepia + effect overlay, JPEG with EXIF, review of the
saved photo, status icons and camera auto-restart. Upload to Immich arrives in
milestone 4.

## Photos

Every shot is saved under `storage.root` (default: the working directory):

- `photos/booth_YYYYMMDD-HHMMSS.jpg` — the finished photo (sepia + a random
  overlay from `effects/` when the filter is on, plain otherwise).
- `raw/booth_YYYYMMDD-HHMMSS.jpg` — the unfiltered original
  (`storage.save_original = true`).

Photos are never mirrored, are 1456×1088 (the sensor's full resolution), JPEG
quality `capture.jpeg_quality` (95) with full colour detail, and carry EXIF date,
camera, exposure, ISO and — when `immich.latitude`/`longitude` are set — GPS.

## Develop on a laptop (no Pi needed)

```bash
cargo test
cargo run -- --config config.dev.toml --fake-camera --windowed --no-gpio --bench
```

`config.dev.toml` points `display.assets_dir` at the repo root, where v1's
`fonts/`, `images/` and `effects/` live. Photos taken while developing land in
`v2/photos/` and `v2/raw/`.

Keys: `Space` take a photo, `d` debug overlay (includes the photo count), `r`
mirror preview, `f` filter on/off for the next photos, `Esc`/`q` quit. The red square marks the image's top-left corner; with
mirroring on it appears top-right.

## Button

Same wiring as v1: a push button between GPIO 27 (BCM) and 3.3 V. v2 enables
the internal pull-down and listens for debounced press edges. For a button to
ground instead, set `[button] active_low = true` (internal pull-up).

## Build on the Raspberry Pi 5

```bash
sudo apt install -y libcamera-dev libclang-dev clang pkg-config cage
pkg-config --modversion libcamera   # must be 0.4.x .. 0.7.x for libcamera crate 0.8
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
. "$HOME/.cargo/env"
cargo build --release --features libcamera -j 2
```

On a 4 GB Pi, give the build some swap first (Raspberry Pi OS ships 200 MB),
and close the desktop/browser while it compiles:

```bash
sudo dphys-swapfile swapoff
sudo sed -i 's/^#\?CONF_SWAPSIZE=.*/CONF_SWAPSIZE=2048/' /etc/dphys-swapfile
sudo dphys-swapfile setup && sudo dphys-swapfile swapon
```

Run from a text console (no desktop), from the repo root so `fonts/` and
`images/` are found:

```bash
cd .. && NO_COLOR=1 cage -s -- v2/target/release/pibooth --bench > bench.log 2>&1
```

Check `top` while it runs: pibooth should be well under one full core.

## Install as a service

```bash
sudo useradd -m -G video,render,input,gpio booth
sudo mkdir -p /opt/pibooth && sudo cp -r target/release/pibooth config.example.toml ../fonts ../effects ../images /opt/pibooth/
sudo cp /opt/pibooth/config.example.toml /opt/pibooth/config.toml
sudo chown -R booth: /opt/pibooth
sudo touch /opt/pibooth/.env && sudo chown booth: /opt/pibooth/.env && sudo chmod 600 /opt/pibooth/.env
sudo cp deploy/pibooth.service /etc/systemd/system/
sudo systemctl daemon-reload && sudo systemctl enable --now pibooth
```

The `gpio` group exists on Raspberry Pi OS; elsewhere create it first with `sudo groupadd gpio`.

The unit takes over tty1 (it stops `getty@tty1` via `Conflicts=`); use another VT, e.g. Ctrl+Alt+F2, for a console.

The Immich key goes in `/opt/pibooth/.env` as `PIBOOTH_IMMICH_API_KEY=...`
(or v1's `API_KEY=...`).
````

- [ ] **Step 2: Example config comments**

In `v2/config.example.toml`: the `[display]` `assets_dir` comment becomes `# folder containing fonts/, images/ and effects/ (".." when running from v2/)`, and add as the first line under `[capture]`: `# Milestone 3 uses one frame per photo; burst_frames/stack/save_dng arrive in milestone 5.` (`config::tests::example_config_parses` must still pass.)

- [ ] **Step 3: Spec updates**

1. §3.1 table, `pipeline` row dependencies → "image, jpeg-encoder, fastrand; EXIF via an in-crate TIFF writer (rayon arrives with stacking in milestone 5)"; add a `storage` row: "Photo folders, v1 file names, atomic writes" / "chrono".
2. §4.1: replace the diagram's `Capturing ──burst done──► Review{until}` with `Capturing ──frame──► Processing ──photo──► Review{until}` and add bullets: "`Processing`: the photo worker filters, encodes and saves; preview continues. Failure or a 10 s timeout ⇒ review shows "Oops, try again"." and "Every capture has an id; results for another id are ignored."
3. §4.2 "Configuration" bullet → "One BGR888 stream at 1456×1088 — the sensor's native size — serves preview and capture. ≈`flash.lead_ms` before the shot `PrepareCapture` sets `NoiseReductionMode = HighQuality`; the photo is the next complete frame after `Capture`. A raw Bayer stream is added in milestone 5 when `capture.save_dng = true`."
4. §4.2 "AE/AWB" bullet → "AE/AWB meter during the lead (with the screen flash on in screen mode), then lock at `Capture` by switching `ExposureTimeMode` and `AnalogueGainMode` to Manual and `AwbEnable = false` (libcamera keeps the last computed values); `ReleaseCapture` (once the frame arrives) restores Auto and Fast noise reduction."
5. §4.4 step 5 → "**Encode**: JPEG quality `capture.jpeg_quality` (95), 4:4:4, pure-Rust `jpeg-encoder`, with EXIF Make/Model/Orientation/DateTimeOriginal/ExposureTime/ISO and GPS when configured → `photos/booth_<ts>.jpg`; `_2`, `_3`… for same-second shots; temp file + rename." Step 6 → "Return the review image (the saved photo as RGBA) and re-check disk usage; upload is enqueued in milestone 4."

- [ ] **Step 4: Verify and commit**

Run: `cargo test` (all pass). Then:
```bash
git add README.md config.example.toml ../docs/superpowers/specs/2026-11-18-rust-v2-design.md
git commit -m "docs(v2): milestone 3 README and spec updates"
```

- [ ] **Step 5: Verify on the Pi (user)**

Build (`cargo build --release --features libcamera -j 2`), run from the repo root with `--bench` and `RUST_LOG=info`, take a few photos, then:
```bash
grep -E "frame captured|photo saved|photo failed|camera stopped|bench" bench.log | tail -20
```
Expected:
- `frame captured id=N … exposure_us=… gain=…` then `photo saved … ms=…` (under ~500 ms) per shot; no `camera stopped`.
- `bench` lines stay ≈ 60 fps, including during the flash (high-quality noise reduction must not drop the rate; if `camera_frames` dips below 58 only around shots, note it).
- Files in `photos/` and `raw/` open correctly, are not mirrored, look sharper/cleaner than the live preview in dim light, and carry the EXIF date (and GPS if configured).
- Exposure lock: in two shots of the same scene, `exposure_us`/`gain` in the log match what auto-exposure showed under the flash; the preview returns to normal brightness right after the shot.
---

## Spec coverage (milestone 3)

| Spec item | Task |
|---|---|
| §4.1 capture flow, ids, Processing, review with photo or "Oops" | 5, 6 |
| §4.2 capture from the native-resolution stream, HQ noise reduction, AE/AWB lock and release | 5 |
| §4.4 steps 2–6 (original, sepia, effect, JPEG + EXIF, atomic write, review image) | 1–4 |
| §4.7 disk re-check after each save | 5 |
| §5 "Oops, try again"; camera restart off the render thread | 5, 6 |
| §2 v1 defects: saving disabled, mirrored photos, BGR sepia, `photos/` crash | 2–5 |

Deferred: upload queue (M4); burst stacking, DNG, GPIO strobe (M5); dmabuf, hardware trigger, bare KMS (M6).
