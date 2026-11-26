#!/usr/bin/env python3
"""The phone's Arabic drawn over real GTA VI frames, the way the app draws it, and checked.

    python3 tools/phone_render.py <probe.json> <ui dir> <out dir> [--chrome PATH] [--app PATH]

<probe.json> comes from `vi-translator --probe-phone` (the app's own reading of each frame). Each frame is
loaded in headless Chrome under the app's own drawing code (ui/common.js: AS.renderPhone, the Arabic inside
the game's phone), saved as <name>-after.png next to <name>-before.png, and checked:

- every translated piece is drawn, inside its own box (no Arabic runs out of it: overflow 0 px);
- one text size for the chat's bubbles (±1 px), except a bubble that had to take a smaller one (reported);
- on a tilted phone, the drawn layer is turned as the phone is (±2°);
- with --app: the app's OCR reads the drawn bubbles again (`--ocr-boxes`): no English word is left in them,
  except names kept as names.

Writes <out dir>/report.json and exits 1 if a check fails.
"""

import argparse
import base64
import json
import math
import os
import subprocess
import sys

from playwright.sync_api import sync_playwright

# words that stay English on purpose: names, app names, the game's own brand words
KEEP = {"donnie", "lucia", "jason", "whatup", "vc", "kingfishers", "cal"}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("probe")
    ap.add_argument("ui")
    ap.add_argument("out")
    ap.add_argument("--chrome", default=None)
    ap.add_argument("--app", default=None)
    a = ap.parse_args()
    os.makedirs(a.out, exist_ok=True)
    probe = json.load(open(a.probe, encoding="utf-8"))
    ui = os.path.abspath(a.ui)
    report = {}
    failed = []
    with sync_playwright() as p:
        kw = {"executable_path": a.chrome} if a.chrome else {"channel": "chrome"}
        browser = p.chromium.launch(**kw)
        for name, item in probe.items():
            phone = item.get("phone")
            w, h = item["w"], item["h"]
            img = open(item["file"], "rb").read()
            src = "data:image/jpeg;base64," + base64.b64encode(img).decode()
            with open(os.path.join(a.out, f"{name}-before.png"), "wb") as f:
                pass
            page = browser.new_page(viewport={"width": w, "height": h})
            html = f"""<!doctype html><html><head><meta charset="utf-8">
<link rel="stylesheet" href="file://{ui}/fonts.css">
<style>html,body{{margin:0;background:#000}}#root{{position:relative;width:{w}px;height:{h}px;overflow:hidden}}
#bg{{position:absolute;left:0;top:0;width:{w}px;height:{h}px}}</style></head>
<body><div id="root"><img id="bg" src="{src}"><div id="ph" style="position:absolute;left:0;top:0;width:{w}px;height:{h}px"></div></div>
<script src="file://{ui}/common.js"></script></body></html>"""
            path = os.path.join(a.out, f"{name}.html")
            open(path, "w", encoding="utf-8").write(html)
            page.goto("file://" + os.path.abspath(path))
            page.wait_for_timeout(300)
            page.screenshot(path=os.path.join(a.out, f"{name}-before.png"))
            res = page.evaluate(
                """([phone, anchor]) => new Promise((done) => {
                  const AS = window.AS;
                  AS.setLang("ar");
                  const ph = document.getElementById("ph");
                  if (anchor) AS.phoneAnchor(anchor);
                  // (both: the in-place layer is checked as before, and the steady panel beside it)
                  AS.renderPhone(ph, phone ? Object.assign({}, phone, { display: "both" }) : phone);
                  document.fonts.ready.then(() => requestAnimationFrame(() => requestAnimationFrame(() => {
                    const layer = ph.querySelector(".phone-inplace");
                    const boxes = [];
                    const out = { pieces: 0, drawn: 0, overflow: [], sizes: [], angle: null, boxes: [], chat: !!ph.firstElementChild && !layer };
                    if (layer) {
                      // (the turn of the layer's middle line as drawn: perspective skews the matrix's own terms)
                      const m = new DOMMatrixReadOnly(getComputedStyle(layer).transform);
                      const fh = parseFloat(layer.style.height);
                      const a0 = m.transformPoint(new DOMPoint(0, fh / 2)), a1 = m.transformPoint(new DOMPoint(360, fh / 2));
                      out.angle = Math.atan2(a1.y / a1.w - a0.y / a0.w, a1.x / a1.w - a0.x / a0.w) * 180 / Math.PI;
                      out.base = parseFloat(layer.dataset.fs);
                      for (const d of layer.children) {
                        out.drawn++;
                        const over = Math.max(0, d.scrollHeight - d.clientHeight, d.scrollWidth - d.clientWidth);
                        if (over > 1) out.overflow.push([d.textContent, over]);
                        out.sizes.push(parseFloat(getComputedStyle(d).fontSize));
                        const r = d.getBoundingClientRect();
                        out.boxes.push([r.left / innerWidth, r.top / innerHeight, r.width / innerWidth, r.height / innerHeight, d.textContent]);
                      }
                    }
                    const panel = ph.querySelector(".phone-panel");
                    if (panel) {
                      const r = panel.getBoundingClientRect();
                      out.panel = { rect: [r.left / innerWidth, r.top / innerHeight, r.width / innerWidth, r.height / innerHeight], bubbles: panel.children.length };
                    }
                    out.pieces = phone && phone.pieces ? phone.pieces.filter((p) => !p.title && !p.pending && p.ar && p.ar.trim()).length : 0;
                    done(out);
                  })));
                })""",
                [phone, item.get("anchor")],
            )
            page.screenshot(path=os.path.join(a.out, f"{name}-after.png"))
            page.close()
            checks = []
            if phone and phone.get("pieces"):
                # the steady panel: there, and beside the phone's screen, not over it
                if res["pieces"]:
                    pn = res.get("panel")
                    if not pn:
                        checks.append("no steady panel beside the phone")
                    elif phone.get("screen"):
                        sx, sy, sw, sh = phone["screen"]
                        px, py, pw, phh = pn["rect"]
                        if px < sx + sw - 0.005 and px + pw > sx + 0.005 and py < sy + sh and py + phh > sy:
                            checks.append(f"the panel covers the phone's screen: {pn['rect']} over {phone['screen']}")
                if res["drawn"] != res["pieces"]:
                    checks.append(f"drawn {res['drawn']} of {res['pieces']} translated pieces")
                if res["overflow"]:
                    checks.append(f"Arabic runs out of its box: {res['overflow']}")
                base = res.get("base")
                odd = [s for s in res["sizes"] if base and abs(s - base) > 1]
                res["smaller"] = len(odd)
                turn = phone.get("turn") or 0
                if res["angle"] is not None and abs(turn) >= 1 and abs(res["angle"] - turn) > 2:
                    checks.append(f"layer turned {res['angle']:.1f}°, the phone {turn:.1f}°")
                if a.app and res["boxes"]:
                    bj = os.path.join(a.out, f"{name}-boxes.json")
                    json.dump([b[:4] for b in res["boxes"]], open(bj, "w"))
                    r = subprocess.run([a.app, "--ocr-boxes", os.path.join(a.out, f"{name}-after.png"), bj], capture_output=True, text=True)
                    try:
                        words = json.loads(r.stdout.strip().splitlines()[-1])
                    except Exception:
                        words = None
                        checks.append(f"ocr-boxes failed: {r.stderr[-300:]}")
                    if words is not None:
                        # (only words of the English the bubble covered count: the OCR reads the Arabic letters as
                        # Latin nonsense, which is no English left)
                        drawn = [pc for pc in phone["pieces"] if not pc.get("title") and not pc.get("pending") and (pc.get("ar") or "").strip()]
                        def norm(t):
                            return "".join(c for c in t.lower() if c.isalnum())
                        left = []
                        for i, ws in enumerate(words):
                            en = {norm(x) for x in (drawn[i].get("en", "") if i < len(drawn) else "").split() if len(norm(x)) >= 3}
                            left.append([x for x in ws if norm(x) in en and norm(x) not in KEEP])
                        res["english_left"] = left
                        if any(left):
                            checks.append(f"English left in the bubbles: {left}")
            res["checks"] = checks
            report[name] = res
            if checks:
                failed.append(f"{name}: " + "; ".join(checks))
            print(f"{name}: drawn {res['drawn']}/{res['pieces']}, angle {res['angle']}, sizes {res['sizes']}, " + ("OK" if not checks else "FAIL " + "; ".join(checks)))
        browser.close()
    json.dump(report, open(os.path.join(a.out, "report.json"), "w", encoding="utf-8"), ensure_ascii=False, indent=1)
    if failed:
        print("\n".join(failed))
        sys.exit(1)


if __name__ == "__main__":
    main()
