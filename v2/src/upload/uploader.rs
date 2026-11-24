//! The upload thread: drains the queue oldest first, one photo at a time,
//! with backoff on failures. It never blocks the booth; at quit it is
//! detached (an interrupted upload is simply retried at the next start, and
//! the server reports it as a duplicate if it had already arrived).

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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
    /// The most recent error of any photo still in the queue (parked photos
    /// keep theirs visible); `None` once everything went through.
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
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run(&queue, &client, pacing, &wake_rx, &r, &p, &e);
            }));
            if let Err(payload) = outcome {
                let msg = payload
                    .downcast_ref::<&str>()
                    .map(|s| (*s).to_string())
                    .or_else(|| payload.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "unknown panic".into());
                tracing::error!("upload thread stopped after a panic: {msg}");
            }
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

/// Idle wait: returns early on a wake-up. Returns false when shutting down.
fn wait_idle(wake: &Receiver<()>, running: &AtomicBool, d: Duration) -> bool {
    let ok = match wake.recv_timeout(d) {
        Ok(()) | Err(RecvTimeoutError::Timeout) => running.load(Ordering::SeqCst),
        Err(RecvTimeoutError::Disconnected) => false,
    };
    wake.try_iter().for_each(drop);
    ok
}

/// Retry wait: sleeps until the deadline, ignoring wake-ups (they only serve
/// to notice shutdown). Returns false when shutting down.
fn wait_retry(wake: &Receiver<()>, running: &AtomicBool, d: Duration) -> bool {
    let deadline = Instant::now() + d;
    loop {
        if !running.load(Ordering::SeqCst) {
            return false;
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        if let Err(RecvTimeoutError::Disconnected) = wake.recv_timeout(left.min(Duration::from_millis(100))) {
            return false;
        }
    }
    wake.try_iter().for_each(drop);
    running.load(Ordering::SeqCst)
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
        let now = unix_now();
        // A Retry/Config failure stops the pass with this delay.
        let mut stop: Option<Duration> = None;
        // Earliest `next_try` among photos skipped or parked in this pass.
        let mut earliest: Option<u64> = None;
        for (file, mut entry) in entries {
            if !running.load(Ordering::SeqCst) {
                return;
            }
            if let Some(t) = entry.next_try.filter(|t| *t > now) {
                earliest = Some(earliest.map_or(t, |e| e.min(t)));
                continue;
            }
            let result = match entry.photo.try_exists() {
                Ok(true) => send(client, queue, &file, &mut entry),
                Ok(false) => {
                    tracing::warn!(photo = %entry.photo.display(), "queued photo no longer exists; dropping it from the queue");
                    match queue.done(&file) {
                        Ok(()) => {
                            pending.fetch_sub(1, Ordering::SeqCst);
                        }
                        Err(e) => tracing::error!("{e:#}"),
                    }
                    continue;
                }
                Err(e) => Err(Failure::Retry(format!("cannot check {}: {e}", entry.photo.display()))),
            };
            match result {
                Ok(()) => {
                    backoff.reset();
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
                    let d = match &failure {
                        Failure::Retry(_) => backoff.next_delay(),
                        Failure::Config(_) | Failure::Permanent(_) => pacing.permanent_retry,
                    };
                    let park = matches!(failure, Failure::Permanent(_));
                    if park {
                        // Whole seconds, rounded up, so a short test delay still parks.
                        let t = now + d.as_secs() + u64::from(d.subsec_nanos() > 0);
                        entry.next_try = Some(t);
                        earliest = Some(earliest.map_or(t, |e| e.min(t)));
                    }
                    if let Err(e) = queue.update(&file, &entry) {
                        tracing::error!("{e:#}");
                    }
                    tracing::warn!(photo = %entry.photo.display(), attempts = entry.attempts, "upload failed ({failure}); retry in {d:?}");
                    if !park {
                        stop = Some(d);
                        break; // keep order: retry the oldest photo first
                    }
                    // This photo (or its album step) is the problem, not the
                    // server: carry on with the next one.
                }
            }
        }
        // Errors of photos still queued stay visible; a success clears only its own.
        *last_error.lock().unwrap() = queue.pending().into_iter().filter_map(|(_, e)| e.last_error).next_back();
        let alive = match stop {
            Some(d) => wait_retry(wake, running, d),
            None => {
                let wait = earliest.map_or(pacing.idle_rescan, |t| {
                    Duration::from_secs(t.saturating_sub(unix_now())).min(pacing.idle_rescan)
                });
                wait_idle(wake, running, wait)
            }
        };
        if !alive {
            return;
        }
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Upload (if not done yet) and add to the album, recording progress so a
/// retry never uploads the same photo twice.
fn send(client: &Immich, queue: &Queue, file: &std::path::Path, entry: &mut Entry) -> Result<(), Failure> {
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

    const ID: &str = "4c1d8f6e-0b2a-4f6e-9a7c-2d4e6f8a0b1c";
    const MS: Duration = Duration::from_millis(1);

    fn fast() -> Pacing {
        Pacing { retry_initial: 20 * MS, retry_max: 80 * MS, permanent_retry: 200 * MS, idle_rescan: 50 * MS }
    }

    /// A URL on a port nothing listens on.
    fn dead_url() -> String {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://127.0.0.1:{}/api", l.local_addr().unwrap().port())
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
        let u = Uploader::start(q.clone(), Immich::new(&dead_url(), "k", None), fast()).unwrap();
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

    #[test]
    fn permanent_failure_skips_to_next_photo() {
        let mut server = mockito::Server::new();
        // Matches the first photo (booth_0) by its form field.
        server
            .mock("POST", "/api/assets")
            .match_body(mockito::Matcher::Regex("booth_0.jpg".into()))
            .with_status(400)
            .with_body("bad file")
            .create();
        server
            .mock("POST", "/api/assets")
            .match_body(mockito::Matcher::Regex("booth_1.jpg".into()))
            .with_status(201)
            .with_body(format!(r#"{{"id":"{ID}","status":"created"}}"#))
            .expect(1)
            .create();
        let (_dir, q) = setup(2);
        let _u = Uploader::start(q.clone(), Immich::new(&format!("{}/api", server.url()), "k", None), fast()).unwrap();
        wait_until(|| q.len() == 1 && q.pending().first().is_some_and(|(_, e)| e.attempts >= 1));
        let (f, e) = q.pending().remove(0);
        assert_eq!(f.file_name().unwrap(), "booth_0.json");
        assert!(e.next_try.is_some());
        assert!(e.last_error.unwrap().contains("400"));
    }

    #[test]
    fn bad_album_still_uploads_all_photos() {
        let mut server = mockito::Server::new();
        let up = server
            .mock("POST", "/api/assets")
            .with_status(201)
            .with_body(format!(r#"{{"id":"{ID}","status":"created"}}"#))
            .expect(3)
            .create();
        server.mock("PUT", "/api/albums/alb/assets").with_status(404).create();
        let (_dir, q) = setup(3);
        let _u = Uploader::start(q.clone(), Immich::new(&format!("{}/api", server.url()), "k", Some("alb")), fast())
            .unwrap();
        wait_until(|| {
            q.pending().len() == 3 && q.pending().iter().all(|(_, e)| e.asset_id.is_some() && e.attempts >= 1)
        });
        std::thread::sleep(300 * MS); // would re-upload if the asset id were ignored
        up.assert();
    }

    #[test]
    fn auth_failure_stops_the_pass() {
        let mut server = mockito::Server::new();
        let up = server.mock("POST", "/api/assets").with_status(401).with_body("nope").expect(1).create();
        let (_dir, q) = setup(2);
        let slow = Pacing { permanent_retry: Duration::from_secs(60), ..fast() };
        let u = Uploader::start(q.clone(), Immich::new(&format!("{}/api", server.url()), "k", None), slow).unwrap();
        wait_until(|| u.status().last_error.is_some());
        std::thread::sleep(300 * MS);
        up.assert();
        assert_eq!(q.len(), 2);
    }

    #[test]
    fn upload_404_pauses_all_uploads() {
        let mut server = mockito::Server::new();
        let up = server.mock("POST", "/api/assets").with_status(404).expect(1).create();
        let (_q_dir, q) = setup(2);
        let slow = Pacing { permanent_retry: Duration::from_secs(60), ..fast() };
        let u = Uploader::start(q.clone(), Immich::new(&format!("{}/api", server.url()), "k", None), slow).unwrap();
        wait_until(|| u.status().last_error.is_some());
        std::thread::sleep(300 * MS);
        up.assert();
        assert_eq!(q.len(), 2);
    }

    #[test]
    fn parked_error_stays_visible_after_another_photo_succeeds() {
        let mut server = mockito::Server::new();
        server
            .mock("POST", "/api/assets")
            .match_body(mockito::Matcher::Regex("booth_0.jpg".into()))
            .with_status(400)
            .with_body("bad file")
            .create();
        server
            .mock("POST", "/api/assets")
            .match_body(mockito::Matcher::Regex("booth_1.jpg".into()))
            .with_status(201)
            .with_body(format!(r#"{{"id":"{ID}","status":"created"}}"#))
            .create();
        let (_dir, q) = setup(2);
        let slow = Pacing { permanent_retry: Duration::from_secs(60), ..fast() };
        let u = Uploader::start(q.clone(), Immich::new(&format!("{}/api", server.url()), "k", None), slow).unwrap();
        wait_until(|| q.len() == 1 && u.status().last_error.is_some());
        std::thread::sleep(200 * MS);
        let s = u.status();
        assert!(s.last_error.as_ref().is_some_and(|e| e.contains("400")), "{s:?}");
    }

    #[test]
    fn wake_does_not_cut_backoff() {
        let (_dir, q) = setup(1);
        let slow = Pacing { retry_initial: 400 * MS, retry_max: 400 * MS, ..fast() };
        let u = Uploader::start(q.clone(), Immich::new(&dead_url(), "k", None), slow).unwrap();
        wait_until(|| q.pending()[0].1.attempts >= 1);
        for _ in 0..5 {
            u.wake();
        }
        std::thread::sleep(150 * MS);
        assert_eq!(q.pending()[0].1.attempts, 1, "wake must not trigger an early retry");
        wait_until(|| q.pending()[0].1.attempts >= 2);
    }
}
