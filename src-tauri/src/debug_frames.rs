//! Debugging aid: while "Save frames" is on, write captured frames to disk as PNG files, one new
//! folder per recording, so detection problems can be reproduced later with `--test-screen` etc.

use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use image::ImageEncoder;
use image::codecs::png::{CompressionType, FilterType, PngEncoder};
use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::capture::{FrameData, Shared};
use crate::config::Config;

#[derive(Default)]
pub struct SaverState {
    /// Folder of the current (or last) recording.
    pub folder: Mutex<Option<PathBuf>>,
    pub count: AtomicU64,
}

#[derive(Serialize, Clone)]
pub struct SaverEvent {
    pub on: bool,
    pub count: u64,
    pub folder: Option<String>,
}

impl SaverState {
    pub fn event(&self, on: bool) -> SaverEvent {
        SaverEvent {
            on,
            count: self.count.load(Ordering::Relaxed),
            folder: self.folder.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_ref().map(|p| p.display().to_string()),
        }
    }
}

/// "2026-11-15_07-41-03" in local time.
pub(crate) fn timestamp() -> String {
    #[cfg(windows)]
    {
        let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
        format!("{:04}-{:02}-{:02}_{:02}-{:02}-{:02}", t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond)
    }
    #[cfg(not(windows))]
    {
        chrono::Local::now().format("%Y-%m-%d_%H-%M-%S").to_string()
    }
}

/// PNG with fast compression: a 4K frame takes a fraction of the time of the default settings.
pub(crate) fn save_png_fast(f: &FrameData, path: &Path) -> anyhow::Result<()> {
    let file = BufWriter::new(std::fs::File::create(path)?);
    PngEncoder::new_with_quality(file, CompressionType::Fast, FilterType::Sub).write_image(
        &f.rgba,
        f.w,
        f.h,
        image::ExtendedColorType::Rgba8,
    )?;
    Ok(())
}

pub fn run(app: AppHandle, cfg: Arc<Mutex<Config>>, shared: Arc<Shared>, state: Arc<SaverState>, base: PathBuf) {
    let mut was_on = false;
    let mut last_seq = 0u64;
    let mut last_save = Instant::now() - Duration::from_secs(60);
    let mut started = Instant::now();
    loop {
        std::thread::sleep(Duration::from_millis(50));
        let (on, interval) = {
            let c = cfg.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            (c.debug_save_frames, Duration::from_millis(c.debug_frame_interval_ms.max(100)))
        };
        if on && !was_on {
            let folder = base.join(timestamp());
            if let Err(e) = std::fs::create_dir_all(&folder) {
                let _ = app.emit("status-msg", format!("Can't create frames folder: {e}"));
                continue; // try again next tick
            }
            *state.folder.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(folder);
            state.count.store(0, Ordering::Relaxed);
            started = Instant::now();
            last_seq = 0;
            let _ = app.emit("frame-saver", state.event(true));
        }
        if !on && was_on {
            let _ = app.emit("frame-saver", state.event(false));
        }
        was_on = on;
        if !on || last_save.elapsed() < interval {
            continue;
        }
        let Some(frame) = shared.latest() else { continue };
        if frame.seq == last_seq {
            continue; // nothing new on screen
        }
        last_seq = frame.seq;
        last_save = Instant::now();
        let Some(folder) = state.folder.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone() else { continue };
        let n = state.count.load(Ordering::Relaxed) + 1;
        // frame_00012_0011500ms.png = 12th frame, 11.5 s after recording started
        let stem = format!("frame_{n:05}_{:07}ms", started.elapsed().as_millis());
        let path = folder.join(format!("{stem}.png"));
        match save_png_fast(&frame, &path) {
            Ok(()) => {
                save_with_translation(&shared, &folder, &stem, n);
                state.count.store(n, Ordering::Relaxed);
                let _ = app.emit("frame-saver", state.event(true));
            }
            Err(e) => {
                let _ = app.emit("status-msg", format!("Saving frame failed: {e:#}"));
            }
        }
    }
}

/// Next to a saved frame: "<stem>_overlay.png" — the game window as seen on screen, Arabic overlays
/// included (only while the game is the active window; otherwise the overlays are hidden) — and
/// "<stem>.json" with the English and Arabic each area showed at that moment.
fn save_with_translation(shared: &Shared, folder: &Path, stem: &str, n: u64) {
    let mut overlay_file = None;
    // (the overlay window's game part, only while it's in front: otherwise the overlays are hidden)
    let shown_at = crate::capture::overlay_rect(shared, true);
    let active = shown_at.is_some();
    {
        if let Some((x, y, w, h)) = shown_at {
            if let Some(rgba) = crate::capture::grab_screen_region(x, y, w, h) {
                let f = FrameData { w: w as u32, h: h as u32, rgba, seq: 0 };
                let p = folder.join(format!("{stem}_overlay.png"));
                if save_png_fast(&f, &p).is_ok() {
                    overlay_file = p.file_name().map(|n| n.to_string_lossy().to_string());
                }
            }
        }
    }
    let note = serde_json::json!({
        "frame": n,
        "frame_file": format!("{stem}.png"),
        "overlay_file": overlay_file,
        "overlay_note": if active { "screen copy with the Arabic overlays" } else { "game window was not active: overlays hidden, no screen copy" },
        "areas": shared.on_screen.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone(),
        "page": shared.screen.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone().map(|mut l| {
            // icons are large base64 images; keep the note readable
            if let Some(bs) = l.get_mut("buttons").and_then(|b| b.as_array_mut()) {
                for b in bs { b["icon"] = serde_json::Value::Null; }
            }
            l
        }),
        "page_reason_ar": shared.screen_reason.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone(),
    });
    let _ = std::fs::write(folder.join(format!("{stem}.json")), serde_json::to_string_pretty(&note).unwrap_or_default());
}

/// Open a folder in File Explorer.
pub fn open_folder(path: &Path) -> std::io::Result<()> {
    std::process::Command::new("explorer").arg(path).spawn().map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saves_a_4k_frame_quickly() {
        let (w, h) = (3840u32, 2053u32);
        let rgba: Vec<u8> = (0..w * h * 4).map(|i| (i % 251) as u8).collect();
        let f = FrameData { w, h, rgba, seq: 1 };
        let dir = std::env::temp_dir().join("vi-translator-frame-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("frame.png");
        let t = Instant::now();
        save_png_fast(&f, &path).unwrap();
        let ms = t.elapsed().as_millis();
        let size = std::fs::metadata(&path).unwrap().len();
        let back = image::open(&path).unwrap();
        assert_eq!((back.width(), back.height()), (w, h));
        println!("saved 4K frame in {ms} ms, {} KB", size / 1024);
        assert!(!timestamp().is_empty());
        println!("timestamp folder name: {}", timestamp());
        std::fs::remove_dir_all(&dir).ok();
    }
}
