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
    /// Single writer only: the photo worker is the sole caller, so the
    /// exists-check and the later write cannot race.
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
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".part"); // photo.jpg → photo.jpg.part, entry.json → entry.json.part
    let tmp = std::path::PathBuf::from(tmp);
    let mut f = std::fs::File::create(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
    let result = f
        .write_all(bytes)
        .and_then(|_| f.sync_all())
        .with_context(|| format!("writing {}", tmp.display()))
        .and_then(|_| std::fs::rename(&tmp, path).with_context(|| format!("renaming to {}", path.display())));
    if let Err(e) = result {
        drop(f);
        let _ = std::fs::remove_file(&tmp); // best effort: don't leave a stray .part
        return Err(e);
    }
    // Persist the rename itself; not supported everywhere, so ignore failures.
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        let _ = std::fs::File::open(dir).and_then(|d| d.sync_all());
    }
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
    fn failed_rename_leaves_no_part_file() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("x.jpg");
        std::fs::create_dir(&target).unwrap(); // rename file -> directory fails
        assert!(write_atomic(&target, b"jpeg").is_err());
        assert!(!dir.path().join("x.jpg.part").exists(), "temp file cleaned up");
    }

    #[test]
    fn unwritable_root_is_an_error() {
        assert!(Storage::create(Path::new("/proc/pibooth-test")).is_err());
    }
}
