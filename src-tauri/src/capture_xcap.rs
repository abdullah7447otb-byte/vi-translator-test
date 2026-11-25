//! Window capture on Linux and macOS, with the xcap crate (X11 / PipeWire on Linux,
//! ScreenCaptureKit / CoreGraphics on macOS).
//!
//! A thread grabs the chosen window at the capture rate (and small preview frames at the preview
//! rate while the preview is watched). Window "handles" are xcap's window ids.
//!
//! On macOS the window list and the window captures are the app's own (module `mac` at the end of this file): xcap
//! leaves out every window without a title, and the PS Remote Play window can be one.
//!
//! Limits: on Linux Wayland, other apps' windows can't be listed or captured; use an OBS source
//! or a capture card there (X11 and XWayland windows work). On macOS the app needs the Screen
//! Recording permission (System Settings > Privacy & Security).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
#[cfg(not(target_os = "macos"))]
use std::time::{Duration, Instant};

#[cfg(not(target_os = "macos"))]
use anyhow::{Result, anyhow};
#[cfg(not(target_os = "macos"))]
use xcap::Window;

use super::{FrameData, Shared, WindowInfo, shrink};

/// Screens as a source: Windows only for now.
pub fn list_screens() -> Vec<super::ScreenInfo> {
    Vec::new()
}
pub fn screen_rect(_id: &str) -> Option<(i32, i32, i32, i32)> {
    None
}
pub fn start_screen(_id: &str, _shared: std::sync::Arc<super::Shared>) -> anyhow::Result<WindowCapture> {
    Err(anyhow::anyhow!("A whole screen as the source is only on Windows for now."))
}

#[cfg(target_os = "macos")]
pub use mac::{client_rect, find_window, focused_client_rect, foreground_window, grab_once, list_windows, start_window};

/// A running window capture.
pub struct WindowCapture {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl WindowCapture {
    pub fn is_finished(&self) -> bool {
        self.thread.as_ref().is_none_or(|t| t.is_finished())
    }
    /// (the compatibility capture is Windows only)
    pub fn is_compat(&self) -> bool {
        false
    }
    pub fn stop(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

// (dropped without `stop` — its owner ended on an error — it still ends, rather than running on
// beside the one started in its place)
impl Drop for WindowCapture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

#[cfg(not(target_os = "macos"))]
fn find(title: &str) -> Option<Window> {
    let all = Window::all().ok()?;
    let exact = all.iter().position(|w| w.title().ok().as_deref() == Some(title));
    let idx = exact.or_else(|| all.iter().position(|w| w.title().is_ok_and(|t| !t.is_empty() && t.contains(title))))?;
    all.into_iter().nth(idx)
}

#[cfg(not(target_os = "macos"))]
fn by_id(id: isize) -> Option<Window> {
    Window::all().ok()?.into_iter().find(|w| w.id().ok().map(|i| i as isize) == Some(id))
}

#[cfg(not(target_os = "macos"))]
/// Start capturing the window titled `title`; returns the capture and the window's id.
pub fn start_window(title: &str, shared: Arc<Shared>) -> Result<(WindowCapture, isize)> {
    let win = find(title).ok_or_else(|| anyhow!("Window \"{title}\" not found. Open it, or pick another window."))?;
    let id = win.id().map_err(|e| anyhow!("Capture failed: {e}"))? as isize;
    // one frame now: fails fast when capture isn't allowed (macOS Screen Recording permission)
    win.capture_image().map_err(|e| anyhow!("Capture failed: {e}"))?;
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let thread = std::thread::spawn(move || {
        let ago = Instant::now() - Duration::from_secs(1);
        let (mut last, mut last_preview) = (ago, ago);
        let mut misses = 0;
        while !flag.load(Ordering::Relaxed) {
            let full = shared.full_due(last);
            let preview = shared.preview_due(last_preview);
            if full || preview {
                match win.capture_image() {
                    Ok(img) => {
                        misses = 0;
                        let (w, h) = img.dimensions();
                        if preview {
                            last_preview = Instant::now();
                            shared.put_preview(shrink(img.as_raw(), w, h, (w * 4) as usize, false));
                        }
                        if full {
                            last = Instant::now();
                            shared.put_full(w, h, img.into_raw());
                        }
                    }
                    // the window closed (or can't be read any more): end, so the manager restarts
                    Err(_) => {
                        misses += 1;
                        if misses > 20 {
                            break;
                        }
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(8));
        }
    });
    Ok((WindowCapture { stop, thread: Some(thread) }, id))
}

#[cfg(not(target_os = "macos"))]
/// Capture a single frame of a window (used by `--grab` for building and testing profiles).
pub fn grab_once(title: &str) -> Result<Arc<FrameData>> {
    let win = find(title).ok_or_else(|| anyhow!("window \"{title}\" not found"))?;
    let img = win.capture_image().map_err(|e| anyhow!("capture failed: {e}"))?;
    let (w, h) = img.dimensions();
    Ok(Arc::new(FrameData { w, h, rgba: img.into_raw(), seq: 1 }))
}

#[cfg(not(target_os = "macos"))]
/// Visible windows that can be captured (excluding this app's own windows).
pub fn list_windows(own_title: &str) -> Vec<WindowInfo> {
    let mut out: Vec<WindowInfo> = Window::all()
        .unwrap_or_default()
        .into_iter()
        .filter(|w| !w.is_minimized().unwrap_or(false))
        .filter_map(|w| {
            let title = w.title().ok()?.trim().to_string();
            if title.is_empty() || title == own_title || title.starts_with("VI Translator overlay") {
                return None;
            }
            Some(WindowInfo { title, process: w.app_name().unwrap_or_default() })
        })
        .collect();
    out.sort_by_key(|w| w.title.to_lowercase());
    out.dedup_by(|a, b| a.title == b.title);
    out
}

#[cfg(not(target_os = "macos"))]
/// The window's area on screen: (x, y, width, height).
pub fn client_rect(id: isize) -> Option<(i32, i32, i32, i32)> {
    if id == 0 {
        return None;
    }
    rect_of(&by_id(id)?)
}

#[cfg(not(target_os = "macos"))]
/// The window's rect, only while it has the focus. Asks about this one window: here every xcap
/// query is costly (a new X connection on Linux, a copy of the whole window list on macOS), so the
/// overlays' 150 ms loop doesn't go through all the windows.
pub fn focused_client_rect(id: isize) -> Option<(i32, i32, i32, i32)> {
    if id == 0 {
        return None;
    }
    let w = by_id(id)?;
    if !w.is_focused().unwrap_or(false) {
        return None;
    }
    rect_of(&w)
}

#[cfg(not(target_os = "macos"))]
fn rect_of(w: &Window) -> Option<(i32, i32, i32, i32)> {
    // (macOS lists windows on screen only: a minimised one isn't found at all)
    if cfg!(not(target_os = "macos")) && w.is_minimized().unwrap_or(false) {
        return None;
    }
    let (ww, hh) = (w.width().ok()? as i32, w.height().ok()? as i32);
    (ww > 50 && hh > 50).then_some((w.x().ok()?, w.y().ok()?, ww, hh))
}

#[cfg(not(target_os = "macos"))]
/// The id of the focused window (0 if none is known).
pub fn foreground_window() -> isize {
    Window::all()
        .ok()
        .and_then(|all| all.into_iter().find(|w| w.is_focused().unwrap_or(false)))
        .and_then(|w| w.id().ok())
        .map(|i| i as isize)
        .unwrap_or(0)
}

#[cfg(not(target_os = "macos"))]
/// The id of the window titled `title`.
pub fn find_window(title: &str) -> Option<isize> {
    find(title).and_then(|w| w.id().ok()).map(|i| i as isize)
}

/// Copy a region of the screen as RGBA (what the user sees there, overlays included).
pub fn grab_screen_region(x: i32, y: i32, w: i32, h: i32) -> Option<Vec<u8>> {
    if w <= 0 || h <= 0 {
        return None;
    }
    let m = xcap::Monitor::from_point(x, y).ok()?;
    let (mx, my) = (m.x().ok()?, m.y().ok()?);
    let img = m.capture_region((x - mx).max(0) as u32, (y - my).max(0) as u32, w as u32, h as u32).ok()?;
    Some(img.into_raw())
}

// ------------------------------------------------------------------------------------------- macOS

/// The window list and window captures on macOS, from CoreGraphics directly. xcap leaves out every
/// window that has no title (`kCGWindowName`) — a game-streaming window like PS Remote Play's can be
/// one — and windows in another Space (a full-screen app). Here a window without a title goes by its
/// app's name, and windows off this Space are listed too.
// (CGWindowListCreateImage, as xcap uses too: ScreenCaptureKit is the newer way)
#[cfg(target_os = "macos")]
#[allow(deprecated)]
mod mac {
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use anyhow::{Result, anyhow};
    use objc2_core_foundation::{CFDictionary, CFNumber, CFNumberType, CFString, CGRect};
    use objc2_core_graphics::{
        CGDataProvider, CGImage, CGRectMakeWithDictionaryRepresentation, CGWindowImageOption, CGWindowListCopyWindowInfo,
        CGWindowListCreateImage, CGWindowListOption,
    };

    use super::{FrameData, Shared, WindowCapture, WindowInfo, shrink};

    /// One window as CoreGraphics lists it.
    #[derive(Clone, Debug)]
    pub struct Win {
        pub id: u32,
        /// The window's title, or its app's name when it has none ("PS Remote Play").
        pub label: String,
        pub app: String,
        pub rect: CGRect,
        /// False when the app keeps its window out of screenshots (it then can't be read).
        pub shareable: bool,
    }

    fn value(d: &CFDictionary, key: &str) -> *const c_void {
        let k = CFString::from_str(key);
        #[allow(unused_unsafe)]
        unsafe {
            d.value((k.as_ref() as *const CFString).cast())
        }
    }

    fn string(d: &CFDictionary, key: &str) -> Option<String> {
        let v = value(d, key);
        (!v.is_null()).then(|| unsafe { (*(v as *const CFString)).to_string() })
    }

    fn number(d: &CFDictionary, key: &str) -> Option<f64> {
        let v = value(d, key);
        if v.is_null() {
            return None;
        }
        let mut x: f64 = 0.0;
        #[allow(unused_unsafe)]
        let ok = unsafe { (*(v as *const CFNumber)).value(CFNumberType::Float64Type, &mut x as *mut f64 as *mut c_void) };
        ok.then_some(x)
    }

    fn bounds(d: &CFDictionary) -> Option<CGRect> {
        let v = value(d, "kCGWindowBounds");
        if v.is_null() {
            return None;
        }
        let mut r = CGRect::default();
        #[allow(unused_unsafe)]
        let ok = unsafe { CGRectMakeWithDictionaryRepresentation(Some(&*(v as *const CFDictionary)), &mut r) };
        ok.then_some(r)
    }

    /// Normal windows (layer 0, not this app's but its Xbox player, big enough to read), front to back.
    fn windows(option: CGWindowListOption, relative_to: u32) -> Vec<Win> {
        let own = std::process::id() as f64;
        #[allow(unused_unsafe)]
        let Some(list) = (unsafe { CGWindowListCopyWindowInfo(option, relative_to) }) else { return Vec::new() };
        let mut out = Vec::new();
        #[allow(unused_unsafe)]
        let n = unsafe { list.count() };
        for i in 0..n {
            #[allow(unused_unsafe)]
            let d = unsafe { list.value_at_index(i) } as *const CFDictionary;
            if d.is_null() {
                continue;
            }
            let d = unsafe { &*d };
            // (this app's windows are left out, except the Xbox player's, which is a source)
            let mine = number(d, "kCGWindowOwnerPID") == Some(own) && string(d, "kCGWindowName").as_deref() != Some(crate::xbox::TITLE);
            if number(d, "kCGWindowLayer") != Some(0.0) || mine || number(d, "kCGWindowAlpha") == Some(0.0) {
                continue;
            }
            let (Some(id), Some(rect)) = (number(d, "kCGWindowNumber"), bounds(d)) else { continue };
            if rect.size.width < 50.0 || rect.size.height < 50.0 {
                continue;
            }
            let app = string(d, "kCGWindowOwnerName").unwrap_or_default().trim().to_string();
            let title = string(d, "kCGWindowName").unwrap_or_default().trim().to_string();
            let label = if title.is_empty() { app.clone() } else { title };
            if label.is_empty() || label.starts_with("VI Translator") || app == "Window Server" || app == "Dock" {
                continue;
            }
            let shareable = number(d, "kCGWindowSharingState") != Some(0.0);
            out.push(Win { id: id as u32, label, app, rect, shareable });
        }
        out
    }

    fn all() -> Vec<Win> {
        windows(CGWindowListOption::OptionAll | CGWindowListOption::ExcludeDesktopElements, 0)
    }

    fn by_id(id: u32) -> Option<Win> {
        windows(CGWindowListOption::OptionIncludingWindow, id).into_iter().find(|w| w.id == id)
    }

    fn find(title: &str) -> Option<Win> {
        let all = all();
        let exact = all.iter().position(|w| w.label == title);
        // else one whose title contains it, else the app it names: a source saved as "PS Remote Play"
        // is that app's window under any title (a Mac set to Arabic titles it "اللعب عن بُعد")
        let program = super::super::program_of(title).unwrap_or(title);
        let idx = exact
            .or_else(|| all.iter().position(|w| w.label.contains(title)))
            .or_else(|| all.iter().position(|w| !w.app.is_empty() && w.app == program))?;
        all.into_iter().nth(idx)
    }

    /// Windows at least this wide (in points) are read at one pixel per point, not at the Retina
    /// screen's two: the picture in them (a 1080p stream at most) has no more detail than that, and
    /// four times the pixels made every frame four times the work for nothing.
    const NOMINAL_FROM_W: f64 = 1280.0;

    /// One frame of the window (RGBA).
    fn capture(w: &Win) -> Option<(u32, u32, Vec<u8>)> {
        let mut opt = CGWindowImageOption::BoundsIgnoreFraming;
        if w.rect.size.width >= NOMINAL_FROM_W {
            opt |= CGWindowImageOption::NominalResolution;
        }
        #[allow(unused_unsafe)]
        let img = unsafe { CGWindowListCreateImage(w.rect, CGWindowListOption::OptionIncludingWindow, w.id, opt) };
        let (width, height) = (CGImage::width(img.as_deref()), CGImage::height(img.as_deref()));
        if width == 0 || height == 0 {
            return None;
        }
        let data = CGDataProvider::data(CGImage::data_provider(img.as_deref()).as_deref())?.to_vec();
        let row = CGImage::bytes_per_row(img.as_deref());
        if row < width * 4 || data.len() < row * height {
            return None;
        }
        // BGRA rows (with padding) -> packed RGBA, in one pass
        let mut rgba = vec![0u8; width * height * 4];
        for (line, out) in data.chunks_exact(row).take(height).zip(rgba.chunks_exact_mut(width * 4)) {
            for (src, dst) in line[..width * 4].chunks_exact(4).zip(out.chunks_exact_mut(4)) {
                dst[0] = src[2];
                dst[1] = src[1];
                dst[2] = src[0];
                dst[3] = src[3];
            }
        }
        Some((width as u32, height as u32, rgba))
    }

    pub fn list_windows(own_title: &str) -> Vec<WindowInfo> {
        let mut out: Vec<WindowInfo> =
            all().into_iter().filter(|w| w.label != own_title).map(|w| WindowInfo { title: w.label, process: w.app }).collect();
        out.sort_by_key(|w| w.title.to_lowercase());
        out.dedup_by(|a, b| a.title == b.title);
        out
    }

    pub fn start_window(title: &str, shared: Arc<Shared>) -> Result<(WindowCapture, isize)> {
        let win = find(title).ok_or_else(|| anyhow!("Window \"{title}\" not found. Open it, or pick another window."))?;
        if !win.shareable {
            return Err(anyhow!(
                "\"{}\" keeps its window out of screen captures, so it can't be read. Use a capture card or an OBS source instead.",
                win.label
            ));
        }
        // one frame now: fails fast when capture isn't allowed (the Screen Recording permission)
        capture(&win).ok_or_else(|| {
            anyhow!("Capture failed: allow VI Translator in System Settings > Privacy & Security > Screen Recording, then restart it.")
        })?;
        let id = win.id;
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let thread = std::thread::spawn(move || {
            let ago = Instant::now() - Duration::from_secs(1);
            let (mut last, mut last_preview) = (ago, ago);
            let mut misses = 0;
            // (where the window is: looked up again twice a second, not for every frame, as listing
            // the windows costs about as much as reading one)
            let mut win = Some(win);
            let mut looked = Instant::now();
            while !flag.load(Ordering::Relaxed) {
                let full = shared.full_due(last);
                let preview = shared.preview_due(last_preview);
                if full || preview {
                    if win.is_none() || looked.elapsed() >= Duration::from_millis(500) {
                        win = by_id(id);
                        looked = Instant::now();
                    }
                    match win.as_ref().and_then(capture) {
                        Some((w, h, rgba)) => {
                            misses = 0;
                            if preview {
                                last_preview = Instant::now();
                                shared.put_preview(shrink(&rgba, w, h, (w * 4) as usize, false));
                            }
                            if full {
                                last = Instant::now();
                                shared.put_full(w, h, rgba);
                            }
                        }
                        // the window closed (or can't be read any more): end, so the manager restarts
                        None => {
                            misses += 1;
                            // (look it up again at once: it may have moved or closed)
                            win = None;
                            if misses > 20 {
                                break;
                            }
                        }
                    }
                }
                std::thread::sleep(Duration::from_millis(8));
            }
        });
        Ok((WindowCapture { stop, thread: Some(thread) }, id as isize))
    }

    pub fn grab_once(title: &str) -> Result<Arc<FrameData>> {
        let win = find(title).ok_or_else(|| anyhow!("window \"{title}\" not found"))?;
        let (w, h, rgba) = capture(&win).ok_or_else(|| anyhow!("capture failed"))?;
        Ok(Arc::new(FrameData { w, h, rgba, seq: 1 }))
    }

    fn rect_of(w: &Win) -> (i32, i32, i32, i32) {
        (w.rect.origin.x as i32, w.rect.origin.y as i32, w.rect.size.width as i32, w.rect.size.height as i32)
    }

    pub fn client_rect(id: isize) -> Option<(i32, i32, i32, i32)> {
        (id > 0).then(|| by_id(id as u32)).flatten().map(|w| rect_of(&w))
    }

    /// The frontmost normal window on screen (the list comes front to back).
    fn front() -> Option<Win> {
        windows(CGWindowListOption::OptionOnScreenOnly | CGWindowListOption::ExcludeDesktopElements, 0).into_iter().next()
    }

    pub fn focused_client_rect(id: isize) -> Option<(i32, i32, i32, i32)> {
        let f = front()?;
        (id > 0 && f.id as isize == id).then(|| rect_of(&f))
    }

    pub fn foreground_window() -> isize {
        front().map(|w| w.id as isize).unwrap_or(0)
    }

    pub fn find_window(title: &str) -> Option<isize> {
        find(title).map(|w| w.id as isize)
    }
}
