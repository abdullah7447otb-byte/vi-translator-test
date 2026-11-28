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

// ------------------------------------------------------------------ the menu's own words, exactly
// The base script's word list is looked up by letters only ("Game" = "GAME /", "On" = "ON"), so when two of
// its entries share their letters the last one wins: the settings' "On" / "Off" read as the items wheel's
// worn / not worn (مرتدى / منزوع), "Game" kept the slash of "GAME /", "Save Game" the question mark of
// "SAVE GAME?". Here the entry written most like what was read wins: the very same text, else the same
// punctuation and the same capitals. And a heading in capitals with one letter misread ("CAME", "JAME") is
// the one word of its length it can be, when only one is.
(function () {
  if (typeof ownWord !== "function" || typeof WORDS !== "object") return;
  const base = ownWord;
  const BY_KEY = {};
  const index = (lang) => {
    if (BY_KEY[lang]) return BY_KEY[lang];
    const m = {};
    for (const src of [(typeof HELP_TOPICS === "object" && HELP_TOPICS[lang]) || {}, WORDS[lang] || {}]) {
      for (const [en, t] of Object.entries(src)) (m[wordKey(en)] = m[wordKey(en)] || []).push([en, t]);
    }
    return (BY_KEY[lang] = m);
  };
  const marks = (s) => String(s).replace(/[\p{L}\p{N}\s]/gu, "");
  const shape = (s) => (/\p{Ll}/u.test(s) ? "mixed" : "caps");
  // one letter apart, same length
  const oneOff = (a, b) => {
    if (a.length !== b.length) return false;
    let d = 0;
    for (let i = 0; i < a.length && d < 2; i++) if (a[i] !== b[i]) d++;
    return d === 1;
  };
  // (built now, while the script loads with time to spare: built in a page's first read, the word lists
  // took that read past its time limit now and then, and the first page opened showed nothing)
  try {
    index(vi.lang());
    base("MAP");
  } catch (e) {
    // (built on first use instead)
  }
  globalThis.ownWord = function (en, exact) {
    try {
      const lang = vi.lang();
      const s = String(en).trim();
      const list = index(lang)[wordKey(s)] || [];
      if (list.length > 1) {
        const same = list.find(([e]) => e === s) ||
          list.find(([e]) => marks(e) === marks(s) && shape(e) === shape(s)) ||
          list.find(([e]) => marks(e) === marks(s));
        if (same) return same[1];
      }
      const got = base(en, exact);
      if (got || exact) return got;
      // (a place or a name the names dictionary has: the map's "KELLY COUNTY", "Gulf of Leonida")
      const named = typeof vi.name === "function" ? vi.name(s) : "";
      if (named) return named;
      // (a capitals heading, 3–6 letters, one misread: "CAME" for "GAME")
      if (/^\p{Lu}{3,6}$/u.test(s)) {
        const k = wordKey(s);
        const near = Object.entries(index(lang)).filter(([key, l]) => oneOff(key, k) && l.some(([e]) => shape(e) === "caps"));
        if (near.length === 1) {
          const w = near[0][1].find(([e]) => marks(e) === "" && shape(e) === "caps") || near[0][1][0];
          return w[1];
        }
      }
      return got;
    } catch (e) {
      return base(en, exact);
    }
  };
})();

// ------------------------------------------------------------------ the selected row, settled
// The pause menu's read waits while its white selection bar slides between rows: a row's middle must be
// within a fifth of the bar's height of the bar's middle. Apple's Vision boxes a line a little higher or
// lower than the letters, so on the Mac a settled bar was taken for a sliding one on the first page and
// on Help (nothing translated there). A row the bar covers for most of its height has the bar's middle.
(function () {
  const names = ["readHome", "readPlain", "readTabsPage", "readHub", "readDialog"];
  if (typeof selectionBar !== "function") return;
  const settle = (f, r) => {
    if (!r || !Array.isArray(r.menu)) return r;
    const bar = selectionBar(f);
    if (!bar) return r;
    const mid = bar.y + bar.h / 2;
    for (const m of r.menu) {
      if (!m || !m.rect || m.rect.x >= 0.4) continue;
      const y0 = m.rect.y * f.h, y1 = (m.rect.y + m.rect.h) * f.h, h = y1 - y0;
      const over = Math.min(y1, bar.y + bar.h) - Math.max(y0, bar.y);
      if (h > 0 && over >= h * 0.6) m.mid = mid;
      else if (typeof m.mid !== "number") m.mid = (y0 + y1) / 2;
    }
    return r;
  };
  for (const n of names) {
    const base = globalThis[n];
    if (typeof base !== "function") continue;
    globalThis[n] = function () {
      const r = base.apply(this, arguments);
      try {
        return settle(arguments[0], r);
      } catch (e) {
        return r;
      }
    };
  }
})();

// ------------------------------------------------------------------ the hub's selected tab
// On the hub (Map, Brief, Progress, Jason & Lucia) the open tab is dark letters on pink, and OCR often misses
// it ("RIEF" or nothing): the tab row then has no selected tab. It is named by the tabs either side, as the
// Settings and Help pages name theirs, and drawn on the pink box.
(function () {
  if (typeof readHub !== "function" || typeof byNeighbours !== "function") return;
  const base = readHub;
  globalThis.readHub = function (f, k) {
    const r = base(f, k);
    try {
      const pink = k && k.pink;
      if (!r || !pink || !Array.isArray(r.tabs) || r.tabs.some((t) => t.selected)) return r;
      // (a misread of the open tab under the pink: dropped, named again below)
      const tabs = r.tabs.filter((t) => !(t.rect && Math.abs((t.rect.x + t.rect.w / 2) * f.w - (pink.x + pink.w / 2)) < pink.w * 0.6));
      const xs = tabs.map((t) => (t.rect.x + t.rect.w / 2) * f.w);
      const order = tabs.map((t, i) => [t, xs[i]]).sort((a, b) => a[1] - b[1]);
      const i = order.filter(([, x]) => x < pink.x + pink.w / 2).length;
      const names = order.map(([t]) => t.en);
      names.splice(i, 0, null);
      const en = byNeighbours(names, i, TAB_ROWS);
      if (!en) return r;
      const bg = f.median(pink, { step_x: 2, step_y: 2 }) || { r: 255, g: 255, b: 255 };
      const fg = f.contrast(pink, bg, 0.1) || { r: 0, g: 0, b: 0 };
      const sel = { en, ar: ownWord(en), rect: f.box(pink.x, pink.y, pink.w, pink.h), bg: hex(bg), fg: hex(fg), selected: true };
      const out = order.map(([t]) => t);
      out.splice(i, 0, sel);
      return Object.assign({}, r, { tabs: out });
    } catch (e) {
      return r;
    }
  };
})();

// ------------------------------------------------------------------ a tab row read as one word
// A reader can give a whole row of tabs as one word or one box ("CONTROL SCHEMECONTROLLERGAMEPLAY…",
// "JASON & LUCIA MAP BRIEF PROGRESS" in one place): the row then has one "tab" and the page is dropped.
// When a piece of the row spells two or more tabs of a known row in order, the tabs are found again by
// their ink: the row's bright shapes, grouped until there are as many groups as names, each group a tab.
(function () {
  if (typeof readTabs !== "function" || typeof TAB_ROWS === "undefined") return;
  const base = readTabs;
  const caps = (s) => String(s).toUpperCase().replace(/[^A-Z0-9]/g, "");
  // the tab names a run of letters spells, in a known row's order: between two names up to two stray
  // letters (an icon read as "G", "NO"), a name's first or last letter may be lost ("RIEF"), and what's left
  // at the end is a name cut by the fade ("ACCO") or an icon ("RI")
  function spell(text) {
    const s = caps(text);
    let best = null;
    const at = (n, pos) => {
      for (let skip = 0; skip <= 2; skip++) {
        const p = pos + skip;
        if (s.startsWith(n, p)) return p + n.length;
        if (n.length >= 4 && s.startsWith(n.slice(1), p)) return p + n.length - 1;
        if (n.length >= 4 && s.startsWith(n.slice(0, -1), p)) return p + n.length - 1;
      }
      return -1;
    };
    for (const row of TAB_ROWS) {
      for (let start = 0; start < row.length; start++) {
        let pos = 0;
        const got = [];
        let j = start;
        for (; j < row.length && pos < s.length; j++) {
          const end = at(caps(row[j]), pos);
          if (end < 0) break;
          got.push(row[j]);
          pos = end;
        }
        const left = s.slice(pos);
        const cut = j < row.length && left.length >= 2 && caps(row[j]).startsWith(left.slice(0, Math.min(left.length, 3)));
        if (got.length >= 2 && (left.length <= 4 || cut) && (!best || got.length > best.length)) best = got;
      }
    }
    return best;
  }
  // The row's shapes matched to the names, left to right: each name takes one to four shapes side by side
  // (its words), a shape no name was read from (an icon, a tab cut by the fade) is left out; the match whose
  // widths best fit the names' letters wins (one letter's width from all the shapes and all the letters).
  // None when even the best fits badly: then the reader's own tabs stand.
  function place(blobs, names) {
    const n = names.length, m = blobs.length;
    if (!n || m < n) return null;
    const chars = names.map((t) => caps(t).length + (String(t).trim().split(/\s+/).length - 1));
    const ink = blobs.reduce((a, b) => a + b.w, 0);
    const letter = ink / chars.reduce((a, b) => a + b, 0);
    const SKIP = 0.5, GROUP = 4;
    const cost = Array.from({ length: n + 1 }, () => new Array(m + 1).fill(Infinity));
    const from = Array.from({ length: n + 1 }, () => new Array(m + 1).fill(null));
    cost[0][0] = 0;
    for (let jj = 1; jj <= m; jj++) { cost[0][jj] = cost[0][jj - 1] + SKIP; from[0][jj] = [0, jj - 1]; }
    for (let ii = 1; ii <= n; ii++) {
      for (let jj = 1; jj <= m; jj++) {
        let best = [cost[ii][jj - 1] + SKIP, [ii, jj - 1]];
        for (let k = 1; k <= GROUP && k <= jj; k++) {
          const a = blobs[jj - k], b = blobs[jj - 1];
          const w = b.x + b.w - a.x;
          const c = cost[ii - 1][jj - k] + Math.abs(Math.log(Math.max(w, 1) / Math.max(chars[ii - 1] * letter, 1)));
          if (c < best[0]) best = [c, [ii - 1, jj - k]];
        }
        cost[ii][jj] = best[0];
        from[ii][jj] = best[1];
      }
    }
    if (cost[n][m] / n > 0.45) return null;
    const out = new Array(n);
    let ii = n, jj = m;
    while (ii > 0) {
      const [pi, pj] = from[ii][jj];
      if (pi < ii) out[pi] = { x0: blobs[pj].x, x1: blobs[jj - 1].x + blobs[jj - 1].w };
      ii = pi;
      jj = pj;
    }
    return out;
  }
  globalThis.readTabs = function (f, area, tabBox, selected, names, minH, own, fades) {
    const labels = base.apply(this, arguments);
    try {
      // (only the hub's row, four tabs far apart: the Settings and Help rows, boxed tabs side by side with
      // an icon at each end and a tab cut by the fade, Vision reads as tabs of their own, and a match by
      // widths there put a name one box off now and then)
      if (labels.length >= 2 || own || fades) return labels;
      for (const s of segments(f, area, { scale: 2 }, 0.9)) {
        const want = spell(s.words.map((w) => w.text).join(" "));
        if (!want) continue;
        // the bright letters (and the open tab's light box) on the row's text lines, not its underline
        // (the middle of the text's height only: the open tab's box edges and the row's rule are above and below it)
        const cy = (s.y0 + s.y1) / 2, half = Math.max(Math.round(s.h * 0.25), 2);
        const bx0 = Math.max(area.x, s.x0 - Math.round(s.h * 2)), bx1 = Math.min(area.x + area.w, s.x1 + Math.round(s.h * 2));
        const band = { x: bx0, y: Math.round(cy - half), w: bx1 - bx0, h: 2 * half };
        const blobs = f.blobs(band, { gap: Math.max(2, Math.round(s.h * 0.2)), bright: [150, 255], spread: [0, 60] })
          .filter((b) => b.h >= half * 0.6);
        // (the L1 / R1 button icons at the row's ends: a square-ish shape set apart from the rest)
        const icon = (b, gap) => b.w <= s.h * 1.3 && gap > s.h * 0.5;
        let trimmed = blobs.slice();
        if (trimmed.length > want.length && icon(trimmed[0], trimmed[1] ? trimmed[1].x - (trimmed[0].x + trimmed[0].w) : 0)) trimmed = trimmed.slice(1);
        const last = trimmed.length - 1;
        if (trimmed.length > want.length && icon(trimmed[last], trimmed[last - 1] ? trimmed[last].x - (trimmed[last - 1].x + trimmed[last - 1].w) : 0)) trimmed = trimmed.slice(0, -1);
        // (the open tab's light box, when the page found it, is one shape: the open tab)
        let shapes = trimmed;
        if (own) {
          const inPill = (b) => b.x + b.w > own.x && b.x < own.x + own.w;
          shapes = trimmed.filter((b) => !inPill(b));
          shapes.push({ x: own.x, y: own.y, w: own.w, h: own.h, pill: true });
          shapes.sort((a, b) => a.x - b.x);
        }
        const g = place(shapes, want);
        if (!g) continue;
        // (with the open tab's box known, the match must give it one name, about its width)
        if (own) {
          const hit = g.filter((t) => t && t.x0 <= own.x + 2 && t.x1 >= own.x + own.w - 2);
          if (hit.length !== 1 || hit[0].x1 - hit[0].x0 > own.w * 1.15) continue;
        }
        const out = [];
        want.forEach((en, i) => {
          if (!g[i]) return;
          const { x0, x1 } = g[i];
          const y0 = s.y0, y1 = s.y1;
          if (y1 - y0 < minH) return;
          const box = tabBox(x0, y0, x1, y1);
          const bg = f.median(box, { step_x: 2, step_y: 2, exclude: [x0 - 2, y0 - 2, x1 + 2, y1 + 2] });
          if (!bg) return;
          const fg = f.contrast({ x: x0, y: y0, w: x1 - x0, h: y1 - y0 }, bg, 0.15) || { r: 255, g: 255, b: 255 };
          const isSel = selected({ x: x0, y: y0, w: x1 - x0, h: y1 - y0 });
          out.push({ en, ar: ownWord(en), rect: f.box(box.x, box.y, box.w, box.h), bg: hex(bg), fg: hex(fg), selected: isSel });
        });
        if (out.length >= 2) return out;
      }
    } catch (e) {
      // (the reader's own tabs, whatever they are)
    }
    return labels;
  };
})();

// ------------------------------------------------------------------ the actions, bottom right
// GTA VI's actions in the bottom-right corner, one under another, each with its button on its right
// ("HEAD OUT 👥 △", "● LUCIA (L2)"): two, three, four of them at times. No read area covered that corner (the
// subtitles' is centred, the prompts' ends above it), so they were never translated. Each line is its own
// action: read, translated and covered on its own line, the button left as it is.
(function () {
  const ACTIONS = {
    ar: {
      "HEAD OUT": "انطلق", "LEAVE": "غادر", "EXIT": "اخرج", "ENTER": "ادخل", "TALK": "تحدّث", "GREET": "سلّم",
      "SIT": "اجلس", "SIT DOWN": "اجلس", "STAND UP": "قف", "GET UP": "انهض", "SEARCH": "فتّش", "INSPECT": "افحص",
      "LOOK": "انظر", "PICK UP": "التقط", "TAKE": "خذ", "GIVE": "أعطِ", "USE": "استخدم", "INTERACT": "تفاعل",
      "OPEN": "افتح", "CLOSE": "أغلق", "KNOCK": "اطرق الباب", "FOLLOW": "اتبع", "WAIT": "انتظر", "ANSWER": "رُد",
      "IGNORE": "تجاهل", "CALL": "اتصل", "SKIP": "تخطَّ", "CONTINUE": "تابع", "ACCEPT": "اقبل", "DECLINE": "ارفض",
      "CANCEL": "إلغاء", "BACK": "رجوع", "BUY": "اشترِ", "SELL": "بِع", "PAY": "ادفع", "SHOP": "تسوّق",
      "BROWSE": "تصفّح", "ROB": "اسرق", "HOLD UP": "سطو مسلّح", "START": "ابدأ", "START MISSION": "ابدأ المهمة",
      "GET IN": "اركب", "GET OUT": "انزل", "DRIVE": "قُد", "PARK": "اركن", "REPAIR": "أصلِح", "REFUEL": "عبّئ الوقود",
      "HIDE": "اختبئ", "CLIMB": "تسلّق", "JUMP": "اقفز", "HUG": "عانق", "KISS": "قبّل", "DANCE": "ارقص",
      "DRINK": "اشرب", "EAT": "كُل", "SLEEP": "نَم", "REST": "استرح", "SAVE": "احفظ", "SAVE GAME": "احفظ اللعبة",
      "CHANGE OUTFIT": "غيّر الملابس", "WARDROBE": "خزانة الملابس", "SWITCH": "بدّل", "HELP": "ساعد",
      "PET": "دلّل", "PLAY": "العب", "WATCH": "شاهد", "TAKE PHOTO": "التقط صورة", "SHOW": "أرِ", "DROP": "اترك",
      "THROW": "ارمِ", "RELOAD": "أعد التعبئة", "AIM": "صوّب", "SHOOT": "أطلق النار", "CARRY": "احمل",
      "LIFT": "ارفع", "PUSH": "ادفع", "PULL": "اسحب", "STEAL": "اسرق", "LOCKPICK": "افتح القفل", "HACK": "اخترق",
      "COLLECT": "اجمع", "DELIVER": "سلّم", "STORE": "خزّن", "LOOT": "انهب", "WAVE": "لوّح", "WHISTLE": "صفّر",
      "JASON": "جيسون", "LUCIA": "لوسيا",
    },
  };
  const key = (en) => en.toUpperCase().replace(/0/g, "O").replace(/[^A-Z &'’-]/g, "").replace(/\s+/g, " ").trim();
  const actionWord = (en) => {
    const t = ACTIONS[vi.lang()] || {};
    const k = key(en);
    if (t[k]) return t[k];
    // (the language's own word list, then the names dictionary)
    let w = "";
    try { w = ownWord(en, true) || ""; } catch (e) { w = ""; }
    if (!w && typeof vi.name === "function") w = vi.name(en) || "";
    return w;
  };
  // the corner (the buttons' column ends ~0.985 of the width; four actions reach ~0.78 of the height)
  const zone = (f) => pxRect(f, { x: 0.76, y: 0.74, w: 0.235, h: 0.25 });
  // the buttons on the corner's right edge: shapes about a letter and a half tall, one per action (white, or
  // grey ~105 for an action that can't be taken now, its words grey too)
  function buttons(f) {
    const z = zone(f);
    const col = { x: z.x + Math.round(z.w * 0.72), y: z.y, w: z.w - Math.round(z.w * 0.72), h: z.h };
    const unit = f.h * 0.024; // a button's height at 1080p is ~26 px
    const rows = f.profile(col, { axis: "rows", step: 1, bright: [80, 255], spread: [0, 45] });
    const out = [];
    let start = -1;
    for (let i = 0; i <= rows.length; i++) {
      const on = i < rows.length && rows[i] >= 2;
      if (on && start < 0) start = i;
      if (!on && start >= 0) {
        const h = i - start;
        if (h >= unit * 0.7 && h <= unit * 2) out.push({ y0: col.y + start, y1: col.y + i });
        start = -1;
      }
    }
    return out;
  }
  const sig = (bs) => bs.map((b) => Math.round((b.y0 + b.y1) / 2));
  const sameSig = (a, b) => a.length === b.length && a.every((y, i) => Math.abs(y - b[i]) <= 5);

  pages.add({
    id: "gta6_actions",
    option: "game_notices",
    beside: true,
    // (a translation found later shows on the next read; a line's own read keeps the boxes where they were)
    hold: { ms: 250, rereadMs: 700 },
    present(f) {
      const bs = buttons(f);
      if (!bs.length || bs.length > 6) return null;
      // (what was read there and wasn't actions: not read again while it looks the same)
      if (this.not && sameSig(this.not, sig(bs))) return null;
      return { bs };
    },
    read(f, k) {
      if (!k) return null;
      const z = zone(f);
      const segs = segments(f, z, { scale: 2 }, 1.6);
      const menu = [];
      const unit = f.h * 0.024;
      for (const b of k.bs) {
        // the action's words: on the button's row, left of it, in capitals
        const mid = (b.y0 + b.y1) / 2;
        const row = segs.filter((s) => s.y0 <= mid + unit * 0.5 && s.y1 >= mid - unit * 0.5 && s.x1 < z.x + z.w * 0.9);
        const ws = row.flatMap((s) => s.words).filter((w) => /\p{L}{2}/u.test(w.text) && !/^[LR][1-3]$/.test(w.text)).sort((a, b) => a.x - b.x);
        if (!ws.length) continue;
        const en = ws.map((w) => w.text).join(" ").replace(/\s+/g, " ").trim();
        // (a menu's own buttons row, "SELECT ✕  BACK ○": two of its button names or more on the line; the menu's
        // page has it)
        if (typeof splitButtons === "function" && splitButtons({ words: ws }).filter((p) => known(p.text, BUTTON_NAMES)).length >= 2) continue;
        // (the game's actions are in capitals: a sign or a plate in the scene is mixed or has none)
        if (/\p{Ll}/u.test(en) || !/^[\p{Lu}\d &'’.!?-]+$/u.test(en) || en.length > 28) continue;
        const x0 = Math.min(...ws.map((w) => w.x)), x1 = Math.max(...ws.map((w) => w.x + w.w));
        const y0 = Math.min(...ws.map((w) => w.y)), y1 = Math.max(...ws.map((w) => w.y + w.h));
        const h = y1 - y0, pad = Math.round(h * 0.3);
        // (the Arabic may be longer: room to the left, the right edge where the English ends, by its icon)
        const left = Math.max(z.x, x0 - Math.round(h * 2.5));
        const box = { x: left, y: y0 - pad, w: x1 + pad - left, h: h + 2 * pad };
        const bg = f.median({ x: x0 - pad, y: y0 - pad, w: x1 - x0 + 2 * pad, h: h + 2 * pad }, { step_x: 2, step_y: 2, exclude: [x0 - 1, y0 - 1, x1 + 1, y1 + 1] });
        const fg = f.pixels({ x: x0, y: y0, w: x1 - x0, h }, { step: 1, bright: [80, 255], spread: [0, 40], median: true });
        const ar = actionWord(en);
        menu.push({
          en, ar: ar || "", fixed: !!ar, icons: [], lines: 1,
          rect: f.box(box.x, box.y, box.w, box.h), bg: hex(bg || { r: 0, g: 0, b: 0 }), fg: hex((fg && fg.median) || { r: 255, g: 255, b: 255 }),
          line_h: h / f.h, size: 1.15, font: "title", weight: 700, align: "right", wrap: "no",
          item: false, right: true, missed: 0,
        });
      }
      if (!menu.length) {
        this.not = sig(k.bs);
        return null;
      }
      this.not = null;
      this.sig = sig(k.bs);
      return { id: "gta6_actions", title: { x: 0, y: 0, w: 0, h: 0 }, menu, buttons: [], buttons_row: null };
    },
    // still up: the same buttons in the same places (one more, one less: read again)
    same(f) {
      return !!this.sig && sameSig(this.sig, sig(buttons(f)));
    },
  });
})();
