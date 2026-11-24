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
        let mut delay = None;
        for (file, mut entry) in entries {
            if !running.load(Ordering::SeqCst) {
                return;
            }
            if !entry.photo.exists() {
                tracing::warn!(photo = %entry.photo.display(), "queued photo no longer exists; dropping it from the queue");
                match queue.done(&file) {
                    Ok(()) => {
                        pending.fetch_sub(1, Ordering::SeqCst);
                    }
                    Err(e) => tracing::error!("{e:#}"),
                }
                continue;
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
        let alive = match delay {
            Some(d) => wait_retry(wake, running, d),
            None => wait_idle(wake, running, pacing.idle_rescan),
        };
        if !alive {
            return;
        }
    }
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

    #[test]
    fn wake_does_not_cut_backoff() {
        let (_dir, q) = setup(1);
        let slow = Pacing { retry_initial: 400 * MS, retry_max: 400 * MS, ..fast() };
        let u = Uploader::start(q.clone(), Immich::new("http://127.0.0.1:9/api", "k", None), slow).unwrap();
        wait_until(|| q.pending()[0].1.attempts >= 1);
        for _ in 0..5 {
            u.wake();
        }
        std::thread::sleep(150 * MS);
        assert_eq!(q.pending()[0].1.attempts, 1, "wake must not trigger an early retry");
        wait_until(|| q.pending()[0].1.attempts >= 2);
    }
}
