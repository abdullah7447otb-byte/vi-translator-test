//! Debugging: a picture of the preview exactly as the app shows it (the game's frame with the translation
//! drawn over it), saved as PNG. The first one of a run makes a new folder in Pictures\VI Translator\previews;
//! the next ones of that run go to the same folder.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

/// The folder this run's pictures go to (made with the first one).
pub static FOLDER: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Where the preview is in the page (CSS pixels) and how wide the page is (CSS pixels): the captured
/// picture of the whole page is cut down to it.
#[derive(serde::Deserialize, Debug, Clone, Copy)]
pub struct Area {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    pub page_w: f64,
}

/// Capture the page, cut out the preview and save it in this run's folder (`base`/<date>): the file's path.
/// A PNG of the whole page of this webview as it is drawn now (dev builds' debug server: dev_debug.rs).
#[cfg(debug_assertions)]
pub async fn capture_png(webview: &tauri::Webview) -> Result<Vec<u8>, String> {
    capture(webview).await
}

pub async fn save(webview: &tauri::Webview, area: Area, base: &Path) -> Result<PathBuf, String> {
    let png = capture(webview).await?;
    let img = image::load_from_memory(&png).map_err(|e| format!("the page's picture: {e}"))?;
    let k = img.width() as f64 / area.page_w.max(1.0);
    let x = (area.x * k).round().max(0.0) as u32;
    let y = (area.y * k).round().max(0.0) as u32;
    let w = ((area.w * k).round() as u32).min(img.width().saturating_sub(x));
    let h = ((area.h * k).round() as u32).min(img.height().saturating_sub(y));
    if w < 8 || h < 8 {
        return Err("the preview isn't showing".into());
    }
    let cut = img.crop_imm(x, y, w, h);
    let dir = {
        let mut f = FOLDER.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        f.get_or_insert_with(|| base.join(crate::debug_frames::timestamp())).clone()
    };
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let n = std::fs::read_dir(&dir).map(|d| d.flatten().filter(|e| e.file_name().to_string_lossy().starts_with("preview_")).count()).unwrap_or(0);
    let path = dir.join(format!("preview_{:03}.png", n + 1));
    cut.save(&path).map_err(|e| e.to_string())?;
    Ok(path)
}

/// The whole page as WebView2 draws it (canvas, video and text included), as PNG.
#[cfg(windows)]
async fn capture(webview: &tauri::Webview) -> Result<Vec<u8>, String> {
    use webview2_com::Microsoft::Web::WebView2::Win32::COREWEBVIEW2_CAPTURE_PREVIEW_IMAGE_FORMAT_PNG;
    use windows::Win32::System::Com::IStream;

    let (tx, rx) = std::sync::mpsc::channel::<Result<Vec<u8>, String>>();
    let tx2 = tx.clone();
    // (on the window's own thread; the answer comes back there too, once it's drawn)
    webview
        .with_webview(move |pw| {
            let started = (|| -> windows::core::Result<()> {
                unsafe {
                    let core = pw.controller().CoreWebView2()?;
                    let stream: IStream = windows::Win32::UI::Shell::SHCreateMemStream(None).ok_or_else(windows::core::Error::from_thread)?;
                    let kept = stream.clone();
                    let done = tx.clone();
                    let handler = webview2_com::CapturePreviewCompletedHandler::create(Box::new(move |r: windows::core::Result<()>| {
                        let _ = done.send(r.map_err(|e| e.to_string()).and_then(|_| read_all(&kept).map_err(|e| e.to_string())));
                        Ok(())
                    }));
                    core.CapturePreview(COREWEBVIEW2_CAPTURE_PREVIEW_IMAGE_FORMAT_PNG, &stream, &handler)?;
                }
                Ok(())
            })();
            if let Err(e) = started {
                let _ = tx2.send(Err(e.to_string()));
            }
        })
        .map_err(|e| e.to_string())?;
    tauri::async_runtime::spawn_blocking(move || rx.recv_timeout(Duration::from_secs(10)).map_err(|_| "the page didn't answer".to_string())?)
        .await
        .map_err(|e| e.to_string())?
}

#[cfg(windows)]
unsafe fn read_all(s: &windows::Win32::System::Com::IStream) -> windows::core::Result<Vec<u8>> {
    use windows::Win32::System::Com::{STATFLAG_NONAME, STATSTG, STREAM_SEEK_SET};
    let mut stat = STATSTG::default();
    unsafe {
        s.Stat(&mut stat, STATFLAG_NONAME)?;
        s.Seek(0, STREAM_SEEK_SET, None)?;
        let mut buf = vec![0u8; stat.cbSize as usize];
        let mut read = 0u32;
        s.Read(buf.as_mut_ptr().cast(), buf.len() as u32, Some(&mut read)).ok()?;
        buf.truncate(read as usize);
        Ok(buf)
    }
}

#[cfg(not(windows))]
async fn capture(_webview: &tauri::Webview) -> Result<Vec<u8>, String> {
    Err("saving the preview works on Windows only for now".into())
}
