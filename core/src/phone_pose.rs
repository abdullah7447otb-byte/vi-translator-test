//! Where GTA VI's phone is held in the frame, tilted or not: its earpiece slot and camera dot at the top,
//! its home bar at the bottom, on a coarse 400×225 luma grid (a few milliseconds a frame). The pose
//! (top and bottom of the phone, as fractions of the frame) gives the phone's box and its tilt; the
//! tracker opens the phone after two readings agree and closes it after a few misses.
//!
//! Ported from CouchSubs (AGPL-3.0, (C) 2026 Sinan Yorulmaz), commit bc54ff7:
//! core/Sources/CouchSubsCore/GamePhoneTracker.swift (GamePhoneGeometry, GamePhoneTracker.plausible,
//! missGrace) and core/Sources/CouchSubsCore/GamePhonePixels.swift (nearest-pixel sampling).
//! Thresholds and constants are theirs, unchanged.

/// The phone's top (earpiece) and bottom (home bar) middles, fractions of the frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pose {
    pub top_x: f64,
    pub top_y: f64,
    pub bottom_x: f64,
    pub bottom_y: f64,
}

/// A box as fractions of the frame: left, top, right, bottom.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Region {
    pub left: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
}

impl Region {
    /// Every edge within `tol` (fraction of the frame) of the other's.
    pub fn is_close(&self, o: &Region, tol: f64) -> bool {
        (self.left - o.left).abs() <= tol && (self.top - o.top).abs() <= tol && (self.right - o.right).abs() <= tol && (self.bottom - o.bottom).abs() <= tol
    }
    /// In frame pixels: x, y, w, h.
    pub fn px(&self, fw: u32, fh: u32) -> (u32, u32, u32, u32) {
        let x0 = (self.left.max(0.0) * fw as f64).round() as u32;
        let y0 = (self.top.max(0.0) * fh as f64).round() as u32;
        let x1 = (self.right.min(1.0) * fw as f64).round() as u32;
        let y1 = (self.bottom.min(1.0) * fh as f64).round() as u32;
        (x0, y0, x1.saturating_sub(x0), y1.saturating_sub(y0))
    }
}

impl Pose {
    /// The phone's box round its body (the casing a quarter of the phone's width beyond the slots' line).
    pub fn bounds(&self) -> Region {
        let h = self.bottom_y - self.top_y;
        let w = h * 225.0 / 400.0 * 0.25;
        Region { top: self.top_y + h * 0.012, bottom: self.bottom_y - h * 0.015, left: self.top_x.min(self.bottom_x) - w, right: self.top_x.max(self.bottom_x) + w }
    }
    /// The phone's screen's four corners in a `fw`×`fh` frame (top left, top right, bottom right, bottom
    /// left, pixels), from the pose alone: along the phone's axis from its earpiece to its home bar, the screen
    /// from 3 % to 97.5 % of the way, as wide as 0.54 of that length (GTA VI's phone, measured on CouchSubs'
    /// frames). For a tilted phone whose screen edges aren't found by brightness.
    pub fn screen_quad(&self, fw: u32, fh: u32) -> [(f64, f64); 4] {
        self.screen_quad_turned(fw, fh, None)
    }

    /// `screen_quad`, turned to `turn` degrees clockwise (the screen's text lines' own slant, when it is read:
    /// a phone tilted in the hand towards the camera leans its edges more than its lines, and the Arabic follows
    /// the lines), about the phone's middle.
    pub fn screen_quad_turned(&self, fw: u32, fh: u32, turn: Option<f64>) -> [(f64, f64); 4] {
        let (tx0, ty0) = (self.top_x * fw as f64, self.top_y * fh as f64);
        let (bx0, by0) = (self.bottom_x * fw as f64, self.bottom_y * fh as f64);
        let len = ((bx0 - tx0).powi(2) + (by0 - ty0).powi(2)).sqrt().max(1.0);
        let (mx, my) = ((tx0 + bx0) / 2.0, (ty0 + by0) / 2.0);
        // (the axis from the earpiece to the home bar: turned clockwise by θ, (−sin θ, cos θ))
        let theta = turn.unwrap_or_else(|| self.tilt(fw, fh)).to_radians();
        let (ux, uy) = (-theta.sin(), theta.cos());
        let (tx, ty) = (mx - ux * len / 2.0, my - uy * len / 2.0);
        // (the axis turned a quarter counter-clockwise points to the screen's left: −x for an upright phone)
        let (nx, ny) = (-uy, ux);
        let half = 0.27 * len;
        let _ = (bx0, by0);
        let at = |t: f64, side: f64| (tx + ux * t * len + nx * side * half, ty + uy * t * len + ny * side * half);
        [at(0.03, 1.0), at(0.03, -1.0), at(0.975, -1.0), at(0.975, 1.0)]
    }

    /// The phone's lean in the frame, degrees clockwise (its bottom to the right of its top: positive),
    /// measured in pixels of a `fw`×`fh` frame.
    pub fn tilt(&self, fw: u32, fh: u32) -> f64 {
        let dx = (self.bottom_x - self.top_x) * fw as f64;
        let dy = (self.bottom_y - self.top_y) * fh as f64;
        -(dx.atan2(dy)).to_degrees()
    }
}

#[derive(Clone, Copy, Debug)]
struct Slot {
    x: f64,
    y: i32,
    length: i32,
    contrast: i32,
}

/// Luma samples of part of the picture on the 400×225 grid, two pixels a cell each way (nearest pixel).
struct Grid {
    w2: i32,
    h2: i32,
    left: i32,
    luma: Vec<i32>,
}

impl Grid {
    /// Columns `c0..c1` and rows `0..rows` of the grid, from the picture `pic` (x, y, w, h) of the frame.
    fn new(rgba: &[u8], fw: u32, pic: (u32, u32, u32, u32), c0: i32, c1: i32, rows: i32) -> Grid {
        let (px, py, pw, ph) = (pic.0 as f64, pic.1 as f64, pic.2 as f64, pic.3 as f64);
        let (xs, ys) = (pw / 400.0, ph / 225.0);
        let (cx, cy, cw, ch) = (c0 as f64 * xs, 0.0, (c1 - c0) as f64 * xs, rows as f64 * ys);
        let (w2, h2) = (2 * (c1 - c0), 2 * rows);
        let mut luma = vec![0i32; (w2 * h2) as usize];
        for j in 0..h2 {
            let sy = (py + cy + ((j as f64 + 0.5) * ch / h2 as f64).floor()).min(py + ph - 1.0) as usize;
            for i in 0..w2 {
                let sx = (px + cx + ((i as f64 + 0.5) * cw / w2 as f64).floor()).min(px + pw - 1.0) as usize;
                let p = (sy * fw as usize + sx) * 4;
                luma[(j * w2 + i) as usize] = 77 * rgba[p] as i32 + 150 * rgba[p + 1] as i32 + 29 * rgba[p + 2] as i32;
            }
        }
        Grid { w2, h2, left: c0, luma }
    }

    /// The grid cell's tone (0–255); cells outside give the nearest edge's.
    fn at(&self, x: i32, y: i32) -> i32 {
        let i = (2 * (x - self.left)).clamp(0, self.w2 - 2);
        let j = (2 * y).clamp(0, self.h2 - 2);
        let p = (j * self.w2 + i) as usize;
        let w = self.w2 as usize;
        (self.luma[p] + self.luma[p + 1] + self.luma[p + w] + self.luma[p + w + 1]) >> 10
    }
}

fn rounded(v: f64) -> i32 {
    // (round half to even, as theirs)
    let r = v.round();
    if (v - v.trunc()).abs() == 0.5 && (r as i64) % 2 != 0 { (r - v.signum()) as i32 } else { r as i32 }
}
fn limit(n: i32, lo: i32, hi: i32) -> i32 {
    n.clamp(lo, hi)
}

fn slots(g: &Grid, r0: i32, r1: i32, lmin: i32, lmax: i32, maximum: usize) -> Vec<Slot> {
    let (left, width) = (95i32, 211i32);
    let rows = r1 - r0;
    let mut band = vec![0u8; ((rows + 8) * width) as usize];
    for y in (r0 - 4)..(r1 + 4) {
        let row = (y - r0 + 4) * width;
        for i in 0..width {
            band[(row + i) as usize] = g.at(left + i, y) as u8;
        }
    }
    let b = |i: i32| band[i as usize] as i32;
    let mut found = Vec::new();
    for y in r0..r1 {
        let row = (y - r0 + 4) * width;
        let mut start: Option<i32> = None;
        for x in (105 - left)..=(294 - left) {
            let p = row + x;
            let surround = b(p - 3 * width).min(b(p + 3 * width));
            if x < 294 - left && surround - b(p) >= limit(surround / 3, 14, 25) {
                if start.is_none() {
                    start = Some(x);
                }
                continue;
            }
            if let Some(s) = start {
                let len = x - s;
                if (lmin..=lmax).contains(&len) {
                    let center = row + s + len / 2;
                    let end = p + 1;
                    let border = row + s - 2;
                    let mut rim = [b(border - 4 * width), b(center - 4 * width), b(end - 4 * width), b(border + 4 * width), b(center + 4 * width), b(end + 4 * width), b(border), b(end)];
                    rim.sort_unstable();
                    let tone = (rim[3] + rim[4]) / 2;
                    let contrast = tone - b(center);
                    if contrast >= limit(tone / 3, 20, 33) && rim[7] - rim[0] <= 24.max(contrast * 3 / 4) {
                        if found.len() >= maximum {
                            return Vec::new();
                        }
                        found.push(Slot { x: left as f64 + (s + x - 1) as f64 / 2.0, y, length: len, contrast });
                    }
                }
            }
            start = None;
        }
    }
    found
}

fn has_camera(g: &Grid, top: &Slot) -> bool {
    for side in [-1i32, 1] {
        let d0 = 5.max(rounded(top.length as f64 * 0.62));
        let d1 = rounded(top.length as f64 * 1.15);
        for d in d0..=d1 {
            for y in (top.y - 2)..=(top.y + 2) {
                let x = rounded(top.x) + side * d;
                let (above, below, l, r) = (g.at(x, y - 3), g.at(x, y + 3), g.at(x - 3, y), g.at(x + 3, y));
                let low = above.min(below).min(l).min(r);
                let high = above.max(below).max(l).max(r);
                if low - g.at(x, y) >= limit(low / 3, 15, 23) && ((high - low) as f64) < 25f64.max(top.contrast as f64 * 0.9) {
                    return true;
                }
            }
        }
    }
    false
}

fn smooth_rim(g: &Grid, s: &Slot, dx: f64, h: f64) -> bool {
    let r = (h * 0.14) as i32;
    [-r, 3 - r, r - 3, r]
        .iter()
        .filter(|&&k| {
            let x = rounded(s.x) + k;
            let y = s.y + rounded(-(k as f64) * dx / h);
            (g.at(x, y - 2) - g.at(x, y + 2)).abs() < 16
        })
        .count()
        >= 3
}

fn rim_boundary(g: &Grid, s: &Slot, dx: f64, h: f64, lower: bool) -> i32 {
    let mut best = 0;
    let (from, to) = if lower { (-10, -4) } else { (3, 7) };
    let mut distance = from;
    while distance <= to {
        let mut samples: Vec<i32> = (-2..=2)
            .map(|i| {
                let k = i as f64 * 0.08;
                let d = distance as f64 * 0.01;
                let x = rounded(s.x + k * h + dx * d);
                let y = rounded(s.y as f64 + h * d - (if lower { 0.8 } else { 0.52 }) * dx * k);
                (g.at(x, y - 2) - g.at(x, y + 2)).abs()
            })
            .collect();
        samples.sort_unstable();
        best = best.max(samples[2]);
        distance += 2;
    }
    best
}

fn side_strength(g: &Grid, s: &Slot, dx: f64, h: f64, side: i32) -> i32 {
    let mut best = 0;
    for width in 23..=27 {
        let r = (side * width) as f64 * 0.01;
        let mut samples: Vec<i32> = [0.10, 0.24, 0.39, 0.54, 0.69, 0.84]
            .iter()
            .map(|t| {
                let x = rounded(s.x + t * dx + r * h);
                let y = rounded(s.y as f64 + t * h - r * dx);
                (g.at(x - 4, y) - g.at(x, y)).abs().max((g.at(x - 2, y) - g.at(x + 2, y)).abs()).max((g.at(x, y) - g.at(x + 4, y)).abs())
            })
            .collect();
        samples.sort_unstable();
        best = best.max((samples[2] + samples[3]) / 2);
    }
    best
}

/// The phone in a 16:9 picture `pic` of the frame, as fractions of that picture.
fn locate_picture(rgba: &[u8], fw: u32, pic: (u32, u32, u32, u32)) -> Option<Pose> {
    if pic.2 < 400 || pic.3 < 225 || (pic.2 as f64 / pic.3 as f64 - 400.0 / 225.0).abs() > 0.04 {
        return None;
    }
    let top_band = Grid::new(rgba, fw, pic, 80, 320, 90);
    // (normal play ends here: the body is looked at only with an earpiece and a camera in the top band)
    let upper: Vec<Slot> = slots(&top_band, 17, 75, 8, 19, 12).into_iter().filter(|s| has_camera(&top_band, s)).collect();
    if upper.is_empty() {
        return None;
    }
    let grid = Grid::new(rgba, fw, pic, 40, 360, 225);
    let lower = slots(&grid, 145, 217, 5, 14, 16);
    let mut best = 0.0;
    let mut pose = None;
    for top in &upper {
        for bottom in &lower {
            let h = (bottom.y - top.y) as f64;
            let dx = bottom.x - top.x;
            let (tl, bl) = (top.length as f64, bottom.length as f64);
            if !((115.0..=179.0).contains(&h)
                && dx.abs() <= h * 0.24
                && bl >= tl * 0.46
                && bottom.length <= top.length
                && h >= 7.9 * tl
                && h <= 16.5 * tl
                && h >= 12.0 * bl
                && h <= 30.0 * bl
                && smooth_rim(&grid, top, dx, h)
                && smooth_rim(&grid, bottom, dx, h))
            {
                continue;
            }
            let left = side_strength(&grid, top, dx, h, -1);
            if left < 12 {
                continue;
            }
            let right = side_strength(&grid, top, dx, h, 1);
            if right < 12 {
                continue;
            }
            let edges = left.min(right) as f64 + 0.25 * left.max(right) as f64;
            let rims = rim_boundary(&grid, top, dx, h, false) + rim_boundary(&grid, bottom, dx, h, true);
            let score = edges + 0.1 * top.contrast.min(bottom.contrast) as f64 + 0.5 * rims as f64;
            if score > best {
                best = score;
                pose = Some(Pose { top_x: top.x / 400.0, top_y: top.y as f64 / 225.0, bottom_x: bottom.x / 400.0, bottom_y: bottom.y as f64 / 225.0 });
            }
        }
    }
    pose
}

/// A pitch-black bar's samples are all darker than this (0–255).
const DARK_TONE: i32 = 24;

/// The 16:9 game picture inside the frame (pixels): a window's frame carries its title bar on top and black
/// bars where the window's shape isn't the picture's. The black bars at the bottom and sides are dropped;
/// the picture is as tall as 16:9 of the width left, at the bottom. The whole frame when it is 16:9.
pub fn picture(fw: u32, fh: u32, tone: impl Fn(u32, u32) -> i32) -> Option<(u32, u32, u32, u32)> {
    let (width, height) = (fw as i64, fh as i64);
    if width < 400 || height < 225 {
        return None;
    }
    if (width as f64 / height as f64 - 400.0 / 225.0).abs() <= 0.04 {
        return Some((0, 0, fw, fh));
    }
    let columns: Vec<i64> = (0..64).map(|i| (width - 1).min(((i as f64 + 0.5) * width as f64 / 64.0) as i64)).collect();
    let dark_row = |y: i64| columns.iter().all(|&x| tone(x as u32, y as u32) < DARK_TONE);
    let mut bottom = height;
    while bottom > height / 2 && dark_row(bottom - 1) {
        bottom -= 1;
    }
    let from = height * 2 / 5;
    let rows: Vec<i64> = (0..48).map(|i| from + ((i as f64 + 0.5) * (bottom - from) as f64 / 48.0) as i64).collect();
    let dark_col = |x: i64| rows.iter().all(|&y| tone(x as u32, y as u32) < DARK_TONE);
    let (mut left, mut right) = (0i64, width);
    while left < width / 4 && dark_col(left) {
        left += 1;
    }
    while right > width * 3 / 4 && dark_col(right - 1) {
        right -= 1;
    }
    let mut w = right - left;
    let mut h = (w as f64 * 9.0 / 16.0).round() as i64;
    if h > bottom {
        h = bottom;
        w = (h as f64 * 16.0 / 9.0).round() as i64;
        left = (left + right - w) / 2;
    }
    if w < 400 || h < 225 || left < 0 || left + w > width {
        return None;
    }
    Some((left as u32, (bottom - h) as u32, w as u32, h as u32))
}

/// The phone held in the frame (RGBA, `fw`×`fh`), as fractions of the frame; None without one.
pub fn locate(rgba: &[u8], fw: u32, fh: u32) -> Option<Pose> {
    if fw < 400 || fh < 225 || rgba.len() < (fw * fh * 4) as usize {
        return None;
    }
    let tone = |x: u32, y: u32| {
        let p = ((y.min(fh - 1) * fw + x.min(fw - 1)) * 4) as usize;
        (77 * rgba[p] as i32 + 150 * rgba[p + 1] as i32 + 29 * rgba[p + 2] as i32) >> 8
    };
    let area = picture(fw, fh, tone)?;
    let p = locate_picture(rgba, fw, area)?;
    let (w, h) = (fw as f64, fh as f64);
    let x = |v: f64| (area.0 as f64 + v * area.2 as f64) / w;
    let y = |v: f64| (area.1 as f64 + v * area.3 as f64) / h;
    Some(Pose { top_x: x(p.top_x), top_y: y(p.top_y), bottom_x: x(p.bottom_x), bottom_y: y(p.bottom_y) })
}

/// The phone's box area as a share of the frame must be in this range.
pub const MIN_AREA: f64 = 0.04;
pub const MAX_AREA: f64 = 0.35;

/// A phone box that makes sense: 4–35 % of the frame, upright in pixels (height/width 0.9–2.6; a tilted
/// phone's box is wider than an upright one's), at most 85 % of the frame's height and half its width, its
/// middle not at the frame's left edge nor in its top third.
pub fn plausible(r: &Region, fw: u32, fh: u32) -> bool {
    let (w, h) = (r.right - r.left, r.bottom - r.top);
    if !(w > 0.0 && h > 0.0 && h <= 0.85 && w <= 0.5) {
        return false;
    }
    let area = w * h;
    if !(MIN_AREA..=MAX_AREA).contains(&area) {
        return false;
    }
    if fw >= 64 {
        let aspect = h * fh.max(1) as f64 / (w * fw.max(1) as f64);
        if !(0.9..=2.6).contains(&aspect) {
            return false;
        }
    }
    (r.left + r.right) / 2.0 >= 0.35 && (r.top + r.bottom) / 2.0 >= 1.0 / 3.0
}

/// What the tracker did with a sample (for the log).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Change {
    None,
    Opened,
    Closed,
    /// found twice in a row, too far apart to open
    Moving,
}

/// The phone over time: opened when two samples in a row find it in the same place (one menu or
/// subtitle frame can't open it), kept on a sample near it, closed after `miss_grace` samples without it
/// (a frame it wasn't read in, a shaking hand: it stays).
pub struct Tracker {
    previous: Option<Pose>,
    shown: Option<Pose>,
    misses: u32,
    pub miss_grace: u32,
}

impl Default for Tracker {
    fn default() -> Self {
        Self::new()
    }
}

impl Tracker {
    pub const fn new() -> Self {
        Tracker { previous: None, shown: None, misses: 0, miss_grace: 3 }
    }

    /// A sample's pose (None: none found, or not plausible); the phone as shown now, and what changed.
    pub fn sample(&mut self, found: Option<Pose>, fw: u32, fh: u32) -> (Option<Pose>, Change) {
        let was = self.shown.is_some();
        let found = found.filter(|p| plausible(&p.bounds(), fw, fh));
        let near = |a: &Pose, b: &Pose| a.bounds().is_close(&b.bounds(), 0.035);
        match (found, self.previous, self.shown) {
            (Some(f), Some(p), _) if near(&f, &p) => {
                self.shown = Some(f);
                self.misses = 0;
            }
            (Some(f), _, Some(s)) if near(&f, &s) => {
                self.shown = Some(f);
                self.misses = 0;
            }
            _ if self.shown.is_some() && self.misses < self.miss_grace => self.misses += 1,
            _ => {
                self.shown = None;
                self.misses = 0;
            }
        }
        let moving = self.shown.is_none() && found.is_some() && self.previous.is_some();
        self.previous = found;
        let change = match (was, self.shown.is_some()) {
            (false, true) => Change::Opened,
            (true, false) => Change::Closed,
            _ if moving => Change::Moving,
            _ => Change::None,
        };
        (self.shown, change)
    }

    /// The phone as shown now.
    pub fn shown(&self) -> Option<Pose> {
        self.shown
    }

    pub fn reset(&mut self) {
        *self = Tracker { miss_grace: self.miss_grace, ..Default::default() };
    }
}

/// Misses before the phone closes, for a read every `interval_ms`: as many as fit in `hold`, 2 at least.
pub fn miss_grace_for(interval_ms: u64, hold: std::time::Duration) -> u32 {
    ((hold.as_millis() as u64 / interval_ms.max(1)) as u32).max(2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn implausible_boxes_are_rejected() {
        let r = |top, bottom, left, right| Region { top, bottom, left, right };
        assert!(plausible(&r(0.15, 0.85, 0.45, 0.64), 1920, 1080));
        assert!(!plausible(&r(0.0, 1.0, 0.0, 1.0), 1920, 1080));
        assert!(!plausible(&r(0.05, 0.95, 0.2, 0.8), 1920, 1080));
        assert!(!plausible(&r(0.6, 0.75, 0.4, 0.95), 1920, 1080));
        assert!(!plausible(&r(0.15, 0.85, 0.02, 0.2), 1920, 1080));
        assert!(!plausible(&r(0.0, 0.5, 0.5, 0.64), 1920, 1080));
        assert!(!plausible(&r(0.5, 0.6, 0.6, 0.63), 1920, 1080));
    }

    #[test]
    fn flat_screens_open_no_phone() {
        for c in [[0x24u8, 0x27, 0x2a], [0, 0, 0], [0x1a, 0x1c, 0x22], [255, 255, 255]] {
            let mut px = vec![255u8; 1920 * 1080 * 4];
            for p in px.chunks_exact_mut(4) {
                p[..3].copy_from_slice(&c);
            }
            assert_eq!(locate(&px, 1920, 1080), None);
        }
    }

    #[test]
    fn the_tracker_opens_on_two_and_closes_after_its_grace() {
        let p = Pose { top_x: 0.70, top_y: 0.20, bottom_x: 0.71, bottom_y: 0.86 };
        let mut t = Tracker::default();
        assert_eq!(t.sample(Some(p), 1920, 1080), (None, Change::None));
        assert_eq!(t.sample(Some(p), 1920, 1080).1, Change::Opened);
        // missed: kept for its grace
        for _ in 0..3 {
            assert!(t.sample(None, 1920, 1080).0.is_some());
        }
        assert_eq!(t.sample(None, 1920, 1080), (None, Change::Closed));
        // a full-screen "phone" never opens
        let full = Pose { top_x: 0.5, top_y: 0.0, bottom_x: 0.5, bottom_y: 1.0 };
        let mut t = Tracker::default();
        t.sample(Some(full), 1920, 1080);
        assert_eq!(t.sample(Some(full), 1920, 1080).0, None);
    }

    #[test]
    fn an_upright_phone_s_screen_corners_go_round_clockwise() {
        let p = Pose { top_x: 0.70, top_y: 0.20, bottom_x: 0.70, bottom_y: 0.86 };
        let q = p.screen_quad(1920, 1080);
        // top left, top right, bottom right, bottom left
        assert!(q[0].0 < q[1].0 && q[3].0 < q[2].0 && q[0].1 < q[3].1 && q[1].1 < q[2].1, "{q:?}");
        assert!(((q[1].0 - q[0].0) - 0.54 * 0.66 * 1080.0).abs() < 1.0);
    }

    #[test]
    fn a_phone_leaning_right_tilts_clockwise() {
        let p = Pose { top_x: 0.50, top_y: 0.16, bottom_x: 0.55, bottom_y: 0.82 };
        // bottom to the right: counter-clockwise lean of the top… expressed as the phone's turn
        let t = p.tilt(1920, 1080);
        assert!((t.abs() - (0.05f64 * 1920.0).atan2(0.66 * 1080.0).to_degrees()).abs() < 1e-9);
        assert_eq!(miss_grace_for(330, std::time::Duration::from_millis(990)), 3);
        assert_eq!(miss_grace_for(1000, std::time::Duration::from_millis(900)), 2);
    }
}
