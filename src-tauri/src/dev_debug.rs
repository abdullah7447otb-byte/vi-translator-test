//! Dev builds only: a small HTTP server on 127.0.0.1 for looking at the live preview from outside the
//! app (a developer's scripts, an assistant debugging with them): pictures of the app's windows as they
//! are drawn now, the game's frame now, what the overlays are given to draw, and a page's JavaScript run
//! with its answer back. Nothing of it is in a release build.
//!
//! Every request carries the token written at start to `<data dir>/dev-debug.json` (`?t=…`): a web page
//! open in a browser can send requests to 127.0.0.1 too, and must not be able to run code in the app.
//!
//! GET /windows                    the app's windows (labels)
//! GET /shot?w=main                a PNG of that window's page as drawn now (saved; its path back)
//! GET /frame                      a PNG of the game's frame now (saved; its path back)
//! GET /state                      the phone's, the game page's and the overlays' data now
//! GET /pad?b=cross,down&ms=120    press the PS5's controller buttons (playing in the app: PS5 Remote Play) for
//!                                 ms milliseconds, and sticks (lx, ly, rx, ry: -32768..32767; l2, r2: 0..255)
//! GET /eval?w=main&js=…           run JavaScript in that window; what it gives `__devReport(v)` comes back
//!                                 (waits up to 5 s; a plain expression: wrap it, `__devReport(1 + 1)`)

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tauri::{AppHandle, Manager};

use crate::capture::Shared;

pub const PORT: u16 = 47811;

/// The answers pages gave `__devReport` (see `dev_report`), by request id.
static REPORTS: Mutex<Option<HashMap<String, serde_json::Value>>> = Mutex::new(None);

/// A page's answer to an /eval (the `dev_report` command: dev builds only).
pub fn report(id: String, value: serde_json::Value) {
    REPORTS.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get_or_insert_with(HashMap::new).insert(id, value);
}

pub fn run(app: AppHandle, shared: Arc<Shared>, data: PathBuf, out: PathBuf) {
    let listener = match TcpListener::bind(("127.0.0.1", PORT)) {
        Ok(l) => l,
        Err(e) => {
            vi_translator_core::vlog!("[dev-debug] port {PORT}: {e}");
            return;
        }
    };
    let token = format!("{:016x}{:016x}", rand_u64(), rand_u64());
    let info = serde_json::json!({ "url": format!("http://127.0.0.1:{PORT}"), "token": token, "out": out });
    let _ = std::fs::create_dir_all(&data);
    let _ = std::fs::write(data.join("dev-debug.json"), serde_json::to_string_pretty(&info).unwrap_or_default());
    vi_translator_core::vlog!("[dev-debug] http://127.0.0.1:{PORT} (token in {})", data.join("dev-debug.json").display());
    for s in listener.incoming().flatten() {
        let (app, shared, out, token) = (app.clone(), shared.clone(), out.clone(), token.clone());
        std::thread::spawn(move || {
            if let Err(e) = handle(s, &app, &shared, &out, &token) {
                vi_translator_core::vlog!("[dev-debug] {e}");
            }
        });
    }
}

fn rand_u64() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(Instant::now().elapsed().as_nanos() ^ std::process::id() as u128);
    h.write_u64(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0));
    h.finish()
}

fn handle(s: TcpStream, app: &AppHandle, shared: &Shared, out: &PathBuf, token: &str) -> Result<(), String> {
    let mut r = BufReader::new(s.try_clone().map_err(|e| e.to_string())?);
    let mut first = String::new();
    r.read_line(&mut first).map_err(|e| e.to_string())?;
    // (the rest of the request's head: not needed)
    let mut l = String::new();
    while r.read_line(&mut l).map_err(|e| e.to_string())? > 2 {
        l.clear();
    }
    let target = first.split_whitespace().nth(1).unwrap_or("/");
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let q: HashMap<String, String> = query.split('&').filter_map(|kv| kv.split_once('=')).map(|(k, v)| (k.to_string(), decode(v))).collect();
    let (code, body) = if q.get("t").map(String::as_str) != Some(token) {
        (403, serde_json::json!({ "error": "token: see dev-debug.json" }))
    } else {
        match answer(path, &q, app, shared, out) {
            Ok(v) => (200, v),
            Err(e) => (500, serde_json::json!({ "error": e })),
        }
    };
    let body = serde_json::to_string_pretty(&body).unwrap_or_default();
    let mut w = s;
    write!(w, "HTTP/1.1 {code} X\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
        .map_err(|e| e.to_string())
}

fn answer(path: &str, q: &HashMap<String, String>, app: &AppHandle, shared: &Shared, out: &PathBuf) -> Result<serde_json::Value, String> {
    let stamp = || crate::debug_frames::timestamp();
    match path {
        "/windows" => Ok(serde_json::json!(app.webview_windows().keys().cloned().collect::<Vec<_>>())),
        "/shot" => {
            let label = q.get("w").map(String::as_str).unwrap_or("main");
            let ww = app.get_webview_window(label).ok_or_else(|| format!("no window {label:?}: see /windows"))?;
            let png = tauri::async_runtime::block_on(crate::preview_shot::capture_png(ww.as_ref()))?;
            std::fs::create_dir_all(out).map_err(|e| e.to_string())?;
            let file = out.join(format!("{}_{}.png", stamp(), label.replace(|c: char| !c.is_alphanumeric(), "_")));
            std::fs::write(&file, png).map_err(|e| e.to_string())?;
            Ok(serde_json::json!({ "file": file }))
        }
        "/frame" => {
            let f = shared.latest().ok_or("no frame yet: pick a source")?;
            std::fs::create_dir_all(out).map_err(|e| e.to_string())?;
            let file = out.join(format!("{}_frame.png", stamp()));
            crate::debug_frames::save_png_fast(&f, &file).map_err(|e| format!("{e:#}"))?;
            Ok(serde_json::json!({ "file": file, "w": f.w, "h": f.h }))
        }
        "/state" => Ok(serde_json::json!({
            "phone": shared.phone.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone(),
            "phone_active": shared.phone_active.load(std::sync::atomic::Ordering::Relaxed),
            "screen": shared.screen.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone(),
            "screen_reason": shared.screen_reason.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone(),
            "covers": shared.covers.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone(),
        })),
        "/pad" => {
            use vi_translator_ps::buttons as b;
            let mut p = vi_translator_ps::Pad::default();
            for name in q.get("b").map(String::as_str).unwrap_or("").split(',').filter(|n| !n.is_empty()) {
                p.buttons |= match name.trim().to_lowercase().as_str() {
                    "cross" | "x" => b::CROSS,
                    "circle" | "o" => b::CIRCLE,
                    "square" => b::SQUARE,
                    "triangle" => b::TRIANGLE,
                    "left" => b::LEFT,
                    "right" => b::RIGHT,
                    "up" => b::UP,
                    "down" => b::DOWN,
                    "l1" => b::L1,
                    "r1" => b::R1,
                    "l3" => b::L3,
                    "r3" => b::R3,
                    "options" => b::OPTIONS,
                    "share" | "create" => b::SHARE,
                    "touchpad" => b::TOUCHPAD,
                    "ps" => b::PS,
                    other => return Err(format!("no button {other:?}")),
                };
            }
            let num = |k: &str| q.get(k).and_then(|v| v.parse::<i64>().ok());
            p.lx = num("lx").unwrap_or(0).clamp(-32768, 32767) as i16;
            p.ly = num("ly").unwrap_or(0).clamp(-32768, 32767) as i16;
            p.rx = num("rx").unwrap_or(0).clamp(-32768, 32767) as i16;
            p.ry = num("ry").unwrap_or(0).clamp(-32768, 32767) as i16;
            p.l2 = num("l2").unwrap_or(0).clamp(0, 255) as u8;
            p.r2 = num("r2").unwrap_or(0).clamp(0, 255) as u8;
            let ms = num("ms").unwrap_or(120).clamp(30, 5000) as u64;
            vi_translator_ps::play::press(p, Duration::from_millis(ms));
            // (held for its time, then let go: the answer once it has been)
            std::thread::sleep(Duration::from_millis(ms + 60));
            Ok(serde_json::json!({ "pressed": format!("{:#x}", p.buttons), "ms": ms }))
        }
        "/eval" => {
            let label = q.get("w").map(String::as_str).unwrap_or("main");
            let js = q.get("js").ok_or("js=…")?;
            let ww = app.get_webview_window(label).ok_or_else(|| format!("no window {label:?}: see /windows"))?;
            let id = format!("{:x}", rand_u64());
            let id_js = serde_json::to_string(&id).unwrap_or_default();
            // (the page's answer comes back through the dev_report command; an error is an answer too)
            let wrapped = format!(
                "(async()=>{{const __devReport=(v)=>window.__TAURI__.core.invoke('dev_report',{{id:{id_js},value:v===undefined?null:v}});\
                 try{{{js}\n}}catch(e){{__devReport({{error:String(e&&e.stack||e)}})}}}})()"
            );
            ww.eval(&wrapped).map_err(|e| e.to_string())?;
            let until = Instant::now() + Duration::from_secs(5);
            while Instant::now() < until {
                if let Some(v) = REPORTS.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_mut().and_then(|m| m.remove(&id)) {
                    return Ok(v);
                }
                std::thread::sleep(Duration::from_millis(30));
            }
            Ok(serde_json::json!({ "note": "ran; no __devReport(…) within 5 s" }))
        }
        _ => Err("GET /windows, /shot?w=, /frame, /state, /eval?w=&js=".into()),
    }
}

/// %xx and + in a query value.
fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                match u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("zz"), 16) {
                    Ok(v) => {
                        out.push(v);
                        i += 2;
                    }
                    Err(_) => out.push(b'%'),
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    #[test]
    fn decodes_query_values() {
        assert_eq!(super::decode("a%20b+c%2Bd"), "a b c+d");
        assert_eq!(super::decode("100%"), "100%");
        assert_eq!(super::decode("%D8%A7"), "ا");
    }
}
