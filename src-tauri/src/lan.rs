//! Live translations on other devices: a small read-only web page on the home network
//! (http://<this PC>:<port>/), for a phone, tablet or another computer, and the overlays for a TV
//! (http://<this PC>:<port>/tv, drawn by the VI Translator app on the TV over the game picture).
//!
//! Off by default. While "lan_page" is on, it listens on all addresses but serves only the pages,
//! their fonts and drawing code, the list of recent translations and what the overlays show now:
//! never settings beyond the overlays' own places and style, the picture or the OBS page (those
//! stay on the 127.0.0.1-only server in obs_page.rs).

use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tauri::AppHandle;

use crate::capture::Shared;
use crate::config::Config;
use crate::obs_page::{read_request, respond, serve_asset, Slot};
use vi_translator_core::lan_page::LivePage;
use vi_translator_core::tv_server;

/// The page's address and QR code (the core's).
pub use vi_translator_core::lan_page::{LanInfo, info};
/// This PC's address on the home network a device (the TV) at an address reaches it at.
pub use vi_translator_core::net::lan_ip_toward;
use vi_translator_core::net::host_is_address;

/// Runs for the app's lifetime: starts and stops the listener as the setting changes.
pub fn run(app: AppHandle, cfg: Arc<Mutex<Config>>, shared: Arc<Shared>, memory: Arc<Mutex<crate::memory::Memory>>, sink: Arc<dyn vi_translator_core::Sink>) {
    use std::sync::atomic::Ordering;
    // the live page (the core's), its snapshots made as the app's Snapshot button makes them
    let page = {
        let (c, sh) = (cfg.clone(), shared.clone());
        Arc::new(LivePage {
            cfg: cfg.clone(),
            live: shared.live.clone(),
            paths: crate::paths(),
            sink,
            snapshot: Box::new(move || crate::snapshot_now(&c, &sh, &memory)),
        })
    };
    let mut listener: Option<(TcpListener, u16)> = None;
    loop {
        let (on, port) = {
            let c = cfg.lock().unwrap();
            shared.live_off.store(!c.live_translations, Ordering::Relaxed);
            (c.lan_page, c.lan_port)
        };
        match (&listener, on) {
            (Some((_, p)), true) if *p == port => {}
            (_, true) => {
                listener = match TcpListener::bind(("0.0.0.0", port)) {
                    Ok(l) => {
                        let _ = l.set_nonblocking(true);
                        *shared.lan_error.lock().unwrap() = None;
                        Some((l, port))
                    }
                    // (shown in the window: "Starting…" forever would hide it)
                    Err(e) => {
                        *shared.lan_error.lock().unwrap() = Some(format!("port {port}: {e}"));
                        None
                    }
                };
            }
            (Some(_), false) => listener = None,
            (None, false) => {}
        }
        shared.lan_running.store(listener.is_some(), Ordering::Relaxed);
        match &listener {
            Some((l, _)) => {
                // accept everything waiting, then look at the setting again
                static ACTIVE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
                while let Ok((s, _)) = l.accept() {
                    // (too many at once: closed)
                    let Some(slot) = Slot::take(&ACTIVE) else { continue };
                    let _ = s.set_nonblocking(false);
                    let (a, c, sh, pg) = (app.clone(), cfg.clone(), shared.clone(), page.clone());
                    std::thread::spawn(move || {
                        let _slot = slot;
                        handle(&a, &c, &sh, &pg, s);
                    });
                }
                std::thread::sleep(Duration::from_millis(60));
            }
            None => std::thread::sleep(Duration::from_millis(500)),
        }
    }
}

fn handle(app: &AppHandle, cfg: &Mutex<Config>, shared: &Shared, page: &LivePage, s: TcpStream) {
    let Some(req) = read_request(&s) else { return };
    // Asked for by an address (this PC's, as the phone opens it), not a name: a website can't point a
    // name of its own at this PC and read the page from the browser ("DNS rebinding").
    if !host_is_address(&req.host) {
        return respond(s, "403 Forbidden", "text/plain", b"not allowed");
    }
    // the TV app's page (/tv, /tv.json, /tv/forget) and a Samsung TV offering its code, then the
    // live page: the core's (the same as a phone serves), with the files from inside the executable
    let serve = |s, file: &str| serve_asset(app, s, file);
    let Some(s) = tv_server::route(&req, s, cfg, shared, &serve) else { return };
    let Some(s) = page.route(&req, s, &serve) else { return };
    respond(s, "404 Not Found", "text/plain", b"not found")
}

#[cfg(test)]
mod host_tests {
    // Every answer written by hand ends its header lines in CRLF: a line break inside the literal
    // would be a bare LF on the wire (browsers take it, strict HTTP clients don't).
    #[test]
    fn hand_written_answers_use_crlf() {
        for (file, src) in [("lan.rs", include_str!("lan.rs")), ("obs_page.rs", include_str!("obs_page.rs"))] {
            for line in src.lines() {
                // (the patterns split, so this line isn't one)
                let starts_answer = [concat!("\"HTTP", "/1.1 "), concat!("\"--", "frame")].iter().any(|a| line.contains(a));
                if starts_answer {
                    assert!(line.contains("\\r\\n"), "{file}: a header line without \\r\\n: {}", line.trim());
                }
            }
        }
    }
}
