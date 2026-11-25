// Ported from CouchSubs (AGPL-3.0, (C) 2026 Sinan Yorulmaz), commit bc54ff7: couchsubs-android/app/src/main/java/com/couchsubs/mobile/core/PhoneChat.kt
// (and ScreenPhoneCard.kt's Handset: the phone's screen laid flat)
//
//! What each line on GTA VI's phone is: the lines are laid flat on the phone's screen first (a
//! tilted phone too: left and right measured against the screen's own edges), then the screen is
//! worked out. On a chat each bubble is one piece (its lines joined), incoming or outgoing by the
//! edge it leans on. On the chat list the contacts' names stay as they are and the last messages'
//! previews are translated. The home screen (weather, the notification counter, the icon grid) is
//! not a chat: its text is translated in place, the apps' names are not. Rules and thresholds as
//! CouchSubs' own, checked against its fixture (tests/fixtures/phone-chat.json).

/// A box in fractions (left, top, right, bottom).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Box4 {
    pub left: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
}

impl Box4 {
    pub fn new(left: f64, top: f64, right: f64, bottom: f64) -> Self {
        Box4 { left, top, right, bottom }
    }
    fn union(&self, o: &Box4) -> Box4 {
        Box4 { left: self.left.min(o.left), top: self.top.min(o.top), right: self.right.max(o.right), bottom: self.bottom.max(o.bottom) }
    }
}

/// The phone in the frame: its top's and bottom's middle (fractions of the frame) and the frame's
/// width / height.
#[derive(Debug, Clone, Copy)]
pub struct Handset {
    pub top_x: f64,
    pub top_y: f64,
    pub bottom_x: f64,
    pub bottom_y: f64,
    pub aspect: f64,
}

impl Handset {
    /// The flat screen's width / height (the phone reader's screen: 0.52 / 0.915).
    pub const SCREEN_SHAPE: f64 = 0.52 / 0.915;

    /// The screen's height, as a share of the frame's.
    pub fn screen_height(&self) -> f64 {
        (self.bottom_y - self.top_y) * 0.915
    }

    /// Where a point of the flat screen is in the frame.
    pub fn point(&self, u: f64, v: f64) -> (f64, f64) {
        let (dx, dy) = (self.bottom_x - self.top_x, self.bottom_y - self.top_y);
        let t = 0.04 + 0.915 * v;
        let side = (u - 0.5) * 0.52;
        (self.top_x + dx * t + side * dy / self.aspect, self.top_y + dy * t - side * dx * self.aspect * (0.52 + v * 0.30))
    }

    /// Where a point of the frame is on the flat screen (Newton steps: the map is nearly linear,
    /// four are enough).
    pub fn screen(&self, x: f64, y: f64) -> (f64, f64) {
        let (dx, dy) = (self.bottom_x - self.top_x, self.bottom_y - self.top_y);
        let mut u = 0.5;
        let mut v = ((y - self.top_y) / dy - 0.04) / 0.915;
        for _ in 0..4 {
            let (px, py) = self.point(u, v);
            let (ex, ey) = (px - x, py - y);
            let a = 0.52 * dy / self.aspect;
            let b = 0.915 * dx;
            let c = -0.52 * dx * self.aspect * (0.52 + 0.30 * v);
            let d = 0.915 * dy - (u - 0.5) * 0.52 * dx * self.aspect * 0.30;
            let det = a * d - b * c;
            if det.abs() <= 1e-12 {
                return (u, v);
            }
            u -= (d * ex - b * ey) / det;
            v -= (a * ey - c * ex) / det;
        }
        (u, v)
    }

    /// A line's box in the frame, on the flat screen. On a tilted screen the box read takes in the
    /// line's slant too: the line's own height is the box's less the slant's share.
    pub fn flat(&self, b: &Box4) -> Box4 {
        let (cx, cy) = ((b.left + b.right) / 2.0, (b.top + b.bottom) / 2.0);
        let (_, cv) = self.screen(cx, cy);
        let (dx, dy) = (self.bottom_x - self.top_x, self.bottom_y - self.top_y);
        let slope = (-0.52 * dx * self.aspect * (0.52 + 0.30 * cv)) / (0.52 * dy / self.aspect);
        let left = self.screen(b.left, cy + slope * (b.left - cx)).0;
        let right = self.screen(b.right, cy + slope * (b.right - cx)).0;
        let height = b.bottom - b.top;
        let upright = (height - slope.abs() * (b.right - b.left)).max(height * 0.35);
        let half = upright / self.screen_height() / 2.0;
        Box4::new(left, cv - half, right, cv + half)
    }
}

/// A line is the phone's even with its middle this far outside the flat screen (boxes overhang a little).
pub const INSIDE_SLACK: f64 = 0.05;
/// A line reaching this far outside the flat screen isn't the phone's (a subtitle behind it, a sign).
pub const OUTSIDE_REACH: f64 = 0.3;
/// A line this far past the screen's top or bottom edge is cut (the chat scrolled: half of it shows).
pub const EDGE_CUT: f64 = 0.015;
/// A cut line: lower than this share of the median line (the first under the header, the last at the bottom).
pub const CUT_HEIGHT: f64 = 0.6;
/// Text in a photo: this many times the median line's height (with three lines at least).
pub const PHOTO_HEIGHT: f64 = 2.0;
/// Two lines of one bubble are closer than this share of the median line; bubbles further apart.
pub const BUBBLE_GAP: f64 = 0.6;
/// One bubble's lines line up on the left this closely (of the screen's width); on the right
/// [`RIGHT_ALIGN`] in a bubble on the right.
pub const LEFT_ALIGN: f64 = 0.04;
pub const RIGHT_ALIGN: f64 = 0.03;
/// A bubble leans on a side when its margin there is under this share of the other side's.
pub const SIDE_RATIO: f64 = 0.6;
/// A bubble narrower on both sides than this (a long message filling the screen) leans on neither:
/// its colour tells its side.
pub const WIDE_MARGIN: f64 = 0.15;
/// A short line in the middle (the time, "Today", "Delivered"): its middle at most this far from
/// the screen's, at least [`NOTE_MARGIN`] on each side.
pub const NOTE_CENTER: f64 = 0.06;
pub const NOTE_MARGIN: f64 = 0.2;
/// The header (the contact's name): in the screen's top this share, in the middle within [`HEADER_CENTER`].
pub const HEADER_BAND: f64 = 0.2;
pub const HEADER_CENTER: f64 = 0.12;
/// Pieces read side by side on a line (an emoji splits it) join when the gap is under this many line heights.
pub const ROW_JOIN: f64 = 2.0;
/// On the home screen side-by-side pieces join only this close (icon labels are further apart).
pub const HOME_JOIN: f64 = 1.0;
/// On the home screen two lines of one block (a widget's title and text, a notification's lines)
/// are closer than this share of the median line.
pub const HOME_GAP: f64 = 0.8;
/// The icon grid: at least this many app names in one row.
pub const GRID_LABELS: usize = 3;

/// What a piece of the phone's screen is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// the chat's title (the contact's name): not translated
    Header,
    /// a message on the left (incoming), on the right (outgoing), or either (its colour tells)
    Incoming,
    Outgoing,
    Message,
    /// a short line in the middle: the time, "Today", "Delivered"
    Note,
    /// the chat list: a contact's name (not translated), its last message, a section's name ("Messages")
    Contact,
    Preview,
    Heading,
    /// the home screen's text (a widget, a notification): translated in place
    Home,
}

impl Role {
    pub fn wire(&self) -> &'static str {
        match self {
            Role::Header => "header",
            Role::Incoming => "incoming",
            Role::Outgoing => "outgoing",
            Role::Message => "message",
            Role::Note => "note",
            Role::Contact => "contact",
            Role::Preview => "preview",
            Role::Heading => "heading",
            Role::Home => "home",
        }
    }
    /// Shown as it is, not translated (a name).
    pub fn keeps_text(&self) -> bool {
        matches!(self, Role::Header | Role::Contact)
    }
}

/// One line read on the phone: its text and its box in the frame.
#[derive(Debug, Clone)]
pub struct Line {
    pub text: String,
    pub frame_box: Box4,
}

/// A piece of the phone's screen: what it is, its text (its lines joined), the lines it's made of
/// (indexes into what was given) and its place on the flat screen.
#[derive(Debug, Clone)]
pub struct Group {
    pub role: Role,
    pub text: String,
    pub lines: Vec<usize>,
    pub spot: Box4,
}

#[derive(Debug, Clone)]
struct Row {
    lines: Vec<usize>,
    spot: Box4,
    text: String,
}

impl Row {
    fn height(&self) -> f64 {
        self.spot.bottom - self.spot.top
    }
    fn center(&self) -> f64 {
        (self.spot.left + self.spot.right) / 2.0
    }
    fn words(&self) -> usize {
        self.text.split_whitespace().count()
    }
}

/// On the phone (its middle on the flat screen, not reaching far past it).
pub fn on_screen(spot: &Box4) -> bool {
    let u = (spot.left + spot.right) / 2.0;
    let v = (spot.top + spot.bottom) / 2.0;
    u >= -INSIDE_SLACK && u <= 1.0 + INSIDE_SLACK && v >= -INSIDE_SLACK && v <= 1.0 + INSIDE_SLACK && spot.left >= -OUTSIDE_REACH && spot.right <= 1.0 + OUTSIDE_REACH
}

fn letters(s: &str) -> usize {
    s.chars().filter(|c| c.is_alphabetic()).count()
}

fn is_weather(s: &str) -> bool {
    // "76°", "24 °C", "-3º"
    let t = s.trim();
    let t = t.strip_prefix(['-', '−']).unwrap_or(t);
    let digits = t.chars().take_while(|c| c.is_ascii_digit()).count();
    if !(1..=3).contains(&digits) {
        return false;
    }
    let rest = t[digits..].trim_start();
    let mut cs = rest.chars();
    if !matches!(cs.next(), Some('°' | 'º' | '˚')) {
        return false;
    }
    let tail: String = cs.collect::<String>().trim_start().to_string();
    tail.is_empty() || matches!(tail.as_str(), "C" | "F" | "c" | "f")
}

fn is_counter(s: &str) -> bool {
    // "10 Notifications", "3 new messages"
    let lower = s.to_lowercase();
    let mut w = lower.split_whitespace();
    let Some(n) = w.next() else { return false };
    if !(1..=3).contains(&n.len()) || !n.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    let mut rest: Vec<&str> = w.collect();
    if rest.first() == Some(&"new") {
        rest.remove(0);
    }
    let what = rest.join(" ");
    let one = |stem: &str| what == stem || what == format!("{stem}s");
    one("notification") || one("reminder") || one("alert") || one("message") || one("missed call") || one("update") || one("email") || one("e-mail")
}

/// Crumbs at a line's start and end with no letter or digit (an emoji or icon misread: "©", "•", "♡") are dropped.
pub fn clean(text: &str) -> String {
    let mut words: Vec<&str> = text.split_whitespace().collect();
    let junk = |w: &str| !w.chars().any(|c| c.is_alphanumeric()) && !w.chars().all(|c| ".,!?…'\"-:;()".contains(c));
    while words.first().is_some_and(|w| junk(w)) {
        words.remove(0);
    }
    while words.last().is_some_and(|w| junk(w)) {
        words.pop();
    }
    words.join(" ")
}

fn same_band(a: &Row, b: &Row) -> bool {
    a.spot.bottom.min(b.spot.bottom) - a.spot.top.max(b.spot.top) >= a.height().min(b.height()) * 0.5
}

fn same_row(a: &Row, b: &Row, join: f64) -> bool {
    if !same_band(a, b) {
        return false;
    }
    let gap = a.spot.left.max(b.spot.left) - a.spot.right.min(b.spot.right);
    gap * Handset::SCREEN_SHAPE <= a.height().max(b.height()) * join
}

fn joined_rows(rows: &[Row], join: f64) -> Vec<Row> {
    let mut out: Vec<Row> = Vec::new();
    for row in rows {
        match out.iter().rposition(|r| same_row(r, row, join)) {
            Some(i) => {
                let (a, b) = if out[i].spot.left <= row.spot.left { (out[i].clone(), row.clone()) } else { (row.clone(), out[i].clone()) };
                out[i] = Row { lines: [a.lines, b.lines].concat(), spot: a.spot.union(&b.spot), text: format!("{} {}", a.text, b.text) };
            }
            None => out.push(row.clone()),
        }
    }
    out.sort_by(|a, b| a.spot.top.total_cmp(&b.spot.top));
    out
}

pub fn ends_sentence(text: &str) -> bool {
    text.trim().chars().last().is_some_and(|c| ".!?…".contains(c))
}

/// Like a name: one to three words, each starting with a capital or a digit, no comma, no sentence end.
pub fn looks_like_name(text: &str) -> bool {
    let words: Vec<&str> = text.split_whitespace().collect();
    if !(1..=3).contains(&words.len()) || text.contains(',') || ends_sentence(text) {
        return false;
    }
    words.iter().all(|w| w.chars().next().is_some_and(|c| c.is_uppercase() || c.is_ascii_digit()))
}

/// Like an app's name: one or two words, all letters, each starting with a capital ("WhatUp", "Camera").
pub fn is_app_label(text: &str) -> bool {
    let words: Vec<&str> = text.split_whitespace().collect();
    (1..=2).contains(&words.len())
        && words.iter().all(|w| w.chars().next().is_some_and(|c| c.is_uppercase()) && w.chars().all(|c| c.is_alphabetic() || c == '-' || c == '\'' || c == '&'))
}

fn is_home(rows: &[Row], raw: &[&str]) -> bool {
    if raw.iter().any(|t| is_weather(t.trim_matches([' ', '\t']))) {
        return true;
    }
    if rows.iter().any(|r| is_counter(&r.text)) {
        return true;
    }
    let labels: Vec<&Row> = rows.iter().filter(|r| is_app_label(&r.text) && (r.spot.top + r.spot.bottom) / 2.0 >= HEADER_BAND).collect();
    labels.iter().any(|l| labels.iter().filter(|o| same_band(o, l)).count() >= GRID_LABELS)
}

fn home_blocks(rows: &[Row], median: f64) -> Vec<Vec<Row>> {
    let mut blocks: Vec<Vec<Row>> = Vec::new();
    for row in rows {
        let joins = blocks.last().is_some_and(|block| {
            let last = &block[block.len() - 1];
            row.spot.top - last.spot.bottom < median * HOME_GAP && row.spot.top >= last.spot.top && (row.spot.left - block[0].spot.left).abs() <= LEFT_ALIGN && !same_band(row, last)
        });
        match blocks.last_mut() {
            Some(block) if joins => block.push(row.clone()),
            _ => blocks.push(vec![row.clone()]),
        }
    }
    blocks
}

fn is_app_name(block: &[Row]) -> bool {
    block.len() == 1 && is_app_label(&block[0].text)
}

fn is_header(row: &Row) -> bool {
    let v = (row.spot.top + row.spot.bottom) / 2.0;
    if v >= HEADER_BAND || !(1..=4).contains(&row.words()) || ends_sentence(&row.text) {
        return false;
    }
    (row.center() - 0.5).abs() <= HEADER_CENTER || looks_like_name(&row.text)
}

fn contact_list(rows: &[Row], median: f64) -> Option<Vec<Role>> {
    let mut roles = vec![Role::Heading; rows.len()];
    let (mut pairs, mut i) = (0usize, 0usize);
    while i + 1 < rows.len() {
        let (name, preview) = (&rows[i], &rows[i + 1]);
        if looks_like_name(&name.text)
            && preview.words() >= 2
            && (name.spot.left - preview.spot.left).abs() <= LEFT_ALIGN
            && preview.spot.top - name.spot.bottom <= median * 0.7
            && name.spot.left < 0.5
        {
            roles[i] = Role::Contact;
            roles[i + 1] = Role::Preview;
            pairs += 1;
            i += 2;
        } else {
            i += 1;
        }
    }
    if pairs < 2 || pairs * 2 * 10 < rows.len() * 6 {
        return None;
    }
    // (a name-like line in no pair is a section's name, "Messages"; a longer one a preview alone)
    for (r, role) in rows.iter().zip(roles.iter_mut()) {
        if *role == Role::Heading && !looks_like_name(&r.text) {
            *role = Role::Preview;
        }
    }
    Some(roles)
}

fn continues(row: &Row, last: &Row, first: &Row, median: f64) -> bool {
    if row.spot.top - last.spot.bottom >= median * BUBBLE_GAP {
        return false;
    }
    if (row.spot.left - first.spot.left).abs() <= LEFT_ALIGN {
        return true;
    }
    row.spot.left.min(first.spot.left) > 0.2 && (row.spot.right - last.spot.right).abs() <= RIGHT_ALIGN
}

fn role(bubble: &[Row]) -> Role {
    let spot = bubble[1..].iter().fold(bubble[0].spot, |t, r| t.union(&r.spot));
    let left = spot.left.max(0.0);
    let right = (1.0 - spot.right).max(0.0);
    if bubble.len() == 1 && ((spot.left + spot.right) / 2.0 - 0.5).abs() <= NOTE_CENTER && left >= NOTE_MARGIN && right >= NOTE_MARGIN {
        return Role::Note;
    }
    if left < WIDE_MARGIN && right < WIDE_MARGIN {
        return Role::Message;
    }
    if right < left * SIDE_RATIO {
        return Role::Outgoing;
    }
    if left < right * SIDE_RATIO {
        return Role::Incoming;
    }
    Role::Message
}

fn group(rows: &[Row], role: Role) -> Group {
    let mut text = String::new();
    for r in rows {
        if text.is_empty() {
            text = r.text.clone();
        } else if text.ends_with('-') {
            text.push_str(&r.text);
        } else {
            text.push(' ');
            text.push_str(&r.text);
        }
    }
    let spot = rows[1..].iter().fold(rows[0].spot, |t, r| t.union(&r.spot));
    Group { role, text, lines: rows.iter().flat_map(|r| r.lines.clone()).collect(), spot }
}

fn home_groups(rows: &[Row], median: f64) -> Vec<Group> {
    let mut out = Vec::new();
    for block in home_blocks(rows, median) {
        if is_app_name(&block) {
            continue;
        }
        let mut piece: Vec<Row> = Vec::new();
        for row in block {
            let goes_on = row.text.chars().find(|c| c.is_alphabetic()).is_some_and(|c| c.is_lowercase()) && piece.last().is_some_and(|p| !ends_sentence(&p.text));
            if !piece.is_empty() && !goes_on {
                out.push(group(&piece, Role::Home));
                piece.clear();
            }
            piece.push(row);
        }
        if !piece.is_empty() {
            out.push(group(&piece, Role::Home));
        }
    }
    out
}

/// The phone's lines in pieces, each with what it is. `lines`: the lines read in the frame
/// (only those on the phone's screen are taken: see [`on_screen`]).
pub fn groups(lines: &[Line], handset: &Handset) -> Vec<Group> {
    let on: Vec<(usize, Box4)> = lines.iter().enumerate().map(|(i, l)| (i, handset.flat(&l.frame_box))).filter(|(_, s)| on_screen(s)).collect();
    // (every line on the phone, letters or not: a "76°" line tells the home screen)
    let raw: Vec<&str> = on.iter().map(|(i, _)| lines[*i].text.as_str()).collect();
    let mut single: Vec<Row> = on
        .iter()
        .map(|(i, s)| Row { lines: vec![*i], spot: *s, text: clean(&lines[*i].text) })
        .filter(|r| letters(&r.text) >= 2 && !is_clock(&r.text))
        .collect();
    single.sort_by(|a, b| a.spot.top.total_cmp(&b.spot.top));
    let mut rows = joined_rows(&single, ROW_JOIN);
    if rows.is_empty() {
        return Vec::new();
    }
    let mut hs: Vec<f64> = rows.iter().map(Row::height).collect();
    hs.sort_by(f64::total_cmp);
    let median = hs[hs.len() / 2];
    rows.retain(|r| r.spot.top >= -EDGE_CUT && r.spot.bottom <= 1.0 + EDGE_CUT);
    let home: Vec<Row> = joined_rows(&single, HOME_JOIN).into_iter().filter(|r| r.spot.top >= -EDGE_CUT && r.spot.bottom <= 1.0 + EDGE_CUT).collect();
    if is_home(&home, &raw) {
        return home_groups(&home, median);
    }
    let header = if rows.first().is_some_and(is_header) { Some(rows.remove(0)) } else { None };
    if rows.len() >= 3 {
        rows.retain(|r| r.height() <= median * PHOTO_HEIGHT);
    }
    // (the half line at the top and the bottom of a scrolled chat isn't read)
    if rows.first().is_some_and(|r| r.height() < median * CUT_HEIGHT) {
        rows.remove(0);
    }
    if rows.last().is_some_and(|r| r.height() < median * CUT_HEIGHT) {
        rows.pop();
    }
    let mut out = Vec::new();
    if let Some(list) = contact_list(&rows, median) {
        // (a heading not in the middle at the list's top is a section's name, "Messages": translated)
        if let Some(h) = &header {
            let role = if (h.center() - 0.5).abs() <= HEADER_CENTER { Role::Header } else { Role::Heading };
            out.push(group(std::slice::from_ref(h), role));
        }
        for (r, role) in rows.iter().zip(list) {
            out.push(group(std::slice::from_ref(r), role));
        }
        return out;
    }
    if let Some(h) = &header {
        out.push(group(std::slice::from_ref(h), Role::Header));
    }
    let mut bubble: Vec<Row> = Vec::new();
    for row in rows {
        if !bubble.is_empty() && !continues(&row, &bubble[bubble.len() - 1], &bubble[0], median) {
            out.push(group(&bubble, role(&bubble)));
            bubble.clear();
        }
        bubble.push(row);
    }
    if !bubble.is_empty() {
        out.push(group(&bubble, role(&bubble)));
    }
    out
}

/// A clock ("12:00", "9:41 PM"): not translated.
fn is_clock(text: &str) -> bool {
    let t = text.trim().to_lowercase();
    let t = t.trim_end_matches("am").trim_end_matches("pm").trim_end_matches("a.m.").trim_end_matches("p.m.").trim();
    let mut parts = t.split(':');
    let (Some(h), Some(m), None) = (parts.next(), parts.next(), parts.next()) else { return false };
    (1..=2).contains(&h.len()) && h.chars().all(|c| c.is_ascii_digit()) && m.len() == 2 && m.chars().all(|c| c.is_ascii_digit())
}
