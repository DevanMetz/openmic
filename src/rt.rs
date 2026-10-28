//! Real-time priority for audio threads.
//!
//! Windows joins the Pro Audio class on the processing thread (cpal does the
//! same for WASAPI callbacks). Linux PulseAudio callbacks are ordinary
//! threads, and `audio_thread_priority` without its dbus feature reports
//! success while leaving them at SCHED_OTHER. Ask RealtimeKit from a helper
//! so a missing `rtkit` daemon cannot stall the callback that asked.

#[cfg(target_os = "linux")]
mod linux {
    use std::cell::Cell;
    use std::thread;

    struct Job {
        info: audio_thread_priority::RtPriorityThreadInfo,
        sample_rate: u32,
        on_error: Box<dyn FnOnce(String) + Send>,
    }

    fn sender() -> &'static crossbeam_channel::Sender<Job> {
        use std::sync::OnceLock;
        static SENDER: OnceLock<crossbeam_channel::Sender<Job>> = OnceLock::new();
        SENDER.get_or_init(|| {
            let (tx, rx) = crossbeam_channel::unbounded::<Job>();
            // Spawn owns the receiver. If the thread cannot start, that
            // receiver is dropped and later sends report the failure.
            // Dropping the handle detaches the helper; it runs until exit.
            drop(
                thread::Builder::new()
                    .name("openmic-rt".into())
                    .spawn(move || {
                        while let Ok(job) = rx.recv() {
                            if let Err(error) = promote(job.info, job.sample_rate) {
                                (job.on_error)(error);
                            }
                        }
                    }),
            );
            tx
        })
    }

    fn promote(
        info: audio_thread_priority::RtPriorityThreadInfo,
        sample_rate: u32,
    ) -> Result<(), String> {
        // 10 ms, the period OpenMic asks PulseAudio for.
        let frames = (sample_rate / 100).max(1);
        audio_thread_priority::promote_thread_to_real_time(info, frames, sample_rate)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    /// True when this thread already queued a request.
    fn already_requested() -> bool {
        thread_local! {
            static REQUESTED: Cell<bool> = const { Cell::new(false) };
        }
        REQUESTED.with(|requested| {
            let was = requested.get();
            requested.set(true);
            was
        })
    }

    pub fn request(sample_rate: u32, on_error: impl FnOnce(String) + Send + 'static) {
        if already_requested() {
            return;
        }
        let info = match audio_thread_priority::get_current_thread_info() {
            Ok(info) => info,
            Err(error) => {
                on_error(error.to_string());
                return;
            }
        };
        let job = Job {
            info,
            sample_rate,
            on_error: Box::new(on_error),
        };
        if let Err(crossbeam_channel::SendError(job)) = sender().send(job) {
            (job.on_error)("real-time helper stopped".into());
        }
    }
}

/// Ask for real-time priority and return without waiting for the answer.
#[inline]
pub(crate) fn request_current_thread(
    sample_rate: u32,
    on_error: impl FnOnce(String) + Send + 'static,
) {
    #[cfg(target_os = "linux")]
    linux::request(sample_rate, on_error);
    #[cfg(not(target_os = "linux"))]
    let _ = (sample_rate, on_error);
}

/// Same request for a stream callback, logged when RealtimeKit refuses it.
#[inline]
pub(crate) fn request_audio_thread(sample_rate: u32, what: &'static str) {
    request_current_thread(sample_rate, move |error| {
        crate::logfile::warn(format_args!(
            "{what} thread: real-time priority refused: {error}"
        ));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn requesting_priority_is_a_no_op_off_linux() {
        request_audio_thread(48_000, "test");
        request_current_thread(48_000, |_| panic!("no real-time helper off Linux"));
    }

    /// `/proc/self/stat` field 41 (0-based index 38 after `comm`): scheduling
    /// policy. 0 is SCHED_OTHER, 1 is SCHED_FIFO, 2 is SCHED_RR.
    #[cfg(target_os = "linux")]
    fn scheduler_policy() -> i64 {
        let stat = std::fs::read_to_string("/proc/thread-self/stat").unwrap();
        let rest = stat.rsplit_once(')').unwrap().1;
        rest.split_whitespace().nth(38).unwrap().parse().unwrap()
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn promotion_succeeds_only_when_the_policy_actually_changes() {
        let result = audio_thread_priority::promote_current_thread_to_real_time(480, 48_000);
        match result {
            Ok(handle) => {
                let policy = scheduler_policy();
                assert!(
                    policy == 1 || policy == 2,
                    "promotion reported success at policy {policy}"
                );
                let _ = audio_thread_priority::demote_current_thread_from_real_time(handle);
            }
            Err(error) => assert!(!error.to_string().is_empty(), "{error}"),
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn each_audio_thread_asks_once_without_waiting_for_the_reply() {
        use std::sync::mpsc;
        use std::time::{Duration, Instant};

        // A fresh thread: the test runner reuses threads, and a request sticks
        // to the thread that made it.
        let report = std::thread::spawn(|| {
            let (tx, rx) = mpsc::channel();
            let start = Instant::now();
            request_current_thread(48_000, move |error| {
                let name = std::thread::current().name().map(str::to_owned);
                let _ = tx.send((name, error));
            });
            let elapsed = start.elapsed();
            request_current_thread(48_000, |_| panic!("a thread asks only once"));
            (elapsed, rx)
        })
        .join()
        .unwrap();
        let (elapsed, rx) = report;
        assert!(
            elapsed < Duration::from_millis(500),
            "the caller waited for promotion: {elapsed:?}"
        );
        match rx.recv_timeout(Duration::from_secs(5)) {
            Ok((name, error)) => {
                assert_eq!(name.as_deref(), Some("openmic-rt"));
                assert!(!error.is_empty(), "{error}");
                assert!(
                    rx.recv_timeout(Duration::from_millis(200)).is_err(),
                    "a second ask on the same thread must not run"
                );
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(error) => panic!("{error}"),
        }
    }
}
