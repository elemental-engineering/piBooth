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
    /// Do not try again before this Unix time (seconds): set after a failure
    /// that only this photo causes, so it does not block the others.
    #[serde(default)]
    pub next_try: Option<u64>,
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

/// `booth_YYYYMMDD-HHMMSS[_n]` (local time) → RFC 3339 UTC; falls back to the
/// file's modification time.
fn taken_from_name(stem: &str, path: &Path) -> String {
    let parsed = stem
        .strip_prefix("booth_")
        .and_then(|r| r.get(..15))
        .and_then(|t| chrono::NaiveDateTime::parse_from_str(t, "%Y%m%d-%H%M%S").ok());
    match parsed {
        Some(t) => taken_utc(t),
        None => std::fs::metadata(path)
            .and_then(|m| m.modified())
            .map(|t| chrono::DateTime::<chrono::Utc>::from(t).to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
            .unwrap_or_else(|_| chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)),
    }
}

/// Sort key of a booth photo stem `booth_YYYYMMDD-HHMMSS[_N]`: (timestamp
/// part, N), N defaulting to 1, so `_10` sorts after `_9`. `None` for any
/// other name (stray files in `photos/`).
fn booth_key(stem: &str) -> Option<(&str, u64)> {
    let rest = stem.strip_prefix("booth_")?;
    match rest.split_once('_') {
        Some((ts, n)) => Some((ts, n.parse().unwrap_or(1))),
        None => Some((rest, 1)),
    }
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
        let file = self.entry_path(stem);
        let photo = photo.canonicalize().unwrap_or_else(|_| photo.to_path_buf());
        let entry =
            Entry { photo, taken: taken.to_string(), asset_id: None, attempts: 0, last_error: None, next_try: None };
        self.update(&file, &entry)?;
        self.advance_last(&stem.to_string_lossy());
        Ok(file)
    }

    /// `queue/<stem>.json`; appended (not `with_extension`) so stems that
    /// contain dots stay distinct.
    fn entry_path(&self, stem: &std::ffi::OsStr) -> PathBuf {
        let mut name = stem.to_os_string();
        name.push(".json");
        self.dir.join(name)
    }

    fn last_path(&self) -> PathBuf {
        self.dir.join(".last")
    }

    fn read_last(&self) -> Option<String> {
        std::fs::read_to_string(self.last_path()).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
    }

    /// Remember the newest queued photo (names sort by time), only forwards.
    fn advance_last(&self, stem: &str) {
        let Some(key) = booth_key(stem) else { return };
        if self.read_last().is_some_and(|l| booth_key(&l).is_some_and(|k| k >= key)) {
            return;
        }
        if let Err(e) = write_atomic(&self.last_path(), stem.as_bytes()) {
            tracing::warn!("cannot record newest queued photo: {e:#}");
        }
    }

    /// Queue photos that were saved but never queued (crash or power loss
    /// between the two writes). Without `queue/.last` (first start with
    /// uploads) nothing is queued: photos from before are not mass-uploaded;
    /// the newest one just becomes the marker. Returns how many were queued.
    pub fn reconcile(&self, photos_dir: &Path) -> anyhow::Result<usize> {
        let mut photos: Vec<(String, PathBuf)> = match std::fs::read_dir(photos_dir) {
            Ok(d) => d
                .filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "jpg"))
                .filter_map(|p| Some((p.file_stem()?.to_string_lossy().into_owned(), p)))
                .filter(|(stem, _)| booth_key(stem).is_some()) // stray JPEGs are not booth photos
                .collect(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e).with_context(|| format!("reading {}", photos_dir.display())),
        };
        photos.sort_by(|a, b| booth_key(&a.0).cmp(&booth_key(&b.0)));
        let Some(last) = self.read_last() else {
            if let Some((stem, _)) = photos.last() {
                write_atomic(&self.last_path(), stem.as_bytes())?;
            }
            return Ok(0);
        };
        let last_key = booth_key(&last);
        let mut count = 0;
        for (stem, path) in photos.iter().filter(|(stem, _)| booth_key(stem) > last_key) {
            if self.entry_path(stem.as_ref()).exists() {
                continue;
            }
            self.enqueue(path, &taken_from_name(stem, path))?;
            count += 1;
        }
        Ok(count)
    }

    /// Pending entries, oldest first (names sort by time). Files with invalid JSON
    /// are renamed to `*.json.bad` so they stop blocking the queue.
    pub fn pending(&self) -> Vec<(PathBuf, Entry)> {
        let Ok(dir) = std::fs::read_dir(&self.dir) else { return Vec::new() };
        let mut files: Vec<PathBuf> = dir
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
            .collect();
        // Oldest first; `_10` after `_9` (other names first, by name).
        files.sort_by_cached_key(|f| {
            let stem = f.file_stem().unwrap_or_default().to_string_lossy().into_owned();
            (booth_key(&stem).map(|(ts, n)| (ts.to_owned(), n)), stem)
        });
        files
            .into_iter()
            .filter_map(|f| {
                let bytes = match std::fs::read(&f) {
                    Ok(b) => b,
                    Err(err) => {
                        // Possibly transient: leave it alone, look again next pass.
                        tracing::warn!("cannot read queue entry {}: {err}; skipping it this pass", f.display());
                        return None;
                    }
                };
                match serde_json::from_slice(&bytes) {
                    Ok(e) => Some((f, e)),
                    Err(err) => {
                        tracing::error!("unreadable queue entry {}: {err}; set aside as .bad", f.display());
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
        assert_eq!((e.asset_id.clone(), e.attempts, e.next_try), (None, 0, None));
    }

    fn jpg_dir(root: &Path, names: &[&str]) -> PathBuf {
        let d = root.join("photos");
        std::fs::create_dir_all(&d).unwrap();
        for n in names {
            std::fs::write(d.join(n), b"jpeg").unwrap();
        }
        d
    }

    fn last(root: &Path) -> Option<String> {
        std::fs::read_to_string(root.join("queue/.last")).ok()
    }

    #[test]
    fn first_reconcile_queues_nothing_and_sets_marker() {
        let dir = tempfile::tempdir().unwrap();
        let q = Queue::open(dir.path()).unwrap();
        let photos = jpg_dir(dir.path(), &["booth_20261119-185907.jpg", "booth_20261119-185910.jpg"]);
        assert_eq!(q.reconcile(&photos).unwrap(), 0);
        assert!(q.is_empty());
        assert_eq!(last(dir.path()).as_deref(), Some("booth_20261119-185910"));
    }

    #[test]
    fn first_reconcile_without_photos_leaves_marker_absent() {
        let dir = tempfile::tempdir().unwrap();
        let q = Queue::open(dir.path()).unwrap();
        assert_eq!(q.reconcile(&dir.path().join("photos")).unwrap(), 0);
        assert_eq!(last(dir.path()), None);
    }

    #[test]
    fn reconcile_queues_unqueued_newer_photos_with_taken_from_name() {
        let dir = tempfile::tempdir().unwrap();
        let q = Queue::open(dir.path()).unwrap();
        let photos = jpg_dir(dir.path(), &["booth_20261119-185907.jpg"]);
        q.reconcile(&photos).unwrap();
        jpg_dir(dir.path(), &["booth_20261119-190000.jpg", "booth_20261119-190000_2.jpg"]);
        assert_eq!(q.reconcile(&photos).unwrap(), 2);
        let local = chrono::NaiveDateTime::parse_from_str("2026-11-19 19:00:00", "%Y-%m-%d %H:%M:%S").unwrap();
        let by_name: Vec<_> = q.pending().into_iter().map(|(f, e)| (f.file_name().unwrap().to_owned(), e)).collect();
        let e = &by_name.iter().find(|(n, _)| n == "booth_20261119-190000.json").unwrap().1;
        assert_eq!(e.taken, taken_utc(local));
        // Second run: everything is queued or older than the marker.
        assert_eq!(q.reconcile(&photos).unwrap(), 0);
    }

    #[test]
    fn stray_jpegs_are_not_queued_and_do_not_move_the_marker() {
        let dir = tempfile::tempdir().unwrap();
        let q = Queue::open(dir.path()).unwrap();
        let photos = jpg_dir(dir.path(), &["booth_20261119-185907.jpg", "party.jpg"]);
        assert_eq!(q.reconcile(&photos).unwrap(), 0);
        assert_eq!(last(dir.path()).as_deref(), Some("booth_20261119-185907"));
        assert_eq!(q.reconcile(&photos).unwrap(), 0);
        assert!(q.is_empty());
        q.enqueue(&photo(dir.path(), "party.jpg"), "t").unwrap();
        assert_eq!(last(dir.path()).as_deref(), Some("booth_20261119-185907"));
    }

    #[test]
    fn numeric_suffix_sorts_numerically() {
        let dir = tempfile::tempdir().unwrap();
        let q = Queue::open(dir.path()).unwrap();
        let photos = jpg_dir(dir.path(), &["booth_20261119-190000.jpg", "booth_20261119-190000_9.jpg"]);
        q.reconcile(&photos).unwrap(); // marker = _9
        assert_eq!(last(dir.path()).as_deref(), Some("booth_20261119-190000_9"));
        jpg_dir(dir.path(), &["booth_20261119-190000_10.jpg"]);
        assert_eq!(q.reconcile(&photos).unwrap(), 1);
        assert_eq!(last(dir.path()).as_deref(), Some("booth_20261119-190000_10"));
        assert_eq!(booth_key("booth_X"), Some(("X", 1)));
        assert!(booth_key("booth_X_10") > booth_key("booth_X_9"));
        let order: Vec<_> = q.pending().into_iter().map(|(f, _)| f.file_stem().unwrap().to_owned()).collect();
        assert_eq!(order.last().unwrap(), "booth_20261119-190000_10");
    }

    #[test]
    fn reconcile_skips_queued_and_older_photos() {
        let dir = tempfile::tempdir().unwrap();
        let q = Queue::open(dir.path()).unwrap();
        let photos = jpg_dir(dir.path(), &["booth_20261119-185907.jpg"]);
        q.reconcile(&photos).unwrap(); // marker = 185907
        jpg_dir(dir.path(), &["booth_20261119-180000.jpg", "booth_20261119-190000.jpg"]);
        // 190000 already has an entry.
        q.enqueue(&photos.join("booth_20261119-190000.jpg"), "t").unwrap();
        assert_eq!(q.reconcile(&photos).unwrap(), 0, "older than marker or already queued");
        assert_eq!(q.len(), 1);
    }

    #[test]
    fn enqueue_advances_marker_only_forwards() {
        let dir = tempfile::tempdir().unwrap();
        let q = Queue::open(dir.path()).unwrap();
        q.enqueue(&photo(dir.path(), "booth_20261119-185910.jpg"), "t").unwrap();
        assert_eq!(last(dir.path()).as_deref(), Some("booth_20261119-185910"));
        q.enqueue(&photo(dir.path(), "booth_20261119-185907.jpg"), "t").unwrap();
        assert_eq!(last(dir.path()).as_deref(), Some("booth_20261119-185910"));
        q.enqueue(&photo(dir.path(), "booth_20261119-185920.jpg"), "t").unwrap();
        assert_eq!(last(dir.path()).as_deref(), Some("booth_20261119-185920"));
    }

    #[test]
    fn stems_with_dots_get_distinct_entries() {
        let dir = tempfile::tempdir().unwrap();
        let q = Queue::open(dir.path()).unwrap();
        q.enqueue(&photo(dir.path(), "booth.a.jpg"), "t").unwrap();
        q.enqueue(&photo(dir.path(), "booth.b.jpg"), "t").unwrap();
        assert_eq!(q.len(), 2);
        assert!(dir.path().join("queue/booth.a.json").exists() && dir.path().join("queue/booth.b.json").exists());
    }
}
