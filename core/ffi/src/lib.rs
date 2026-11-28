//! VI Translator's core for the phone apps (Kotlin and Swift through UniFFI). See ../README.md.
//!
//! One `Engine` per app: it keeps the settings, the translation memory, the downloaded translations
//! and layouts, the connected TVs and the TV page server. The app hands it the game picture
//! (`process_frame`, with its own text recognizer behind `TextRecognizer`) or each area's crop with
//! the words its OCR read (`process_areas`), a few times a second, and hears what to show through
//! `EngineListener`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use vi_translator_core::config::Config;
use vi_translator_core::engine::{self, AreaRead, Input};
use vi_translator_core::frame::FrameData;
use vi_translator_core::live::Live;
use vi_translator_core::memory::Memory;
use vi_translator_core::ocr::{Crop, Ocr, RawWord, TextEngine, Word};
use vi_translator_core::lan_page::LivePage;
use vi_translator_core::snapshot;
use vi_translator_core::tv_server::TvServer;
use vi_translator_core::{Paths, Sink, langs, sync, tv};

uniffi::setup_scaffolding!();

// ------------------------------------------------------------------------------------- what the app supplies

/// A word the app's text recognizer read, in the picture it was given (pixels).
#[derive(uniffi::Record, Clone, Debug)]
pub struct OcrWord {
    /// which text line it is on (0 = the first line the recognizer found)
    pub line: u32,
    pub text: String,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// The app's text recognizer (ML Kit, Apple Vision): the words in a picture of dark text on a light
/// background, BGRA (4 bytes per pixel, rows `width * 4` bytes; grey, so the channel order doesn't
/// matter). Called from the engine's reading thread, several times per area and frame (the core
/// reads each crop a few ways and keeps the best reading, as the desktop app does). Return no words
/// when it fails.
#[uniffi::export(with_foreign)]
pub trait TextRecognizer: Send + Sync {
    fn recognize(&self, bgra: Vec<u8>, width: u32, height: u32) -> Vec<OcrWord>;
    /// The largest picture side it takes (bigger pictures are halved until they fit).
    fn max_dimension(&self) -> u32;
}

/// A line read and translated, to show (or, with an empty `text` and no `error`, the area to clear).
#[derive(uniffi::Record, Clone, Debug)]
pub struct TranslatedLine {
    /// the area it was read in ("subtitles", "help" …)
    pub area: String,
    /// the English, with 🎮 for the game's button icons
    pub en: String,
    /// the translation as plain text, with 🎮 for the button icons ("" = clear the area)
    pub text: String,
    /// why it couldn't be translated (not shown over the game)
    pub error: Option<String>,
    /// from the translation memory
    pub cached: bool,
    /// when the English appeared (Unix ms; 0 = unknown)
    pub at_ms: u64,
    /// the whole event as the desktop's window gets it (segments with colours and icons, size, place)
    pub json: String,
}

/// What the engine tells the app. Called from the engine's threads: hop to the UI thread to show it.
#[uniffi::export(with_foreign)]
pub trait EngineListener: Send + Sync {
    /// a line to show, or an area to clear, at the time the desktop's overlays would
    fn on_translation(&self, line: TranslatedLine);
    /// a game page to show (its layout, JSON), or None when it's gone
    fn on_screen(&self, layout_json: Option<String>);
    /// a status line ("Downloading …", an OCR or translator problem)
    fn on_status(&self, message: String);
    /// anything else: "config" (the settings changed: a layout update), "pages" (the game's pages
    /// script changed), "tv-paired" ({"name"} or {"error"}); `json` is its payload
    fn on_event(&self, name: String, json: String);
}

// ------------------------------------------------------------------------------------- records

/// An area to read, as fractions of the frame (crop it with `Crop::from_frame`'s rounding: x0 =
/// floor(x * width) …).
#[derive(uniffi::Record, Clone, Debug)]
pub struct ReadArea {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    /// only text centred in the picture counts (subtitles)
    pub centered_only: bool,
}

/// A word in an area's crop (crop pixels), as the app's own OCR read it.
#[derive(uniffi::Record, Clone, Debug)]
pub struct CropWord {
    pub text: String,
    pub line: u32,
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// One area read by the app itself (`process_areas`).
#[derive(uniffi::Record, Clone, Debug)]
pub struct AreaWords {
    pub area_id: String,
    /// the crop, RGBA (rows `crop_width * 4` bytes)
    pub crop_rgba: Vec<u8>,
    pub crop_width: u32,
    pub crop_height: u32,
    pub words: Vec<CropWord>,
    /// the reading is settled (false: text still fading in; it waits longer)
    pub sure: bool,
}

/// A game the core knows (its built-in profile).
#[derive(uniffi::Record, Clone, Debug)]
pub struct Game {
    pub id: String,
    pub name: String,
}

/// A line shown lately (newest last), as the desktop's home-network page lists them.
#[derive(uniffi::Record, Clone, Debug)]
pub struct RecentLine {
    /// increases with every line (starts again when the engine does)
    pub id: u64,
    pub area: String,
    /// the English, with 🎮 for the button icons
    pub en: String,
    /// the translation, with 🎮 for the button icons
    pub text: String,
    /// when the English appeared (Unix ms)
    pub at_ms: u64,
}

#[derive(uniffi::Record, Clone, Debug)]
pub struct Language {
    pub code: String,
    pub name: String,
    pub native: String,
    pub rtl: bool,
}

/// A TV on the home network running the VI Translator TV app.
#[derive(uniffi::Record, Clone, Debug)]
pub struct FoundTv {
    /// where to reach it ("192.168.1.20:8792", or "offered:<id>" for a Samsung TV that offered its code)
    pub address: String,
    pub id: String,
    pub name: String,
    /// already connected to this phone
    pub connected: bool,
}

/// A connected TV.
#[derive(uniffi::Record, Clone, Debug)]
pub struct TvInfo {
    pub id: String,
    pub name: String,
    /// its page asked for the overlays in the last few seconds
    pub online: bool,
    /// shows the translations (`set_tv_on`); off, it stays connected and shows nothing
    pub on: bool,
}

#[derive(Debug, uniffi::Error)]
pub enum CoreError {
    /// something went wrong (the message says what)
    Failed { reason: String },
}

impl std::fmt::Display for CoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CoreError::Failed { reason } => f.write_str(reason),
        }
    }
}

impl std::error::Error for CoreError {}

fn failed(e: impl std::fmt::Display) -> CoreError {
    CoreError::Failed { reason: e.to_string() }
}

/// Why a TV couldn't be connected.
#[derive(Debug, uniffi::Error)]
pub enum PairError {
    /// not the code the TV shows
    WrongCode,
    /// too many wrong codes: the TV shows a new one
    TooMany,
    /// no answer, or not the TV app
    NoAnswer,
    /// the TV page server isn't running (`start_tv_server`), or there's no home network
    NotSharing,
}

impl std::fmt::Display for PairError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}

impl std::error::Error for PairError {}

impl PairError {
    fn code(&self) -> &'static str {
        match self {
            PairError::WrongCode => "wrong_code",
            PairError::TooMany => "too_many",
            PairError::NoAnswer => "no_answer",
            PairError::NotSharing => "not_sharing",
        }
    }
}

impl From<tv::PairError> for PairError {
    fn from(e: tv::PairError) -> Self {
        match e {
            tv::PairError::WrongCode => PairError::WrongCode,
            tv::PairError::TooMany => PairError::TooMany,
            tv::PairError::NoAnswer => PairError::NoAnswer,
            tv::PairError::NotSharing => PairError::NotSharing,
        }
    }
}

// ------------------------------------------------------------------------------------- glue

/// The app's recognizer as the core's OCR engine.
struct HostOcr(Arc<dyn TextRecognizer>, Mutex<std::collections::VecDeque<(u64, Vec<RawWord>)>>);

/// How many recent crops' readings are kept (an area can be read a few ways a frame: bands, crops).
const SEEN_CROPS: usize = 24;

/// A crop's look, coarse enough that the stream's noise doesn't change it and fine enough that other
/// text does: its size and the mean brightness of a 64×16 grid of cells (each cell a mean over many
/// pixels, so a thin letter changes it where a sampled pixel could miss it), in 32 steps.
fn look_of(bgra: &[u8], w: u32, h: u32) -> u64 {
    use std::hash::{Hash, Hasher};
    let (w, h) = (w as usize, h as usize);
    let (gw, gh) = (w.clamp(1, 64), h.clamp(1, 16));
    let mut sums = vec![0u64; gw * gh];
    let mut counts = vec![0u64; gw * gh];
    for y in 0..h {
        let gy = y * gh / h;
        let row = &bgra[y * w * 4..(y + 1) * w * 4];
        for x in 0..w {
            let p = &row[x * 4..x * 4 + 3];
            let cell = gy * gw + x * gw / w;
            sums[cell] += p[0] as u64 + 2 * p[1] as u64 + p[2] as u64;
            counts[cell] += 4;
        }
    }
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    (w, h).hash(&mut hasher);
    for (s, n) in sums.iter().zip(&counts) {
        ((s / (*n).max(1)) >> 3).hash(&mut hasher);
    }
    hasher.finish()
}

impl TextEngine for HostOcr {
    fn read(&self, bgra: &[u8], w: u32, h: u32) -> anyhow::Result<Vec<RawWord>> {
        // A crop that looks as it did a moment ago (the same subtitle, a still menu) has the same
        // words: the phone's text recognizer isn't asked again (it kept the phone hot for nothing).
        let look = (bgra.len() == w as usize * h as usize * 4).then(|| look_of(bgra, w, h));
        if let Some(look) = look {
            if let Some((_, words)) = self.1.lock().unwrap_or_else(std::sync::PoisonError::into_inner).iter().find(|(l, _)| *l == look) {
                return Ok(words.clone());
            }
        }
        let words: Vec<RawWord> = self
            .0
            .recognize(bgra.to_vec(), w, h)
            .into_iter()
            .map(|o| RawWord { line: o.line, text: o.text, x: o.x, y: o.y, w: o.w, h: o.h })
            .collect();
        if let Some(look) = look {
            let mut seen = self.1.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if seen.len() >= SEEN_CROPS {
                seen.pop_front();
            }
            seen.push_back((look, words.clone()));
        }
        Ok(words)
    }
    fn max_dim(&self) -> u32 {
        match self.0.max_dimension() {
            0 => 2600,
            n => n,
        }
    }
}

/// The core's events as the listener's calls.
struct ListenerSink(Arc<dyn EngineListener>);

impl Sink for ListenerSink {
    fn emit(&self, event: &str, payload: serde_json::Value) {
        match event {
            "translation" => {
                let segs = payload["segments"].as_array().cloned().unwrap_or_default();
                let text: String = segs
                    .iter()
                    .map(|s| if s[1].as_str().is_some_and(|c| c.starts_with("icon:")) { "🎮" } else { s[0].as_str().unwrap_or("") })
                    .collect();
                self.0.on_translation(TranslatedLine {
                    area: payload["area"].as_str().unwrap_or("").to_string(),
                    en: payload["en"].as_str().unwrap_or("").to_string(),
                    text: text.trim().to_string(),
                    error: payload["error"].as_str().map(String::from),
                    cached: payload["cached"].as_bool().unwrap_or(false),
                    at_ms: payload["at"].as_u64().unwrap_or(0),
                    json: payload.to_string(),
                });
            }
            "screen" => self.0.on_screen((!payload.is_null()).then(|| payload.to_string())),
            "status-msg" => self.0.on_status(payload.as_str().map(String::from).unwrap_or_else(|| payload.to_string())),
            _ => self.0.on_event(event.to_string(), payload.to_string()),
        }
    }
}

/// What the reading thread is asked to read.
enum Job {
    Frame(Arc<FrameData>),
    Areas { w: u32, h: u32, reads: HashMap<String, AreaRead> },
    Nothing,
}

// ------------------------------------------------------------------------------------- the engine

/// VI Translator's reading and translating, the translation memory and downloads, and the TV side.
#[derive(uniffi::Object)]
pub struct Engine {
    cfg: Arc<Mutex<Config>>,
    live: Arc<Live>,
    memory: Arc<Mutex<Memory>>,
    sink: Arc<dyn Sink>,
    paths: Paths,
    /// to the reading thread: what to read, and where to answer how long to wait
    reader: Mutex<mpsc::Sender<(Job, mpsc::Sender<u64>)>>,
    tv_server: Mutex<Option<TvServer>>,
    sync_started: AtomicBool,
    frame_seq: AtomicU64,
    /// the app's recognizer (snapshots read the whole picture with it)
    recognizer: Arc<dyn TextRecognizer>,
    /// the last frame given to `process_frame` (what the live page's snapshots translate)
    last_frame: Arc<Mutex<Option<Arc<FrameData>>>>,
}

/// A snapshot of `frame` as the desktop's Snapshot button makes it (the game page's private text covered).
fn snapshot_of(cfg: &Mutex<Config>, live: &Live, memory: &Mutex<Memory>, paths: &Paths, recognizer: &Arc<dyn TextRecognizer>, frame: &FrameData) -> anyhow::Result<snapshot::Snapshot> {
    let private = live.screen_private.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
    let c = cfg.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
    let ocr = || Ok(Ocr::with_engine(Box::new(HostOcr(recognizer.clone(), Mutex::new(Default::default())))));
    let id = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0).to_string();
    snapshot::now(frame, &private, &c, memory, paths, &ocr, id)
}

#[uniffi::export]
impl Engine {
    /// `data_dir`: the app's own folder (settings, downloads, connected TVs, the offline model);
    /// `db_dir`: where the translation memory is kept (may be the same folder).
    #[uniffi::constructor]
    pub fn new(data_dir: String, db_dir: String, recognizer: Arc<dyn TextRecognizer>, listener: Arc<dyn EngineListener>) -> Result<Arc<Self>, CoreError> {
        let (data, db) = (PathBuf::from(data_dir), PathBuf::from(db_dir));
        std::fs::create_dir_all(&data).map_err(failed)?;
        std::fs::create_dir_all(&db).map_err(failed)?;
        let paths = Paths { config: data.join("config.json"), data, db };
        let mut c = Config::load(&paths.config);
        // (a phone: its own timing defaults, see config::PHONE_TIMING)
        c.phone = true;
        c.normalize();
        let mut save = std::mem::take(&mut c.defaults_moved);
        if c.install_id.is_empty() {
            // a random id for this install (no account)
            c.install_id = new_id();
            save = true;
        }
        if save {
            let _ = c.save(&paths.config);
        }
        let memory = Arc::new(Mutex::new(Memory::load(paths.db.clone(), &c.target_lang)));
        let cfg = Arc::new(Mutex::new(c));
        let live = Arc::new(Live::default());
        live.tvs.load(paths.data.join("tvs.json"));
        let sink: Arc<dyn Sink> = Arc::new(ListenerSink(listener));
        let ctx = engine::Context { cfg: cfg.clone(), live: live.clone(), memory: memory.clone(), sink: sink.clone(), paths: paths.clone() };
        let (tx, rx) = mpsc::channel::<(Job, mpsc::Sender<u64>)>();
        let reading_recognizer = recognizer.clone();
        // (the reading lives on its own thread: a game's pages script can't move between threads)
        std::thread::Builder::new()
            .name("vit-reader".into())
            .spawn(move || {
                let ocr = Ocr::with_engine(Box::new(HostOcr(reading_recognizer, Mutex::new(Default::default()))));
                let mut reader = engine::start(ctx);
                for (job, answer) in rx {
                    let wait = match job {
                        Job::Frame(f) => reader.step(Input::Frame(&f, &ocr)),
                        Job::Areas { w, h, reads } => reader.step(Input::Areas { w, h, reads }),
                        Job::Nothing => reader.step(Input::Nothing),
                    };
                    let _ = answer.send(wait.as_millis() as u64);
                }
            })
            .map_err(failed)?;
        Ok(Arc::new(Engine { cfg, live, memory, sink, paths, reader: Mutex::new(tx), tv_server: Mutex::new(None), sync_started: AtomicBool::new(false), frame_seq: AtomicU64::new(0), recognizer, last_frame: Arc::new(Mutex::new(None)) }))
    }

    // ---- settings

    /// The settings, as JSON (the desktop's config.json; with its keys and passwords).
    pub fn config_json(&self) -> String {
        serde_json::to_string(&*self.cfg.lock().unwrap_or_else(std::sync::PoisonError::into_inner)).unwrap_or_default()
    }

    /// Change some settings: `patch` is a JSON object of the settings to change (as in
    /// `config_json`), e.g. `{"target_lang": "fr", "engine": "deepl", "deepl_key": "…"}`. Saved.
    pub fn update_config(&self, patch: String) -> Result<(), CoreError> {
        let patch: serde_json::Value = serde_json::from_str(&patch).map_err(failed)?;
        let serde_json::Value::Object(patch) = patch else { return Err(failed("not a JSON object")) };
        let mut c = self.cfg.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut v = serde_json::to_value(&*c).map_err(failed)?;
        if let Some(o) = v.as_object_mut() {
            o.extend(patch);
        }
        let mut new: Config = serde_json::from_value(v).map_err(failed)?;
        // (what only the engine changes stays as it is)
        new.unreadable = c.unreadable;
        new.phone = c.phone;
        new.normalize();
        *c = new;
        c.save(&self.paths.config).map_err(failed)?;
        self.sink.emit("config", serde_json::to_value(c.redacted()).unwrap_or_default());
        Ok(())
    }

    /// The language the game is translated into ("ar", "fr" …: see `languages`).
    pub fn set_target_lang(&self, code: String) -> Result<(), CoreError> {
        if !langs::LANGS.iter().any(|l| l.code == code) {
            return Err(failed(format!("no language {code}")));
        }
        self.update_config(serde_json::json!({ "target_lang": code }).to_string())
    }

    /// The games the core knows (GTA VI first; the server can add more with a layout update).
    pub fn games(&self) -> Vec<Game> {
        let c = self.cfg.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        c.builtins.iter().filter(|p| c.is_builtin(&p.id)).map(|p| Game { id: p.id.clone(), name: p.name.clone() }).collect()
    }

    /// Get ready for this game (one of `games`) and language (one of `languages`): its profile and
    /// language in use, and its translations and layouts kept up to date (`start_sync`).
    pub fn prepare(&self, game: String, language: String) -> Result<(), CoreError> {
        if !self.games().iter().any(|g| g.id == game) {
            return Err(failed(format!("no game {game}")));
        }
        if !langs::LANGS.iter().any(|l| l.code == language) {
            return Err(failed(format!("no language {language}")));
        }
        {
            let mut c = self.cfg.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if c.profile != game {
                c.profile = game;
                c.profile_rev += 1;
            }
        }
        self.update_config(serde_json::json!({ "target_lang": language }).to_string())?;
        self.start_sync();
        Ok(())
    }

    /// The game the profile in use is for ("gta6" …).
    pub fn game(&self) -> String {
        self.cfg.lock().unwrap_or_else(std::sync::PoisonError::into_inner).game()
    }

    /// The areas of the frame the engine reads (the profile in use), as fractions of the frame.
    pub fn read_areas(&self) -> Vec<ReadArea> {
        self.cfg
            .lock()
            .unwrap()
            .areas
            .iter()
            .map(|a| ReadArea { id: a.id.clone(), name: a.name.clone(), enabled: a.enabled, x: a.x, y: a.y, w: a.w, h: a.h, centered_only: a.centered_only })
            .collect()
    }

    // ---- reading

    /// Read a frame of the game picture (RGBA, rows `width * 4` bytes) with the app's recognizer:
    /// every area, and the game's pages. Returns how long to wait before the next frame (ms).
    pub fn process_frame(&self, rgba: Vec<u8>, width: u32, height: u32) -> Result<u64, CoreError> {
        if width == 0 || height == 0 || rgba.len() != width as usize * height as usize * 4 {
            return Err(failed(format!("{} bytes is not a {width}x{height} RGBA picture", rgba.len())));
        }
        let seq = self.frame_seq.fetch_add(1, Ordering::Relaxed) + 1;
        let frame = Arc::new(FrameData { w: width, h: height, rgba, seq });
        *self.last_frame.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(frame.clone());
        self.run(Job::Frame(frame))
    }

    /// Read each area from the crops and words the app's own OCR read (no game pages: they need
    /// the whole frame). An enabled area missing from `areas` reads as empty. How long to wait (ms).
    pub fn process_areas(&self, frame_width: u32, frame_height: u32, areas: Vec<AreaWords>) -> Result<u64, CoreError> {
        let mut reads = HashMap::new();
        for a in areas {
            if a.crop_rgba.len() != a.crop_width as usize * a.crop_height as usize * 4 {
                return Err(failed(format!("area {}: {} bytes is not a {}x{} RGBA picture", a.area_id, a.crop_rgba.len(), a.crop_width, a.crop_height)));
            }
            let words = a.words.into_iter().map(|w| Word { text: w.text, rect: (w.x, w.y, w.w, w.h), line: w.line }).collect();
            reads.insert(a.area_id, AreaRead { crop: Crop { w: a.crop_width, h: a.crop_height, rgba: a.crop_rgba }, words, sure: a.sure });
        }
        self.run(Job::Areas { w: frame_width, h: frame_height, reads })
    }

    /// No picture right now (the stream stopped): what's shown goes once that lasts. How long to wait (ms).
    pub fn no_picture(&self) -> Result<u64, CoreError> {
        self.run(Job::Nothing)
    }

    /// What the overlays show now (the JSON the TV page draws: areas, style, shown lines, the game page).
    pub fn overlay_state_json(&self) -> String {
        vi_translator_core::live::overlay_state(&self.cfg, &self.live).to_string()
    }

    /// The lines shown lately, newest last (at most `max`; the engine keeps 200).
    pub fn recent_lines(&self, max: u32) -> Vec<RecentLine> {
        let log = self.live.live_log.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let skip = log.len().saturating_sub(max as usize);
        log.iter()
            .skip(skip)
            .map(|l| RecentLine {
                id: l["id"].as_u64().unwrap_or(0),
                area: l["area"].as_str().unwrap_or("").to_string(),
                en: l["en"].as_str().unwrap_or("").to_string(),
                text: l["text"].as_str().unwrap_or("").to_string(),
                at_ms: l["at"].as_u64().unwrap_or(0),
            })
            .collect()
    }

    /// Forget everything the translation memory learned (the downloaded reviewed translations stay).
    pub fn memory_clear(&self) {
        self.memory.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clear();
    }

    /// The game pages a game's script has, and the switches it declares (Game pages):
    /// {"pages": [{"id", "option"}], "options": [{"id", "name", "about", "on"}]}.
    pub fn page_options(&self, game: String) -> String {
        vi_translator_core::pagescript::page_options(&game, &self.paths.data).to_string()
    }

    /// The game page on show now, for a view opened after it appeared: {"layout", "reason"}.
    pub fn screen_json(&self) -> String {
        serde_json::json!({ "layout": self.live.screen.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone(), "reason": self.live.screen_reason.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone() }).to_string()
    }

    /// Translate the whole picture now, as the desktop's Snapshot button does (RGBA, rows `width * 4`
    /// bytes): the snapshot service in the settings (`snap_service`, else `engine`), the reviewed
    /// translations first, a game page's private text covered. The snapshot as JSON: {"id", "w", "h",
    /// "image" (JPEG data URL), "translated_image" (a service that translates the picture itself),
    /// "blocks" [{"rect", "en", "segs", "lines", "align", "fg", "bg", …}], "service", "lang", "rtl", "ms"}.
    pub fn take_snapshot(&self, rgba: Vec<u8>, width: u32, height: u32) -> Result<String, CoreError> {
        if width == 0 || height == 0 || rgba.len() != width as usize * height as usize * 4 {
            return Err(failed(format!("{} bytes is not a {width}x{height} RGBA picture", rgba.len())));
        }
        let frame = FrameData { w: width, h: height, rgba, seq: 0 };
        let snap = snapshot_of(&self.cfg, &self.live, &self.memory, &self.paths, &self.recognizer, &frame).map_err(|e| failed(format!("{e:#}")))?;
        serde_json::to_string(&snap).map_err(failed)
    }

    /// The live page on the home network (served by the TV page server while the settings'
    /// `lan_page` is on): {"enabled", "running", "url", "qr" (SVG), "error"}.
    pub fn lan_info(&self) -> String {
        let c = self.cfg.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        serde_json::to_string(&vi_translator_core::lan_page::info(&c, &self.live)).unwrap_or_default()
    }

    /// The TV page for this phone's own preview, exactly as the TVs draw it:
    /// http://127.0.0.1:<port>/tv?t=<key> (a key for this run only, not a connected TV). None while
    /// the TV page server isn't running.
    pub fn local_viewer_url(&self) -> Option<String> {
        let port = self.tv_server.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_ref().map(|s| s.port)?;
        Some(format!("http://127.0.0.1:{port}/tv?t={}", self.live.tvs.local_key()))
    }

    /// The fonts bundled with the pages (what an area's `font_family` can be).
    pub fn font_list(&self) -> Vec<String> {
        vi_translator_core::tv_server::bundled_fonts()
    }

    /// The translation memory: {"entries", "reviewed", "hits", "misses", …}.
    pub fn memory_stats_json(&self) -> String {
        serde_json::to_string(&self.memory.lock().unwrap_or_else(std::sync::PoisonError::into_inner).stats()).unwrap_or_default()
    }

    /// How the engine keeps up over the last minute: {"read", "translate", "ready"} ms (or null).
    pub fn perf_json(&self) -> String {
        vi_translator_core::perf::perf_status().to_string()
    }

    // ---- downloads

    /// Keep the reviewed translations and layouts for the game and language in use up to date
    /// (from https://vitranslator.app, in the background; what was downloaded before is used at once).
    pub fn start_sync(&self) {
        if self.sync_started.swap(true, Ordering::AcqRel) {
            return;
        }
        let (k, c, m, p) = (self.sink.clone(), self.cfg.clone(), self.memory.clone(), self.paths.clone());
        std::thread::spawn(move || sync::run(k, c, m, p));
    }

    // ---- TVs

    /// Serve the TV page on the home network at `port` (the desktop's is 8791), so connected TVs
    /// draw the overlays. The port it listens on.
    pub fn start_tv_server(&self, port: u16) -> Result<u16, CoreError> {
        let mut slot = self.tv_server.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(s) = slot.as_ref().filter(|s| s.port == port || port == 0) {
            return Ok(s.port);
        }
        *slot = None;
        // (with the live page, served while the settings' `lan_page` is on; its snapshots of the last frame)
        let snap = {
            let (c, l, m, p, r, f) = (self.cfg.clone(), self.live.clone(), self.memory.clone(), self.paths.clone(), self.recognizer.clone(), self.last_frame.clone());
            move || {
                let Some(frame) = f.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone() else { anyhow::bail!("no picture yet") };
                snapshot_of(&c, &l, &m, &p, &r, &frame)
            }
        };
        let page = Arc::new(LivePage { cfg: self.cfg.clone(), live: self.live.clone(), paths: self.paths.clone(), sink: self.sink.clone(), snapshot: Box::new(snap), memory: Some(self.memory.clone()) });
        let s = TvServer::start(port, self.cfg.clone(), self.live.clone(), Some(page)).map_err(failed)?;
        let port = s.port;
        *slot = Some(s);
        Ok(port)
    }

    pub fn stop_tv_server(&self) {
        *self.tv_server.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }

    /// This phone's address on the home network (what TVs reach it at), if it has one.
    pub fn home_address(&self) -> Option<String> {
        vi_translator_core::net::lan_ip()
    }

    /// The TVs on the home network waiting to be connected (a couple of seconds).
    pub fn find_tvs(&self) -> Vec<FoundTv> {
        tv::find(&self.live.tvs).into_iter().map(|t| FoundTv { address: t.address, id: t.id, name: t.name, connected: t.connected }).collect()
    }

    /// Connect the TV at `address` (from `find_tvs`, or an IP address alone: the TV app's port is
    /// added) with the code it shows; `device_name` is what
    /// the TV shows as connected to ("Pixel 9"). Its name. Also told as the "tv-paired" event.
    pub fn pair_tv(&self, address: String, code: String, device_name: String) -> Result<String, PairError> {
        let r = self.pair(&address, &code, &device_name);
        let payload = match &r {
            Ok(name) => serde_json::json!({ "name": name }),
            Err(e) => serde_json::json!({ "error": e.code() }),
        };
        self.sink.emit("tv-paired", payload);
        r
    }

    /// The connected TVs.
    pub fn list_tvs(&self) -> Vec<TvInfo> {
        self.live.tvs.list().into_iter().map(|t| TvInfo { id: t.id, name: t.name, online: t.online, on: t.on }).collect()
    }

    /// Switch a connected TV's translations on or off at once, while translating (kept with the
    /// TV). False when there's no such TV.
    pub fn set_tv_on(&self, id: String, on: bool) -> bool {
        self.live.tvs.set_on(&id, on)
    }

    /// Disconnect a TV: its key stops working, and the TV shows a new code.
    pub fn remove_tv(&self, id: String) -> bool {
        self.live.tvs.remove_id(&id)
    }
}

impl Engine {
    fn run(&self, job: Job) -> Result<u64, CoreError> {
        let (tx, rx) = mpsc::channel();
        self.reader.lock().unwrap_or_else(std::sync::PoisonError::into_inner).send((job, tx)).map_err(|_| failed("the reading stopped"))?;
        rx.recv_timeout(Duration::from_secs(120)).map_err(|_| failed("the reading stopped"))
    }

    fn pair(&self, address: &str, code: &str, device_name: &str) -> Result<String, PairError> {
        let port = self.tv_server.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_ref().map(|s| s.port);
        let name = if device_name.trim().is_empty() { "Phone".to_string() } else { device_name.trim().chars().take(60).collect() };
        // (an address typed alone: the TV app's port)
        let address = match address.trim().parse::<std::net::Ipv4Addr>() {
            Ok(ip) => format!("{ip}:{}", tv::TV_PORT),
            Err(_) => address.trim().to_string(),
        };
        // (this phone's address on the TV's network: not mobile data's or a VPN's)
        let (Some(port), Some(ip)) = (port, vi_translator_core::net::lan_ip_toward(&address)) else { return Err(PairError::NotSharing) };
        tv::pair(&self.live.tvs, &address, code, &format!("{ip}:{port}"), &name).map_err(PairError::from)
    }
}

fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// The languages the game can be translated into.
#[uniffi::export]
pub fn languages() -> Vec<Language> {
    langs::LANGS.iter().map(|l| Language { code: l.code.into(), name: l.name.into(), native: l.native.into(), rtl: l.rtl }).collect()
}

/// This core's version (the desktop app's version it matches).
#[uniffi::export]
pub fn core_version() -> String {
    vi_translator_core::VERSION.to_string()
}

#[cfg(test)]
mod look_tests {
    use super::look_of;

    fn picture(w: u32, h: u32, text_at: Option<(u32, u32)>, noise: u8) -> Vec<u8> {
        let mut px = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                let n = ((x * 7 + y * 13) % 3) as u8 * noise; // a little stream noise
                let lit = text_at.is_some_and(|(x0, x1)| x >= x0 && x < x1 && y % 6 < 3 && x % 5 < 2);
                let v = if lit { 240 } else { 20 + n };
                px[i..i + 4].copy_from_slice(&[v, v, v, 255]);
            }
        }
        px
    }

    #[test]
    fn a_crop_looks_the_same_through_noise_but_not_with_other_text() {
        let (w, h) = (640, 48);
        let a = look_of(&picture(w, h, Some((100, 400)), 0), w, h);
        assert_eq!(a, look_of(&picture(w, h, Some((100, 400)), 1), w, h), "noise of a step or two");
        assert_ne!(a, look_of(&picture(w, h, Some((100, 300)), 0), w, h), "a shorter line");
        assert_ne!(a, look_of(&picture(w, h, Some((150, 450)), 0), w, h), "a line elsewhere");
        assert_ne!(a, look_of(&picture(w, h, None, 0), w, h), "no line");
    }
}
