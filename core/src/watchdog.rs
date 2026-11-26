//! The reading's watchdog: it notices when a line read on screen isn't shown translated in time, says in
//! the log why (`[stall] cause=… stage=… for=…ms`), and counts it for the health panel. The stages it
//! knows: the reading (no read for a while), the translator (one line taking long, or lines waiting
//! behind it), the presenter (a translation ready but not shown). Apple's helper heals itself
//! (apple_translate.rs); a stalled stage is told here, so a check report has the cause in it.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// A line asked for longer ago than this, and not shown: a stall.
pub const STALL_AFTER: Duration = Duration::from_secs(4);

#[derive(Default)]
struct Watch {
    /// per area: when its newest line was asked for (and its number), and when a translation was last shown
    asked: HashMap<String, (Instant, u64)>,
    shown: HashMap<String, Instant>,
    /// translations ready (sent to the presenter) per area, not yet shown
    ready: HashMap<String, Instant>,
    /// stalls told, by cause, since the start
    stalls: HashMap<&'static str, u64>,
    /// the area whose stall was told last (told once per stall)
    told: HashMap<String, Instant>,
    last: Option<(String, &'static str, u64)>,
}

static WATCH: Mutex<Option<Watch>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut Watch) -> R) -> R {
    let mut w = WATCH.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    f(w.get_or_insert_with(Watch::default))
}

/// Line `seq` of `area` was sent to the translator.
pub fn asked(area: &str, seq: u64) {
    with(|w| {
        w.asked.insert(area.to_string(), (Instant::now(), seq));
        w.told.remove(area);
    });
}

/// Its translation went to the presenter.
pub fn ready(area: &str) {
    with(|w| {
        w.ready.insert(area.to_string(), Instant::now());
    });
}

/// A translation of `area` is on screen now; or the line went (nothing to show any more).
pub fn shown(area: &str) {
    with(|w| {
        let now = Instant::now();
        w.shown.insert(area.to_string(), now);
        w.ready.remove(area);
        w.told.remove(area);
    });
}

/// Line `seq` of the area went (its English is gone, or a newer one replaced it): no stall to wait for.
pub fn dropped(area: &str, seq: u64) {
    with(|w| {
        if w.asked.get(area).is_some_and(|(_, s)| *s == seq) {
            w.asked.remove(area);
            w.ready.remove(area);
            w.told.remove(area);
        }
    });
}

/// Why a line asked at `asked` isn't shown at `now`, from what the stages say. `translating`: how long
/// the translator has been on its line; `read_ago`: since the last read of the areas.
pub fn cause(ready_at: Option<Instant>, translating: Option<Duration>, read_ago: Option<Duration>) -> (&'static str, &'static str) {
    if ready_at.is_some() {
        ("presenter_waiting", "present")
    } else if translating.is_some_and(|t| t >= STALL_AFTER) {
        ("translator_slow", "translate")
    } else if translating.is_some() {
        ("translator_queue", "translate")
    } else if read_ago.is_some_and(|t| t >= STALL_AFTER) {
        ("reading_stopped", "read")
    } else {
        ("translator_idle", "translate")
    }
}

/// Looked at every second (the watchdog thread): stalls told in the log, once each.
pub fn check(now: Instant) -> Vec<String> {
    let translating = crate::perf::translating_for();
    let read_ago = crate::perf::last(crate::perf::PerfKind::Read).map(|t| now.saturating_duration_since(t));
    with(|w| {
        let mut out = Vec::new();
        let areas: Vec<(String, Instant)> = w.asked.iter().map(|(a, (t, _))| (a.clone(), *t)).collect();
        for (area, at) in areas {
            let shown_after = w.shown.get(&area).is_some_and(|s| *s >= at);
            if shown_after || now.saturating_duration_since(at) < STALL_AFTER || w.told.contains_key(&area) {
                continue;
            }
            let (c, stage) = cause(w.ready.get(&area).copied(), translating, read_ago);
            let ms = now.saturating_duration_since(at).as_millis() as u64;
            *w.stalls.entry(c).or_default() += 1;
            w.told.insert(area.clone(), now);
            w.last = Some((area.clone(), c, ms));
            out.push(format!("[stall] cause={c} stage={stage} area={area} for={ms}ms"));
        }
        out
    })
}

/// {"stalls": {cause: n}, "last": {"area", "cause", "ms"}} for the health panel.
pub fn health() -> serde_json::Value {
    with(|w| {
        serde_json::json!({
            "stalls": w.stalls.iter().map(|(k, v)| (k.to_string(), serde_json::json!(v))).collect::<serde_json::Map<_, _>>(),
            "last": w.last.as_ref().map(|(a, c, ms)| serde_json::json!({ "area": a, "cause": c, "ms": ms })),
        })
    })
}

/// The watchdog thread (once).
pub fn start() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        crate::log::spawn_supervised("watchdog", || loop {
            std::thread::sleep(Duration::from_secs(1));
            for l in check(Instant::now()) {
                crate::vlog!("{l}");
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cause_follows_the_stages() {
        let s = Duration::from_secs;
        assert_eq!(cause(Some(Instant::now()), None, None).0, "presenter_waiting");
        assert_eq!(cause(None, Some(s(6)), Some(s(0))).0, "translator_slow");
        assert_eq!(cause(None, Some(s(1)), Some(s(0))).0, "translator_queue");
        assert_eq!(cause(None, None, Some(s(9))).0, "reading_stopped");
        assert_eq!(cause(None, None, Some(s(0))).0, "translator_idle");
    }

    #[test]
    fn a_stall_is_told_once_and_a_shown_line_is_none() {
        let t = Instant::now();
        with(|w| {
            w.asked.insert("wd-a".into(), (t, 7));
            w.asked.insert("wd-b".into(), (t, 8));
        });
        shown("wd-b");
        let later = t + Duration::from_secs(5);
        let first = check(later);
        assert!(first.iter().any(|l| l.contains("area=wd-a")), "{first:?}");
        assert!(!first.iter().any(|l| l.contains("area=wd-b")), "{first:?}");
        assert!(check(later + Duration::from_secs(1)).iter().all(|l| !l.contains("area=wd-a")), "told once");
        dropped("wd-a", 6);
        assert!(with(|w| w.asked.contains_key("wd-a")), "another line's end doesn't drop it");
        dropped("wd-a", 7);
        assert!(!with(|w| w.asked.contains_key("wd-a")));
        assert!(health()["stalls"].as_object().is_some_and(|m| !m.is_empty()));
    }
}
