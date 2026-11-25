//! The phone's screen followed on every captured frame, so its Arabic stays stuck to it (see screen_anchor):
//! set up at a reading that found the phone and its screen's corners, fed every frame after, lost when the
//! phone goes. What the overlays get: `phone-anchor` events with the screen's four corners now (fractions
//! of the frame) and the anchor's generation, or `{gen, lost: true}`.
//!
//! Ported from CouchSubs (AGPL-3.0, (C) 2026 Sinan Yorulmaz), commit bc54ff7:
//! core/Sources/CouchSubsCore/PhoneAnchor.swift (set up at a reading, fed every frame, set up again when the
//! reading's phone is far from the anchor's).

use std::sync::Mutex;
use std::time::Instant;

use crate::screen_anchor::{Gray, Motion, Rect, Smoother, Tracker, TRACK_WIDTH};

/// A quad's four corners (top left, top right, bottom right, bottom left), frame pixels.
pub type Quad = [(f64, f64); 4];

#[derive(Default)]
pub struct Anchor {
    tracker: Tracker,
    smoother: Smoother,
    /// the generation: new each time the anchor is set up
    pub generation: u64,
    /// the reference quad, frame pixels, and the tracking frame's scale (frame px per tracking px)
    reference: Option<(Quad, f64)>,
    /// the corners shown now, frame pixels
    shown: Option<Quad>,
    /// the frame size
    size: (u32, u32),
    /// the last fed frame's sequence (a frame is fed once)
    last_seq: u64,
    /// measurements: frames tracked, time spent (ms), inliers (sum) since the last report
    stats: (u64, f64, f64, f64),
    report_at: Option<Instant>,
}

/// What a frame did to the anchor.
#[derive(Clone, Debug, PartialEq)]
pub enum Moved {
    /// nothing to send (not set up, the same frame, or within the dead zone)
    Still,
    /// the corners now
    To(Quad),
    /// lost: hide what's drawn on it
    Lost,
}

fn bbox(q: &Quad) -> (f64, f64, f64, f64) {
    let xs = q.iter().map(|p| p.0);
    let ys = q.iter().map(|p| p.1);
    let (x0, x1) = (xs.clone().fold(f64::MAX, f64::min), xs.fold(f64::MIN, f64::max));
    let (y0, y1) = (ys.clone().fold(f64::MAX, f64::min), ys.fold(f64::MIN, f64::max));
    (x0, y0, x1 - x0, y1 - y0)
}

impl Anchor {
    pub fn is_set(&self) -> bool {
        self.tracker.is_active()
    }

    /// The corners as shown now (frame pixels), when set.
    pub fn shown(&self) -> Option<Quad> {
        self.shown.filter(|_| self.is_set())
    }

    /// Set up on the frame a reading found the screen's corners in (`rgba`, `w`×`h`). The anchor is set
    /// up again when it is lost, or the corners read are far from where it has them now (more than a
    /// quarter of the screen's height): otherwise it is kept (its layer doesn't move to a reading's
    /// slightly different corners).
    pub fn place(&mut self, rgba: &[u8], w: u32, h: u32, quad: Quad) -> bool {
        if self.is_set()
            && self.size == (w, h)
            && let Some(now) = self.shown
        {
            let (.., qh) = bbox(&quad);
            let far = now.iter().zip(quad.iter()).any(|(a, b)| ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt() > qh * 0.25);
            if !far {
                return false;
            }
        }
        let gray = Gray::from_rgba(rgba, w as usize, h as usize, false, TRACK_WIDTH);
        let k = w as f64 / gray.width as f64;
        let (x, y, bw, bh) = bbox(&quad);
        let rect = Rect { x: x / k, y: y / k, width: bw / k, height: bh / k };
        if rect.width < 16.0 || rect.height < 16.0 {
            return false;
        }
        self.tracker.begin(&gray, rect);
        self.smoother.reset();
        self.generation += 1;
        self.reference = Some((quad, k));
        self.shown = Some(quad);
        self.size = (w, h);
        crate::vlog!("[anchor] set up (gen {}; screen {:.0}×{:.0} at {:.0},{:.0})", self.generation, bw, bh, x, y);
        true
    }

    pub fn lose(&mut self, why: &str) -> bool {
        if !self.is_set() {
            return false;
        }
        crate::vlog!("[anchor] lost (gen {}; {why})", self.generation);
        self.tracker.reset();
        self.smoother.reset();
        self.reference = None;
        self.shown = None;
        true
    }

    /// One captured frame (`seq`: its number; each is fed once).
    pub fn feed(&mut self, rgba: &[u8], w: u32, h: u32, seq: u64) -> Moved {
        if !self.is_set() || seq == self.last_seq {
            return Moved::Still;
        }
        self.last_seq = seq;
        if (w, h) != self.size {
            self.lose("the picture's size changed");
            return Moved::Lost;
        }
        let Some((reference, k)) = self.reference else { return Moved::Still };
        let t0 = Instant::now();
        let Some(rect) = self.tracker.rect() else { return Moved::Still };
        // (only round where the screen is now, with room for a fast move: see Gray::from_rgba_region)
        let region = {
            let shown = self.shown.unwrap_or(reference);
            let (x, y, bw, bh) = bbox(&shown);
            let m = bw.max(bh) * 0.35;
            let c = |v: f64| (v / k).max(0.0) as usize;
            (c(x - m), c(y - m), c(x + bw + m) + 2, c(y + bh + m) + 2)
        };
        let gray = Gray::from_rgba_region(rgba, w as usize, h as usize, false, TRACK_WIDTH, region);
        let fix = self.tracker.track(&gray);
        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        self.stats.0 += 1;
        self.stats.1 += ms;
        self.stats.3 = self.stats.3.max(ms);
        let report = self.report_at.get_or_insert_with(Instant::now);
        if report.elapsed().as_secs() >= 10 && self.stats.0 > 0 {
            crate::vlog!("[anchor] gen {}, {} frames, inliers avg {:.0} %, {:.1} ms avg / {:.1} ms max", self.generation, self.stats.0, 100.0 * self.stats.2 / self.stats.0 as f64, self.stats.1 / self.stats.0 as f64, self.stats.3);
            self.stats = (0, 0.0, 0.0, 0.0);
            *report = Instant::now();
        }
        let Some(fix) = fix else {
            self.lose("too little of the screen fits: put away, scrolled or covered");
            return Moved::Lost;
        };
        self.stats.2 += fix.inliers;
        let (motion, jumped) = self.smoother.push(fix.motion, &rect);
        if jumped {
            self.lose("jumped");
            return Moved::Lost;
        }
        let now = corners(&reference, &motion, &rect, k);
        if self.shown.is_some_and(|s| s == now) {
            return Moved::Still;
        }
        self.shown = Some(now);
        Moved::To(now)
    }
}

/// The reference corners moved by `motion` (tracking-frame transform) into frame pixels.
fn corners(reference: &Quad, motion: &Motion, rect: &Rect, k: f64) -> Quad {
    let (cx, cy) = (rect.center_x(), rect.center_y());
    reference.map(|(x, y)| {
        let (tx, ty) = motion.map(x / k, y / k, cx, cy);
        (tx * k, ty * k)
    })
}

/// The app's one phone anchor.
pub static ANCHOR: Mutex<Option<Anchor>> = Mutex::new(None);

/// With the anchor.
pub fn with<R>(f: impl FnOnce(&mut Anchor) -> R) -> R {
    let mut g = ANCHOR.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    f(g.get_or_insert_with(Anchor::default))
}

/// The `phone-anchor` event for a move: `{gen, quad: [[x, y] × 4]}` as fractions of the frame, or
/// `{gen, lost: true}`.
pub fn event(generation: u64, moved: &Moved, w: u32, h: u32) -> Option<serde_json::Value> {
    let r = |v: f64| (v * 10000.0).round() / 10000.0;
    match moved {
        Moved::Still => None,
        Moved::Lost => Some(serde_json::json!({ "gen": generation, "lost": true })),
        Moved::To(q) => Some(serde_json::json!({ "gen": generation, "quad": q.iter().map(|p| [r(p.0 / w as f64), r(p.1 / h as f64)]).collect::<Vec<_>>() })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A frame with a textured "phone" at `quad`'s place, shifted by (dx, dy).
    fn frame(w: u32, h: u32, dx: f64, dy: f64) -> Vec<u8> {
        let mut px = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let (u, v) = (x as f64 - dx, y as f64 - dy);
                let inside = (800.0..1100.0).contains(&u) && (200.0..800.0).contains(&v);
                let val = if inside { 128.0 + 50.0 * (0.21 * u).sin() * (0.17 * v).cos() + 40.0 * (0.05 * (u + 2.0 * v)).sin() } else { 60.0 + 30.0 * (0.13 * x as f64 + 0.07 * y as f64).sin() };
                let p = ((y * w + x) * 4) as usize;
                let g = val.clamp(0.0, 255.0) as u8;
                px[p..p + 4].copy_from_slice(&[g, g, g, 255]);
            }
        }
        px
    }

    #[test]
    fn the_corners_follow_the_phone_and_hold_still_when_it_does() {
        let (w, h) = (1920, 1080);
        let quad: Quad = [(800.0, 200.0), (1100.0, 200.0), (1100.0, 800.0), (800.0, 800.0)];
        let mut a = Anchor::default();
        assert!(a.place(&frame(w, h, 0.0, 0.0), w, h, quad));
        // the same frame again: nothing
        assert_eq!(a.feed(&frame(w, h, 0.0, 0.0), w, h, 1), Moved::Still);
        // moved 9 px right, 5 px down: the corners with it, within a pixel
        match a.feed(&frame(w, h, 9.0, 5.0), w, h, 2) {
            Moved::To(q) => {
                for (got, want) in q.iter().zip(quad.iter()) {
                    assert!((got.0 - want.0 - 9.0).abs() < 1.0 && (got.1 - want.1 - 5.0).abs() < 1.0, "{got:?} vs {want:?}");
                }
            }
            m => panic!("{m:?}"),
        }
        // a reading's corners close to the anchor's: kept, not set up again
        assert!(!a.place(&frame(w, h, 9.0, 5.0), w, h, quad.map(|p| (p.0 + 10.0, p.1 + 6.0))));
        // the phone gone: lost
        let empty: Vec<u8> = (0..w * h).flat_map(|i| { let g = (60.0 + 30.0 * (0.13 * (i % w) as f64 + 0.07 * (i / w) as f64).sin()) as u8; [g, g, g, 255] }).collect();
        assert_eq!(a.feed(&empty, w, h, 3), Moved::Lost);
        assert!(!a.is_set());
        let e = event(a.generation, &Moved::Lost, w, h).unwrap();
        assert_eq!(e["lost"], true);
    }
}
