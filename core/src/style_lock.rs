// Ported from CouchSubs (AGPL-3.0, (C) 2026 Sinan Yorulmaz), commit bc54ff7: couchsubs-android/app/src/main/java/com/couchsubs/mobile/core/ScreenStyleLock.kt
//
//! A screen-fixed text's look stays put while it's up (the pause menu, the map's mission card, a
//! page script's boxes): for each kind of text (a button hint, a paragraph, a CAPITALS title or
//! tab, a plain label) its size, weight and colours are locked at the first reading. A reading
//! within [`SIZE_SHARE`] of a locked size is drawn at that size (an item keeps the size it was last
//! drawn at); with items matching in the same frame, one that doesn't is a different size and is
//! locked at once. When no item of the kind matches, a change is taken only after it lasts
//! [`SETTLE_MS`] without a break. A colour pair within [`COLOR_SLACK`] of a locked pair is drawn as
//! that pair. Phone bubbles and items with no look of their own don't take part. Checked against
//! CouchSubs' fixture (tests/fixtures/screen-style-lock.json).

use std::collections::HashMap;

/// A reading within this share of a locked size is that size.
pub const SIZE_SHARE: f64 = 0.25;
/// A different size taken only after this long without a break.
pub const SETTLE_MS: u64 = 1000;
/// Sizes within this share in one frame are one size.
pub const SAME_LINE: f64 = 0.15;
/// Colours this close (each of R, G, B) are the same colour.
pub const COLOR_SLACK: i32 = 40;
const PALETTE_LIMIT: usize = 32;
const SEEN_LIMIT: usize = 256;

/// One text as read (and as drawn, after [`StyleLock::steady`]).
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    /// the game's own text (the item's identity across readings)
    pub source: String,
    /// one line's height (frame fraction)
    pub line: f64,
    pub upper: bool,
    pub bold: bool,
    pub condensed: bool,
    pub rows: u32,
    pub icons: usize,
    pub fg: String,
    pub bg: String,
    /// a phone bubble (not locked: the phone has its own)
    pub bubble: bool,
    /// whether the item has a look of its own (else it isn't locked)
    pub look: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Tier {
    line: f64,
    bold: bool,
    condensed: bool,
}

#[derive(Debug, Default)]
struct Kind {
    tiers: Vec<Tier>,
    since: Option<u64>,
    /// the size each item (by its own text) was last drawn at
    seen: HashMap<String, usize>,
}

/// The locked looks, for one screen session (reset when it's gone).
#[derive(Debug, Default)]
pub struct StyleLock {
    kinds: HashMap<String, Kind>,
    palettes: HashMap<String, Vec<(String, String)>>,
}

fn role(i: &Item) -> Option<&'static str> {
    if !i.look || i.bubble || i.line <= 0.0 {
        return None;
    }
    if i.icons > 0 {
        return Some("hint");
    }
    if i.rows > 1 {
        return Some("body");
    }
    Some(if i.upper { "caps" } else { "label" })
}

fn rgb(hex: &str) -> Option<(i32, i32, i32)> {
    if hex.len() != 7 || !hex.starts_with('#') {
        return None;
    }
    let v = i32::from_str_radix(&hex[1..], 16).ok()?;
    Some(((v >> 16) & 0xff, (v >> 8) & 0xff, v & 0xff))
}

/// Two colours the same within [`COLOR_SLACK`].
pub fn near(a: &str, b: &str) -> bool {
    match (rgb(a), rgb(b)) {
        (Some(x), Some(y)) => (x.0 - y.0).abs() <= COLOR_SLACK && (x.1 - y.1).abs() <= COLOR_SLACK && (x.2 - y.2).abs() <= COLOR_SLACK,
        _ => a.eq_ignore_ascii_case(b),
    }
}

/// A frame's items' sizes: close sizes ([`SAME_LINE`]) one size, the middle item's; each size's items.
fn tiers(items: &[&Item]) -> Vec<(Tier, Vec<usize>)> {
    let mut order: Vec<usize> = (0..items.len()).collect();
    order.sort_by(|a, b| items[*a].line.total_cmp(&items[*b].line));
    let mut groups: Vec<Vec<usize>> = Vec::new();
    for i in order {
        match groups.last_mut() {
            Some(g) if items[i].line <= items[g[0]].line * (1.0 + SAME_LINE) => g.push(i),
            _ => groups.push(vec![i]),
        }
    }
    groups
        .into_iter()
        .map(|g| {
            let m = items[g[(g.len() - 1) / 2]];
            (Tier { line: m.line, bold: m.bold, condensed: m.condensed }, g)
        })
        .collect()
}

impl StyleLock {
    pub fn new() -> Self {
        Self::default()
    }

    /// The items with their locked look. `now`: milliseconds (a steady clock).
    pub fn steady(&mut self, items: &[Item], now: u64) -> Vec<Item> {
        let mut out = items.to_vec();
        let roles: Vec<Option<&'static str>> = items.iter().map(role).collect();
        let mut kinds_here: Vec<&'static str> = roles.iter().flatten().copied().collect();
        kinds_here.sort();
        kinds_here.dedup();
        for r in kinds_here {
            let members: Vec<usize> = (0..items.len()).filter(|i| roles[*i] == Some(r)).collect();
            let mut kind = self.kinds.remove(r).unwrap_or_default();
            let off = |kind: &Kind, line: f64, t: usize| (line / kind.tiers[t].line - 1.0).abs();
            // (an item's size: the one it was drawn at last if it still fits, so it doesn't swing
            // between two near sizes; else the nearest that fits)
            let tier_of = |kind: &Kind, i: usize| -> Option<usize> {
                let line = items[i].line;
                if let Some(&s) = kind.seen.get(&items[i].source)
                    && s < kind.tiers.len()
                    && off(kind, line, s) <= SIZE_SHARE
                {
                    return Some(s);
                }
                (0..kind.tiers.len()).filter(|t| off(kind, line, *t) <= SIZE_SHARE).min_by(|a, b| off(kind, line, *a).total_cmp(&off(kind, line, *b)))
            };
            let adopt = |kind: &mut Kind, fresh: &[usize]| {
                let refs: Vec<&Item> = fresh.iter().map(|i| &items[*i]).collect();
                for (tier, group) in tiers(&refs) {
                    for k in group {
                        kind.seen.insert(items[fresh[k]].source.clone(), kind.tiers.len());
                    }
                    kind.tiers.push(tier);
                }
            };
            let matched = members.iter().filter(|i| tier_of(&kind, **i).is_some()).count();
            if kind.tiers.is_empty() {
                adopt(&mut kind, &members);
            } else if matched == 0 {
                // (no item of the kind fits a locked size: perhaps the scale changed)
                match kind.since {
                    Some(since) if now.saturating_sub(since) >= SETTLE_MS => {
                        kind = Kind::default();
                        adopt(&mut kind, &members);
                    }
                    None => kind.since = Some(now),
                    _ => {}
                }
            } else {
                kind.since = None;
                // (with items fitting, one that doesn't is another size on the same screen: a title and its lines)
                let fresh: Vec<usize> = members.iter().copied().filter(|i| tier_of(&kind, *i).is_none()).collect();
                if !fresh.is_empty() {
                    adopt(&mut kind, &fresh);
                }
            }
            if kind.seen.len() > SEEN_LIMIT {
                kind.seen.clear();
            }
            for &i in &members {
                let line = items[i].line;
                let found = tier_of(&kind, i);
                let index = found.unwrap_or_else(|| (0..kind.tiers.len()).min_by(|a, b| off(&kind, line, *a).total_cmp(&off(&kind, line, *b))).unwrap_or(0));
                if found.is_some() {
                    kind.seen.insert(items[i].source.clone(), index);
                }
                let Some(locked) = kind.tiers.get(index).copied() else { continue };
                let (fg, bg) = self.palette(&items[i].fg, &items[i].bg, r);
                let o = &mut out[i];
                o.line = locked.line;
                o.bold = locked.bold;
                o.condensed = locked.condensed;
                o.fg = fg;
                o.bg = bg;
            }
            self.kinds.insert(r.to_string(), kind);
        }
        out
    }

    fn palette(&mut self, fg: &str, bg: &str, role: &str) -> (String, String) {
        let known = self.palettes.entry(role.to_string()).or_default();
        if let Some((f, b)) = known.iter().find(|(f, b)| near(b, bg) && near(f, fg)) {
            return (f.clone(), b.clone());
        }
        known.push((fg.to_string(), bg.to_string()));
        if known.len() > PALETTE_LIMIT {
            known.remove(0);
        }
        (fg.to_string(), bg.to_string())
    }

    /// The screen is gone: the next items are measured again.
    pub fn reset(&mut self) {
        self.kinds.clear();
        self.palettes.clear();
    }
}
