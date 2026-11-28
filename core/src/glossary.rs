//! Words the free offline translator gets wrong in GTA VI, and its Arabic tidied after it.
//!
//! The glossary: whole lines the game shows on their own ("Wasted", "Mission Passed") and names
//! (places, brands) with their Arabic. A whole line is answered from it as it is; a name inside a
//! line goes to the model as "[n]" — the one form it never changes — and comes back in Arabic. The
//! player's own lines (Translator tab) come first, then the built-in ones (glossary_ar.tsv). Only
//! for the offline model: a language model or a machine translator knows these, and a placeholder
//! would cost it the sentence's grammar.
//!
//! The tidy-up: what the model adds that the game's English hasn't — brackets round names, a full
//! stop at the end, a word said twice — and Arabic punctuation (، ؟ ؛) in Arabic text.

use std::sync::RwLock;

const BUILT_IN: &str = include_str!("glossary_ar.tsv");
/// Lines the game shows on their own only (the map's legend): never taken out of a sentence.
const LINES: &str = include_str!("glossary_ar_lines.tsv");

/// The player's own lines, "English = Arabic" one a line (the setting), parsed when it changes.
static PLAYER: RwLock<(String, Vec<(String, String)>)> = RwLock::new((String::new(), Vec::new()));

fn parse_tsv(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.split_once('\t'))
        .map(|(en, ar)| (en.trim().to_string(), ar.trim().to_string()))
        .filter(|(en, ar)| !en.is_empty() && !ar.is_empty())
        .collect()
}

/// "English = Arabic" lines (the Translator tab's box).
pub fn parse_player(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|l| l.split_once('='))
        .map(|(en, ar)| (en.trim().to_string(), ar.trim().to_string()))
        .filter(|(en, ar)| !en.is_empty() && !ar.is_empty())
        .collect()
}

fn player(setting: &str) -> Vec<(String, String)> {
    {
        let g = PLAYER.read().unwrap_or_else(std::sync::PoisonError::into_inner);
        if g.0 == setting {
            return g.1.clone();
        }
    }
    let parsed = parse_player(setting);
    *PLAYER.write().unwrap_or_else(std::sync::PoisonError::into_inner) = (setting.to_string(), parsed.clone());
    parsed
}

/// The built-in names and lines (for the Translator tab's list: what the player can see and override).
pub fn built_in_list() -> Vec<(String, String)> {
    built_in().to_vec()
}

fn built_in() -> &'static [(String, String)] {
    static B: std::sync::OnceLock<Vec<(String, String)>> = std::sync::OnceLock::new();
    B.get_or_init(|| parse_tsv(BUILT_IN))
}

/// The entries in force: the player's, then the built-in ones (an English the player has is theirs).
fn entries(setting: &str) -> Vec<(String, String)> {
    let mine = player(setting);
    let mut all = mine.clone();
    all.extend(built_in().iter().filter(|(en, _)| !mine.iter().any(|(m, _)| m.eq_ignore_ascii_case(en))).cloned());
    all
}

fn plain(t: &str) -> String {
    t.trim().trim_end_matches(['.', '!', '?', ':']).trim().to_lowercase()
}

/// The Arabic for a whole line the glossary has (its case and a full stop at its end aside).
pub fn whole_line(setting: &str, line: &str) -> Option<String> {
    let want = plain(line);
    static L: std::sync::OnceLock<Vec<(String, String)>> = std::sync::OnceLock::new();
    entries(setting).into_iter().chain(L.get_or_init(|| parse_tsv(LINES)).iter().cloned()).find(|(en, _)| plain(en) == want).map(|(_, ar)| ar)
}

/// `line` with each glossary name in it (a whole word or words, any case) as "[n]", numbered after
/// the `taken` placeholders already in it; and the Arabic for each, in order.
pub fn protect(setting: &str, line: &str, taken: usize) -> (String, Vec<String>) {
    let mut out = line.to_string();
    let mut arabic = Vec::new();
    // (the longest first: "Vice City Port" before "Vice City")
    let mut list = entries(setting);
    list.sort_by_key(|(en, _)| std::cmp::Reverse(en.len()));
    for (en, ar) in list {
        if en.split_whitespace().count() == 0 {
            continue;
        }
        let lower = out.to_lowercase();
        let find = en.to_lowercase();
        let mut at = 0;
        while let Some(i) = lower[at..].find(&find).map(|i| i + at) {
            let end = i + find.len();
            let before = lower[..i].chars().next_back();
            let after = lower[end..].chars().next();
            let edge = |c: Option<char>| c.is_none_or(|c| !c.is_alphanumeric() && c != '\'');
            // (a name the glossary writes with a capital is a name only where the line writes it so: "Downtown"
            // is the district, "meet me downtown" a word)
            let named = !en.chars().next().is_some_and(char::is_uppercase) || out[i..].chars().next().is_some_and(char::is_uppercase);
            if edge(before) && edge(after) && out.is_char_boundary(i) && out.is_char_boundary(end) && named {
                arabic.push(ar.clone());
                let mark = format!("[{}]", taken + arabic.len());
                out.replace_range(i..end, &mark);
                break; // (once a line: a name said twice keeps its English the second time)
            }
            at = end;
        }
    }
    (out, arabic)
}

/// The names (places, people, brands) the glossary has that this line says, each once, longest first, with
/// their Arabic: for the language models, told to write each exactly so — the same name the same way in the
/// subtitles, on the map and on the phone. Matched as `protect` matches (whole words; a name written with a
/// capital only where the line writes it so).
pub fn names_in(setting: &str, line: &str) -> Vec<(String, String)> {
    let mut list = entries(setting);
    list.sort_by_key(|(en, _)| std::cmp::Reverse(en.len()));
    let lower = line.to_lowercase();
    // (a shorter name inside a longer one already found is not said on its own: "Leonida" in "Leonida Keys")
    let mut taken: Vec<(usize, usize)> = Vec::new();
    let mut out: Vec<(String, String)> = Vec::new();
    for (en, ar) in list {
        if en.split_whitespace().count() == 0 || !en.chars().next().is_some_and(char::is_uppercase) {
            continue;
        }
        let find = en.to_lowercase();
        let mut at = 0;
        while let Some(i) = lower[at..].find(&find).map(|i| i + at) {
            let end = i + find.len();
            let edge = |c: Option<char>| c.is_none_or(|c| !c.is_alphanumeric() && c != '\'');
            let named = line.is_char_boundary(i) && line[i..].chars().next().is_some_and(char::is_uppercase);
            let inside = taken.iter().any(|&(a, b)| i >= a && end <= b);
            if edge(lower[..i].chars().next_back()) && edge(lower[end..].chars().next()) && named && !inside {
                taken.push((i, end));
                if !out.iter().any(|(e, _)| e.eq_ignore_ascii_case(&en)) {
                    out.push((en.clone(), ar.clone()));
                }
                break;
            }
            at = end;
        }
    }
    out
}

/// The model's Arabic tidied, against the line's English: brackets the English hasn't (the model puts
/// names in them, and leaves one half of a pair), a full stop the English hasn't, a word said twice
/// in a row, and Arabic punctuation between Arabic words.
pub fn tidy(en: &str, ar: &str) -> String {
    let mut t = ar.to_string();
    if !en.contains('(') && !en.contains(')') {
        t = t.replace(['(', ')'], "");
    }
    let en_end = en.trim_end().chars().next_back();
    if en_end != Some('.') {
        let trimmed = t.trim_end();
        if trimmed.ends_with('.') && !trimmed.ends_with("..") {
            t = trimmed[..trimmed.len() - 1].to_string();
        }
    }
    // a word said twice in a row ("و و", "هيا هيا" is said so in English too: kept when it is)
    let words: Vec<&str> = t.split_whitespace().collect();
    let en_lower = en.to_lowercase();
    let mut kept: Vec<&str> = Vec::with_capacity(words.len());
    for w in words {
        let arabic = w.chars().any(|c| ('\u{0600}'..='\u{06FF}').contains(&c));
        if kept.last() == Some(&w) && arabic && !has_repeat(&en_lower) {
            continue;
        }
        kept.push(w);
    }
    let mut t = kept.join(" ");
    // Arabic punctuation after Arabic letters
    let mut out = String::with_capacity(t.len());
    let mut prev_arabic = false;
    for c in t.chars() {
        let c2 = match c {
            ',' if prev_arabic => '،',
            '?' if prev_arabic => '؟',
            ';' if prev_arabic => '؛',
            _ => c,
        };
        if !c.is_whitespace() {
            prev_arabic = ('\u{0600}'..='\u{06FF}').contains(&c);
        }
        out.push(c2);
    }
    t = out;
    t.trim().to_string()
}

/// The English says a word twice in a row ("go, go", "no no").
fn has_repeat(en: &str) -> bool {
    let w: Vec<String> = en.split(|c: char| !c.is_alphanumeric() && c != '\'').filter(|w| !w.is_empty()).map(str::to_string).collect();
    w.windows(2).any(|p| p[0] == p[1])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_place_and_legend_entry_round_trips_and_names_need_their_capital() {
        // every name comes back as its Arabic through [n]
        for (en, ar) in parse_tsv(BUILT_IN) {
            let (p, arabic) = protect("", &format!("Go to {en} now"), 0);
            assert!(p.contains("[1]"), "{en}: {p}");
            assert_eq!(arabic.first(), Some(&ar), "{en}");
        }
        // the longest first, and two names in one line both kept
        let (p, arabic) = protect("", "Fly from Vice City International Airport to Vice Beach", 0);
        assert_eq!(p, "Fly from [1] to [2]");
        assert_eq!(arabic, vec!["مطار فايس سيتي الدولي".to_string(), "فايس بيتش".to_string()]);
        // a word, not the district
        assert_eq!(protect("", "Meet me downtown", 0).1.len(), 0);
        // the legend: whole lines only
        assert_eq!(whole_line("", "Gas Station").as_deref(), Some("محطة وقود"));
        assert_eq!(whole_line("", "Pay 'n' Spray").as_deref(), Some("باي آند سبراي"));
        assert_eq!(protect("", "Find a safe place", 0).1.len(), 0);
        assert_eq!(protect("", "Take it to the Gas Station", 0).1.len(), 0);
    }

    #[test]
    fn whole_lines_and_names() {
        assert_eq!(whole_line("", "WASTED").as_deref(), Some("مُتّ"));
        assert_eq!(whole_line("", "Mission Passed").as_deref(), Some("نجحت المهمة"));
        assert_eq!(whole_line("", "Mission"), None);
        let (line, ar) = protect("", "Buy body armor at Ammu-Nation.", 0);
        assert_eq!(line, "Buy body armor at [1].");
        assert_eq!(ar, vec!["أمو-نيشن"]);
        // after a term already kept as [1]; a word inside another isn't a name
        let (line, ar) = protect("", "Vice City and Vice Citylike", 1);
        assert_eq!((line.as_str(), ar.len()), ("[2] and Vice Citylike", 1));
    }

    #[test]
    fn the_player_s_lines_come_first() {
        assert_eq!(parse_player("Wasted = خلصت عليك\n  bad line\nCops= الشرطة"), vec![("Wasted".into(), "خلصت عليك".into()), ("Cops".into(), "الشرطة".into())]);
        assert_eq!(whole_line("Wasted = خلصت عليك", "Wasted").as_deref(), Some("خلصت عليك"));
    }

    #[test]
    fn the_model_s_arabic_tidied() {
        // brackets round names, and half a pair
        assert_eq!(tidy("Lucia, I need you to trust me.", "(لوسيا)، أريدكِ أن تثقي بي."), "لوسيا، أريدكِ أن تثقي بي.");
        assert_eq!(tidy("Pick up Lucia.", "(ألتقط (لوسيا"), "ألتقط لوسيا");
        // brackets the English has stay
        assert_eq!(tidy("Cash (bank)", "نقود (البنك)"), "نقود (البنك)");
        // a full stop the English hasn't
        assert_eq!(tidy("Mission Passed", "نجحت المهمة."), "نجحت المهمة");
        assert_eq!(tidy("Go home.", "اذهب للبيت."), "اذهب للبيت.");
        // a word twice, unless the English says it twice
        assert_eq!(tidy("Get out", "اخرج اخرج"), "اخرج");
        assert_eq!(tidy("Go, go!", "هيا هيا!"), "هيا هيا!");
        // Arabic punctuation between Arabic words, not in English or numbers
        assert_eq!(tidy("Where are you?", "أين أنت?"), "أين أنت؟");
        assert_eq!(tidy("Press X, then Y?", "اضغط X, ثم Y?"), "اضغط X, ثم Y?");
        assert_eq!(tidy("You owe me, Jason.", "أنت مدين لي, جيسون."), "أنت مدين لي، جيسون.");
    }
}

#[cfg(test)]
mod names_tests {
    use super::*;

    #[test]
    fn a_line_names_its_places_once_each_the_longest_first() {
        let got = names_in("", "Drive to the Leonida Keys, then back to Vice City.");
        assert_eq!(got, vec![("Leonida Keys".to_string(), "جزر ليونيدا".to_string()), ("Vice City".to_string(), "فايس سيتي".to_string())]);
    }

    #[test]
    fn a_word_that_is_only_a_name_with_its_capital_is_left_alone() {
        assert!(names_in("", "meet me downtown").is_empty());
        assert_eq!(names_in("", "Meet me in Downtown")[0].1, "داون تاون");
    }

    #[test]
    fn the_players_own_spelling_wins() {
        let got = names_in("Vice City = فايس ستي", "Welcome to Vice City");
        assert_eq!(got, vec![("Vice City".to_string(), "فايس ستي".to_string())]);
    }

    #[test]
    fn the_maps_capitals_are_names_too() {
        let got = names_in("", "KELLY COUNTY");
        assert_eq!(got[0].1, "مقاطعة كيلي");
    }
}
