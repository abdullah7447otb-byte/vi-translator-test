//! Live translations on other devices: a small read-only web page on the home network
//! (http://<this device>:<port>/, ui/live.html), for a phone, tablet or another computer, served by
//! the same server as the TV page (tv_server.rs) — the desktop's, or a phone's `TvServer`.
//!
//! It serves only the page, its fonts and drawing code, the list of recent translations and, when
//! the settings allow it, a few switches ("Allow changes from that page") and snapshots: never
//! settings beyond those, or the picture.

use std::net::TcpStream;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use serde::Serialize;

use crate::config::Config;
use crate::live::Live;
use crate::net::{Request, lan_ip, respond};
use crate::snapshot::Snapshot;
use crate::{Paths, Sink};

/// What the page needs from its host: the settings and what's shown, where the settings are saved,
/// where events go ("lan-control", "config", "snapshot"), and how a snapshot is made.
pub struct LivePage {
    pub cfg: Arc<Mutex<Config>>,
    pub live: Arc<Live>,
    pub paths: Paths,
    pub sink: Arc<dyn Sink>,
    pub snapshot: Box<dyn Fn() -> anyhow::Result<Snapshot> + Send + Sync>,
    /// the translation memory, for a line corrected on the page (none: no corrections there)
    pub memory: Option<Arc<Mutex<crate::memory::Memory>>>,
}

/// How long a `live.json?wait=1` waits for a new line before answering with none.
const LONG_POLL: std::time::Duration = std::time::Duration::from_secs(20);

#[derive(Serialize, Clone, Debug)]
pub struct LanInfo {
    pub enabled: bool,
    pub running: bool,
    pub url: Option<String>,
    /// QR code of the address, as SVG
    pub qr: Option<String>,
    /// why the page isn't running although it's on (the port is taken …)
    pub error: Option<String>,
}

/// The page's address (with a QR code of it) while it's on and served.
pub fn info(c: &Config, live: &Live) -> LanInfo {
    let running = live.lan_running.load(Ordering::Relaxed);
    let url = (c.lan_page && running).then(|| lan_ip().map(|ip| format!("http://{ip}:{}/", c.lan_port))).flatten();
    let qr = url.as_ref().and_then(|u| {
        let code = qrcode::QrCode::new(u.as_bytes()).ok()?;
        Some(code.render::<qrcode::render::svg::Color>().min_dimensions(180, 180).quiet_zone(true).build())
    });
    let error = (c.lan_page && !running).then(|| live.lan_error.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()).flatten();
    LanInfo { enabled: c.lan_page, running, url, qr, error }
}

impl LivePage {
    /// The line `id` of the list is `ar`: into the memory as the player's, in the list, and on screen
    /// if it's still up there. The line as the list now has it (None: no such line any more).
    fn correct(&self, id: u64, ar: &str) -> Option<serde_json::Value> {
        let (area, en) = {
            let log = self.live.live_log.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let l = log.iter().find(|l| l["id"].as_u64() == Some(id))?;
            (l["area"].as_str()?.to_string(), l["en"].as_str()?.to_string())
        };
        let segs: crate::translate::Segments = vec![(ar.to_string(), None)];
        if let Some(m) = &self.memory {
            let lang = self.cfg.lock().unwrap_or_else(std::sync::PoisonError::into_inner).target_lang.clone();
            let mut m = m.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if m.lang() == lang {
                m.put(&en, &segs, "player");
            }
        }
        crate::vlog!("[lan] corrected: {en:?} -> {ar:?}");
        // the list: this line, and any other line of the same English
        let mut out = None;
        for l in self.live.live_log.lock().unwrap_or_else(std::sync::PoisonError::into_inner).iter_mut() {
            if l["en"].as_str() == Some(en.as_str()) {
                l["text"] = serde_json::json!(ar);
                l["segs"] = serde_json::json!([[ar, null]]);
                l["corrected"] = serde_json::json!(true);
                if l["id"].as_u64() == Some(id) {
                    out = Some(l.clone());
                }
            }
        }
        // on screen now: shown corrected at once (the overlays, the TV and OBS pages)
        let shown = {
            let mut shown = self.live.shown.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            shown.get_mut(&area).filter(|ev| ev["en"].as_str() == Some(en.as_str())).map(|ev| {
                ev["segments"] = serde_json::json!([[ar, null]]);
                ev.clone()
            })
        };
        if let Some(ev) = shown {
            self.sink.emit("translation", ev);
        }
        out
    }

    /// The page's routes (after the host check, and the TV routes). `serve` answers with one of the
    /// page's files by name ("live.html"). The stream back when the request is for something else.
    pub fn route(&self, req: &Request, s: TcpStream, serve: &dyn Fn(TcpStream, &str)) -> Option<TcpStream> {
        let (cfg, shared) = (&*self.cfg, &*self.live);
        let (path, query) = (req.path.as_str(), req.query.as_str());
        match path {
            "/" | "/live" => serve(s, "live.html"),
            "/live.json" => {
                let after: u64 = query.split('&').find_map(|kv| kv.strip_prefix("after=")).and_then(|v| v.parse().ok()).unwrap_or(0);
                // (`wait=1`: answered as soon as a new line is there, or after LONG_POLL with none — the
                // page then has each line the moment it's ready, not up to 700 ms later)
                let wait = query.split('&').any(|kv| kv == "wait=1");
                let newer = || -> Vec<serde_json::Value> {
                    shared.live_log.lock().unwrap_or_else(std::sync::PoisonError::into_inner).iter().filter(|l| l["id"].as_u64().unwrap_or(0) > after).cloned().collect()
                };
                let started = std::time::Instant::now();
                let mut lines = newer();
                // (a line id below `after`: the app started again, its ids with it — answered at once)
                let restarted = shared.live_seq.load(Ordering::Relaxed) < after;
                while wait && lines.is_empty() && !restarted && started.elapsed() < LONG_POLL {
                    std::thread::sleep(std::time::Duration::from_millis(40));
                    lines = newer();
                }
                let live = !shared.live_off.load(Ordering::Relaxed);
                // ("run": new each time the app starts; line ids start again then, and the page with it)
                let (snap, control, ui) = {
                    let c = cfg.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    (c.snap_lan, c.lan_control.then(|| self.controls(&c)), c.ui_lang.clone())
                };
                // ("ui": the app's own interface language, which the page takes for its buttons and notices)
                let body = serde_json::json!({ "run": crate::net::build_id(), "lang": shared.live_lang.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone(), "lines": lines, "live": live, "snap": snap, "control": control, "ui": ui, "perf": crate::perf::perf_status() });
                respond(s, "200 OK", "application/json; charset=utf-8", body.to_string().as_bytes());
            }
            "/common.js" => serve(s, "common.js"),
            "/snapshot.js" => serve(s, "snapshot.js"),
            // the interface's words, in the app's languages (for the page's own text)
            "/i18n.js" => serve(s, "i18n.js"),
            p if p.starts_with("/i18n/") && p.ends_with(".js") && p[6..p.len() - 3].chars().all(|c| c.is_ascii_alphanumeric() || c == '-') && p.len() <= 20 => {
                serve(s, p.trim_start_matches('/'))
            }
            // a switch changed on this page (a POST from its own address, when the app allows it):
            // {"what": "overlays" | "live" | "area" | "page" | "lang", "id": "...", "on": true}
            "/control" if req.method == "POST" && req.origin == format!("http://{}", req.host) => {
                let Ok(v) = serde_json::from_slice::<serde_json::Value>(&req.body) else {
                    respond(s, "400 Bad Request", "text/plain", b"bad request");
                    return None;
                };
                let (what, id, on) = (v["what"].as_str().unwrap_or(""), v["id"].as_str().unwrap_or(""), v["on"].as_bool());
                let Some(on) = on else {
                    respond(s, "400 Bad Request", "text/plain", b"bad request");
                    return None;
                };
                let mut c = cfg.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                if !c.lan_control {
                    respond(s, "403 Forbidden", "text/plain; charset=utf-8", b"Changes from this page are switched off in VI Translator.");
                    return None;
                }
                let ok = match what {
                    "overlays" => {
                        c.overlays_enabled = on;
                        true
                    }
                    "lang" if crate::langs::LANGS.iter().any(|l| l.code == id) => {
                        c.target_lang = id.to_string();
                        true
                    }
                    "live" => {
                        c.live_translations = on;
                        shared.live_off.store(!on, Ordering::Relaxed);
                        true
                    }
                    // (only the profile's own areas, and the switches its game's script declares)
                    "area" if c.areas.iter().any(|a| a.id == id) => c.set_shown("area", id, on),
                    "page" if crate::pagescript::options_for(&c.game(), &self.paths.data).iter().any(|o| o.id == id) => c.set_shown("page", id, on),
                    _ => false,
                };
                if !ok {
                    respond(s, "404 Not Found", "text/plain", b"not found");
                    return None;
                }
                let _ = c.save(&self.paths.config);
                // (the app's window keeps its own copy of the settings: told, so its next save doesn't undo this)
                self.sink.emit(
                    "lan-control",
                    serde_json::json!({
                        "overlays_enabled": c.overlays_enabled, "live_translations": c.live_translations, "shown": c.shown, "target_lang": c.target_lang,
                        "profile": c.profile, "areas": c.areas.iter().map(|a| (a.id.clone(), a.enabled)).collect::<std::collections::BTreeMap<_, _>>(),
                        "more": c.profile_opts.more,
                    }),
                );
                self.sink.emit("config", serde_json::to_value(c.redacted()).unwrap_or_default());
                respond(s, "200 OK", "application/json; charset=utf-8", self.controls(&c).to_string().as_bytes())
            }
            // a line corrected on this page (a POST from its own address, when the app allows changes
            // from it): {"id": <the line's id>, "ar": "the right translation"}. It goes into the
            // translation memory as the player's own (above every other translation, never replaced),
            // and on screen at once if the line is still up.
            "/correct" if req.method == "POST" && req.origin == format!("http://{}", req.host) => {
                if !cfg.lock().unwrap_or_else(std::sync::PoisonError::into_inner).lan_control {
                    respond(s, "403 Forbidden", "text/plain; charset=utf-8", b"Changes from this page are switched off in VI Translator.");
                    return None;
                }
                let v: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
                let (Some(id), Some(ar)) = (v["id"].as_u64(), v["ar"].as_str().map(str::trim).filter(|a| !a.is_empty() && a.chars().count() <= 500)) else {
                    respond(s, "400 Bad Request", "text/plain", b"bad request");
                    return None;
                };
                match self.correct(id, ar) {
                    Some(line) => respond(s, "200 OK", "application/json; charset=utf-8", line.to_string().as_bytes()),
                    None => respond(s, "404 Not Found", "text/plain; charset=utf-8", b"That line isn't in the list any more."),
                }
            }
            // a snapshot (the whole picture translated), asked for by this page (a POST from its own
            // address: another website can't make one), when the app allows it
            "/snapshot" if req.method == "POST" && req.origin == format!("http://{}", req.host) => {
                if !cfg.lock().unwrap_or_else(std::sync::PoisonError::into_inner).snap_lan {
                    respond(s, "403 Forbidden", "text/plain; charset=utf-8", b"Snapshots from this page are switched off in VI Translator.");
                    return None;
                }
                match (self.snapshot)() {
                    Ok(snap) => {
                        // (the app keeps it and shows it in its Snapshot tab)
                        self.sink.emit("snapshot", serde_json::to_value(&snap).unwrap_or_default());
                        respond(s, "200 OK", "application/json; charset=utf-8", serde_json::to_string(&snap).unwrap_or_default().as_bytes())
                    }
                    Err(e) => respond(s, "500 Internal Server Error", "text/plain; charset=utf-8", format!("{e:#}").as_bytes()),
                }
            }
            // the pages' fonts and nothing else
            p if p.starts_with("/fonts/") && !p.contains("..") || p == "/fonts.css" => serve(s, p.trim_start_matches('/')),
            _ => return Some(s),
        }
        None
    }

    /// What the page can switch: the overlays, the live translations, the profile's read areas and
    /// its game's pages (with each one's state).
    pub fn controls(&self, c: &Config) -> serde_json::Value {
        let opts = crate::pagescript::options_for(&c.game(), &self.paths.data);
        serde_json::json!({
            // the language the game is translated into, and the ones it can be
            "lang": c.target_lang,
            "langs": crate::langs::LANGS.iter().map(|l| serde_json::json!({ "code": l.code, "name": l.name, "native": l.native })).collect::<Vec<_>>(),
            "overlays": c.overlays_enabled,
            "live": c.live_translations,
            "areas": c.areas.iter().map(|a| serde_json::json!({ "id": a.id, "name": a.name, "on": a.enabled })).collect::<Vec<_>>(),
            // (a page's name as the script gives it: text, or {"en": …, "ar": …} for the page to pick from)
            "pages": opts.iter().map(|o| serde_json::json!({ "id": o.id, "name": o.name, "on": c.profile_opts.more.get(&o.id).copied().unwrap_or(o.on) })).collect::<Vec<_>>(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qr_code_for_the_address() {
        let c = Config { lan_page: true, ..Config::default() };
        let live = Live::default();
        live.lan_running.store(true, Ordering::Relaxed);
        let i = info(&c, &live);
        if i.url.is_some() {
            assert!(i.qr.as_deref().is_some_and(|s| s.starts_with("<?xml") || s.contains("<svg")));
        }
        let off = info(&Config::default(), &live);
        assert!(off.url.is_none() && off.qr.is_none());
    }
}
