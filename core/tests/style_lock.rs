// Ported from CouchSubs (AGPL-3.0, (C) 2026 Sinan Yorulmaz), commit bc54ff7: couchsubs-android/app/src/test/java/com/couchsubs/mobile/core/ScreenStyleLockTest.kt
// (sharedStyleLockVectors, jitteringMenuNeverChangesLook), on their fixture fixtures/screen-style-lock.json as is.

use serde_json::Value;
use vi_translator_core::style_lock::{Item, StyleLock};

fn item(e: &Value) -> Item {
    Item {
        source: e["source"].as_str().unwrap_or("").to_string(),
        line: e["line"].as_f64().unwrap_or(0.0),
        upper: e["upper"].as_bool().unwrap_or(false),
        bold: e["bold"].as_bool().unwrap_or(false),
        condensed: e["condensed"].as_bool().unwrap_or(false),
        rows: e["rows"].as_u64().unwrap_or(1) as u32,
        icons: e["icons"].as_u64().unwrap_or(0) as usize,
        fg: e["fg"].as_str().unwrap_or("").to_string(),
        bg: e["bg"].as_str().unwrap_or("").to_string(),
        bubble: e["bubble"].as_bool().unwrap_or(false),
        look: e["look"].as_bool().unwrap_or(true),
    }
}

#[test]
fn style_lock_vectors() {
    let root: Value = serde_json::from_str(include_str!("fixtures/screen-style-lock.json")).expect("fixture");
    let cases = root["cases"].as_array().expect("cases");
    assert_eq!(cases.len(), 7);
    for c in cases {
        let name = c["name"].as_str().unwrap_or("?");
        let mut lock = StyleLock::new();
        for (s, step) in c["steps"].as_array().into_iter().flatten().enumerate() {
            let items: Vec<Item> = step["items"].as_array().into_iter().flatten().map(item).collect();
            let got = lock.steady(&items, step["now"].as_u64().unwrap_or(0));
            let expect = step["expect"].as_array().expect("expect");
            assert_eq!(got.len(), expect.len(), "{name} #{s}");
            for (k, (g, w)) in got.iter().zip(expect).enumerate() {
                let label = format!("{name} #{s}.{k}");
                assert!((g.line - w["line"].as_f64().unwrap_or(-1.0)).abs() < 1e-9, "{label}: line {}", g.line);
                assert_eq!(g.fg, w["fg"].as_str().unwrap_or(""), "{label}");
                assert_eq!(g.bg, w["bg"].as_str().unwrap_or(""), "{label}");
                if items[k].look {
                    assert_eq!(g.bold, w["bold"].as_bool().unwrap_or(false), "{label}");
                    assert_eq!(g.condensed, w["condensed"].as_bool().unwrap_or(false), "{label}");
                }
            }
        }
    }
}

/// A menu jittering ±10 % never changes an item's look between frames.
#[test]
fn a_jittering_menu_never_changes_look() {
    let mut lock = StyleLock::new();
    let names = ["MAP", "BRIEF", "STATS", "SETTINGS", "Resume", "Quit to desktop"];
    let mut first: Option<Vec<Item>> = None;
    let mut seed = 7u64;
    for frame in 0..300u64 {
        let items: Vec<Item> = names
            .iter()
            .map(|n| {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                let j = ((seed >> 33) as f64 / (1u64 << 31) as f64 - 0.5) * 0.2;
                let base = if n.chars().all(|c| c.is_uppercase()) { 0.03 } else { 0.022 };
                Item { source: n.to_string(), line: base * (1.0 + j), upper: n.chars().all(|c| c.is_uppercase()), bold: true, condensed: false, rows: 1, icons: 0, fg: format!("#F{}F{}F{}", (frame % 3), (frame % 2), 0).replace("#F0F0F0", "#FFFFFF"), bg: "#101010".into(), bubble: false, look: true }
            })
            .collect();
        let got = lock.steady(&items, frame * 100);
        match &first {
            None => first = Some(got),
            Some(f) => {
                for (a, b) in f.iter().zip(&got) {
                    assert_eq!((a.line, a.bold, &a.fg, &a.bg), (b.line, b.bold, &b.fg, &b.bg), "frame {frame}: {}", a.source);
                }
            }
        }
    }
}
