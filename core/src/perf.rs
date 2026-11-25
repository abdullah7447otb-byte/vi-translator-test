//! How the app keeps up, over the last minute, for the Translator tab: a read of the areas (OCR), a
//! translation, and how long after the English appeared its translation was ready. Shown to players so a
//! slow device (or a slowdown after a long session) can be told from a missing translation.

use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Clone, Copy)]
pub enum PerfKind {
    Read,
    Translate,
    Ready,
}

struct Perf {
    started: Instant,
    samples: [std::collections::VecDeque<(Instant, f64)>; 3],
}

static PERF: Mutex<Option<Perf>> = Mutex::new(None);
const PERF_WINDOW: Duration = Duration::from_secs(60);

pub fn perf_note(kind: PerfKind, ms: f64) {
    let mut p = PERF.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let p = p.get_or_insert_with(|| Perf { started: Instant::now(), samples: Default::default() });
    let q = &mut p.samples[kind as usize];
    let now = Instant::now();
    q.push_back((now, ms));
    while q.front().is_some_and(|(t, _)| now - *t > PERF_WINDOW) || q.len() > 5000 {
        q.pop_front();
    }
}

/// {"minutes", "read", "translate", "ready"}, each {"avg", "max", "n"} in ms over the last minute
/// (null before anything was read).
pub fn perf_status() -> serde_json::Value {
    let p = PERF.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(p) = p.as_ref() else { return serde_json::json!(null) };
    let now = Instant::now();
    let stat = |k: PerfKind| {
        let v: Vec<f64> = p.samples[k as usize].iter().filter(|(t, _)| now - *t <= PERF_WINDOW).map(|(_, ms)| *ms).collect();
        if v.is_empty() {
            return serde_json::json!(null);
        }
        let avg = v.iter().sum::<f64>() / v.len() as f64;
        let max = v.iter().cloned().fold(0.0, f64::max);
        serde_json::json!({ "avg": avg.round(), "max": max.round(), "n": v.len() })
    };
    serde_json::json!({
        "minutes": (now - p.started).as_secs() / 60,
        "read": stat(PerfKind::Read), "translate": stat(PerfKind::Translate), "ready": stat(PerfKind::Ready),
    })
}

/// When the last sample of this kind came (none yet: None): the reading has stalled when its last
/// read is long ago.
pub fn last(kind: PerfKind) -> Option<Instant> {
    let p = PERF.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    p.as_ref().and_then(|p| p.samples[kind as usize].back().map(|(t, _)| *t))
}

static TRANSLATING: Mutex<Option<Instant>> = Mutex::new(None);

/// The translator started a line (`true`) or finished it (`false`).
pub fn translating(on: bool) {
    *TRANSLATING.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = on.then(Instant::now);
}

/// How long the translator has been on the line it's on (None: it's waiting for one).
pub fn translating_for() -> Option<Duration> {
    TRANSLATING.lock().unwrap_or_else(std::sync::PoisonError::into_inner).map(|t| t.elapsed())
}

/// The least time between reads the performance mode allows now (ms; 0: as the settings say). The
/// desktop app sets it (src-tauri/src/load.rs); the reading never reads more often.
pub static READ_FLOOR_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The average of this kind's samples over the last `secs` seconds (none in that time: None).
pub fn recent_avg(kind: PerfKind, secs: u64) -> Option<f64> {
    let p = PERF.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let now = Instant::now();
    let v: Vec<f64> = p.as_ref()?.samples[kind as usize].iter().rev().take_while(|(t, _)| now - *t <= Duration::from_secs(secs)).map(|(_, ms)| *ms).collect();
    (!v.is_empty()).then(|| v.iter().sum::<f64>() / v.len() as f64)
}

/// This thread at a lower priority than the picture's (the translator's: a line ready a few
/// milliseconds later is never seen, a picture late is). macOS: the "utility" quality of service,
/// which the threads it starts take too; Windows: below normal.
pub fn lower_this_thread() {
    #[cfg(target_os = "macos")]
    {
        unsafe extern "C" {
            fn pthread_set_qos_class_self_np(qos: u32, relative_priority: i32) -> i32;
        }
        const QOS_CLASS_UTILITY: u32 = 0x11;
        unsafe {
            pthread_set_qos_class_self_np(QOS_CLASS_UTILITY, 0);
        }
    }
    #[cfg(windows)]
    {
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetCurrentThread() -> isize;
            fn SetThreadPriority(thread: isize, priority: i32) -> i32;
        }
        const THREAD_PRIORITY_BELOW_NORMAL: i32 = -1;
        unsafe {
            SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL);
        }
    }
}

