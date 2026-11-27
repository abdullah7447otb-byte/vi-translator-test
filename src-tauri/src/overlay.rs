//! Arabic overlay windows: one transparent, click-through, never-focused window per read area,
//! kept next to that area on top of the captured window.

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};

use crate::capture::{self, Shared};
use crate::config::{Area, Config};

#[derive(Serialize, Clone, PartialEq)]
struct Geom {
    placement: String,
    /// Height of the captured window's client area in physical pixels (font sizes scale with it).
    client_h: i32,
    /// Width of the client area in physical pixels.
    client_w: i32,
    /// This overlay window's top-left inside the client area, in physical pixels.
    win_x: i32,
    win_y: i32,
}

/// Window label of the full-window overlay used by game-profile screens.
pub const SCREEN_LABEL: &str = "ov-screen";
pub const PHONE_LABEL: &str = "ov-phone";

pub fn label_for(id: &str) -> String {
    let safe: String = id.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' }).collect();
    format!("ov-{safe}")
}

/// Overlay rectangle (client-relative pixels) for an area. Shared formula with the preview in app.js.
pub fn rect_for(placement: &str, a: (i32, i32, i32, i32), cw: i32, ch: i32) -> (i32, i32, i32, i32) {
    let (ax, ay, aw, ah) = a;
    let band = (ch as f64 * 0.40) as i32;
    let side = (cw as f64 * 0.35) as i32;
    let (x, y, w, h) = match placement {
        "below" => (ax, ay + ah, aw, band),
        "right" => (ax + aw, ay, side, band),
        "left" => (ax - side, ay, side, band),
        "over" => (ax, ay + ah - band, aw, band),
        _ => (ax, ay - band, aw, band), // above
    };
    let w = w.clamp(50, cw);
    let h = h.clamp(50, ch);
    (x.clamp(0, cw - w), y.clamp(0, ch - h), w, h)
}

fn area_px(a: &Area, cw: i32, ch: i32) -> (i32, i32, i32, i32) {
    ((a.x * cw as f64) as i32, (a.y * ch as f64) as i32, (a.w * cw as f64) as i32, (a.h * ch as f64) as i32)
}

/// Overlay rectangle for an area: the user's own region for "custom", else derived from the read area.
fn overlay_rect(a: &Area, cw: i32, ch: i32) -> (i32, i32, i32, i32) {
    if a.placement == "custom" {
        let w = ((a.out_w * cw as f64) as i32).clamp(20, cw);
        let h = ((a.out_h * ch as f64) as i32).clamp(20, ch);
        let x = ((a.out_x * cw as f64) as i32).clamp(0, cw - w);
        let y = ((a.out_y * ch as f64) as i32).clamp(0, ch - h);
        // The region sets where the box sits and how wide lines may be. The window is made taller in
        // the direction the text grows, so long Arabic that wraps onto more lines isn't cut off.
        let tall = h.max((ch as f64 * 0.4) as i32).min(ch);
        let grown_y = match a.out_valign.as_str() {
            "top" => y,                       // grows downward
            "middle" => y + h / 2 - tall / 2, // grows both ways
            _ => y + h - tall,                // bottom: grows upward
        };
        return (x, grown_y.clamp(0, ch - tall), w, tall);
    }
    if a.placement == "under_text" {
        // Covers the read area and room below it; the page puts the Arabic box right under the
        // game's text box, same left edge, at most its width, growing downward.
        let (ax, ay, aw, ah) = area_px(a, cw, ch);
        let h = (ah + (ch as f64 * 0.4) as i32).min(ch);
        let w = aw.clamp(50, cw);
        return (ax.clamp(0, cw - w), ay.clamp(0, ch - h), w, h);
    }
    if a.placement == "above_text" {
        // Covers the read area and room above it; the page puts the Arabic right above the game's
        // text, centred on it.
        let (ax, ay, aw, ah) = area_px(a, cw, ch);
        let h = (ah + (ch as f64 * 0.4) as i32).min(ch);
        let w = aw.clamp(50, cw);
        return (ax.clamp(0, cw - w), (ay + ah - h).clamp(0, ch - h), w, h);
    }
    rect_for(&a.placement, area_px(a, cw, ch), cw, ch)
}

fn ensure_window(app: &AppHandle, label: &str, area_id: &str) -> Option<tauri::WebviewWindow> {
    if let Some(w) = app.get_webview_window(label) {
        return Some(w);
    }
    let win = WebviewWindowBuilder::new(app, label, WebviewUrl::App("overlay.html".into()))
        .title("VI Translator overlay")
        .initialization_script(format!("window.__AREA__ = {};", serde_json::to_string(area_id).ok()?))
        .transparent(true)
        .decorations(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .focused(false)
        .shadow(false)
        .resizable(false)
        .visible(false)
        .inner_size(400.0, 200.0)
        .build()
        .ok()?;
    // clicks go straight through to the game (elsewhere it's set once shown: GTK can't before)
    #[cfg(windows)]
    let _ = win.set_ignore_cursor_events(true);
    win_ops::never_focus(&win);
    Some(win)
}

pub fn position_loop(app: AppHandle, cfg: Arc<Mutex<Config>>, shared: Arc<Shared>) {
    let mut shown: HashMap<String, bool> = HashMap::new();
    let mut last_rect: HashMap<String, (i32, i32, i32, i32)> = HashMap::new();
    let mut last_geom: HashMap<String, Geom> = HashMap::new();
    let mut tick = 0u32;
    let mut hidden_from_capture = false;
    loop {
        std::thread::sleep(Duration::from_millis(150));
        tick = tick.wrapping_add(1);
        if tick % 13 == 0 {
            last_geom.clear(); // re-send geometry now and then, in case an overlay page (re)loaded
        }
        let c = cfg.lock().unwrap().clone();
        // Only show the Arabic while the overlay window is the one in front (not over other apps), over
        // the part of it showing the game.
        // (the header's Overlays switch for every output, and this PC's own on the Play screen)
        let client = if c.overlays_enabled && c.device_overlays { capture::overlay_rect(&shared, true) } else { None };
        // (a screen as the source: the overlays don't show up in screen captures, so the app never
        // reads its own Arabic back)
        let hide_from_capture = shared.reading_screen.load(Ordering::Relaxed) || shared.compat_capture.load(Ordering::Relaxed);
        if hide_from_capture != hidden_from_capture {
            hidden_from_capture = hide_from_capture;
            for (label, win) in app.webview_windows() {
                if label.starts_with("ov-") {
                    let _ = win.set_content_protected(hide_from_capture);
                }
            }
        }
        let visible = client.is_some();

        // Every overlay window we want: one per enabled area, plus the full-window "screen" overlay
        // used by game profiles (a menu, a "mission failed" page …). Each gets a target rect when it should show.
        let screen_on = shared.screen_active.load(Ordering::Relaxed);
        let mut wanted: Vec<(String, String, String, Option<(i32, i32, i32, i32)>)> = Vec::new(); // label, id, placement, rect
        for a in c.areas.iter().filter(|a| a.enabled) {
            // While a profile screen is up, the area overlays step aside.
            let rect = match (visible && !screen_on, client) {
                // (over the game's text, or riding above its stacked lines: the box goes where the game's
                // text is, so the window covers the whole picture)
                (true, Some((_, _, cw, ch))) if c.cover_look() || c.fx(&a.id).rides(&a.id) => Some((0, 0, cw, ch)),
                (true, Some((_, _, cw, ch))) => Some(overlay_rect(a, cw, ch)),
                _ => None,
            };
            wanted.push((label_for(&a.id), a.id.clone(), a.placement.clone(), rect));
        }
        let screen_rect = match (visible && screen_on, client) {
            (true, Some((_, _, cw, ch))) => Some((0, 0, cw, ch)),
            _ => None,
        };
        wanted.push((SCREEN_LABEL.into(), "__screen".into(), "screen".into(), screen_rect));
        // GTA VI's phone held up: its translations beside it, over the whole picture
        let phone_on = shared.phone_active.load(Ordering::Relaxed);
        let phone_rect = match (visible && phone_on && !screen_on, client) {
            (true, Some((_, _, cw, ch))) => Some((0, 0, cw, ch)),
            _ => None,
        };
        wanted.push((PHONE_LABEL.into(), "__phone".into(), "phone".into(), phone_rect));

        for (label, win) in app.webview_windows() {
            if label.starts_with("ov-") && !wanted.iter().any(|w| w.0 == label) {
                let _ = win.destroy();
                shown.remove(&label);
                last_rect.remove(&label);
                last_geom.remove(&label);
            }
        }

        for (label, id, placement, rect) in &wanted {
            // Don't create the screen window until a screen actually appears.
            if (label == SCREEN_LABEL || label == PHONE_LABEL) && rect.is_none() && app.get_webview_window(label).is_none() {
                continue;
            }
            let Some(win) = ensure_window(&app, label, id) else { continue };
            let is_shown = shown.get(label).copied().unwrap_or(false);
            match (rect, client) {
                (Some((x, y, w, hh)), Some((cx, cy, cw, ch))) => {
                    let r = (cx + x, cy + y, *w, *hh);
                    if last_rect.get(label) != Some(&r) || !is_shown {
                        win_ops::place(&win, r);
                        last_rect.insert(label.clone(), r);
                    }
                    let geom = Geom { placement: placement.clone(), client_h: ch, client_w: cw, win_x: *x, win_y: *y };
                    if last_geom.get(label) != Some(&geom) {
                        let _ = app.emit_to(label.as_str(), "geom", geom.clone());
                        last_geom.insert(label.clone(), geom);
                    }
                    if !is_shown {
                        if hidden_from_capture {
                            let _ = win.set_content_protected(true); // (a window made since the source changed)
                        }
                        win_ops::show(&win);
                        shown.insert(label.clone(), true);
                    }
                }
                _ => {
                    if is_shown {
                        win_ops::hide(&win);
                        shown.insert(label.clone(), false);
                    }
                }
            }
        }
    }
}

/// Placing, showing and hiding overlay windows without ever taking focus from the game.
#[cfg(windows)]
mod win_ops {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        GWL_EXSTYLE, GetWindowLongPtrW, HWND_TOPMOST, SW_HIDE, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SetWindowLongPtrW,
        SetWindowPos, ShowWindow, WS_EX_NOACTIVATE,
    };

    fn hwnd(win: &tauri::WebviewWindow) -> Option<HWND> {
        win.hwnd().ok().map(|h| HWND(h.0 as *mut _))
    }

    /// Never take focus from the game, even when shown or clicked.
    pub fn never_focus(win: &tauri::WebviewWindow) {
        if let Some(h) = hwnd(win) {
            unsafe {
                let ex = GetWindowLongPtrW(h, GWL_EXSTYLE);
                SetWindowLongPtrW(h, GWL_EXSTYLE, ex | WS_EX_NOACTIVATE.0 as isize);
            }
        }
    }

    pub fn place(win: &tauri::WebviewWindow, r: (i32, i32, i32, i32)) {
        if let Some(h) = hwnd(win) {
            unsafe {
                let _ = SetWindowPos(h, Some(HWND_TOPMOST), r.0, r.1, r.2, r.3, SWP_NOACTIVATE);
            }
        }
    }

    pub fn show(win: &tauri::WebviewWindow) {
        if let Some(h) = hwnd(win) {
            unsafe {
                let _ = ShowWindow(h, SW_SHOWNOACTIVATE);
            }
        }
    }

    pub fn hide(win: &tauri::WebviewWindow) {
        if let Some(h) = hwnd(win) {
            unsafe {
                let _ = ShowWindow(h, SW_HIDE);
            }
        }
    }
}

/// Linux and macOS: Tauri's own window calls (unfocusable, always on top, click-through once shown).
#[cfg(not(windows))]
mod win_ops {
    pub fn never_focus(win: &tauri::WebviewWindow) {
        let _ = win.set_focusable(false);
    }

    /// `r` is in the units the source window's rect comes in: points on macOS (kCGWindowBounds),
    /// so a Retina screen (2 pixels a point) doesn't halve the overlays' place and size; pixels on Linux.
    #[cfg(target_os = "macos")]
    pub fn place(win: &tauri::WebviewWindow, r: (i32, i32, i32, i32)) {
        use tauri::{LogicalPosition, LogicalSize};
        let _ = win.set_position(LogicalPosition::new(r.0 as f64, r.1 as f64));
        let _ = win.set_size(LogicalSize::new(r.2.max(1) as f64, r.3.max(1) as f64));
        let _ = win.set_always_on_top(true);
    }

    #[cfg(not(target_os = "macos"))]
    pub fn place(win: &tauri::WebviewWindow, r: (i32, i32, i32, i32)) {
        use tauri::{PhysicalPosition, PhysicalSize};
        let _ = win.set_position(PhysicalPosition::new(r.0, r.1));
        let _ = win.set_size(PhysicalSize::new(r.2.max(1) as u32, r.3.max(1) as u32));
        let _ = win.set_always_on_top(true);
    }

    pub fn show(win: &tauri::WebviewWindow) {
        let _ = win.show();
        let _ = win.set_ignore_cursor_events(true);
    }

    pub fn hide(win: &tauri::WebviewWindow) {
        let _ = win.hide();
    }
}
