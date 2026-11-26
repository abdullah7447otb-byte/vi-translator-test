//! The game's instructions speak to the player: "Press ✕ to sprint" is «اضغط» for Jason and «اضغطي» for
//! Lucia. The translators and the reviewed lines give the masculine; when the player plays Lucia, the
//! instruction areas' Arabic has its imperative verbs made feminine here, by a list of the verbs the game's
//! instructions use (a word not in the list is left as it is: never a wrong guess). Spoken lines are left
//! alone: who speaks to whom there is the translator's to tell.

/// Masculine imperative → feminine, the verbs GTA VI's instructions use.
const VERBS: &[(&str, &str)] = &[
    ("اضغط", "اضغطي"), ("اذهب", "اذهبي"), ("اتبع", "اتبعي"), ("اركب", "اركبي"), ("ادخل", "ادخلي"), ("اخرج", "اخرجي"),
    ("استخدم", "استخدمي"), ("انتظر", "انتظري"), ("اقترب", "اقتربي"), ("ابحث", "ابحثي"), ("اجمع", "اجمعي"), ("تخلص", "تخلصي"),
    ("قد", "قودي"), ("اقتل", "اقتلي"), ("احم", "احمي"), ("اختبئ", "اختبئي"), ("اهرب", "اهربي"), ("اصعد", "اصعدي"),
    ("انزل", "انزلي"), ("التقط", "التقطي"), ("افتح", "افتحي"), ("أطلق", "أطلقي"), ("صوّب", "صوّبي"), ("صوب", "صوبي"),
    ("اركض", "اركضي"), ("تحدث", "تحدثي"), ("تحدّث", "تحدّثي"), ("اتصل", "اتصلي"), ("عد", "عودي"), ("ارجع", "ارجعي"),
    ("توجه", "توجهي"), ("توجّه", "توجّهي"), ("اتجه", "اتجهي"), ("اسرق", "اسرقي"), ("ادفع", "ادفعي"), ("اشتر", "اشتري"),
    ("بع", "بيعي"), ("اختر", "اختاري"), ("اضبط", "اضبطي"), ("اسحب", "اسحبي"), ("تفاعل", "تفاعلي"), ("أمسك", "أمسكي"),
    ("امسك", "امسكي"), ("اقفز", "اقفزي"), ("تسلق", "تسلقي"), ("تسلّق", "تسلّقي"), ("انحن", "انحني"), ("اسبح", "اسبحي"),
    ("غص", "غوصي"), ("أوقف", "أوقفي"), ("اوقف", "اوقفي"), ("حدد", "حددي"), ("حدّد", "حدّدي"), ("اعثر", "اعثري"),
    ("تجنب", "تجنبي"), ("تجنّب", "تجنّبي"), ("راقب", "راقبي"), ("احم", "احمي"), ("ساعد", "ساعدي"), ("اطلب", "اطلبي"),
    ("أنقذ", "أنقذي"), ("انقذ", "انقذي"), ("ضع", "ضعي"), ("خذ", "خذي"), ("أعد", "أعيدي"), ("اعرض", "اعرضي"),
    ("اختبر", "اختبري"), ("تأكد", "تأكدي"), ("تأكّد", "تأكّدي"), ("كن", "كوني"), ("لاحق", "لاحقي"), ("طارد", "طاردي"),
    ("تخطّ", "تخطّي"), ("أكمل", "أكملي"), ("اكمل", "اكملي"), ("ابق", "ابقي"), ("ارم", "ارمي"),
    ("فجّر", "فجّري"), ("فجر", "فجري"), ("اسلك", "اسلكي"), ("اتّبع", "اتّبعي"), ("استمر", "استمري"), ("استمرّ", "استمرّي"),
    ("انطلق", "انطلقي"), ("أوصل", "أوصلي"), ("اوصل", "اوصلي"), ("اصطحب", "اصطحبي"), ("قابل", "قابلي"), ("استعد", "استعيدي"),
    ("اشحن", "اشحني"), ("أصلح", "أصلحي"), ("اصلح", "اصلحي"), ("افقد", "افقدي"), ("أفقد", "أفقدي"), ("اهزم", "اهزمي"),
];

/// The game's areas that speak to the player (not the spoken lines).
pub fn speaks_to_player(area: &str) -> bool {
    matches!(area, "help" | "instructions" | "notes" | "prompts")
}

fn feminine_word(w: &str) -> Option<String> {
    if let Some((_, f)) = VERBS.iter().find(|(m, _)| *m == w) {
        return Some((*f).to_string());
    }
    // (with "and" or "then" in front: «واضغط», «فاضغط»; not the two-letter verbs, whose «و…»/«ف…» are other
    // words: «وضع» mode, «فقد» lost, «وعد» promise)
    if w.chars().count() <= 3 {
        return None;
    }
    for p in ["و", "ف"] {
        if let Some(rest) = w.strip_prefix(p)
            && let Some((_, f)) = VERBS.iter().find(|(m, _)| *m == rest)
        {
            return Some(format!("{p}{f}"));
        }
    }
    None
}

/// «قد» is "drive!" only before where to: «قد إلى…», «قد السيارة»; else it is the particle («قد يكون»).
fn drive_follows(next: Option<&str>) -> bool {
    const VEHICLES: [&str; 8] = ["السيارة", "المركبة", "القارب", "الدراجة", "الشاحنة", "الطائرة", "سيارتك", "مركبتك"];
    next.is_some_and(|n| ["إلى", "الى", "نحو"].contains(&n) || VEHICLES.iter().any(|v| n.ends_with(v)))
}

/// The text with its listed masculine imperatives made feminine (whole words only; punctuation, icon
/// placeholders and everything else as it was).
pub fn feminine(ar: &str) -> String {
    // the text as words (Arabic letters and their marks) and what's between them
    let mut parts: Vec<(bool, String)> = Vec::new();
    for c in ar.chars() {
        let arabic = ('\u{0621}'..='\u{064A}').contains(&c) || ('\u{064B}'..='\u{0652}').contains(&c) || c == '\u{0670}';
        match parts.last_mut() {
            Some((a, t)) if *a == arabic => t.push(c),
            _ => parts.push((arabic, c.to_string())),
        }
    }
    let words: Vec<usize> = parts.iter().enumerate().filter(|(_, (a, _))| *a).map(|(i, _)| i).collect();
    for (k, &i) in words.iter().enumerate() {
        let w = parts[i].1.clone();
        let base = ["و", "ف"].iter().find_map(|p| w.strip_prefix(p)).filter(|r| feminine_word(r).is_some() && !VERBS.iter().any(|(m, _)| *m == w)).unwrap_or(&w);
        if base == "قد" && !drive_follows(words.get(k + 1).map(|&j| parts[j].1.as_str())) {
            continue;
        }
        if let Some(f) = feminine_word(&w) {
            parts[i].1 = f;
        }
    }
    parts.into_iter().map(|(_, t)| t).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instructions_speak_to_lucia() {
        assert_eq!(feminine("اضغط ⟦1⟧ للركض."), "اضغطي ⟦1⟧ للركض.");
        assert_eq!(feminine("اتبع جيسون واركب السيارة"), "اتبعي جيسون واركبي السيارة");
        assert_eq!(feminine("قد إلى السجن."), "قودي إلى السجن.");
        // other words, and verbs inside other words, stay
        assert_eq!(feminine("السيارة قديمة"), "السيارة قديمة");
        assert_eq!(feminine("تفاعلات"), "تفاعلات");
        assert_eq!(feminine(""), "");
        // «قد» the particle stays
        assert_eq!(feminine("قد يكون هذا فخًا."), "قد يكون هذا فخًا.");
        assert_eq!(feminine("ثم قد السيارة"), "ثم قودي السيارة");
        // words that only look like a verb with «و»/«ف», and «قد» before anything but where to drive
        assert_eq!(feminine("بدّل إلى وضع التصوير"), "بدّل إلى وضع التصوير");
        assert_eq!(feminine("اضغط ⟦1⟧ لتغيير وضع الكاميرا"), "اضغطي ⟦1⟧ لتغيير وضع الكاميرا");
        assert_eq!(feminine("فقد الاتصال بالهدف"), "فقد الاتصال بالهدف");
        assert_eq!(feminine("وعد جيسون"), "وعد جيسون");
        assert_eq!(feminine("قد بدأت المهمة"), "قد بدأت المهمة");
        assert_eq!(feminine("قد الاتصال"), "قد الاتصال");
        assert!(speaks_to_player("instructions") && !speaks_to_player("subtitles"));
    }
}
