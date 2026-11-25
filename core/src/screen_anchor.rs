//! The phone's (or a card's) quad tracked on every frame, without OCR, below a pixel: the quad's picture in
//! the reference frame is the template; on each new frame its projective transform (shift, scale, turn and
//! the perspective of a phone tilting in the hand) is found by inverse-compositional Lucas–Kanade on a
//! pyramid, with robust weights. Text changing on the screen, a hand passing in front, lose their weight;
//! the template is never updated (no drift). When too little fits (a chat scrolled, the phone put away,
//! the scene changed) the anchor is lost and set up again at the next read. The translation layer is laid
//! out once in the reference frame and only moved by this transform: its text size and line breaks never
//! change.
//!
//! Ported from CouchSubs (AGPL-3.0, (C) 2026 Sinan Yorulmaz), commit bc54ff7:
//! core/Sources/CouchSubsCore/ScreenAnchor.swift. Steps and thresholds are theirs, unchanged; their
//! shared test vectors (Fixtures/screen-anchor.json) are core/tests/fixtures/screen-anchor.json here.

/// Width of the tracking frame (pixels): frames are shrunk to it (half a 1920 frame).
pub const TRACK_WIDTH: usize = 960;
const LEVELS: usize = 4;
const COARSEST_WIDTH: f64 = 20.0;
const SEARCH_RADIUS: i32 = 3;
const MAX_POINTS: f64 = 2500.0;
const ITERATIONS: usize = 15;
/// Robust weight threshold (grey levels): full weight up to it, huber/size up to 3×, none beyond.
const HUBER: f64 = 8.0;
/// Fewer fitting points than this share: the anchor is lost.
pub const LOST_SHARE: f64 = 0.45;
/// Sampled this far inside the edges (the scene behind the phone stays out of the template).
const INSET: f64 = 0.03;

fn level_count(width: f64) -> usize {
    let mut count = 1;
    while count < LEVELS && width / (1usize << count) as f64 >= COARSEST_WIDTH {
        count += 1;
    }
    count
}

/// A grey frame, row by row from the top.
#[derive(Clone, Debug)]
pub struct Gray {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u8>,
}

impl Gray {
    pub fn new(width: usize, height: usize, pixels: Vec<u8>) -> Gray {
        assert!(width > 1 && height > 1 && pixels.len() >= width * height);
        Gray { width, height, pixels }
    }

    /// From an RGBA (or BGRA) frame, shrunk by a whole factor to about `target_width`: a box average of
    /// the brightness (2R + 5G + B) / 8. The factor (tracking frame → frame) is `frame width / width`.
    pub fn from_rgba(bytes: &[u8], width: usize, height: usize, bgr: bool, target_width: usize) -> Gray {
        let factor = ((width as f64 / target_width as f64).round() as usize).max(1);
        let (w, h) = ((width / factor).max(2), (height / factor).max(2));
        let mut out = vec![0u8; w * h];
        let (r, b) = if bgr { (2, 0) } else { (0, 2) };
        let count = (factor * factor) as u32;
        for y in 0..h {
            for x in 0..w {
                let mut sum = 0u32;
                for yy in 0..factor {
                    let mut p = ((height - 1).min(y * factor + yy) * width + (width - 1).min(x * factor)) * 4;
                    for _ in 0..factor {
                        sum += 2 * bytes[p + r] as u32 + 5 * bytes[p + 1] as u32 + bytes[p + b] as u32;
                        p += 4;
                    }
                }
                out[y * w + x] = ((sum + 4 * count) / (8 * count)).min(255) as u8;
            }
        }
        Gray { width: w, height: h, pixels: out }
    }

    /// `from_rgba`, worked out only inside `region` (tracking-frame pixels: x0, y0, x1, y1); the rest is 0.
    /// The anchor needs only the part round its quad: the whole 1920 frame took ~10 ms, its quad's
    /// neighbourhood a fraction of that.
    pub fn from_rgba_region(bytes: &[u8], width: usize, height: usize, bgr: bool, target_width: usize, region: (usize, usize, usize, usize)) -> Gray {
        let factor = ((width as f64 / target_width as f64).round() as usize).max(1);
        let (w, h) = ((width / factor).max(2), (height / factor).max(2));
        let mut out = vec![0u8; w * h];
        let (r, b) = if bgr { (2, 0) } else { (0, 2) };
        let count = (factor * factor) as u32;
        let (x0, y0, x1, y1) = (region.0.min(w), region.1.min(h), region.2.min(w), region.3.min(h));
        if factor == 2 && width >= 2 * w && height >= 2 * h {
            // (the usual case, 1920 → 960: two rows, two pixels each, without bounds arithmetic per pixel)
            let luma = |c: &[u8]| 2 * c[r] as u32 + 5 * c[1] as u32 + c[b] as u32;
            for y in y0..y1 {
                let a = &bytes[(2 * y * width) * 4..(2 * y * width + width) * 4];
                let bb = &bytes[((2 * y + 1) * width) * 4..((2 * y + 1) * width + width) * 4];
                let row_out = &mut out[y * w..(y + 1) * w];
                for x in x0..x1 {
                    let i = 8 * x;
                    let sum = luma(&a[i..i + 4]) + luma(&a[i + 4..i + 8]) + luma(&bb[i..i + 4]) + luma(&bb[i + 4..i + 8]);
                    row_out[x] = ((sum + 16) / 32).min(255) as u8;
                }
            }
            return Gray { width: w, height: h, pixels: out };
        }
        for y in y0..y1 {
            let row_out = &mut out[y * w..(y + 1) * w];
            for (x, o) in row_out.iter_mut().enumerate().take(x1).skip(x0) {
                let mut sum = 0u32;
                for yy in 0..factor {
                    let sy = (height - 1).min(y * factor + yy);
                    let start = (sy * width + (width - 1).min(x * factor)) * 4;
                    let px = &bytes[start..(start + 4 * factor).min(bytes.len())];
                    for c in px.chunks_exact(4) {
                        sum += 2 * c[r] as u32 + 5 * c[1] as u32 + c[b] as u32;
                    }
                }
                *o = ((sum + 4 * count) / (8 * count)).min(255) as u8;
            }
        }
        Gray { width: w, height: h, pixels: out }
    }

    fn half(&self) -> Gray {
        let (w, h) = ((self.width / 2).max(2), (self.height / 2).max(2));
        let mut out = vec![0u8; w * h];
        for y in 0..h {
            let r0 = (self.height - 1).min(2 * y) * self.width;
            let r1 = (self.height - 1).min(2 * y + 1) * self.width;
            for x in 0..w {
                let (x0, x1) = ((self.width - 1).min(2 * x), (self.width - 1).min(2 * x + 1));
                let sum = self.pixels[r0 + x0] as u32 + self.pixels[r0 + x1] as u32 + self.pixels[r1 + x0] as u32 + self.pixels[r1 + x1] as u32;
                out[y * w + x] = ((sum + 2) / 4) as u8;
            }
        }
        Gray { width: w, height: h, pixels: out }
    }

    /// Bilinear sample; None outside the frame.
    #[inline(always)]
    fn sample(&self, x: f64, y: f64) -> Option<f64> {
        if !(x >= 0.0 && y >= 0.0 && x <= (self.width - 1) as f64 && y <= (self.height - 1) as f64) {
            return None;
        }
        let x0 = (self.width - 2).min(x as usize);
        let y0 = (self.height - 2).min(y as usize);
        let (fx, fy) = (x - x0 as f64, y - y0 as f64);
        let i = y0 * self.width + x0;
        let (p00, p10) = (self.pixels[i] as f64, self.pixels[i + 1] as f64);
        let (p01, p11) = (self.pixels[i + self.width] as f64, self.pixels[i + self.width + 1] as f64);
        let top = p00 + (p10 - p00) * fx;
        let bottom = p01 + (p11 - p01) * fx;
        Some(top + (bottom - top) * fy)
    }
}

/// A rectangle in tracking-frame pixels: its top left and size.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Rect {
    pub fn center_x(&self) -> f64 {
        self.x + self.width / 2.0
    }
    pub fn center_y(&self) -> f64 {
        self.y + self.height / 2.0
    }
}

/// From the reference frame to the current one. With u = x − cx, v = y − cy round the reference quad's
/// middle (cx, cy), a point goes to (cx + (a u + b v + dx) / w, cy + (c u + d v + dy) / w), w = 1 + g u + h v
/// — the same as CSS `matrix3d` (`matrix(a, c, b, d, dx, dy)` when g = h = 0).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Motion {
    pub a: f64,
    pub b: f64,
    pub c: f64,
    pub d: f64,
    pub dx: f64,
    pub dy: f64,
    pub g: f64,
    pub h: f64,
}

impl Default for Motion {
    fn default() -> Self {
        Motion::IDENTITY
    }
}

impl Motion {
    pub const IDENTITY: Motion = Motion { a: 1.0, b: 0.0, c: 0.0, d: 1.0, dx: 0.0, dy: 0.0, g: 0.0, h: 0.0 };

    pub fn from_slice(v: &[f64]) -> Motion {
        Motion { a: v[0], b: v[1], c: v[2], d: v[3], dx: v[4], dy: v[5], g: v[6], h: v[7] }
    }
    pub fn to_array(&self) -> [f64; 8] {
        [self.a, self.b, self.c, self.d, self.dx, self.dy, self.g, self.h]
    }
    pub fn scale(&self) -> f64 {
        (self.a * self.d - self.b * self.c).abs().sqrt()
    }
    pub fn map(&self, x: f64, y: f64, cx: f64, cy: f64) -> (f64, f64) {
        let (u, v) = (x - cx, y - cy);
        let w = 1.0 + self.g * u + self.h * v;
        (cx + (self.a * u + self.b * v + self.dx) / w, cy + (self.c * u + self.d * v + self.dy) / w)
    }
    fn scaled(&self, f: f64) -> Motion {
        Motion { dx: self.dx / f, dy: self.dy / f, g: self.g * f, h: self.h * f, ..*self }
    }
    /// The farthest the quad's corners land apart under the two transforms (pixels).
    pub fn distance(&self, o: &Motion, r: &Rect) -> f64 {
        let (cx, cy) = (r.center_x(), r.center_y());
        [(r.x, r.y), (r.x + r.width, r.y), (r.x, r.y + r.height), (r.x + r.width, r.y + r.height)]
            .iter()
            .map(|&(x, y)| {
                let (p, q) = (self.map(x, y, cx, cy), o.map(x, y, cx, cy));
                ((p.0 - q.0).powi(2) + (p.1 - q.1).powi(2)).sqrt()
            })
            .fold(0.0, f64::max)
    }
    /// The same transform for frames `k` times bigger than the tracking frame (its points multiplied by k).
    pub fn in_frame(&self, k: f64) -> Motion {
        self.scaled(1.0 / k)
    }
}

/// A frame's result: the transform and the share of fitting points (0–1).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fix {
    pub motion: Motion,
    pub inliers: f64,
}

#[derive(Clone, Default)]
struct Level {
    factor: f64,
    offset: f64,
    u: Vec<f64>,
    v: Vec<f64>,
    t: Vec<f64>,
    gx: Vec<f64>,
    gy: Vec<f64>,
}

/// The template tracker: `begin` makes the quad of the reference frame its template; `track` finds the
/// transform in each new frame, None once the anchor is lost (then `begin` again).
#[derive(Default)]
pub struct Tracker {
    rect: Option<Rect>,
    motion: Motion,
    previous: Option<Motion>,
    pyramid: Vec<Level>,
}

impl Tracker {
    pub fn new() -> Tracker {
        Tracker::default()
    }
    pub fn rect(&self) -> Option<Rect> {
        self.rect
    }
    pub fn motion(&self) -> Motion {
        self.motion
    }
    pub fn is_active(&self) -> bool {
        self.rect.is_some()
    }

    pub fn begin(&mut self, frame: &Gray, rect: Rect) {
        let inner = Rect { x: rect.x + rect.width * INSET, y: rect.y + rect.height * INSET, width: rect.width * (1.0 - 2.0 * INSET), height: rect.height * (1.0 - 2.0 * INSET) };
        self.rect = Some(rect);
        self.motion = Motion::IDENTITY;
        self.previous = None;
        self.pyramid.clear();
        let count = level_count(inner.width);
        let mut image = frame.clone();
        for index in 0..count {
            let factor = (1usize << index) as f64;
            let mut level = Level { factor, offset: (factor - 1.0) / 2.0, ..Default::default() };
            let (w, h) = (inner.width / factor, inner.height / factor);
            let step = ((w * h / MAX_POINTS).sqrt().ceil() as usize).max(1);
            let (cols, rows) = (((w as usize) / step).max(2), ((h as usize) / step).max(2));
            for row in 0..rows {
                for col in 0..cols {
                    level.u.push((col as f64 + 0.5) * w / cols as f64 - w / 2.0);
                    level.v.push((row as f64 + 0.5) * h / rows as f64 - h / 2.0);
                }
            }
            sample_template(&mut level, &image, &rect);
            self.pyramid.push(level);
            if index + 1 < count {
                image = image.half();
            }
        }
    }

    pub fn reset(&mut self) {
        *self = Tracker::default();
    }

    pub fn track(&mut self, frame: &Gray) -> Option<Fix> {
        let rect = self.rect?;
        if self.pyramid.is_empty() {
            return None;
        }
        let mut images = vec![frame.clone()];
        for _ in 1..self.pyramid.len() {
            let next = images.last().unwrap().half();
            images.push(next);
        }
        let coarsest = self.pyramid.len() - 1;
        let mut starts = vec![self.motion];
        if let Some(prev) = self.previous {
            let mut predicted = self.motion;
            predicted.dx += self.motion.dx - prev.dx;
            predicted.dy += self.motion.dy - prev.dy;
            if predicted != self.motion {
                starts.push(predicted);
            }
        }
        let around = *starts.last().unwrap();
        starts.push(search(&self.pyramid[coarsest], &images[coarsest], &rect, around));
        let mut best: Option<Fix> = None;
        for start in starts {
            if let Some(fix) = align(&self.pyramid[coarsest], &images[coarsest], &rect, start) {
                if best.is_none_or(|b| fix.inliers > b.inliers) {
                    best = Some(fix);
                }
            }
        }
        let Some(mut current) = best else {
            self.reset();
            return None;
        };
        for index in (0..coarsest).rev() {
            match align(&self.pyramid[index], &images[index], &rect, current.motion) {
                Some(fix) => current = fix,
                None => {
                    self.reset();
                    return None;
                }
            }
        }
        if current.inliers < LOST_SHARE || !plausible(&current.motion, &rect) {
            self.reset();
            return None;
        }
        self.previous = Some(self.motion);
        self.motion = current.motion;
        Some(current)
    }
}

fn sample_template(level: &mut Level, image: &Gray, rect: &Rect) {
    let cx = (rect.center_x() - level.offset) / level.factor;
    let cy = (rect.center_y() - level.offset) / level.factor;
    let n = level.u.len();
    level.t = vec![f64::NAN; n];
    level.gx = vec![0.0; n];
    level.gy = vec![0.0; n];
    for i in 0..n {
        let (x, y) = (cx + level.u[i], cy + level.v[i]);
        if let (Some(c), Some(l), Some(r), Some(up), Some(down)) = (image.sample(x, y), image.sample(x - 1.0, y), image.sample(x + 1.0, y), image.sample(x, y - 1.0), image.sample(x, y + 1.0)) {
            level.t[i] = c;
            level.gx[i] = (r - l) / 2.0;
            level.gy[i] = (down - up) / 2.0;
        }
    }
}

fn search(level: &Level, image: &Gray, rect: &Rect, start: Motion) -> Motion {
    let cx = (rect.center_x() - level.offset) / level.factor;
    let cy = (rect.center_y() - level.offset) / level.factor;
    let m = start.scaled(level.factor);
    let n = level.u.len();
    let stride = (n / 400).max(1);
    let mut points = Vec::new();
    let mut i = 0;
    while i < n {
        let (t, u, v) = (level.t[i], level.u[i], level.v[i]);
        let w = 1.0 + m.g * u + m.h * v;
        if !t.is_nan() && w > 0.1 {
            points.push((cx + (m.a * u + m.b * v + m.dx) / w, cy + (m.c * u + m.d * v + m.dy) / w, t));
        }
        i += stride;
    }
    let (mut best_cost, mut bx, mut by) = (f64::INFINITY, 0, 0);
    let mut values = vec![0.0; points.len()];
    for oy in -SEARCH_RADIUS..=SEARCH_RADIUS {
        for ox in -SEARCH_RADIUS..=SEARCH_RADIUS {
            let (mut count, mut shift) = (0.0, 0.0);
            for (k, p) in points.iter().enumerate() {
                match image.sample(p.0 + ox as f64, p.1 + oy as f64) {
                    Some(v) => {
                        values[k] = v;
                        count += 1.0;
                        shift += p.2 - v;
                    }
                    None => values[k] = f64::NAN,
                }
            }
            if count < 16.0 {
                continue;
            }
            shift /= count;
            let mut cost = 0.0;
            for (k, p) in points.iter().enumerate() {
                if !values[k].is_nan() {
                    cost += (values[k] + shift - p.2).abs();
                }
            }
            cost /= count;
            if cost < best_cost {
                best_cost = cost;
                bx = ox;
                by = oy;
            }
        }
    }
    let mut found = start;
    found.dx += bx as f64 * level.factor;
    found.dy += by as f64 * level.factor;
    found
}

fn align(level: &Level, image: &Gray, rect: &Rect, start: Motion) -> Option<Fix> {
    let cx = (rect.center_x() - level.offset) / level.factor;
    let cy = (rect.center_y() - level.offset) / level.factor;
    let mut m = start.scaled(level.factor);
    let n = level.u.len();
    let mut share = 0.0;
    let mut values = vec![f64::NAN; n];
    let mut rejected = vec![false; n];
    let cut = 3.0 * HUBER;
    for _ in 0..ITERATIONS {
        let mut hessian = [0.0f64; 64];
        let mut gradient = [0.0f64; 8];
        let (mut count, mut kept, mut sum_i, mut sum_ii, mut sum_t, mut sum_tt) = (0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
        for i in 0..n {
            values[i] = f64::NAN;
            let t = level.t[i];
            if t.is_nan() {
                continue;
            }
            let (u, v) = (level.u[i], level.v[i]);
            let w = 1.0 + m.g * u + m.h * v;
            if w <= 0.1 {
                continue;
            }
            let Some(value) = image.sample(cx + (m.a * u + m.b * v + m.dx) / w, cy + (m.c * u + m.d * v + m.dy) / w) else { continue };
            values[i] = value;
            count += 1.0;
            if rejected[i] {
                continue;
            }
            kept += 1.0;
            sum_i += value;
            sum_ii += value * value;
            sum_t += t;
            sum_tt += t * t;
        }
        if count < 16.0 || kept < 16.0 {
            return None;
        }
        let (mean_i, mean_t) = (sum_i / kept, sum_t / kept);
        let spread_i = (sum_ii / kept - mean_i * mean_i).max(0.0).sqrt();
        let spread_t = (sum_tt / kept - mean_t * mean_t).max(0.0).sqrt();
        let gain = if spread_i > 1e-6 { (spread_t / spread_i).clamp(0.25, 4.0) } else { 1.0 };
        let mut good = 0.0;
        for i in 0..n {
            let value = values[i];
            if value.is_nan() {
                continue;
            }
            let (t, u, v) = (level.t[i], level.u[i], level.v[i]);
            let r = (value - mean_i) * gain + mean_t - t;
            let size = r.abs();
            rejected[i] = size > cut;
            if size <= HUBER {
                good += 1.0;
            }
            if rejected[i] {
                continue;
            }
            let weight = if size <= HUBER { 1.0 } else { HUBER / size };
            let (gx, gy) = (level.gx[i], level.gy[i]);
            let radial = gx * u + gy * v;
            let s = [gx * u, gx * v, gy * u, gy * v, gx, gy, -radial * u, -radial * v];
            for p in 0..8 {
                let ws = weight * s[p];
                gradient[p] += ws * r;
                for q in p..8 {
                    hessian[p * 8 + q] += ws * s[q];
                }
            }
        }
        share = good / count;
        for p in 0..8 {
            for q in 0..p {
                hessian[p * 8 + q] = hessian[q * 8 + p];
            }
        }
        let step = solve(&hessian, &gradient)?;
        // inverse composition: H ← H · H(Δ)⁻¹
        let delta = [1.0 + step[0], step[1], step[4], step[2], 1.0 + step[3], step[5], step[6], step[7], 1.0];
        let inverse = invert3(&delta)?;
        let current = [m.a, m.b, m.dx, m.c, m.d, m.dy, m.g, m.h, 1.0];
        let mut product = [0.0f64; 9];
        for row in 0..3 {
            for col in 0..3 {
                product[row * 3 + col] = current[row * 3] * inverse[col] + current[row * 3 + 1] * inverse[3 + col] + current[row * 3 + 2] * inverse[6 + col];
            }
        }
        let k = product[8];
        if k.abs() <= 1e-12 {
            return None;
        }
        m = Motion { a: product[0] / k, b: product[1] / k, c: product[3] / k, d: product[4] / k, dx: product[2] / k, dy: product[5] / k, g: product[6] / k, h: product[7] / k };
        if step[4].abs() < 0.005 && step[5].abs() < 0.005 && step[0].abs() < 1e-5 && step[1].abs() < 1e-5 && step[2].abs() < 1e-5 && step[3].abs() < 1e-5 && step[6].abs() < 1e-7 && step[7].abs() < 1e-7 {
            break;
        }
    }
    Some(Fix { motion: m.scaled(1.0 / level.factor), inliers: share })
}

/// A transform a held phone or a card on the screen can have: scale 0.5–2, the two axes' ratio
/// 0.75–1.33, under 15 % perspective at the corners.
pub fn plausible(m: &Motion, rect: &Rect) -> bool {
    let scale = m.scale();
    if !(scale > 0.5 && scale < 2.0) {
        return false;
    }
    let x = (m.a * m.a + m.c * m.c).sqrt();
    let y = (m.b * m.b + m.d * m.d).sqrt();
    if !(y > 0.0 && x / y > 0.75 && x / y < 1.0 / 0.75) {
        return false;
    }
    m.g.abs() * rect.width / 2.0 + m.h.abs() * rect.height / 2.0 < 0.15
}

/// The inverse of a 3×3 matrix (row by row); None if singular.
pub fn invert3(m: &[f64; 9]) -> Option<[f64; 9]> {
    let c00 = m[4] * m[8] - m[5] * m[7];
    let c01 = m[5] * m[6] - m[3] * m[8];
    let c02 = m[3] * m[7] - m[4] * m[6];
    let det = m[0] * c00 + m[1] * c01 + m[2] * c02;
    if det.abs() <= 1e-12 {
        return None;
    }
    Some([
        c00 / det,
        (m[2] * m[7] - m[1] * m[8]) / det,
        (m[1] * m[5] - m[2] * m[4]) / det,
        c01 / det,
        (m[0] * m[8] - m[2] * m[6]) / det,
        (m[2] * m[3] - m[0] * m[5]) / det,
        c02 / det,
        (m[1] * m[6] - m[0] * m[7]) / det,
        (m[0] * m[4] - m[1] * m[3]) / det,
    ])
}

fn solve(matrix: &[f64; 64], vector: &[f64; 8]) -> Option<[f64; 8]> {
    let (mut m, mut x) = (*matrix, *vector);
    let n = 8;
    for col in 0..n {
        let mut pivot = col;
        for row in col + 1..n {
            if m[row * n + col].abs() > m[pivot * n + col].abs() {
                pivot = row;
            }
        }
        if m[pivot * n + col].abs() <= 1e-9 {
            return None;
        }
        if pivot != col {
            for k in 0..n {
                m.swap(col * n + k, pivot * n + k);
            }
            x.swap(col, pivot);
        }
        for row in col + 1..n {
            let f = m[row * n + col] / m[col * n + col];
            if f == 0.0 {
                continue;
            }
            for k in col..n {
                m[row * n + k] -= f * m[col * n + k];
            }
            x[row] -= f * x[col];
        }
    }
    for row in (0..n).rev() {
        let mut sum = x[row];
        for k in row + 1..n {
            sum -= m[row * n + k] * x[k];
        }
        x[row] = sum / m[row * n + row];
    }
    Some(x)
}

/// Steadiness: if no corner moved more than DEADBAND from the shown one, the shown one stays (a phone
/// held still: pixel-still); else the raw one, no delay. A corner moving more than JUMP_SHARE of the quad's
/// height between two readings is a jump (the anchor must be set up again).
#[derive(Default, Clone)]
pub struct Smoother {
    last: Option<Motion>,
    shown: Option<Motion>,
}

impl Smoother {
    pub const JUMP_SHARE: f64 = 0.12;
    pub const DEADBAND: f64 = 0.15;

    pub fn reset(&mut self) {
        *self = Smoother::default();
    }

    /// The transform to show, and whether it jumped.
    pub fn push(&mut self, raw: Motion, rect: &Rect) -> (Motion, bool) {
        let last = self.last.replace(raw);
        if let Some(last) = last {
            if raw.distance(&last, rect) > Self::JUMP_SHARE * rect.height {
                self.shown = Some(raw);
                return (raw, true);
            }
        }
        if let Some(shown) = self.shown {
            if raw.distance(&shown, rect) <= Self::DEADBAND {
                return (shown, false);
            }
        }
        self.shown = Some(raw);
        (raw, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A synthetic frame by the shared rule (see the fixture's `note`).
    fn render(width: usize, height: usize, rect: &Rect, m: &Motion, k: usize, gain: f64, content: Option<&[f64]>) -> Gray {
        let (cx, cy) = (rect.center_x(), rect.center_y());
        let inv = invert3(&[m.a, m.b, m.dx, m.c, m.d, m.dy, m.g, m.h, 1.0]).unwrap();
        let mut px = vec![0u8; width * height];
        for y in 0..height {
            for x in 0..width {
                let mut sum = 0.0;
                for (sx, sy) in [(0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)] {
                    let (fx, fy) = (x as f64 + sx - 0.5, y as f64 + sy - 0.5);
                    let (qx, qy) = (fx - cx, fy - cy);
                    let w = inv[6] * qx + inv[7] * qy + inv[8];
                    let u = (inv[0] * qx + inv[1] * qy + inv[2]) / w;
                    let v = (inv[3] * qx + inv[4] * qy + inv[5]) / w;
                    let value = if u.abs() <= rect.width / 2.0 && v.abs() <= rect.height / 2.0 {
                        match content {
                            Some(c) if u >= c[0] && v >= c[1] && u <= c[2] && v <= c[3] => 120.0 + 60.0 * (0.33 * u + 0.11 * v).sin(),
                            _ => 128.0 + 50.0 * (0.21 * u).sin() * (0.17 * v).cos() + 40.0 * (0.05 * (u + 2.0 * v)).sin(),
                        }
                    } else {
                        60.0 + 30.0 * (0.13 * fx + 0.07 * fy + 0.4 * k as f64).sin()
                    };
                    sum += value;
                }
                px[y * width + x] = (sum / 4.0 * gain).round().clamp(0.0, 255.0) as u8;
            }
        }
        Gray::new(width, height, px)
    }

    fn fixture() -> serde_json::Value {
        serde_json::from_str(include_str!("../tests/fixtures/screen-anchor.json")).unwrap()
    }

    #[test]
    fn the_shared_tracking_vectors() {
        let f = fixture();
        for case in f["cases"].as_array().unwrap() {
            let name = case["name"].as_str().unwrap();
            let r: Vec<f64> = case["rect"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect();
            let rect = Rect { x: r[0], y: r[1], width: r[2], height: r[3] };
            let (w, h) = (case["width"].as_u64().unwrap() as usize, case["height"].as_u64().unwrap() as usize);
            let tol = case["tolerance"].as_f64().unwrap();
            let gains: Option<Vec<f64>> = case["gain"].as_array().map(|a| a.iter().map(|v| v.as_f64().unwrap()).collect());
            let content: Option<Vec<f64>> = case["content"].as_array().map(|a| a.iter().map(|v| v.as_f64().unwrap()).collect());
            let content_from = case["contentFrom"].as_u64().map(|v| v as usize).unwrap_or(usize::MAX);
            let lost_from = case["lostFrom"].as_u64().map(|v| v as usize).unwrap_or(usize::MAX);
            let mut tr = Tracker::new();
            for (k, mv) in case["motions"].as_array().unwrap().iter().enumerate() {
                let truth = Motion::from_slice(&mv.as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect::<Vec<_>>());
                let frame = render(w, h, &rect, &truth, k, gains.as_ref().map(|g| g[k]).unwrap_or(1.0), if k >= content_from { content.as_deref() } else { None });
                if k == 0 {
                    tr.begin(&frame, rect);
                    continue;
                }
                let fix = tr.track(&frame);
                if k >= lost_from {
                    assert!(fix.is_none(), "{name} #{k}: should be lost");
                    break;
                }
                let fix = fix.unwrap_or_else(|| panic!("{name} #{k}: anchor lost"));
                let err = fix.motion.distance(&truth, &rect);
                assert!(err < tol, "{name} #{k}: error {err:.3} ≥ {tol}");
            }
        }
    }

    #[test]
    fn the_shared_smoother_vectors() {
        let f = fixture();
        for v in f["smoother"].as_array().unwrap() {
            let r: Vec<f64> = v["rect"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect();
            let rect = Rect { x: r[0], y: r[1], width: r[2], height: r[3] };
            let mut s = Smoother::default();
            for (k, step) in v["steps"].as_array().unwrap().iter().enumerate() {
                let arr = |key: &str| Motion::from_slice(&step[key].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect::<Vec<_>>());
                let (m, jumped) = s.push(arr("raw"), &rect);
                assert_eq!(m, arr("shown"), "{} #{k}", v["name"]);
                assert_eq!(jumped, step["jumped"].as_bool().unwrap(), "{} #{k}", v["name"]);
            }
        }
    }

    #[test]
    fn a_region_s_grey_is_the_whole_frame_s_there() {
        let (w, h) = (1920usize, 1080usize);
        let px: Vec<u8> = (0..w * h * 4).map(|i| ((i * 7919) % 251) as u8).collect();
        let whole = Gray::from_rgba(&px, w, h, false, TRACK_WIDTH);
        let part = Gray::from_rgba_region(&px, w, h, false, TRACK_WIDTH, (100, 50, 400, 300));
        for y in 50..300 {
            for x in 100..400 {
                assert_eq!(part.pixels[y * part.width + x], whole.pixels[y * whole.width + x], "{x},{y}");
            }
        }
        assert_eq!(part.pixels[0], 0);
    }

    #[test]
    fn gray_from_rgba_box_averages_the_brightness() {
        let mut b = vec![255u8; 4 * 4 * 4];
        b[0] = 0;
        b[1] = 0;
        b[2] = 0;
        let n = b.len();
        b[n - 4] = 80;
        b[n - 3] = 40;
        b[n - 2] = 0;
        let g = Gray::from_rgba(&b, 4, 4, false, 2);
        assert_eq!((g.width, g.height), (2, 2));
        assert_eq!(g.pixels, vec![191, 255, 255, 203]);
        assert_eq!(Gray::from_rgba(&b, 4, 4, true, 2).pixels, vec![191, 255, 255, 200]);
    }
}
