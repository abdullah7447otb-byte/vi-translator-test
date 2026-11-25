//! OCR engine on Linux: Tesseract (libtesseract with the English data, e.g. Debian/Ubuntu
//! packages `tesseract-ocr` and `tesseract-ocr-eng`).

use std::sync::Mutex;

use anyhow::{Context as _, Result, anyhow};
use tesseract::Tesseract;

use vi_translator_core::ocr::RawWord;

pub struct Engine {
    /// one Tesseract instance, kept between reads (loading the English model takes a while); its
    /// calls consume it, so it's taken out and put back each time
    tess: Mutex<Option<Tesseract>>,
}

fn create() -> Result<Tesseract> {
    Tesseract::new(None, Some("eng")).context("Tesseract with English is not installed (packages tesseract-ocr, tesseract-ocr-eng)")
}

impl Engine {
    pub fn new() -> Result<Self> {
        Ok(Self { tess: Mutex::new(Some(create()?)) })
    }

    pub fn init_thread() {}

    pub fn max_dim() -> u32 {
        4000
    }

    /// Words in a dark-on-light image (4 bytes per pixel; the prepared image is grey, so the
    /// channel order doesn't matter), with their line numbers and boxes (image pixels).
    pub fn read(&self, bgra: &[u8], w: u32, h: u32) -> Result<Vec<RawWord>> {
        let mut slot = self.tess.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let t = match slot.take() {
            Some(t) => t,
            None => create()?,
        };
        let mut t = t
            .set_frame(bgra, w as i32, h as i32, 4, (w * 4) as i32)
            .map_err(|e| anyhow!("OCR: {e}"))?
            .recognize()
            .map_err(|e| anyhow!("OCR: {e}"))?;
        let tsv = t.get_tsv_text(0).map_err(|e| anyhow!("OCR: {e}"));
        *slot = Some(t);
        Ok(parse_tsv(&tsv?))
    }
}

/// Tesseract's TSV: level, page, block, par, line, word, left, top, width, height, conf, text.
/// Level 5 rows are words; (block, par, line) numbers the lines, renumbered 0, 1, 2 … in order.
fn parse_tsv(tsv: &str) -> Vec<RawWord> {
    let mut words = Vec::new();
    let mut lines: Vec<(u32, u32, u32)> = Vec::new();
    for row in tsv.lines() {
        let c: Vec<&str> = row.split('\t').collect();
        if c.len() < 12 || c[0] != "5" {
            continue;
        }
        let text = c[11].trim();
        let conf: f64 = c[10].parse().unwrap_or(-1.0);
        if text.is_empty() || conf < 0.0 {
            continue;
        }
        let n = |i: usize| c[i].parse::<u32>().unwrap_or(0);
        let key = (n(2), n(3), n(4));
        let line = match lines.iter().position(|l| *l == key) {
            Some(i) => i,
            None => {
                lines.push(key);
                lines.len() - 1
            }
        };
        words.push(RawWord {
            line: line as u32,
            text: text.to_string(),
            x: n(6) as f64,
            y: n(7) as f64,
            w: n(8) as f64,
            h: n(9) as f64,
        });
    }
    words
}

#[cfg(test)]
mod tests {
    #[test]
    fn reads_tsv_words_and_lines() {
        let tsv = "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n\
                   4\t1\t1\t1\t1\t0\t10\t5\t200\t30\t-1\t\n\
                   5\t1\t1\t1\t1\t1\t10\t5\t60\t30\t95.5\tPress\n\
                   5\t1\t1\t1\t1\t2\t80\t5\t30\t30\t91\tto\n\
                   5\t1\t1\t1\t2\t1\t10\t50\t80\t30\t90\tjump.\n";
        let w = super::parse_tsv(tsv);
        assert_eq!(w.len(), 3);
        assert_eq!((w[0].text.as_str(), w[0].line, w[0].x, w[0].w), ("Press", 0, 10.0, 60.0));
        assert_eq!((w[2].text.as_str(), w[2].line, w[2].y), ("jump.", 1, 50.0));
    }
}
