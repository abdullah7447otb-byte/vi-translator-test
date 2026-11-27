//! The check report's picture: the game's frame (at the size the app reads it) with each reading area drawn
//! on it as a frame of its own colour, from what the check mode saw (`Live.area_debug`):
//! - green: it read text and kept it (translated);
//! - red: it read text and dropped it (the reason is in areas.json, "drop");
//! - yellow: it read text and is still waiting for it to hold steady;
//! - grey: nothing read there.
//!
//! Drawn by the app itself, so it is the same on every computer (no screenshot of the window needed).

use std::collections::HashMap;

/// What an area's frame shows (see the module's doc).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Kept,
    Dropped,
    Waiting,
    Empty,
}

impl State {
    fn rgb(self) -> [u8; 3] {
        match self {
            State::Kept => [40, 220, 90],
            State::Dropped => [240, 50, 50],
            State::Waiting => [250, 200, 40],
            State::Empty => [150, 150, 150],
        }
    }
}

/// An area's state from its check-mode entry.
pub fn state_of(v: &serde_json::Value) -> State {
    let has = |k: &str| v.get(k).and_then(|x| x.as_str()).is_some_and(|s| !s.trim().is_empty());
    if has("drop") {
        State::Dropped
    } else if has("kept") {
        State::Kept
    } else if has("read") {
        State::Waiting
    } else {
        State::Empty
    }
}

/// The frame (RGBA, `w`×`h`) with every area's frame drawn on it, 3 px wide at 1080p (scaled with the
/// height), inside the area so an area at the picture's edge shows whole. An area without a place
/// (x, y, w, h as fractions of the picture) is left out.
pub fn draw(rgba: &[u8], w: u32, h: u32, areas: &HashMap<String, serde_json::Value>) -> Vec<u8> {
    let mut out = rgba.to_vec();
    if w == 0 || h == 0 || out.len() < (w as usize) * (h as usize) * 4 {
        return out;
    }
    let line = ((h as f64 / 1080.0) * 3.0).round().max(2.0) as u32;
    // (the same order every time: an area drawn over another one is drawn last whatever the map's order)
    let mut ids: Vec<&String> = areas.keys().collect();
    ids.sort();
    for id in ids {
        let v = &areas[id];
        let f = |k: &str| v.get(k).and_then(|x| x.as_f64()).filter(|x| x.is_finite());
        let (Some(x), Some(y), Some(aw), Some(ah)) = (f("x"), f("y"), f("w"), f("h")) else { continue };
        let x0 = (x.clamp(0.0, 1.0) * w as f64).round() as u32;
        let y0 = (y.clamp(0.0, 1.0) * h as f64).round() as u32;
        let x1 = (((x + aw).clamp(0.0, 1.0)) * w as f64).round() as u32;
        let y1 = (((y + ah).clamp(0.0, 1.0)) * h as f64).round() as u32;
        if x1 <= x0 + 2 * line || y1 <= y0 + 2 * line {
            continue;
        }
        let rgb = state_of(v).rgb();
        let mut put = |px: u32, py: u32| {
            let i = ((py as usize) * (w as usize) + px as usize) * 4;
            out[i..i + 3].copy_from_slice(&rgb);
            out[i + 3] = 255;
        };
        for py in y0..y1 {
            for px in x0..x1 {
                let edge = px < x0 + line || px >= x1 - line || py < y0 + line || py >= y1 - line;
                if edge {
                    put(px, py);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn px(img: &[u8], w: u32, x: u32, y: u32) -> [u8; 3] {
        let i = ((y * w + x) * 4) as usize;
        [img[i], img[i + 1], img[i + 2]]
    }

    #[test]
    fn each_area_gets_the_colour_of_what_it_did() {
        assert_eq!(state_of(&serde_json::json!({ "read": "Go to the car", "kept": "Go to the car", "drop": null })), State::Kept);
        assert_eq!(state_of(&serde_json::json!({ "read": "SUPERMARKET", "kept": "", "drop": "capitals" })), State::Dropped);
        assert_eq!(state_of(&serde_json::json!({ "read": "Go to", "kept": "" })), State::Waiting);
        assert_eq!(state_of(&serde_json::json!({ "read": " ", "kept": "" })), State::Empty);
        assert_eq!(state_of(&serde_json::json!({})), State::Empty);
    }

    #[test]
    fn the_frames_sit_on_the_areas_and_the_picture_stays_inside_them() {
        let (w, h) = (1920u32, 1080u32);
        let rgba = vec![10u8; (w * h * 4) as usize];
        let mut areas = HashMap::new();
        areas.insert("subtitles".to_string(), serde_json::json!({ "x": 0.25, "y": 0.8, "w": 0.5, "h": 0.15, "read": "Hi", "kept": "Hi" }));
        areas.insert("help".to_string(), serde_json::json!({ "x": 0.0, "y": 0.0, "w": 0.3, "h": 0.2, "read": "SHOP", "kept": "", "drop": "capitals" }));
        areas.insert("bad".to_string(), serde_json::json!({ "x": "no", "read": "x" }));
        let out = draw(&rgba, w, h, &areas);
        // the subtitles' frame: green on its edge (x 480, y 864 … 1026), the inside untouched
        assert_eq!(px(&out, w, 480, 900), [40, 220, 90]);
        assert_eq!(px(&out, w, 1000, 865), [40, 220, 90]);
        assert_eq!(px(&out, w, 1000, 950), [10, 10, 10]);
        // the help area at the picture's corner: red, whole (its edge on the picture's first pixel)
        assert_eq!(px(&out, w, 0, 0), [240, 50, 50]);
        assert_eq!(px(&out, w, 300, 1), [240, 50, 50]);
        assert_eq!(px(&out, w, 300, 100), [10, 10, 10]);
        // far from every area: untouched
        assert_eq!(px(&out, w, 1800, 500), [10, 10, 10]);
    }

    #[test]
    fn a_wrong_size_or_an_area_off_the_picture_changes_nothing_and_never_panics() {
        let rgba = vec![0u8; 16];
        let mut areas = HashMap::new();
        areas.insert("a".to_string(), serde_json::json!({ "x": 0.5, "y": 0.5, "w": 2.0, "h": 2.0, "kept": "x" }));
        assert_eq!(draw(&rgba, 100, 100, &areas), rgba);
        let big = vec![0u8; 200 * 100 * 4];
        let out = draw(&big, 200, 100, &areas);
        assert_eq!(out.len(), big.len());
        areas.insert("b".to_string(), serde_json::json!({ "x": 1.5, "y": -3.0, "w": 0.1, "h": 0.1, "kept": "x" }));
        let _ = draw(&big, 200, 100, &areas);
    }
}
