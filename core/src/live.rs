//! What the overlays show right now, shared by the reading (engine.rs), the pages that draw the
//! overlays elsewhere (the TV page, tv_server.rs; on the desktop also the OBS page and the window's
//! overlays) and the list of recent lines for the phone page.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Menu text translations, for one target language (the translator's answers in another are dropped).
/// At most `MenuCache::MOST` of them (the oldest go first), found by their English as it is, or as
/// OCR spells it a little differently (`similar`) without looking through all of them.
#[derive(Default)]
pub struct MenuCache {
    pub lang: String,
    pub map: HashMap<String, String>,
    /// the English in the order it came, oldest first
    order: VecDeque<String>,
    /// the English by its length in characters (`ocr::similar` only matches lengths within 15 %)
    by_len: HashMap<usize, Vec<String>>,
}

impl MenuCache {
    pub const MOST: usize = 5000;

    pub fn insert(&mut self, en: String, ar: String) {
        if self.map.insert(en.clone(), ar).is_none() {
            self.by_len.entry(en.chars().count()).or_default().push(en.clone());
            self.order.push_back(en);
        }
        while self.order.len() > Self::MOST {
            let Some(old) = self.order.pop_front() else { break };
            self.map.remove(&old);
            if let Some(v) = self.by_len.get_mut(&old.chars().count()) {
                v.retain(|e| *e != old);
            }
        }
    }

    /// The translation of `en` as it is, else of one OCR spelt alike.
    pub fn similar(&self, en: &str) -> Option<String> {
        if let Some(a) = self.map.get(en) {
            return Some(a.clone());
        }
        let n = en.chars().count();
        // (lengths `similar` can match: a difference under 15 % of the longer)
        let (lo, hi) = (n * 17 / 20, n * 20 / 17 + 1);
        (lo..=hi)
            .filter_map(|len| self.by_len.get(&len))
            .flat_map(|v| v.iter())
            .find(|k| crate::ocr::similar(k, en))
            .and_then(|k| self.map.get(k).cloned())
    }

    pub fn clear(&mut self) {
        self.map.clear();
        self.order.clear();
        self.by_len.clear();
    }
}

#[derive(Default)]
pub struct Live {
    /// A game page (a menu, a "mission failed" page …) is on screen; the full-window overlay shows it.
    pub screen_active: AtomicBool,
    /// The profile screen currently shown (layout JSON) and its translated reason line, for windows
    /// that open after the screen appeared.
    pub screen: Mutex<Option<serde_json::Value>>,
    /// Where the game page on screen has text marked private (other players' names): x, y, w, h
    /// fractions. Snapshots leave it out.
    pub screen_private: Mutex<Vec<[f64; 4]>>,
    pub screen_reason: Mutex<serde_json::Value>,
    /// Area id -> {"en": ..., "ar": ...} currently shown by that area's overlay (for saved frames).
    pub on_screen: Mutex<HashMap<String, serde_json::Value>>,
    /// Area id -> the full translation event now shown (segments, size, place), for the OBS and TV pages.
    pub shown: Mutex<HashMap<String, serde_json::Value>>,
    /// Area id -> the game's text seen there and not translated on screen yet: {"seq", "text_box",
    /// "game_box", "line_h"} (seq 0 = not sent to the translator yet). With the "over the game's
    /// text" look its black box goes up at once (Read Areas: "black box as soon as the text appears").
    pub covers: Mutex<HashMap<String, serde_json::Value>>,
    /// GTA VI's phone held up: where its screen is and its pieces' translations ({"screen", "pieces"};
    /// see engine::read_phone), for the overlay that draws them beside it.
    pub phone: Mutex<Option<serde_json::Value>>,
    /// When the phone was last seen (it goes a moment after it's put away).
    pub phone_seen: Mutex<Option<std::time::Instant>>,
    /// The phone's pieces as read before (see phone::steady).
    pub phone_pieces: Mutex<Vec<crate::phone::Seen>>,
    /// The phone's translations are up (its overlay window shows).
    pub phone_active: AtomicBool,
    /// GTA VI's message bubble at the bottom right as last read (see chat.rs): the picture of its box
    /// then (read again only when it changes), the box, and what was read.
    pub chat_read: Mutex<Option<(Vec<u8>, (u32, u32, u32, u32), Option<crate::chat::Bubble>)>>,
    /// Pause-menu text translated by the translator this session: English -> translation, in one language.
    pub menu_cache: Mutex<MenuCache>,
    /// A page's words as they were first shown, by their English (as OCR spells it, near enough): kept
    /// while it reads the same, so an item doesn't flip between two translations from one read to the next.
    pub menu_steady: Mutex<MenuCache>,
    /// …and the menu text it couldn't translate (asked again later).
    pub menu_failed: Mutex<Vec<String>>,
    /// Recent translations for the home-network page, newest last, and their ids.
    pub live_log: Mutex<VecDeque<serde_json::Value>>,
    pub live_seq: AtomicU64,
    /// the target language for that page: {"code": "ar", "rtl": true}
    pub live_lang: Mutex<serde_json::Value>,
    /// the home-network server is running
    pub lan_running: AtomicBool,
    /// why the home-network server couldn't start listening
    pub lan_error: Mutex<Option<String>>,
    /// TVs connected to show the overlays (the VI Translator app on the TV), with their keys
    pub tvs: crate::tv::Tvs,
    /// the "Live translations" switch is off: the home-network page gets no lines meanwhile
    pub live_off: AtomicBool,
}

/// How many recent lines the page can fetch.
pub const KEEP: usize = 200;

/// Remember a line that was just shown, for the page.
/// `en_segments` and `segments`: the English and the translation with the game's button icons
/// (data URLs), for the page to draw them; `en` and "text" have 🎮 in their place.
pub fn push(live: &Live, area: &str, en: &str, en_segments: &[(String, Option<String>)], segments: &[(String, Option<String>)], at_ms: u64) {
    // "Live translations" switched off: the list pauses (and switching it back on doesn't dump what came
    // in meanwhile). Independent of the overlays: with this on, the list keeps filling with them off.
    if live.live_off.load(Ordering::Relaxed) {
        return;
    }
    let text: String = segments
        .iter()
        .map(|(t, c)| if c.as_deref().is_some_and(|c| c.starts_with("icon:")) { "🎮" } else { t.as_str() })
        .collect();
    if text.trim().is_empty() {
        return;
    }
    let id = live.live_seq.fetch_add(1, Ordering::Relaxed) + 1;
    let mut log = live.live_log.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    // the same line sent again with the game's colours (same area and start): the first goes
    log.retain(|l| !(l["area"] == area && l["at"] == at_ms));
    log.push_back(serde_json::json!({ "id": id, "at": at_ms, "area": area, "en": en, "text": text.trim(), "en_segs": en_segments, "segs": segments }));
    while log.len() > KEEP {
        log.pop_front();
    }
}

/// What a page needs to draw the overlays over the whole game picture (the TV page, and the
/// desktop's OBS page): where the areas are, their style, and what each shows now. Never settings
/// beyond that (keys, passwords) or the picture; private screen text is never in `screen` (it's
/// taken out before a page is shown).
pub fn overlay_state(cfg: &Mutex<crate::config::Config>, live: &Live) -> serde_json::Value {
    let (areas, style, profile_opts, overlays, lang) = {
        let c = cfg.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        (c.areas.clone(), c.style.clone(), c.profile_opts.clone(), c.overlays_enabled, c.target_lang.clone())
    };
    // (each area's effects as they apply: the player's own, else the game's default)
    let area_fx = cfg.lock().unwrap_or_else(std::sync::PoisonError::into_inner).fx_all();
    serde_json::json!({
        "build": crate::net::build_id(),
        "areas": areas,
        "style": style,
        "profile_opts": profile_opts,
        // the header's Overlays switch: off = draw nothing
        "overlays": overlays,
        "lang": lang,
        "shown": live.shown.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone(),
        "covers": live.covers.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone(),
        "phone": live.phone.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone(),
        "area_fx": area_fx,
        "screen": live.screen.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone(),
        "reason": live.screen_reason.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone(),
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_menu_cache_finds_ocr_spellings_and_keeps_the_newest() {
        let mut c = MenuCache::default();
        c.insert("Mission Failed".into(), "فشلت المهمة".into());
        c.insert("Settings".into(), "الإعدادات".into());
        assert_eq!(c.similar("Settings").as_deref(), Some("الإعدادات"));
        assert_eq!(c.similar("Missi0n Failed").as_deref(), Some("فشلت المهمة"));
        assert_eq!(c.similar("Map"), None);
        for i in 0..MenuCache::MOST {
            c.insert(format!("line {i}"), format!("سطر {i}"));
        }
        assert_eq!(c.map.len(), MenuCache::MOST);
        assert_eq!(c.similar("Mission Failed"), None, "the oldest went first");
        assert_eq!(c.similar("line 4999").as_deref(), Some("سطر 4999"));
    }

    use super::*;
    use crate::config::Config;

    #[test]
    fn keeps_the_last_lines_with_icons_as_a_game_pad() {
        let live = Live::default();
        for i in 0..(KEEP + 5) {
            push(&live, "subtitles", &format!("Line {i}"), &[], &[(format!("سطر {i}"), None)], 1000 + i as u64);
        }
        let en_icon = [("Press ".into(), None), (String::new(), Some("icon:data:x".into()))];
        push(&live, "help", "Press 🎮", &en_icon, &[("اضغط ".into(), None), (String::new(), Some("icon:data:x".into()))], 9);
        push(&live, "help", "nothing", &[], &[(" ".into(), None)], 10); // blank: not kept
        let log = live.live_log.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(log.len(), KEEP);
        assert_eq!(log.back().unwrap()["text"], "اضغط 🎮");
        // the icons themselves too, for the page to draw them in both lines
        assert_eq!(log.back().unwrap()["segs"][1][1], "icon:data:x");
        assert_eq!(log.back().unwrap()["en_segs"][1][1], "icon:data:x");
        let ids: Vec<u64> = log.iter().map(|l| l["id"].as_u64().unwrap()).collect();
        assert!(ids.windows(2).all(|w| w[0] < w[1]), "ids increase");
        assert_eq!(log.front().unwrap()["en"], "Line 6");
    }

    #[test]
    fn the_tv_state_has_the_overlays_only() {
        let c = Mutex::new(Config { deepl_key: "secret-deepl-key".into(), claude_key: "secret-claude-key".into(), openai_key: "secret-openai-key".into(), ..Config::default() });
        let live = Live::default();
        let v = overlay_state(&c, &live);
        let text = v.to_string();
        assert!(!text.contains("secret-"), "{text}");
        for key in ["areas", "style", "shown", "screen", "overlays", "lang"] {
            assert!(v.get(key).is_some(), "{key}");
        }
        assert!(v.get("status").is_none() && v.get("frame").is_none() && v.get("preview").is_none());
    }
}
