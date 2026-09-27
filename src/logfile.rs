//! A small log beside the settings, so device and audio problems leave
//! something to read afterwards: `openmic.log`, with the previous run's
//! log kept as `openmic.old.log` once it passes 1 MB.

use std::fmt::Display;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const FILE_NAME: &str = "openmic.log";
const OLD_FILE_NAME: &str = "openmic.old.log";
/// Past this size at startup, the log starts over (keeping one old file).
const ROTATE_BYTES: u64 = 1 << 20;
/// Identical messages this close together are counted, not repeated.
const REPEAT_WINDOW: Duration = Duration::from_secs(60);

struct Log {
    file: File,
    last: Option<(String, Instant)>,
    repeats: usize,
}

static LOG: OnceLock<Mutex<Log>> = OnceLock::new();

/// The folder the log lives in (the settings folder).
pub fn dir() -> PathBuf {
    crate::config::settings_path().parent().map(Path::to_owned).unwrap_or_default()
}

/// Open the log for this run and record crashes into it. Logging is best
/// effort: if the file can't be opened, OpenMic runs without it.
pub fn init() {
    let Ok(file) = open(&dir()) else { return };
    if LOG.set(Mutex::new(Log { file, last: None, repeats: 0 })).is_err() {
        return;
    }
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic| {
        let thread = std::thread::current();
        write("PANIC", format_args!("{} thread: {panic}", thread.name().unwrap_or("unnamed")));
        previous(panic);
    }));
}

fn open(dir: &Path) -> std::io::Result<File> {
    fs::create_dir_all(dir)?;
    let path = dir.join(FILE_NAME);
    if fs::metadata(&path).is_ok_and(|m| m.len() > ROTATE_BYTES) {
        fs::rename(&path, dir.join(OLD_FILE_NAME))?;
    }
    OpenOptions::new().create(true).append(true).open(path)
}

pub fn info(message: impl Display) {
    write("INFO", message);
}

pub fn warn(message: impl Display) {
    write("WARN", message);
}

pub fn error(message: impl Display) {
    write("ERROR", message);
}

fn write(level: &str, message: impl Display) {
    let Some(log) = LOG.get() else { return };
    let Ok(mut log) = log.lock() else { return };
    let line = format!("{level} {message}");
    let _ = log.record(&line, SystemTime::now(), Instant::now());
}

impl Log {
    fn record(&mut self, line: &str, now: SystemTime, at: Instant) -> std::io::Result<()> {
        if let Some((last, when)) = &self.last
            && last == line
            && at.duration_since(*when) < REPEAT_WINDOW
        {
            self.repeats += 1;
            return Ok(());
        }
        if self.repeats > 0 {
            writeln!(self.file, "{} (repeated {} more times)", timestamp(now), self.repeats)?;
            self.repeats = 0;
        }
        self.last = Some((line.to_owned(), at));
        writeln!(self.file, "{} {line}", timestamp(now))?;
        self.file.flush()
    }
}

/// "2026-09-27 17:34:26Z" (UTC, so no time zone lookups).
fn timestamp(now: SystemTime) -> String {
    let secs = now.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let (days, rest) = (secs / 86_400, secs % 86_400);
    let (year, month, day) = civil_from_days(days as i64);
    format!("{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}Z", rest / 3600, rest % 3600 / 60, rest % 60)
}

/// Days since 1970-01-01 to a calendar date (Howard Hinnant's algorithm).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(month <= 2), month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_are_utc_calendar_times() {
        assert_eq!(timestamp(UNIX_EPOCH), "1970-01-01 00:00:00Z");
        let at = UNIX_EPOCH + Duration::from_secs(1_790_530_466); // 2026-09-27 17:34:26 UTC
        assert_eq!(timestamp(at), "2026-09-27 17:34:26Z");
        let leap = UNIX_EPOCH + Duration::from_secs(951_782_400); // 2000-02-29
        assert_eq!(timestamp(leap), "2000-02-29 00:00:00Z");
    }

    #[test]
    fn repeated_messages_are_counted_once() {
        let dir = tempfile::tempdir().unwrap();
        let mut log = Log { file: open(dir.path()).unwrap(), last: None, repeats: 0 };
        let (now, at) = (UNIX_EPOCH, Instant::now());
        for _ in 0..5 {
            log.record("WARN output: buffer underrun", now, at).unwrap();
        }
        log.record("INFO stopped", now, at).unwrap();
        log.record("INFO stopped", now, at + REPEAT_WINDOW).unwrap();
        let text = fs::read_to_string(dir.path().join(FILE_NAME)).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 4, "{text}");
        assert!(lines[0].ends_with("WARN output: buffer underrun"));
        assert!(lines[1].ends_with("(repeated 4 more times)"));
        assert!(lines[2].ends_with("INFO stopped"));
        assert!(lines[3].ends_with("INFO stopped"), "a repeat after the window is written again");
    }

    #[test]
    fn a_large_log_is_rotated_at_startup() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(FILE_NAME), vec![b'x'; ROTATE_BYTES as usize + 1]).unwrap();
        let mut file = open(dir.path()).unwrap();
        writeln!(file, "fresh").unwrap();
        assert_eq!(fs::read_to_string(dir.path().join(FILE_NAME)).unwrap(), "fresh\n");
        assert_eq!(fs::metadata(dir.path().join(OLD_FILE_NAME)).unwrap().len(), ROTATE_BYTES + 1);
    }
}
