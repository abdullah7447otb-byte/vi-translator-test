//! GTA VI's phone in real frames: found when held (upright or tilted, day or night), never in frames
//! without one. The frames are CouchSubs' regression set (testdata/frames at commit bc54ff7, screenshots
//! of gameplay videos): not kept in this repository; CI fetches them and sets VIT_FRAMES to the folder.
//! Without VIT_FRAMES the test says so and passes.

use std::path::{Path, PathBuf};

use vi_translator_core::{chat, phone, phone_pose};

fn frames() -> Option<PathBuf> {
    match std::env::var("VIT_FRAMES") {
        Ok(d) if Path::new(&d).join("frames.json").exists() => Some(PathBuf::from(d)),
        _ => {
            eprintln!("VIT_FRAMES not set (or no frames.json there): the real-frame phone test is skipped");
            None
        }
    }
}

fn load(dir: &Path, name: &str) -> (Vec<u8>, u32, u32) {
    let img = image::open(dir.join(format!("{name}.jpg"))).unwrap_or_else(|e| panic!("{name}: {e}")).to_rgba8();
    let (w, h) = img.dimensions();
    (img.into_raw(), w, h)
}

/// What must be found in each phone frame: (frame, pose expected, tilt in degrees when tilted).
const PHONES: &[(&str, bool, Option<f64>)] = &[
    ("gta6-telefon-sohbet-1", true, None),
    ("gta6-telefon-sohbet-2", true, Some(7.5)),
    ("gta6-telefon-ana-1", true, None),
    // (its home bar is under the set's blurred caption: found by its bright screen, opened with its buttons)
    ("gta6-telefon-ana-2", false, None),
];

#[test]
fn the_phone_is_found_in_every_phone_frame_and_in_no_other() {
    let Some(dir) = frames() else { return };
    let mut report = Vec::new();
    let mut failures = Vec::new();
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| (n.starts_with("gta6-") || n.starts_with("pencere-")) && n.ends_with(".jpg"))
        .map(|n| n.trim_end_matches(".jpg").to_string())
        .collect();
    names.sort();
    assert!(names.len() >= 50, "the frame set is incomplete: {} frames", names.len());
    for name in &names {
        let (rgba, w, h) = load(&dir, name);
        let t = std::time::Instant::now();
        let pose = phone_pose::locate(&rgba, w, h).filter(|p| phone_pose::plausible(&p.bounds(), w, h));
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        let bright = phone::find_screen(&rgba, w, h);
        report.push(format!("{name:30} pose={:<5} tilt={:>6} bright={:<5} {ms:.2} ms", pose.is_some(), pose.map(|p| format!("{:.1}", p.tilt(w, h))).unwrap_or_default(), bright.is_some()));
        match PHONES.iter().find(|(n, ..)| n == name) {
            Some((_, want_pose, tilt)) => {
                if pose.is_some() != *want_pose {
                    failures.push(format!("{name}: pose {}, expected {}", pose.is_some(), want_pose));
                }
                if let (Some(p), Some(t)) = (pose, tilt)
                    && (p.tilt(w, h) - t).abs() > 3.0
                {
                    failures.push(format!("{name}: tilt {:.1}°, expected {t}° ±3°", p.tilt(w, h)));
                }
                // (a phone: its screen is found inside the pose's box, or by brightness)
                let screen = pose.map(|p| phone::screen_in_pose(bright, p.bounds().px(w, h))).or(bright);
                if screen.is_none() {
                    failures.push(format!("{name}: no screen"));
                }
                if pose.is_some() && phone::screen_quad(&rgba, w, h, screen.unwrap()).is_none() && tilt.is_none() && *name == "gta6-telefon-sohbet-1" {
                    failures.push(format!("{name}: no corners"));
                }
            }
            None => {
                // (no phone: never by its shape; a bright box alone opens nothing without the phone's buttons)
                if pose.is_some() {
                    failures.push(format!("{name}: a phone found where there is none"));
                }
            }
        }
        // (the time only in an optimised build: a debug build is ten times slower)
        if ms > 5.0 && !cfg!(debug_assertions) {
            failures.push(format!("{name}: {ms:.1} ms, over 5 ms"));
        }
    }
    // the notification bubble at the bottom right
    let (rgba, w, h) = load(&dir, "gta6-n4-bildirim");
    if chat::find_bubble(&rgba, w, h).is_none() {
        failures.push("gta6-n4-bildirim: no message bubble".into());
    }
    eprintln!("{}", report.join("\n"));
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

#[test]
fn the_phone_opens_in_two_readings_and_closes_when_it_goes() {
    let Some(dir) = frames() else { return };
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(dir.join("frames.json")).unwrap()).unwrap();
    let scenarios = v["phoneScenarios"].as_array().expect("phoneScenarios");
    assert!(!scenarios.is_empty());
    for s in scenarios {
        let (phone_name, gone) = (s["phone"].as_str().unwrap(), s["gone"].as_str().unwrap());
        let (p, w, h) = load(&dir, phone_name);
        let (g, ..) = load(&dir, gone);
        let mut tr = phone_pose::Tracker::default();
        let mut opened_at = None;
        for i in 0..2 {
            if tr.sample(phone_pose::locate(&p, w, h), w, h).0.is_some() && opened_at.is_none() {
                opened_at = Some(i);
            }
        }
        assert!(opened_at.is_some(), "{phone_name}: not open after 2 readings");
        let mut closed_at = None;
        for i in 0..=tr.miss_grace {
            if tr.sample(phone_pose::locate(&g, w, h), w, h).0.is_none() {
                closed_at = Some(i);
                break;
            }
        }
        assert_eq!(closed_at, Some(tr.miss_grace), "{gone}: closes after its grace, not before or later");
    }
}
