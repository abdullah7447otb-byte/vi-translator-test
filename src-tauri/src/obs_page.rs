//! The Arabic as a web page for an OBS Browser Source: http://127.0.0.1:<port>/obs
//!
//! The page has a transparent background and draws only the Arabic (subtitles, instructions, the
//! Mission Failed page) where the overlays would, sized to the page — so stacked over the capture
//! card in an OBS scene it lands on the game picture. It polls /state ~10 times a second.
//! Listens on 127.0.0.1 only: nothing outside this PC can reach it.

use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tauri::AppHandle;

use crate::capture::Shared;
use crate::config::Config;

// (the HTTP pieces are the core's, shared with its TV page server)
pub(crate) use vi_translator_core::net::{Slot, read_request, respond};
#[cfg(test)]
use vi_translator_core::net::{MAX_CONNECTIONS, Request};
// (what the overlays show: also the core's, for the TV page)
pub(crate) use vi_translator_core::live::overlay_state;

/// The app's own window, as the origin of its requests (Windows; macOS and Linux).
fn app_origin(origin: &str) -> bool {
    matches!(origin, "http://tauri.localhost" | "https://tauri.localhost" | "tauri://localhost")
}

fn state_json(cfg: &Mutex<Config>, shared: &Shared) -> Vec<u8> {
    let mut v = overlay_state(cfg, shared);
    let extra = serde_json::json!({
        // for troubleshooting
        "status": shared.status.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone(),
        "frame": shared.latest().map(|f| [f.w, f.h, f.seq as u32]),
        "preview": {
            "clients": shared.preview_clients.load(std::sync::atomic::Ordering::Relaxed),
            "paused": shared.preview_paused.load(std::sync::atomic::Ordering::Relaxed),
            "raw": shared.preview_raw.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_ref().map(|f| [f.w, f.h]),
            "jpeg_seq": shared.preview_seq.load(std::sync::atomic::Ordering::Relaxed),
        },
    });
    if let (Some(o), serde_json::Value::Object(e)) = (v.as_object_mut(), extra) {
        o.extend(e);
    }
    serde_json::to_vec(&v).unwrap_or_default()
}

fn handle(app: &AppHandle, cfg: &Mutex<Config>, shared: &Shared, s: TcpStream, port: u16) {
    let Some(req) = read_request(&s) else { return };
    // Only requests addressed to this PC by its own name (OBS, the app's window): a website that
    // points its own domain name here (DNS rebinding) gets nothing.
    if ![format!("127.0.0.1:{port}"), format!("localhost:{port}"), format!("[::1]:{port}")].contains(&req.host) {
        return respond(s, "403 Forbidden", "text/plain", b"not allowed");
    }
    let (path, query) = (req.path.as_str(), req.query.as_str());
    match path {
        "/state" => respond(s, "200 OK", "application/json; charset=utf-8", &state_json(cfg, shared)),
        "/preview" | "/preview.jpg" => preview_frame(shared, s, query, app_origin(&req.origin).then_some(req.origin.as_str())),
        "/preview.mjpg" => stream_preview(shared, s),
        "/" | "/obs" => serve_asset(app, s, "obs.html"),
        p if !p.contains("..") => serve_asset(app, s, p.trim_start_matches('/')),
        _ => respond(s, "404 Not Found", "text/plain", b"not found"),
    }
}

/// The app's live preview, one frame per request: `/preview?after=<seq>` answers with the newest
/// preview frame once there is one other than `seq` (waiting up to a second; 204 if none came), its
/// number in `X-Seq`. The window asks for the next frame only after showing this one, so frames can
/// never queue up on the way and the picture is never behind (an MJPEG stream, by contrast, keeps
/// sending, and if the window draws a little slower the extra frames pile up in the connection).
///
/// The frame is either a JPEG, or — from a capture card sending YUY2 — the card's own frame, raw
/// (`application/x-yuy2`, size and colour standard in headers), which the window turns into colour
/// on the GPU like OBS does: no encoding or decoding on the way.
fn preview_frame(shared: &Shared, mut s: TcpStream, query: &str, origin: Option<&str>) {
    let after: u64 = query.split('&').find_map(|kv| kv.strip_prefix("after=")).and_then(|v| v.parse().ok()).unwrap_or(0);
    // (the picture is for the app's own window: no other website may read it)
    let expose = match origin {
        Some(o) => format!("Access-Control-Allow-Origin: {o}\r\nVary: Origin\r\nAccess-Control-Expose-Headers: X-Seq, X-Width, X-Height, X-Matrix, X-Range\r\n"),
        None => String::new(),
    };
    let start = std::time::Instant::now();
    loop {
        shared.mark_preview_pulled();
        let jpeg = shared.preview_jpeg.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        let yuv = shared.preview_yuv.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        // the newer of the two (the source may have changed from one kind to the other)
        let yuv_newer = match (&jpeg, &yuv) {
            (Some((j, _)), Some((y, _))) => y > j,
            (None, Some(_)) => true,
            _ => false,
        };
        if yuv_newer {
            if let Some((seq, f)) = yuv.filter(|(seq, _)| *seq != after) {
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/x-yuy2\r\nContent-Length: {}\r\nX-Seq: {seq}\r\nX-Width: {}\r\nX-Height: {}\r\nX-Matrix: {}\r\nX-Range: {}\r\nCache-Control: no-store\r\n{expose}Connection: close\r\n\r\n",
                    f.data.len(),
                    f.w,
                    f.h,
                    if f.color.bt709 { "709" } else { "601" },
                    if f.color.full_range { "full" } else { "limited" },
                );
                let _ = s.set_nodelay(true);
                let _ = s.write_all(head.as_bytes()).and_then(|_| s.write_all(&f.data));
                return;
            }
        } else if let Some((seq, jpeg)) = jpeg.filter(|(seq, _)| *seq != after) {
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\nX-Seq: {seq}\r\nCache-Control: no-store\r\n{expose}Connection: close\r\n\r\n",
                jpeg.len()
            );
            let _ = s.set_nodelay(true);
            let _ = s.write_all(head.as_bytes()).and_then(|_| s.write_all(&jpeg));
            return;
        }
        if start.elapsed() >= Duration::from_secs(1) {
            let head = format!("HTTP/1.1 204 No Content\r\nCache-Control: no-store\r\n{expose}Content-Length: 0\r\nConnection: close\r\n\r\n");
            let _ = s.write_all(head.as_bytes());
            return;
        }
        std::thread::sleep(Duration::from_millis(3));
    }
}

/// The live preview as an MJPEG stream (an <img> plays it natively): each new preview JPEG is
/// sent as one part. Preview frames are only made while at least one stream is open.
fn stream_preview(shared: &Shared, mut s: TcpStream) {
    use std::sync::atomic::Ordering;
    let head = "HTTP/1.1 200 OK\r\nContent-Type: multipart/x-mixed-replace; boundary=frame\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n";
    if s.write_all(head.as_bytes()).is_err() {
        return;
    }
    let _ = s.set_nodelay(true);
    shared.preview_clients.fetch_add(1, Ordering::Relaxed);
    let mut sent = 0u64;
    let mut sent_at = std::time::Instant::now();
    loop {
        // a new frame — or, when the picture hasn't changed for a second (a still game screen), the
        // same one again: keeps the picture there and notices a closed viewer
        let next = shared
            .preview_jpeg
            .lock()
            .unwrap()
            .clone()
            .filter(|(seq, _)| *seq != sent || sent_at.elapsed() >= Duration::from_secs(1));
        match next {
            Some((seq, jpeg)) => {
                sent = seq;
                sent_at = std::time::Instant::now();
                let part = format!("--frame\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\n\r\n", jpeg.len());
                if s.write_all(part.as_bytes()).and_then(|_| s.write_all(&jpeg)).and_then(|_| s.write_all(b"\r\n")).is_err() {
                    break; // the picture was closed
                }
            }
            None => std::thread::sleep(Duration::from_millis(4)),
        }
    }
    shared.preview_clients.fetch_sub(1, Ordering::Relaxed);
}

/// Files of the app's own UI (the page, common.js, fonts), from inside the executable.
pub(crate) fn serve_asset(app: &AppHandle, s: TcpStream, path: &str) {
    match app.asset_resolver().get(path.to_string()) {
        Some(a) => respond(s, "200 OK", a.mime_type(), a.bytes()),
        None => respond(s, "404 Not Found", "text/plain", b"not found"),
    }
}

pub fn run(app: AppHandle, cfg: Arc<Mutex<Config>>, shared: Arc<Shared>) {
    let port = cfg.lock().unwrap_or_else(std::sync::PoisonError::into_inner).obs_page_port;
    let listener = match TcpListener::bind(("127.0.0.1", port)) {
        Ok(l) => l,
        Err(e) => {
            vi_translator_core::vlog!("OBS page: port {port} unavailable: {e}");
            return;
        }
    };
    static ACTIVE: AtomicUsize = AtomicUsize::new(0);
    for s in listener.incoming().flatten() {
        // (too many at once: the newest waits for nothing, it's closed)
        let Some(slot) = Slot::take(&ACTIVE) else { continue };
        let (app, cfg, shared) = (app.clone(), cfg.clone(), shared.clone());
        std::thread::spawn(move || {
            let _slot = slot;
            handle(&app, &cfg, &shared, s, port);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(raw: &[u8]) -> Option<Request> {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut c = TcpStream::connect(l.local_addr().unwrap()).unwrap();
        c.write_all(raw).unwrap();
        let (s, _) = l.accept().unwrap();
        read_request(&s)
    }

    #[test]
    fn reads_the_target_host_and_origin() {
        let r = request(b"GET /preview?after=7 HTTP/1.1
Host: 127.0.0.1:8790
ORIGIN: http://tauri.localhost
X-Other: y

").unwrap();
        assert_eq!((r.path.as_str(), r.query.as_str(), r.host.as_str(), r.origin.as_str()), ("/preview", "after=7", "127.0.0.1:8790", "http://tauri.localhost"));
        assert!(app_origin(&r.origin));
        assert!(!app_origin("http://evil.example") && !app_origin("null") && !app_origin(""));
    }

    #[test]
    fn reads_a_small_body_and_the_vit_header() {
        let r = request(b"POST /vit-tv/offer HTTP/1.1\r\nHost: 192.168.1.5:8791\r\nX-VIT: 1\r\nContent-Length: 7\r\n\r\n{\"a\":1}").unwrap();
        assert_eq!((r.method.as_str(), r.path.as_str(), r.vit, r.body.as_slice()), ("POST", "/vit-tv/offer", true, &b"{\"a\":1}"[..]));
        let r = request(b"POST /x HTTP/1.1\r\nContent-Length: 99999\r\n\r\nabc").unwrap();
        assert!(!r.vit && r.body.is_empty(), "too long: not read");
    }

    #[test]
    fn a_huge_request_is_cut_short() {
        // a header with no end: read up to 8 KB, then given up (no unbounded memory)
        let mut raw = b"GET / HTTP/1.1
Host: 127.0.0.1:1
X: ".to_vec();
        raw.extend(std::iter::repeat_n(b'a', 64 * 1024));
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let t = std::thread::spawn(move || {
            let mut c = TcpStream::connect(addr).unwrap();
            let _ = c.write_all(&raw);
        });
        let (s, _) = l.accept().unwrap();
        let r = read_request(&s);
        assert!(r.is_some_and(|r| r.path == "/"), "the first line is read, the endless header isn't");
        let _ = t.join();
    }

    #[test]
    fn connections_are_capped() {
        static COUNT: AtomicUsize = AtomicUsize::new(0);
        let slots: Vec<Slot> = (0..MAX_CONNECTIONS).map(|_| Slot::take(&COUNT).unwrap()).collect();
        assert!(Slot::take(&COUNT).is_none());
        drop(slots);
        assert!(Slot::take(&COUNT).is_some());
    }
}
