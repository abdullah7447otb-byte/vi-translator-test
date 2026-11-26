//! Apple's on-device translation (macOS 26 and later) through the app's helper, `vit-apple-translate`, next
//! to the app's own program in the .app (src-tauri/apple): started once and kept, newline-delimited JSON
//! over its stdin/stdout.
//!
//! It heals itself, so the translation never stops until the app is started again:
//! - the helper is warmed up (its session made and its model loaded) when the app starts and when the
//!   engine is chosen: the first real line doesn't pay for it;
//! - a time limit per batch that follows how long batches take (three times the slow ones), longer for the
//!   first batch of a helper;
//! - a batch that takes longer gives an error, but the helper is left to finish: its late answer goes to
//!   a small cache that the next request for those lines takes. Two time-outs in a row: the helper is
//!   stopped (and waited for: no zombie) and the next request starts a new one;
//! - an error answer (the helper's session gone bad: after sleep, a model update) counts too: two in a
//!   row and the helper is started again — a new helper makes a new session;
//! - the helper itself makes its session again on an error and tries once more.

use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context as _, Result};
use serde_json::{json, Value};

/// A helper's first batch (its model loading): at most this long.
pub const FIRST_TIMEOUT: Duration = Duration::from_secs(10);
/// A later batch: three times the slow ones (p95), within these.
pub const MIN_TIMEOUT: Duration = Duration::from_millis(1500);
pub const MAX_TIMEOUT: Duration = Duration::from_secs(6);
/// Time-outs, or error answers, in a row that start the helper again.
const RESTART_AFTER: u32 = 2;

thread_local! {
    /// Tests run the time limits faster: a divisor (1 = real time), for the calling thread only.
    static SPEED: std::cell::Cell<u32> = const { std::cell::Cell::new(1) };
}
fn scaled(d: Duration) -> Duration {
    d / SPEED.with(|s| s.get()).max(1)
}

struct Helper {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
    next_id: u64,
    /// answered at least once (its model is loaded)
    warm: bool,
    /// requests that timed out, by id: their texts (a late answer fills the cache)
    pending: HashMap<u64, Vec<String>>,
}

#[derive(Default)]
struct State {
    helper: Option<Helper>,
    /// time-outs and error answers in a row
    timeouts: u32,
    errors: u32,
    /// the last batches' times, for the time limit
    times: VecDeque<Duration>,
    /// helpers started (after the first): shown in the health panel
    restarts: u64,
}

static STATE: Mutex<State> = Mutex::new(State { helper: None, timeouts: 0, errors: 0, times: VecDeque::new(), restarts: 0 });

/// Lines answered after their request had timed out: (lang, English) -> translation. Small and bounded.
static LATE: Mutex<Option<HashMap<(String, String), String>>> = Mutex::new(None);
const LATE_MAX: usize = 256;

/// Where the helper is: `VIT_APPLE_TRANSLATE`, else next to this program.
pub fn helper_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("VIT_APPLE_TRANSLATE") {
        return Some(PathBuf::from(p));
    }
    let p = std::env::current_exe().ok()?.parent()?.join("vit-apple-translate");
    p.exists().then_some(p)
}

fn start(path: &PathBuf) -> Result<Helper> {
    let mut child = Command::new(path).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().with_context(|| format!("can't start {}", path.display()))?;
    let stdin = child.stdin.take().ok_or_else(|| anyhow!("no stdin"))?;
    let stdout = child.stdout.take().ok_or_else(|| anyhow!("no stdout"))?;
    let (tx, rx) = std::sync::mpsc::channel();
    // (it ends by itself when the helper's stdout closes)
    let mut out = Some((stdout, tx));
    crate::log::spawn_supervised("apple-helper-out", move || {
        let Some((stdout, tx)) = out.take() else { return };
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    Ok(Helper { child, stdin, lines: rx, next_id: 1, warm: false, pending: HashMap::new() })
}

/// The helper stopped and waited for (no zombie process left).
fn stop(st: &mut State, why: &str) {
    if let Some(mut h) = st.helper.take() {
        let _ = h.child.kill();
        let _ = h.child.wait();
        crate::vlog!("[apple] helper stopped: {why}");
    }
    st.timeouts = 0;
    st.errors = 0;
}

fn time_limit(st: &State, warm: bool) -> Duration {
    if !warm {
        return scaled(FIRST_TIMEOUT);
    }
    let mut v: Vec<Duration> = st.times.iter().copied().collect();
    if v.is_empty() {
        return scaled(MAX_TIMEOUT);
    }
    v.sort_unstable();
    let p95 = v[((v.len() * 95).div_ceil(100)).saturating_sub(1).min(v.len() - 1)];
    scaled((p95 * 3).clamp(MIN_TIMEOUT, MAX_TIMEOUT))
}

/// A late answer to a request that timed out: its lines into the cache.
fn keep_late(lang: &str, texts: &[String], v: &Value) {
    let Some(out) = v["out"].as_array() else { return };
    if out.len() != texts.len() {
        return;
    }
    let mut late = LATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let m = late.get_or_insert_with(HashMap::new);
    if m.len() > LATE_MAX {
        m.clear();
    }
    for (en, ar) in texts.iter().zip(out) {
        if let Some(ar) = ar.as_str().filter(|s| !s.trim().is_empty()) {
            m.insert((lang.to_string(), en.clone()), ar.to_string());
        }
    }
}

/// One request and its answer. The helper is started if it isn't running (or has exited).
fn ask(mut request: Value, lang: &str, texts: &[String], limit: Option<Duration>) -> Result<Value> {
    let path = helper_path().ok_or_else(|| anyhow!("the Apple translation helper isn't in this build"))?;
    let mut st = STATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let exited = st.helper.as_mut().is_some_and(|h| h.child.try_wait().ok().flatten().is_some());
    if exited {
        // (reaped by try_wait)
        st.helper = None;
        st.restarts += 1;
        crate::vlog!("[apple] the helper had exited: started again");
    }
    if st.helper.is_none() {
        st.helper = Some(start(&path)?);
    }
    let warm = st.helper.as_ref().is_some_and(|h| h.warm);
    let timeout = limit.unwrap_or_else(|| time_limit(&st, warm));
    let h = st.helper.as_mut().expect("started");
    let id = h.next_id;
    h.next_id += 1;
    request["id"] = json!(id);
    let sent = writeln!(h.stdin, "{request}").and_then(|_| h.stdin.flush());
    if let Err(e) = sent {
        stop(&mut st, "it stopped reading");
        bail!("the Apple translation helper stopped: {e}");
    }
    let t0 = Instant::now();
    let deadline = t0 + timeout;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let h = st.helper.as_mut().expect("running");
        match h.lines.recv_timeout(left) {
            Ok(line) => {
                let v: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
                let got = v["id"].as_u64();
                if got != Some(id) {
                    // (an answer to an earlier request that timed out: its lines are kept)
                    if let Some(old) = got.and_then(|g| h.pending.remove(&g)) {
                        keep_late(lang, &old, &v);
                    }
                    continue;
                }
                h.warm = true;
                if let Some(e) = v["error"].as_str() {
                    st.errors += 1;
                    st.timeouts = 0;
                    let msg = e.to_string();
                    if st.errors >= RESTART_AFTER {
                        stop(&mut st, &format!("{RESTART_AFTER} errors in a row ({msg})"));
                        st.restarts += 1;
                    }
                    bail!("Apple translation: {msg}");
                }
                st.errors = 0;
                st.timeouts = 0;
                if !texts.is_empty() {
                    st.times.push_back(t0.elapsed());
                    if st.times.len() > 32 {
                        st.times.pop_front();
                    }
                }
                return Ok(v);
            }
            Err(RecvTimeoutError::Timeout) => {
                h.pending.insert(id, texts.to_vec());
                if h.pending.len() > 16 {
                    let oldest = h.pending.keys().min().copied();
                    if let Some(o) = oldest {
                        h.pending.remove(&o);
                    }
                }
                st.timeouts += 1;
                st.errors = 0;
                if st.timeouts >= RESTART_AFTER {
                    stop(&mut st, &format!("{RESTART_AFTER} time-outs in a row"));
                    st.restarts += 1;
                }
                bail!("Apple translation took longer than {} ms", timeout.as_millis());
            }
            Err(RecvTimeoutError::Disconnected) => {
                stop(&mut st, "it closed its output");
                st.restarts += 1;
                bail!("the Apple translation helper stopped");
            }
        }
    }
}

/// The lines translated from English into `dst` (a language code: "ar").
pub fn translate(dst: &str, texts: &[String]) -> Result<Vec<String>> {
    // (lines a late answer brought: no need to ask again)
    let cached: Vec<Option<String>> = {
        let late = LATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        texts.iter().map(|t| late.as_ref().and_then(|m| m.get(&(dst.to_string(), t.clone())).cloned())).collect()
    };
    if cached.iter().all(Option::is_some) && !texts.is_empty() {
        return Ok(cached.into_iter().flatten().collect());
    }
    let v = ask(json!({ "src": "en", "dst": dst, "texts": texts }), dst, texts, None)?;
    let out: Vec<String> = v["out"].as_array().ok_or_else(|| anyhow!("Apple translation: no answer"))?.iter().map(|s| s.as_str().unwrap_or_default().to_string()).collect();
    if out.len() != texts.len() {
        bail!("Apple translation answered {} lines for {}", out.len(), texts.len());
    }
    Ok(out)
}

/// The helper started and its session made and loaded (a model download isn't started: only the
/// language's own pack, if installed). For the app's start and when the engine is chosen; on a thread of
/// its own (it can take seconds).
pub fn warm_up(dst: &str) -> Result<()> {
    let t0 = Instant::now();
    ask(json!({ "src": "en", "dst": dst, "warm": true }), dst, &[], Some(scaled(FIRST_TIMEOUT)))?;
    crate::vlog!("[apple] ready in {} ms", t0.elapsed().as_millis());
    Ok(())
}

/// For the health panel: the helper running, warm, its restarts and the time limit now.
pub fn health() -> Value {
    // (a request going on holds the state: "busy", the panel isn't held up)
    let st = match STATE.try_lock() {
        Ok(st) => st,
        Err(std::sync::TryLockError::Poisoned(p)) => p.into_inner(),
        Err(std::sync::TryLockError::WouldBlock) => return json!({ "running": true, "busy": true }),
    };
    let warm = st.helper.as_ref().is_some_and(|h| h.warm);
    json!({
        "running": st.helper.is_some(),
        "warm": warm,
        "restarts": st.restarts,
        "limit_ms": time_limit(&st, warm).as_millis() as u64,
    })
}

/// The engine's state on this Mac for English → `dst`: "installed" (ready), "supported" (its language pack
/// to download first), "unsupported", "no-os" (before macOS 26), or "missing" (no helper in this build).
pub fn status(dst: &str) -> String {
    let Some(path) = helper_path() else { return "missing".into() };
    match Command::new(path).args(["--status", "en", dst]).output() {
        Ok(o) => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        Err(_) => "missing".into(),
    }
}

/// Apple failing again and again: the offline translator takes over for a while, then Apple is tried
/// again by itself (see translate.rs).
pub struct Breaker {
    fails: VecDeque<Instant>,
    open_until: Option<Instant>,
}

/// Failures within `WINDOW` that open the breaker, and for how long.
const BREAK_FAILS: usize = 3;
const BREAK_WINDOW: Duration = Duration::from_secs(60);
const BREAK_FOR: Duration = Duration::from_secs(30);

impl Breaker {
    pub const fn new() -> Self {
        Self { fails: VecDeque::new(), open_until: None }
    }
    /// Whether Apple should be asked now. (Closing again after the pause is logged.)
    pub fn allows(&mut self, now: Instant) -> bool {
        match self.open_until {
            Some(t) if now < t => false,
            Some(_) => {
                self.open_until = None;
                self.fails.clear();
                crate::vlog!("[apple] trying Apple translation again");
                true
            }
            None => true,
        }
    }
    pub fn failed(&mut self, now: Instant) {
        self.fails.push_back(now);
        while self.fails.front().is_some_and(|t| now.saturating_duration_since(*t) > BREAK_WINDOW) {
            self.fails.pop_front();
        }
        if self.fails.len() >= BREAK_FAILS && self.open_until.is_none() {
            self.open_until = Some(now + BREAK_FOR);
            crate::vlog!("[apple] {BREAK_FAILS} failures in a minute: the offline translator for {} s", BREAK_FOR.as_secs());
        }
    }
    pub fn succeeded(&mut self) {
        self.fails.clear();
    }
}

impl Default for Breaker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// A fake helper (a shell script): echoes each text back upper-cased with its request's id. "slow" makes
    /// it sleep past the time limit (it answers late), "die" makes it exit, "bad" makes this helper answer
    /// errors from then on (its session gone bad) — a new helper is well again. "warm" is answered ok.
    fn fake() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("vit-apple-fake-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let p = dir.join("vit-apple-translate");
        std::fs::write(
            &p,
            r#"#!/bin/sh
if [ "$1" = "--status" ]; then echo installed; exit 0; fi
bad=0
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  case "$line" in
    *'"warm"'*) printf '{"id":%s,"ok":true}\n' "$id"; continue ;;
    *slow*) sleep 1 ;;
    *die*) exit 1 ;;
    *bad*) bad=1 ;;
  esac
  if [ "$bad" = "1" ]; then printf '{"id":%s,"error":"session gone"}\n' "$id"; continue; fi
  t=$(printf '%s' "$line" | sed -n 's/.*"texts":\["\([^"]*\)".*/\1/p' | tr a-z A-Z)
  printf '{"id":%s,"out":["%s"]}\n' "$id" "$t"
done
"#,
        )
        .expect("fake helper");
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        p
    }

    /// The tests that use the helper run one at a time (they share the one helper of the process).
    static SERIAL: Mutex<()> = Mutex::new(());

    fn running_pid() -> Option<u32> {
        STATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner).helper.as_ref().map(|h| h.child.id())
    }

    #[test]
    fn the_helper_heals_after_timeouts_errors_and_crashes() {
        let _one = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        unsafe { std::env::set_var("VIT_APPLE_TRANSLATE", fake()) };
        // time limits 8× faster: first batch 1.25 s, later ones 0.19–0.75 s
        SPEED.with(|s| s.set(8));
        assert_eq!(status("ar"), "installed");
        warm_up("ar").expect("warm");
        assert_eq!(translate("ar", &["hello".into()]).expect("hello"), vec!["HELLO".to_string()]);
        assert_eq!(translate("ar", &["again".into()]).expect("again"), vec!["AGAIN".to_string()]);

        // one slow batch: an error, the helper kept; its late answer is used for that line afterwards
        let pid = running_pid();
        let t0 = Instant::now();
        assert!(translate("ar", &["slow".into()]).unwrap_err().to_string().contains("longer than"));
        assert!(t0.elapsed() < Duration::from_millis(900), "{:?}", t0.elapsed());
        assert_eq!(running_pid(), pid, "one time-out doesn't stop the helper");
        std::thread::sleep(Duration::from_millis(1200));
        assert_eq!(translate("ar", &["after".into()]).expect("after"), vec!["AFTER".to_string()]);
        assert_eq!(translate("ar", &["slow".into()]).expect("late answer"), vec!["SLOW".to_string()], "the late answer was kept");

        // its session goes bad (every answer an error): after two errors a new helper, which works
        assert!(translate("ar", &["bad".into()]).is_err());
        assert!(translate("ar", &["next".into()]).is_err());
        assert_ne!(running_pid(), pid, "a helper answering errors is replaced");
        assert_eq!(translate("ar", &["well".into()]).expect("new helper"), vec!["WELL".to_string()]);

        // it dies: an error, then a new one
        assert!(translate("ar", &["die".into()]).is_err());
        assert_eq!(translate("ar", &["back".into()]).expect("back"), vec!["BACK".to_string()]);
        assert!(health()["restarts"].as_u64().unwrap_or(0) >= 2);
        SPEED.with(|s| s.set(1));
    }

    /// Zombie children of this process (Linux: /proc), to tell a helper wasn't waited for.
    fn zombies() -> usize {
        let me = std::process::id().to_string();
        std::fs::read_dir("/proc")
            .map(|d| {
                d.flatten()
                    .filter_map(|e| std::fs::read_to_string(e.path().join("stat")).ok())
                    .filter(|st| {
                        let after = st.rsplit(')').next().unwrap_or("");
                        let f: Vec<&str> = after.split_whitespace().collect();
                        f.first() == Some(&"Z") && f.get(1) == Some(&me.as_str())
                    })
                    .count()
            })
            .unwrap_or(0)
    }

    #[test]
    fn a_long_session_with_faults_keeps_translating_and_leaves_no_zombies() {
        let _one = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        unsafe { std::env::set_var("VIT_APPLE_TRANSLATE", fake()) };
        SPEED.with(|s| s.set(8));
        let mut ok = 0;
        let mut after_fault_ok = true;
        for i in 0..120 {
            let fault = match i % 20 {
                5 => Some("bad"),
                11 => Some("die"),
                17 => Some("slow"),
                _ => None,
            };
            let text = fault.unwrap_or("line");
            let r = translate("ar", &[text.to_string()]);
            if fault.is_none() && r.is_ok() {
                ok += 1;
            }
            if fault.is_some() {
                // the lines right after a fault: back within three tries
                let back = (0..3).any(|_| translate("ar", &["again".to_string()]).is_ok());
                after_fault_ok &= back;
            }
        }
        SPEED.with(|s| s.set(1));
        assert!(after_fault_ok, "a fault left the translation stopped");
        assert!(ok >= 100, "{ok} of 102 plain lines translated");
        if cfg!(target_os = "linux") {
            std::thread::sleep(Duration::from_millis(1500));
            assert_eq!(zombies(), 0, "a stopped helper wasn't waited for");
        }
    }

    #[test]
    fn the_breaker_pauses_apple_then_tries_again() {
        let mut b = Breaker::new();
        let t = Instant::now();
        assert!(b.allows(t));
        b.failed(t);
        b.failed(t + Duration::from_secs(1));
        assert!(b.allows(t + Duration::from_secs(2)), "two failures: still Apple");
        b.failed(t + Duration::from_secs(2));
        assert!(!b.allows(t + Duration::from_secs(3)), "three in a minute: paused");
        assert!(!b.allows(t + Duration::from_secs(31)));
        assert!(b.allows(t + Duration::from_secs(33)), "tried again after the pause");
        // failures far apart don't open it
        let mut b = Breaker::new();
        for i in 0..5 {
            b.failed(t + Duration::from_secs(70 * i));
        }
        assert!(b.allows(t + Duration::from_secs(400)));
    }

    #[test]
    fn the_time_limit_follows_the_batches() {
        let mut st = State::default();
        assert_eq!(time_limit(&st, false), scaled(FIRST_TIMEOUT));
        assert_eq!(time_limit(&st, true), scaled(MAX_TIMEOUT));
        st.times.extend([Duration::from_millis(100); 20]);
        assert_eq!(time_limit(&st, true), scaled(MIN_TIMEOUT));
        st.times.extend([Duration::from_millis(1500); 5]);
        assert_eq!(time_limit(&st, true), scaled(Duration::from_millis(4500)));
    }
}
