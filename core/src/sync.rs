//! Background sync with the VI Translator server: downloads translations and layout updates (read
//! areas, fonts, menu words) in the background. Nothing is sent: the app only downloads.
//!
//! Nothing runs while no server is set.

use std::collections::HashMap;
use std::io::Read;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::Sink;
use crate::config::Config;
use crate::memory::{self, Memory};

/// The project server (a different one can be set in the settings file: `server_url`).
pub const DEFAULT_SERVER: &str = "https://vitranslator.app";
// (an unchanged download is a short "304 Not Modified": checking often costs next to nothing)
const DOWNLOAD_EVERY: Duration = Duration::from_secs(60 * 60);
#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
struct State {
    /// ETag of each download
    versions: HashMap<String, String>,
}

#[derive(Clone)]
pub struct Paths {
    /// settings folder (AppData): downloads and sync state
    pub data: PathBuf,
    /// translation-db folder: the lines log
    pub db: PathBuf,
    /// the settings file (saved again when a layout update changes the built-in profiles)
    pub config: PathBuf,
}

impl Paths {
    fn state(&self) -> PathBuf {
        self.data.join("sync-state.json")
    }
    fn shared(&self, game: &str, lang: &str) -> PathBuf {
        self.data.join(format!("shared-{game}-{lang}.bin"))
    }
    fn layout(&self, game: &str) -> PathBuf {
        self.data.join(format!("layout-{game}.json"))
    }
}

fn load_state(p: &Paths) -> State {
    std::fs::read_to_string(p.state()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn save_state(p: &Paths, s: &State) {
    let _ = crate::config::write_atomic(&p.state(), serde_json::to_string_pretty(s).unwrap_or_default().as_bytes());
}

fn server(c: &Config) -> Option<String> {
    let s = if c.server_url.trim().is_empty() { DEFAULT_SERVER } else { c.server_url.trim() };
    (!s.is_empty()).then(|| s.trim_end_matches('/').to_string())
}

fn game(c: &Config) -> Option<String> {
    Some(c.game())
}

/// Use what was downloaded before (works offline), then keep it up to date.
pub fn run(sink: Arc<dyn Sink>, cfg: Arc<Mutex<Config>>, memory: Arc<Mutex<Memory>>, paths: Paths) {

    let http = crate::net::http_client()
        .user_agent(concat!("vi-translator/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(30))
        .build()
        .expect("http client");
    // The app no longer shares the lines it reads: the log those were kept in (lines seen in
    // games) goes from this PC.
    for old in ["lines-log.dat", "lines-log.old.dat", "lines-log.jsonl"] {
        let _ = std::fs::remove_file(paths.db.join(old));
    }
    let mut first = true;
    let mut next_download = Instant::now();
    let mut last = (String::new(), String::new()); // game, language
    loop {
        let c = cfg.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        // what was downloaded before works offline; another game or language: its own downloads
        if let Some(g) = game(&c) {
            if last != (g.clone(), c.target_lang.clone()) {
                // (give the app a moment to switch its memory to the new language first)
                std::thread::sleep(Duration::from_secs(2));
                // (the game or language as they are now: they may have changed again meanwhile)
                let now = cfg.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
                last = (now.game(), now.target_lang.clone());
                load_cached(&*sink, &cfg, &memory, &paths, &last.0, &last.1);
                next_download = Instant::now();
            }
        }
        if first {
            first = false;
            std::thread::sleep(Duration::from_secs(20)); // let the app start first
        }
        // (the settings as they are after the waits)
        let c = cfg.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        if let (Some(url), Some(g)) = (server(&c), game(&c)) {
            if Instant::now() >= next_download {
                next_download = Instant::now() + DOWNLOAD_EVERY;
                download(&http, &url, &*sink, &cfg, &memory, &paths, &g, &c.target_lang);
            }
        }
        std::thread::sleep(Duration::from_secs(30));
    }
}

// ------------------------------------------------------------------------------------- downloads

fn download(
    http: &reqwest::blocking::Client,
    url: &str,
    sink: &dyn Sink,
    cfg: &Arc<Mutex<Config>>,
    memory: &Arc<Mutex<Memory>>,
    paths: &Paths,
    game: &str,
    lang: &str,
) {
    let mut st = load_state(paths);
    let _ = std::fs::create_dir_all(&paths.data);
    // translations: only what changed since the version this app has
    if let Some(entries) = download_pack(http, url, paths, &mut st, game, lang) {
        memory.lock().unwrap_or_else(std::sync::PoisonError::into_inner).set_shared(lang, entries);
    }
    // the games the server adds (published like a layout, under the name "games")
    let k = "layout/games".to_string();
    // (a "not modified" answer only helps when the kept copy is still there)
    let have = st.versions.get(&k).filter(|_| paths.layout("games").exists());
    if let Some((bytes, tag)) = fetch(http, &format!("{url}/v1/{k}"), have) {
        if let Ok(list) = serde_json::from_slice::<serde_json::Value>(&bytes) {
            if crate::config::write_atomic(&paths.layout("games"), &bytes).is_ok() {
                st.versions.insert(k, tag.clone());
            }
            apply_games(sink, cfg, paths, &st, &list, tag.parse().unwrap_or(0), lang);
        }
    }
    // layout
    let k = format!("layout/{game}");
    let have = st.versions.get(&k).filter(|_| paths.layout(game).exists());
    if let Some((bytes, tag)) = fetch(http, &format!("{url}/v1/{k}"), have) {
        if let Ok(layout) = serde_json::from_slice::<serde_json::Value>(&bytes) {
            if crate::config::write_atomic(&paths.layout(game), &bytes).is_ok() {
                st.versions.insert(k, tag.clone());
            }
            let id = cfg.lock().unwrap_or_else(std::sync::PoisonError::into_inner).install_id.clone();
            if crate::pagescript::save_from_layout(&paths.data, game, &layout, &id) {
                sink.emit("pages", serde_json::Value::Null);
            }
            apply_layout(sink, cfg, &paths.config, &layout, tag.parse().unwrap_or(0), lang, game);
        }
    }
    save_state(paths, &st);
}

/// The translations for a game and language, when there is a newer version than this app's: the
/// server's version first (tiny), then only what changed since this app's version, merged into the
/// pack it keeps; the whole pack when it has none, the series started again, or it's too far behind.
/// The merged pack is kept sealed like the server's, with the new version. None: nothing new.
fn download_pack(http: &reqwest::blocking::Client, url: &str, paths: &Paths, st: &mut State, game: &str, lang: &str) -> Option<Vec<memory::Entry>> {
    download_pack_with(&key(), http, url, paths, st, game, lang)
}

fn download_pack_with(key: &[u8; 32], http: &reqwest::blocking::Client, url: &str, paths: &Paths, st: &mut State, game: &str, lang: &str) -> Option<Vec<memory::Entry>> {
    let k = format!("pack/{game}/{lang}");
    let info: serde_json::Value = get(http, &format!("{url}/v1/data/{game}/{lang}/version")).and_then(|b| serde_json::from_slice(&b).ok())?;
    let (series, version) = (info["series"].as_str()?.to_string(), info["version"].as_u64()? as u32);
    let have = st.versions.get(&k).and_then(|v| v.split_once('/')).map(|(s, v)| (s.to_string(), v.parse::<u32>().unwrap_or(0)));
    // (the same version, and the kept pack still opens: nothing to do; lost or damaged: downloaded again)
    let kept_ok = || std::fs::read(paths.shared(game, lang)).ok().and_then(|b| open_pack_with(key, &b, game, lang, &version.to_string())).is_some();
    if have.as_ref() == Some(&(series.clone(), version)) && kept_ok() {
        return None;
    }
    let base = format!("{url}/v1/data/{game}/{lang}/{series}");
    let changed = have.filter(|(s, v)| *s == series && *v > 0 && *v < version).and_then(|(_, from)| {
        let mine = std::fs::read(paths.shared(game, lang)).ok().and_then(|b| open_pack_with(key, &b, game, lang, &from.to_string()))?;
        let file = get_pack(http, &format!("{base}/changes-{from}-{version}.bin"))?;
        let changes = open_changes(key, &file, &format!("{game}/{lang}/{from}-{version}"))?;
        Some(apply_changes(mine, changes))
    });
    let entries = match changed {
        Some(e) => e,
        None => {
            let file = get_pack(http, &format!("{base}/full-{version}.bin"))?;
            open_pack_with(key, &file, game, lang, &version.to_string())?
        }
    };
    // (the version is recorded only once the pack is kept: else the next check downloads it again)
    if crate::config::write_atomic(&paths.shared(game, lang), &seal_pack(key, &entries, game, lang, version)).is_ok() {
        st.versions.insert(k, format!("{series}/{version}"));
        st.versions.remove(&format!("data/{game}/{lang}"));
    }
    Some(entries)
}

/// A plain GET: the body when the answer is 200.
fn get(http: &reqwest::blocking::Client, url: &str) -> Option<Vec<u8>> {
    let r = http.get(url).send().ok()?;
    r.status().is_success().then(|| r.bytes().ok().map(|b| b.to_vec())).flatten()
}

/// How long a pack file may take to download: a whole game's translations are several megabytes, more
/// than the client's 30 seconds allow on a slow connection (the download would fail every time).
const PACK_TIMEOUT: Duration = Duration::from_secs(600);

/// A GET for a pack file (whole, or its changes): as `get`, with the time a big file needs.
fn get_pack(http: &reqwest::blocking::Client, url: &str) -> Option<Vec<u8>> {
    let r = http.get(url).timeout(PACK_TIMEOUT).send().ok()?;
    r.status().is_success().then(|| r.bytes().ok().map(|b| b.to_vec())).flatten()
}

/// A pack's changes: each line as it is now, or (key, None) for a line taken out.
fn open_changes(key: &[u8; 32], file: &[u8], aad: &str) -> Option<Vec<(String, Option<memory::Entry>)>> {
    let text = open_sealed(key, file, aad)?;
    Some(
        text.lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter_map(|v| {
                let k = memory::key_of(v["en"].as_str()?);
                if k.is_empty() {
                    return None;
                }
                if v["deleted"].as_bool() == Some(true) {
                    return Some((k, None));
                }
                let ar = v["ar"].as_str().filter(|a| !a.trim().is_empty())?.to_string();
                Some((k.clone(), Some(memory::Entry { en: k, ar, source: "reviewed".into() })))
            })
            .collect(),
    )
}

/// The pack with the changes in (same English: replaced; taken out: gone).
fn apply_changes(pack: Vec<memory::Entry>, changes: Vec<(String, Option<memory::Entry>)>) -> Vec<memory::Entry> {
    let mut by_key: std::collections::BTreeMap<String, memory::Entry> = pack.into_iter().map(|e| (e.en.clone(), e)).collect();
    for (k, e) in changes {
        match e {
            Some(e) => {
                by_key.insert(k, e);
            }
            None => {
                by_key.remove(&k);
            }
        }
    }
    by_key.into_values().collect()
}

/// Seal a pack like the server does ("G6T1" | nonce | AES-256-GCM(gzip(JSON lines)), associated data
/// "game/lang/version"), to keep it on disk.
fn seal_pack(key: &[u8; 32], entries: &[memory::Entry], game: &str, lang: &str, version: u32) -> Vec<u8> {
    use aes_gcm::aead::{Aead, AeadCore, KeyInit, OsRng, Payload};
    use std::io::Write as _;
    let mut text = String::new();
    for e in entries {
        text.push_str(&serde_json::to_string(e).unwrap_or_default());
        text.push('\n');
    }
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let _ = gz.write_all(text.as_bytes());
    let zipped = gz.finish().unwrap_or_default();
    let cipher = aes_gcm::Aes256Gcm::new_from_slice(key).expect("32-byte key");
    let nonce = aes_gcm::Aes256Gcm::generate_nonce(&mut OsRng);
    let aad = format!("{game}/{lang}/{version}");
    let ct = cipher.encrypt(&nonce, Payload { msg: &zipped, aad: aad.as_bytes() }).expect("encrypts");
    let mut out = b"G6T1".to_vec();
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    out
}

/// GET with If-None-Match: Some((body, version)) when there is something new.
fn fetch(http: &reqwest::blocking::Client, url: &str, have: Option<&String>) -> Option<(Vec<u8>, String)> {
    let mut req = http.get(url);
    if let Some(v) = have {
        req = req.header("If-None-Match", format!("\"{v}\""));
    }
    let r = req.send().ok()?;
    if !r.status().is_success() {
        return None; // 304 (unchanged), 404 (nothing yet), errors
    }
    let tag = r.headers().get("ETag")?.to_str().ok()?.trim_start_matches("W/").trim_matches('"').to_string();
    Some((r.bytes().ok()?.to_vec(), tag))
}

/// The pack kept from the last download ("series/version"; before change downloads: the version).
fn kept_pack(paths: &Paths, st: &State, game: &str, lang: &str) -> Option<Vec<memory::Entry>> {
    let version = st.versions.get(&format!("pack/{game}/{lang}")).and_then(|v| v.split_once('/')).map(|(_, v)| v.to_string())
        .or_else(|| st.versions.get(&format!("data/{game}/{lang}")).cloned())?;
    open_pack(&std::fs::read(paths.shared(game, lang)).ok()?, game, lang, &version)
}

/// The pack kept from the last download, outside the running app (`--test-image`).
pub fn kept_pack_in(data: &std::path::Path, game: &str, lang: &str) -> Option<Vec<memory::Entry>> {
    let paths = Paths { data: data.to_path_buf(), db: data.to_path_buf(), config: data.join("config.json") };
    kept_pack(&paths, &load_state(&paths), game, lang)
}

fn load_cached(sink: &dyn Sink, cfg: &Arc<Mutex<Config>>, memory: &Arc<Mutex<Memory>>, paths: &Paths, game: &str, lang: &str) {
    let st = load_state(paths);
    if let (Ok(bytes), Some(tag)) = (std::fs::read(paths.layout("games")), st.versions.get("layout/games")) {
        if let Ok(list) = serde_json::from_slice::<serde_json::Value>(&bytes) {
            apply_games(sink, cfg, paths, &st, &list, tag.parse().unwrap_or(0), lang);
        }
    }
    if let Some(entries) = kept_pack(paths, &st, game, lang) {
        memory.lock().unwrap_or_else(std::sync::PoisonError::into_inner).set_shared(lang, entries);
    }
    // (another game: the last one's extra words, phrases and OCR fixes no longer apply)
    crate::screens::set_layout_words(game, lang, Vec::new(), Vec::new(), Vec::new());
    crate::phrasebook::set_extra(game, lang, Vec::new());
    crate::ocr::set_fixes(Default::default());
    if let (Ok(bytes), Some(tag)) = (std::fs::read(paths.layout(game)), st.versions.get(&format!("layout/{game}"))) {
        if let Ok(layout) = serde_json::from_slice::<serde_json::Value>(&bytes) {
            // (test versions turned on or off: the script that's for this install now)
            let id = cfg.lock().unwrap_or_else(std::sync::PoisonError::into_inner).install_id.clone();
            if crate::pagescript::save_from_layout(&paths.data, game, &layout, &id) {
                sink.emit("pages", serde_json::Value::Null);
            }
            apply_layout(sink, cfg, &paths.config, &layout, tag.parse().unwrap_or(0), lang, game);
        }
    }
}

/// A new list of games from the server: the games it changed get their saved layout again.
fn apply_games(sink: &dyn Sink, cfg: &Arc<Mutex<Config>>, paths: &Paths, st: &State, list: &serde_json::Value, version: u32, lang: &str) {
    let changed = {
        let mut c = cfg.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let changed = c.apply_games(list, version);
        if changed.is_empty() {
            return;
        }
        let _ = c.save(&paths.config);
        sink.emit("config", serde_json::to_value(c.redacted()).unwrap_or_default());
        changed
    };
    for g in changed {
        if let (Ok(bytes), Some(tag)) = (std::fs::read(paths.layout(&g)), st.versions.get(&format!("layout/{g}"))) {
            if let Ok(layout) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                apply_layout(sink, cfg, &paths.config, &layout, tag.parse().unwrap_or(0), lang, &g);
            }
        }
    }
}

pub(crate) fn key() -> [u8; 32] {
    // (debug builds can test against a local server's key)
    if cfg!(debug_assertions) {
        if let Some(k) = std::env::var("VI_TRANSLATOR_DATA_KEY").ok().and_then(|s| b64(&s)).filter(|k| k.len() == 32) {
            return k.try_into().unwrap();
        }
    }
    const A: [u8; 32] = [
        0xa6, 0x23, 0x37, 0x91, 0x51, 0x53, 0x7d, 0x05, 0xdc, 0xc4, 0x89, 0x9c, 0x51, 0x02, 0xd6, 0x5e, 0xa3, 0xd1, 0x81,
        0xfb, 0xfb, 0x5d, 0xa5, 0xf7, 0x3f, 0xa3, 0x04, 0x6e, 0x9d, 0x14, 0x15, 0x36,
    ];
    const B: [u8; 32] = [
        0x2a, 0x6c, 0x1a, 0x44, 0xa3, 0x4c, 0xc9, 0x81, 0x8c, 0xda, 0x59, 0x9d, 0x86, 0x63, 0x4c, 0xfa, 0x40, 0x3b, 0x53,
        0xd3, 0x98, 0x6c, 0x93, 0x62, 0x4a, 0xf1, 0xb8, 0x7c, 0xc8, 0x41, 0x7c, 0x18,
    ];
    std::array::from_fn(|i| A[i] ^ B[i])
}

fn b64(s: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.decode(s.trim()).ok()
}

/// "G6T1" | nonce (12) | AES-256-GCM(gzip(JSON lines)), associated data "game/lang/version".
pub fn open_pack(file: &[u8], game: &str, lang: &str, version: &str) -> Option<Vec<memory::Entry>> {
    open_pack_with(&key(), file, game, lang, version)
}

/// The JSON lines inside a sealed pack file (None if it isn't one, or was sealed for something else).
fn open_sealed(key: &[u8; 32], file: &[u8], aad: &str) -> Option<String> {
    use aes_gcm::aead::{Aead, KeyInit, Payload};
    if file.len() < 4 + 12 + 16 || &file[..4] != b"G6T1" {
        return None;
    }
    let cipher = aes_gcm::Aes256Gcm::new_from_slice(key).ok()?;
    let plain = cipher
        .decrypt(aes_gcm::Nonce::from_slice(&file[4..16]), Payload { msg: &file[16..], aad: aad.as_bytes() })
        .ok()?;
    let mut text = String::new();
    flate2::read::GzDecoder::new(&plain[..]).read_to_string(&mut text).ok()?;
    Some(text)
}

fn open_pack_with(key: &[u8; 32], file: &[u8], game: &str, lang: &str, version: &str) -> Option<Vec<memory::Entry>> {
    let text = open_sealed(key, file, &format!("{game}/{lang}/{version}"))?;
    Some(
        text.lines()
            .filter_map(|l| serde_json::from_str::<memory::Entry>(l).ok())
            .map(|mut e| {
                e.en = memory::key_of(&e.en);
                e.source = "reviewed".into();
                e
            })
            .filter(|e| !e.en.is_empty() && !e.ar.trim().is_empty())
            .collect(),
    )
}

// ------------------------------------------------------------------------------------- layouts

/// `base` with the fields `patch` names replaced (a value that doesn't fit leaves it all as it was).
fn merged<T: serde::Serialize + serde::de::DeserializeOwned>(base: &T, patch: &serde_json::Value) -> Option<T> {
    let mut v = serde_json::to_value(base).ok()?;
    let (o, p) = (v.as_object_mut()?, patch.as_object()?);
    for (k, x) in p {
        if o.contains_key(k) {
            o.insert(k.clone(), x.clone());
        }
    }
    serde_json::from_value(v).ok()
}

/// Apply a game layout:
/// ```json
/// { "areas": [ { "id": "subtitles", "x": 0.1, "y": 0.8, "w": 0.8, "h": 0.15, "centered_only": true } ],
///   "fonts": { "title_font": "Lalezar", "text_font": "Tajawal" },
///   "words": { "ar": { "menu": { "Hide Menu": "…" }, "buttons": { "Browse": "…" }, "tabs": { "MAP": "…" } } } }
/// ```
/// Words always apply (they only add to or fix the built-in lists). Areas and fonts are applied
/// once per new layout version: the player's own changes stay until the next new layout.
fn apply_layout(sink: &dyn Sink, cfg: &Arc<Mutex<Config>>, config_path: &PathBuf, layout: &serde_json::Value, version: u32, lang: &str, game: &str) {
    // (the built-in word lists are Arabic; a layout can carry words for any language)
    let words = |kind: &str| -> Vec<(String, String)> {
        layout["words"][lang][kind]
            .as_object()
            .map(|m| m.iter().filter_map(|(en, ar)| Some((en.clone(), ar.as_str()?.to_string()))).collect())
            .unwrap_or_default()
    };
    let mut c = cfg.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    // (the game and language in use: a download that began before the player switched doesn't count)
    if game == c.game() && lang == c.target_lang {
        crate::screens::set_layout_words(game, lang, words("menu"), words("buttons"), words("tabs"));
        // hand-written translations of the game's lines, and OCR fixes for its fonts
        let phrases = layout["phrases"][lang].as_object().map(|m| m.iter().filter_map(|(en, t)| Some((en.clone(), t.as_str()?.to_string()))).collect()).unwrap_or_default();
        crate::phrasebook::set_extra(game, lang, phrases);
        crate::ocr::set_fixes(crate::ocr::fixes_from(&layout["ocr"]));
    }
    if version <= c.layout_versions.get(game).copied().unwrap_or(0) {
        return;
    }
    // The update tunes the game's built-in profile (the player's own copies are theirs: untouched).
    let Some(p) = c.builtins.iter_mut().find(|p| p.id == game) else { return };
    if let Some(areas) = layout["areas"].as_array() {
        for a in areas {
            let Some(id) = a["id"].as_str().filter(|id| !id.is_empty()) else { continue };
            // a place outside the picture is a mistake: that part is left as it was
            let mut a = a.clone();
            if ["x", "y", "w", "h"].iter().any(|k| a[*k].as_f64().is_some_and(|v| !(0.0..=1.0).contains(&v))) {
                for k in ["x", "y", "w", "h"] {
                    a.as_object_mut().map(|o| o.remove(k));
                }
            }
            // the area's own text style: only the parts the layout names, on top of what it has now
            // (its own style, or the profile's it started from)
            let style_patch = a.as_object_mut().and_then(|o| o.remove("style")).filter(|s| s.is_object());
            match p.areas.iter_mut().find(|x| x.id == id) {
                // any of the area's settings (place, alignment, font, background, size …)
                Some(area) => {
                    if let Some(new) = merged(area, &a) {
                        *area = new;
                    }
                    if let Some(patch) = &style_patch {
                        let base = area.style.clone().unwrap_or_else(|| p.style.clone());
                        if let Some(mut st) = merged(&base, patch) {
                            st.sanitize();
                            area.style = Some(st);
                        }
                    }
                }
                // a new area (with its place)
                None if ["x", "y", "w", "h"].iter().all(|k| a[*k].is_number()) => {
                    if let Some(mut new) = merged(&crate::config::Area::default(), &a) {
                        if let Some(patch) = &style_patch {
                            if let Some(mut st) = merged(&p.style, patch) {
                                st.sanitize();
                                new.style = Some(st);
                            }
                        }
                        p.areas.push(new);
                    }
                }
                None => {}
            }
        }
    }
    // the whole text style (colours, outline, sizes …), only the parts it names
    if layout["style"].is_object() {
        if let Some(new) = merged(&p.style, &layout["style"]) {
            p.style = new;
        }
    }
    // reading timing for the game
    if layout["timing"].is_object() {
        if let Some(new) = merged(&p.timing, &layout["timing"]) {
            p.timing = new;
        }
    }
    // the game's defaults for its areas' effects (`{"fx": {"subtitles": {"fade_ms": 0}}}`): what applies
    // where the player chose nothing; an area left out keeps what it had
    if let Some(fx) = layout["fx"].as_object() {
        for (id, d) in fx {
            if let Ok(d) = serde_json::from_value::<crate::config::FxDefaults>(d.clone()) {
                if !id.is_empty() {
                    p.fx.insert(id.clone(), d);
                }
            }
        }
    }
    if let Some(f) = layout["fonts"]["title_font"].as_str() {
        p.pages.title_font = f.to_string();
    }
    if let Some(f) = layout["fonts"]["text_font"].as_str() {
        p.pages.text_font = f.to_string();
    }
    // (from the server, not signed: colours, fonts, ids, names and numbers kept to what the app uses)
    p.sanitize();
    c.layout_versions.insert(game.to_string(), version);
    c.normalize(); // (if that built-in profile is in use, its new layout applies now)
    let _ = c.save(config_path);
    sink.emit("config", serde_json::to_value(c.redacted()).unwrap_or_default());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_layout_update_sets_the_games_fade_default() {
        let dir = std::env::temp_dir().join(format!("vit-fx-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let mut c = Config::default();
        c.normalize();
        let cfg = Arc::new(Mutex::new(c));
        let layout = serde_json::json!({"fx": {"subtitles": {"fade_ms": 120}, "help": {"fade_ms": 99999}, "": {"fade_ms": 5}}});
        apply_layout(&crate::NoSink, &cfg, &dir.join("config.json"), &layout, 1, "ar", "gta6");
        let c = cfg.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(c.fx("subtitles").fade_ms, Some(120));
        assert_eq!(c.fx("help").fade_ms, Some(2000), "kept to what the app allows");
        assert_eq!(c.fx("instructions").fade_ms, Some(0));
        assert!(!c.fx_defaults.contains_key(""));
        drop(c);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Against a running backend in dev mode (its test pack key): the first download is the whole pack,
    /// later ones only the changes. `VIT_TEST_SERVER=http://localhost:8080 cargo test -- --ignored`
    #[test]
    #[ignore]
    fn downloads_from_a_dev_server() {
        let Ok(url) = std::env::var("VIT_TEST_SERVER") else { return };
        let dev_key: [u8; 32] = core::array::from_fn(|i| i as u8);
        let dir = std::env::temp_dir().join(format!("vit-pack-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let paths = Paths { data: dir.clone(), db: dir.clone(), config: dir.join("config.json") };
        let http = reqwest::blocking::Client::new();
        let mut st = State::default();
        let game = std::env::var("VIT_TEST_GAME").unwrap_or_else(|_| "gta6".into());
        let first = download_pack_with(&dev_key, &http, &url, &paths, &mut st, &game, "ar").expect("the whole pack");
        println!("whole pack: {} lines, now {:?}", first.len(), st.versions.get(&format!("pack/{game}/ar")));
        assert!(download_pack_with(&dev_key, &http, &url, &paths, &mut st, &game, "ar").is_none(), "nothing new");
        // pretend to have the version before: only the changes come, merged into the kept pack
        let (series, v) = st.versions[&format!("pack/{game}/ar")].split_once('/').map(|(s, v)| (s.to_string(), v.parse::<u32>().unwrap())).unwrap();
        if v > 1 {
            let older: Vec<memory::Entry> = first.iter().filter(|e| e.en != "get in the car lucia").cloned().collect();
            std::fs::write(paths.shared(&game, "ar"), seal_pack(&dev_key, &older, &game, "ar", v - 1)).unwrap();
            st.versions.insert(format!("pack/{game}/ar"), format!("{series}/{}", v - 1));
            let again = download_pack_with(&dev_key, &http, &url, &paths, &mut st, &game, "ar").expect("the changes");
            assert_eq!(again.len(), first.len());
            assert_eq!(again.iter().find(|e| e.en == "get in the car lucia").map(|e| e.ar.clone()), first.iter().find(|e| e.en == "get in the car lucia").map(|e| e.ar.clone()));
            println!("changes {}-{v}: merged, {} lines", v - 1, again.len());
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn changes_are_merged_and_the_pack_kept_sealed() {
        let k = [5u8; 32];
        let e = |en: &str, ar: &str| memory::Entry { en: en.into(), ar: ar.into(), source: "reviewed".into() };
        let pack = vec![e("get in the car", "Monte"), e("lose the cops", "Sème")];
        let changes = vec![("get in the car".to_string(), Some(e("get in the car", "Monte dans la bagnole"))), ("lose the cops".to_string(), None), ("follow lucia".to_string(), Some(e("follow lucia", "Suis Lucia")))];
        let merged = apply_changes(pack, changes);
        assert_eq!(merged.iter().map(|e| (e.en.as_str(), e.ar.as_str())).collect::<Vec<_>>(), vec![("follow lucia", "Suis Lucia"), ("get in the car", "Monte dans la bagnole")]);
        let file = seal_pack(&k, &merged, "gta6", "fr", 7);
        assert_eq!(open_pack_with(&k, &file, "gta6", "fr", "7").unwrap().len(), 2);
        assert!(open_pack_with(&k, &file, "gta6", "fr", "6").is_none(), "sealed for its version");
        // the server's changes file: a line, and one taken out
        let text = "{\"en\":\"Follow Lucia.\",\"ar\":\"Suis Lucia\",\"source\":\"reviewed\"}\n{\"en\":\"lose the cops\",\"deleted\":true}\n";
        let sealed = {
            use aes_gcm::aead::{Aead, KeyInit, Payload};
            use std::io::Write as _;
            let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            gz.write_all(text.as_bytes()).unwrap();
            let zipped = gz.finish().unwrap();
            let nonce = [3u8; 12];
            let ct = aes_gcm::Aes256Gcm::new_from_slice(&k).unwrap().encrypt(aes_gcm::Nonce::from_slice(&nonce), Payload { msg: &zipped, aad: b"gta6/fr/6-7" }).unwrap();
            [b"G6T1".to_vec(), nonce.to_vec(), ct].concat()
        };
        let ch = open_changes(&k, &sealed, "gta6/fr/6-7").unwrap();
        assert_eq!(ch.len(), 2);
        assert_eq!((ch[0].0.as_str(), ch[0].1.as_ref().map(|e| e.ar.as_str())), ("follow lucia", Some("Suis Lucia")));
        assert_eq!((ch[1].0.as_str(), ch[1].1.is_none()), ("lose the cops", true));
        assert!(open_changes(&k, &sealed, "gta6/fr/5-7").is_none(), "only for those two versions");
    }

    #[test]
    fn layouts_change_only_what_they_name() {
        let style = crate::config::Style::default();
        let new: crate::config::Style = merged(&style, &serde_json::json!({ "no_such_field": 1 })).unwrap();
        assert_eq!(serde_json::to_value(&new).unwrap(), serde_json::to_value(&style).unwrap());
        let area = crate::config::Area { id: "subtitles".into(), x: 0.1, ..Default::default() };
        let moved: crate::config::Area = merged(&area, &serde_json::json!({ "x": 0.2, "font_scale": 1.1 })).unwrap();
        assert_eq!((moved.id.as_str(), moved.x, moved.font_scale), ("subtitles", 0.2, 1.1));
        assert!(merged(&area, &serde_json::json!({ "x": "far" })).is_none(), "a value that doesn't fit changes nothing");
        // an area's own style: only the parts named change, the rest stays what the area had
        let base = crate::config::Style { outline_width: 2.0, ..style.clone() };
        let st: crate::config::Style = merged(&base, &serde_json::json!({ "text_color": "#ffd25a" })).unwrap();
        assert_eq!((st.text_color.as_str(), st.outline_width, st.font_size), ("#ffd25a", 2.0, style.font_size));
        // an area without its own style (settings from before): none, and it reads back the same
        let v = serde_json::to_value(&area).unwrap();
        assert!(v["style"].is_null());
        let back: crate::config::Area = serde_json::from_value(serde_json::json!({ "id": "old" })).unwrap();
        assert!(back.style.is_none());
    }

    /// A pack made like the server makes it (PackCrypto.seal) opens; a wrong version doesn't.
    #[test]
    fn opens_server_packs() {
        use aes_gcm::aead::{Aead, KeyInit, Payload};
        use std::io::Write;
        let key = [7u8; 32];
        let jsonl = "{\"en\":\"press {1} to bring up the phone\",\"ar\":\"اضغط {1} لإظهار الهاتف.\",\"source\":\"reviewed\"}\n";
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(jsonl.as_bytes()).unwrap();
        let zipped = gz.finish().unwrap();
        let nonce = [9u8; 12];
        let ct = aes_gcm::Aes256Gcm::new_from_slice(&key)
            .unwrap()
            .encrypt(aes_gcm::Nonce::from_slice(&nonce), Payload { msg: &zipped, aad: b"gta5/ar/3" })
            .unwrap();
        let file = [b"G6T1".as_slice(), &nonce, &ct].concat();
        let e = open_pack_with(&key, &file, "gta5", "ar", "3").expect("opens");
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].en, "press {1} to bring up the phone");
        assert_eq!(e[0].source, "reviewed");
        assert!(open_pack_with(&key, &file, "gta5", "ar", "2").is_none(), "another version must not open");
        assert!(open_pack_with(&key, &file, "gta5", "hi", "3").is_none(), "another language must not open");
        assert!(open_pack_with(&[8u8; 32], &file, "gta5", "ar", "3").is_none(), "another key must not open");
    }
}
