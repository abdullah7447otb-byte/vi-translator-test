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
    [190, 140, 255], [255, 90, 90],
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
    // (a downloaded base script that renamed a helper: the base's card only, the map page unharmed)
    try {
      for (const c of COLORS) {
        const card = objectiveCardIn(f, c);
        if (card) return card;
      }
    } catch (e) {
      return null;
    }
    return null;
  };
})();

// ------------------------------------------------------------------ a scanned car's card
// Scanning a car with the phone's WAiNK app (Moses' garage) puts its card bottom right: a dark navy band with the
// make and model ("VAPID / MONTAGNE XL"), then rows of a name and a value ("ACCESS — LOCKED 🔑", "SECURITY —
// ALARMED", "TRACKER — NONE", "SELL VALUE — $550", "COST TO REGISTER — $3,400"). The names and the words among
// the values are written over the English, in their places; the make, the model and the prices stay. A card
// beside the game: the subtitles go on under it (`beside`).
(function () {
  const CAR_WORDS = {
    ar: {
      "ACCESS": "الوصول", "SECURITY": "الحماية", "TRACKER": "جهاز التتبّع", "SELL VALUE": "قيمة البيع",
      "COST TO REGISTER": "رسوم التسجيل", "CONDITION": "الحالة", "VALUE": "القيمة", "OWNER": "المالك",
      "LOCKED": "مقفلة", "UNLOCKED": "غير مقفلة", "ALARMED": "بإنذار", "NO ALARM": "بلا إنذار", "NONE": "لا يوجد",
      "FITTED": "مركّب", "YES": "نعم", "NO": "لا", "STOLEN": "مسروقة", "REGISTERED": "مسجّلة", "UNREGISTERED": "غير مسجّلة",
      "DAMAGED": "متضررة", "GOOD": "جيدة", "POOR": "سيئة", "EXCELLENT": "ممتازة", "UNKNOWN": "غير معروف",
    },
  };
  const NAMES = ["ACCESS", "SECURITY", "TRACKER", "SELL VALUE", "COST TO REGISTER", "CONDITION", "VALUE", "OWNER"];
  const word = (en) => {
    const t = CAR_WORDS[vi.lang()] || {};
    // (the condensed capitals read with digits or a stray letter now and then: "L0CKED", "NONE.")
    const k = en.toUpperCase().replace(/0/g, "O").replace(/[^A-Z ]/g, "").replace(/\s+/g, " ").trim();
    return t[k] || "";
  };

  // the navy band: dark, a little blue, a band of rows tall, with white letters in it
  function carCard(f) {
    const x0 = Math.round(f.w * 0.772), sw = Math.round(f.w * 0.08);
    const navy = (y) => {
      const m = f.median({ x: x0, y, w: sw, h: 2 }, { step_x: 3, step_y: 1 });
      return !!m && Math.max(m.r, m.g, m.b) < 48 && m.b - m.r >= 4;
    };
    const step = 2, want = Math.round(f.h * 0.045);
    let run = 0;
    for (let y = Math.round(f.h * 0.3); y < Math.round(f.h * 0.86); y += step) {
      if (navy(y)) {
        run += step;
        continue;
      }
      // (a band of one row's height: a night sky or the map's sea is far taller)
      if (run >= want && run <= f.h * 0.09) {
        const top = y - run;
        const head = { x: x0 - Math.round(f.w * 0.008), y: top, w: Math.round(f.w * 0.2), h: run };
        const ink = f.pixels(head, { step: 2, bright: [190, 255], spread: [0, 40] });
        // (and white names under it, at its left: the rows)
        const rows = f.pixels({ x: head.x, y: y + 4, w: Math.round(f.w * 0.06), h: Math.round(f.h * 0.2) }, { step: 2, bright: [190, 255], spread: [0, 40] });
        // (the rows a lighter grey than the band: a dark scene under a dark band is not the card)
        const band = f.median(head, { step_x: 3, step_y: 2 });
        const under = f.median({ x: x0, y: y + Math.round(f.h * 0.02), w: sw, h: Math.round(f.h * 0.1) }, { step_x: 3, step_y: 2 });
        const lighter = !!band && !!under && under.r + under.g + under.b >= band.r + band.g + band.b + 45;
        if (lighter && ink.count >= 20 && rows.count >= 30) return { x: head.x, top, band: y, right: Math.min(Math.round(f.w * 0.995), f.w - 1) };
      }
      run = 0;
    }
    return null;
  }

  pages.add({
    id: "gta6_car_card",
    option: "game_notices",
    beside: true,
    present(f) {
      return carCard(f);
    },
    read(f, k) {
      if (!k) return null;
      const area = { x: k.x, y: k.band, w: k.right - k.x, h: Math.min(Math.round(f.h * 0.34), f.h - k.band - 2) };
      const segs = segments(f, area, { scale: 2 }, 2.5);
      const menu = [];
      let names = 0;
      const mid = k.x + (k.right - k.x) * 0.5;
      for (const s of segs) {
        const en = s.words.map((w) => w.text).join(" ").trim();
        const ar = word(en);
        if (!ar) continue;
        const isName = s.x0 < mid && NAMES.includes(en.toUpperCase().replace(/0/g, "O").replace(/[^A-Z ]/g, "").replace(/\s+/g, " ").trim());
        if (isName) names++;
        const h = s.y1 - s.y0, pad = Math.round(h * 0.35);
        // a name's box reaches to the card's middle (a longer word has room); a value's covers its word only
        const x0 = s.x0 - pad, x1 = isName ? Math.max(s.x1 + pad, Math.round(mid)) : s.x1 + pad;
        const box = { x: x0, y: s.y0 - pad, w: x1 - x0, h: h + 2 * pad };
        const bg = f.median(box, { step_x: 2, step_y: 2, exclude: [s.x0 - 1, s.y0 - 1, s.x1 + 1, s.y1 + 1] });
        if (!bg) continue;
        menu.push({
          en, ar, fixed: true, icons: [], lines: 1,
          rect: f.box(box.x, box.y, box.w, box.h), bg: hex(bg), fg: "#ffffff",
          line_h: h / f.h, size: 1.25, font: "title", weight: 700, align: isName ? "left" : "right", wrap: "no",
          item: false, right: !isName, missed: 0,
        });
      }
      // (two of its names at least: a car's card, not words that happen to be on a dark band)
      if (names < 2) return null;
      this.at = k.band;
      const buttons = readButtons(f);
      return { id: "gta6_car_card", title: { x: 0, y: 0, w: 0, h: 0 }, menu, buttons, buttons_row: buttonsRow(buttons) };
    },
    // still up: the band where it was (another car's card is read again)
    same(f) {
      const k = carCard(f);
      return !!k && Math.abs(k.band - this.at) <= 4;
    },
  });
})();
