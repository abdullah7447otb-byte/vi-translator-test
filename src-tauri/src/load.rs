//! Performance mode: how hard the app works, so a slower computer keeps up with the picture.
//!
//! "auto" (the default) watches, once a second, whether the computer keeps up: the PS5's frames
//! decoded too late, the reading of the text taking longer than the time between reads, the app's
//! picture falling short of its frame rate. When it doesn't, the app reads the text less often first,
//! then shows its picture at a lower rate, a level at a time; after 15 seconds without trouble it
//! goes back a level. "light" is the lowest level all the time; "full" never lowers anything.
//!
//! Each level is a floor under the times the settings give (they're never made faster here): the
//! least time between frames read for text, between reads, and between frames of the app's picture.

use std::sync::atomic::{AtomicU64, Ordering};

/// (frames for the reading, ms apart; reads, ms apart; frames of the app's picture, ms apart) per level.
/// Level 3 is 12 frames a second read every 66 ms: a line is still read within about 100 ms of
/// appearing (a frame's wait and a read), and the game's subtitles stay up for seconds.
pub const LEVELS: [(u64, u64, u64); 4] = [(0, 0, 0), (33, 33, 0), (50, 50, 22), (83, 66, 33)];
const TOP: usize = LEVELS.len() - 1;

/// The floors in force now (read by the capture manager, the reading and the preview).
pub static CAPTURE_FLOOR_MS: AtomicU64 = AtomicU64::new(0);
pub static PREVIEW_FLOOR_MS: AtomicU64 = AtomicU64::new(0);
/// The level in force now (0: nothing lowered).
pub static LEVEL: AtomicU64 = AtomicU64::new(0);

/// What the computer did in the last second.
#[derive(Clone, Copy, Debug, Default)]
pub struct Second {
    /// PS5 frames dropped because they were decoded too late
    pub late_frames: u64,
    /// the reading's average time, and the time it has between reads (ms)
    pub read_ms: Option<f64>,
    pub read_interval_ms: u64,
    /// frames of the app's picture sent, and how many there should have been (none: nobody watches,
    /// or the source sends only when the picture changes)
    pub preview_sent: Option<(u64, u64)>,
}

impl Second {
    /// The computer didn't keep up.
    pub fn behind(&self) -> bool {
        self.late_frames > 0
            || self.read_ms.is_some_and(|ms| ms > self.read_interval_ms.max(16) as f64 * 1.2)
            || self.preview_sent.is_some_and(|(sent, want)| want >= 10 && (sent as f64) < want as f64 * 0.6)
    }
}

pub struct Load {
    pub level: usize,
    calm: u32,
    since_raise: u32,
}

impl Default for Load {
    fn default() -> Self {
        Load { level: 0, calm: 0, since_raise: u32::MAX }
    }
}

impl Load {
    /// One second more, in `mode` ("auto", "light", "full"): the level to use now.
    pub fn second(&mut self, mode: &str, s: &Second) -> usize {
        match mode {
            "full" => self.level = 0,
            "light" => self.level = TOP,
            _ => {
                self.since_raise = self.since_raise.saturating_add(1);
                if s.behind() {
                    self.calm = 0;
                    // (a level at a time, every 2 s at most: the last change is given time to work)
                    if self.level < TOP && self.since_raise >= 2 {
                        self.level += 1;
                        self.since_raise = 0;
                    }
                } else {
                    self.calm += 1;
                    if self.calm >= 15 && self.level > 0 {
                        self.level -= 1;
                        self.calm = 0;
                    }
                }
            }
        }
        let (capture, read, preview) = LEVELS[self.level];
        CAPTURE_FLOOR_MS.store(capture, Ordering::Relaxed);
        vi_translator_core::perf::READ_FLOOR_MS.store(read, Ordering::Relaxed);
        PREVIEW_FLOOR_MS.store(preview, Ordering::Relaxed);
        LEVEL.store(self.level as u64, Ordering::Relaxed);
        self.level
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slow() -> Second {
        Second { late_frames: 3, ..Default::default() }
    }

    #[test]
    fn auto_steps_down_when_behind_and_back_when_calm() {
        let mut l = Load::default();
        assert_eq!(l.second("auto", &slow()), 1);
        // (not again the next second: the change is given time)
        assert_eq!(l.second("auto", &slow()), 1);
        assert_eq!(l.second("auto", &slow()), 2);
        l.second("auto", &slow());
        l.second("auto", &slow());
        assert_eq!(l.second("auto", &slow()), 3, "no lower than the last level");
        for _ in 0..14 {
            assert_eq!(l.second("auto", &Second::default()), 3);
        }
        assert_eq!(l.second("auto", &Second::default()), 2, "15 calm seconds: a level back");
    }

    #[test]
    fn light_and_full_are_fixed() {
        let mut l = Load::default();
        assert_eq!(l.second("light", &Second::default()), TOP);
        assert_eq!(l.second("full", &slow()), 0);
    }

    #[test]
    fn what_counts_as_behind() {
        assert!(!Second::default().behind());
        // a read of 30 ms every 16 ms can't keep up; 18 ms can, near enough
        assert!(Second { read_ms: Some(30.0), read_interval_ms: 16, ..Default::default() }.behind());
        assert!(!Second { read_ms: Some(18.0), read_interval_ms: 16, ..Default::default() }.behind());
        // 30 of 60 frames of the picture sent is behind; 50 of 60 isn't
        assert!(Second { preview_sent: Some((30, 60)), ..Default::default() }.behind());
        assert!(!Second { preview_sent: Some((50, 60)), ..Default::default() }.behind());
    }
}
