//! One photo, start to finish: burst stacking, optional unfiltered original,
//! sepia + random effect overlay, JPEG with EXIF, atomic writes, the optional
//! DNG, and the review image.

use std::path::PathBuf;
use std::time::Instant;

use anyhow::Context;
use jpeg_encoder::{ColorType, Encoder, SamplingFactor};

use super::dng;
use super::exif::{PhotoInfo, build as build_exif};
use super::filters::{composite, sepia, to_rgba};
use super::stack::{StackReport, stack};
use crate::camera::RawCapture;
use crate::config::StackMode;
use crate::osd::Rgba;
use crate::storage::{Storage, write_atomic};

/// A captured burst plus everything needed to turn it into files.
#[derive(Debug, Clone)]
pub struct PhotoJob {
    pub id: u64,
    pub width: u32,
    pub height: u32,
    /// Burst frames: tightly packed RGB, row 0 at the top, never mirrored;
    /// `frames[0]` is the photo metered under the flash. At least one.
    pub frames: Vec<Vec<u8>>,
    pub stack: StackMode,
    pub max_shift_px: u32,
    /// Written as `raw/<name>.dng` when present (`capture.save_dng`).
    pub raw: Option<RawCapture>,
    pub info: PhotoInfo,
    pub filter: bool,
    pub jpeg_quality: u8,
    pub save_original: bool,
}

/// Time spent in each stage of `process`, in milliseconds (a stage that did
/// not run is 0).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Timings {
    pub stack_ms: u64,
    pub encode_original_ms: u64,
    pub write_original_ms: u64,
    pub filter_ms: u64,
    pub encode_photo_ms: u64,
    pub write_photo_ms: u64,
    pub dng_ms: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PhotoDone {
    pub id: u64,
    pub photo: PathBuf,
    pub original: Option<PathBuf>,
    pub dng: Option<PathBuf>,
    pub stack: StackReport,
    /// What the guests see during review: the saved photo as RGBA.
    pub review: Rgba,
    pub timings: Timings,
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
    let dng_path = original_path.with_extension("dng"); // raw/<photo name>.dng

    let mut t = Timings::default();
    let ms = |since: Instant| since.elapsed().as_millis() as u64;

    let clock = Instant::now();
    let frames = std::mem::take(&mut job.frames);
    let (mut rgb, report) = stack(frames, job.width, job.height, job.stack, job.max_shift_px);
    t.stack_ms = ms(clock);

    let clock = Instant::now();
    let plain = if job.save_original || !job.filter {
        Some(encode_jpeg(&rgb, job.width, job.height, job.jpeg_quality, &exif)?)
    } else {
        None
    };
    t.encode_original_ms = ms(clock);
    let clock = Instant::now();
    let original = match (&plain, job.save_original) {
        (Some(bytes), true) => {
            write_atomic(&original_path, bytes)?;
            Some(original_path)
        }
        _ => None,
    };
    t.write_original_ms = ms(clock);

    let clock = Instant::now();
    let photo_bytes = if job.filter {
        sepia(&mut rgb);
        if !effects.is_empty() {
            let i = pick(effects.len()).min(effects.len() - 1);
            let effect = &effects[i];
            if (effect.width, effect.height) == (job.width, job.height) {
                composite(&mut rgb, effect);
            } else {
                tracing::warn!(
                    effect = ?(effect.width, effect.height),
                    frame = ?(job.width, job.height),
                    "effect size differs from frame; overlay skipped"
                );
            }
        }
        t.filter_ms = ms(clock);
        let clock = Instant::now();
        let bytes = encode_jpeg(&rgb, job.width, job.height, job.jpeg_quality, &exif)?;
        t.encode_photo_ms = ms(clock);
        bytes
    } else {
        plain.expect("plain JPEG exists when the filter is off")
    };
    let clock = Instant::now();
    write_atomic(&photo_path, &photo_bytes)?;
    t.write_photo_ms = ms(clock);

    // Last, and never fatal: the guests' photo is already safe.
    let clock = Instant::now();
    let dng = job.raw.as_ref().and_then(|raw| {
        match dng::encode(&raw.image, &raw.colour, &job.info).and_then(|bytes| write_atomic(&dng_path, &bytes)) {
            Ok(()) => Some(dng_path.clone()),
            Err(e) => {
                tracing::error!("DNG not saved: {e:#}");
                None
            }
        }
    });
    t.dng_ms = ms(clock);

    Ok(PhotoDone {
        id: job.id,
        photo: photo_path,
        original,
        dng,
        stack: report,
        review: to_rgba(&rgb, job.width, job.height),
        timings: t,
    })
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
            frames: vec![[200u8, 40, 40].repeat((w * h) as usize)], // reddish
            stack: StackMode::Average,
            max_shift_px: 8,
            raw: None,
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
    fn mismatched_effect_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::create(dir.path()).unwrap();
        let done = process(job(true, false), &storage, &[effect(8, 8)], |_| 0).unwrap();
        assert!(done.photo.exists());
        // Only sepia was applied: the left column carries no blue overlay.
        let edge = decode(&done.photo).get_pixel(0, 8).0;
        assert!(edge[0] > edge[1] && edge[1] > edge[2] && edge[2] < 120, "sepia only, no overlay: {edge:?}");
    }

    #[test]
    fn burst_is_stacked_before_filtering() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::create(dir.path()).unwrap();
        let mut j = job(false, false);
        let second = j.frames[0].clone();
        j.frames.push(second);
        let done = process(j, &storage, &[], |_| 0).unwrap();
        assert_eq!(done.stack.frames.len(), 2);
        assert_eq!(done.stack.averaged(), 1);
        let px = decode(&done.photo).get_pixel(16, 8).0;
        assert!(px[0] > 180 && px[1] < 70, "same colours after stacking: {px:?}");
    }

    fn raw_capture() -> RawCapture {
        use crate::pipeline::dng::{RGGB, RawColour, RawImage};
        RawCapture {
            image: RawImage { width: 32, height: 16, cfa: RGGB, data: vec![1 << 6; 32 * 16] },
            colour: RawColour::default(),
        }
    }

    #[test]
    fn dng_is_saved_next_to_the_original() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::create(dir.path()).unwrap();
        let mut j = job(true, true);
        j.raw = Some(raw_capture());
        let done = process(j, &storage, &[], |_| 0).unwrap();
        let dng = done.dng.expect("dng written");
        assert!(dng.ends_with("raw/booth_20261119-140307.dng"));
        let bytes = std::fs::read(&dng).unwrap();
        assert_eq!(&bytes[..4], b"II\x2a\x00");
        assert!(bytes.len() > 32 * 16 * 2);
        assert!(done.timings.dng_ms < 5_000);
    }

    #[test]
    fn dng_failure_keeps_the_photo() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::create(dir.path()).unwrap();
        std::fs::remove_dir(dir.path().join("raw")).unwrap();
        let mut j = job(true, false);
        j.raw = Some(raw_capture());
        let done = process(j, &storage, &[], |_| 0).unwrap();
        assert!(done.photo.exists());
        assert_eq!(done.dng, None);
    }

    #[test]
    fn short_raw_data_keeps_the_photo() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::create(dir.path()).unwrap();
        let mut j = job(true, false);
        let mut raw = raw_capture();
        raw.image.data.truncate(10);
        j.raw = Some(raw);
        let done = process(j, &storage, &[], |_| 0).unwrap();
        assert!(done.photo.exists());
        assert_eq!(done.dng, None);
    }

    #[test]
    fn effect_index_is_clamped() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::create(dir.path()).unwrap();
        assert!(process(job(true, false), &storage, &[effect(32, 16)], |_| 99).is_ok());
    }
}
