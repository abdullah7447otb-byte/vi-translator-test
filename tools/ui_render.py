#!/usr/bin/env python3
"""GTA VI's screen-fixed UI (mission menus, the map and its card, the GPS line) translated by the
app's own snapshot ("translate the whole picture") on CouchSubs' frames, drawn by the app's own
snapshot.js in headless Chrome: before/after PNGs and a report of which of the frame's text blocks
(frames.json `screen.blocks`) were translated.

    python3 tools/ui_render.py <app> <frames dir> <ui dir> <out dir> [--service local|apple|...]
"""
import base64
import http.server
import json
import os
import re
import subprocess
import sys
import threading

FRAMES = ["gta6-menu-gorevler", "gta6-menu-harita", "gta6-harita-kart", "gta6-854-gorev-1", "gta6-854-gorev-2",
          "gta6-854-gorev-3", "gta6-20-gorev", "gta6-13-gps"]


def norm(s):
    return re.sub(r"[^a-z0-9]+", " ", s.lower()).strip()


def main():
    app, frames, ui, out = [os.path.abspath(a) if os.path.exists(a) or i > 0 else a for i, a in enumerate(sys.argv[1:5])]
    if os.path.exists(sys.argv[1]):
        app = os.path.abspath(sys.argv[1])
    service = sys.argv[sys.argv.index("--service") + 1] if "--service" in sys.argv else None
    os.makedirs(out, exist_ok=True)
    meta = {f["id"]: f for f in json.load(open(os.path.join(frames, "frames.json"), encoding="utf-8"))["frames"]}
    snaps = {}
    for fid in FRAMES:
        path = os.path.join(frames, fid + ".jpg")
        if not os.path.exists(path):
            continue
        j = os.path.join(out, fid + ".json")
        cmd = [app, "--test-snapshot", path, "--json", j] + (["--service", service] if service else [])
        r = subprocess.run(cmd, capture_output=True, text=True, timeout=600)
        if r.returncode != 0 or not os.path.exists(j):
            print(f"{fid}: snapshot failed: {(r.stdout + r.stderr)[-400:]}")
            continue
        snaps[fid] = json.load(open(j, encoding="utf-8"))

    # (the app's own drawing: snapshot.js, served with the page's fonts)
    os.chdir(ui)

    class Quiet(http.server.SimpleHTTPRequestHandler):
        def log_message(self, *a):
            pass

    srv = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Quiet)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    port = srv.server_address[1]
    from playwright.sync_api import sync_playwright
    report = ["# UI renders (snapshot of the whole picture)", "",
              "The app's snapshot on each frame (the host's OCR, the chosen translator), drawn by `ui/snapshot.js`. "
              "Blocks: the frame's text blocks in `frames.json` (`screen.blocks`) and whether a translated block covers each.", ""]
    with sync_playwright() as p:
        try:
            b = p.chromium.launch(channel="chrome")
        except Exception:
            b = p.chromium.launch()
        pg = b.new_page()
        pg.goto(f"http://127.0.0.1:{port}/live.html")
        pg.add_script_tag(url=f"http://127.0.0.1:{port}/snapshot.js")
        for fid, snap in snaps.items():
            data = pg.evaluate("async (s) => await Snap.png(s)", snap)
            png = base64.b64decode(data.split(",", 1)[1])
            open(os.path.join(out, fid + "_after.png"), "wb").write(png)
            before = snap["image"]
            open(os.path.join(out, fid + "_before." + ("png" if "image/png" in before[:30] else "jpg")), "wb").write(base64.b64decode(before.split(",", 1)[1]))
            blocks = snap.get("blocks", [])
            report.append(f"## {fid}")
            report.append("")
            report.append(f"{len(blocks)} block(s), {snap.get('ms', 0)} ms, service {snap.get('service', '?')}")
            report.append("")
            for bl in blocks:
                ar = "".join(s[0] for s in bl.get("segs", []))
                report.append(f"- `{bl.get('en', '')}` → {ar}")
            want = (meta.get(fid, {}).get("screen") or {}).get("blocks") or []
            if want:
                seen = " ".join(norm(bl.get("en", "")) for bl in blocks)
                missing = [w for w in want if norm(w) and norm(w) not in seen]
                report.append("")
                report.append(f"Ground truth blocks covered: {len(want) - len(missing)}/{len(want)}" + (f"; missed: {', '.join(missing)}" if missing else ""))
            report.append("")
        b.close()
    srv.shutdown()
    open(os.path.join(out, "ui-renders.md"), "w", encoding="utf-8").write("\n".join(report) + "\n")
    print(f"{len(snaps)} frames rendered")
    if not snaps:
        sys.exit(1)


if __name__ == "__main__":
    main()
