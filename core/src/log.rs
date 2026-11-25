//! The app's log: what it prints for debugging, its errors, a thread's panic and a few figures now
//! and then, in a file in its data folder (a packaged app's printed output goes nowhere: a problem
//! seen by a player can only be told from this file). Five files of 2 MB at most: `vi-translator.log`,
//! then `vi-translator.1.log` … `.4.log`, oldest last.
//!
//! And `supervise`: a long-lived worker that panics is started again (with what it's given afresh),
//! not lost until the app is restarted; its panic is in the log.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const FILE: &str = "vi-translator.log";
const MAX_BYTES: u64 = 2 * 1024 * 1024;
const KEEP: usize = 5;

struct LogFile {
    dir: PathBuf,
    file: Option<std::fs::File>,
    size: u64,
}

static LOG: Mutex<Option<LogFile>> = Mutex::new(None);

/// A line for the log (and, as before, the printed output): `vlog!("[ps5] {}", why)`.
#[macro_export]
macro_rules! vlog {
    ($($arg:tt)*) => {
        $crate::log::line(&format!($($arg)*))
    };
}

/// The log's folder (once, at startup); its files are kept there from now on.
pub fn init(dir: &Path) {
    let _ = std::fs::create_dir_all(dir);
    let mut g = LOG.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    *g = Some(LogFile { dir: dir.to_path_buf(), file: None, size: 0 });
    drop(g);
    install_panic_hook();
}

/// The folder the log is in (none before `init`).
pub fn folder() -> Option<PathBuf> {
    LOG.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_ref().map(|l| l.dir.clone())
}

/// One line, with the time (UTC) in front, to the printed output and the log file.
pub fn line(text: &str) {
    let stamped = format!("{} {text}", timestamp(SystemTime::now()));
    eprintln!("{stamped}");
    let mut g = LOG.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(log) = g.as_mut() else { return };
    if log.file.is_none() || log.size >= MAX_BYTES {
        if log.size >= MAX_BYTES {
            log.file = None;
            rotate(&log.dir);
        }
        let path = log.dir.join(FILE);
        log.size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        log.file = std::fs::OpenOptions::new().create(true).append(true).open(&path).ok();
    }
    if let Some(f) = log.file.as_mut() {
        if writeln!(f, "{stamped}").is_ok() {
            log.size += stamped.len() as u64 + 1;
        }
    }
}

/// vi-translator.log → .1.log → … → .4.log (the oldest goes).
fn rotate(dir: &Path) {
    let name = |i: usize| if i == 0 { dir.join(FILE) } else { dir.join(format!("vi-translator.{i}.log")) };
    let _ = std::fs::remove_file(name(KEEP - 1));
    for i in (0..KEEP - 1).rev() {
        let _ = std::fs::rename(name(i), name(i + 1));
    }
}

/// "2026-11-25 08:31:07.042Z"
pub fn timestamp(t: SystemTime) -> String {
    let d = t.duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = d.as_secs();
    let (days, rest) = ((secs / 86_400) as i64, secs % 86_400);
    // days since 1970-01-01 → year, month, day (Howard Hinnant's civil_from_days)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}.{:03}Z", rest / 3600, rest % 3600 / 60, rest % 60, d.subsec_millis())
}

/// A panic anywhere goes into the log too: the thread, the message and where it happened.
fn install_panic_hook() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let before = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let thread = std::thread::current();
            let name = thread.name().unwrap_or("unnamed");
            let msg = info
                .payload()
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| info.payload().downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "(no message)".into());
            let at = info.location().map(|l| format!("{}:{}", l.file(), l.line())).unwrap_or_default();
            line(&format!("[panic] thread '{name}' at {at}: {msg}"));
            before(info);
        }));
    });
}

/// Runs `work` until it returns; when it panics, it's started again after a moment (1 s, then
/// longer, up to 10 s, while it keeps failing within a minute of starting). Each start gets what
/// `work` makes afresh, so give it a closure that clones what the worker takes.
pub fn supervise(name: &str, mut work: impl FnMut()) {
    let mut wait = Duration::from_secs(1);
    loop {
        let started = std::time::Instant::now();
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(&mut work)) {
            Ok(()) => return,
            Err(_) => {
                // (the panic hook has logged what happened and where)
                if started.elapsed() > Duration::from_secs(60) {
                    wait = Duration::from_secs(1);
                }
                line(&format!("[{name}] stopped on an error; starting it again in {} s", wait.as_secs()));
                std::thread::sleep(wait);
                wait = (wait * 2).min(Duration::from_secs(10));
            }
        }
    }
}

/// A supervised worker on a thread of its own, named `name`.
pub fn spawn_supervised(name: &'static str, work: impl FnMut() + Send + 'static) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new().name(name.into()).spawn(move || supervise(name, work)).expect("a thread")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    #[test]
    fn a_worker_that_panics_runs_again() {
        let runs = Arc::new(AtomicU32::new(0));
        let r = runs.clone();
        let t = std::thread::spawn(move || {
            supervise("test", || {
                // the first run fails; the second one finishes
                if r.fetch_add(1, Ordering::SeqCst) == 0 {
                    panic!("on purpose");
                }
            })
        });
        t.join().unwrap();
        assert_eq!(runs.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn the_time_reads_as_a_date() {
        assert_eq!(timestamp(UNIX_EPOCH), "1970-01-01 00:00:00.000Z");
        // 2026-11-25 08:31:07.042 UTC
        let t = UNIX_EPOCH + Duration::from_millis(1_795_595_467_042);
        assert_eq!(timestamp(t), "2026-11-25 08:31:07.042Z");
        // a leap day
        assert_eq!(timestamp(UNIX_EPOCH + Duration::from_secs(951_782_400)), "2000-02-29 00:00:00.000Z");
    }

    #[test]
    fn the_log_rotates_and_keeps_five() {
        let dir = std::env::temp_dir().join(format!("vit-log-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..7 {
            std::fs::write(dir.join(FILE), format!("{i}")).unwrap();
            rotate(&dir);
        }
        let mut names: Vec<String> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        names.sort();
        assert_eq!(names, ["vi-translator.1.log", "vi-translator.2.log", "vi-translator.3.log", "vi-translator.4.log"]);
        assert_eq!(std::fs::read_to_string(dir.join("vi-translator.1.log")).unwrap(), "6");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
