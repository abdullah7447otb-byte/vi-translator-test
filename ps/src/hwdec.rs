//! The PS5's picture decoded: by the computer's own video decoder where it has one
//! (VideoToolbox on a Mac, the graphics card through Media Foundation on Windows), else on the
//! CPU by OpenH264. A hardware decoder falls back to OpenH264 only when it really fails: on key frames (three
//! of them), or on every frame for a second and more. A frame broken on the network (its references lost) is
//! not the decoder's failure, and the session lost to sleep is made again on the next frame, uncounted.
//! The caller tries the hardware again a while after a fall-back (play.rs).

use crate::play::I420;

/// Key frames the hardware decoder failed on, in a row, before OpenH264 takes over.
#[cfg_attr(not(any(target_os = "macos", windows, test)), allow(dead_code))]
const KEY_FAILURES: u32 = 3;
/// Frames in a row it failed on (about a second at 60 frames a second), the same.
#[cfg_attr(not(any(target_os = "macos", windows, test)), allow(dead_code))]
const FAILURES: u32 = 60;

/// Which decoder the stream uses (the setting: Automatic or Software).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Decode {
    /// the computer's own decoder where there is one, OpenH264 if not
    #[default]
    Auto,
    /// OpenH264 always
    Software,
}

#[cfg(any(target_os = "macos", windows))]
mod ffi {
    use std::ffi::c_void;
    unsafe extern "C" {
        #[cfg(target_os = "macos")]
        pub fn vit_vt_new(hevc: i32) -> *mut c_void;
        #[cfg(target_os = "macos")]
        pub fn vit_vt_free(p: *mut c_void);
        #[cfg(target_os = "macos")]
        pub fn vit_vt_hardware(p: *mut c_void) -> i32;
        #[cfg(target_os = "macos")]
        pub fn vit_vt_decode(p: *mut c_void, data: *const u8, len: usize, y: *mut *const u8, uv: *mut *const u8, w: *mut i32, h: *mut i32, ys: *mut i32, uvs: *mut i32) -> i32;
        #[cfg(windows)]
        pub fn vit_mf_new(software: i32) -> *mut c_void;
        #[cfg(windows)]
        pub fn vit_mf_free(p: *mut c_void);
        #[cfg(windows)]
        pub fn vit_mf_hardware(p: *const c_void) -> i32;
        #[cfg(windows)]
        pub fn vit_mf_decode(p: *mut c_void, data: *const u8, len: usize, y: *mut *const u8, uv: *mut *const u8, w: *mut i32, h: *mut i32, ys: *mut i32, uvs: *mut i32) -> i32;
    }
}

#[cfg(any(target_os = "macos", windows))]
struct Native(*mut std::ffi::c_void);
// (used by the decoder thread alone, one call at a time)
#[cfg(any(target_os = "macos", windows))]
unsafe impl Send for Native {}

#[cfg(any(target_os = "macos", windows))]
impl Drop for Native {
    fn drop(&mut self) {
        unsafe {
            #[cfg(target_os = "macos")]
            ffi::vit_vt_free(self.0);
            #[cfg(windows)]
            ffi::vit_mf_free(self.0);
        }
    }
}

enum Inner {
    Soft(openh264::decoder::Decoder),
    #[cfg(any(target_os = "macos", windows))]
    Hard(Native),
}

pub struct VideoDecoder {
    inner: Inner,
    /// the system's decoder kept even when it turns out to decode on the CPU (the tests); for
    /// the stream it is left for OpenH264 then (a software VideoToolbox, as in a virtual Mac, was
    /// slower than OpenH264)
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    keep_software: bool,
    /// whether the system's decoder is the hardware's (None: not known yet)
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    hardware: Option<bool>,
    /// hardware failures in a row: on any frame (`failures`), and on key frames (`key_failures`)
    #[cfg_attr(not(any(target_os = "macos", windows)), allow(dead_code))]
    failures: u32,
    #[cfg_attr(not(any(target_os = "macos", windows)), allow(dead_code))]
    key_failures: u32,
    /// the stream is HEVC (only the Mac's VideoToolbox decodes it: no fall-back to OpenH264)
    hevc: bool,
}

impl VideoDecoder {
    pub fn new(kind: Decode) -> Result<Self, String> {
        Self::new_for(kind, false)
    }

    /// For an HEVC stream (`hevc`) or H.264. HEVC is decoded by VideoToolbox only (the Mac), whatever
    /// `kind` says: OpenH264 has no HEVC.
    pub fn new_for(kind: Decode, hevc: bool) -> Result<Self, String> {
        #[cfg(any(target_os = "macos", windows))]
        if kind == Decode::Auto || hevc {
            if let Some(n) = Self::native(false, hevc) {
                return Ok(VideoDecoder { inner: Inner::Hard(n), failures: 0, key_failures: 0, keep_software: false, hardware: None, hevc });
            }
        }
        let _ = kind;
        if hevc {
            return Err("HEVC: this computer has no decoder for it".into());
        }
        Self::software()
    }

    pub fn software() -> Result<Self, String> {
        let d = openh264::decoder::Decoder::new().map_err(|e| format!("video decoder: {e}"))?;
        Ok(VideoDecoder { inner: Inner::Soft(d), failures: 0, key_failures: 0, keep_software: false, hardware: Some(false), hevc: false })
    }

    /// The system's decoder; `cpu`: also Windows decoding on the CPU (for the tests); `hevc`: for an
    /// HEVC stream (the Mac's only).
    #[cfg(any(target_os = "macos", windows))]
    fn native(cpu: bool, hevc: bool) -> Option<Native> {
        let _ = (cpu, hevc);
        #[cfg(target_os = "macos")]
        let p = unsafe { ffi::vit_vt_new(hevc as i32) };
        #[cfg(windows)]
        let p = if hevc { std::ptr::null_mut() } else { unsafe { ffi::vit_mf_new(cpu as i32) } };
        (!p.is_null()).then_some(Native(p))
    }

    /// The system's decoder even where it would decode on the CPU: for tests and the benchmark.
    pub fn system_for_test() -> Option<Self> {
        Self::system_for_test_of(false)
    }

    /// The same for an HEVC stream (`hevc`).
    pub fn system_for_test_of(hevc: bool) -> Option<Self> {
        #[cfg(any(target_os = "macos", windows))]
        {
            Self::native(true, hevc).map(|n| VideoDecoder { inner: Inner::Hard(n), failures: 0, key_failures: 0, keep_software: true, hardware: None, hevc })
        }
        #[cfg(not(any(target_os = "macos", windows)))]
        {
            let _ = hevc;
            None
        }
    }

    /// Decodes HEVC (else H.264).
    pub fn is_hevc(&self) -> bool {
        self.hevc
    }

    /// What decodes, for the log and the stream's status.
    pub fn name(&self) -> &'static str {
        match &self.inner {
            Inner::Soft(_) => "OpenH264 (CPU)",
            #[cfg(target_os = "macos")]
            Inner::Hard(_) => match (self.hevc, self.hardware == Some(false)) {
                (true, true) => "VideoToolbox HEVC (CPU)",
                (true, false) => "VideoToolbox HEVC",
                (false, true) => "VideoToolbox (CPU)",
                (false, false) => "VideoToolbox",
            },
            #[cfg(windows)]
            Inner::Hard(n) => {
                if unsafe { ffi::vit_mf_hardware(n.0) } != 0 { "Media Foundation (graphics card)" } else { "Media Foundation (CPU)" }
            }
        }
    }

    /// The system's decoder (hardware or not: see `decodes_in_hardware`).
    pub fn is_hardware(&self) -> bool {
        !matches!(self.inner, Inner::Soft(_))
    }

    /// Known to decode in the hardware (the Mac's media engine, a graphics card).
    pub fn decodes_in_hardware(&self) -> bool {
        match &self.inner {
            Inner::Soft(_) => false,
            #[cfg(target_os = "macos")]
            Inner::Hard(_) => self.hardware == Some(true),
            #[cfg(windows)]
            Inner::Hard(n) => (unsafe { ffi::vit_mf_hardware(n.0) }) != 0,
        }
    }

    /// OpenH264 from now on (the caller asks the PS5 for a key frame). Not for HEVC (an error: the
    /// stream is started again in H.264 instead).
    pub fn fall_back(&mut self) -> Result<(), String> {
        if self.hevc {
            return Err("HEVC: no decoder to fall back to: H.264 from now on".into());
        }
        *self = Self::software()?;
        Ok(())
    }

    /// One frame in; a picture out once there is one. `key`: the frame is a key frame. A hardware
    /// decoder that really fails (see the module) is told as `Err` with "fall back" in it: the caller
    /// switches with [`Self::fall_back`].
    pub fn decode(&mut self, frame: &[u8], key: bool) -> Result<Option<I420>, String> {
        let _ = key;
        match &mut self.inner {
            Inner::Soft(d) => {
                use openh264::formats::YUVSource;
                match d.decode(frame) {
                    Ok(Some(yuv)) => {
                        let (w, h) = yuv.dimensions();
                        let (ys, us, _) = yuv.strides();
                        Ok(Some(I420 {
                            w: w as u32,
                            h: h as u32,
                            y: yuv.y()[..ys * h].to_vec(),
                            u: yuv.u()[..us * h.div_ceil(2)].to_vec(),
                            v: yuv.v()[..us * h.div_ceil(2)].to_vec(),
                            y_stride: ys,
                            uv_stride: us,
                        }))
                    }
                    Ok(None) => Ok(None),
                    Err(e) => Err(e.to_string()),
                }
            }
            #[cfg(any(target_os = "macos", windows))]
            Inner::Hard(n) => {
                let (mut y, mut uv) = (std::ptr::null(), std::ptr::null());
                let (mut w, mut h, mut ys, mut uvs) = (0i32, 0i32, 0i32, 0i32);
                #[cfg(target_os = "macos")]
                let r = unsafe { ffi::vit_vt_decode(n.0, frame.as_ptr(), frame.len(), &mut y, &mut uv, &mut w, &mut h, &mut ys, &mut uvs) };
                #[cfg(windows)]
                let r = unsafe { ffi::vit_mf_decode(n.0, frame.as_ptr(), frame.len(), &mut y, &mut uv, &mut w, &mut h, &mut ys, &mut uvs) };
                if r < 0 {
                    // (-6: VideoToolbox's session lost, after sleep: made again with the next frame, not a failure)
                    if cfg!(target_os = "macos") && r == -6 {
                        return Err(format!("hardware decoder session lost ({r}): made again"));
                    }
                    let give_up = failed(&mut self.failures, &mut self.key_failures, key);
                    return Err(if give_up { format!("hardware decoder failed ({r}): fall back") } else { format!("hardware decoder failed ({r})") });
                }
                self.failures = 0;
                if key {
                    self.key_failures = 0;
                }
                #[cfg(target_os = "macos")]
                if self.hardware.is_none() {
                    let hw = unsafe { ffi::vit_vt_hardware(n.0) };
                    if hw >= 0 {
                        self.hardware = Some(hw == 1);
                        if hw == 0 && !self.keep_software {
                            self.failures = FAILURES;
                            return Err("VideoToolbox decodes on the CPU here: fall back".into());
                        }
                    }
                }
                if r == 0 || y.is_null() || uv.is_null() || w <= 0 || h <= 0 || ys < w || uvs < w {
                    return Ok(None);
                }
                let (w, h, ys, uvs) = (w as usize, h as usize, ys as usize, uvs as usize);
                let y = unsafe { std::slice::from_raw_parts(y, ys * h) };
                let uv = unsafe { std::slice::from_raw_parts(uv, uvs * h.div_ceil(2)) };
                Ok(Some(nv12_to_i420(w, h, y, ys, uv, uvs)))
            }
        }
    }
}

/// A hardware decode failed (on a key frame or not): whether to give up on the hardware decoder now.
#[cfg_attr(not(any(target_os = "macos", windows, test)), allow(dead_code))]
fn failed(failures: &mut u32, key_failures: &mut u32, key: bool) -> bool {
    *failures += 1;
    if key {
        *key_failures += 1;
    }
    *key_failures >= KEY_FAILURES || *failures >= FAILURES
}

/// NV12 (the system decoders' picture: U and V interleaved) as the app's I420.
pub fn nv12_to_i420(w: usize, h: usize, y: &[u8], ys: usize, uv: &[u8], uvs: usize) -> I420 {
    let cw = w.div_ceil(2);
    let ch = h.div_ceil(2);
    let mut u = vec![0u8; cw * ch];
    let mut v = vec![0u8; cw * ch];
    for r in 0..ch {
        let row = &uv[r * uvs..r * uvs + cw * 2];
        let (ur, vr) = (&mut u[r * cw..(r + 1) * cw], &mut v[r * cw..(r + 1) * cw]);
        for (i, p) in row.chunks_exact(2).enumerate() {
            ur[i] = p[0];
            vr[i] = p[1];
        }
    }
    I420 { w: w as u32, h: h as u32, y: y[..ys * h].to_vec(), u, v, y_stride: ys, uv_stride: cw }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn broken_frames_alone_dont_give_up_the_hardware() {
        let (mut any, mut key) = (0, 0);
        // a burst of frames broken on the network (their references lost), each followed by good ones
        for _ in 0..10 {
            for _ in 0..5 {
                assert!(!failed(&mut any, &mut key, false));
            }
            any = 0; // (a good frame in between)
        }
        // three key frames in a row that fail: given up
        assert!(!failed(&mut any, &mut key, true));
        assert!(!failed(&mut any, &mut key, true));
        assert!(failed(&mut any, &mut key, true));
        // or a second and more of nothing but failures
        let (mut any, mut key) = (0, 0);
        for i in 1..=FAILURES {
            assert_eq!(failed(&mut any, &mut key, false), i == FAILURES);
        }
    }

    #[test]
    fn nv12_is_split_into_u_and_v() {
        // 4×2, rows of 6 bytes (padding at the end)
        let y = [1u8, 2, 3, 4, 0, 0, 5, 6, 7, 8, 0, 0];
        let uv = [10u8, 20, 11, 21, 0, 0];
        let f = nv12_to_i420(4, 2, &y, 6, &uv, 6);
        assert_eq!((f.w, f.h, f.y_stride, f.uv_stride), (4, 2, 6, 2));
        assert_eq!(f.u, vec![10, 11]);
        assert_eq!(f.v, vec![20, 21]);
        assert_eq!(f.y.len(), 12);
    }
}
