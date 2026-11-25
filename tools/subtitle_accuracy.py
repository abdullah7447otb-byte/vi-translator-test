#!/usr/bin/env python3
"""The subtitle reader on CouchSubs' GTA VI frames against their ground truth (frames.json).

    python3 tools/subtitle_accuracy.py <app> <frames dir> <out.md>

The app's own reading (`--read-lines`, the subtitles and instructions areas, as the running app
reads them, with the host's OCR: Vision on a Mac, Windows OCR on Windows) on each `gta6-NN-*`
subtitle frame; each frame's words against its `subtitle`. Word accuracy is the share of the
ground truth's words read (in any order, each once), and every miss is listed. Exits 1 when the
reading failed to run at all.
"""
import collections
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile


def words(s):
    s = s.lower().replace("’", "'").replace("…", " ")
    return [w for w in re.split(r"[^a-z0-9']+", s) if w and w != "'"]


def read(app, d, area):
    out = subprocess.run([app, "--read-lines", d, "--area", area], capture_output=True, text=True, timeout=900)
    got, name = {}, None
    for line in (out.stdout + out.stderr).splitlines():
        m = re.match(r"^(\S+\.(?:png|jpg)) \(\d+ ms\)", line)
        if m:
            name = m.group(1).rsplit(".", 1)[0]
            continue
        m = re.match(r'^\s+=> "(.*)"$', line)
        if m and name:
            got[name] = m.group(1).replace("\\\"", "\"")
    return got, out.returncode


def main():
    app, frames, out = sys.argv[1], sys.argv[2], sys.argv[3]
    meta = json.load(open(os.path.join(frames, "frames.json"), encoding="utf-8"))["frames"]
    picked = [f for f in meta if re.match(r"^gta6-\d\d-", f["id"]) and f.get("subtitle")]
    extra = [f for f in meta if re.match(r"^gta6-8\d\d-", f["id"]) and f.get("subtitle")]
    d = tempfile.mkdtemp(prefix="vit-acc-")
    for f in picked + extra:
        shutil.copy(os.path.join(frames, f["file"]), os.path.join(d, f["file"]))
    subs, rc1 = read(app, d, "subtitles")
    inst, rc2 = read(app, d, "instructions")
    if rc1 != 0 and not subs:
        print("the reading didn't run", file=sys.stderr)
        sys.exit(1)

    def score(group):
        rows, total, hit = [], 0, 0
        for f in group:
            truth = words(f["subtitle"])
            text = " ".join(t for t in [subs.get(f["id"], ""), inst.get(f["id"], "")] if t)
            have = collections.Counter(words(text))
            missed = []
            n = 0
            for w in truth:
                if have[w] > 0:
                    have[w] -= 1
                    n += 1
                else:
                    missed.append(w)
            total += len(truth)
            hit += n
            extra_words = [w for w, c in have.items() for _ in range(c)]
            rows.append((f["id"], f["subtitle"], text, n, len(truth), missed, extra_words))
        return rows, total, hit

    main_rows, total, hit = score(picked)
    x_rows, x_total, x_hit = score(extra)
    host = {"darwin": "Apple Vision (macOS)", "win32": "Windows OCR"}.get(sys.platform, "Tesseract")
    lines = ["# Subtitle reading accuracy", "",
             f"OCR: {host}. Frames: CouchSubs' `gta6-NN-*` subtitle frames ({len(picked)}), ground truth `frames.json` (`subtitle`).",
             "The app's own reading of the subtitles and instructions areas (`--read-lines`), words compared in any order.", "",
             f"**Word accuracy: {hit}/{total} = {100 * hit / max(total, 1):.1f} %**", "",
             "| Frame | Read | Words | Missed | Extra |", "|---|---|---|---|---|"]
    for fid, truth, text, n, t, missed, ex in main_rows:
        lines.append(f"| {fid} | {text.replace('|', '/') or '—'} | {n}/{t} | {' '.join(missed) or '—'} | {' '.join(ex) or '—'} |")
    lines += ["", f"Extra frames (`gta6-8NN-*`, a later set): {x_hit}/{x_total} = {100 * x_hit / max(x_total, 1):.1f} %", "",
              "| Frame | Truth | Read | Words | Missed |", "|---|---|---|---|---|"]
    for fid, truth, text, n, t, missed, ex in x_rows:
        lines.append(f"| {fid} | {truth} | {text.replace('|', '/') or '—'} | {n}/{t} | {' '.join(missed) or '—'} |")
    open(out, "w", encoding="utf-8").write("\n".join(lines) + "\n")
    print(lines[5])


if __name__ == "__main__":
    main()
