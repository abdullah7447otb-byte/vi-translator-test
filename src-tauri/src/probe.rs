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
        // that has held for a second, or 20 s)
        while started.elapsed() < Duration::from_secs(20) {
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
        results.insert(name, serde_json::json!({ "file": path, "w": f.w, "h": f.h, "phone": phone, "anchor": anchor, "settled": settled_at.is_some() }));
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
