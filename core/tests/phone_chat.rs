// Ported from CouchSubs (AGPL-3.0, (C) 2026 Sinan Yorulmaz), commit bc54ff7: couchsubs-android/app/src/test/java/com/couchsubs/mobile/core/ScreenPhoneCardTest.kt
// (sharedChatVectors, sharedEdgeAndFlatVectors), on their fixture fixtures/phone-chat.json as is.

use serde_json::Value;
use vi_translator_core::phone_chat::{Box4, Handset, Line, groups, on_screen};

fn fixture() -> Value {
    serde_json::from_str(include_str!("fixtures/phone-chat.json")).expect("phone-chat.json")
}

fn nums(v: &Value) -> Vec<f64> {
    v.as_array().map(|a| a.iter().filter_map(Value::as_f64).collect()).unwrap_or_default()
}

fn box4(v: &Value) -> Box4 {
    let n = nums(v);
    Box4::new(n[0], n[1], n[2], n[3])
}

fn handset(pose: &Value, aspect: f64) -> Handset {
    let p = nums(pose);
    Handset { top_x: p[0], top_y: p[1], bottom_x: p[2], bottom_y: p[3], aspect }
}

#[test]
fn chat_vectors() {
    let root = fixture();
    let cases = root["cases"].as_array().expect("cases");
    let mut failures = Vec::new();
    for c in cases {
        let name = c["name"].as_str().unwrap_or("?");
        let lines: Vec<Line> = c["lines"].as_array().into_iter().flatten().map(|l| Line { text: l["text"].as_str().unwrap_or("").to_string(), frame_box: box4(&l["box"]) }).collect();
        let h = handset(&c["pose"], c["aspect"].as_f64().unwrap_or(16.0 / 9.0));
        let got: Vec<(String, String)> = groups(&lines, &h).into_iter().map(|g| (g.role.wire().to_string(), g.text)).collect();
        let want: Vec<(String, String)> = c["groups"].as_array().into_iter().flatten().map(|g| (g[0].as_str().unwrap_or("").to_string(), g[1].as_str().unwrap_or("").to_string())).collect();
        if got != want {
            failures.push(format!("{name}:\n  got  {got:?}\n  want {want:?}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    assert_eq!(cases.len(), 10);
}

#[test]
fn edge_and_flat_vectors() {
    let root = fixture();
    let o = &root["outside"];
    let h = handset(&o["pose"], o["aspect"].as_f64().unwrap_or(16.0 / 9.0));
    for b in o["boxes"].as_array().into_iter().flatten() {
        assert_eq!(on_screen(&h.flat(&box4(&b["box"]))), b["inside"].as_bool() == Some(true), "{b}");
    }
    for f in root["flat"].as_array().into_iter().flatten() {
        let spot = handset(&f["pose"], f["aspect"].as_f64().unwrap_or(16.0 / 9.0)).flat(&box4(&f["box"]));
        let want = nums(&f["flat"]);
        for (got, want) in [spot.left, spot.top, spot.right, spot.bottom].iter().zip(want.iter()) {
            assert!((got - want).abs() <= 0.02, "{f}: got {spot:?}");
        }
    }
}
