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
const UNDEFINED: u16 = 7;

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

/// Degrees → (deg, min, sec×1000/1000) rationals. Works on an integer count
/// of thousandths of an arc-second so seconds never round up to 60.000.
fn dms(value: f64) -> [(u32, u32); 3] {
    let total = (value.abs() * 3600.0 * 1000.0).round() as u64;
    let deg = total / 3_600_000;
    let min = total % 3_600_000 / 60_000;
    let sec_milli = total % 60_000;
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

    let mut exif = vec![
        Entry { tag: 0x9000, kind: UNDEFINED, count: 4, data: b"0232".to_vec() }, // ExifVersion
        ascii(0x9003, &stamp),
        ascii(0x9004, &stamp),
    ];
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
        assert_eq!(text(&e, Tag::DateTimeDigitized), "2026-11-19 14:03:07");
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

    #[test]
    fn exif_version_is_present() {
        let e = parse(build(&info()));
        match &e.get_field(Tag::ExifVersion, In::PRIMARY).unwrap().value {
            Value::Undefined(v, _) => assert_eq!(v, b"0232"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn dms_carries_instead_of_reaching_sixty_seconds() {
        assert_eq!(dms(10.999_999_999), [(11, 1), (0, 1), (0, 1000)]);
        assert_eq!(dms(48.858_37), [(48, 1), (51, 1), (30_132, 1000)]);
    }
}
