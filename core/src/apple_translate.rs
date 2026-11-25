//! Apple's on-device translation (macOS 26 and later) through the app's helper, `vit-apple-translate`, next
//! to the app's own program in the .app (src-tauri/apple): started once and kept, newline-delimited JSON
//! over its stdin/stdout, a time limit per batch, started again after a crash or a timeout.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{anyhow, bail, Context as _, Result};
use serde_json::{json, Value};

/// A batch's time limit.
pub const TIMEOUT: Duration = Duration::from_secs(3);

struct Helper {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
    next_id: u64,
}

static HELPER: Mutex<Option<Helper>> = Mutex::new(None);

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
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    Ok(Helper { child, stdin, lines: rx, next_id: 1 })
}

/// One request and its answer (the helper started if it isn't; stopped after a timeout or an error of its
/// own, so the next request starts it again).
fn ask(mut request: Value, timeout: Duration) -> Result<Value> {
    let path = helper_path().ok_or_else(|| anyhow!("the Apple translation helper isn't in this build"))?;
    let mut guard = HELPER.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if guard.as_mut().is_none_or(|h| h.child.try_wait().ok().flatten().is_some()) {
        *guard = Some(start(&path)?);
    }
    let h = guard.as_mut().expect("started");
    let id = h.next_id;
    h.next_id += 1;
    request["id"] = json!(id);
    let sent = writeln!(h.stdin, "{request}").and_then(|_| h.stdin.flush());
    if let Err(e) = sent {
        *guard = None;
        bail!("the Apple translation helper stopped: {e}");
    }
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        match h.lines.recv_timeout(left) {
            Ok(line) => {
                let v: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
                // (an answer to an earlier request that timed out: not this one's)
                if v["id"].as_u64() != Some(id) {
                    continue;
                }
                if let Some(e) = v["error"].as_str() {
                    bail!("Apple translation: {e}");
                }
                return Ok(v);
            }
            Err(RecvTimeoutError::Timeout) => {
                if let Some(mut old) = guard.take() {
                    let _ = old.child.kill();
                }
                bail!("Apple translation took longer than {} s", timeout.as_secs());
            }
            Err(RecvTimeoutError::Disconnected) => {
                *guard = None;
                bail!("the Apple translation helper stopped");
            }
        }
    }
}

/// The lines translated from English into `dst` (a language code: "ar").
pub fn translate(dst: &str, texts: &[String]) -> Result<Vec<String>> {
    let v = ask(json!({ "src": "en", "dst": dst, "texts": texts }), TIMEOUT)?;
    let out: Vec<String> = v["out"].as_array().ok_or_else(|| anyhow!("Apple translation: no answer"))?.iter().map(|s| s.as_str().unwrap_or_default().to_string()).collect();
    if out.len() != texts.len() {
        bail!("Apple translation answered {} lines for {}", out.len(), texts.len());
    }
    Ok(out)
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// A fake helper (a shell script): echoes each text back upper-cased with its request's id; "slow" makes it
    /// sleep past the time limit; "die" makes it exit.
    fn fake() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("vit-apple-fake-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("vit-apple-translate");
        std::fs::write(
            &p,
            r#"#!/bin/sh
if [ "$1" = "--status" ]; then echo installed; exit 0; fi
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  case "$line" in
    *slow*) sleep 5 ;;
    *die*) exit 1 ;;
  esac
  t=$(printf '%s' "$line" | sed -n 's/.*"texts":\["\([^"]*\)".*/\1/p' | tr a-z A-Z)
  printf '{"id":%s,"out":["%s"]}\n' "$id" "$t"
done
"#,
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    #[test]
    fn the_helper_answers_in_order_and_is_started_again_after_a_timeout_or_a_crash() {
        unsafe { std::env::set_var("VIT_APPLE_TRANSLATE", fake()) };
        assert_eq!(status("ar"), "installed");
        assert_eq!(translate("ar", &["hello".into()]).unwrap(), vec!["HELLO".to_string()]);
        assert_eq!(translate("ar", &["again".into()]).unwrap(), vec!["AGAIN".to_string()]);
        // too slow: an error after the time limit, and the next request goes to a new helper
        let t0 = std::time::Instant::now();
        assert!(translate("ar", &["slow".into()]).unwrap_err().to_string().contains("longer than"));
        assert!(t0.elapsed() < Duration::from_secs(5));
        assert_eq!(translate("ar", &["after".into()]).unwrap(), vec!["AFTER".to_string()]);
        // it dies: an error, then a new one
        assert!(translate("ar", &["die".into()]).is_err());
        assert_eq!(translate("ar", &["back".into()]).unwrap(), vec!["BACK".to_string()]);
    }
}
