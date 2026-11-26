// pages-extension: 1
// GTA VI's pages, added to the app's own script (core/pages/gta6.js, which stays as it is): run after
// it, in the same engine. Each change replaces one of its functions by name, falling back to it.

// The map's objective card: GTA VI draws its top line in the colour of the mission's character or kind,
// not only Lucia's pink — Jason's blue, a stranger's yellow, green, orange, white. The card is the same box
// whatever the line's colour: tried in the base script's pink first, then in the others.
(function () {
  if (typeof objectiveCard !== "function") return;
  const base = objectiveCard;
  const COLORS = [
    [90, 170, 255], [120, 200, 255], [255, 210, 80], [250, 170, 60], [120, 220, 120], [80, 220, 200],
    [190, 140, 255], [240, 240, 240], [255, 90, 90],
  ];
  function objectiveCardIn(f, color) {
    const zone = pxRect(f, { x: 0.05, y: 0.06, w: 0.36, h: 0.1 });
    // (its top: the picture under it may be pink too; the line itself measured in a thin band there)
    const any = f.pixels(zone, { step: 1, near: color, tol: 55 });
    if (!any.box) return null;
    const line = f.pixels({ x: zone.x, y: any.box.y, w: zone.w, h: Math.ceil(f.h * 0.008) }, { step: 1, near: color, tol: 55, median: true });
    if (!line.box || line.box.w < f.w * 0.15) return null;
    const x0 = line.box.x, x1 = line.box.x + line.box.w, top = line.box.y + line.box.h;
    const segs = segments(f, { x: x0 + 2, y: top + 2, w: x1 - x0 - 4, h: Math.floor(f.h * 0.2) }, { scale: 2 }, 3)
      .filter((s) => s.words.some((w) => /\p{L}{2}/u.test(w.text)));
    if (!segs.length) return null;
    // the lines that belong to it: each a line's pitch under the one before (the map's own names further down don't)
    const lines = [segs[0]];
    for (const s of segs.slice(1)) {
      const prev = lines[lines.length - 1];
      if (s.y0 - prev.y1 > prev.h * 1.4) break;
      lines.push(s);
    }
    const words = (s) => s.words.map((w) => w.text).join(" ");
    // (the picture before the name reads as a stray letter, "n COMING HOME": the name's own words only)
    const tw = lines[0].words.filter((w) => /\p{L}{2}/u.test(w.text));
    if (!tw.length) return null;
    const title = { words: tw, y0: Math.min(...tw.map((w) => w.y)), y1: Math.max(...tw.map((w) => w.y + w.h)) };
    title.h = title.y1 - title.y0;
    const body = lines.slice(1);
    const tx = Math.min(...tw.map((w) => w.x));
    const bottom = Math.max(...lines.map((s) => s.y1)) + Math.round(title.h * 0.9);
    // the character's picture: left of the name, about as tall as two of its lines
    const s = Math.round((tx - x0) * 0.66), py = Math.round((title.y0 + title.y1) / 2 - s / 2);
    const pic = s >= 8 ? f.icon({ x: x0 + Math.round((tx - x0) * 0.24), y: py, w: s, h: s }, 0) : null;
    const hs = body.map((b) => b.h).sort((a, b) => a - b);
    return {
      rect: f.box(x0, line.box.y, x1 - x0, bottom - line.box.y),
      bg: "rgba(14, 16, 30, 0.8)",
      border: hex(line.median || { r: color[0], g: color[1], b: color[2] }),
      border_h: line.box.h / f.h,
      pic: pic || "",
      title_en: words(title),
      title_h: title.h / f.h,
      text_en: body.map(words).join(" "),
      text_h: (hs.length ? hs[Math.floor(hs.length / 2)] : title.h) / f.h,
    };
  }

  globalThis.objectiveCard = function (f) {
    const pink = base(f);
    if (pink) return pink;
    for (const c of COLORS) {
      const card = objectiveCardIn(f, c);
      if (card) return card;
    }
    return null;
  };
})();
