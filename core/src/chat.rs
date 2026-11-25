//! GTA VI's message bubble at the picture's bottom right (a text that comes in while playing: the
//! contact's picture and name over the message, on a translucent light-grey box with a tail at its
//! bottom left): found, read, and its translation drawn in a bubble just like it, just above it.

use crate::ocr::{self, Crop, Ocr, Word};

/// A message bubble read from the frame (frame pixels).
#[derive(Clone, Debug, PartialEq)]
pub struct Bubble {
    /// the bubble's box
    pub rect: (u32, u32, u32, u32),
    /// the contact's name (the bold first line), when there is one
    pub name: Option<String>,
    /// the message as shown (the game cuts a long one: "... ge...")
    pub en: String,
    /// the contact's picture before the name
    pub avatar: Option<(u32, u32, u32, u32)>,
    /// the mark at the bubble's top right (a map pin, a camera), when there is one
    pub badge: Option<(u32, u32, u32, u32)>,
    /// the emojis in the message (`en` has them as "⟦1⟧", "⟦2⟧" … in their places)
    pub emoji: Vec<(u32, u32, u32, u32)>,
    /// the box's colour, the message's and the name's
    pub bg: [u8; 3],
    pub fg: [u8; 3],
    pub name_fg: [u8; 3],
}

impl Bubble {
    /// The game cut the message off ("…" at its end).
    pub fn cut(&self) -> bool {
        let t = self.en.trim_end();
        t.ends_with("...") || t.ends_with('…')
    }
}

/// A message that came in, as words for the language models' context: who sent it to the
/// player's character. None without a name: nothing more than the message itself.
pub fn message_context(b: &Bubble) -> Option<String> {
    let name = b.name.as_deref().map(str::trim).filter(|n| !n.is_empty())?;
    let name: String = name.chars().take(60).collect();
    Some(format!("A text message to the player's character, from: {name}"))
}

fn px(rgba: &[u8], fw: u32, x: u32, y: u32) -> [u8; 3] {
    let i = ((y * fw + x) * 4) as usize;
    [rgba[i], rgba[i + 1], rgba[i + 2]]
}

/// light and nearly colourless: the bubble's own grey (about 140 to 230 over any scene); see-through, it
/// takes a tint from bright colours behind it (a light-blue car: 190 / 225 / 240), still very light then
fn light(p: [u8; 3]) -> bool {
    let (lo, hi) = (*p.iter().min().unwrap(), *p.iter().max().unwrap());
    (lo >= 140 && hi - lo <= 25) || (lo >= 195 && hi - lo <= 60)
}

/// The bubble's box, when one is up: the largest light, colourless region in the picture's bottom
/// right corner (its right three tenths and bottom three tenths) that stands on its own there (not
/// running off the corner's edges, as bright scenery does), as wide as 0.13 to 0.26 of the picture,
/// at least 0.05 of its height tall, mostly filled (its text is thin), its right edge near the
/// picture's, with dark text in it.
pub fn find_bubble(rgba: &[u8], fw: u32, fh: u32) -> Option<(u32, u32, u32, u32)> {
    if fw < 320 || fh < 180 || rgba.len() < (fw * fh * 4) as usize {
        return None;
    }
    let s = (fh / 180).max(3); // a cell: 6 pixels in 1080p
    let (ax, ay) = (fw * 7 / 10, fh * 7 / 10);
    let (cw, ch) = ((fw - ax) / s, (fh - ay) / s);
    let mut cells = vec![false; (cw * ch) as usize];
    for cy in 0..ch {
        for cx in 0..cw {
            let (x0, y0) = (ax + cx * s, ay + cy * s);
            let n = [(1, 1), (s - 2, 1), (1, s - 2), (s - 2, s - 2), (s / 2, s / 2)]
                .iter()
                .filter(|(dx, dy)| light(px(rgba, fw, x0 + dx, y0 + dy)))
                .count();
            cells[(cy * cw + cx) as usize] = n >= 3;
        }
    }
    // the largest connected region
    let mut seen = vec![false; cells.len()];
    let mut best: Option<(u32, u32, u32, u32, u32)> = None; // x0, y0, x1, y1, cells
    for start in 0..cells.len() {
        if !cells[start] || seen[start] {
            continue;
        }
        let (mut x0, mut y0, mut x1, mut y1, mut n) = (u32::MAX, u32::MAX, 0, 0, 0);
        let mut stack = vec![start];
        seen[start] = true;
        while let Some(i) = stack.pop() {
            let (x, y) = (i as u32 % cw, i as u32 / cw);
            (x0, y0, x1, y1, n) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y), n + 1);
            let mut push = |j: usize| {
                if cells[j] && !seen[j] {
                    seen[j] = true;
                    stack.push(j);
                }
            };
            if x > 0 {
                push(i - 1);
            }
            if x + 1 < cw {
                push(i + 1);
            }
            if y > 0 {
                push(i - cw as usize);
            }
            if y + 1 < ch {
                push(i + cw as usize);
            }
        }
        if best.is_none_or(|b| n > b.4) {
            best = Some((x0, y0, x1, y1, n));
        }
    }
    let (x0, y0, x1, y1, n) = best?;
    let (bw, bh) = ((x1 - x0 + 1) * s, (y1 - y0 + 1) * s);
    let fill = (n * s * s) as f64 / (bw * bh) as f64;
    let right = ax + (x1 + 1) * s;
    let on_its_own = x0 > 0 && y0 > 0 && x1 + 1 < cw - 1;
    if !(on_its_own && bw * 100 >= fw * 13 && bw * 100 <= fw * 26 && bh * 100 >= fh * 5 && bh * 100 <= fh * 28 && fill >= 0.7 && right * 100 >= fw * 95) {
        return None;
    }
    let rect = (ax + x0 * s, ay + y0 * s, bw, bh);
    // dark text in it, not a blank light patch or a busy one
    let (mut dark, mut all) = (0u32, 0u32);
    for y in (rect.1..rect.1 + rect.3).step_by(2) {
        for x in (rect.0..rect.0 + rect.2).step_by(2) {
            all += 1;
            if *px(rgba, fw, x, y).iter().max().unwrap() < 90 {
                dark += 1;
            }
        }
    }
    (dark * 100 >= all && dark * 4 <= all).then_some(rect)
}

fn median(v: &mut [[u8; 3]]) -> [u8; 3] {
    if v.is_empty() {
        return [0, 0, 0];
    }
    let mut out = [0u8; 3];
    for (c, o) in out.iter_mut().enumerate() {
        v.sort_unstable_by_key(|p| p[c]);
        *o = v[v.len() / 2][c];
    }
    out
}

/// The text's colour in a box of the frame: the darker third of its dark pixels.
fn ink(rgba: &[u8], fw: u32, r: (u32, u32, u32, u32)) -> [u8; 3] {
    let mut v: Vec<[u8; 3]> = Vec::new();
    for y in r.1..r.1 + r.3 {
        for x in r.0..r.0 + r.2 {
            let p = px(rgba, fw, x, y);
            if *p.iter().max().unwrap() < 110 {
                v.push(p);
            }
        }
    }
    v.sort_by_key(|p| p.iter().map(|&c| c as u32).sum::<u32>());
    v.truncate((v.len() / 3).max(1));
    median(&mut v)
}

/// The bubble's text: the name (its bold first line, after the contact's picture) and the message.
pub fn read_bubble(ocr: &Ocr, rgba: &[u8], fw: u32, fh: u32, rect: (u32, u32, u32, u32)) -> Option<Bubble> {
    let (x, y, w, h) = rect;
    let crop = Crop::from_frame(rgba, fw, fh, x as f64 / fw as f64, y as f64 / fh as f64, w as f64 / fw as f64, h as f64 / fh as f64)?;
    let all_words = ocr.recognize_checked(&crop).ok()?.0;
    let mut words: Vec<Word> = all_words.into_iter().filter(|w| w.text.chars().any(|c| c.is_alphanumeric())).collect();
    if words.is_empty() {
        return None;
    }
    // lines, top to bottom
    words.sort_by_key(|w| w.rect.1 + w.rect.3 / 2);
    let mut lines: Vec<Vec<Word>> = Vec::new();
    for wd in words {
        let m = wd.rect.1 + wd.rect.3 / 2;
        match lines.last_mut() {
            Some(l) if m <= l[0].rect.1 + l[0].rect.3 / 2 + l[0].rect.3 / 2 => l.push(wd),
            _ => lines.push(vec![wd]),
        }
    }
    for l in lines.iter_mut() {
        l.sort_by_key(|w| w.rect.0);
    }
    // (the marks on the name's line, the picture or a pin read as "O": no words)
    if let Some(l) = lines.first_mut() {
        while l.len() > 1 && l[0].text.chars().filter(|c| c.is_alphanumeric()).count() <= 1 {
            l.remove(0);
        }
        while l.len() > 1 && l.last().is_some_and(|w| w.text.chars().filter(|c| c.is_alphanumeric()).count() <= 1) {
            l.pop();
        }
    }
    // a short name ("Cal") the reading missed: the first line read is the message's, well below the
    // bubble's top: the name's row above it read on its own
    let top = lines[0].iter().map(|w| w.rect.1).min().unwrap_or(0);
    if top * 100 > crop.h * 25 {
        let (rx, ry, rw, rh) = (x + w / 8, y + 2, w / 2, top.saturating_sub(4));
        if rh >= 8 {
            if let Some(strip) = Crop::from_frame(rgba, fw, fh, rx as f64 / fw as f64, ry as f64 / fh as f64, rw as f64 / fw as f64, rh as f64 / fh as f64) {
                if let Ok((more, _)) = ocr.recognize_checked(&strip) {
                    let more: Vec<Word> = more
                        .into_iter()
                        .filter(|wd| wd.text.chars().filter(|c| c.is_alphanumeric()).count() >= 2)
                        .map(|mut wd| {
                            // (to the bubble's crop's pixels)
                            wd.rect.0 += rx - x;
                            wd.rect.1 += ry - y;
                            wd
                        })
                        .collect();
                    if !more.is_empty() {
                        lines.insert(0, more);
                    }
                }
            }
        }
    }
    let span = |l: &[Word]| {
        let (a, b) = (l.iter().map(|w| w.rect.0).min().unwrap_or(0), l.iter().map(|w| w.rect.0 + w.rect.2).max().unwrap_or(0));
        let (t, u) = (l.iter().map(|w| w.rect.1).min().unwrap_or(0), l.iter().map(|w| w.rect.1 + w.rect.3).max().unwrap_or(0));
        (a, t, b - a, u - t)
    };
    let text = |l: &[Word]| ocr::clean(l);
    // the name: a short first line starting after the picture (a fifth of the way in, or less), no
    // sentence in it, with the message under it
    let first = span(&lines[0]);
    // (a short name on its own, "Cal", is noise to the clean-up: then as read)
    let first_text = match text(&lines[0]) {
        t if t.is_empty() => lines[0].iter().map(|w| w.text.trim_matches(|c: char| !c.is_alphanumeric() && c != '\'')).collect::<Vec<_>>().join(" ").trim().to_string(),
        t => t,
    };
    let named = lines.len() >= 2
        && first.0 * 100 >= crop.w * 8
        && first_text.split_whitespace().count() <= 4
        && !first_text.contains(['.', '!', '?', ','])
        && first_text.chars().next().is_some_and(|c| c.is_uppercase());
    let (name, body) = if named { (Some(first_text), lines[1..].to_vec()) } else { (None, lines.clone()) };
    if body.is_empty() {
        return None;
    }
    // the picture: what's drawn left of the name, about its line's height
    let avatar = if named {
        let (lh, mid) = (first.3.max(8), y + first.1 + first.3 / 2);
        let (ax0, ax1) = (x + 2, x + first.0.saturating_sub(2));
        let size = (lh * 2).min(ax1.saturating_sub(ax0));
        (size >= 8).then(|| (ax1 - size, mid.saturating_sub(size / 2), size, size))
    } else {
        None
    };
    // colours: the box's grey (its light pixels), the message's ink and the name's
    let mut grey: Vec<[u8; 3]> = Vec::new();
    for yy in (y..y + h).step_by(3) {
        for xx in (x..x + w).step_by(3) {
            let p = px(rgba, fw, xx, yy);
            if light(p) {
                grey.push(p);
            }
        }
    }
    let bg = median(&mut grey);
    let body_rect = body.iter().map(|l| span(l)).fold(None, |acc: Option<(u32, u32, u32, u32)>, r| {
        Some(match acc {
            None => r,
            Some(a) => {
                let (x0, y0) = (a.0.min(r.0), a.1.min(r.1));
                (x0, y0, (a.0 + a.2).max(r.0 + r.2) - x0, (a.1 + a.3).max(r.1 + r.3) - y0)
            }
        })
    })?;
    let fg = ink(rgba, fw, (x + body_rect.0, y + body_rect.1, body_rect.2, body_rect.3));
    let name_fg = if named { ink(rgba, fw, (x + first.0, y + first.1, first.2, first.3)) } else { fg };
    // emojis in the message (the OCR skips them): in their places in the text, as the game's own
    // lines have them ("Hurry home babe ⟦1⟧"), and their pictures
    let mut hs: Vec<u32> = body.iter().flatten().map(|w| w.rect.3).collect();
    hs.sort_unstable();
    let lh = hs[hs.len() / 2].max(8);
    let below_name = if named { first.1 + first.3 + 2 } else { 2 };
    let area = (x + 4, y + below_name, w.saturating_sub(8), h.saturating_sub(below_name + 4));
    let emoji = emoji_in(rgba, fw, fh, area, bg, fg, lh);
    let body = with_emojis(body, &emoji, (x, y));
    // (a line of emojis only, 🔥🔥🔥 under the text: no letters for the clean-up, kept as they are)
    let line_text = |l: &[Word]| if l.iter().all(|w| w.text.starts_with('⟦')) { l.iter().map(|w| w.text.clone()).collect::<Vec<_>>().join(" ") } else { text(l) };
    let en = body.iter().map(|l| line_text(l)).filter(|t| !t.is_empty()).collect::<Vec<_>>().join(" ");
    // (words in it: a call's notice, "Valentina 00:14", is no message)
    let words_only = en.replace(|c: char| c == '⟦' || c == '⟧' || c.is_ascii_digit(), "");
    if words_only.chars().filter(|c| c.is_alphabetic()).count() < 2 || !ocr::looks_like_words(&words_only) {
        return None;
    }
    let badge = badge_of(rgba, fw, rect, if named { first.1 + first.3 } else { h / 3 });
    Some(Bubble { rect, name, en, avatar, badge, emoji, bg, fg, name_fg })
}

/// Emojis among a message's text (frame pixels, in reading order): colourful shapes, unlike the
/// bubble (`bg`) and the text (`fg`), about a line's height (`lh`) tall and wide.
pub fn emoji_in(rgba: &[u8], fw: u32, fh: u32, area: (u32, u32, u32, u32), bg: [u8; 3], fg: [u8; 3], lh: u32) -> Vec<(u32, u32, u32, u32)> {
    let (ax, ay) = (area.0.min(fw), area.1.min(fh));
    let (aw, ah) = (area.2.min(fw - ax), area.3.min(fh - ay));
    let d = |a: [u8; 3], b: [u8; 3]| (0..3).map(|c| (a[c] as i32 - b[c] as i32).abs()).max().unwrap_or(0);
    let on = |xx: u32, yy: u32| {
        let p = px(rgba, fw, ax + xx, ay + yy);
        let (lo, hi) = (*p.iter().min().unwrap() as i32, *p.iter().max().unwrap() as i32);
        // (and not a blend of the two, a letter's soft edge on a coloured bubble: far from the line
        // between them)
        hi - lo > 70 && d(p, bg) > 60 && d(p, fg) > 60 && off_blend(p, bg, fg) > 40.0
    };
    let mut seen = vec![false; (aw * ah) as usize];
    let mut boxes: Vec<(u32, u32, u32, u32, u32)> = Vec::new(); // x0, y0, x1, y1, pixels
    for start in 0..(aw * ah) {
        let (sx, sy) = (start % aw, start / aw);
        if seen[start as usize] || !on(sx, sy) {
            continue;
        }
        let (mut x0, mut y0, mut x1, mut y1, mut n) = (sx, sy, sx, sy, 0u32);
        let mut stack = vec![(sx, sy)];
        seen[start as usize] = true;
        while let Some((cx, cy)) = stack.pop() {
            (x0, y0, x1, y1, n) = (x0.min(cx), y0.min(cy), x1.max(cx), y1.max(cy), n + 1);
            for (nx, ny) in [(cx.wrapping_sub(1), cy), (cx + 1, cy), (cx, cy.wrapping_sub(1)), (cx, cy + 1)] {
                if nx < aw && ny < ah && !seen[(ny * aw + nx) as usize] && on(nx, ny) {
                    seen[(ny * aw + nx) as usize] = true;
                    stack.push((nx, ny));
                }
            }
        }
        boxes.push((x0, y0, x1, y1, n));
    }
    // (an emoji's parts — a face and its heart — as one: boxes touching or nearly)
    let gap = (lh / 6).max(2);
    let mut merged = true;
    while merged {
        merged = false;
        let mut join: Option<(usize, usize)> = None;
        for i in 0..boxes.len() {
            for j in i + 1..boxes.len() {
                let (a, b) = (boxes[i], boxes[j]);
                // (side by side only when touching: emojis in a row, 🔥🔥🔥, stay apart)
                if a.0 <= b.2 + 1 && b.0 <= a.2 + 1 && a.1 <= b.3 + gap && b.1 <= a.3 + gap {
                    join = Some((i, j));
                    break;
                }
            }
            if join.is_some() {
                break;
            }
        }
        if let Some((i, j)) = join {
            let (a, b) = (boxes[i], boxes[j]);
            boxes[i] = (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3), a.4 + b.4);
            boxes.remove(j);
            merged = true;
        }
    }
    let mut out: Vec<(u32, u32, u32, u32)> = boxes
        .into_iter()
        .filter(|b| {
            let (bw, bh) = (b.2 - b.0 + 1, b.3 - b.1 + 1);
            // (a flame's colour fills only some of its box: an eighth at least, and enough pixels)
            bw * 2 >= lh && bh * 2 >= lh && bw <= lh * 9 / 4 && bh <= lh * 9 / 4 && b.4 * 8 >= bw * bh && b.4 * 10 >= lh * lh
        })
        .map(|b| (ax + b.0, ay + b.1, b.2 - b.0 + 1, b.3 - b.1 + 1))
        // (inside the bubble: the bubble's own colour round it, mostly; a send arrow beside a reply,
        // an outline, the bubble's edge are on the screen's)
        .filter(|&(ex, ey, ew, eh)| {
            let m = 3;
            let mut ring = Vec::new();
            for k in 0..8 {
                let (tx, ty) = (ex + ew * k / 8, ey + eh * k / 8);
                ring.extend([(tx, ey.saturating_sub(m)), (tx, ey + eh + m), (ex.saturating_sub(m), ty), (ex + ew + m, ty)]);
            }
            let inside = ring.iter().filter(|&&(rx, ry)| rx < fw && ry < fh && d(px(rgba, fw, rx, ry), bg) < 45).count();
            inside * 10 >= ring.len() * 5
        })
        .collect();
    // reading order: line by line, left to right
    out.sort_by_key(|b| ((b.1 + b.3 / 2) / lh.max(1), b.0));
    out
}

/// How far a colour is from every blend of `a` and `b` (RGB distance to the segment between them).
fn off_blend(p: [u8; 3], a: [u8; 3], b: [u8; 3]) -> f64 {
    let (p, a, b) = ([p[0] as f64, p[1] as f64, p[2] as f64], [a[0] as f64, a[1] as f64, a[2] as f64], [b[0] as f64, b[1] as f64, b[2] as f64]);
    let ab: Vec<f64> = (0..3).map(|i| b[i] - a[i]).collect();
    let len2 = ab.iter().map(|v| v * v).sum::<f64>().max(1.0);
    let t = ((0..3).map(|i| (p[i] - a[i]) * ab[i]).sum::<f64>() / len2).clamp(0.0, 1.0);
    (0..3).map(|i| (p[i] - (a[i] + t * ab[i])).powi(2)).sum::<f64>().sqrt()
}

/// A message's lines with its emojis put in their places as words ("⟦1⟧", numbered in reading
/// order): in the line they sit in, after the words left of them; one under the text, a line of
/// its own. `emoji` in frame pixels, the words in the bubble's crop (its top left at `origin`).
fn with_emojis(mut lines: Vec<Vec<Word>>, emoji: &[(u32, u32, u32, u32)], origin: (u32, u32)) -> Vec<Vec<Word>> {
    for (i, e) in emoji.iter().enumerate() {
        let r = (e.0 - origin.0, e.1 - origin.1, e.2, e.3);
        let mid = r.1 + r.3 / 2;
        let word = Word { text: ocr::icon_token(i + 1), rect: r, line: 0 };
        let at = lines.iter().position(|l| {
            let (t, b) = (l.iter().map(|w| w.rect.1).min().unwrap_or(0), l.iter().map(|w| w.rect.1 + w.rect.3).max().unwrap_or(0));
            mid + 4 >= t && mid <= b + 4
        });
        match at {
            Some(k) => {
                let pos = lines[k].iter().position(|w| w.rect.0 > r.0).unwrap_or(lines[k].len());
                lines[k].insert(pos, word);
            }
            None => {
                let pos = lines.iter().position(|l| l.iter().map(|w| w.rect.1).min().unwrap_or(0) > mid).unwrap_or(lines.len());
                lines.insert(pos, vec![word]);
            }
        }
    }
    lines
}

/// The coloured mark in the bubble's top right corner, level with the name (a blue map pin, a
/// camera): the box round its coloured pixels, about square, when there is one.
fn badge_of(rgba: &[u8], fw: u32, rect: (u32, u32, u32, u32), name_bottom: u32) -> Option<(u32, u32, u32, u32)> {
    let (x, y, w, _) = rect;
    let (ax, ay, aw, ah) = (x + w * 4 / 5, y + 2, w / 5 - 2, name_bottom.max(12) + 4);
    let (mut x0, mut y0, mut x1, mut y1, mut n) = (u32::MAX, u32::MAX, 0, 0, 0);
    for yy in ay..ay + ah {
        for xx in ax..ax + aw {
            let p = px(rgba, fw, xx, yy);
            let (lo, hi) = (*p.iter().min().unwrap() as i32, *p.iter().max().unwrap() as i32);
            if hi - lo > 60 {
                (x0, y0, x1, y1, n) = (x0.min(xx), y0.min(yy), x1.max(xx), y1.max(yy), n + 1);
            }
        }
    }
    let (bw, bh) = (x1.saturating_sub(x0) + 1, y1.saturating_sub(y0) + 1);
    (n >= 20 && bw >= 8 && bh >= 8 && bw * 10 <= bh * 15 && bh * 10 <= bw * 15).then(|| (x0.saturating_sub(1), y0.saturating_sub(1), bw + 2, bh + 2))
}

/// A translation cut as the game cut its English: about as much of it as `shown` is of `whole`
/// (by letters), at a word's end, then "…".
pub fn cut_like(ar: &str, shown: &str, whole: &str) -> String {
    let (s, w) = (shown.trim_end_matches(['.', '…', ' ']).chars().count(), whole.chars().count().max(1));
    if s >= w {
        return ar.to_string();
    }
    let want = ar.chars().count() * s / w;
    let mut out = String::new();
    for word in ar.split_whitespace() {
        if !out.is_empty() && out.chars().count() + 1 + word.chars().count() > want + 2 {
            break;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    format!("{}…", out.trim_end_matches(['.', '،', ',', '!', '؟', '?']))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cut_translation_is_cut_like_its_english() {
        let ar = "أنا مجددًا. لا أطيق صبرًا للخروج من هنا والذهاب إلى البحر.";
        let cut = cut_like(ar, "Me again. Can't wait to ge...", "me again can't wait to get out of here and go to the sea");
        assert!(cut.ends_with('…') && cut.chars().count() < ar.chars().count(), "{cut}");
        assert!(cut.starts_with("أنا مجددًا"), "{cut}");
        assert_eq!(cut_like("مرحبا", "Hi", "hi"), "مرحبا");
    }

    #[test]
    fn a_bubble_is_found_in_the_corner_and_scenery_is_not() {
        let (fw, fh) = (1920u32, 1080u32);
        let mut rgba = vec![20u8; (fw * fh * 4) as usize];
        let fill = |rgba: &mut Vec<u8>, r: (u32, u32, u32, u32), c: u8| {
            for y in r.1..r.1 + r.3 {
                for x in r.0..r.0 + r.2 {
                    let i = ((y * fw + x) * 4) as usize;
                    rgba[i..i + 3].copy_from_slice(&[c, c, c + 4]);
                }
            }
        };
        // the bubble, with lines of dark text in it
        fill(&mut rgba, (1494, 924, 390, 132), 190);
        for line in 0..3 {
            fill(&mut rgba, (1560, 944 + line * 34, 280, 3), 30);
        }
        assert_eq!(find_bubble(&rgba, fw, fh).map(|r| (r.0 / 6, r.1 / 6)), Some((1494 / 6, 924 / 6)));
        // a bright sky across the whole corner is none
        fill(&mut rgba, (1344, 756, 576, 324), 200);
        assert_eq!(find_bubble(&rgba, fw, fh), None);
    }

    #[test]
    fn a_message_s_context_is_who_sent_it() {
        let b = Bubble { rect: (0, 0, 1, 1), name: Some(" Cal ".into()), en: "You up?".into(), avatar: None, badge: None, emoji: Vec::new(), bg: [0; 3], fg: [0; 3], name_fg: [0; 3] };
        assert_eq!(message_context(&b).as_deref(), Some("A text message to the player's character, from: Cal"));
        assert_eq!(message_context(&Bubble { name: None, ..b }), None);
    }
}
