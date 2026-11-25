#!/usr/bin/env python3
"""A LibreTranslate-compatible stand-in translator, for CI on an Intel Mac (whose build has no
offline translator: ONNX Runtime has no Intel Mac build). It answers with Arabic: the hand-made
translations in tools/mock_translator.json where it has one, else an Arabic placeholder of the same
number of words. What it lets the tests check is the drawing (every message's Arabic in its
bubble, no English left, one size, the tilt), not the translation.

    python3 tools/mock_translator.py [port]
"""
import http.server
import json
import os
import sys
import urllib.parse

HERE = os.path.dirname(os.path.abspath(__file__))
DICT = json.load(open(os.path.join(HERE, "mock_translator.json"), encoding="utf-8"))
WORDS = ["كلمة", "ترجمة", "عربية", "هنا", "تجربة", "نص"]


def tr(t):
    t = t.strip()
    if t in DICT:
        return DICT[t]
    n = max(1, len(t.split()))
    return " ".join(WORDS[i % len(WORDS)] for i in range(n))


class H(http.server.BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def do_GET(self):
        # (/languages: what the app asks to check the server)
        b = json.dumps([{"code": "en", "name": "English"}, {"code": "ar", "name": "Arabic"}]).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(b)))
        self.end_headers()
        self.wfile.write(b)

    def do_POST(self):
        n = int(self.headers.get("Content-Length", 0))
        body = self.rfile.read(n)
        try:
            d = json.loads(body)
        except Exception:
            d = {k: v[0] for k, v in urllib.parse.parse_qs(body.decode()).items()}
        q = d.get("q", "")
        # (what was asked, for the test's log)
        if os.environ.get("VIT_MOCK_LOG"):
            with open(os.environ["VIT_MOCK_LOG"], "a", encoding="utf-8") as f:
                f.write(json.dumps(q, ensure_ascii=False) + "\n")
        out = {"translatedText": [tr(x) for x in q] if isinstance(q, list) else tr(q)}
        b = json.dumps(out, ensure_ascii=False).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(b)))
        self.end_headers()
        self.wfile.write(b)


http.server.ThreadingHTTPServer(("127.0.0.1", int(sys.argv[1]) if len(sys.argv) > 1 else 5000), H).serve_forever()
