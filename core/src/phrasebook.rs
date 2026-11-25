//! Phrasebook: exact, hand-written translations of a game's common lines, from its layout update
//! (`"phrases": {"<lang>": {"Press {1} to take cover.": "…"}}`).
//!
//! Machine translators handle short commands badly ("take cover" -> "to cover it"), and games use a
//! fixed set of them, so known lines get a hand-written translation. `{1}`, `{2}` mark the
//! controller button icons; the translation puts each icon where it belongs in the sentence.
//! Lines not in the book go to the translator as usual.

use std::sync::RwLock;

use crate::translate::Segments;

/// Phrases from the game's layout update (any game, any language): (game, language, phrases).
static EXTRA: RwLock<(String, String, Vec<(String, String)>)> = RwLock::new((String::new(), String::new(), Vec::new()));

/// The phrases a layout update brings for this game and language (`"phrases": {"ar": {"Press {1} to …": "…"}}`).
pub fn set_extra(game: &str, lang: &str, phrases: Vec<(String, String)>) {
    *EXTRA.write().unwrap_or_else(std::sync::PoisonError::into_inner) = (game.to_string(), lang.to_string(), phrases);
}

/// The hand-written translation of a line for this game and language, if its layout update has one.
pub fn find(game: &str, lang: &str, line: &str, icons: &[String]) -> Option<Segments> {
    let extra = EXTRA.read().unwrap_or_else(std::sync::PoisonError::into_inner);
    if extra.0 != game || extra.1 != lang || extra.2.is_empty() {
        return None;
    }
    best(extra.2.iter().map(|(e, a)| (e.as_str(), a.as_str())), line, icons)
}

fn normalize_line(line: &str) -> String {
    let mut out = Vec::new();
    let mut icon = 0;
    for tok in line.split_whitespace() {
        let core = tok.trim_matches(|c: char| !c.is_alphanumeric() && c != '⟦' && c != '⟧'); // "⟦1⟧." is still an icon
        if core.starts_with('⟦') && core.ends_with('⟧') {
            icon += 1;
            out.push(format!("{{{icon}}}"));
            continue;
        }
        let w: String = tok.chars().filter(|c| c.is_alphanumeric() || *c == '\'' || *c == '+').collect::<String>().to_lowercase();
        if !w.is_empty() {
            out.push(w);
        }
    }
    out.join(" ")
}

/// A book line's English, normalised like an OCR line: icons as "{n}" (the layout's notation) or
/// "⟦n⟧" (the review exports' and the editor's), inside punctuation too ("({1})").
fn normalize_template(t: &str) -> String {
    normalize_line(&t.replace('{', "⟦").replace('}', "⟧"))
}

/// Edit-distance similarity 0..1.
fn similarity(a: &str, b: &str) -> f64 {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut cur = vec![i; b.len() + 1];
        for j in 1..=b.len() {
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + usize::from(a[i - 1] != b[j - 1]));
        }
        prev = cur;
    }
    1.0 - prev[b.len()] as f64 / a.len().max(b.len()) as f64
}

/// The book's translation for this English line (with `⟦n⟧` icon placeholders), as segments with the
/// icons put back in. Exact match first; then a close match (≥ 90 %, same icons) to tolerate small
/// OCR slips like a dropped letter. Matching ignores case and punctuation; `{n}` = n-th icon.
fn best<'a>(phrases: impl Iterator<Item = (&'a str, &'a str)>, line: &str, icons: &[String]) -> Option<Segments> {
    let norm = normalize_line(line);
    if norm.is_empty() {
        return None;
    }
    let n_icons = norm.matches('{').count();
    let mut best: Option<(f64, &str)> = None;
    for (en, ar) in phrases {
        let t = normalize_template(en);
        if t.matches('{').count() != n_icons {
            continue;
        }
        let sim = if t == norm { 1.0 } else { similarity(&t, &norm) };
        if sim >= 0.9 && best.is_none_or(|(s, _)| sim > s) {
            best = Some((sim, ar));
        }
    }
    let (_, ar) = best?;
    Some(fill(ar, icons))
}

/// "اضغط {1} للاحتماء." -> segments, with icon n taken from `icons[n-1]` ("⟦n⟧" too). Like the
/// packs' templates: "<cK>…</cK>" keeps its words (coloured afterwards, see `Translator::recolor`).
fn fill(ar: &str, icons: &[String]) -> Segments {
    crate::memory::fill(&ar.replace('⟦', "{").replace('⟧', "}"), icons, &[])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(segs: &Segments) -> String {
        segs.iter().map(|(t, c)| if c.is_some() { "🎮".to_string() } else { t.clone() }).collect()
    }
    fn icons(n: usize) -> Vec<String> {
        (1..=n).map(|i| format!("0.8|icon{i}")).collect()
    }

    const BOOK: [(&str, &str); 4] = [
        ("Press {1} to take cover.", "اضغط {1} للاحتماء."),
        ("Press {1} to reload a weapon.", "اضغط {1} لإعادة تعبئة السلاح."),
        ("Use {1} to accelerate and {2} to brake or reverse.", "استخدم {1} للتسارع و{2} للفرملة أو الرجوع."),
        ("Aim using {1}", "صوّب باستخدام {1}"),
    ];
    fn look(en: &str, icons: &[String]) -> Option<Segments> {
        best(BOOK.iter().copied(), en, icons)
    }

    #[test]
    fn lines_get_their_hand_written_translation() {
        assert_eq!(text(&look("Press ⟦1⟧ to take cover", &icons(1)).unwrap()), "اضغط 🎮 للاحتماء.");
        assert_eq!(text(&look("Aim using ⟦1⟧", &icons(1)).unwrap()), "صوّب باستخدام 🎮");
    }

    #[test]
    fn small_ocr_slips_still_match() {
        assert!(look("Press ⟦1⟧ to reload a wepon", &icons(1)).is_some());
        assert!(look("press ⟦1⟧ to take cover..", &icons(1)).is_some());
    }

    #[test]
    fn unknown_lines_and_wrong_icon_counts_dont_match() {
        assert!(look("Blame the pricks who called you out here!", &[]).is_none());
        assert!(look("Press to take cover", &[]).is_none()); // icon missing: don't invent one
        assert!(look("Press ⟦1⟧ to call Lamar.", &icons(1)).is_none());
    }

    #[test]
    fn the_layouts_phrases_for_its_game_and_language() {
        set_extra("rdr2", "fr", vec![("Press {1} to mount.".into(), "Appuyez sur {1} pour monter.".into())]);
        assert_eq!(text(&find("rdr2", "fr", "Press ⟦1⟧ to mount", &icons(1)).unwrap()), "Appuyez sur 🎮 pour monter.");
        assert!(find("rdr2", "ar", "Press ⟦1⟧ to mount", &icons(1)).is_none(), "another language");
        assert!(find("gta6", "fr", "Press ⟦1⟧ to mount", &icons(1)).is_none(), "another game");
    }

    #[test]
    fn keys_and_translations_in_the_exports_notation_work_too() {
        let book = [("Press ⟦1⟧ to take cover.", "اضغط ⟦1⟧ <c1>للاحتماء</c1>."), ("Hold ({1}) to run.", "اضغط مطولًا ({1}) للركض.")];
        assert_eq!(text(&best(book.iter().copied(), "Press ⟦1⟧ to take cover", &icons(1)).unwrap()), "اضغط 🎮 للاحتماء.");
        assert_eq!(text(&best(book.iter().copied(), "Hold (⟦1⟧) to run.", &icons(1)).unwrap()), "اضغط مطولًا (🎮) للركض.");
        assert!(best(book.iter().copied(), "Press to take cover", &[]).is_none());
    }

    #[test]
    fn icons_land_in_order() {
        let segs = look("Use ⟦1⟧ to accelerate and ⟦2⟧ to brake or reverse.", &icons(2)).unwrap();
        let order: Vec<&str> = segs.iter().filter_map(|(_, c)| c.as_deref()).collect();
        assert_eq!(order, vec!["icon:0.8|icon1", "icon:0.8|icon2"]);
    }
}
