//! Snapshots: the game's whole picture translated at once, like a translated photo. Every bit of text on
//! the frame is read, grouped into blocks (a line, a paragraph, a menu item), translated, and drawn over
//! the picture by the page that shows it (ui/snapshot.js), each block in the colours it had.
//!
//! The services:
//! - the app's translators (local, DeepL, Azure, Google, LibreTranslate): block by block, the translation
//!   memory first, so the game's own words read the same as in the subtitles;
//! - Claude and ChatGPT: the picture and all its blocks in one request (the model sees the screen, fixes
//!   OCR misreads and translates with the whole screen as context);
//! - Azure image translation: the picture goes to Azure and comes back translated, as a picture;
//! - Gemini ("Nano Banana"): the picture goes to Google's image model and comes back redrawn, its text
//!   in the target language (it can change small details: a redrawn picture, not an edited one).
//!
//! Text a game page marks private (other players' names) is never read, sent or kept: its words are left
//! out, and covered in the picture a language model or Azure gets.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use anyhow::{Result, bail};
use base64::Engine as _;
use serde::Serialize;

use crate::frame::FrameData;
use crate::config::Config;
use crate::ocr::{self, Crop, Ocr, Word};
use crate::translate::{Segments, Translator};

/// One snapshot at a time (the app's button, the home-network page's).
static BUSY: AtomicBool = AtomicBool::new(false);

/// Held while a snapshot is made; a second one asked meanwhile is refused.
pub struct Busy(());

impl Busy {
    pub fn take() -> Option<Busy> {
        (!BUSY.swap(true, Ordering::AcqRel)).then_some(Busy(()))
    }
}

impl Drop for Busy {
    fn drop(&mut self) {
        BUSY.store(false, Ordering::Release);
    }
}

/// The translator snapshots use (its local model stays loaded between snapshots).
static SNAP_TR: std::sync::Mutex<Option<Translator>> = std::sync::Mutex::new(None);

/// A snapshot of `frame` as the app's Snapshot button makes it: one at a time, translated with the
/// snapshot service (the reviewed translations of the memory first), `private` covered.
pub fn now(frame: &FrameData, private: &[[f64; 4]], c: &Config, memory: &std::sync::Mutex<crate::memory::Memory>, paths: &crate::Paths, ocr: &dyn Fn() -> Result<Ocr>, id: String) -> Result<Snapshot> {
    let Some(_busy) = Busy::take() else { bail!("a snapshot is already being translated") };
    let mut slot = SNAP_TR.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let tr = slot.get_or_insert_with(|| Translator::new(paths.model_dir()));
    let lang = c.target_lang.clone();
    // (the reviewed translations only: the memory's own lines come from whatever service was in use, and
    // the snapshot has its own; looked at line by line, not held while a translator is asked)
    let lookup = |en: &str| -> Option<Segments> {
        let m = memory.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        m.get(en, &[], &[]).filter(|(_, src)| crate::memory::trusted(src)).map(|(segs, _)| crate::translate::polite(&lang, segs))
    };
    make(frame, private, Tools { cfg: c, tr, memory: &lookup, ocr }, id)
}

/// A block of text on the picture and its translation.
#[derive(Serialize, Clone, Debug)]
pub struct Block {
    pub en: String,
    /// the translation (colours as the translator kept them)
    pub segs: Segments,
    /// where it is: x, y, w, h as fractions of the picture
    pub rect: [f64; 4],
    /// the letters' height (fraction of the picture's height) and how many lines
    pub line_h: f64,
    pub lines: usize,
    /// "left" or "center" (how its lines line up)
    pub align: String,
    /// the letters' and the background's colours ("#rrggbb")
    pub fg: String,
    pub bg: String,
}

#[derive(Serialize, Clone, Debug)]
pub struct Snapshot {
    /// a name for its files ("2026-11-20_06-41-12")
    pub id: String,
    pub w: u32,
    pub h: u32,
    /// the picture (JPEG data URL, at most 1920 wide)
    pub image: String,
    /// the picture as the service translated it (Azure image translation): drawn instead of the blocks
    pub translated_image: Option<String>,
    pub blocks: Vec<Block>,
    /// the service used ("local", "claude", "azure-image" …) and the language
    pub service: String,
    pub lang: String,
    pub rtl: bool,
    pub ms: u64,
}

/// What a snapshot is made with: the settings, the translator kept for snapshots, a lookup in the
/// translation memory (English -> translation), and the host's OCR (made when the snapshot reads
/// the picture: not for the services that translate the picture themselves).
pub struct Tools<'a> {
    pub cfg: &'a Config,
    pub tr: &'a mut Translator,
    pub memory: &'a dyn Fn(&str) -> Option<Segments>,
    pub ocr: &'a dyn Fn() -> Result<Ocr>,
}

/// The service a snapshot uses: its own setting, or the live translations' service.
pub fn service(cfg: &Config) -> String {
    if cfg.snap_service.is_empty() { cfg.engine.clone() } else { cfg.snap_service.clone() }
}

/// Make a snapshot of `frame`, leaving out `private` (x, y, w, h fractions: other players' names).
pub fn make(frame: &FrameData, private: &[[f64; 4]], tools: Tools, id: String) -> Result<Snapshot> {
    let t0 = Instant::now();
    let cfg = tools.cfg;
    if frame.w < 64 || frame.h < 64 {
        bail!("no picture yet");
    }
    let service = service(cfg);
    let lang = crate::langs::get(&cfg.target_lang);
    // the private parts covered (dark) in the picture that is shown, sent and saved
    let frame = covered(frame, private);
    let image = jpeg_data_url(&frame, 1920, 88)?;

    if service == "azure-image" || service == "gemini-image" {
        let (bytes, mime) = if service == "gemini-image" {
            let jpeg = base64::engine::general_purpose::STANDARD.decode(jpeg_b64(&frame, 1920, 92)?)?;
            tools.tr.gemini_image(&cfg.snap_gemini_key, &cfg.snap_gemini_model, &cfg.target_lang, &jpeg, frame.w, frame.h)?
        } else {
            tools.tr.azure_image(&cfg.snap_azure_endpoint, &cfg.snap_azure_key, &cfg.target_lang, &png_bytes(&frame)?)?
        };
        let translated = format!("data:{mime};base64,{}", base64::engine::general_purpose::STANDARD.encode(&bytes));
        return Ok(Snapshot { id, w: frame.w, h: frame.h, image, translated_image: Some(translated), blocks: Vec::new(), service, lang: lang.code.into(), rtl: lang.rtl, ms: t0.elapsed().as_millis() as u64 });
    }

    // every word on the picture (the private ones left out), grouped into blocks
    let ocr = (tools.ocr)()?;
    let crop = Crop { w: frame.w, h: frame.h, rgba: frame.rgba.clone() };
    let words: Vec<Word> = ocr
        .read_screen(&crop)?
        .into_iter()
        .filter(|w| {
            let (cx, cy) = ((w.rect.0 as f64 + w.rect.2 as f64 / 2.0) / frame.w as f64, (w.rect.1 as f64 + w.rect.3 as f64 / 2.0) / frame.h as f64);
            !private.iter().any(|r| cx >= r[0] && cx <= r[0] + r[2] && cy >= r[1] && cy <= r[1] + r[3])
        })
        .collect();
    if std::env::var_os("VI_TRANSLATOR_DEBUG").is_some() {
        for w in &words {
            crate::vlog!("[snapshot] {:?} {:?}", w.text, w.rect);
        }
    }
    let mut blocks: Vec<Block> = Vec::new();
    for group in group_blocks(&words) {
        let en = ocr::clean(&group.words.iter().map(|&i| words[i].clone()).collect::<Vec<_>>());
        // text worth translating: real words (not "4.7", "$315", "R3")
        if en.chars().filter(|c| c.is_alphabetic()).count() < 2 || !ocr::looks_like_words(&en) {
            continue;
        }
        let (fg, bg) = colors(&crop, group.rect, group.line_h);
        let (fw, fh) = (frame.w as f64, frame.h as f64);
        blocks.push(Block {
            en,
            segs: Vec::new(),
            rect: [group.rect.0 as f64 / fw, group.rect.1 as f64 / fh, group.rect.2 as f64 / fw, group.rect.3 as f64 / fh],
            line_h: group.line_h as f64 / fh,
            lines: group.lines,
            align: group.align.into(),
            fg,
            bg,
        });
    }

    // translated
    if crate::translate::is_llm(&service) {
        let mut c = cfg.clone();
        c.engine = service.clone();
        // (the picture the model gets: at most 1568 px on its long side, as the models read it)
        let small = jpeg_b64(&frame, 1568, 85)?;
        let list: Vec<(usize, String)> = blocks.iter().enumerate().map(|(i, b)| (i + 1, b.en.clone())).collect();
        if !list.is_empty() {
            let got = tools.tr.picture(&c, &small, &list)?;
            for (i, b) in blocks.iter_mut().enumerate() {
                if let Some(t) = got.get(&(i + 1)).filter(|t| !t.is_empty()) {
                    b.segs = vec![(t.clone(), None)];
                }
            }
        }
    } else {
        let mut c = cfg.clone();
        c.engine = service.clone();
        let mut done: HashMap<String, Segments> = HashMap::new();
        for b in blocks.iter_mut() {
            if let Some(s) = done.get(&b.en) {
                b.segs = s.clone();
                continue;
            }
            let plain = sentence_case(&b.en);
            let debug = std::env::var_os("VI_TRANSLATOR_DEBUG").is_some();
            let segs = match (tools.memory)(&b.en).or_else(|| (tools.memory)(&plain)) {
                Some(s) => {
                    if debug {
                        crate::vlog!("[snapshot] memory: {}", b.en);
                    }
                    s
                }
                None => {
                    if debug {
                        crate::vlog!("[snapshot] {}: {plain}", c.engine);
                    }
                    tools.tr.translate(&c, &plain, &[], &[], &|_| {})?
                }
            };
            done.insert(b.en.clone(), segs.clone());
            b.segs = segs;
        }
    }
    blocks.retain(|b| b.segs.iter().any(|s| !s.0.trim().is_empty()));
    Ok(Snapshot { id, w: frame.w, h: frame.h, image, translated_image: None, blocks, service, lang: lang.code.into(), rtl: lang.rtl, ms: t0.elapsed().as_millis() as u64 })
}

/// A menu's ALL-CAPS label in ordinary case ("ASPHALT HANDLING" -> "Asphalt handling"): translators
/// read capitals as names or acronyms. Short words in capitals (VCPD, GTA) stay as they are.
pub fn sentence_case(en: &str) -> String {
    let letters: Vec<char> = en.chars().filter(|c| c.is_alphabetic()).collect();
    if letters.len() < 4 || letters.iter().any(|c| c.is_lowercase()) {
        return en.to_string();
    }
    let mut first = true;
    en.split(' ')
        .map(|w| {
            let n = w.chars().filter(|c| c.is_alphabetic()).count();
            let out = if n <= 3 && !first && n > 0 && !matches!(w.to_lowercase().as_str(), "the" | "and" | "for" | "of" | "to" | "a" | "in" | "on" | "off") {
                w.to_string()
            } else if first {
                let low = w.to_lowercase();
                let mut cs = low.chars();
                cs.next().map(|c| c.to_uppercase().collect::<String>() + cs.as_str()).unwrap_or_default()
            } else {
                w.to_lowercase()
            };
            if n > 0 {
                first = false;
            }
            out
        })
        .collect::<Vec<_>>()
        .join(" ")
}

// ------------------------------------------------------------------------------------- blocks

/// A block of words: which words, where (pixels), its letters' height, lines and alignment.
#[derive(Debug)]
pub struct Group {
    pub words: Vec<usize>,
    pub rect: (u32, u32, u32, u32),
    pub line_h: u32,
    pub lines: usize,
    pub align: &'static str,
}

struct Line {
    words: Vec<usize>,
    x0: u32,
    y0: u32,
    x1: u32,
    y1: u32,
}

impl Line {
    fn h(&self) -> u32 {
        self.y1 - self.y0
    }
}

/// Words -> lines (side by side, the same height, no wide gap: a gap of more than two and a half
/// letters' height starts another line, so a menu's items on one row stay apart) -> blocks (lines stacked close
/// together, about the same height, lined up on the left or the middle).
pub fn group_blocks(words: &[Word]) -> Vec<Group> {
    let mut order: Vec<usize> = (0..words.len()).filter(|&i| words[i].rect.3 > 0).collect();
    order.sort_by_key(|&i| (words[i].rect.1 + words[i].rect.3 / 2, words[i].rect.0));
    let mut lines: Vec<Line> = Vec::new();
    for i in order {
        let (x, y, w, h) = words[i].rect;
        let found = lines.iter_mut().find(|l| {
            let lh = l.h().max(1);
            let overlap = (y + h).min(l.y1) as i64 - y.max(l.y0) as i64;
            let ratio = h.max(lh) as f64 / h.min(lh).max(1) as f64;
            let gap = if x >= l.x1 { x - l.x1 } else if x + w <= l.x0 { l.x0 - (x + w) } else { 0 };
            overlap as f64 >= 0.5 * h.min(lh) as f64 && ratio <= 1.6 && gap as f64 <= 2.5 * h.max(lh) as f64
        });
        match found {
            Some(l) => {
                l.words.push(i);
                l.x0 = l.x0.min(x);
                l.x1 = l.x1.max(x + w);
                l.y0 = l.y0.min(y);
                l.y1 = l.y1.max(y + h);
            }
            None => lines.push(Line { words: vec![i], x0: x, y0: y, x1: x + w, y1: y + h }),
        }
    }
    // (words were taken top to bottom, so a line can have started before the word between it and
    // another was seen: lines that now meet the same test are joined)
    let near = |a: &Line, b: &Line| {
        let (ha, hb) = (a.h().max(1), b.h().max(1));
        let overlap = a.y1.min(b.y1) as i64 - a.y0.max(b.y0) as i64;
        let ratio = ha.max(hb) as f64 / ha.min(hb) as f64;
        let gap = if b.x0 >= a.x1 { b.x0 - a.x1 } else if a.x0 >= b.x1 { a.x0 - b.x1 } else { 0 };
        overlap as f64 >= 0.5 * ha.min(hb) as f64 && ratio <= 1.6 && gap as f64 <= 2.5 * ha.max(hb) as f64
    };
    'join: loop {
        for i in 0..lines.len() {
            for j in i + 1..lines.len() {
                if near(&lines[i], &lines[j]) {
                    let b = lines.remove(j);
                    let a = &mut lines[i];
                    a.words.extend(b.words);
                    a.x0 = a.x0.min(b.x0);
                    a.x1 = a.x1.max(b.x1);
                    a.y0 = a.y0.min(b.y0);
                    a.y1 = a.y1.max(b.y1);
                    continue 'join;
                }
            }
        }
        break;
    }
    for l in lines.iter_mut() {
        l.words.sort_by_key(|&i| words[i].rect.0);
    }
    lines.sort_by_key(|l| (l.y0, l.x0));

    let mut blocks: Vec<Vec<Line>> = Vec::new();
    for line in lines {
        let h = line.h().max(1) as f64;
        let found = blocks.iter_mut().find(|b| {
            let last = b.last().unwrap();
            let lh = last.h().max(1) as f64;
            let gap = line.y0 as f64 - last.y1 as f64;
            let ratio = h.max(lh) / h.min(lh);
            let (bx0, bx1) = (b.iter().map(|l| l.x0).min().unwrap() as f64, b.iter().map(|l| l.x1).max().unwrap() as f64);
            let overlap = (line.x1 as f64).min(bx1) - (line.x0 as f64).max(bx0);
            let left = (line.x0 as f64 - bx0).abs() <= 1.5 * lh;
            let middle = ((line.x0 + line.x1) as f64 / 2.0 - (bx0 + bx1) / 2.0).abs() <= 1.5 * lh;
            gap >= -0.3 * lh && gap <= 0.8 * lh && ratio <= 1.35 && overlap > 0.0 && (left || middle)
        });
        match found {
            Some(b) => b.push(line),
            None => blocks.push(vec![line]),
        }
    }

    blocks
        .into_iter()
        .map(|b| {
            let x0 = b.iter().map(|l| l.x0).min().unwrap();
            let y0 = b.iter().map(|l| l.y0).min().unwrap();
            let x1 = b.iter().map(|l| l.x1).max().unwrap();
            let y1 = b.iter().map(|l| l.y1).max().unwrap();
            let mut hs: Vec<u32> = b.iter().map(|l| l.h()).collect();
            hs.sort_unstable();
            let line_h = hs[hs.len() / 2];
            // centred lines: their middles agree better than their left edges
            let align = if b.len() > 1 {
                let spread = |v: Vec<f64>| v.iter().cloned().fold(f64::MIN, f64::max) - v.iter().cloned().fold(f64::MAX, f64::min);
                let lefts = spread(b.iter().map(|l| l.x0 as f64).collect());
                let mids = spread(b.iter().map(|l| (l.x0 + l.x1) as f64 / 2.0).collect());
                if mids + 2.0 < lefts { "center" } else { "left" }
            } else {
                "left"
            };
            Group { words: b.iter().flat_map(|l| l.words.iter().copied()).collect(), rect: (x0, y0, x1 - x0, y1 - y0), line_h, lines: b.len(), align }
        })
        .collect()
}

// ------------------------------------------------------------------------------------- colours

/// The background around a block (the median of a ring just outside it) and its letters' colour (the
/// median of the pixels inside that differ most from that background).
fn colors(crop: &Crop, r: (u32, u32, u32, u32), line_h: u32) -> (String, String) {
    let pad = (line_h / 3).max(3);
    let (x0, y0) = (r.0.saturating_sub(pad), r.1.saturating_sub(pad));
    let (x1, y1) = ((r.0 + r.2 + pad).min(crop.w - 1), (r.1 + r.3 + pad).min(crop.h - 1));
    let mut ring: Vec<(i32, i32, i32)> = Vec::new();
    let step = ((x1 - x0) / 60).max(1);
    for x in (x0..=x1).step_by(step as usize) {
        ring.push(crop.px(x, y0));
        ring.push(crop.px(x, y1));
    }
    for y in (y0..=y1).step_by(((y1 - y0) / 20).max(1) as usize) {
        ring.push(crop.px(x0, y));
        ring.push(crop.px(x1, y));
    }
    let bg = median(&mut ring);
    let mut inside: Vec<((i32, i32, i32), i32)> = Vec::new();
    for y in (r.1..(r.1 + r.3).min(crop.h)).step_by(1) {
        for x in (r.0..(r.0 + r.2).min(crop.w)).step_by(2) {
            let p = crop.px(x, y);
            let d = (p.0 - bg.0).abs() + (p.1 - bg.1).abs() + (p.2 - bg.2).abs();
            inside.push((p, d));
        }
    }
    inside.sort_by_key(|(_, d)| -d);
    let mut top: Vec<(i32, i32, i32)> = inside.iter().take((inside.len() / 8).max(1)).map(|(p, _)| *p).collect();
    let fg = if top.is_empty() || inside[0].1 < 60 {
        // (no clear letters: white on dark, black on light)
        if bg.0 + bg.1 + bg.2 > 382 { (0, 0, 0) } else { (255, 255, 255) }
    } else {
        median(&mut top)
    };
    (hex(fg), hex(bg))
}

fn median(v: &mut [(i32, i32, i32)]) -> (i32, i32, i32) {
    if v.is_empty() {
        return (0, 0, 0);
    }
    let c = |f: fn(&(i32, i32, i32)) -> i32| {
        let mut x: Vec<i32> = v.iter().map(f).collect();
        x.sort_unstable();
        x[x.len() / 2]
    };
    (c(|p| p.0), c(|p| p.1), c(|p| p.2))
}

fn hex(c: (i32, i32, i32)) -> String {
    format!("#{:02x}{:02x}{:02x}", c.0.clamp(0, 255), c.1.clamp(0, 255), c.2.clamp(0, 255))
}

// ------------------------------------------------------------------------------------- pictures

/// The picture with the private parts (fractions) covered.
pub fn covered(frame: &FrameData, private: &[[f64; 4]]) -> FrameData {
    let mut rgba = frame.rgba.clone();
    for r in private {
        cover(&mut rgba, frame.w, frame.h, *r);
    }
    FrameData { w: frame.w, h: frame.h, rgba, seq: frame.seq }
}

/// Cover a part of the picture (fractions) in near-black.
fn cover(rgba: &mut [u8], w: u32, h: u32, r: [f64; 4]) {
    let (x0, y0) = ((r[0].clamp(0.0, 1.0) * w as f64) as u32, (r[1].clamp(0.0, 1.0) * h as f64) as u32);
    let (x1, y1) = (((r[0] + r[2]).clamp(0.0, 1.0) * w as f64) as u32, ((r[1] + r[3]).clamp(0.0, 1.0) * h as f64) as u32);
    for y in y0..y1 {
        for x in x0..x1 {
            let i = ((y * w + x) * 4) as usize;
            rgba[i..i + 3].copy_from_slice(&[18, 18, 24]);
        }
    }
}

fn scaled(frame: &FrameData, max_w: u32) -> image::RgbImage {
    let rgb: Vec<u8> = frame.rgba.chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]]).collect();
    let img = image::RgbImage::from_raw(frame.w, frame.h, rgb).unwrap_or_default();
    if frame.w > max_w {
        let h = (frame.h as f64 * max_w as f64 / frame.w as f64).round() as u32;
        image::imageops::resize(&img, max_w, h.max(1), image::imageops::FilterType::Triangle)
    } else {
        img
    }
}

fn jpeg_b64(frame: &FrameData, max_w: u32, quality: u8) -> Result<String> {
    let img = scaled(frame, max_w);
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality).encode_image(&img)?;
    Ok(base64::engine::general_purpose::STANDARD.encode(&out))
}

pub fn jpeg_data_url(frame: &FrameData, max_w: u32, quality: u8) -> Result<String> {
    Ok(format!("data:image/jpeg;base64,{}", jpeg_b64(frame, max_w, quality)?))
}

/// The picture as PNG (full size).
pub fn png_bytes(frame: &FrameData) -> Result<Vec<u8>> {
    let img = scaled(frame, u32::MAX);
    let mut out = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)?;
    Ok(out)
}

/// A data URL's bytes ("data:image/png;base64,…").
pub fn data_url_bytes(url: &str) -> Option<(Vec<u8>, &'static str)> {
    let (head, b64) = url.split_once(',')?;
    let ext = if head.contains("image/png") { "png" } else if head.contains("image/jpeg") { "jpg" } else if head.contains("image/webp") { "webp" } else if head.contains("image/bmp") { "bmp" } else { return None };
    Some((base64::engine::general_purpose::STANDARD.decode(b64).ok()?, ext))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(text: &str, x: u32, y: u32, width: u32, h: u32) -> Word {
        Word { text: text.into(), rect: (x, y, width, h), line: 0 }
    }

    #[test]
    fn a_paragraph_is_one_block_and_a_menu_row_is_split() {
        let words = vec![
            // a paragraph of two lines
            w("Give", 30, 100, 50, 24), w("your", 86, 100, 50, 24), w("vehicle", 142, 100, 80, 24),
            w("a", 30, 130, 12, 24), w("boost.", 48, 130, 70, 24),
            // a menu row: three items far apart
            w("Map", 500, 300, 50, 24), w("Game", 700, 300, 60, 24), w("Store", 900, 300, 60, 24),
        ];
        let g = group_blocks(&words);
        assert_eq!(g.len(), 4);
        assert_eq!(g[0].words, vec![0, 1, 2, 3, 4]);
        assert_eq!(g[0].lines, 2);
        assert_eq!(g[1].words, vec![5]);
    }

    #[test]
    fn capitals_become_ordinary_case() {
        assert_eq!(sentence_case("ASPHALT HANDLING"), "Asphalt handling");
        assert_eq!(sentence_case("OFF-ROAD HANDLING"), "Off-road handling");
        assert_eq!(sentence_case("CALL THE FIB"), "Call the FIB");
        assert_eq!(sentence_case("Full Engine"), "Full Engine");
        assert_eq!(sentence_case("GTA"), "GTA");
    }

    #[test]
    fn a_title_does_not_join_the_smaller_text_under_it() {
        let words = vec![w("COASTAL", 30, 30, 140, 40), w("AUTO", 180, 30, 80, 40), w("Full", 30, 80, 40, 22), w("Engine", 76, 80, 70, 22)];
        let g = group_blocks(&words);
        assert_eq!(g.len(), 2);
    }
}
