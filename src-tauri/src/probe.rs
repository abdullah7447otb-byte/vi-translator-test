//! `vi-translator --probe-phone <out.json> <frame.jpg>…`: the app's own reading (the engine, the host's OCR,
//! the chosen translator) run on still frames of GTA VI's phone, as the running app would on a phone held
//! still: each frame given to the engine for a few seconds, then a dark frame until the phone closes. What
//! comes out is the `phone` events the windows draw, for each frame (the tests render them over the frame).
//!
//! The translator: the app's offline one by default; `VIT_PROBE_ENGINE` and `VIT_PROBE_LIBRE` choose another
//! (a LibreTranslate-compatible server). Nothing of the player's (settings, memory) is read or written: the
//! memory is a fresh one in a temporary folder.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vi_translator_core::config::Config;
use vi_translator_core::frame::FrameData;
use vi_translator_core::{engine, live, memory};

use vi_translator_core::ocr::Ocr;

use crate::ocr_engine::PlatformOcr;

struct Capture(Mutex<Vec<(String, serde_json::Value)>>);

impl vi_translator_core::Sink for Capture {
    fn emit(&self, event: &str, payload: serde_json::Value) {
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push((event.to_string(), payload));
    }
}

fn frame(path: &str, seq: u64) -> anyhow::Result<Arc<FrameData>> {
    let img = image::open(path)?.to_rgba8();
    let (w, h) = img.dimensions();
    Ok(Arc::new(FrameData { w, h, rgba: img.into_raw(), seq }))
}

pub fn probe_phone(out: &str, files: &[String]) -> anyhow::Result<()> {
    let mut cfg = Config::default();
    cfg.target_lang = "ar".into();
    cfg.ui_lang = "ar".into();
    if let Ok(e) = std::env::var("VIT_PROBE_ENGINE") {
        cfg.engine = e;
    }
    if let Ok(u) = std::env::var("VIT_PROBE_LIBRE") {
        cfg.libre_url = u;
    }
    let tmp = std::env::temp_dir().join(format!("vit-probe-{}", std::process::id()));
    std::fs::create_dir_all(&tmp)?;
    let sink = Arc::new(Capture(Mutex::new(Vec::new())));
    let ctx = engine::Context {
        cfg: Arc::new(Mutex::new(cfg.clone())),
        live: Arc::new(live::Live::default()),
        memory: Arc::new(Mutex::new(memory::Memory::load(tmp.clone(), &cfg.target_lang))),
        sink: sink.clone(),
        paths: vi_translator_core::sync::Paths { data: crate::data_dir(), db: tmp.clone(), config: tmp.join("config.json") },
    };
    Ocr::init_thread();
    let ocr = Ocr::new()?;
    let mut reader = engine::start(ctx);
    let mut seq = 0u64;
    let mut results = serde_json::Map::new();
    for path in files {
        let f = frame(path, 0)?;
        let dark = Arc::new(FrameData { w: f.w, h: f.h, rgba: vec![0u8; f.rgba.len()], seq: 0 });
        sink.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clear();
        let started = Instant::now();
        let mut settled_at: Option<Instant> = None;
        // (the same frame again and again, as a phone held still: until every message has its Arabic and
        // that has held for a second, or 60 s — a slow computer, a virtual Mac, reads a frame in seconds)
        while started.elapsed() < Duration::from_secs(60) {
            seq += 1;
            let fr = Arc::new(FrameData { w: f.w, h: f.h, rgba: f.rgba.clone(), seq });
            let wait = reader.step(engine::Input::Frame(&fr, &ocr));
            let last = sink.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).iter().rev().find(|(e, _)| e == "phone").map(|(_, v)| v.clone());
            let done = last.as_ref().is_some_and(|v| !v.is_null() && v["pieces"].as_array().is_some_and(|p| !p.is_empty() && p.iter().all(|x| x["pending"] != true)));
            match (done, settled_at) {
                (true, None) => settled_at = Some(Instant::now()),
                (true, Some(t)) if t.elapsed() > Duration::from_secs(1) => break,
                (false, _) => settled_at = None,
                _ => {}
            }
            std::thread::sleep(wait.clamp(Duration::from_millis(20), Duration::from_millis(120)));
        }
        let events = sink.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        let phone = events.iter().rev().find(|(e, _)| e == "phone").map(|(_, v)| v.clone()).unwrap_or(serde_json::Value::Null);
        let anchor = events.iter().rev().find(|(e, _)| e == "phone-anchor").map(|(_, v)| v.clone()).unwrap_or(serde_json::Value::Null);
        let name = std::path::Path::new(path).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        vi_translator_core::vlog!("probe-phone: {name}: {} pieces, {:.1} s", phone["pieces"].as_array().map_or(0, |p| p.len()), started.elapsed().as_secs_f64());
        // (and the chat as the home-network page's chat view shows it, read only)
        let chat = if phone.is_null() { serde_json::Value::Null } else { vi_translator_core::lan_page::chat_view(&phone) };
        results.insert(name, serde_json::json!({ "file": path, "w": f.w, "h": f.h, "phone": phone, "chat": chat, "anchor": anchor, "settled": settled_at.is_some() }));
        // (put away: dark frames until the phone has closed)
        let gone = Instant::now();
        while gone.elapsed() < Duration::from_secs(4) {
            seq += 1;
            let d = Arc::new(FrameData { w: dark.w, h: dark.h, rgba: dark.rgba.clone(), seq });
            let wait = reader.step(engine::Input::Frame(&d, &ocr));
            std::thread::sleep(wait.clamp(Duration::from_millis(20), Duration::from_millis(120)));
        }
    }
    std::fs::write(out, serde_json::to_string_pretty(&serde_json::Value::Object(results))?)?;
    let _ = std::fs::remove_dir_all(&tmp);
    Ok(())
}

/// `vi-translator --ocr-boxes <picture> <boxes.json>`: the English words the host's OCR reads inside each
/// box (fractions of the picture: [[x, y, w, h], …]), as JSON — the renders' check that no English is left
/// in a translated bubble.
pub fn ocr_boxes(picture: &str, boxes: &str) -> anyhow::Result<()> {
    let img = image::open(picture)?.to_rgba8();
    let (w, h) = img.dimensions();
    let rgba = img.into_raw();
    let boxes: Vec<[f64; 4]> = serde_json::from_str(&std::fs::read_to_string(boxes)?)?;
    Ocr::init_thread();
    let ocr = Ocr::new()?;
    let mut out = Vec::new();
    for b in boxes {
        let Some(crop) = vi_translator_core::ocr::Crop::from_frame(&rgba, w, h, b[0], b[1], b[2], b[3]) else {
            out.push(serde_json::json!([]));
            continue;
        };
        let (words, _) = ocr.recognize_checked(&crop)?;
        let latin: Vec<String> = words.iter().map(|w| w.text.clone()).filter(|t| t.chars().filter(|c| c.is_ascii_alphabetic()).count() >= 2).collect();
        out.push(serde_json::json!(latin));
    }
    println!("{}", serde_json::Value::Array(out));
    Ok(())
}

/// This process's resident memory, in kB (None where it can't be told).
fn rss_kb() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let s = std::fs::read_to_string("/proc/self/status").ok()?;
        s.lines().find_map(|l| l.strip_prefix("VmRSS:")).and_then(|v| v.split_whitespace().next()).and_then(|v| v.parse().ok())
    }
    #[cfg(target_os = "macos")]
    {
        let o = std::process::Command::new("ps").args(["-o", "rss=", "-p", &std::process::id().to_string()]).output().ok()?;
        String::from_utf8_lossy(&o.stdout).trim().parse().ok()
    }
    #[cfg(windows)]
    {
        let o = std::process::Command::new("powershell").args(["-NoProfile", "-Command", &format!("(Get-Process -Id {}).WorkingSet64", std::process::id())]).output().ok()?;
        String::from_utf8_lossy(&o.stdout).trim().parse::<u64>().ok().map(|b| b / 1024)
    }
}

/// `vi-translator --soak <minutes> <out.md> <frame>…`: the engine fed the frames over and over (each
/// held for 4 s, as a scene stays on screen: the phone ones and the subtitle ones), at the reading's
/// own pace, for `minutes`. Every pass through all the frames: the process's memory and the time
/// the reading took. Pass: memory grows at most 30 MB after the first 2 minutes, the reading's time
/// per pass has no upward trend over 5 %, and no panic. Writes the table to `out.md`; exits 1 on a
/// failed check.
pub fn soak(minutes: f64, out: &str, files: &[String]) -> anyhow::Result<bool> {
    let mut cfg = Config::default();
    cfg.target_lang = "ar".into();
    if let Ok(e) = std::env::var("VIT_PROBE_ENGINE") {
        cfg.engine = e;
    }
    if let Ok(u) = std::env::var("VIT_PROBE_LIBRE") {
        cfg.libre_url = u;
    }
    let tmp = std::env::temp_dir().join(format!("vit-soak-{}", std::process::id()));
    std::fs::create_dir_all(&tmp)?;
    let sink = Arc::new(Capture(Mutex::new(Vec::new())));
    let ctx = engine::Context {
        cfg: Arc::new(Mutex::new(cfg.clone())),
        live: Arc::new(live::Live::default()),
        memory: Arc::new(Mutex::new(memory::Memory::load(tmp.clone(), &cfg.target_lang))),
        sink: sink.clone(),
        paths: vi_translator_core::sync::Paths { data: crate::data_dir(), db: tmp.clone(), config: tmp.join("config.json") },
    };
    Ocr::init_thread();
    let ocr = Ocr::new()?;
    let frames: Vec<Arc<FrameData>> = files.iter().map(|p| frame(p, 0)).collect::<anyhow::Result<_>>()?;
    let mut reader = engine::start(ctx);
    let started = Instant::now();
    let end = Duration::from_secs_f64(minutes * 60.0);
    let (mut seq, mut pass) = (0u64, 0u32);
    // (pass, minutes, memory MB, reading ms in the pass, steps)
    let mut rows: Vec<(u32, f64, f64, f64, u32)> = Vec::new();
    let mut events = 0usize;
    while started.elapsed() < end {
        pass += 1;
        let (mut busy, mut steps) = (Duration::ZERO, 0u32);
        for f in &frames {
            let held = Instant::now();
            while held.elapsed() < Duration::from_secs(4) {
                seq += 1;
                let fr = Arc::new(FrameData { w: f.w, h: f.h, rgba: f.rgba.clone(), seq });
                let t = Instant::now();
                let wait = reader.step(engine::Input::Frame(&fr, &ocr));
                busy += t.elapsed();
                steps += 1;
                // (what the windows would get: counted, not kept)
                let mut got = sink.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                events += got.len();
                got.clear();
                drop(got);
                std::thread::sleep(wait.clamp(Duration::from_millis(16), Duration::from_millis(120)));
            }
        }
        let mb = rss_kb().map(|k| k as f64 / 1024.0).unwrap_or(0.0);
        let m = started.elapsed().as_secs_f64() / 60.0;
        vi_translator_core::vlog!("[soak] pass {pass}: {m:.1} min, {mb:.0} MB, reading {:.0} ms in {steps} steps", busy.as_secs_f64() * 1000.0);
        rows.push((pass, m, mb, busy.as_secs_f64() * 1000.0, steps));
    }
    drop(reader);
    let _ = std::fs::remove_dir_all(&tmp);
    // memory after the first 2 minutes
    let warm: Vec<&(u32, f64, f64, f64, u32)> = rows.iter().filter(|r| r.1 >= 2.0).collect();
    let base = warm.first().map(|r| r.2).unwrap_or(0.0);
    let peak = warm.iter().map(|r| r.2).fold(base, f64::max);
    let growth = peak - base;
    // the reading's time per pass: its trend over the run (least squares), as a share of its mean
    let (n, xs, ys): (f64, Vec<f64>, Vec<f64>) = (warm.len() as f64, warm.iter().map(|r| r.1).collect(), warm.iter().map(|r| r.3).collect());
    let (mx, my) = (xs.iter().sum::<f64>() / n.max(1.0), ys.iter().sum::<f64>() / n.max(1.0));
    let slope = {
        let num: f64 = xs.iter().zip(&ys).map(|(x, y)| (x - mx) * (y - my)).sum();
        let den: f64 = xs.iter().map(|x| (x - mx).powi(2)).sum();
        if den > 0.0 { num / den } else { 0.0 }
    };
    let span = xs.last().copied().unwrap_or(0.0) - xs.first().copied().unwrap_or(0.0);
    let trend = if my > 0.0 { slope * span / my * 100.0 } else { 0.0 };
    let ok_mem = growth <= 30.0;
    let ok_time = trend <= 5.0;
    let mut md = vec![
        "# Soak: the engine for a long session".to_string(),
        String::new(),
        format!("{} frames ({}), each held 4 s, over and over for {minutes:.0} min at the reading's own pace; {events} events out.", frames.len(), files.iter().map(|f| std::path::Path::new(f).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()).collect::<Vec<_>>().join(", ")),
        String::new(),
        format!("- Memory after 2 min: {base:.0} MB, peak {peak:.0} MB: grew {growth:.1} MB (pass: ≤ 30 MB) — {}", if ok_mem { "pass" } else { "FAIL" }),
        format!("- Reading time per pass: mean {my:.0} ms, trend over the run {trend:+.1} % (pass: ≤ +5 %) — {}", if ok_time { "pass" } else { "FAIL" }),
        String::new(),
        "| Pass | Minutes | Memory (MB) | Reading (ms) | Steps |".to_string(),
        "|---|---|---|---|---|".to_string(),
    ];
    for r in &rows {
        md.push(format!("| {} | {:.1} | {:.0} | {:.0} | {} |", r.0, r.1, r.2, r.3, r.4));
    }
    std::fs::write(out, md.join("\n") + "\n")?;
    println!("{}", md[4..6].join("\n"));
    Ok(ok_mem && ok_time)
}
