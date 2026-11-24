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
