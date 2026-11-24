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
pub fn encode(raw: &RawImage, colour: &RawColour, info: &PhotoInfo) -> anyhow::Result<Vec<u8>> {
    let n = raw.width as usize * raw.height as usize;
    if raw.data.len() < n {
        anyhow::bail!("raw frame has {} samples, {}x{} needs {n}", raw.data.len(), raw.width, raw.height);
    }
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
    Ok(out)
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
        let bytes = encode(&r, &colour(), &info()).unwrap();
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
    fn short_data_is_an_error() {
        let mut r = raw(4, 2);
        r.data.truncate(7);
        assert!(encode(&r, &colour(), &info()).is_err());
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
        let bytes = encode(&raw(4, 2), &colour(), &info()).unwrap();
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
        let e = exif::Reader::new().read_raw(encode(&raw(4, 2), &RawColour::default(), &info()).unwrap()).unwrap();
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
        let e = exif::Reader::new().read_raw(encode(&r, &colour(), &info()).unwrap()).unwrap();
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
        std::fs::write(out, encode(&RawImage { width: w, height: h, cfa: RGGB, data }, &c, &info()).unwrap()).unwrap();
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
