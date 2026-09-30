//! Read saved clips' duration headers without blocking the soundboard.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};

use crossbeam_channel::{Receiver, TryRecvError};
use eframe::egui;

#[derive(Default)]
pub(super) struct Durations {
    known: HashMap<PathBuf, Option<f32>>,
    queued: VecDeque<PathBuf>,
    job: Option<Job>,
}

struct Job {
    /// Cleared when the source is invalidated, but the job stays tracked
    /// until it finishes so a slow read cannot spawn competing workers.
    path: Option<PathBuf>,
    result: Receiver<Option<f32>>,
}

impl Durations {
    pub(super) fn get(&mut self, path: &Path) -> Option<f32> {
        if let Some(duration) = self.known.get(path) {
            return *duration;
        }
        let running = self.job.as_ref().is_some_and(|job| job.path.as_deref() == Some(path));
        if !running && !self.queued.iter().any(|queued| queued == path) {
            self.queued.push_back(path.to_owned());
        }
        None
    }

    pub(super) fn forget(&mut self, path: &Path) {
        self.known.remove(path);
        self.queued.retain(|queued| queued != path);
        if let Some(job) = &mut self.job && job.path.as_deref() == Some(path) {
            job.path = None;
        }
    }

    pub(super) fn poll(&mut self, ctx: &egui::Context) {
        let finished = self.job.as_ref().and_then(|job| match job.result.try_recv() {
            Ok(duration) => Some(duration),
            Err(TryRecvError::Disconnected) => Some(None),
            Err(TryRecvError::Empty) => None,
        });
        if let Some(duration) = finished {
            let job = self.job.take().expect("checked above");
            if let Some(path) = job.path {
                self.known.insert(path, duration);
            }
        }
        while self.job.is_none() && let Some(path) = self.queued.pop_front() {
            let (done, result) = crossbeam_channel::bounded(1);
            let (source, wake) = (path.clone(), ctx.clone());
            let spawned = std::thread::Builder::new().name("openmic-clip-duration".into()).spawn(move || {
                let _ = done.send(crate::decode::duration(&source));
                wake.request_repaint();
            });
            match spawned {
                Ok(_) => self.job = Some(Job { path: Some(path), result }),
                Err(error) => {
                    crate::logfile::warn(format_args!("could not read duration for {}: {error}", path.display()));
                    self.known.insert(path, None);
                }
            }
        }
    }

    #[cfg(test)]
    pub(super) fn finish(&mut self, ctx: &egui::Context) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while self.job.is_some() || !self.queued.is_empty() {
            self.poll(ctx);
            assert!(std::time::Instant::now() < deadline, "duration reads did not finish");
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn delayed(path: PathBuf) -> (Durations, crossbeam_channel::Sender<Option<f32>>) {
        let (done, result) = crossbeam_channel::bounded(1);
        (Durations { job: Some(Job { path: Some(path), result }), ..Default::default() }, done)
    }

    #[test]
    fn slow_header_reads_keep_one_worker_and_deduplicate_waiting_paths() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first.wav");
        let second = dir.path().join("missing.wav");
        let (mut durations, done) = delayed(first.clone());
        let ctx = egui::Context::default();
        for _ in 0..10 {
            assert_eq!(durations.get(&first), None);
            assert_eq!(durations.get(&second), None);
            durations.poll(&ctx);
        }
        assert_eq!(durations.queued.len(), 1);
        assert_eq!(durations.job.as_ref().unwrap().path.as_ref(), Some(&first));
        done.send(Some(1.25)).unwrap();
        durations.finish(&ctx);
        assert_eq!(durations.get(&first), Some(1.25));
        assert_eq!(durations.get(&second), None);
        assert!(durations.known.contains_key(&second), "an unreadable header is cached as unknown");
        assert!(durations.queued.is_empty(), "unknown lengths must not be retried on every frame");
    }

    #[test]
    fn invalidating_a_file_ignores_its_old_result_and_removes_queued_reads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("replaced.wav");
        let removed = dir.path().join("removed.wav");
        let (mut durations, done) = delayed(path.clone());
        let ctx = egui::Context::default();
        durations.get(&removed);
        durations.forget(&removed);
        durations.forget(&path);
        assert!(durations.queued.is_empty());
        assert!(durations.job.as_ref().unwrap().path.is_none(), "track the worker but discard its answer");
        durations.get(&path);
        durations.poll(&ctx);
        assert_eq!(durations.queued.len(), 1, "a replacement must wait for the old read to finish");
        done.send(Some(99.0)).unwrap();
        durations.finish(&ctx);
        assert_eq!(durations.get(&path), None, "the stale duration cannot reappear after invalidation");
        assert!(!durations.known.contains_key(&removed));
    }

    #[test]
    fn a_failed_worker_does_not_stall_the_rest_of_the_duration_queue() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first.wav");
        let second = dir.path().join("second.wav");
        let (mut durations, done) = delayed(first.clone());
        durations.get(&second);
        drop(done);
        durations.finish(&egui::Context::default());
        assert_eq!(durations.known.get(&first), Some(&None));
        assert_eq!(durations.known.get(&second), Some(&None));
        assert!(durations.job.is_none() && durations.queued.is_empty());
    }
}
