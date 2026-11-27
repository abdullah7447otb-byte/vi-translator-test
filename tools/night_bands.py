#!/usr/bin/env python3
"""The display shader's night colours, measured (docs/night-colours-1.6.md).

A dark ramp as the PS5 sends it (studio range twice over: luma 30 to 62 over 1024 px) goes through the
app's own picture shader (PICTURE_GLSL in ui/app.js) with the "Dark scenes" look, in headless Chromium.
Checks: the largest step from one pixel to the next (averaged over 8 rows: a band's edge as the eye sees
it) under 2 levels with the default deband, and no black clipped at the ramp's dark end.

    python3 tools/night_bands.py ui        (exits 1 if a check fails)
"""
import json
import re
import sys

from playwright.sync_api import sync_playwright

ui = sys.argv[1] if len(sys.argv) > 1 else "ui"
glsl = re.search(r"const PICTURE_GLSL = `([\s\S]*?)`;", open(f"{ui}/app.js", encoding="utf-8").read()).group(1)
page = """<!doctype html><canvas id=c width=1024 height=64></canvas><script>
const GL = %s;
function run(deband, local) {
  const c = document.getElementById('c'), gl = c.getContext('webgl2', { preserveDrawingBuffer: true });
  const sh = (t, src) => { const x = gl.createShader(t); gl.shaderSource(x, src); gl.compileShader(x); if (!gl.getShaderParameter(x, gl.COMPILE_STATUS)) throw gl.getShaderInfoLog(x); return x; };
  const pr = gl.createProgram();
  gl.attachShader(pr, sh(gl.VERTEX_SHADER, `#version 300 es
  void main() { vec2 p = vec2((gl_VertexID << 1) & 2, gl_VertexID & 2); gl_Position = vec4(p * 2.0 - 1.0, 0.0, 1.0); }`));
  gl.attachShader(pr, sh(gl.FRAGMENT_SHADER, `#version 300 es
  precision highp float; uniform highp sampler2D ty; uniform float yOff, yScale;
  vec3 rgbAt(ivec2 p) { float Y = texelFetch(ty, ivec2(p.x, 0), 0).r * 255.0; return clamp(vec3((Y - yOff) * yScale) / 255.0, 0.0, 1.0); }
  ` + GL));
  gl.linkProgram(pr); if (!gl.getProgramParameter(pr, gl.LINK_STATUS)) throw gl.getProgramInfoLog(pr); gl.useProgram(pr);
  const Y = new Uint8Array(1024); for (let x = 0; x < 1024; x++) Y[x] = Math.round(30 + 32 * x / 1023);
  const t = gl.createTexture(); gl.bindTexture(gl.TEXTURE_2D, t); gl.pixelStorei(gl.UNPACK_ALIGNMENT, 1);
  gl.texImage2D(gl.TEXTURE_2D, 0, gl.R8, 1024, 1, 0, gl.RED, gl.UNSIGNED_BYTE, Y);
  for (const k of [gl.TEXTURE_MIN_FILTER, gl.TEXTURE_MAG_FILTER]) gl.texParameteri(gl.TEXTURE_2D, k, gl.NEAREST);
  const U = (n) => gl.getUniformLocation(pr, n);
  const black = 16 + 16 * 219 / 255, white = 16 + 235 * 219 / 255;
  gl.uniform1f(U('yOff'), black); gl.uniform1f(U('yScale'), 255 / (white - black)); gl.uniform2i(U('pSize'), 1024, 64);
  for (const [n, v] of [['pBright', 0], ['pContrast', 1], ['pGamma', 1.25], ['pShadows', 0.6], ['pSat', 1], ['pSharp', 0], ['pDeband', deband], ['pLocal', local], ['pFrame', 3]]) gl.uniform1f(U(n), v);
  gl.viewport(0, 0, 1024, 64); gl.drawArrays(gl.TRIANGLES, 0, 3);
  const px = new Uint8Array(1024 * 64 * 4); gl.readPixels(0, 0, 1024, 64, gl.RGBA, gl.UNSIGNED_BYTE, px);
  const row = []; for (let x = 0; x < 1024; x++) { let s = 0; for (let y = 28; y < 36; y++) s += px[(y * 1024 + x) * 4]; row.push(s / 8); }
  let maxJump = 0; for (let x = 1; x < 1024; x++) maxJump = Math.max(maxJump, Math.abs(row[x] - row[x - 1]));
  return { maxJump, clipped: row.slice(0, 40).filter((v) => v < 1).length };
}
</script>""" % json.dumps(glsl)
open("/tmp/night_bands.html", "w", encoding="utf-8").write(page)
failed = []
with sync_playwright() as p:
    # (Playwright's own Chromium where it is installed, else the system's Chrome, as phone_render.py)
    args = ["--use-gl=swiftshader", "--enable-unsafe-swiftshader", "--ignore-gpu-blocklist"]
    try:
        b = p.chromium.launch(args=args)
    except Exception:
        b = p.chromium.launch(channel="chrome", args=args)
    for deband, local, limit in [(0.4, 0, 2.0), (1, 0.5, 2.0)]:
        # (a fresh page, so a fresh WebGL context, for each run: Chrome on the macOS runner loses a
        # context used twice)
        pg = b.new_page()
        pg.goto("file:///tmp/night_bands.html")
        r = pg.evaluate(f"run({deband}, {local})")
        pg.close()
        print(f"deband {deband}, detail in the dark {local}: largest step {r['maxJump']:.2f} levels, black clipped {r['clipped']} px")
        if r["maxJump"] >= limit or r["clipped"]:
            failed.append(f"deband {deband}: {r}")
    b.close()
if failed:
    print("\n".join(failed))
    sys.exit(1)
