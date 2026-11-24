//! The upload queue: one small JSON file per saved photo in `queue/`,
//! written right after the photo, removed once Immich has it. It survives
//! crashes, reboots and days offline; files are processed oldest first.

use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::storage::write_atomic;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    /// The finished photo (absolute path when it could be resolved).
    pub photo: PathBuf,
    /// When the photo was taken, RFC 3339 UTC (sent as fileCreatedAt).
    pub taken: String,
    /// Set once the upload succeeded, so a retry only redoes the album step.
    #[serde(default)]
    pub asset_id: Option<String>,
    #[serde(default)]
    pub attempts: u32,
    #[serde(default)]
    pub last_error: Option<String>,
}

/// Local wall-clock time of the shot → RFC 3339 UTC with milliseconds
/// (`2026-11-19T17:59:07.818Z`), the form Immich expects for fileCreatedAt.
pub fn taken_utc(taken: chrono::NaiveDateTime) -> String {
    use chrono::TimeZone;
    chrono::Local
        .from_local_datetime(&taken)
        .earliest() // DST fall-back: either reading is fine for a photo
        .map(|t| t.with_timezone(&chrono::Utc))
        .unwrap_or_else(chrono::Utc::now)
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

pub struct Queue {
    dir: PathBuf,
}

impl Queue {
    /// Open (and create) `queue/` under `root`.
    pub fn open(root: &Path) -> anyhow::Result<Self> {
        let dir = root.join("queue");
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        Ok(Self { dir })
    }

    /// Add a saved photo. The entry is named after the photo, so a photo is
    /// never queued twice.
    pub fn enqueue(&self, photo: &Path, taken: &str) -> anyhow::Result<PathBuf> {
        let stem = photo.file_stem().context("photo path has no file name")?;
        let file = self.dir.join(stem).with_extension("json");
        let photo = photo.canonicalize().unwrap_or_else(|_| photo.to_path_buf());
        let entry = Entry { photo, taken: taken.to_string(), asset_id: None, attempts: 0, last_error: None };
        self.update(&file, &entry)?;
        Ok(file)
    }

    /// Pending entries, oldest first (names sort by time). Unreadable files
    /// are renamed to `*.json.bad` so they stop blocking the queue.
    pub fn pending(&self) -> Vec<(PathBuf, Entry)> {
        let Ok(dir) = std::fs::read_dir(&self.dir) else { return Vec::new() };
        let mut files: Vec<PathBuf> = dir
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
            .collect();
        files.sort();
        files
            .into_iter()
            .filter_map(|f| {
                match std::fs::read(&f).map_err(anyhow::Error::from).and_then(|b| Ok(serde_json::from_slice(&b)?)) {
                    Ok(e) => Some((f, e)),
                    Err(err) => {
                        tracing::error!("unreadable queue entry {}: {err:#}; set aside as .bad", f.display());
                        let mut bad = f.as_os_str().to_owned();
                        bad.push(".bad");
                        let _ = std::fs::rename(&f, PathBuf::from(bad));
                        None
                    }
                }
            })
            .collect()
    }

    pub fn len(&self) -> usize {
        std::fs::read_dir(&self.dir)
            .map(|d| d.filter_map(Result::ok).filter(|e| e.path().extension().is_some_and(|x| x == "json")).count())
            .unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn update(&self, file: &Path, entry: &Entry) -> anyhow::Result<()> {
        write_atomic(file, &serde_json::to_vec_pretty(entry)?)
    }

    /// The photo is in Immich: forget it.
    pub fn done(&self, file: &Path) -> anyhow::Result<()> {
        std::fs::remove_file(file).with_context(|| format!("removing {}", file.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn photo(dir: &Path, name: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, b"jpeg").unwrap();
        p
    }

    #[test]
    fn taken_utc_is_rfc3339_utc_millis_of_the_same_instant() {
        use chrono::TimeZone;
        let local = chrono::NaiveDateTime::parse_from_str("2026-11-19 18:59:07.818", "%Y-%m-%d %H:%M:%S%.3f").unwrap();
        let s = taken_utc(local);
        assert!(s.ends_with('Z') && s.len() == "2026-11-19T17:59:07.818Z".len(), "{s}");
        let back = chrono::DateTime::parse_from_rfc3339(&s).unwrap();
        assert_eq!(
            back.with_timezone(&chrono::Local).naive_local(),
            chrono::Local.from_local_datetime(&local).earliest().unwrap().naive_local()
        );
    }

    #[test]
    fn enqueue_lists_oldest_first_and_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let q = Queue::open(dir.path()).unwrap();
        let b = photo(dir.path(), "booth_20261119-185910.jpg");
        let a = photo(dir.path(), "booth_20261119-185907.jpg");
        q.enqueue(&b, "2026-11-19T17:59:10.000Z").unwrap();
        q.enqueue(&a, "2026-11-19T17:59:07.000Z").unwrap();
        let q = Queue::open(dir.path()).unwrap(); // e.g. after a reboot
        let pending = q.pending();
        assert_eq!(pending.len(), 2);
        assert_eq!(pending[0].1.photo, a.canonicalize().unwrap());
        assert_eq!(pending[0].1.taken, "2026-11-19T17:59:07.000Z");
        assert_eq!(pending[0].0.file_name().unwrap(), "booth_20261119-185907.json");
        assert_eq!(q.len(), 2);
    }

    #[test]
    fn update_and_done() {
        let dir = tempfile::tempdir().unwrap();
        let q = Queue::open(dir.path()).unwrap();
        let file = q.enqueue(&photo(dir.path(), "booth_1.jpg"), "t").unwrap();
        let (_, mut e) = q.pending().remove(0);
        e.asset_id = Some("abc".into());
        e.attempts = 3;
        q.update(&file, &e).unwrap();
        assert_eq!(q.pending()[0].1, e);
        q.done(&file).unwrap();
        assert!(q.is_empty());
        assert!(q.pending().is_empty());
    }

    #[test]
    fn corrupt_entries_are_set_aside() {
        let dir = tempfile::tempdir().unwrap();
        let q = Queue::open(dir.path()).unwrap();
        std::fs::write(dir.path().join("queue/booth_0.json"), b"{not json").unwrap();
        q.enqueue(&photo(dir.path(), "booth_1.jpg"), "t").unwrap();
        let pending = q.pending();
        assert_eq!(pending.len(), 1);
        assert!(dir.path().join("queue/booth_0.json.bad").exists());
        assert_eq!(q.len(), 1);
    }

    #[test]
    fn older_entry_files_without_optional_fields_still_load() {
        let dir = tempfile::tempdir().unwrap();
        let q = Queue::open(dir.path()).unwrap();
        std::fs::write(dir.path().join("queue/booth_0.json"), br#"{"photo":"/x.jpg","taken":"t"}"#).unwrap();
        let e = &q.pending()[0].1;
        assert_eq!((e.asset_id.clone(), e.attempts), (None, 0));
    }
}
