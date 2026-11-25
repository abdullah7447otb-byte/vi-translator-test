//! The phone's anchor on a synthetic 30-second clip made from real GTA VI phone frames (see phone_frames.rs
//! for where the frames come from; VIT_FRAMES): the picture shaken like a hand (±15 px), turned (±10°),
//! jolted as in a car, two slightly blurred frames, two frames without the phone. The anchor must follow
//! the screen's corners within 1 px (median) and 2 px (95th percentile), lose them only where the phone is
//! missing, and cost little per frame.

use std::path::Path;

use vi_translator_core::phone_anchor::{Anchor, Moved, Quad};
use vi_translator_core::{phone, phone_pose};

fn load(dir: &Path, name: &str) -> (Vec<u8>, u32, u32) {
    let img = image::open(dir.join(format!("{name}.jpg"))).unwrap().to_rgba8();
    let (w, h) = img.dimensions();
    (img.into_raw(), w, h)
}

/// `src` moved by the homography `m` (3×3, row by row: frame → frame), bilinear.
fn warp(src: &[u8], w: u32, h: u32, m: &[f64; 9]) -> Vec<u8> {
    let inv = vi_translator_core::screen_anchor::invert3(m).unwrap();
    let mut out = vec![0u8; src.len()];
    for y in 0..h {
        for x in 0..w {
            let (fx, fy) = (x as f64, y as f64);
            let z = inv[6] * fx + inv[7] * fy + inv[8];
            let sx = (inv[0] * fx + inv[1] * fy + inv[2]) / z;
            let sy = (inv[3] * fx + inv[4] * fy + inv[5]) / z;
            let p = ((y * w + x) * 4) as usize;
            if sx < 0.0 || sy < 0.0 || sx >= (w - 1) as f64 || sy >= (h - 1) as f64 {
                out[p + 3] = 255;
                continue;
            }
            let (x0, y0) = (sx as u32, sy as u32);
            let (ax, ay) = (sx - x0 as f64, sy - y0 as f64);
            for c in 0..3 {
                let at = |xx: u32, yy: u32| src[((yy * w + xx) * 4) as usize + c] as f64;
                let top = at(x0, y0) * (1.0 - ax) + at(x0 + 1, y0) * ax;
                let bot = at(x0, y0 + 1) * (1.0 - ax) + at(x0 + 1, y0 + 1) * ax;
                out[p + c] = (top * (1.0 - ay) + bot * ay).round() as u8;
            }
            out[p + 3] = 255;
        }
    }
    out
}

fn blur(src: &[u8], w: u32, h: u32) -> Vec<u8> {
    let mut out = src.to_vec();
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            for c in 0..3 {
                let mut s = 0u32;
                for (dx, dy) in [(-1i32, 0i32), (1, 0), (0, -1), (0, 1), (0, 0)] {
                    s += src[(((y as i32 + dy) as u32 * w + (x as i32 + dx) as u32) * 4) as usize + c] as u32;
                }
                out[((y * w + x) * 4) as usize + c] = (s / 5) as u8;
            }
        }
    }
    out
}

/// The motion at frame `i` of `n` (30 fps): a hand's shake, a slow turn, a car's jolts.
fn motion_at(i: usize, cx: f64, cy: f64) -> [f64; 9] {
    let t = i as f64 / 30.0;
    let dx = 15.0 * (2.0 * std::f64::consts::PI * 0.7 * t).sin() + 4.0 * (2.0 * std::f64::consts::PI * 3.1 * t).sin();
    let dy = 10.0 * (2.0 * std::f64::consts::PI * 0.5 * t + 1.0).sin() + (if (t * 2.0) as u64 % 5 == 0 { 6.0 * (2.0 * std::f64::consts::PI * 6.0 * t).sin() } else { 0.0 });
    let angle = (10.0f64).to_radians() * (2.0 * std::f64::consts::PI * 0.08 * t).sin();
    let s = 1.0 + 0.04 * (2.0 * std::f64::consts::PI * 0.15 * t).sin();
    let (sn, cs) = angle.sin_cos();
    // about the phone's middle: T(c + d) · R·S · T(−c)
    let (a, b, c, d) = (s * cs, -s * sn, s * sn, s * cs);
    [a, b, cx + dx - a * cx - b * cy, c, d, cy + dy - c * cx - d * cy, 0.0, 0.0, 1.0]
}

fn apply(m: &[f64; 9], p: (f64, f64)) -> (f64, f64) {
    let z = m[6] * p.0 + m[7] * p.1 + m[8];
    ((m[0] * p.0 + m[1] * p.1 + m[2]) / z, (m[3] * p.0 + m[4] * p.1 + m[5]) / z)
}

#[test]
fn the_anchor_follows_a_shaken_turning_phone() {
    if cfg!(debug_assertions) {
        eprintln!("the phone stress test runs in an optimised build only (cargo test --release)");
        return;
    }
    let Ok(dir) = std::env::var("VIT_FRAMES") else {
        eprintln!("VIT_FRAMES not set: the phone stress test is skipped");
        return;
    };
    let dir = Path::new(&dir);
    for name in ["gta6-telefon-sohbet-1", "gta6-telefon-sohbet-2"] {
        let (src, w, h) = load(dir, name);
        let (empty, ..) = load(dir, "gta6-01-lucia");
        let pose = phone_pose::locate(&src, w, h).expect("phone");
        let screen = phone::screen_in_pose(phone::find_screen(&src, w, h), pose.bounds().px(w, h));
        let quad: Quad = phone::screen_quad(&src, w, h, screen).unwrap_or_else(|| {
            let (x, y, sw, sh) = (screen.0 as f64, screen.1 as f64, screen.2 as f64, screen.3 as f64);
            [(x, y), (x + sw, y), (x + sw, y + sh), (x, y + sh)]
        });
        let (cx, cy) = (quad.iter().map(|p| p.0).sum::<f64>() / 4.0, quad.iter().map(|p| p.1).sum::<f64>() / 4.0);
        let n = 900;
        let missing = [400usize, 401];
        let blurred = [200usize, 650];
        let mut a = Anchor::default();
        assert!(a.place(&src, w, h, quad));
        let mut errors = Vec::new();
        let mut lost_at = Vec::new();
        let mut times = Vec::new();
        for i in 1..n {
            let m = motion_at(i, cx, cy);
            let base = if missing.contains(&i) { warp(&empty, w, h, &m) } else { warp(&src, w, h, &m) };
            let f = if blurred.contains(&i) { blur(&base, w, h) } else { base };
            let truth = quad.map(|p| apply(&m, p));
            if !a.is_set() {
                // (the next reading sets the anchor up again where the phone is)
                if !missing.contains(&i) {
                    a.place(&f, w, h, truth);
                }
                continue;
            }
            let t0 = std::time::Instant::now();
            let moved = a.feed(&f, w, h, i as u64);
            times.push(t0.elapsed().as_secs_f64() * 1000.0);
            if moved == Moved::Lost {
                lost_at.push(i);
                continue;
            }
            if let Some(shown) = a.shown() {
                let e = shown.iter().zip(truth.iter()).map(|(s, t)| ((s.0 - t.0).powi(2) + (s.1 - t.1).powi(2)).sqrt()).fold(0.0, f64::max);
                errors.push(e);
            }
        }
        errors.sort_by(|a, b| a.partial_cmp(b).unwrap());
        times.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let med = errors[errors.len() / 2];
        let p95 = errors[errors.len() * 95 / 100];
        let t_avg = times.iter().sum::<f64>() / times.len() as f64;
        eprintln!("{name}: {} frames tracked, error median {med:.2} px, p95 {p95:.2} px, max {:.2} px; lost at {lost_at:?}; {t_avg:.1} ms avg, {:.1} ms p95 a frame", errors.len(), errors.last().unwrap(), times[times.len() * 95 / 100]);
        assert!(med <= 1.0, "{name}: median error {med:.2} px > 1");
        assert!(p95 <= 2.0, "{name}: p95 error {p95:.2} px > 2");
        assert!(lost_at.iter().all(|i| missing.contains(i)), "{name}: lost at {lost_at:?}, only {missing:?} have no phone");
        assert!(!lost_at.is_empty(), "{name}: the missing phone was not noticed");
    }
}
