//! English words, to tell a new line the game shows (all real words) from an OCR misreading of one ("Drive
//! tofthe pr.iSoi7"). The word list is dwyl/english-words (public domain, see THIRD-PARTY-NOTICES.md),
//! words of up to 14 letters, loaded on first use. The game's names (people, places, brands) come from the
//! glossary and the speaker names, and any capitalised word that looks like a name counts too.

use std::collections::HashSet;
use std::sync::OnceLock;

static WORDS: OnceLock<HashSet<String>> = OnceLock::new();
const PACKED: &[u8] = include_bytes!("words_en.txt.gz");

fn words() -> &'static HashSet<String> {
    WORDS.get_or_init(|| {
        use std::io::Read;
        let mut text = String::new();
        let _ = flate2::read::GzDecoder::new(PACKED).read_to_string(&mut text);
        let mut set: HashSet<String> = text.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect();
        // (the game's names: the glossary's English and the speakers')
        for src in [include_str!("glossary_ar.tsv"), include_str!("glossary_ar_lines.tsv"), include_str!("speaker_names_ar.tsv")] {
            for l in src.lines().filter(|l| !l.starts_with('#')) {
                let en = l.split('\t').next().unwrap_or("");
                for w in en.split(|c: char| !c.is_alphabetic()) {
                    if w.len() >= 2 {
                        set.insert(w.to_lowercase());
                    }
                }
            }
        }
        set
    })
}

/// A word of English or a name the game uses ("RydeMe", "Leonida"). Endings "'s", "'ll", "n't" and the like
/// are taken off first.
pub fn is_word(w: &str) -> bool {
    let w = w.trim_matches(|c: char| !c.is_alphanumeric());
    if w.is_empty() {
        return false;
    }
    let lower = w.to_lowercase();
    let base = ["'s", "’s", "'ll", "'re", "'ve", "'d", "'m", "n't", "n’t"].iter().find_map(|e| lower.strip_suffix(e)).unwrap_or(&lower);
    let base = if base == "ca" || base == "wo" { "can" } else { base };
    if base.chars().all(|c| c.is_ascii_digit()) {
        return true;
    }
    if !base.chars().all(|c| c.is_alphabetic()) {
        return false;
    }
    words().contains(base)
}

/// The share (0..1) of a line's words that are words: 1 for a line read right. Button-icon placeholders
/// ("⟦1⟧") and lone punctuation don't count either way.
pub fn word_share(text: &str) -> f64 {
    let toks: Vec<&str> = text
        .split(|c: char| c.is_whitespace() || "/,;:!?()[]\"…—–".contains(c))
        .map(|t| t.trim_matches(|c: char| ".'’-".contains(c)))
        .filter(|t| !t.is_empty() && !t.contains('⟦') && t.chars().any(|c| c.is_alphanumeric()))
        .collect();
    if toks.is_empty() {
        return 0.0;
    }
    let good = toks.iter().filter(|t| is_word(t)).count();
    good as f64 / toks.len() as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_lines_are_words_and_misreadings_arent() {
        for l in ["Drive to the prison.", "Lose the cops.", "Follow Lucia to the car.", "Don't let him get away!", "Go to Leonida Keys.", "Press ⟦1⟧ to sprint.", "Get in the truck"] {
            assert!(word_share(l) >= 0.99, "{l}: {}", word_share(l));
        }
        for l in ["Drive tofthe pr.iSoi7", "Getin a vehigle", "ulnJiJl tx myeleg"] {
            assert!(word_share(l) < 0.9, "{l}: {}", word_share(l));
        }
        assert_eq!(word_share(""), 0.0);
    }
}
