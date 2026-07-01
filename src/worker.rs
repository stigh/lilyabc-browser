//! Background render worker. LilyPond can take several seconds per file with no
//! incremental output, so all engraving happens off the UI thread. Jobs carry a
//! monotonically increasing id; the UI keeps only the newest result (latest-wins),
//! which also gives us debounced live-edit for free.

use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread;

use eframe::egui;
use lru::LruCache;

use crate::engraver::{self, RenderRequest};
use crate::model::{Format, RenderOutput};

struct Job {
    id: u64,
    req: RenderRequest,
}

/// A completed render, tagged with the id of the job that produced it.
pub struct RenderResult {
    pub id: u64,
    pub output: RenderOutput,
}

/// Handle to the worker thread: submit jobs, poll results.
pub struct RenderWorker {
    tx: Sender<Job>,
    rx: Receiver<RenderResult>,
}

impl RenderWorker {
    /// Spawn the worker. `ctx` is used to wake the UI when a render completes.
    pub fn spawn(ctx: egui::Context) -> Self {
        let (job_tx, job_rx) = channel::<Job>();
        let (res_tx, res_rx) = channel::<RenderResult>();
        thread::Builder::new()
            .name("render-worker".into())
            .spawn(move || worker_loop(job_rx, res_tx, ctx))
            .expect("spawn render worker");
        Self {
            tx: job_tx,
            rx: res_rx,
        }
    }

    /// Queue a render. Returns the assigned job id (already incremented by the caller).
    pub fn submit(&self, id: u64, req: RenderRequest) {
        let _ = self.tx.send(Job { id, req });
    }

    /// Drain any finished renders (non-blocking).
    pub fn poll(&self) -> Vec<RenderResult> {
        self.rx.try_iter().collect()
    }
}

const CACHE_CAP: usize = 64;

fn worker_loop(jobs: Receiver<Job>, results: Sender<RenderResult>, ctx: egui::Context) {
    let scratch_root = std::env::temp_dir().join(format!("lilyabc-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&scratch_root);
    let mut cache: LruCache<[u8; 32], RenderOutput> =
        LruCache::new(NonZeroUsize::new(CACHE_CAP).unwrap());

    while let Ok(mut job) = jobs.recv() {
        // Coalesce: skip straight to the newest queued job (the UI discards superseded
        // results anyway), avoiding wasted multi-second engraver runs on rapid clicks/edits.
        while let Ok(next) = jobs.try_recv() {
            job = next;
        }
        let key = cache_key(&job.req);
        let cached = if job.req.force {
            None
        } else {
            cache.get(&key).cloned()
        };
        let output = if let Some(hit) = cached {
            hit // identical content already rendered — skip the engraver
        } else {
            let work = scratch_root.join(format!("job-{}", job.id));
            let _ = std::fs::create_dir_all(&work);
            let out = engraver::render(&job.req, &work);
            cleanup(&work);
            if out.ok {
                cache.put(key, out.clone());
            }
            out
        };
        if results.send(RenderResult { id: job.id, output }).is_err() {
            break; // UI gone
        }
        ctx.request_repaint();
    }
}

/// Content-addressed cache key: format + tune + base dir + source text.
fn cache_key(req: &RenderRequest) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(&[match req.format {
        Format::LilyPond => 0u8,
        Format::Abc => 1u8,
    }]);
    h.update(&req.tune.unwrap_or(0).to_le_bytes());
    h.update(req.base_dir.to_string_lossy().as_bytes());
    h.update(&[0]); // separator between path and source
    h.update(req.source.as_bytes());
    *h.finalize().as_bytes()
}

/// Remove a job's scratch directory once its bytes have been read into memory.
fn cleanup(work: &PathBuf) {
    let _ = std::fs::remove_dir_all(work);
}
