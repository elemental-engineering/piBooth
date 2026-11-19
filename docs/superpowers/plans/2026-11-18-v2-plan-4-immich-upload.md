# piBooth v2 — Plan 4: Immich Upload (Milestone 4)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Upload every finished photo to Immich in the background — queued on disk right after it is saved, uploaded oldest first, added to the configured album — so nothing is lost while the booth is offline, crashes or reboots.

**Architecture:** The photo worker writes `queue/<photo>.json` atomically after saving a photo. One upload thread (blocking `ureq` client, rustls with `ring`, the system certificate store) checks the key at start, then drains the queue: `POST /api/assets` (multipart, fields valid for Immich 1.x–3.x), stores the asset id in the entry, `PUT /api/albums/{id}/assets`, deletes the entry. Network/5xx errors back off 5 s→10 min; 4xx "needs attention" errors retry every 10 min. The controller wakes the thread after each photo; the debug overlay shows the queue.

**Tech Stack:** Rust 2024 (rustc ≥ 1.88); new: `ureq` 3.5 (`json`, `multipart`, `platform-verifier`), `serde_json` 1; dev: `mockito` 1.7. The Immich API facts were taken from the server's OpenAPI specs for v1.132.3, v2.0.0, v2.3.0 and main (3.4.0-rc) and the v2.3 CLI's upload code.

**Spec:** `docs/superpowers/specs/2026-11-18-rust-v2-design.md` (§4.6 upload, §4.8 config; this plan covers §8 milestone 4). Builds on Plans 1–3 on branch `v2-rust`.

## Global Constraints

- Crate `pibooth` in `v2/`; v1 files are read-only.
- Only finished photos from `photos/` are uploaded; `raw/` originals stay local. Local photos are never deleted.
- Queue: `<storage.root>/queue/<photo stem>.json` = `{ photo, taken, asset_id?, attempts, last_error? }`, `taken` RFC 3339 UTC with milliseconds (`2026-11-19T17:59:07.818Z`); written with `write_atomic` (temp `*.part` + rename) by the photo worker right after the photo file, also when upload is off; processed oldest first (file-name order); unreadable entries renamed `*.json.bad`; an entry is deleted only after upload and album add both succeed; an entry whose photo is gone is dropped with a warning.
- Upload request: `POST {base_url}/assets`, header `x-api-key`, multipart fields `deviceAssetId` = photo file name, `deviceId` = `piBooth`, `fileCreatedAt` = `fileModifiedAt` = `taken`, `filename`, `isFavorite` = `false`, `assetData` (file, `image/jpeg`). HTTP 200/201 with `{id, status}`; `created` and `duplicate` both succeed. The asset id is stored before the album step so a retry never re-uploads.
- Album: `PUT {base_url}/albums/{album_id}/assets` JSON `{"ids":[id]}` when `album_id` is non-empty; response array entry `success: true` or `error: "duplicate"` ⇒ success.
- GPS/date reach Immich through EXIF (milestone 3); no separate location call.
- Failure classes: transport errors, 408, 429, 5xx ⇒ retry with backoff 5 s doubling to 600 s (reset on success); other 4xx ⇒ "needs attention", retry after 600 s. Timeouts: connect 5 s, whole request 120 s. Startup `GET {base_url}/users/me` logs `Immich OK user=… album=…` or the failure.
- `base_url` is the API root including `/api` (v1 `BASE_URL`); a trailing `/` is tolerated. API key from `PIBOOTH_IMMICH_API_KEY` or `.env` `API_KEY` (existing config code). Upload runs only when key and base_url are set.
- TLS: rustls with `ring` (no aws-lc build on the Pi) and the platform certificate store.
- The render thread never blocks on uploads; at quit the upload thread is detached.
- `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` pass on x86 without the `libcamera` feature; `--features libcamera` clippy passes where libcamera ≥ 0.5 headers exist.
- Commit messages end with the project's attribution trailer lines.

## File Structure

| File | Responsibility |
|---|---|
| `v2/src/storage.rs` | `write_atomic` temp name = path + `.part` (works for `.json`) |
| `v2/src/upload/mod.rs` (new) | module list |
| `v2/src/upload/queue.rs` (new) | `Entry`, `Queue`, `taken_utc` |
| `v2/src/upload/immich.rs` (new) | `Immich` client, `Failure`, `Uploaded`, `classify` |
| `v2/src/upload/uploader.rs` (new) | upload thread: `Uploader`, `Pacing`, `UploadStatus` |
| `v2/src/pipeline/worker.rs` | enqueue after saving |
| `v2/src/display/controller.rs` | queue + uploader wiring, `upload_status()` |
| `v2/src/osd/scene.rs`, `v2/src/display/app.rs` | "Upload queue" debug line |
| `v2/tools/fake_immich.py` (new) | fake Immich for dev / end-to-end tests |
| `v2/Cargo.toml`, `v2/.gitignore`, `v2/README.md`, `v2/config.example.toml`, spec | deps, ignore `queue/`, docs |

All commands run from `v2/` unless stated.

---
### Task 1: Upload queue

**Files:**
- Create: `v2/src/upload/mod.rs`, `v2/src/upload/queue.rs`
- Modify: `v2/Cargo.toml`, `v2/src/lib.rs` (add `pub mod upload;` after `pub mod storage;`), `v2/src/storage.rs` (`write_atomic` temp name), `v2/.gitignore`
- Test: inline in `queue.rs`; existing storage tests must still pass

**Interfaces:**
- Produces: `pub struct Entry { pub photo: PathBuf, pub taken: String, pub asset_id: Option<String>, pub attempts: u32, pub last_error: Option<String> }` (serde, optional fields default); `pub fn taken_utc(chrono::NaiveDateTime) -> String`; `pub struct Queue` with `open(root) -> anyhow::Result<Self>`, `enqueue(&self, photo: &Path, taken: &str) -> anyhow::Result<PathBuf>`, `pending(&self) -> Vec<(PathBuf, Entry)>`, `len`, `is_empty`, `update(&self, file, &Entry)`, `done(&self, file)`.

- [ ] **Step 1: Dependency, ignore file, module list, atomic-write temp name**

`v2/Cargo.toml` `[dependencies]`: after `serde = …` add `serde_json = "1"`.
`v2/.gitignore`: append the line `queue/`.
`v2/src/upload/mod.rs` (Tasks 2–3 add modules):
```rust
//! Uploading saved photos to Immich (spec §4.6).

pub mod queue;
```

In `v2/src/storage.rs` `write_atomic`, replace `let tmp = path.with_extension("jpg.part");` with:
```rust
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".part"); // photo.jpg → photo.jpg.part, entry.json → entry.json.part
    let tmp = std::path::PathBuf::from(tmp);
```

- [ ] **Step 2: Write the failing tests**

Create `queue.rs` with the Interfaces items stubbed `todo!()`, plus:
```rust
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
```

Run: `cargo test upload::queue` — Expected: FAIL.

- [ ] **Step 3: Implement**

`v2/src/upload/queue.rs`:
```rust
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
```

- [ ] **Step 4: Verify**

Run: `cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check` — Expected: all pass (5 new), clean.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock .gitignore src/lib.rs src/storage.rs src/upload
git commit -m "feat(v2): crash-safe upload queue"
```

### Task 2: Immich client

**Files:**
- Create: `v2/src/upload/immich.rs`
- Modify: `v2/Cargo.toml`, `v2/src/upload/mod.rs` (add `pub mod immich;` before `pub mod queue;`)
- Test: inline, against a `mockito` HTTP server

**Interfaces:**
- Produces: `pub enum Failure { Retry(String), Permanent(String) }` (`Display`: "temporary: …" / "needs attention: …"); `pub struct Uploaded { pub id: String, pub duplicate: bool }`; `pub fn classify(status: u16, body: &str) -> Failure`; `pub struct Immich` with `new(base_url: &str, key: &str, album: Option<&str>)`, `album(&self) -> Option<&str>`, `whoami(&self) -> Result<String, Failure>`, `upload(&self, photo: &Path, taken: &str) -> Result<Uploaded, Failure>`, `add_to_album(&self, asset_id: &str) -> Result<(), Failure>`.

- [ ] **Step 1: Dependencies**

`[dependencies]`: after `toml = …` add:
```toml
ureq = { version = "3.5", features = ["json", "multipart", "platform-verifier"] }
```

`[dev-dependencies]`: after `kamadak-exif = …` add `mockito = "1.7"`.
Check: `cargo tree -i ring` shows `ring` and `cargo tree -i aws-lc-rs` finds nothing (no aws-lc C build on the Pi).

- [ ] **Step 2: Write the failing tests**

Create `immich.rs` with the Interfaces items stubbed `todo!()`, plus:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use mockito::Matcher;

    const ID: &str = "4c1d8f6e-0b2a-4f6e-9a7c-2d4e6f8a0b1c";

    fn photo() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("booth_20261119-185907.jpg");
        std::fs::write(&p, b"\xFF\xD8fake-jpeg\xFF\xD9").unwrap();
        (dir, p)
    }

    #[test]
    fn upload_sends_v1_compatible_form_and_parses_created() {
        let mut server = mockito::Server::new();
        let m = server
            .mock("POST", "/api/assets")
            .match_header("x-api-key", "secret")
            .match_header("content-type", Matcher::Regex("multipart/form-data; boundary=".into()))
            .match_body(Matcher::AllOf(vec![
                Matcher::Regex(r#"name="deviceAssetId"\r\n\r\nbooth_20261119-185907.jpg"#.into()),
                Matcher::Regex(r#"name="deviceId"\r\n\r\npiBooth"#.into()),
                Matcher::Regex(r#"name="fileCreatedAt"\r\n\r\n2026-11-19T17:59:07.818Z"#.into()),
                Matcher::Regex(r#"name="fileModifiedAt"\r\n\r\n2026-11-19T17:59:07.818Z"#.into()),
                Matcher::Regex(r#"name="assetData"; filename="booth_20261119-185907.jpg""#.into()),
                Matcher::Regex("fake-jpeg".into()),
            ]))
            .with_status(201)
            .with_header("content-type", "application/json")
            .with_body(format!(r#"{{"id":"{ID}","status":"created"}}"#))
            .create();
        let (_d, p) = photo();
        let c = Immich::new(&format!("{}/api/", server.url()), "secret", None);
        assert_eq!(c.upload(&p, "2026-11-19T17:59:07.818Z"), Ok(Uploaded { id: ID.into(), duplicate: false }));
        m.assert();
    }

    #[test]
    fn duplicate_is_success() {
        let mut server = mockito::Server::new();
        server
            .mock("POST", "/api/assets")
            .with_status(200)
            .with_body(format!(r#"{{"id":"{ID}","status":"duplicate"}}"#))
            .create();
        let (_d, p) = photo();
        let c = Immich::new(&format!("{}/api", server.url()), "k", None);
        assert!(c.upload(&p, "t").unwrap().duplicate);
    }

    #[test]
    fn errors_are_classified() {
        let mut server = mockito::Server::new();
        server.mock("POST", "/api/assets").with_status(401).with_body("Invalid API key").create();
        let (_d, p) = photo();
        let c = Immich::new(&format!("{}/api", server.url()), "bad", None);
        assert!(
            matches!(c.upload(&p, "t"), Err(Failure::Permanent(m)) if m.contains("401") && m.contains("Invalid API key"))
        );
        assert!(matches!(classify(503, ""), Failure::Retry(_)));
        assert!(matches!(classify(429, ""), Failure::Retry(_)));
        assert!(matches!(classify(404, ""), Failure::Permanent(_)));
        // Nothing listening: a network failure, retried.
        let dead = Immich::new("http://127.0.0.1:9/api", "k", None);
        assert!(matches!(dead.upload(&p, "t"), Err(Failure::Retry(_))));
        // Missing file: no point retrying.
        assert!(matches!(c.upload(Path::new("/nonexistent.jpg"), "t"), Err(Failure::Permanent(_))));
    }

    #[test]
    fn album_add_success_duplicate_and_refusal() {
        let mut server = mockito::Server::new();
        let ok = server
            .mock("PUT", "/api/albums/alb-1/assets")
            .match_header("x-api-key", "k")
            .match_body(Matcher::Json(serde_json::json!({ "ids": [ID] })))
            .with_status(200)
            .with_body(format!(r#"[{{"id":"{ID}","success":true}}]"#))
            .expect(1)
            .create();
        let c = Immich::new(&format!("{}/api", server.url()), "k", Some("alb-1"));
        assert_eq!(c.add_to_album(ID), Ok(()));
        ok.assert();

        server
            .mock("PUT", "/api/albums/alb-2/assets")
            .with_status(200)
            .with_body(format!(r#"[{{"id":"{ID}","success":false,"error":"duplicate"}}]"#))
            .create();
        assert_eq!(Immich::new(&format!("{}/api", server.url()), "k", Some("alb-2")).add_to_album(ID), Ok(()));

        server
            .mock("PUT", "/api/albums/alb-3/assets")
            .with_status(200)
            .with_body(format!(r#"[{{"id":"{ID}","success":false,"error":"no_permission"}}]"#))
            .create();
        let r = Immich::new(&format!("{}/api", server.url()), "k", Some("alb-3")).add_to_album(ID);
        assert!(matches!(r, Err(Failure::Permanent(m)) if m.contains("no_permission")));

        assert_eq!(Immich::new("http://127.0.0.1:9/api", "k", None).add_to_album(ID), Ok(()), "no album: no call");
    }

    #[test]
    fn whoami_reports_owner_or_bad_key() {
        let mut server = mockito::Server::new();
        server
            .mock("GET", "/api/users/me")
            .match_header("x-api-key", "good")
            .with_status(200)
            .with_body(r#"{"id":"u","name":"Booth","email":"booth@example.com","isAdmin":false}"#)
            .create();
        server.mock("GET", "/api/users/me").match_header("x-api-key", "bad").with_status(401).create();
        let base = format!("{}/api", server.url());
        assert_eq!(Immich::new(&base, "good", None).whoami(), Ok("Booth <booth@example.com>".into()));
        assert!(matches!(Immich::new(&base, "bad", None).whoami(), Err(Failure::Permanent(_))));
    }
}
```

Run: `cargo test upload::immich` — Expected: FAIL.

- [ ] **Step 3: Implement**

`v2/src/upload/immich.rs`:
```rust
//! Minimal Immich API client (blocking; used from the upload thread only).
//! Field names follow the server's OpenAPI spec: `deviceAssetId`/`deviceId`
//! are required up to Immich 2.x and ignored by 3.x, so they are always sent.

use std::path::Path;
use std::time::Duration;

use serde::Deserialize;
use ureq::Agent;
use ureq::tls::{RootCerts, TlsConfig};
use ureq::unversioned::multipart::{Form, Part};

const DEVICE_ID: &str = "piBooth";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// A 1.5 MP JPEG is ~0.3 MB; generous for slow Wi-Fi.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// Why a call failed, and whether trying again later can help.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// Network trouble, timeouts, 408/429 and 5xx: retry with backoff.
    Retry(String),
    /// Wrong key, missing permission, unknown album, rejected file: needs a
    /// human; retried rarely.
    Permanent(String),
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Failure::Retry(m) => write!(f, "temporary: {m}"),
            Failure::Permanent(m) => write!(f, "needs attention: {m}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Uploaded {
    pub id: String,
    /// The server already had this photo.
    pub duplicate: bool,
}

pub struct Immich {
    agent: Agent,
    /// e.g. `https://immich.example/api` (no trailing slash).
    base: String,
    key: String,
    album: Option<String>,
}

#[derive(Deserialize)]
struct UploadResponse {
    id: String,
    status: String,
}

#[derive(Deserialize)]
struct BulkIdResponse {
    success: bool,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Deserialize)]
struct Me {
    #[serde(default)]
    name: String,
    #[serde(default)]
    email: String,
}

/// HTTP status → failure kind.
pub fn classify(status: u16, body: &str) -> Failure {
    let msg = format!("HTTP {status}: {}", body.chars().take(200).collect::<String>().trim());
    match status {
        408 | 429 | 500..=599 => Failure::Retry(msg),
        _ => Failure::Permanent(msg),
    }
}

impl Immich {
    /// `base_url` is the API root as in v1's `.env` (`…/api`).
    pub fn new(base_url: &str, key: &str, album: Option<&str>) -> Self {
        let agent = Agent::config_builder()
            .timeout_connect(Some(CONNECT_TIMEOUT))
            .timeout_global(Some(REQUEST_TIMEOUT))
            .http_status_as_error(false)
            // The Pi's certificate store: works with a private CA too.
            .tls_config(TlsConfig::builder().root_certs(RootCerts::PlatformVerifier).build())
            .build()
            .into();
        Self {
            agent,
            base: base_url.trim_end_matches('/').to_string(),
            key: key.to_string(),
            album: album.filter(|a| !a.is_empty()).map(str::to_string),
        }
    }

    pub fn album(&self) -> Option<&str> {
        self.album.as_deref()
    }

    /// Check the key: returns "name <email>" of the key's owner.
    pub fn whoami(&self) -> Result<String, Failure> {
        let mut resp = self
            .agent
            .get(format!("{}/users/me", self.base))
            .header("x-api-key", &self.key)
            .header("accept", "application/json")
            .call()
            .map_err(transport)?;
        let status = resp.status().as_u16();
        if status != 200 {
            return Err(classify(status, &resp.body_mut().read_to_string().unwrap_or_default()));
        }
        let me: Me = resp.body_mut().read_json().map_err(|e| Failure::Retry(format!("bad response: {e}")))?;
        Ok(format!("{} <{}>", me.name, me.email))
    }

    /// Upload one JPEG. `taken` is RFC 3339 (fileCreatedAt/fileModifiedAt).
    pub fn upload(&self, photo: &Path, taken: &str) -> Result<Uploaded, Failure> {
        let name = photo.file_name().and_then(|n| n.to_str()).unwrap_or("photo.jpg");
        let part = Part::file(photo)
            .map_err(|e| Failure::Permanent(format!("cannot read {}: {e}", photo.display())))?
            .file_name(name)
            .mime_str("image/jpeg")
            .map_err(|e| Failure::Permanent(format!("mime: {e}")))?;
        let form = Form::new()
            .text("deviceAssetId", name)
            .text("deviceId", DEVICE_ID)
            .text("fileCreatedAt", taken)
            .text("fileModifiedAt", taken)
            .text("filename", name)
            .text("isFavorite", "false")
            .part("assetData", part);
        let mut resp = self
            .agent
            .post(format!("{}/assets", self.base))
            .header("x-api-key", &self.key)
            .header("accept", "application/json")
            .send(form)
            .map_err(transport)?;
        let status = resp.status().as_u16();
        if status != 200 && status != 201 {
            return Err(classify(status, &resp.body_mut().read_to_string().unwrap_or_default()));
        }
        let r: UploadResponse =
            resp.body_mut().read_json().map_err(|e| Failure::Retry(format!("bad upload response: {e}")))?;
        Ok(Uploaded { id: r.id, duplicate: r.status == "duplicate" })
    }

    /// Add an uploaded asset to the configured album (no-op without one).
    /// An asset already in the album counts as success.
    pub fn add_to_album(&self, asset_id: &str) -> Result<(), Failure> {
        let Some(album) = &self.album else { return Ok(()) };
        let mut resp = self
            .agent
            .put(format!("{}/albums/{album}/assets", self.base))
            .header("x-api-key", &self.key)
            .header("accept", "application/json")
            .send_json(serde_json::json!({ "ids": [asset_id] }))
            .map_err(transport)?;
        let status = resp.status().as_u16();
        if status != 200 {
            return Err(classify(status, &resp.body_mut().read_to_string().unwrap_or_default()));
        }
        let results: Vec<BulkIdResponse> =
            resp.body_mut().read_json().map_err(|e| Failure::Retry(format!("bad album response: {e}")))?;
        match results.first() {
            Some(r) if r.success || r.error.as_deref() == Some("duplicate") => Ok(()),
            Some(r) => Err(Failure::Permanent(format!(
                "album {album} refused asset: {}",
                r.error.as_deref().unwrap_or("unknown")
            ))),
            None => Err(Failure::Retry("empty album response".into())),
        }
    }
}

fn transport(e: ureq::Error) -> Failure {
    Failure::Retry(format!("network: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use mockito::Matcher;

    const ID: &str = "4c1d8f6e-0b2a-4f6e-9a7c-2d4e6f8a0b1c";

    fn photo() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("booth_20261119-185907.jpg");
        std::fs::write(&p, b"\xFF\xD8fake-jpeg\xFF\xD9").unwrap();
        (dir, p)
    }

    #[test]
    fn upload_sends_v1_compatible_form_and_parses_created() {
        let mut server = mockito::Server::new();
        let m = server
            .mock("POST", "/api/assets")
            .match_header("x-api-key", "secret")
            .match_header("content-type", Matcher::Regex("multipart/form-data; boundary=".into()))
            .match_body(Matcher::AllOf(vec![
                Matcher::Regex(r#"name="deviceAssetId"\r\n\r\nbooth_20261119-185907.jpg"#.into()),
                Matcher::Regex(r#"name="deviceId"\r\n\r\npiBooth"#.into()),
                Matcher::Regex(r#"name="fileCreatedAt"\r\n\r\n2026-11-19T17:59:07.818Z"#.into()),
                Matcher::Regex(r#"name="fileModifiedAt"\r\n\r\n2026-11-19T17:59:07.818Z"#.into()),
                Matcher::Regex(r#"name="assetData"; filename="booth_20261119-185907.jpg""#.into()),
                Matcher::Regex("fake-jpeg".into()),
            ]))
            .with_status(201)
            .with_header("content-type", "application/json")
            .with_body(format!(r#"{{"id":"{ID}","status":"created"}}"#))
            .create();
        let (_d, p) = photo();
        let c = Immich::new(&format!("{}/api/", server.url()), "secret", None);
        assert_eq!(c.upload(&p, "2026-11-19T17:59:07.818Z"), Ok(Uploaded { id: ID.into(), duplicate: false }));
        m.assert();
    }

    #[test]
    fn duplicate_is_success() {
        let mut server = mockito::Server::new();
        server
            .mock("POST", "/api/assets")
            .with_status(200)
            .with_body(format!(r#"{{"id":"{ID}","status":"duplicate"}}"#))
            .create();
        let (_d, p) = photo();
        let c = Immich::new(&format!("{}/api", server.url()), "k", None);
        assert!(c.upload(&p, "t").unwrap().duplicate);
    }

    #[test]
    fn errors_are_classified() {
        let mut server = mockito::Server::new();
        server.mock("POST", "/api/assets").with_status(401).with_body("Invalid API key").create();
        let (_d, p) = photo();
        let c = Immich::new(&format!("{}/api", server.url()), "bad", None);
        assert!(
            matches!(c.upload(&p, "t"), Err(Failure::Permanent(m)) if m.contains("401") && m.contains("Invalid API key"))
        );
        assert!(matches!(classify(503, ""), Failure::Retry(_)));
        assert!(matches!(classify(429, ""), Failure::Retry(_)));
        assert!(matches!(classify(404, ""), Failure::Permanent(_)));
        // Nothing listening: a network failure, retried.
        let dead = Immich::new("http://127.0.0.1:9/api", "k", None);
        assert!(matches!(dead.upload(&p, "t"), Err(Failure::Retry(_))));
        // Missing file: no point retrying.
        assert!(matches!(c.upload(Path::new("/nonexistent.jpg"), "t"), Err(Failure::Permanent(_))));
    }

    #[test]
    fn album_add_success_duplicate_and_refusal() {
        let mut server = mockito::Server::new();
        let ok = server
            .mock("PUT", "/api/albums/alb-1/assets")
            .match_header("x-api-key", "k")
            .match_body(Matcher::Json(serde_json::json!({ "ids": [ID] })))
            .with_status(200)
            .with_body(format!(r#"[{{"id":"{ID}","success":true}}]"#))
            .expect(1)
            .create();
        let c = Immich::new(&format!("{}/api", server.url()), "k", Some("alb-1"));
        assert_eq!(c.add_to_album(ID), Ok(()));
        ok.assert();

        server
            .mock("PUT", "/api/albums/alb-2/assets")
            .with_status(200)
            .with_body(format!(r#"[{{"id":"{ID}","success":false,"error":"duplicate"}}]"#))
            .create();
        assert_eq!(Immich::new(&format!("{}/api", server.url()), "k", Some("alb-2")).add_to_album(ID), Ok(()));

        server
            .mock("PUT", "/api/albums/alb-3/assets")
            .with_status(200)
            .with_body(format!(r#"[{{"id":"{ID}","success":false,"error":"no_permission"}}]"#))
            .create();
        let r = Immich::new(&format!("{}/api", server.url()), "k", Some("alb-3")).add_to_album(ID);
        assert!(matches!(r, Err(Failure::Permanent(m)) if m.contains("no_permission")));

        assert_eq!(Immich::new("http://127.0.0.1:9/api", "k", None).add_to_album(ID), Ok(()), "no album: no call");
    }

    #[test]
    fn whoami_reports_owner_or_bad_key() {
        let mut server = mockito::Server::new();
        server
            .mock("GET", "/api/users/me")
            .match_header("x-api-key", "good")
            .with_status(200)
            .with_body(r#"{"id":"u","name":"Booth","email":"booth@example.com","isAdmin":false}"#)
            .create();
        server.mock("GET", "/api/users/me").match_header("x-api-key", "bad").with_status(401).create();
        let base = format!("{}/api", server.url());
        assert_eq!(Immich::new(&base, "good", None).whoami(), Ok("Booth <booth@example.com>".into()));
        assert!(matches!(Immich::new(&base, "bad", None).whoami(), Err(Failure::Permanent(_))));
    }
}
```

- [ ] **Step 4: Verify**

Run: `cargo test upload:: && cargo clippy --all-targets -- -D warnings && cargo fmt --check` — Expected: 10 under `upload::` (5 queue + 5 immich), clean.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock src/upload
git commit -m "feat(v2): Immich client for upload and album"
```

### Task 3: Upload thread

**Files:**
- Create: `v2/src/upload/uploader.rs`
- Modify: `v2/src/upload/mod.rs` (final content below)
- Test: inline (real thread against `mockito`)

**Interfaces:**
- Consumes: `Queue`, `Entry` (Task 1); `Immich`, `Failure` (Task 2); `camera::Backoff`.
- Produces: `pub struct Pacing { retry_initial, retry_max, permanent_retry, idle_rescan }` (`Default` = 5 s, 600 s, 600 s, 30 s); `pub struct UploadStatus { pub pending: usize, pub last_error: Option<String> }`; `pub struct Uploader` with `start(Arc<Queue>, Immich, Pacing) -> anyhow::Result<Self>`, `wake(&self)`, `status(&self) -> UploadStatus`; dropping it stops the thread without joining.

- [ ] **Step 1: Write the failing tests**

`v2/src/upload/mod.rs`:
```rust
//! Uploading saved photos to Immich (spec §4.6).

pub mod immich;
pub mod queue;
pub mod uploader;
```

Create `uploader.rs` with the Interfaces items stubbed `todo!()`, plus:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    const ID: &str = "4c1d8f6e-0b2a-4f6e-9a7c-2d4e6f8a0b1c";
    const MS: Duration = Duration::from_millis(1);

    fn fast() -> Pacing {
        Pacing { retry_initial: 20 * MS, retry_max: 80 * MS, permanent_retry: 200 * MS, idle_rescan: 50 * MS }
    }

    fn setup(n: usize) -> (tempfile::TempDir, Arc<Queue>) {
        let dir = tempfile::tempdir().unwrap();
        let q = Arc::new(Queue::open(dir.path()).unwrap());
        for i in 0..n {
            let p = dir.path().join(format!("booth_{i}.jpg"));
            std::fs::write(&p, b"jpeg").unwrap();
            q.enqueue(&p, "2026-11-19T17:59:07.818Z").unwrap();
        }
        (dir, q)
    }

    fn wait_until(mut f: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !f() {
            assert!(Instant::now() < deadline, "condition not reached");
            std::thread::sleep(10 * MS);
        }
    }

    #[test]
    fn drains_queue_into_album() {
        let mut server = mockito::Server::new();
        server.mock("GET", "/api/users/me").with_status(200).with_body(r#"{"name":"B","email":"b@x"}"#).create();
        let up = server
            .mock("POST", "/api/assets")
            .with_status(201)
            .with_body(format!(r#"{{"id":"{ID}","status":"created"}}"#))
            .expect(3)
            .create();
        let album = server
            .mock("PUT", "/api/albums/alb/assets")
            .with_status(200)
            .with_body(format!(r#"[{{"id":"{ID}","success":true}}]"#))
            .expect(3)
            .create();
        let (_dir, q) = setup(3);
        let u = Uploader::start(q.clone(), Immich::new(&format!("{}/api", server.url()), "k", Some("alb")), fast())
            .unwrap();
        wait_until(|| q.is_empty() && u.status().pending == 0);
        up.assert();
        album.assert();
        assert_eq!(u.status().last_error, None);
    }

    #[test]
    fn offline_keeps_photos_and_records_attempts() {
        let (_dir, q) = setup(1);
        let u = Uploader::start(q.clone(), Immich::new("http://127.0.0.1:9/api", "k", None), fast()).unwrap();
        wait_until(|| q.pending()[0].1.attempts >= 2);
        let s = u.status();
        assert_eq!(s.pending, 1);
        assert!(s.last_error.unwrap().contains("network"));
        assert_eq!(q.len(), 1, "never deleted while failing");
    }

    #[test]
    fn album_failure_does_not_reupload() {
        let mut server = mockito::Server::new();
        let up = server
            .mock("POST", "/api/assets")
            .with_status(201)
            .with_body(format!(r#"{{"id":"{ID}","status":"created"}}"#))
            .expect(1)
            .create();
        server.mock("PUT", "/api/albums/alb/assets").with_status(503).create();
        let (_dir, q) = setup(1);
        let _u = Uploader::start(q.clone(), Immich::new(&format!("{}/api", server.url()), "k", Some("alb")), fast())
            .unwrap();
        wait_until(|| q.pending()[0].1.attempts >= 3);
        assert_eq!(q.pending()[0].1.asset_id.as_deref(), Some(ID));
        up.assert(); // uploaded exactly once despite 3 album retries
    }

    #[test]
    fn missing_photo_is_dropped_and_wake_triggers_upload() {
        let mut server = mockito::Server::new();
        let up = server
            .mock("POST", "/api/assets")
            .with_status(201)
            .with_body(format!(r#"{{"id":"{ID}","status":"created"}}"#))
            .expect(1)
            .create();
        let (dir, q) = setup(1);
        std::fs::remove_file(dir.path().join("booth_0.jpg")).unwrap();
        let slow = Pacing { idle_rescan: Duration::from_secs(60), ..fast() };
        let u = Uploader::start(q.clone(), Immich::new(&format!("{}/api", server.url()), "k", None), slow).unwrap();
        wait_until(|| q.is_empty());
        let p = dir.path().join("booth_9.jpg");
        std::fs::write(&p, b"jpeg").unwrap();
        q.enqueue(&p, "t").unwrap();
        u.wake(); // without this the next scan would be 60 s away
        wait_until(|| q.is_empty());
        up.assert();
    }
}
```

Run: `cargo test upload::uploader` — Expected: FAIL.

- [ ] **Step 2: Implement**

`v2/src/upload/uploader.rs`:
```rust
//! The upload thread: drains the queue oldest first, one photo at a time,
//! with backoff on failures. It never blocks the booth; at quit it is
//! detached (an interrupted upload is simply retried at the next start, and
//! the server reports it as a duplicate if it had already arrived).

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::immich::{Failure, Immich};
use super::queue::{Entry, Queue};
use crate::camera::Backoff;

/// Upload timings. Tests shorten them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pacing {
    pub retry_initial: Duration,
    pub retry_max: Duration,
    /// Wait after an error that needs a human (bad key, unknown album).
    pub permanent_retry: Duration,
    /// Re-scan the queue this often even without a wake-up.
    pub idle_rescan: Duration,
}

impl Default for Pacing {
    fn default() -> Self {
        Self {
            retry_initial: Duration::from_secs(5),
            retry_max: Duration::from_secs(600),
            permanent_retry: Duration::from_secs(600),
            idle_rescan: Duration::from_secs(30),
        }
    }
}

/// What the booth can show about uploads.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UploadStatus {
    pub pending: usize,
    /// The last failure, cleared by the next success.
    pub last_error: Option<String>,
}

pub struct Uploader {
    wake: Sender<()>,
    running: Arc<AtomicBool>,
    pending: Arc<AtomicUsize>,
    last_error: Arc<Mutex<Option<String>>>,
}

impl Uploader {
    pub fn start(queue: Arc<Queue>, client: Immich, pacing: Pacing) -> anyhow::Result<Self> {
        let (wake, wake_rx) = channel();
        let running = Arc::new(AtomicBool::new(true));
        let pending = Arc::new(AtomicUsize::new(queue.len()));
        let last_error = Arc::new(Mutex::new(None));
        let (r, p, e) = (running.clone(), pending.clone(), last_error.clone());
        std::thread::Builder::new().name("uploader".into()).spawn(move || {
            run(&queue, &client, pacing, &wake_rx, &r, &p, &e);
        })?;
        Ok(Self { wake, running, pending, last_error })
    }

    /// A photo was just queued: look now instead of at the next rescan.
    pub fn wake(&self) {
        let _ = self.wake.send(());
    }

    pub fn status(&self) -> UploadStatus {
        UploadStatus {
            pending: self.pending.load(Ordering::SeqCst),
            last_error: self.last_error.lock().unwrap().clone(),
        }
    }
}

impl Drop for Uploader {
    fn drop(&mut self) {
        self.running.store(false, Ordering::SeqCst);
        let _ = self.wake.send(()); // end any wait; the thread is detached
    }
}

/// Wait up to `d`; returns false when the uploader is shutting down.
fn wait(wake: &Receiver<()>, running: &AtomicBool, d: Duration) -> bool {
    match wake.recv_timeout(d) {
        Ok(()) | Err(RecvTimeoutError::Timeout) => running.load(Ordering::SeqCst),
        Err(RecvTimeoutError::Disconnected) => false,
    }
}

fn run(
    queue: &Queue,
    client: &Immich,
    pacing: Pacing,
    wake: &Receiver<()>,
    running: &AtomicBool,
    pending: &AtomicUsize,
    last_error: &Mutex<Option<String>>,
) {
    match client.whoami() {
        Ok(who) => tracing::info!(user = %who, album = client.album().unwrap_or("-"), "Immich OK"),
        Err(e) => tracing::warn!("Immich check failed ({e}); photos stay queued and are retried"),
    }
    let mut backoff = Backoff::new(pacing.retry_initial, pacing.retry_max);
    while running.load(Ordering::SeqCst) {
        let entries = queue.pending();
        pending.store(entries.len(), Ordering::SeqCst);
        let mut delay = None;
        for (file, mut entry) in entries {
            if !running.load(Ordering::SeqCst) {
                return;
            }
            match send(client, queue, &file, &mut entry) {
                Ok(()) => {
                    backoff.reset();
                    *last_error.lock().unwrap() = None;
                    match queue.done(&file) {
                        Ok(()) => {
                            pending.fetch_sub(1, Ordering::SeqCst);
                        }
                        Err(e) => tracing::error!("{e:#}"),
                    }
                }
                Err(failure) => {
                    entry.attempts += 1;
                    entry.last_error = Some(failure.to_string());
                    if let Err(e) = queue.update(&file, &entry) {
                        tracing::error!("{e:#}");
                    }
                    let d = match &failure {
                        Failure::Retry(_) => backoff.next_delay(),
                        Failure::Permanent(_) => pacing.permanent_retry,
                    };
                    tracing::warn!(photo = %entry.photo.display(), attempts = entry.attempts, "upload failed ({failure}); retry in {d:?}");
                    *last_error.lock().unwrap() = Some(failure.to_string());
                    delay = Some(d);
                    break; // keep order: retry the oldest photo first
                }
            }
        }
        if !wait(wake, running, delay.unwrap_or(pacing.idle_rescan)) {
            return;
        }
    }
}

/// Upload (if not done yet) and add to the album, recording progress so a
/// retry never uploads the same photo twice.
fn send(client: &Immich, queue: &Queue, file: &std::path::Path, entry: &mut Entry) -> Result<(), Failure> {
    if !entry.photo.exists() {
        tracing::warn!(photo = %entry.photo.display(), "queued photo no longer exists; dropping it from the queue");
        return Ok(());
    }
    if entry.asset_id.is_none() {
        let up = client.upload(&entry.photo, &entry.taken)?;
        tracing::info!(photo = %entry.photo.display(), id = %up.id, duplicate = up.duplicate, "uploaded");
        entry.asset_id = Some(up.id);
        if let Err(e) = queue.update(file, entry) {
            tracing::error!("{e:#}");
        }
    }
    client.add_to_album(entry.asset_id.as_deref().expect("set above"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    const ID: &str = "4c1d8f6e-0b2a-4f6e-9a7c-2d4e6f8a0b1c";
    const MS: Duration = Duration::from_millis(1);

    fn fast() -> Pacing {
        Pacing { retry_initial: 20 * MS, retry_max: 80 * MS, permanent_retry: 200 * MS, idle_rescan: 50 * MS }
    }

    fn setup(n: usize) -> (tempfile::TempDir, Arc<Queue>) {
        let dir = tempfile::tempdir().unwrap();
        let q = Arc::new(Queue::open(dir.path()).unwrap());
        for i in 0..n {
            let p = dir.path().join(format!("booth_{i}.jpg"));
            std::fs::write(&p, b"jpeg").unwrap();
            q.enqueue(&p, "2026-11-19T17:59:07.818Z").unwrap();
        }
        (dir, q)
    }

    fn wait_until(mut f: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !f() {
            assert!(Instant::now() < deadline, "condition not reached");
            std::thread::sleep(10 * MS);
        }
    }

    #[test]
    fn drains_queue_into_album() {
        let mut server = mockito::Server::new();
        server.mock("GET", "/api/users/me").with_status(200).with_body(r#"{"name":"B","email":"b@x"}"#).create();
        let up = server
            .mock("POST", "/api/assets")
            .with_status(201)
            .with_body(format!(r#"{{"id":"{ID}","status":"created"}}"#))
            .expect(3)
            .create();
        let album = server
            .mock("PUT", "/api/albums/alb/assets")
            .with_status(200)
            .with_body(format!(r#"[{{"id":"{ID}","success":true}}]"#))
            .expect(3)
            .create();
        let (_dir, q) = setup(3);
        let u = Uploader::start(q.clone(), Immich::new(&format!("{}/api", server.url()), "k", Some("alb")), fast())
            .unwrap();
        wait_until(|| q.is_empty() && u.status().pending == 0);
        up.assert();
        album.assert();
        assert_eq!(u.status().last_error, None);
    }

    #[test]
    fn offline_keeps_photos_and_records_attempts() {
        let (_dir, q) = setup(1);
        let u = Uploader::start(q.clone(), Immich::new("http://127.0.0.1:9/api", "k", None), fast()).unwrap();
        wait_until(|| q.pending()[0].1.attempts >= 2);
        let s = u.status();
        assert_eq!(s.pending, 1);
        assert!(s.last_error.unwrap().contains("network"));
        assert_eq!(q.len(), 1, "never deleted while failing");
    }

    #[test]
    fn album_failure_does_not_reupload() {
        let mut server = mockito::Server::new();
        let up = server
            .mock("POST", "/api/assets")
            .with_status(201)
            .with_body(format!(r#"{{"id":"{ID}","status":"created"}}"#))
            .expect(1)
            .create();
        server.mock("PUT", "/api/albums/alb/assets").with_status(503).create();
        let (_dir, q) = setup(1);
        let _u = Uploader::start(q.clone(), Immich::new(&format!("{}/api", server.url()), "k", Some("alb")), fast())
            .unwrap();
        wait_until(|| q.pending()[0].1.attempts >= 3);
        assert_eq!(q.pending()[0].1.asset_id.as_deref(), Some(ID));
        up.assert(); // uploaded exactly once despite 3 album retries
    }

    #[test]
    fn missing_photo_is_dropped_and_wake_triggers_upload() {
        let mut server = mockito::Server::new();
        let up = server
            .mock("POST", "/api/assets")
            .with_status(201)
            .with_body(format!(r#"{{"id":"{ID}","status":"created"}}"#))
            .expect(1)
            .create();
        let (dir, q) = setup(1);
        std::fs::remove_file(dir.path().join("booth_0.jpg")).unwrap();
        let slow = Pacing { idle_rescan: Duration::from_secs(60), ..fast() };
        let u = Uploader::start(q.clone(), Immich::new(&format!("{}/api", server.url()), "k", None), slow).unwrap();
        wait_until(|| q.is_empty());
        let p = dir.path().join("booth_9.jpg");
        std::fs::write(&p, b"jpeg").unwrap();
        q.enqueue(&p, "t").unwrap();
        u.wake(); // without this the next scan would be 60 s away
        wait_until(|| q.is_empty());
        up.assert();
    }
}
```

- [ ] **Step 3: Verify (three runs — real threads)**

Run: `cargo fmt --check && cargo clippy --all-targets -- -D warnings && for i in 1 2 3; do cargo test upload:: || break; done` — Expected: 14 under `upload::` each time, clean.

- [ ] **Step 4: Commit**

```bash
git add src/upload
git commit -m "feat(v2): background upload thread with retries"
```

### Task 4: Wire uploads into the booth

**Files (replace whole files):** `v2/src/pipeline/worker.rs`, `v2/src/display/controller.rs`, `v2/src/osd/scene.rs`, `v2/src/display/app.rs`
**Create:** `v2/tools/fake_immich.py`

**Interfaces:**
- `Worker::start(storage, effects, queue: Option<Arc<Queue>>)` — enqueues each saved photo (`taken_utc(job.info.taken)`) before reporting the result.
- `Controller`: opens the queue at start, starts an `Uploader` when `cfg.upload_enabled()` (album = `immich.album_id`), wakes it after each saved photo; `upload_status(&self) -> (Option<UploadStatus>, usize)` (None = upload off; usize = photos waiting).
- `DebugInfo` gains `upload: Option<Option<String>>` and `upload_pending: usize` (no longer `Copy`); line after "Photo count": `Upload queue: N`, `Upload queue: N (<first 40 chars of the last error>)` or `Upload: off (N queued)`.

- [ ] **Step 1: Worker (test-first)**

Add the test `saved_photos_are_queued_for_upload` (from the file below) and update the existing worker tests to pass `None` as the third `Worker::start` argument; see it fail; then replace `v2/src/pipeline/worker.rs` with:
```rust
//! Runs photo jobs on a background thread so the preview never stutters.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::JoinHandle;

use super::photo::{PhotoDone, PhotoJob, process};
use crate::osd::Rgba;
use crate::storage::Storage;
use crate::upload::queue::{Queue, taken_utc};

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
    /// `queue`: where saved photos are queued for upload (spec §4.4 step 6).
    /// Queuing here, right after the file is written, means a result that
    /// arrives late or is dropped at quit is still uploaded.
    pub fn start(storage: Storage, effects: Vec<Rgba>, queue: Option<Arc<Queue>>) -> anyhow::Result<Self> {
        let (job_tx, job_rx) = channel::<PhotoJob>();
        let (res_tx, res_rx) = channel();
        let thread = std::thread::Builder::new().name("photo-worker".into()).spawn(move || {
            for job in job_rx {
                let id = job.id;
                let taken = job.info.taken;
                let started = std::time::Instant::now();
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    process(job, &storage, &effects, |n| fastrand::usize(..n))
                }))
                .unwrap_or_else(|payload| {
                    let msg = payload
                        .downcast_ref::<&str>()
                        .map(|s| s.to_string())
                        .or_else(|| payload.downcast_ref::<String>().cloned())
                        .unwrap_or_else(|| "unknown panic".to_string());
                    Err(anyhow::anyhow!("photo processing panicked: {msg}"))
                })
                .map_err(|e| format!("{e:#}"));
                match &outcome {
                    Ok(d) => tracing::info!(
                        id,
                        photo = %d.photo.display(),
                        ms = started.elapsed().as_millis() as u64,
                        encode_original_ms = d.timings.encode_original_ms,
                        write_original_ms = d.timings.write_original_ms,
                        filter_ms = d.timings.filter_ms,
                        encode_photo_ms = d.timings.encode_photo_ms,
                        write_photo_ms = d.timings.write_photo_ms,
                        "photo saved"
                    ),
                    Err(e) => tracing::error!(id, "photo failed: {e}"),
                }
                if let (Ok(done), Some(q)) = (&outcome, &queue)
                    && let Err(e) = q.enqueue(&done.photo, &taken_utc(taken))
                {
                    tracing::error!(id, "could not queue photo for upload: {e:#}");
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
        let w = Worker::start(Storage::create(dir.path()).unwrap(), Vec::new(), None).unwrap();
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
        let w = Worker::start(storage, Vec::new(), None).unwrap();
        w.submit(job(3));
        let r = wait(&w);
        assert_eq!(r.id, 3);
        assert!(r.outcome.unwrap_err().contains("photos"));
    }

    fn effect_8x4() -> Rgba {
        Rgba { width: 8, height: 4, data: vec![255; 8 * 4 * 4] }
    }

    #[test]
    fn panicking_job_is_reported_and_worker_survives() {
        let dir = tempfile::tempdir().unwrap();
        let w = Worker::start(Storage::create(dir.path()).unwrap(), vec![effect_8x4()], None).unwrap();
        let mut bad = job(4);
        bad.rgb.truncate(5); // frame shorter than width*height*3: composite indexes out of bounds
        bad.save_original = false; // skip the plain encode so the overlay step is reached
        w.submit(bad);
        let r = wait(&w);
        assert_eq!(r.id, 4);
        assert!(r.outcome.unwrap_err().contains("panicked"));
        w.submit(job(5));
        assert!(wait(&w).outcome.is_ok());
    }

    #[test]
    fn saved_photos_are_queued_for_upload() {
        let dir = tempfile::tempdir().unwrap();
        let q = Arc::new(Queue::open(dir.path()).unwrap());
        let w = Worker::start(Storage::create(dir.path()).unwrap(), Vec::new(), Some(q.clone())).unwrap();
        w.submit(job(7));
        let r = wait(&w);
        let photo = r.outcome.unwrap().photo;
        let pending = q.pending();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].1.photo, photo.canonicalize().unwrap());
        assert!(pending[0].1.taken.ends_with('Z'));
    }
}
```

- [ ] **Step 2: Controller**

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
use crate::upload::immich::Immich;
use crate::upload::queue::Queue;
use crate::upload::uploader::{Pacing, UploadStatus, Uploader};

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
    queue: Arc<Queue>,
    uploader: Option<Uploader>,
    photo: PhotoSettings,
    review_image: Option<Rgba>,
    photo_count: usize,
    pub debug: bool,
    pub filter: bool,
}

impl Drop for Controller {
    /// Wait for a pending camera stop so libcamera isn't torn down mid-stop
    /// at quit. A pending restart thread is not waited for.
    fn drop(&mut self) {
        if let Some(h) = self.stopping.take() {
            let _ = h.join();
        }
    }
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
        // Photos are queued even when upload is not configured, so they go
        // up once it is (spec §4.8).
        let queue = Arc::new(Queue::open(&cfg.storage.root)?);
        let uploader = match (&cfg.immich_api_key, cfg.upload_enabled()) {
            (Some(key), true) => {
                let album = Some(cfg.immich.album_id.as_str());
                Some(Uploader::start(queue.clone(), Immich::new(&cfg.immich.base_url, key, album), Pacing::default())?)
            }
            _ => {
                tracing::info!(queued = queue.len(), "upload off (no Immich key or base_url); photos stay queued");
                None
            }
        };
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
            worker: Worker::start(storage, effects, Some(queue.clone()))?,
            queue,
            uploader,
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

    /// Upload state for the debug overlay: `None` when upload is off, with
    /// the number of photos waiting either way.
    pub fn upload_status(&self) -> (Option<UploadStatus>, usize) {
        match &self.uploader {
            Some(u) => {
                let s = u.status();
                let n = s.pending;
                (Some(s), n)
            }
            None => (None, self.queue.len()),
        }
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
            if !matches!(self.booth.phase(), Phase::Capturing { id: p, .. } if p == id) {
                tracing::warn!(id, "unexpected captured frame ignored");
                continue;
            }
            let cmds = self.booth.handle(Event::FrameCaptured { id }, now);
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
                    // Only the photo being processed may reach the review screen.
                    if matches!(self.booth.phase(), Phase::Processing { id, .. } if id == result.id) {
                        self.review_image = Some(done.review);
                    }
                    if let Some(u) = &self.uploader {
                        u.wake(); // the worker queued it just before reporting
                    }
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

    fn controller(dir: &tempfile::TempDir, f: CameraFactory, now: Instant) -> (Controller, mpsc::Sender<Action>) {
        let (tx, rx) = mpsc::channel();
        (Controller::start_with(&cfg(dir.path()), f, None, rx, fast(), now).unwrap(), tx)
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
        let (_c, _tx) = controller(&dir, factory(Arc::new(AtomicUsize::new(0)), usize::MAX), Instant::now());
        assert!(dir.path().join("photos").is_dir() && dir.path().join("raw").is_dir());
    }

    #[test]
    fn button_press_takes_saves_and_reviews_a_photo() {
        let dir = tempfile::tempdir().unwrap();
        let (mut c, tx) = controller(&dir, factory(Arc::new(AtomicUsize::new(0)), usize::MAX), Instant::now());
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

    /// A fake camera that delivers every capture twice (same id).
    struct DuplicatingCamera {
        inner: FakeCamera,
        captures: Option<mpsc::Sender<CapturedFrame>>,
    }
    impl Camera for DuplicatingCamera {
        fn start(&mut self, link: CameraLink) -> anyhow::Result<()> {
            self.captures = Some(link.captures.clone());
            self.inner.start(link)
        }
        fn command(&mut self, cmd: CameraCommand) {
            if let (CameraCommand::Capture { id }, Some(tx)) = (cmd, &self.captures) {
                let frame =
                    Frame { width: 64, height: 48, data: vec![0; 64 * 48 * 3], seq: 0, captured_at: Instant::now() };
                let _ = tx.send(CapturedFrame { id, frame, meta: Default::default() });
            }
            self.inner.command(cmd);
        }
        fn stop(&mut self) {
            self.inner.stop();
        }
    }

    #[test]
    fn duplicate_captured_frame_is_processed_once() {
        let dir = tempfile::tempdir().unwrap();
        let f: CameraFactory = Arc::new(|| -> Box<dyn Camera> {
            Box::new(DuplicatingCamera { inner: FakeCamera::new((64, 48), 100), captures: None })
        });
        let (mut c, tx) = controller(&dir, f, Instant::now());
        tx.send(Action::Press).unwrap();
        run_until(&mut c, |c| matches!(c.booth.phase(), Phase::Review { .. }));
        // Let any (wrongly) submitted second job finish and be collected.
        for _ in 0..40 {
            c.update(Instant::now());
            std::thread::sleep(5 * MS);
        }
        assert_eq!(c.photo_count(), 1);
        let photos: Vec<_> = std::fs::read_dir(dir.path().join("photos")).unwrap().collect();
        assert_eq!(photos.len(), 1);
    }

    #[test]
    fn toggles_and_passthrough_actions() {
        let dir = tempfile::tempdir().unwrap();
        let (mut c, _tx) = controller(&dir, factory(Arc::new(AtomicUsize::new(0)), usize::MAX), Instant::now());
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
    fn stalled_camera_shows_banner() {
        let dir = tempfile::tempdir().unwrap();
        let t0 = Instant::now();
        let (mut c, _tx) = controller(&dir, Arc::new(|| -> Box<dyn Camera> { Box::new(SilentCamera) }), t0);
        c.update(t0 + 299 * MS);
        assert!(!c.scene_input(t0, (100, 100)).banner);
        c.update(t0 + 300 * MS);
        assert!(c.scene_input(t0, (100, 100)).banner);
        assert_eq!(c.booth.phase(), Phase::Idle);
    }

    #[test]
    fn press_ignored_while_camera_down() {
        let dir = tempfile::tempdir().unwrap();
        let (mut c, _tx) = controller(&dir, factory(Arc::new(AtomicUsize::new(0)), usize::MAX), Instant::now());
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
        let (mut c, _tx) = controller(&dir, factory(starts.clone(), usize::MAX), Instant::now());
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
        let (mut c, _tx) = controller(&dir, f, Instant::now());
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

- [ ] **Step 3: Debug overlay (test-first)**

Add `upload_line_variants` and the updated `debug_text_formats_values_and_missing` first; see them fail; then replace `v2/src/osd/scene.rs` with:
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
#[derive(Debug, Clone, PartialEq)]
pub struct DebugInfo {
    pub display_fps: f64,
    pub camera_fps: f64,
    pub mirror: bool,
    pub filter: bool,
    pub temp_c: Option<f32>,
    pub disk_pct: Option<f32>,
    pub photo_count: usize,
    /// `None`: upload off. `Some(error)`: on, with the last failure if any.
    pub upload: Option<Option<String>>,
    pub upload_pending: usize,
}

fn upload_line(d: &DebugInfo) -> String {
    match &d.upload {
        None => format!("Upload: off ({} queued)", d.upload_pending),
        Some(None) => format!("Upload queue: {}", d.upload_pending),
        Some(Some(err)) => {
            let short: String = err.chars().take(40).collect();
            format!("Upload queue: {} ({short})", d.upload_pending)
        }
    }
}

/// Debug overlay lines, top to bottom (v1's list plus camera fps).
pub fn debug_text(d: &DebugInfo) -> Vec<String> {
    let opt = |v: Option<f32>, unit: &str| v.map_or("--".to_string(), |v| format!("{v:.1}{unit}"));
    vec![
        format!("fps: {:.0}", d.display_fps),
        format!("Camera fps: {:.0}", d.camera_fps),
        format!("Photo count: {}", d.photo_count),
        upload_line(d),
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
            upload: Some(None),
            upload_pending: 3,
        };
        assert_eq!(
            debug_text(&d),
            vec![
                "fps: 60",
                "Camera fps: 60",
                "Photo count: 12",
                "Upload queue: 3",
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
    fn upload_line_variants() {
        let mut d = DebugInfo {
            display_fps: 60.0,
            camera_fps: 60.0,
            mirror: true,
            filter: true,
            temp_c: None,
            disk_pct: None,
            photo_count: 0,
            upload: None,
            upload_pending: 2,
        };
        assert_eq!(upload_line(&d), "Upload: off (2 queued)");
        d.upload = Some(Some("temporary: network: Connection refused (os error 111)".into()));
        assert_eq!(upload_line(&d), "Upload queue: 2 (temporary: network: Connection refused ()", "first 40 chars");
        d.upload = Some(None);
        assert_eq!(upload_line(&d), "Upload queue: 2");
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
        let (upload, upload_pending) = self.ctrl.upload_status();
        let lines = debug_text(&DebugInfo {
            display_fps: self.display_fps,
            camera_fps: self.camera_fps,
            mirror: gfx.renderer.mirror(),
            filter: self.ctrl.filter,
            temp_c: status.temp_c,
            disk_pct: status.disk_pct,
            photo_count: self.ctrl.photo_count(),
            upload: upload.map(|s| s.last_error),
            upload_pending,
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

- [ ] **Step 4: Fake Immich tool**

`v2/tools/fake_immich.py`:
```python
#!/usr/bin/env python3
"""Fake Immich server for development and end-to-end tests.

Usage: python3 -I tools/fake_immich.py PORT OUT_DIR
Answers GET /api/users/me, POST /api/assets (saves the JPEG to OUT_DIR),
PUT /api/albums/<id>/assets, and logs every request to OUT_DIR/requests.log.
Point the booth at it with [immich] base_url = "http://127.0.0.1:PORT/api".
"""
import http.server, json, sys, os, uuid, re
OUT = sys.argv[2]
os.makedirs(OUT, exist_ok=True)
log = open(os.path.join(OUT, "requests.log"), "a", buffering=1)
class H(http.server.BaseHTTPRequestHandler):
    def _reply(self, code, obj):
        body = json.dumps(obj).encode()
        self.send_response(code); self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(body))); self.end_headers(); self.wfile.write(body)
    def do_GET(self):
        log.write(f"GET {self.path} key={self.headers.get('x-api-key')}\n")
        if self.path == "/api/users/me": return self._reply(200, {"name": "Booth", "email": "booth@example.com"})
        self._reply(404, {})
    def do_POST(self):
        n = int(self.headers.get("content-length", 0)); data = self.rfile.read(n)
        fields = re.findall(rb'name="([^"]+)"(?:; filename="([^"]+)")?\r\n(?:Content-Type: [^\r]+\r\n)?\r\n', data)
        log.write(f"POST {self.path} key={self.headers.get('x-api-key')} bytes={n} fields={[f[0].decode() for f in fields]}\n")
        m = re.search(rb'name="fileCreatedAt"\r\n\r\n([^\r]+)', data); log.write(f"  fileCreatedAt={m.group(1).decode() if m else None}\n")
        i = data.find(b"\xff\xd8"); j = data.rfind(b"\xff\xd9")
        if i >= 0 and j > i: open(os.path.join(OUT, f"upload-{uuid.uuid4().hex[:6]}.jpg"), "wb").write(data[i:j+2])
        self._reply(201, {"id": str(uuid.uuid4()), "status": "created"})
    def do_PUT(self):
        n = int(self.headers.get("content-length", 0)); body = json.loads(self.rfile.read(n) or b"{}")
        log.write(f"PUT {self.path} body={body}\n")
        self._reply(200, [{"id": i, "success": True} for i in body.get("ids", [])])
    def log_message(self, *a): pass
http.server.ThreadingHTTPServer(("127.0.0.1", int(sys.argv[1])), H).serve_forever()
```

- [ ] **Step 5: Verify**

Run: `cargo fmt --check && cargo clippy --all-targets -- -D warnings && for i in 1 2 3; do cargo test || break; done` — Expected: 131 tests pass each run. Where libcamera ≥ 0.5 headers exist: `cargo clippy --all-targets --features libcamera -- -D warnings` clean.

- [ ] **Step 6: End-to-end: offline, then back online**

From the repo root (assets live there), with Xvfb on `:99`, ImageMagick/xdotool installed, `cargo build` done in `v2/`:
```bash
E=/tmp/pb-e2e4 && rm -rf $E && mkdir -p $E
printf '[storage]\nroot = "%s"\n[immich]\nbase_url = "http://127.0.0.1:18283/api"\nalbum_id = "album-1"\nlatitude = 48.8584\nlongitude = 2.2945\n' $E > $E/config.toml
# 1. server down: the shot is queued
(PIBOOTH_IMMICH_API_KEY=testkey NO_COLOR=1 DISPLAY=:99 timeout 18 v2/target/debug/pibooth --config $E/config.toml --fake-camera --no-gpio > $E/run1.log 2>&1) &
sleep 6; W=$(DISPLAY=:99 xdotool search --name piBooth | head -1); DISPLAY=:99 xdotool windowfocus $W key space; sleep 9; DISPLAY=:99 xdotool key Escape; sleep 3
ls $E/queue; grep -E "Immich|upload" $E/run1.log
# 2. server up: the queue drains at startup
python3 -I v2/tools/fake_immich.py 18283 $E/server & sleep 1
PIBOOTH_IMMICH_API_KEY=testkey NO_COLOR=1 DISPLAY=:99 timeout 10 v2/target/debug/pibooth --config $E/config.toml --fake-camera --no-gpio > $E/run2.log 2>&1
ls $E/queue; cat $E/server/requests.log; cmp $E/server/upload-*.jpg $E/photos/*.jpg && echo same; kill %1 2>/dev/null; pkill -f fake_immich.py
```
Expected: after run 1 `queue/booth_….json` exists and the log shows `Immich check failed (temporary: network …)` and `upload failed (temporary …); retry in 5s`. After run 2 the queue is empty; the log shows `Immich OK user=Booth <booth@example.com> album="album-1"` and `uploaded`; `requests.log` shows `GET /api/users/me key=testkey`, one `POST /api/assets` with fields `deviceAssetId, deviceId, fileCreatedAt, fileModifiedAt, filename, isFavorite, assetData` and a `…Z` `fileCreatedAt`, then `PUT /api/albums/album-1/assets`; the uploaded bytes equal the saved photo.

- [ ] **Step 7: Commit**

```bash
git add src/pipeline/worker.rs src/display/controller.rs src/osd/scene.rs src/display/app.rs tools/fake_immich.py
git commit -m "feat(v2): queue saved photos and upload them in the background"
```

### Task 5: Docs and Pi verification

**Files:** `v2/README.md` (replace), `v2/config.example.toml` (comments), `docs/superpowers/specs/2026-11-18-rust-v2-design.md`

- [ ] **Step 1: README**

Replace `v2/README.md` with:
````markdown
# piBooth v2 (Rust)

Rewrite of the Python booth (`../main.py`). Design: `../docs/superpowers/specs/2026-11-18-rust-v2-design.md`.

Status: milestone 3 — live preview, button and countdown, screen flash,
full-quality capture (high-quality noise reduction, exposure metered under the
flash), sepia + effect overlay, JPEG with EXIF, review of the
saved photo, status icons, camera auto-restart, and background upload to
Immich that survives being offline and reboots.

## Photos

Every shot is saved under `storage.root` (default: the working directory):

- `photos/booth_YYYYMMDD-HHMMSS.jpg` — the finished photo (sepia + a random
  overlay from `effects/` when the filter is on, plain otherwise).
- `raw/booth_YYYYMMDD-HHMMSS.jpg` — the unfiltered original
  (`storage.save_original = true`).

Photos are never mirrored, are 1456×1088 (the sensor's full resolution), JPEG
quality `capture.jpeg_quality` (95) with full colour detail, and carry EXIF date,
camera, exposure, ISO and — when `immich.latitude`/`longitude` are set — GPS.

## Immich upload

Every finished photo (from `photos/`, not the `raw/` originals) is queued in
`queue/` right after it is saved and uploaded in the background, oldest first,
then added to the album. Nothing is lost while the booth is offline or
restarted; uploads resume on their own.

Configure (v1's `.env` values move as shown):

| v1 `.env` | v2 |
|---|---|
| `API_KEY` | `PIBOOTH_IMMICH_API_KEY` in the environment or `.env` (`API_KEY` still works) |
| `BASE_URL` | `[immich] base_url` in `config.toml`, e.g. `https://immich.example/api` |
| `ALBUM_ID` | `[immich] album_id` (optional) |
| `latitude` / `longitude` | `[immich] latitude` / `longitude` (written into the photo's EXIF; Immich reads it) |

The API key needs the `asset.upload` and `albumAsset.create` permissions (and
`user.read` for the startup check). At startup the log says `Immich OK user=…`
or why not. Failures retry from 5 s, doubling to 10 min; a bad key or unknown
album is retried every 10 min — fix the config and restart, queued photos go up.
`d` shows the queue length and the last upload error.

Without a key or `base_url`, photos are still queued and go up once upload is
configured. HTTPS uses the Pi's certificate store, so a private CA works once
installed with `update-ca-certificates`; plain `http://` on the LAN works too.

## Develop on a laptop (no Pi needed)

```bash
cargo test
cargo run -- --config config.dev.toml --fake-camera --windowed --no-gpio --bench
```

`config.dev.toml` points `display.assets_dir` at the repo root, where v1's
`fonts/`, `images/` and `effects/` live. Photos taken while developing land in
`v2/photos/` and `v2/raw/`.

Keys: `Space` take a photo, `d` debug overlay (includes the photo count), `r`
mirror preview, `f` filter on/off for the next photos, `Esc`/`q` quit. The red
square marks the image's top-left corner; with mirroring on it appears
top-right.

## Button

Same wiring as v1: a push button between GPIO 27 (BCM) and 3.3 V. v2 enables
the internal pull-down and listens for debounced press edges. For a button to
ground instead, set `[button] active_low = true` (internal pull-up).

## Build on the Raspberry Pi 5

```bash
sudo apt install -y libcamera-dev libclang-dev clang pkg-config cage
pkg-config --modversion libcamera   # must be 0.5.x .. 0.7.x for libcamera crate 0.8 (the Pi ships 0.5.2)
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

- [ ] **Step 2: Example config**

`v2/config.example.toml`: the `[storage] root` comment becomes `# photos/, raw/ and the upload queue/ are created under here`; under `[immich]`: `base_url = ""         # API root incl. /api, e.g. "https://immich.example/api" (empty = upload off)` and `album_id = ""         # optional: album UUID from the album's URL`. `config::tests::example_config_parses` must pass.

- [ ] **Step 3: Spec**

1. §3 diagram: in the upload box replace `(tokio, disk` with `(thread, disk`.
2. §3 "Threads:" paragraph: "a rayon pool for the pipeline" → "a photo-worker thread (a rayon pool for stacking from milestone 5)"; "and a tokio runtime for upload (milestone 4)" → "and an upload thread (blocking `ureq` HTTP client)".
3. §3.1 `upload` row dependencies → "ureq (rustls with ring, system certificate store), serde_json".
4. Replace all of §4.6 with:
```markdown
### 4.6 Upload

- Queue: `queue/<photo stem>.json` per photo — `photo`, `taken` (RFC 3339 UTC,
  milliseconds), `asset_id`, `attempts`, `last_error` — written atomically by the
  photo worker right after the photo is saved (also when upload is off, so photos
  go up once it is configured). Processed oldest first; unreadable entries are
  renamed `*.json.bad`.
- Upload thread (blocking `ureq`, rustls with `ring`, system certificate store),
  started when the API key and `base_url` are set. At start `GET /users/me`
  logs the key's owner or the failure. Per entry: `POST {base_url}/assets`
  (multipart: `deviceAssetId` = file name, `deviceId = "piBooth"`,
  `fileCreatedAt` = `fileModifiedAt` = `taken`, `filename`, `isFavorite = false`,
  `assetData`; the fields Immich 1.x/2.x require are ignored by 3.x). `created`
  and `duplicate` both succeed; the asset id is stored in the entry, then
  `PUT /albums/{album_id}/assets` when `album_id` is set (`success` or error
  `duplicate` ⇒ done).
- Date and GPS reach Immich through the photo's EXIF; v1's separate
  `PUT /assets` location call is dropped.
- Only `photos/` files are uploaded; `raw/` originals stay local.
- Failures: transport errors, timeouts, 408/429/5xx ⇒ retry with backoff 5 s →
  10 min; other 4xx (bad key, missing permission, unknown album) ⇒ "needs
  attention", retried every 10 min. Timeouts: connect 5 s, request 120 s. The
  oldest photo is always retried first.
- An entry is deleted once uploaded and added to the album; a missing photo file
  drops its entry with a warning. Local photos are never deleted by v2. At quit
  the upload thread is detached; an interrupted upload repeats at the next start
  and the server reports a duplicate.
```

- [ ] **Step 4: Verify and commit**

Run `cargo test` (all pass), then:
```bash
git add README.md config.example.toml ../docs/superpowers/specs/2026-11-18-rust-v2-design.md
git commit -m "docs(v2): milestone 4 README and spec updates"
```

- [ ] **Step 5: Verify on the Pi (user)**

Put the key in `.env` (`PIBOOTH_IMMICH_API_KEY=…`), set `[immich] base_url`, `album_id`, `latitude`, `longitude` in the config, build with `-j 2`, run with `--bench`, take a few photos, then:
```bash
grep -E "Immich OK|Immich check|uploaded|upload failed|could not queue" bench.log | tail
ls queue/
```
Expected: `Immich OK user=… album=…` at start; one `uploaded … duplicate=false` per photo within seconds; `queue/` empty. In Immich: the photos appear in the album with the right date and the map location (GPS from EXIF). Then unplug the network, take two photos (`queue/` holds two entries, `d` shows `Upload queue: 2 (temporary: network…)`), reconnect: both upload within ~10 min at most (usually at the next retry). Restart the booth with photos still queued: they upload after the restart.
---

## Spec coverage (milestone 4)

| Spec item | Task |
|---|---|
| §4.6 queue on disk, survives restarts, oldest first | 1, 4 |
| §4.6 upload + album, created/duplicate, retries/backoff, timeouts | 2, 3 |
| §4.6 GPS via EXIF (no separate call) | 2 (by omission), 5 |
| §4.8 upload only with key + base_url; queue filled regardless | 4 |
| §2 v1 defects: blocking HTTP in the render loop, no retry, no offline queue, leaked file handle | 1–4 |

Deferred: burst stacking, DNG, GPIO strobe (M5); dmabuf, hardware trigger, bare KMS (M6).
