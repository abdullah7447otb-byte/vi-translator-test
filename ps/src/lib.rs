//! PS5 Remote Play on the home network, through chiaki-ng's library (built by build.rs) and VI
//! Translator's small C face of it (native/vit_ps.c): find the PS5, register with it (PSN account
//! ID and the PIN it shows), wake it, and stream from it (encoded video, Opus sound, controller).
//!
//! Callbacks run on chiaki's own threads.

pub mod play;

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::sync::{Arc, Once};

mod ffi {
    use std::ffi::{c_char, c_int, c_void};

    pub type LogCb = extern "C" fn(level: c_int, msg: *const c_char, user: *mut c_void);

    #[repr(C)]
    pub struct Host {
        pub state: c_int,
        pub ps5: bool,
        pub addr: *const c_char,
        pub name: *const c_char,
        pub id: *const c_char,
        pub system_version: *const c_char,
    }

    #[repr(C)]
    pub struct Registered {
        pub nickname: [c_char; 0x20],
        pub mac: [u8; 6],
        pub regist_key: [c_char; 16],
        pub rp_key: [u8; 16],
    }

    #[repr(C)]
    pub struct Callbacks {
        pub user: *mut c_void,
        pub log: Option<LogCb>,
        pub event: Option<extern "C" fn(kind: c_int, arg: c_int, text: *const c_char, user: *mut c_void)>,
        pub video: Option<extern "C" fn(buf: *const u8, size: usize, frames_lost: c_int, recovered: bool, frame_index: c_int, user: *mut c_void) -> bool>,
        pub audio_header: Option<extern "C" fn(channels: c_int, rate: c_int, frame_size: c_int, user: *mut c_void)>,
        pub audio_frame: Option<extern "C" fn(buf: *const u8, size: usize, user: *mut c_void)>,
    }

    unsafe extern "C" {
        pub fn vit_ps_init() -> c_int;
        pub fn vit_ps_error_string(err: c_int) -> *const c_char;
        pub fn vit_discovery_start(
            extra: *const u32,
            extra_count: usize,
            cb: extern "C" fn(*const Host, usize, *mut c_void),
            user: *mut c_void,
            log: Option<LogCb>,
            log_user: *mut c_void,
            err: *mut c_int,
        ) -> *mut c_void;
        pub fn vit_discovery_stop(d: *mut c_void);
        pub fn vit_wakeup(host: *const c_char, credential: u64) -> c_int;
        pub fn vit_regist_start(
            host: *const c_char,
            account_id: *const u8,
            pin: u32,
            cb: extern "C" fn(c_int, *const Registered, *mut c_void),
            user: *mut c_void,
            log: Option<LogCb>,
            log_user: *mut c_void,
            err: *mut c_int,
        ) -> *mut c_void;
        pub fn vit_regist_free(r: *mut c_void);
        pub fn vit_session_start(
            host: *const c_char,
            regist_key: *const c_char,
            rp_key: *const u8,
            width: u32,
            height: u32,
            fps: u32,
            bitrate: u32,
            codec: c_int,
            cb: *const Callbacks,
            err: *mut c_int,
        ) -> *mut c_void;
        pub fn vit_session_controller(s: *mut c_void, buttons: u32, l2: u8, r2: u8, lx: i16, ly: i16, rx: i16, ry: i16);
        pub fn vit_session_login_pin(s: *mut c_void, pin: *const c_char);
        pub fn vit_session_request_idr(s: *mut c_void);
        pub fn vit_session_report_corrupt(s: *mut c_void, start: c_int, end: c_int);
        pub fn vit_session_free(s: *mut c_void);
    }
}

/// chiaki's error, as text.
#[derive(Debug, Clone)]
pub struct Error(pub String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

fn error(code: c_int) -> Error {
    let s = unsafe { ffi::vit_ps_error_string(code) };
    Error(if s.is_null() { format!("error {code}") } else { unsafe { CStr::from_ptr(s) }.to_string_lossy().into_owned() })
}

/// chiaki's library, set up once (sockets, its error correction tables).
fn init() -> Result<(), Error> {
    static ONCE: Once = Once::new();
    static mut RESULT: c_int = 0;
    ONCE.call_once(|| unsafe { RESULT = ffi::vit_ps_init() });
    match unsafe { RESULT } {
        0 => Ok(()),
        e => Err(error(e)),
    }
}

fn text(p: *const c_char) -> String {
    if p.is_null() { String::new() } else { unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned() }
}

/// chiaki's log lines: (level: 1 error, 2 warning, 4 info …, text).
pub type Log = Arc<dyn Fn(i32, &str) + Send + Sync>;

extern "C" fn log_cb(level: c_int, msg: *const c_char, user: *mut c_void) {
    let log = unsafe { &*(user as *const Log) };
    log(level, &text(msg));
}

// ---------------------------------------------------------------------------------------- finding

/// A PlayStation that answered on the home network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Host {
    pub addr: String,
    pub name: String,
    pub id: String,
    pub ps5: bool,
    /// on (false: in rest mode, or unknown)
    pub ready: bool,
    pub standby: bool,
}

type HostsCb = Box<dyn Fn(Vec<Host>) + Send + Sync>;

/// Looks for PlayStations while it lives (a broadcast every half second).
pub struct Discovery {
    ptr: *mut c_void,
    _cb: Box<HostsCb>,
    _log: Box<Log>,
}

unsafe impl Send for Discovery {}

extern "C" fn hosts_cb(hosts: *const ffi::Host, count: usize, user: *mut c_void) {
    let cb = unsafe { &*(user as *const HostsCb) };
    let list = if hosts.is_null() { &[][..] } else { unsafe { std::slice::from_raw_parts(hosts, count) } };
    cb(list
        .iter()
        .map(|h| Host { addr: text(h.addr), name: text(h.name), id: text(h.id), ps5: h.ps5, ready: h.state == 1, standby: h.state == 2 })
        .collect());
}

impl Discovery {
    /// Looks on every network this computer is on, and at `known` (consoles' addresses) too.
    pub fn start(known: &[std::net::Ipv4Addr], found: impl Fn(Vec<Host>) + Send + Sync + 'static, log: Log) -> Result<Self, Error> {
        init()?;
        let mut extra: Vec<u32> = broadcast_addresses().into_iter().chain(known.iter().copied()).map(u32::from).collect();
        extra.sort();
        extra.dedup();
        let cb: Box<HostsCb> = Box::new(Box::new(found));
        let log = Box::new(log);
        let mut err = 0;
        let ptr = unsafe {
            ffi::vit_discovery_start(extra.as_ptr(), extra.len(), hosts_cb, &*cb as *const HostsCb as *mut c_void, Some(log_cb), &*log as *const Log as *mut c_void, &mut err)
        };
        if ptr.is_null() {
            return Err(error(err));
        }
        Ok(Discovery { ptr, _cb: cb, _log: log })
    }
}

impl Drop for Discovery {
    fn drop(&mut self) {
        unsafe { ffi::vit_discovery_stop(self.ptr) };
    }
}

/// The broadcast address of each IPv4 network this computer is on (not loopback).
pub fn broadcast_addresses() -> Vec<std::net::Ipv4Addr> {
    let mut out = Vec::new();
    for i in if_addrs::get_if_addrs().unwrap_or_default() {
        if let if_addrs::IfAddr::V4(a) = i.addr {
            if a.ip.is_loopback() || a.ip.is_link_local() {
                continue;
            }
            let b = a.broadcast.unwrap_or_else(|| std::net::Ipv4Addr::from(u32::from(a.ip) | !u32::from(a.netmask)));
            if !out.contains(&b) {
                out.push(b);
            }
        }
    }
    out
}

/// Wakes the PS5 at `host` from rest mode (`credential`: [`Console::credential`]).
pub fn wakeup(host: &str, credential: u64) -> Result<(), Error> {
    init()?;
    let host = CString::new(host).map_err(|_| Error("bad address".into()))?;
    match unsafe { ffi::vit_wakeup(host.as_ptr(), credential) } {
        0 => Ok(()),
        e => Err(error(e)),
    }
}

// ---------------------------------------------------------------------------------------- registering

/// What registering with a PS5 gives: the keys to stream from it, kept by the app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registered {
    pub nickname: String,
    pub mac: [u8; 6],
    pub regist_key: [u8; 16],
    pub rp_key: [u8; 16],
}

impl Registered {
    /// The number the PS5 wakes up for: its registration key, written in hex.
    pub fn credential(&self) -> Option<u64> {
        credential(&self.regist_key)
    }
}

/// The wake-up number of a registration key (hex text, padded with zero bytes).
pub fn credential(regist_key: &[u8; 16]) -> Option<u64> {
    let end = regist_key.iter().position(|b| *b == 0).unwrap_or(regist_key.len());
    u64::from_str_radix(std::str::from_utf8(&regist_key[..end]).ok()?, 16).ok()
}

/// How registering ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistResult {
    Done(Registered),
    Failed,
    Cancelled,
}

type RegistCb = Box<dyn Fn(RegistResult) + Send + Sync>;

/// Registering with a PS5, while it lives (dropping it cancels).
pub struct Regist {
    ptr: *mut c_void,
    _cb: Box<RegistCb>,
    _log: Box<Log>,
}

unsafe impl Send for Regist {}

extern "C" fn regist_cb(result: c_int, reg: *const ffi::Registered, user: *mut c_void) {
    let cb = unsafe { &*(user as *const RegistCb) };
    cb(match result {
        0 if !reg.is_null() => {
            let r = unsafe { &*reg };
            let nickname = unsafe { CStr::from_ptr(r.nickname.as_ptr()) }.to_string_lossy().into_owned();
            let mut regist_key = [0u8; 16];
            for (d, s) in regist_key.iter_mut().zip(r.regist_key.iter()) {
                *d = *s as u8;
            }
            RegistResult::Done(Registered { nickname, mac: r.mac, regist_key, rp_key: r.rp_key })
        }
        2 => RegistResult::Cancelled,
        _ => RegistResult::Failed,
    });
}

impl Regist {
    /// Registers with the PS5 at `host`: the PSN account ID (8 bytes) and the 8-digit PIN the PS5
    /// shows (Settings > System > Remote Play > Link Device).
    pub fn start(host: &str, account_id: [u8; 8], pin: u32, done: impl Fn(RegistResult) + Send + Sync + 'static, log: Log) -> Result<Self, Error> {
        init()?;
        let host = CString::new(host).map_err(|_| Error("bad address".into()))?;
        let cb: Box<RegistCb> = Box::new(Box::new(done));
        let log = Box::new(log);
        let mut err = 0;
        let ptr = unsafe {
            ffi::vit_regist_start(
                host.as_ptr(),
                account_id.as_ptr(),
                pin,
                regist_cb,
                &*cb as *const RegistCb as *mut c_void,
                Some(log_cb),
                &*log as *const Log as *mut c_void,
                &mut err,
            )
        };
        if ptr.is_null() {
            return Err(error(err));
        }
        Ok(Regist { ptr, _cb: cb, _log: log })
    }
}

impl Drop for Regist {
    fn drop(&mut self) {
        unsafe { ffi::vit_regist_free(self.ptr) };
    }
}

// ---------------------------------------------------------------------------------------- playing

/// What happens in a stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Connected,
    /// The PS5 asks for the account's login passcode (`wrong`: the one given was wrong).
    LoginPin { wrong: bool },
    /// The stream ended (`error`: not by our own stop or the PS5 turning off).
    Quit { error: bool, why: String },
    Rumble { left: u8, right: u8 },
    Nickname(String),
}

/// What a stream hands over (each on chiaki's threads).
pub trait Handler: Send + Sync {
    fn event(&self, e: Event);
    /// One encoded frame, and its number in the stream (-1: not known); false if it could not be
    /// decoded (a new key frame is then asked for).
    fn video(&self, frame: &[u8], frames_lost: i32, frame_index: i32) -> bool;
    fn audio_header(&self, channels: u32, rate: u32);
    /// One Opus packet.
    fn audio(&self, packet: &[u8]);
    fn log(&self, level: i32, text: &str);
}

/// The picture asked for.
#[derive(Debug, Clone, Copy)]
pub struct Video {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// kbit/s
    pub bitrate: u32,
    pub h265: bool,
}

impl Video {
    /// Remote Play's 1080p (the most the PS5 streams in H.264), 60 frames a second.
    pub fn p1080(fps: u32) -> Self {
        Video { width: 1920, height: 1080, fps, bitrate: 15000, h265: false }
    }
    /// 1080p at twice the bitrate: a sharper picture (small text, dark scenes) that needs a steady
    /// network, best a cable for the PS5 and this computer.
    pub fn p1080_high(fps: u32) -> Self {
        Video { bitrate: 30000, ..Self::p1080(fps) }
    }
    pub fn p720(fps: u32) -> Self {
        Video { width: 1280, height: 720, fps, bitrate: 10000, h265: false }
    }
}

/// A stream from the PS5 while it lives (dropping it ends it).
pub struct Session {
    ptr: *mut c_void,
    _handler: Box<Arc<dyn Handler>>,
}

unsafe impl Send for Session {}
unsafe impl Sync for Session {}

fn handler<'a>(user: *mut c_void) -> &'a Arc<dyn Handler> {
    unsafe { &*(user as *const Arc<dyn Handler>) }
}

extern "C" fn s_log(level: c_int, msg: *const c_char, user: *mut c_void) {
    handler(user).log(level, &text(msg));
}

extern "C" fn s_event(kind: c_int, arg: c_int, t: *const c_char, user: *mut c_void) {
    let e = match kind {
        1 => Event::Connected,
        2 => Event::LoginPin { wrong: arg != 0 },
        3 => Event::Quit { error: arg != 0, why: text(t) },
        4 => Event::Rumble { left: (arg >> 8) as u8, right: arg as u8 },
        5 => Event::Nickname(text(t)),
        _ => return,
    };
    handler(user).event(e);
}

extern "C" fn s_video(buf: *const u8, size: usize, frames_lost: c_int, _recovered: bool, frame_index: c_int, user: *mut c_void) -> bool {
    if buf.is_null() {
        return true;
    }
    handler(user).video(unsafe { std::slice::from_raw_parts(buf, size) }, frames_lost, frame_index)
}

extern "C" fn s_audio_header(channels: c_int, rate: c_int, _frame_size: c_int, user: *mut c_void) {
    handler(user).audio_header(channels.max(1) as u32, rate.max(1) as u32);
}

extern "C" fn s_audio(buf: *const u8, size: usize, user: *mut c_void) {
    if !buf.is_null() {
        handler(user).audio(unsafe { std::slice::from_raw_parts(buf, size) });
    }
}

/// A controller's state, as the PS5 takes it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Pad {
    /// [`buttons`]
    pub buttons: u32,
    pub l2: u8,
    pub r2: u8,
    pub lx: i16,
    pub ly: i16,
    pub rx: i16,
    pub ry: i16,
}

/// The controller's buttons (chiaki's ChiakiControllerButton).
pub mod buttons {
    pub const CROSS: u32 = 1 << 0;
    pub const CIRCLE: u32 = 1 << 1;
    pub const SQUARE: u32 = 1 << 2;
    pub const TRIANGLE: u32 = 1 << 3;
    pub const LEFT: u32 = 1 << 4;
    pub const RIGHT: u32 = 1 << 5;
    pub const UP: u32 = 1 << 6;
    pub const DOWN: u32 = 1 << 7;
    pub const L1: u32 = 1 << 8;
    pub const R1: u32 = 1 << 9;
    pub const L3: u32 = 1 << 10;
    pub const R3: u32 = 1 << 11;
    pub const OPTIONS: u32 = 1 << 12;
    pub const SHARE: u32 = 1 << 13;
    pub const TOUCHPAD: u32 = 1 << 14;
    pub const PS: u32 = 1 << 15;
}

impl Session {
    /// Connects to the registered PS5 at `host`; events tell how it goes.
    pub fn start(host: &str, regist_key: &[u8; 16], rp_key: &[u8; 16], video: Video, h: Arc<dyn Handler>) -> Result<Self, Error> {
        init()?;
        let host = CString::new(host).map_err(|_| Error("bad address".into()))?;
        let boxed: Box<Arc<dyn Handler>> = Box::new(h);
        let user = &*boxed as *const Arc<dyn Handler> as *mut c_void;
        let cb = ffi::Callbacks {
            user,
            log: Some(s_log),
            event: Some(s_event),
            video: Some(s_video),
            audio_header: Some(s_audio_header),
            audio_frame: Some(s_audio),
        };
        let key: Vec<c_char> = regist_key.iter().map(|b| *b as c_char).collect();
        let mut err = 0;
        let ptr = unsafe {
            ffi::vit_session_start(
                host.as_ptr(),
                key.as_ptr(),
                rp_key.as_ptr(),
                video.width,
                video.height,
                video.fps,
                video.bitrate,
                if video.h265 { 1 } else { 0 },
                &cb,
                &mut err,
            )
        };
        if ptr.is_null() {
            return Err(error(err));
        }
        Ok(Session { ptr, _handler: boxed })
    }

    pub fn controller(&self, p: Pad) {
        unsafe { ffi::vit_session_controller(self.ptr, p.buttons, p.l2, p.r2, p.lx, p.ly, p.rx, p.ry) };
    }

    /// The account's login passcode, when the PS5 asks for it.
    pub fn login_pin(&self, pin: &str) {
        if let Ok(p) = CString::new(pin) {
            unsafe { ffi::vit_session_login_pin(self.ptr, p.as_ptr()) };
        }
    }

    /// Asks for a key frame (after the decoder lost its place).
    pub fn request_key_frame(&self) {
        unsafe { ffi::vit_session_request_idr(self.ptr) };
    }

    /// Tells the PS5 that the frames `start..=end` (their numbers as `Handler::video` had them)
    /// couldn't be decoded, so its next picture doesn't build on them.
    pub fn report_corrupt(&self, start: i32, end: i32) {
        unsafe { ffi::vit_session_report_corrupt(self.ptr, start, end) };
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        unsafe { ffi::vit_session_free(self.ptr) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_library_starts() {
        init().expect("chiaki_lib_init");
    }

    #[test]
    fn the_wake_up_number_is_the_key_in_hex() {
        let mut k = [0u8; 16];
        k[..8].copy_from_slice(b"1a2b3c4d");
        assert_eq!(credential(&k), Some(0x1a2b3c4d));
        assert_eq!(credential(&[0u8; 16]), None);
    }

    /// (on a home network with a PlayStation: `cargo test -p vi-translator-ps -- --ignored --nocapture`)
    #[test]
    #[ignore]
    fn finds_the_playstations_on_this_network() {
        let found = Arc::new(std::sync::Mutex::new(Vec::new()));
        let f = found.clone();
        let d = Discovery::start(&[], move |h| *f.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = h, Arc::new(|l, t| eprintln!("[chiaki {l}] {t}"))).unwrap();
        std::thread::sleep(std::time::Duration::from_secs(4));
        drop(d);
        let found = found.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        eprintln!("{found:#?}");
        assert!(!found.is_empty(), "no PlayStation answered");
    }

    #[test]
    fn looking_for_consoles_starts_and_stops() {
        let d = Discovery::start(&[], |_| {}, Arc::new(|_, _| {})).expect("discovery");
        std::thread::sleep(std::time::Duration::from_millis(300));
        drop(d);
    }
}
