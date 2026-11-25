#!/usr/bin/env python3
"""The PS5 picture in the app's window, drawn as the app draws it, at a steady 60 frames a second.

    python3 tools/smoothness.py <ui dir> <out.md> [--seconds 60] [--size 1280x720]

The app's own page (ui/index.html with the mock Tauri harness, tools/ui_mock) in headless Chrome;
a server gives it I420 frames exactly as the app's own local server does (`/preview?after=N`,
long-polled, X-Seq / X-Width / X-Height), a new one every 1/60 s. Each frame drawn is timed where
the app draws it (its i420Draw). For "Smooth" and "Lowest delay", each with nothing over the
picture, with the in-place phone layer, and with the picture panel open:
- the share of the frames served that were drawn (pass: at least 99 %);
- the longest gap between two drawn frames after the first second (pass: at most two refreshes).
Exits 1 if a check fails.
"""
import http.server
import json
import os
import socketserver
import sys
import threading
import time

ROOT = os.path.dirname(os.path.abspath(__file__))
FPS = 60.0


def frames(w, h, n=60):
    # (a moving pattern: each frame different, so a frame shown twice or skipped is a real one)
    out = []
    ys = w * h
    uv = (w // 2) * (h // 2)
    for k in range(n):
        y = bytearray(ys)
        band = (k * w // n)
        for row in range(0, h, 8):
            start = row * w
            y[start:start + w] = bytes(((x + band) & 0xFF) for x in range(w))
        out.append(bytes(y) + bytes([128]) * uv + bytes([128 + (k % 40)]) * uv)
    return out


class Server(socketserver.ThreadingMixIn, http.server.HTTPServer):
    daemon_threads = True
    allow_reuse_address = True


def serve(ui, w, h):
    pics = frames(w, h)
    state = {"t0": time.monotonic(), "sent": 0}
    mock = open(os.path.join(ROOT, "ui_mock", "mock.js"), "rb").read()
    cfg = open(os.path.join(ROOT, "ui_mock", "mockcfg.js"), "rb").read()

    class H(http.server.SimpleHTTPRequestHandler):
        def __init__(self, *a, **k):
            super().__init__(*a, directory=ui, **k)

        def log_message(self, *a):
            pass

        def send(self, body, ct, extra=()):
            self.send_response(200)
            self.send_header("Content-Type", ct)
            self.send_header("Content-Length", str(len(body)))
            self.send_header("Cache-Control", "no-store")
            for k, v in extra:
                self.send_header(k, v)
            self.end_headers()
            self.wfile.write(body)

        def do_GET(self):
            p = self.path.split("?")[0]
            if p == "/preview":
                after = int(self.path.split("after=")[1].split("&")[0]) if "after=" in self.path else 0
                # (the frame of now; if the page has it, the next one when it's due)
                while True:
                    k = int((time.monotonic() - state["t0"]) * FPS) + 1
                    if k > after:
                        break
                    time.sleep(max(0.0005, (state["t0"] + k / FPS) - time.monotonic()))
                state["sent"] += 1
                return self.send(pics[k % len(pics)], "application/x-i420",
                                 [("X-Seq", str(k)), ("X-Width", str(w)), ("X-Height", str(h)), ("X-Matrix", "709"), ("X-Range", "limited")])
            if p in ("/", "/index.html"):
                html = open(os.path.join(ui, "index.html"), "rb").read()
                html = html.replace(b'<script src="app.js"></script>', b'<script src="/__mockcfg.js"></script><script src="/__mock.js"></script><script src="app.js"></script>')
                return self.send(html, "text/html; charset=utf-8")
            if p == "/__mock.js":
                return self.send(mock, "text/javascript")
            if p == "/__mockcfg.js":
                return self.send(cfg, "text/javascript")
            return super().do_GET()

    srv = Server(("127.0.0.1", 0), H)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    return srv, state


PHONE = {
    "screen": [0.62, 0.18, 0.22, 0.66], "screen_bg": "#efe7e7", "list": False, "turn": 0, "slant": 0,
    "quad": [[0.62, 0.18], [0.84, 0.18], [0.84, 0.84], [0.62, 0.84]], "anchor": 1,
    "pieces": [
        {"rect": [0.66, 0.22, 0.08, 0.03], "title": True, "ar": "دوني", "fg": "#000000", "bg": "#efe7e7"},
        {"rect": [0.64, 0.30, 0.15, 0.06], "ar": "وصّلت لك الأغراض", "en": "Dropped that stuff off for you", "bg": "#dfe6f5", "fg": "#111111"},
        {"rect": [0.64, 0.42, 0.17, 0.10], "ar": "مشكور يا صاحبي! لو لقيت أي شي ثاني جيبه لي", "en": "Thx amigo! U find anything else bring it 2 me", "bg": "#ffffff", "fg": "#111111"},
    ],
}


def run(pg, base, seconds, mode, extra):
    pg.goto(f"{base}/index.html")
    pg.evaluate("m => localStorage.setItem('picture-delay', m)", mode)
    pg.reload()
    pg.wait_for_timeout(1500)
    pg.evaluate("""() => {
      window.__drawn = [];
      const orig = window.i420Draw;
      window.i420Draw = function (...a) { const ok = orig.apply(this, a); if (ok) window.__drawn.push(performance.now()); return ok; };
    }""")
    if extra == "phone":
        pg.evaluate("p => { MOCK_EMIT('phone', p); MOCK_EMIT('phone-anchor', { gen: 1, m: [1,0,0,1,0,0,0,0] }); }", PHONE)
    if extra == "panel":
        pg.evaluate("() => { const b = document.getElementById('pic-btn'); if (b) b.click(); }")
    pg.evaluate(f"() => MOCK_EMIT('frame', {{ live: true, w: 1920, h: 1080, preview_url: '{base}/preview', status: 'Playing on PS5', load: 0, read_fps: 10, preview_fps: 60 }})")
    # (the app asks for "frame" twice a second: kept so, so its loop keeps the stream)
    t_end = time.monotonic() + seconds
    while time.monotonic() < t_end:
        pg.wait_for_timeout(500)
        pg.evaluate(f"() => MOCK_EMIT('frame', {{ live: true, w: 1920, h: 1080, preview_url: '{base}/preview', status: 'Playing on PS5', load: 0, read_fps: 10, preview_fps: 60 }})")
    drawn = pg.evaluate("() => window.__drawn")
    refresh = pg.evaluate("""() => new Promise(r => { const t = []; const f = (x) => { t.push(x); if (t.length < 61) requestAnimationFrame(f); else r((t[60] - t[0]) / 60); }; requestAnimationFrame(f); })""")
    return drawn, refresh


def main():
    ui, out = os.path.abspath(sys.argv[1]), sys.argv[2]
    seconds = float(sys.argv[sys.argv.index("--seconds") + 1]) if "--seconds" in sys.argv else 60.0
    w, h = (int(x) for x in (sys.argv[sys.argv.index("--size") + 1] if "--size" in sys.argv else "1280x720").split("x"))
    srv, state = serve(ui, w, h)
    base = f"http://127.0.0.1:{srv.server_address[1]}"
    from playwright.sync_api import sync_playwright
    rows, ok = [], True
    with sync_playwright() as p:
        try:
            b = p.chromium.launch(channel="chrome", args=["--ignore-gpu-blocklist", "--enable-gpu-rasterization"])
        except Exception:
            b = p.chromium.launch(args=["--use-gl=swiftshader", "--enable-unsafe-swiftshader", "--ignore-gpu-blocklist"])
        for mode in ("smooth", "low"):
            for extra in ("plain", "phone", "panel"):
                pg = b.new_page(viewport={"width": 1600, "height": 960})
                errors = []
                pg.on("pageerror", lambda e: errors.append(str(e)))
                state["t0"], state["sent"] = time.monotonic(), 0
                drawn, refresh = run(pg, base, seconds, mode, extra)
                pg.close()
                served = state["sent"]
                if len(drawn) < 2:
                    rows.append((mode, extra, served, len(drawn), 0, 0, refresh, False, errors))
                    ok = False
                    continue
                start = drawn[0]
                steady = [t for t in drawn if t - start >= 1000]
                gaps = [b2 - a for a, b2 in zip(steady, steady[1:])]
                worst = max(gaps) if gaps else 0
                # (frames made while the page ran: one every 1/60 s from its first drawn frame)
                made = int((drawn[-1] - start) / 1000 * FPS) + 1
                share = len(drawn) / max(made, 1) * 100
                good = share >= 99.0 and worst <= 2 * refresh + 1.0 and not errors
                ok &= good
                rows.append((mode, extra, made, len(drawn), share, worst, refresh, good, errors))
        b.close()
    srv.shutdown()
    lines = ["# Smoothness: the PS5 picture at 60 frames a second", "",
             f"{w}×{h} I420 frames, a new one every 1/60 s, long-polled as from the app's own server; {seconds:.0f} s each; headless Chrome.",
             "Pass: at least 99 % of the frames made drawn, and no gap over two refreshes after the first second.", "",
             "| Picture delay | Over the picture | Frames made | Drawn | Drawn % | Longest gap (ms) | Refresh (ms) | Pass |",
             "|---|---|---|---|---|---|---|---|"]
    for mode, extra, made, n, share, worst, refresh, good, errors in rows:
        name = {"smooth": "Smooth", "low": "Lowest delay"}[mode]
        over = {"plain": "nothing", "phone": "in-place phone layer", "panel": "picture panel open"}[extra]
        lines.append(f"| {name} | {over} | {made} | {n} | {share:.1f} | {worst:.1f} | {refresh:.1f} | {'yes' if good else 'NO'}{' (' + '; '.join(errors)[:120] + ')' if errors else ''} |")
    open(out, "w", encoding="utf-8").write("\n".join(lines) + "\n")
    print("\n".join(lines))
    sys.exit(0 if ok else 1)


if __name__ == "__main__":
    main()
