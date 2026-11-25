//! Capture cards and other video devices (UVC) through Media Foundation.
//!
//! The card's picture is the whole game frame (no window, no title bar), read at ~8 frames per
//! second into the same `Shared` frame slot window capture uses, so OCR, profiles and frame saving
//! work unchanged. Windows' Frame Server lets another app (OBS) keep the same card open.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree};
use windows::core::{Interface, PWSTR};

use crate::capture::{FrameData, Shared};

/// Prefix that marks a device (not a window) in the config's source field.
pub const PREFIX: &str = "device:";

fn init() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    ONCE.call_once(|| unsafe {
        let _ = MFStartup(MF_VERSION, MFSTARTUP_FULL);
    });
}

fn devices() -> Result<Vec<(String, IMFActivate)>> {
    init();
    unsafe {
        let mut attrs: Option<IMFAttributes> = None;
        MFCreateAttributes(&mut attrs, 1)?;
        let attrs = attrs.context("no attributes")?;
        attrs.SetGUID(&MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE, &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID)?;
        let mut list: *mut Option<IMFActivate> = std::ptr::null_mut();
        let mut n = 0u32;
        MFEnumDeviceSources(&attrs, &mut list, &mut n)?;
        let mut out = Vec::new();
        for i in 0..n as usize {
            if let Some(act) = (*list.add(i)).take() {
                let mut name = PWSTR::null();
                let mut len = 0u32;
                if act.GetAllocatedString(&MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME, &mut name, &mut len).is_ok() {
                    out.push((name.to_string().unwrap_or_default(), act));
                    CoTaskMemFree(Some(name.0 as *const _));
                }
            }
        }
        CoTaskMemFree(Some(list as *const _));
        Ok(out)
    }
}

/// Names of the video capture devices (capture cards, webcams).
pub fn list() -> Vec<String> {
    devices().map(|d| d.into_iter().map(|(n, _)| n).collect()).unwrap_or_default()
}

const STREAM: u32 = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;

fn size_of(t: &IMFMediaType) -> (u32, u32) {
    let v = unsafe { t.GetUINT64(&MF_MT_FRAME_SIZE) }.unwrap_or(0);
    ((v >> 32) as u32, v as u32)
}

fn fps_of(t: &IMFMediaType) -> f64 {
    let v = unsafe { t.GetUINT64(&MF_MT_FRAME_RATE) }.unwrap_or(0);
    let (n, d) = ((v >> 32) as f64, (v as u32) as f64);
    if d > 0.0 { n / d } else { 0.0 }
}

/// What the reader hands us: the card's own YUY2 frames as they are (like OBS: the preview turns
/// them into colour on the GPU), or RGB32 that Media Foundation converts for us (other formats).
#[derive(Clone, Copy, PartialEq, Debug)]
enum Out {
    Rgb32,
    Yuy2(crate::capture::YuvColor),
}

/// The card's colour standard (BT.709 for HD unless it says otherwise) and range (16–235 unless 0–255).
unsafe fn yuv_color(t: &IMFMediaType, h: u32) -> crate::capture::YuvColor {
    let matrix = unsafe { t.GetUINT32(&MF_MT_YUV_MATRIX) }.unwrap_or(0);
    let range = unsafe { t.GetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE) }.unwrap_or(0);
    crate::capture::YuvColor {
        bt709: match matrix {
            m if m == MFVideoTransferMatrix_BT709.0 as u32 => true,
            m if m == MFVideoTransferMatrix_BT601.0 as u32 => false,
            _ => h >= 720,
        },
        full_range: range == MFNominalRange_0_255.0 as u32,
    }
}

/// How much a format of the card is wanted: its size first (up to 1920×1080, or 3840×2160 with `big`;
/// a larger one counts less the larger it is), then its frame rate (up to 60), then uncompressed.
fn format_score(w: u32, h: u32, fps: f64, compressed: bool, big: bool) -> i64 {
    let area = (w as i64) * (h as i64);
    let most: i64 = if big { 3840 * 2160 } else { 1920 * 1080 };
    let size_score = if area <= most { area } else { most - (area - most) / 4 };
    size_score * 1000 + (fps.min(60.0) * 10.0) as i64 + if compressed { 0 } else { 5 }
}

/// Open the device: YUY2 frames as the card sends them, or else RGB32. Prefers 1920×1080 (plenty for
/// reading text, and what most cards stream), or with `big` (the 4K picture) its largest size up to
/// 3840×2160; then the highest frame rate, then uncompressed formats. `pick = false` keeps the card's
/// current format (needed while another app is streaming it).
fn open(name: &str, pick: bool, big: bool) -> Result<(IMFSourceReader, u32, u32, Out)> {
    let act = devices()?
        .into_iter()
        .find(|(n, _)| n == name)
        .map(|(_, a)| a)
        .ok_or_else(|| anyhow!("capture device \"{name}\" not found (unplugged?)"))?;
    unsafe {
        let source: IMFMediaSource = act.ActivateObject().context("could not open the device (in use by another app?)")?;
        let mut attrs: Option<IMFAttributes> = None;
        MFCreateAttributes(&mut attrs, 3)?;
        let attrs = attrs.context("no attributes")?;
        // let the reader decode MJPG / convert NV12, YUY2 … to RGB32 for us
        attrs.SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1)?;
        // hold as few frames as possible on the way (live picture, not a video file)
        let _ = attrs.SetUINT32(&MF_LOW_LATENCY, 1);
        let reader = MFCreateSourceReaderFromMediaSource(&source, &attrs)?;

        let mut best: Option<(i64, IMFMediaType)> = None;
        for i in 0.. {
            let Ok(t) = reader.GetNativeMediaType(STREAM, i) else { break };
            let (w, h) = size_of(&t);
            let fps = fps_of(&t).min(60.0);
            let compressed = t.GetGUID(&MF_MT_SUBTYPE).is_ok_and(|g| g == MFVideoFormat_MJPG);
            let score = format_score(w, h, fps, compressed, big);
            if best.as_ref().is_none_or(|(s, _)| score > *s) {
                best = Some((score, t));
            }
        }
        // Another app (OBS) may already be streaming the card in its own format: then keep that one.
        if let (true, Some((_, t))) = (pick, &best) {
            let _ = reader.SetCurrentMediaType(STREAM, None, t);
        }
        if std::env::var_os("VI_TRANSLATOR_DEBUG").is_some() {
            let cur = reader.GetCurrentMediaType(STREAM)?;
            let (w, h) = size_of(&cur);
            vi_translator_core::vlog!("[device] pick {pick}: {w}x{h} @ {:.1} fps, subtype {:?}", fps_of(&cur), cur.GetGUID(&MF_MT_SUBTYPE));
        }
        let cur = reader.GetCurrentMediaType(STREAM)?;
        let (w, h) = size_of(&cur);
        if cur.GetGUID(&MF_MT_SUBTYPE).is_ok_and(|g| g == MFVideoFormat_YUY2) && w > 0 && h > 0 && w % 2 == 0 {
            return Ok((reader, w, h, Out::Yuy2(yuv_color(&cur, h))));
        }
        let out = MFCreateMediaType()?;
        out.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        out.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)?;
        out.SetUINT64(&MF_MT_FRAME_SIZE, ((w as u64) << 32) | h as u64)?;
        reader.SetCurrentMediaType(STREAM, None, &out).context("the device's picture can't be converted to RGB")?;
        let cur = reader.GetCurrentMediaType(STREAM)?;
        let (w, h) = size_of(&cur);
        if w == 0 || h == 0 {
            return Err(anyhow!("the device reports no picture size"));
        }
        Ok((reader, w, h, Out::Rgb32))
    }
}

/// The reader's output now: its size and kind, or None if it's one this reader doesn't take.
fn current(r: &IMFSourceReader) -> Option<(u32, u32, Out)> {
    let cur = unsafe { r.GetCurrentMediaType(STREAM) }.ok()?;
    let (w, h) = size_of(&cur);
    if w == 0 || h == 0 {
        return None;
    }
    let g = unsafe { cur.GetGUID(&MF_MT_SUBTYPE) }.ok()?;
    if g == MFVideoFormat_YUY2 && w % 2 == 0 {
        Some((w, h, Out::Yuy2(unsafe { yuv_color(&cur, h) })))
    } else if g == MFVideoFormat_RGB32 {
        Some((w, h, Out::Rgb32))
    } else {
        None
    }
}

/// Copy one sample out as top-down rows of `w × bpp` bytes (RGB32: 4, YUY2: 2). A sample that
/// doesn't hold that many rows (the card changed its picture meanwhile) is an error, never read past.
fn copy_rows(sample: &IMFSample, w: u32, h: u32, bpp: u32) -> Result<Vec<u8>> {
    unsafe {
        let buf = sample.ConvertToContiguousBuffer()?;
        let mut out = vec![0u8; (w * h * bpp) as usize];
        let row = (w * bpp) as usize;
        let copy_rows = |scan0: *const u8, pitch: isize, out: &mut [u8]| {
            for y in 0..h as usize {
                let src = std::slice::from_raw_parts(scan0.offset(pitch * y as isize), row);
                out[y * row..(y + 1) * row].copy_from_slice(src);
            }
        };
        if let Ok(b2) = buf.cast::<IMF2DBuffer2>() {
            let (mut scan0, mut pitch, mut start, mut len) = (std::ptr::null_mut(), 0i32, std::ptr::null_mut(), 0u32);
            b2.Lock2DSize(MF2DBuffer_LockFlags_Read, &mut scan0, &mut pitch, &mut start, &mut len)?;
            let p = pitch as isize;
            // the top row's place in the buffer, and the bottom row's (a negative pitch: bottom-up)
            let first = scan0 as isize - start as isize;
            let last = first + p * (h as isize - 1);
            if h == 0 || p.unsigned_abs() < row || first.min(last) < 0 || first.max(last) as usize + row > len as usize {
                let _ = b2.Unlock2D();
                return Err(anyhow!("the frame is not {w}×{h}"));
            }
            copy_rows(scan0, p, &mut out);
            let _ = b2.Unlock2D();
        } else {
            let (mut p, mut len) = (std::ptr::null_mut(), 0u32);
            buf.Lock(&mut p, None, Some(&mut len))?;
            if (len as usize) < row * h as usize {
                let _ = buf.Unlock();
                return Err(anyhow!("short frame"));
            }
            copy_rows(p, row as isize, &mut out);
            let _ = buf.Unlock();
        }
        Ok(out)
    }
}

/// A running device capture (a reader thread).
pub struct DeviceCapture {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl DeviceCapture {
    pub fn is_finished(&self) -> bool {
        self.handle.as_ref().is_none_or(|h| h.is_finished())
    }
    pub fn stop(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

// (dropped without `stop` — its owner ended on an error — it still ends, rather than running on
// beside the one started in its place)
impl Drop for DeviceCapture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Start reading `name` into `shared`. Its status gets "Capturing …" or the error.
pub fn start(name: String, shared: Arc<Shared>, big: bool) -> DeviceCapture {
    let stop = Arc::new(AtomicBool::new(false));
    let s2 = stop.clone();
    let handle = std::thread::spawn(move || {
        let status = &shared.status;
        init();
        // First in our preferred format; if the card won't start streaming in it (another app such as
        // OBS has it running in a different format), again in whatever format it is already sending.
        let mut opened = None;
        let mut why = String::new();
        for pick in [true, false] {
            match open(&name, pick, big) {
                Ok((reader, w, h, out)) => {
                    let (mut flags, mut sample) = (0u32, None);
                    match unsafe { reader.ReadSample(STREAM, 0, None, Some(&mut flags), None, Some(&mut sample)) } {
                        Ok(()) if flags & MF_SOURCE_READERF_ERROR.0 as u32 == 0 => {
                            opened = Some((reader, w, h, out));
                            break;
                        }
                        Ok(()) => why = format!("flags {flags:#x}"),
                        Err(e) => why = format!("{e}"),
                    }
                }
                Err(e) => why = format!("{e:#}"),
            }
        }
        let Some((reader, mut w, mut h, mut out)) = opened else {
            if why.contains("0xC00D3704") {
                why = "another app is using it (OBS's Video Capture Device keeps it to itself) — pick the card's OBS source (📺) instead".into();
            }
            *status.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = format!("Capture device \"{name}\": {why}");
            std::thread::sleep(Duration::from_secs(2)); // the manager retries after this
            return;
        };
        *status.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = format!("Capturing device \"{name}\" ({w}×{h})");
        if std::env::var_os("VI_TRANSLATOR_DEBUG").is_some() {
            vi_translator_core::vlog!("[device] reading {w}x{h} as {out:?}");
        }
        let ago = Instant::now() - Duration::from_secs(1);
        let (mut last, mut last_preview) = (ago, ago);
        // VI_TRANSLATOR_DEBUG: once a second, how fast frames come and where the time goes
        let debug = std::env::var_os("VI_TRANSLATOR_DEBUG").is_some();
        let mut st = DebugStats::new();
        while !s2.load(Ordering::Relaxed) {
            let (mut flags, mut sample) = (0u32, None);
            // Read every frame the card sends (so the newest one is never stale); keep full frames at
            // the capture rate and small ones at the preview rate.
            let t_read = Instant::now();
            let mut stamp = 0i64;
            let r = unsafe { reader.ReadSample(STREAM, 0, None, Some(&mut flags), Some(&mut stamp), Some(&mut sample)) };
            if debug {
                st.frame(t_read.elapsed(), stamp);
            }
            if r.is_err() || flags & (MF_SOURCE_READERF_ERROR.0 | MF_SOURCE_READERF_ENDOFSTREAM.0) as u32 != 0 {
                let why = match &r {
                    Err(e) => format!("{e}"),
                    Ok(()) => format!("flags {flags:#x}"),
                };
                *status.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = format!("Capture device \"{name}\" stopped sending frames ({why})");
                break;
            }
            // The card changed its picture (the console switched resolution): follow it, or open the
            // card again if it's now in a form this reader doesn't take.
            if flags & (MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED.0 | MF_SOURCE_READERF_NATIVEMEDIATYPECHANGED.0) as u32 != 0 {
                match current(&reader) {
                    Some(now) => {
                        (w, h, out) = now;
                        *status.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = format!("Capturing device \"{name}\" ({w}×{h})");
                    }
                    None => {
                        *status.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = format!("Capture device \"{name}\" changed its picture: opening it again");
                        break;
                    }
                }
            }
            let Some(sample) = sample else { continue };
            let (full, preview) = (shared.full_due(last), shared.preview_due(last_preview));
            if !full && !preview {
                continue;
            }
            let t_work = Instant::now();
            match out {
                Out::Yuy2(color) => {
                    let Ok(px) = copy_rows(&sample, w, h, 2) else { continue };
                    if full {
                        last = Instant::now();
                        shared.put_full(w, h, crate::capture::yuy2_to_rgba(&px, w, h, color));
                    }
                    if preview {
                        // A game running slower than the console's 60 Hz output (30 fps) arrives as
                        // each frame twice: an exact copy of the last one sent isn't sent again (the
                        // window has it already). The next frame is looked at right away.
                        let copy = shared.preview_yuv.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_ref().is_some_and(|(_, f)| f.w == w && f.h == h && f.data == px);
                        if copy {
                            if debug {
                                st.copy();
                            }
                        } else {
                            // the card's frame as it is: the window converts it on the GPU
                            last_preview = Instant::now();
                            shared.put_preview_yuv(crate::capture::YuvFrame { w, h, color, layout: crate::capture::YuvLayout::Yuy2, stretch: false, data: px });
                        }
                    }
                }
                Out::Rgb32 => {
                    let Ok(mut px) = copy_rows(&sample, w, h, 4) else { continue };
                    if preview && w % 2 == 0 {
                        // (a card that sends MJPEG, decoded by Windows) as YUY2 for the window's GPU, full
                        // size: no JPEG on the way. An exact copy of the last frame isn't sent again.
                        let yuy2 = crate::capture::bgrx_to_yuy2(&px, w, h);
                        let copy = shared.preview_yuv.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_ref().is_some_and(|(_, f)| f.w == w && f.h == h && f.data == yuy2);
                        if copy {
                            if debug {
                                st.copy();
                            }
                        } else {
                            last_preview = Instant::now();
                            let color = crate::capture::YuvColor { bt709: true, full_range: false };
                            shared.put_preview_yuv(crate::capture::YuvFrame { w, h, color, layout: crate::capture::YuvLayout::Yuy2, stretch: false, data: yuy2 });
                        }
                    } else if preview {
                        last_preview = Instant::now();
                        shared.put_preview(crate::capture::shrink(&px, w, h, (w * 4) as usize, true));
                    }
                    if full {
                        last = Instant::now();
                        for p in px.chunks_exact_mut(4) {
                            p.swap(0, 2); // BGRX -> RGBA
                            p[3] = 255;
                        }
                        shared.put_full(w, h, px);
                    }
                }
            }
            if debug {
                st.work(t_work.elapsed());
            }
        }
    });
    DeviceCapture { stop, handle: Some(handle) }
}

/// Debugging numbers for the device loop, printed once a second.
struct DebugStats {
    since: Instant,
    frames: u32,
    read: Duration,
    read_max: Duration,
    work: Duration,
    work_max: Duration,
    /// preview frames not sent: exact copies of the one before
    copies: u32,
    /// (wall clock, frame time stamp) of the first frame: how far the frames drift behind real time
    first: Option<(Instant, i64)>,
}

impl DebugStats {
    fn new() -> Self {
        Self { since: Instant::now(), frames: 0, read: Duration::ZERO, read_max: Duration::ZERO, work: Duration::ZERO, work_max: Duration::ZERO, copies: 0, first: None }
    }
    fn frame(&mut self, read: Duration, stamp: i64) {
        self.frames += 1;
        self.read += read;
        self.read_max = self.read_max.max(read);
        let (t0, s0) = *self.first.get_or_insert((Instant::now(), stamp));
        if self.since.elapsed() >= Duration::from_secs(1) {
            // > 0 and growing: frames reach us later and later (a queue builds up somewhere)
            let behind_ms = t0.elapsed().as_millis() as i64 - (stamp - s0) / 10_000;
            let n = self.frames.max(1);
            vi_translator_core::vlog!(
                "[device] {:.1} fps | wait for frame avg {:.1} ms max {:.1} | our work avg {:.1} ms max {:.1} | drift {behind_ms} ms | copies skipped {}",
                self.frames as f64 / self.since.elapsed().as_secs_f64(),
                self.read.as_secs_f64() * 1000.0 / n as f64,
                self.read_max.as_secs_f64() * 1000.0,
                self.work.as_secs_f64() * 1000.0 / n as f64,
                self.work_max.as_secs_f64() * 1000.0,
                self.copies,
            );
            *self = Self { first: self.first, ..Self::new() };
        }
    }
    fn copy(&mut self) {
        self.copies += 1;
    }
    fn work(&mut self, d: Duration) {
        self.work += d;
        self.work_max = self.work_max.max(d);
    }
}

/// One frame from a device (for `--grab-device`).
pub fn grab_once(name: &str) -> Result<Arc<FrameData>> {
    let shared = Arc::new(Shared::default());
    let cap = start(name.to_string(), shared.clone(), false);
    let t = Instant::now();
    let f = loop {
        if let Some(f) = shared.latest() {
            break Ok(f);
        }
        if cap.is_finished() || t.elapsed() > Duration::from_secs(8) {
            let s = shared.status.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
            break Err(anyhow!("no frame from the device: {s}"));
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    cap.stop();
    f
}

#[cfg(test)]
mod tests {
    use super::format_score;

    /// The best of a 4K card's usual formats.
    fn pick(big: bool) -> (u32, u32, f64, bool) {
        let formats = [(3840, 2160, 30.0, false), (3840, 2160, 60.0, true), (2560, 1440, 60.0, false), (1920, 1080, 60.0, false), (1920, 1080, 60.0, true), (1280, 720, 60.0, false)];
        *formats.iter().max_by_key(|(w, h, fps, c)| format_score(*w, *h, *fps, *c, big)).unwrap()
    }

    #[test]
    fn a_4k_card_runs_at_1080p_unless_the_4k_picture_is_on() {
        assert_eq!(pick(false), (1920, 1080, 60.0, false), "1080p60, uncompressed");
        assert_eq!(pick(true), (3840, 2160, 60.0, true), "4K60 (MJPEG) over 4K30");
    }

    #[test]
    fn a_1080p_card_is_the_same_either_way() {
        let formats = [(1920, 1080, 60.0, false), (1280, 720, 60.0, false)];
        for big in [false, true] {
            let best = formats.iter().max_by_key(|(w, h, fps, c)| format_score(*w, *h, *fps, *c, big)).unwrap();
            assert_eq!(best.0, 1920);
        }
    }

    #[test]
    fn nothing_above_4k_is_preferred() {
        assert!(format_score(3840, 2160, 60.0, false, true) > format_score(7680, 4320, 60.0, false, true));
    }
}
