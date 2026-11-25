//! The game with its translations as a webcam for other apps (Discord, Zoom, a streaming app),
//! through the "OBS Virtual Camera" that OBS Studio installs: OBS's camera shows whatever a program
//! writes into its shared frame queue, so nothing of our own has to be installed. OBS itself needn't
//! run, but its own virtual camera must be off (only one program can feed the camera).
//!
//! The picture is what the player sees on the overlay window's game part (the Arabic included); while
//! that window isn't in front, the game picture alone (never another app that covers it).

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::capture::{self, Shared};
use crate::config::Config;

/// The camera's picture: 1920 × 1080, 30 frames a second.
pub const CAM_W: u32 = 1920;
pub const CAM_H: u32 = 1080;
const FPS: u64 = 30;

/// What the camera is doing, for the Show on tab.
pub fn status(shared: &Shared) -> String {
    shared.vcam_status.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
}

pub fn run(cfg: Arc<Mutex<Config>>, shared: Arc<Shared>) {
    let mut queue: Option<platform::Queue> = None;
    let frame_time = Duration::from_nanos(1_000_000_000 / FPS);
    let mut nv12 = Vec::new();
    loop {
        let on = cfg.lock().unwrap_or_else(std::sync::PoisonError::into_inner).virtual_camera;
        if !on {
            queue = None; // (OBS's camera goes back to its own picture)
            set(&shared, "");
            std::thread::sleep(Duration::from_millis(300));
            continue;
        }
        if queue.is_none() {
            match platform::Queue::create(CAM_W, CAM_H, 10_000_000 / FPS) {
                Ok(q) => {
                    queue = Some(q);
                    set(&shared, "On: pick \"OBS Virtual Camera\" as the camera in the other app.");
                }
                Err(e) => {
                    set(&shared, &e);
                    std::thread::sleep(Duration::from_secs(2));
                    continue;
                }
            }
        }
        let t0 = Instant::now();
        if let Some(rgba) = picture(&shared) {
            rgba_to_nv12_fit(&rgba.2, rgba.0, rgba.1, CAM_W, CAM_H, &mut nv12);
            if let Some(q) = queue.as_mut() {
                q.write(&nv12, now_100ns());
            }
        }
        std::thread::sleep(frame_time.saturating_sub(t0.elapsed()));
    }
}

fn set(shared: &Shared, s: &str) {
    *shared.vcam_status.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = s.to_string();
}

fn now_100ns() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| (d.as_nanos() / 100) as u64).unwrap_or(0)
}

/// The picture for the camera now: (w, h, RGBA).
fn picture(shared: &Shared) -> Option<(u32, u32, Vec<u8>)> {
    // what the player sees there, the translations included, while the overlay window is in front
    if let Some((x, y, w, h)) = capture::overlay_rect(shared, true) {
        if let Some(px) = capture::grab_screen_region(x, y, w, h) {
            return Some((w as u32, h as u32, px));
        }
    }
    // else the game picture alone
    let f = shared.latest()?;
    Some((f.w, f.h, f.rgba.clone()))
}

/// The RGBA picture (`w` × `h`) scaled to fit `ow` × `oh` (black bars where the shapes differ), as
/// NV12 (BT.709, video range: what cameras send).
pub fn rgba_to_nv12_fit(rgba: &[u8], w: u32, h: u32, ow: u32, oh: u32, out: &mut Vec<u8>) {
    let (ow_, oh_) = (ow as usize, oh as usize);
    out.clear();
    out.resize(ow_ * oh_ * 3 / 2, 0);
    // black in video range
    out[..ow_ * oh_].fill(16);
    out[ow_ * oh_..].fill(128);
    if w == 0 || h == 0 || rgba.len() < (w * h * 4) as usize {
        return;
    }
    let scale = (ow as f64 / w as f64).min(oh as f64 / h as f64);
    let (dw, dh) = (((w as f64 * scale).round() as usize).min(ow_) & !1, ((h as f64 * scale).round() as usize).min(oh_) & !1);
    let (dx, dy) = (((ow_ - dw) / 2) & !1, ((oh_ - dh) / 2) & !1);
    let src_x: Vec<usize> = (0..dw).map(|x| ((x as f64 + 0.5) / scale) as usize).map(|x| x.min(w as usize - 1)).collect();
    let rgb = |sx: usize, sy: usize| {
        let i = (sy * w as usize + sx) * 4;
        (rgba[i] as f32, rgba[i + 1] as f32, rgba[i + 2] as f32)
    };
    let (y_plane, uv_plane) = out.split_at_mut(ow_ * oh_);
    for y in 0..dh {
        let sy = (((y as f64 + 0.5) / scale) as usize).min(h as usize - 1);
        let row = &mut y_plane[(dy + y) * ow_ + dx..(dy + y) * ow_ + dx + dw];
        for (x, sx) in src_x.iter().enumerate() {
            let (r, g, b) = rgb(*sx, sy);
            row[x] = (16.0 + 0.1826 * r + 0.6142 * g + 0.0620 * b).round().clamp(16.0, 235.0) as u8;
        }
    }
    for y in (0..dh).step_by(2) {
        let sy = (((y as f64 + 1.0) / scale) as usize).min(h as usize - 1);
        let uv_row = (dy + y) / 2 * ow_;
        for x in (0..dw).step_by(2) {
            let sx = (((x as f64 + 1.0) / scale) as usize).min(w as usize - 1);
            let (r, g, b) = rgb(sx, sy);
            let u = 128.0 - 0.1006 * r - 0.3386 * g + 0.4392 * b;
            let v = 128.0 + 0.4392 * r - 0.3989 * g - 0.0403 * b;
            uv_plane[uv_row + dx + x] = u.round().clamp(16.0, 240.0) as u8;
            uv_plane[uv_row + dx + x + 1] = v.round().clamp(16.0, 240.0) as u8;
        }
    }
}

#[cfg(windows)]
mod platform {
    use windows::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
    use windows::Win32::System::Memory::{
        CreateFileMappingW, FILE_MAP_ALL_ACCESS, FILE_MAP_READ, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile, OpenFileMappingW,
        PAGE_READWRITE, UnmapViewOfFile,
    };
    use windows::core::w;

    /// OBS's camera queue (plugins/win-dshow/shared-memory-queue.c): an 80-byte header, then three
    /// frame slots, each a 32-byte frame header (its timestamp first) and an NV12 picture.
    const HEADER: usize = 80;
    const FRAME_HEADER: usize = 32;
    const STATE_STARTING: u32 = 1;
    const STATE_READY: u32 = 2;
    const STATE_STOPPING: u32 = 3;
    // header fields' places
    const WRITE_IDX: usize = 0;
    const READ_IDX: usize = 4;
    const STATE: usize = 8;
    const OFFSETS: usize = 12;
    const CX: usize = 28;
    const CY: usize = 32;
    const INTERVAL: usize = 40;

    pub struct Queue {
        handle: HANDLE,
        view: *mut u8,
        offsets: [usize; 3],
        frame_size: usize,
        write_idx: u32,
    }

    // (the view is only used from the camera thread)
    unsafe impl Send for Queue {}

    fn align32(n: usize) -> usize {
        (n + 31) & !31
    }

    /// Where the frames go and how big the mapping is, for a `cx` × `cy` picture.
    pub fn layout(cx: u32, cy: u32) -> ([usize; 3], usize) {
        let frame = (cx * cy * 3 / 2) as usize;
        let mut size = align32(HEADER);
        let mut offsets = [0; 3];
        for o in offsets.iter_mut() {
            *o = size;
            size = align32(size + frame + FRAME_HEADER);
        }
        (offsets, size)
    }

    fn camera_installed() -> bool {
        use windows::Win32::System::Registry::{HKEY_CLASSES_ROOT, KEY_READ, RegCloseKey, RegOpenKeyExW};
        let mut key = Default::default();
        let ok = unsafe { RegOpenKeyExW(HKEY_CLASSES_ROOT, w!("CLSID\\{A3FCE0F5-3493-419F-958A-ABA1250EC20B}"), Some(0), KEY_READ, &mut key) }.is_ok();
        if ok {
            unsafe {
                let _ = RegCloseKey(key);
            }
        }
        ok
    }

    impl Queue {
        pub fn create(cx: u32, cy: u32, interval: u64) -> Result<Self, String> {
            if !camera_installed() {
                return Err("The camera needs OBS Studio's \"OBS Virtual Camera\": install OBS Studio (obsproject.com), then turn this on again.".into());
            }
            unsafe {
                // another program feeds the camera already (OBS's own Start Virtual Camera)
                if let Ok(h) = OpenFileMappingW(FILE_MAP_READ.0, false, w!("OBSVirtualCamVideo")) {
                    let _ = CloseHandle(h);
                    return Err("OBS's own virtual camera is on: stop it in OBS (Stop Virtual Camera), then turn this on again.".into());
                }
                let (offsets, size) = layout(cx, cy);
                let handle = CreateFileMappingW(INVALID_HANDLE_VALUE, None, PAGE_READWRITE, 0, size as u32, w!("OBSVirtualCamVideo"))
                    .map_err(|e| format!("The camera couldn't start: {e}"))?;
                let view: MEMORY_MAPPED_VIEW_ADDRESS = MapViewOfFile(handle, FILE_MAP_ALL_ACCESS, 0, 0, 0);
                if view.Value.is_null() {
                    let _ = CloseHandle(handle);
                    return Err("The camera couldn't start (no shared memory).".into());
                }
                let p = view.Value as *mut u8;
                std::ptr::write_bytes(p, 0, HEADER);
                let put32 = |at: usize, v: u32| (p.add(at) as *mut u32).write_unaligned(v);
                put32(STATE, STATE_STARTING);
                for (i, o) in offsets.iter().enumerate() {
                    put32(OFFSETS + i * 4, *o as u32);
                }
                put32(CX, cx);
                put32(CY, cy);
                (p.add(INTERVAL) as *mut u64).write_unaligned(interval);
                Ok(Self { handle, view: p, offsets, frame_size: (cx * cy * 3 / 2) as usize, write_idx: 0 })
            }
        }

        /// One NV12 picture (the queue's size), with its time in 100 ns units.
        pub fn write(&mut self, nv12: &[u8], ts: u64) {
            if nv12.len() < self.frame_size {
                return;
            }
            self.write_idx = self.write_idx.wrapping_add(1);
            let slot = self.offsets[(self.write_idx % 3) as usize];
            unsafe {
                let p = self.view;
                (p.add(WRITE_IDX) as *mut u32).write_volatile(self.write_idx);
                (p.add(slot) as *mut u64).write_unaligned(ts);
                std::ptr::copy_nonoverlapping(nv12.as_ptr(), p.add(slot + FRAME_HEADER), self.frame_size);
                std::sync::atomic::fence(std::sync::atomic::Ordering::Release);
                (p.add(READ_IDX) as *mut u32).write_volatile(self.write_idx);
                (p.add(STATE) as *mut u32).write_volatile(STATE_READY);
            }
        }
    }

    impl Drop for Queue {
        fn drop(&mut self) {
            unsafe {
                (self.view.add(STATE) as *mut u32).write_volatile(STATE_STOPPING);
                let _ = UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS { Value: self.view as *mut _ });
                let _ = CloseHandle(self.handle);
            }
        }
    }

    #[cfg(test)]
    mod tests {
        #[test]
        fn the_queue_is_laid_out_like_obs_does() {
            // 1280 x 720, as worked out from OBS's video_queue_create
            let (offsets, size) = super::layout(1280, 720);
            assert_eq!(offsets, [96, 1382528, 2764960]);
            assert_eq!(size, 4147392);
        }
    }
}

#[cfg(not(windows))]
mod platform {
    pub struct Queue;
    impl Queue {
        pub fn create(_cx: u32, _cy: u32, _interval: u64) -> Result<Self, String> {
            Err("The virtual camera is only on Windows for now.".into())
        }
        pub fn write(&mut self, _nv12: &[u8], _ts: u64) {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_picture_fits_the_camera_with_black_bars() {
        // a 4 x 4 white square into an 8 x 4 camera: white in the middle 4 columns, black at the sides
        let rgba = vec![255u8; 4 * 4 * 4];
        let mut out = Vec::new();
        rgba_to_nv12_fit(&rgba, 4, 4, 8, 4, &mut out);
        assert_eq!(out.len(), 8 * 4 * 3 / 2);
        let row: Vec<u8> = out[..8].to_vec();
        assert_eq!(row, vec![16, 16, 235, 235, 235, 235, 16, 16]);
        // grey chroma (128) everywhere for white and black
        assert!(out[32..].iter().all(|&c| c == 128));
    }
}
