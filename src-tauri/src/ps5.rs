//! PS5 Remote Play inside VI Translator (chiaki-ng, through the `vi-translator-ps` crate): the
//! player signs in to PSN on Sony's own page (for the account ID registering needs), the app finds
//! the PS5 on the home network and registers with it using the PIN the PS5 shows, and from then on
//! the PS5 is a picture source ("ps5:"): its picture goes straight to the reading and the preview
//! (Play shows it full screen), its sound to the speakers, and this computer's controllers to it.
//!
//! What the app keeps (ps5.json in its data folder): the PSN account ID (a number, no password or
//! token) and the PS5's registration keys.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};
use vi_translator_ps as ps;

use crate::capture::{Shared, YuvColor, YuvFrame, YuvLayout};

/// The source value for the PS5.
pub const PREFIX: &str = "ps5:";
const PSN_LABEL: &str = "psn";

// Sony's Remote Play app for PCs, as chiaki-ng signs in with it (gui/include/psnaccountid.h)
const CLIENT_ID: &str = "ba495a24-818c-472b-b12d-ff231c1b5745";
const CLIENT_SECRET: &str = "mvaiZkRsAsI1IBkY";
const REDIRECT: &str = "https://remoteplay.dl.playstation.net/remoteplay/redirect";
const SCOPE: &str = "psn:clientapp referenceDataService:countryConfig.read pushNotification:webSocket.desktop.connect sessionManager:remotePlaySession.system.update";
const TOKEN_URL: &str = "https://auth.api.sonyentertainmentnetwork.com/2.0/oauth/token";

/// The registered PS5.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Console {
    pub host: String,
    pub nickname: String,
    /// base64 of the 16-byte keys and the 6-byte MAC address
    pub regist_key: String,
    pub rp_key: String,
    pub mac: String,
}

impl Console {
    fn keys(&self) -> Option<([u8; 16], [u8; 16])> {
        let rk: [u8; 16] = B64.decode(&self.regist_key).ok()?.try_into().ok()?;
        let rp: [u8; 16] = B64.decode(&self.rp_key).ok()?.try_into().ok()?;
        Some((rk, rp))
    }
}

/// ps5.json
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Saved {
    /// the PSN account ID (8 bytes, base64)
    #[serde(default)]
    pub account_id: String,
    /// the PSN account's online name, if Sony said
    #[serde(default)]
    pub account_name: String,
    #[serde(default)]
    pub console: Option<Console>,
    /// the stream's height: 1080 or 720 (0: 1080)
    #[serde(default)]
    pub resolution: u32,
    /// 1080p at the higher bitrate (ps::Video::p1080_high)
    #[serde(default)]
    pub high_bitrate: bool,
    /// Smooth play: fewer frames read for text while the PS5 plays (see SMOOTH_READ); none: on
    #[serde(default)]
    pub smooth: Option<bool>,
    /// the PS5's sound on this computer: off, and its volume (0–100; none: 100)
    #[serde(default)]
    pub sound_off: bool,
    #[serde(default)]
    pub volume: Option<u32>,
    /// the picture decoded by OpenH264 on the CPU even where the computer has its own decoder
    #[serde(default)]
    pub software_decode: bool,
    /// Read only: the PS5 plays on its own screen with its own controller; the app only reads
    /// the stream (no controller, no sound here) and the translations go to the home-network
    /// page (an iPad beside the screen)
    #[serde(default)]
    pub read_only: bool,
    /// HEVC asked of the PS5 instead of H.264 (the Mac only: VideoToolbox decodes it): a cleaner
    /// picture for the same bitrate. Falls back to H.264 by itself when it doesn't work.
    #[serde(default)]
    pub hevc: bool,
}

struct Global {
    app: AppHandle,
    file: PathBuf,
    saved: Mutex<Saved>,
    /// the stream asks for the account's login passcode (Some(wrong))
    pin_wanted: Mutex<Option<bool>>,
    pin: Mutex<Option<String>>,
    /// what the last PSN sign-in or registering went wrong with
    error: Mutex<String>,
    /// the stream starts again (its quality changed, or the PS5 was unlinked)
    restart: AtomicBool,
    /// the sound as the settings say, read by the playing stream
    sound: Arc<ps::play::Sound>,
    /// smooth play as the settings say, read by the playing stream
    smooth: AtomicBool,
}

/// PS5 frames dropped in the last second because this computer decoded them too late (for the
/// performance mode, load.rs; it takes the number and leaves 0).
/// What decodes the playing stream's picture (empty: no stream).
static DECODER: Mutex<String> = Mutex::new(String::new());

pub static LATE_LAST_SECOND: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Smooth play: the frames read for text while the PS5 plays at most this often (20 a second), so
/// the reading leaves the computer free to decode the picture. A line on screen is still read within
/// 50 ms of appearing; the game's subtitles stay up for seconds.
const SMOOTH_READ: Duration = Duration::from_millis(50);

static G: OnceLock<Global> = OnceLock::new();

fn g() -> &'static Global {
    G.get().expect("ps5::init")
}

/// At startup: the app and its data folder.
pub fn init(app: AppHandle, data_dir: &Path) {
    let file = data_dir.join("ps5.json");
    let saved: Saved = std::fs::read_to_string(&file).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    ps::play::READ_ONLY.store(saved.read_only, Ordering::Relaxed);
    let _ = G.set(Global {
        app,
        file,
        pin_wanted: Mutex::new(None),
        pin: Mutex::new(None),
        error: Mutex::new(String::new()),
        restart: AtomicBool::new(false),
        sound: Arc::new(ps::play::Sound::new(!saved.sound_off, saved.volume.unwrap_or(100))),
        smooth: AtomicBool::new(saved.smooth != Some(false)),
        saved: Mutex::new(saved),
    });
}

fn save(s: &Saved) {
    let g = g();
    *g.saved.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = s.clone();
    if let Ok(t) = serde_json::to_string_pretty(s) {
        let _ = std::fs::write(&g.file, t);
    }
    let _ = g.app.emit_to("main", "ps5", status());
}

pub fn saved() -> Saved {
    g().saved.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
}

/// The Source tab's PS5 panel: signed in? which PS5? does the stream want the login passcode?
pub fn status() -> serde_json::Value {
    let g = g();
    let s = g.saved.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
    serde_json::json!({
        "signed_in": !s.account_id.is_empty(),
        "account": s.account_name,
        "account_id": s.account_id,
        "resolution": if s.resolution == 720 { 720 } else { 1080 },
        "high_bitrate": s.high_bitrate && s.resolution != 720,
        "smooth": s.smooth != Some(false),
        "sound": !s.sound_off,
        "volume": s.volume.unwrap_or(100),
        "software_decode": s.software_decode,
        "read_only": s.read_only,
        "hevc": s.hevc && cfg!(target_os = "macos"),
        "hevc_off_now": HEVC_OFF.load(Ordering::Relaxed),
        "decoder": DECODER.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone(),
        "console": s.console.as_ref().map(|c| serde_json::json!({ "name": c.nickname, "host": c.host })),
        "pin_wanted": *g.pin_wanted.lock().unwrap_or_else(std::sync::PoisonError::into_inner),
        "error": g.error.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone(),
    })
}

fn set_error(e: impl Into<String>) {
    *g().error.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = e.into();
    let _ = g().app.emit_to("main", "ps5", status());
}

// ---------------------------------------------------------------------------------------- PSN

fn enc(s: &str) -> String {
    let mut o = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            o.push(b as char);
        } else {
            o.push_str(&format!("%{b:02X}"));
        }
    }
    o
}

fn login_url() -> String {
    format!(
        "https://auth.api.sonyentertainmentnetwork.com/2.0/oauth/authorize?service_entity=urn:service-entity:psn&response_type=code\
         &client_id={CLIENT_ID}&redirect_uri={}&scope={}&request_locale=en_US&ui=pr&service_logo=ps&layout_type=popup\
         &smcid=remoteplay&prompt=always&PlatformPrivacyWs1=minimal",
        enc(REDIRECT),
        enc(SCOPE)
    )
}

/// The account's number (decimal) as Remote Play's account ID: 8 bytes, little-endian.
pub fn account_id_of(user_id: &str) -> Option<[u8; 8]> {
    user_id.trim().parse::<u64>().ok().map(u64::to_le_bytes)
}

/// The account ID (and online name) for the code Sony's page sent back.
fn account_from_code(code: &str) -> anyhow::Result<([u8; 8], String)> {
    let client = reqwest::blocking::Client::builder().timeout(Duration::from_secs(20)).build()?;
    let form = format!("grant_type=authorization_code&code={}&scope={}&redirect_uri={}", enc(code), enc(SCOPE), enc(REDIRECT));
    let r = client
        .post(TOKEN_URL)
        .basic_auth(CLIENT_ID, Some(CLIENT_SECRET))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(form)
        .send()?;
    let st = r.status();
    let t: serde_json::Value = r.json()?;
    let token = t["access_token"].as_str().ok_or_else(|| anyhow::anyhow!("PSN answered {st}: {t}"))?.to_string();
    let r = client.get(format!("{TOKEN_URL}/{}", enc(&token))).basic_auth(CLIENT_ID, Some(CLIENT_SECRET)).send()?;
    let info: serde_json::Value = r.json()?;
    let id = info["user_id"].as_str().and_then(account_id_of).ok_or_else(|| anyhow::anyhow!("PSN gave no account number"))?;
    Ok((id, info["online_id"].as_str().unwrap_or_default().to_string()))
}

/// Opens Sony's sign-in page in a window of the app. When Sony sends back its code, the account ID
/// is kept and the window closes (the password stays on Sony's page; the token is dropped).
pub fn sign_in(data_dir: &Path) -> Result<(), String> {
    let app = g().app.clone();
    if let Some(w) = app.get_webview_window(PSN_LABEL) {
        let _ = w.set_focus();
        return Ok(());
    }
    set_error("");
    let url: tauri::Url = login_url().parse().map_err(|e| format!("{e}"))?;
    let done = Arc::new(AtomicBool::new(false));
    let b = WebviewWindowBuilder::new(&app, PSN_LABEL, WebviewUrl::External(url))
        .title("PlayStation Network — Barqawi")
        .inner_size(520.0, 760.0)
        .center()
        .on_navigation(move |u| {
            if !u.as_str().starts_with(REDIRECT) {
                return true;
            }
            if done.swap(true, Ordering::Relaxed) {
                return false;
            }
            let q: std::collections::HashMap<String, String> = u.query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
            std::thread::spawn(move || {
                let g = g();
                match q.get("code").filter(|c| !c.is_empty()) {
                    Some(code) => match account_from_code(code) {
                        Ok((id, name)) => {
                            let mut s = saved();
                            s.account_id = B64.encode(id);
                            s.account_name = name;
                            set_error("");
                            save(&s);
                        }
                        Err(e) => set_error(format!("PSN: {e:#}")),
                    },
                    None => set_error(format!(
                        "PSN: {}",
                        q.get("error_description").or(q.get("error")).map(String::as_str).unwrap_or("no code")
                    )),
                }
                if let Some(w) = g.app.get_webview_window(PSN_LABEL) {
                    let _ = w.close();
                }
            });
            false
        });
    // (its own browser profile on Windows, apart from the app's pages and the Xbox player's)
    #[cfg(windows)]
    let b = b.data_directory(data_dir.join("psn-web"));
    #[cfg(not(windows))]
    let _ = data_dir;
    b.build().map_err(|e| e.to_string())?;
    Ok(())
}

/// Sony's sign-in page, for signing in with a browser instead (its bot check sometimes refuses the
/// app's window): the browser ends on the redirect address, which the player pastes back.
pub fn login_url_for_browser() -> String {
    login_url()
}

/// The sign-in code in the address a browser ended on (…/remoteplay/redirect?code=…).
pub fn code_in(address: &str) -> Option<String> {
    let u: tauri::Url = address.trim().parse().ok()?;
    u.query_pairs().find(|(k, _)| k == "code").map(|(_, v)| v.into_owned()).filter(|c| !c.is_empty())
}

/// Signs in with the address a browser ended on. Blocking.
pub fn sign_in_with_address(address: &str) -> Result<(), String> {
    let code = code_in(address).ok_or("no code")?;
    let (id, name) = account_from_code(&code).map_err(|e| format!("{e:#}"))?;
    let mut s = saved();
    s.account_id = B64.encode(id);
    s.account_name = name;
    set_error("");
    save(&s);
    Ok(())
}

/// A PSN account ID (8 bytes in base64, 12 characters), typed or pasted.
pub fn valid_account_id(id: &str) -> bool {
    B64.decode(id.trim()).is_ok_and(|v| v.len() == 8)
}

pub fn set_account_id(id: &str) -> Result<(), String> {
    if !valid_account_id(id) {
        return Err("bad account ID".into());
    }
    let mut s = saved();
    s.account_id = id.trim().to_string();
    s.account_name.clear();
    save(&s);
    Ok(())
}

pub fn set_resolution(r: u32, high_bitrate: bool) {
    let mut s = saved();
    s.resolution = if r == 720 { 720 } else { 1080 };
    s.high_bitrate = high_bitrate && s.resolution == 1080;
    save(&s);
    g().restart.store(true, Ordering::Relaxed);
}

/// HEVC on or off (the Mac); the stream starts again.
pub fn set_hevc(on: bool) {
    let mut s = saved();
    s.hevc = on;
    save(&s);
    HEVC_OFF.store(false, Ordering::Relaxed);
    g().restart.store(true, Ordering::Relaxed);
}

/// HEVC didn't work in this run (the stream or the decoder failed in it): H.264 until the app starts
/// again or HEVC is chosen again.
static HEVC_OFF: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The picture decoded by the computer's own decoder (Automatic) or on the CPU (Software); the
/// stream starts again.
pub fn set_decode(software: bool) {
    let mut s = saved();
    s.software_decode = software;
    save(&s);
    g().restart.store(true, Ordering::Relaxed);
}

/// Read only on or off; at once (the stream goes on).
pub fn set_read_only(on: bool) {
    ps::play::READ_ONLY.store(on, Ordering::Relaxed);
    let mut s = saved();
    s.read_only = on;
    save(&s);
}

/// The PS5's sound on this computer: on or off, and its volume (0–100); heard at once.
pub fn set_sound(on: bool, volume: u32) {
    let g = g();
    g.sound.on.store(on, Ordering::Relaxed);
    g.sound.volume.store(volume.min(100), Ordering::Relaxed);
    let mut s = saved();
    s.sound_off = !on;
    s.volume = Some(volume.min(100));
    *g.saved.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = s.clone();
    if let Ok(t) = serde_json::to_string_pretty(&s) {
        let _ = std::fs::write(&g.file, t);
    }
}

/// Smooth play on or off; at once.
pub fn set_smooth(on: bool) {
    let g = g();
    g.smooth.store(on, Ordering::Relaxed);
    let mut s = saved();
    s.smooth = Some(on);
    save(&s);
}

/// Wakes the linked PS5 from rest mode.
pub fn wake() -> Result<(), String> {
    let c = saved().console.ok_or("no PS5 linked")?;
    let (rk, _) = c.keys().ok_or("bad keys")?;
    let cred = ps::credential(&rk).ok_or("bad keys")?;
    ps::wakeup(&c.host, cred).map_err(|e| e.to_string())
}

pub fn sign_out() {
    let mut s = saved();
    s.account_id.clear();
    s.account_name.clear();
    save(&s);
}

// ---------------------------------------------------------------------------------------- finding and linking

#[derive(Serialize, Clone)]
pub struct Found {
    pub host: String,
    pub name: String,
    pub on: bool,
    pub standby: bool,
}

/// The PS5s that answer on the home network within a few seconds.
pub fn find() -> Result<Vec<Found>, String> {
    let known: Vec<std::net::Ipv4Addr> = saved().console.and_then(|c| c.host.parse().ok()).into_iter().collect();
    let list = Arc::new(Mutex::new(Vec::new()));
    let l = list.clone();
    let d = ps::Discovery::start(&known, move |h| *l.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = h, Arc::new(|_, _| {})).map_err(|e| e.to_string())?;
    let t = Instant::now();
    while t.elapsed() < Duration::from_secs(6) {
        std::thread::sleep(Duration::from_millis(100));
        // (one answer is usually all there is: stop waiting a little after it)
        if !list.lock().unwrap_or_else(std::sync::PoisonError::into_inner).is_empty() && t.elapsed() > Duration::from_millis(1500) {
            break;
        }
    }
    drop(d);
    let hosts = list.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
    Ok(hosts.into_iter().filter(|h| h.ps5).map(|h| Found { host: h.addr, name: h.name, on: h.ready, standby: h.standby }).collect())
}

/// Registers with the PS5 at `host` with the PIN it shows; keeps its keys. Blocking (up to ~30 s).
pub fn link(host: &str, pin: &str) -> Result<String, String> {
    let s = saved();
    let id: [u8; 8] = B64.decode(&s.account_id).ok().and_then(|v| v.try_into().ok()).ok_or("Sign in to PlayStation Network first.")?;
    let pin: u32 = pin.trim().replace(' ', "").parse().map_err(|_| "The PIN is the 8 digits the PS5 shows.")?;
    let (tx, rx) = std::sync::mpsc::channel();
    let log = Arc::new(Mutex::new(Vec::<String>::new()));
    let l = log.clone();
    let r = ps::Regist::start(
        host,
        id,
        pin,
        move |res| {
            let _ = tx.send(res);
        },
        Arc::new(move |level, t| {
            if level <= 2 {
                l.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(t.to_string());
            }
        }),
    )
    .map_err(|e| e.to_string())?;
    let res = rx.recv_timeout(Duration::from_secs(30));
    drop(r);
    match res {
        Ok(ps::RegistResult::Done(reg)) => {
            let mut s = saved();
            s.console = Some(Console {
                host: host.to_string(),
                nickname: reg.nickname.clone(),
                regist_key: B64.encode(reg.regist_key),
                rp_key: B64.encode(reg.rp_key),
                mac: B64.encode(reg.mac),
            });
            set_error("");
            save(&s);
            Ok(reg.nickname)
        }
        Ok(ps::RegistResult::Cancelled) => Err("Linking was cancelled.".into()),
        _ => {
            let why = log.lock().unwrap_or_else(std::sync::PoisonError::into_inner).last().cloned().unwrap_or_default();
            Err(format!("The PS5 did not accept the PIN or the account. {why}").trim().to_string())
        }
    }
}

pub fn forget() {
    let mut s = saved();
    s.console = None;
    save(&s);
    g().restart.store(true, Ordering::Relaxed);
}

/// The login passcode the stream asked for.
pub fn give_pin(pin: &str) {
    *g().pin.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(pin.trim().to_string());
}

// ---------------------------------------------------------------------------------------- playing

/// The PS5's picture going to `Shared` while it lives (as a capture card's does).
pub struct Ps5Capture {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Ps5Capture {
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
impl Drop for Ps5Capture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// The picture into the app: every frame as YUY2 for the preview (at the preview rate) and as RGBA
/// for the reading (at the capture rate). The colour is BT.709 studio range once the PS5's own
/// second studio range is stretched out (play::i420_to_yuy2).
struct ToShared {
    shared: Arc<Shared>,
    times: Mutex<(Instant, Instant)>,
    state: Mutex<Option<ps::Event>>,
}

const COLOR: YuvColor = YuvColor { bt709: true, full_range: false };

impl ToShared {
    /// Time for the next frame read for text: as the timing says, and with smooth play no more than
    /// once every SMOOTH_READ.
    fn full_due(&self, last: Instant) -> bool {
        // (the phone's anchor follows its screen on every frame while it's up: full frames at the capture rate
        // then, else the Arabic on the phone would move in steps of SMOOTH_READ)
        self.shared.full_due(last) && (!g().smooth.load(Ordering::Relaxed) || last.elapsed() >= SMOOTH_READ || vi_translator_core::phone_anchor::with(|a| a.is_set()))
    }
}

impl ps::play::Screen for ToShared {
    // (most frames go nowhere: neither the reading nor the preview is due; they aren't copied out)
    fn wants_frame(&self) -> bool {
        let t = self.times.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        self.full_due(t.0) || self.shared.preview_due(t.1)
    }

    fn frame(&self, f: ps::play::I420) {
        let (full, preview) = {
            let t = self.times.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            (self.full_due(t.0), self.shared.preview_due(t.1))
        };
        if !full && !preview {
            return;
        }
        let w = f.w & !1;
        let mut t = self.times.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if full {
            t.0 = Instant::now();
            // (straight from the planes: no YUY2 picture in between when only the reading wants it)
            self.shared.put_full(w, f.h, i420_to_rgba(&f));
        }
        if preview {
            t.1 = Instant::now();
            // (the planes as they are, packed: the window turns them into colour, levels stretched,
            // on the GPU — a quarter fewer bytes than YUY2, and no conversion here)
            let (pw, ph) = (w, f.h & !1);
            self.shared.put_preview_yuv(YuvFrame { w: pw, h: ph, color: COLOR, layout: YuvLayout::I420, stretch: true, data: pack_i420(&f, pw, ph) });
        }
    }

    fn event(&self, e: &ps::Event) {
        match e {
            ps::Event::LoginPin { wrong } => {
                *g().pin_wanted.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(*wrong);
                let _ = g().app.emit_to("main", "ps5", status());
            }
            ps::Event::Connected => {
                *g().pin_wanted.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = None;
                let _ = g().app.emit_to("main", "ps5", status());
            }
            _ => {}
        }
        *self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(e.clone());
    }

    fn log(&self, level: i32, text: &str) {
        if level <= 2 || std::env::var_os("VI_TRANSLATOR_DEBUG").is_some() {
            vi_translator_core::vlog!("[ps5] {text}");
        }
    }
}

/// The planes packed without their rows' padding, `w` × `h` (even): Y, then U, then V.
fn pack_i420(f: &ps::play::I420, w: u32, h: u32) -> Vec<u8> {
    let (w, h) = (w as usize, h as usize);
    let (cw, ch) = (w / 2, h / 2);
    let mut out = Vec::with_capacity(w * h + 2 * cw * ch);
    for y in 0..h {
        out.extend_from_slice(&f.y[y * f.y_stride..y * f.y_stride + w]);
    }
    for plane in [&f.u, &f.v] {
        for y in 0..ch {
            out.extend_from_slice(&plane[y * f.uv_stride..y * f.uv_stride + cw]);
        }
    }
    out
}

/// The PS5's planes as RGBA for the reading: the same picture as `yuy2_to_rgba(i420_to_yuy2(f, true))`
/// (the second studio range stretched out, then BT.709 studio range to RGB), in one pass.
fn i420_to_rgba(f: &ps::play::I420) -> Vec<u8> {
    // the stretch (play::i420_to_yuy2) and the conversion (capture::yuy2_to_rgba), as tables
    let mut ly = [0i32; 256];
    let mut lc = [0i32; 256];
    for i in 0..256 {
        let sy = (((i as i32 - 16) * 255 + 109) / 219).clamp(0, 255);
        let sc = (128 + ((i as i32 - 128) * 255) / 219).clamp(0, 255);
        ly[i] = (sy - 16) * (255.0f64 / 219.0 * 1024.0).round() as i32;
        lc[i] = sc - 128;
    }
    let k = |v: f64| (v * 255.0 / 224.0 * 1024.0).round() as i32;
    let (cr, cgu, cgv, cb) = (k(1.5748), k(0.1873), k(0.4681), k(1.8556));
    let clamp = |v: i32| ((v + 512) >> 10).clamp(0, 255) as u8;
    let (w, h) = (f.w as usize & !1, f.h as usize);
    let mut out = vec![0u8; w * h * 4];
    for y in 0..h {
        let yr = &f.y[y * f.y_stride..];
        let ur = &f.u[(y / 2) * f.uv_stride..];
        let vr = &f.v[(y / 2) * f.uv_stride..];
        let o = &mut out[y * w * 4..(y + 1) * w * 4];
        for x in (0..w).step_by(2) {
            let (u, v) = (lc[ur[x / 2] as usize], lc[vr[x / 2] as usize]);
            let (dr, dg, db) = (cr * v, cgu * u + cgv * v, cb * u);
            for j in 0..2 {
                let l = ly[yr[x + j] as usize];
                let p = &mut o[(x + j) * 4..(x + j) * 4 + 4];
                p[0] = clamp(l + dr);
                p[1] = clamp(l - dg);
                p[2] = clamp(l + db);
                p[3] = 255;
            }
        }
    }
    out
}

/// The status line while playing: the frame rate of the last second, and what was lost in the last
/// ten (on the network: the Wi-Fi or the cable; too late: this computer decodes too slowly), so a
/// stutter can be told apart. ui/i18n.js has the same words, for the translations.
fn playing_status(name: &str, pads: u64, window: &std::collections::VecDeque<(u64, u64, u64)>) -> String {
    let fps = window.back().map_or(0, |w| w.0);
    let net: u64 = window.iter().map(|w| w.1).sum();
    let late: u64 = window.iter().map(|w| w.2).sum();
    format!("Playing on {name} ({pads} controller(s)) · {fps} fps · lost on the network: {net} · too late to decode: {late} (last 10 s)")
}

/// Connects to the registered PS5 (waking it if it rests) and plays until stopped, the stream ends
/// or it fails (the manager starts it again).
pub fn start(shared: Arc<Shared>) -> Ps5Capture {
    let stop = Arc::new(AtomicBool::new(false));
    let s2 = stop.clone();
    let handle = std::thread::spawn(move || {
        let set = |s: String| shared.set_status(s);
        let Some(c) = saved().console else {
            set("PS5: link your PS5 in Source first.".into());
            std::thread::sleep(Duration::from_secs(2));
            return;
        };
        let Some((rk, rp)) = c.keys() else {
            set("PS5: its saved keys are damaged; link it again.".into());
            std::thread::sleep(Duration::from_secs(2));
            return;
        };
        // asleep? wake it, and wait until it answers as on
        let state = find().unwrap_or_default().into_iter().find(|f| f.host == c.host || f.name == c.nickname);
        let host = state.as_ref().map(|f| f.host.clone()).unwrap_or(c.host.clone());
        if state.as_ref().is_some_and(|f| !f.on) {
            set(format!("Waking {} …", c.nickname));
            if let Some(cred) = ps::credential(&rk) {
                let _ = ps::wakeup(&host, cred);
            }
            let t = Instant::now();
            while t.elapsed() < Duration::from_secs(40) && !s2.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_secs(2));
                if find().unwrap_or_default().iter().any(|f| f.host == host && f.on) {
                    break;
                }
            }
        }
        if s2.load(Ordering::Relaxed) {
            return;
        }
        set(format!("Connecting to {} …", c.nickname));
        let screen = Arc::new(ToShared {
            shared: shared.clone(),
            times: Mutex::new((Instant::now() - Duration::from_secs(1), Instant::now() - Duration::from_secs(1))),
            state: Mutex::new(None),
        });
        let s = saved();
        let mut video = match (s.resolution, s.high_bitrate) {
            (720, _) => ps::Video::p720(60),
            (_, true) => ps::Video::p1080_high(60),
            _ => ps::Video::p1080(60),
        };
        video.h265 = cfg!(target_os = "macos") && s.hevc && !HEVC_OFF.load(Ordering::Relaxed);
        let decode = if s.software_decode { ps::hwdec::Decode::Software } else { ps::hwdec::Decode::Auto };
        let player = match ps::play::Player::start(&host, &rk, &rp, video, decode, g().sound.clone(), screen.clone()) {
            Ok(p) => p,
            Err(e) => {
                set(format!("PS5: {e}"));
                std::thread::sleep(Duration::from_secs(3));
                return;
            }
        };
        // (HEVC asked: a picture within 8 s, or H.264)
        let mut started = Instant::now();
        let mut last_status = Instant::now() - Duration::from_secs(5);
        // the last ten seconds, a second at a time: (pictures, lost on the network, too late to decode)
        let mut window = std::collections::VecDeque::<(u64, u64, u64)>::new();
        let mut before = (0u64, 0u64, 0u64);
        g().restart.store(false, Ordering::Relaxed);
        while !s2.load(Ordering::Relaxed) && !g().restart.swap(false, Ordering::Relaxed) {
            if let Some(pin) = g().pin.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take() {
                player.login_pin(&pin);
            }
            let ev = screen.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
            if let Some(ps::Event::Quit { error, why }) = ev {
                set(if error { format!("PS5: the stream ended ({why})") } else { format!("PS5: {why}") });
                *g().pin_wanted.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = None;
                std::thread::sleep(Duration::from_secs(3));
                break;
            }
            // (not while the PS5 waits for the login passcode: no picture then is no fault of HEVC's)
            let pin_up = g().pin_wanted.lock().unwrap_or_else(std::sync::PoisonError::into_inner).is_some();
            if pin_up {
                started = Instant::now();
            }
            if video.h265 && (player.stats.hevc_failed.load(Ordering::Relaxed) || (!pin_up && started.elapsed() > Duration::from_secs(8) && player.stats.frames.load(Ordering::Relaxed) == 0)) {
                HEVC_OFF.store(true, Ordering::Relaxed);
                vi_translator_core::vlog!("[ps5] HEVC didn't decode here: the stream starts again in H.264");
                break;
            }
            if last_status.elapsed() > Duration::from_secs(1) {
                last_status = Instant::now();
                let st = &player.stats;
                let frames = st.frames.load(Ordering::Relaxed);
                // (too late: frames the full queue couldn't take, and those dropped from a long backlog)
                let late = st.late.load(Ordering::Relaxed) + st.backlog_dropped.load(Ordering::Relaxed);
                let now = (frames, st.net_lost.load(Ordering::Relaxed), late);
                *DECODER.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = st.decoder.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
                window.push_back((now.0 - before.0, now.1 - before.1, now.2 - before.2));
                LATE_LAST_SECOND.fetch_add(now.2 - before.2, Ordering::Relaxed);
                // (frames lost or late in the last second: told in the log with where they went, so a
                // stutter's cause is in a check report — the network, or this computer decoding too slowly)
                let (lost_now, late_now) = (now.1 - before.1, now.2 - before.2);
                if lost_now + late_now >= 3 {
                    vi_translator_core::vlog!("[jank] last second: {} pictures, lost on the network {lost_now}, too late to decode {late_now} ({})", now.0 - before.0, st.decoder.lock().unwrap_or_else(std::sync::PoisonError::into_inner));
                }
                before = now;
                while window.len() > 10 {
                    window.pop_front();
                }
                let pads = st.controllers.load(Ordering::Relaxed);
                match ev {
                    Some(ps::Event::LoginPin { .. }) => set(format!("{}: enter your PlayStation login passcode in Source", c.nickname)),
                    _ if frames > 0 => set(playing_status(&c.nickname, pads, &window)),
                    _ => {}
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        drop(player);
        DECODER.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clear();
    });
    Ps5Capture { stop, handle: Some(handle) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_account_number_becomes_eight_little_endian_bytes() {
        assert_eq!(account_id_of("1"), Some([1, 0, 0, 0, 0, 0, 0, 0]));
        assert_eq!(account_id_of(" 4848584848484848484 "), Some(4848584848484848484u64.to_le_bytes()));
        assert_eq!(account_id_of("x"), None);
    }

    #[test]
    fn the_code_comes_from_the_address_the_browser_ended_on() {
        assert_eq!(code_in("https://remoteplay.dl.playstation.net/remoteplay/redirect?code=v3.AbC&cid=x").as_deref(), Some("v3.AbC"));
        assert_eq!(code_in(" https://remoteplay.dl.playstation.net/remoteplay/redirect?error=x "), None);
        assert_eq!(code_in("not an address"), None);
        assert!(valid_account_id("AQAAAAAAAAA="));
        assert!(!valid_account_id("AQAA"));
    }

    #[test]
    fn the_one_pass_colour_is_the_two_pass_one() {
        // every luma value, and chroma across its range, on a 32 x 16 picture with padded rows
        let (w, h, ys, us) = (32u32, 16u32, 40usize, 24usize);
        let y: Vec<u8> = (0..ys * h as usize).map(|i| (i * 7 % 256) as u8).collect();
        let u: Vec<u8> = (0..us * 8).map(|i| (i * 13 % 256) as u8).collect();
        let v: Vec<u8> = (0..us * 8).map(|i| (255 - i * 11 % 256) as u8).collect();
        let f = ps::play::I420 { w, h, y, u, v, y_stride: ys, uv_stride: us };
        let two = crate::capture::yuy2_to_rgba(&ps::play::i420_to_yuy2(&f, true), w, h, COLOR);
        assert_eq!(i420_to_rgba(&f), two);
    }

    #[test]
    fn the_planes_are_packed_without_padding() {
        // 4 x 2, rows padded to 6 (luma) and 3 (chroma)
        let f = ps::play::I420 { w: 4, h: 2, y: vec![1, 2, 3, 4, 0, 0, 5, 6, 7, 8, 0, 0], u: vec![9, 10, 0], v: vec![11, 12, 0], y_stride: 6, uv_stride: 3 };
        assert_eq!(pack_i420(&f, 4, 2), vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]);
    }

    #[test]
    fn the_status_counts_the_last_ten_seconds() {
        let mut w = std::collections::VecDeque::new();
        for i in 0..10u64 {
            w.push_back((60 - i, i % 2, if i == 9 { 3 } else { 0 }));
        }
        assert_eq!(
            playing_status("PS5", 1, &w),
            "Playing on PS5 (1 controller(s)) · 51 fps · lost on the network: 5 · too late to decode: 3 (last 10 s)"
        );
    }

    #[test]
    fn sony_s_address_is_escaped() {
        assert_eq!(enc("a b:c/d"), "a%20b%3Ac%2Fd");
        assert!(login_url().contains("redirect_uri=https%3A%2F%2Fremoteplay.dl.playstation.net%2Fremoteplay%2Fredirect"));
    }
}
