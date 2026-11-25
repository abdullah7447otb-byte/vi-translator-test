//! Playing on the PS5 from this computer: its picture decoded (OpenH264, on the CPU, into planes
//! whose layout is known), its sound on the speakers and the controllers on this computer sent to
//! it (both through SDL, as chiaki-ng does). The picture goes to the app through [`Screen`].

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, SyncSender, TrySendError, channel, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::{Error, Event, Handler, Pad, Session, Video, buttons};

/// One decoded picture, 4:2:0: a full-size luma plane and quarter-size chroma planes, each with
/// its own row length. Studio range (16–235), BT.709, as the PS5 sends it.
pub struct I420 {
    pub w: u32,
    pub h: u32,
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
    /// bytes per row of y, and of u and v
    pub y_stride: usize,
    pub uv_stride: usize,
}

/// Where a stream's picture and news go (on chiaki's threads).
pub trait Screen: Send + Sync {
    /// Would a picture be taken now? (False: the frame is still decoded, as H.264 needs every one,
    /// but not copied out of the decoder.)
    fn wants_frame(&self) -> bool {
        true
    }
    fn frame(&self, f: I420);
    fn event(&self, e: &Event);
    fn log(&self, _level: i32, _text: &str) {}
}

/// The sound on this computer's speakers, changed while it plays.
pub struct Sound {
    pub on: AtomicBool,
    /// 0–100
    pub volume: AtomicU32,
}

impl Sound {
    pub fn new(on: bool, volume: u32) -> Self {
        Sound { on: AtomicBool::new(on), volume: AtomicU32::new(volume.min(100)) }
    }
    /// The factor samples are multiplied by: the volume on a curve the ear hears as even steps.
    pub fn gain(&self) -> f32 {
        if !self.on.load(Ordering::Relaxed) || READ_ONLY.load(Ordering::Relaxed) {
            return 0.0;
        }
        let v = self.volume.load(Ordering::Relaxed).min(100) as f32 / 100.0;
        v * v
    }
}

/// Samples at `gain` (1.0: as they are).
pub fn scale(pcm: &mut [i16], gain: f32) {
    if (gain - 1.0).abs() < f32::EPSILON {
        return;
    }
    for s in pcm {
        *s = (*s as f32 * gain).round().clamp(i16::MIN as f32, i16::MAX as f32) as i16;
    }
}

/// A controller pressed by the app itself (dev builds' debug server: driving the game to test its overlays), and
/// until when: added to the real controllers' state while it lasts.
static VIRTUAL: Mutex<Option<(Pad, Instant)>> = Mutex::new(None);

/// Read only: the stream is only read (the PS5 plays on its own screen with its own controller),
/// so nothing from this computer's controllers or the app's presses goes to the PS5, and its
/// sound isn't played here.
pub static READ_ONLY: AtomicBool = AtomicBool::new(false);

/// Presses `p` on the PS5 for `hold`, with whatever real controllers do meanwhile.
pub fn press(p: Pad, hold: Duration) {
    *VIRTUAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some((p, Instant::now() + hold));
}

/// What the player has done so far (for the app's status line).
#[derive(Default)]
pub struct Stats {
    /// pictures decoded
    pub frames: AtomicU64,
    /// frames that came in but couldn't be decoded
    pub bad_frames: AtomicU64,
    /// frames lost on the network (chiaki's count: never arrived whole)
    pub net_lost: AtomicU64,
    /// frames dropped because this computer decoded slower than they came
    pub late: AtomicU64,
    pub controllers: AtomicU64,
    /// encoded frames waiting for the decoder thread right now
    pub queued: std::sync::atomic::AtomicUsize,
    /// frames dropped because too many waited too long (the picture jumps to the next key frame)
    pub backlog_dropped: AtomicU64,
    /// key frames asked of the PS5 after such a drop
    pub idr_requests: AtomicU64,
    /// what decodes the picture (VideoToolbox, Media Foundation or OpenH264)
    pub decoder: Mutex<String>,
}

/// A stream from the PS5 with its sound and controllers, while it lives.
pub struct Player {
    session: Arc<Mutex<Option<Session>>>,
    stop: Arc<AtomicBool>,
    sdl: Option<std::thread::JoinHandle<()>>,
    decoder: Option<std::thread::JoinHandle<()>>,
    pub stats: Arc<Stats>,
}

/// Encoded frames waiting for the decoder thread: about half a second at 60 frames a second. More
/// than that waiting means this computer decodes slower than the PS5 sends, and the picture would
/// fall further and further behind: what waits is dropped and the PS5 asked for a fresh key frame.
const DECODE_QUEUE: usize = 30;

/// What the stream's threads tell the SDL thread.
enum ToSdl {
    AudioFormat { channels: u32, rate: u32 },
    Pcm(Vec<i16>),
    Rumble { left: u8, right: u8 },
}

struct Inner {
    screen: Arc<dyn Screen>,
    /// encoded frames to the decoder thread (decoding on chiaki's own thread held up its network
    /// reading on slower computers: packets were lost and the picture broke up and stuttered)
    to_decoder: Mutex<SyncSender<(Vec<u8>, i32)>>,
    /// a frame couldn't be queued: the decoder thread starts again from a key frame
    resync: Arc<AtomicBool>,
    opus: Mutex<Option<(opus::Decoder, u32)>>,
    to_sdl: Mutex<Sender<ToSdl>>,
    stats: Arc<Stats>,
}

impl Handler for Inner {
    fn event(&self, e: Event) {
        if let Event::Rumble { left, right } = e {
            let _ = self.to_sdl.lock().unwrap_or_else(std::sync::PoisonError::into_inner).send(ToSdl::Rumble { left, right });
        }
        self.screen.event(&e);
    }

    fn video(&self, frame: &[u8], frames_lost: i32, frame_index: i32) -> bool {
        if frames_lost > 0 {
            self.stats.net_lost.fetch_add(frames_lost as u64, Ordering::Relaxed);
        }
        match self.to_decoder.lock().unwrap_or_else(std::sync::PoisonError::into_inner).try_send((frame.to_vec(), frame_index)) {
            Ok(()) => {
                self.stats.queued.fetch_add(1, Ordering::Relaxed);
            }
            Err(TrySendError::Full(_)) => {
                self.stats.late.fetch_add(1, Ordering::Relaxed);
                self.resync.store(true, Ordering::Relaxed);
            }
            // (the player is stopping)
            Err(TrySendError::Disconnected(_)) => {}
        }
        true
    }

    fn audio_header(&self, channels: u32, rate: u32) {
        let ch = if channels >= 2 { opus::Channels::Stereo } else { opus::Channels::Mono };
        match opus::Decoder::new(rate, ch) {
            Ok(d) => *self.opus.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some((d, channels.clamp(1, 2))),
            Err(e) => self.screen.log(1, &format!("sound: {e}")),
        }
        let _ = self.to_sdl.lock().unwrap_or_else(std::sync::PoisonError::into_inner).send(ToSdl::AudioFormat { channels: channels.clamp(1, 2), rate });
    }

    fn audio(&self, packet: &[u8]) {
        let mut o = self.opus.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some((dec, channels)) = o.as_mut() else { return };
        // (Opus packets are at most 120 ms: 5760 samples at 48 kHz)
        let mut pcm = vec![0i16; 5760 * *channels as usize];
        match dec.decode(packet, &mut pcm, false) {
            Ok(n) => {
                pcm.truncate(n * *channels as usize);
                let _ = self.to_sdl.lock().unwrap_or_else(std::sync::PoisonError::into_inner).send(ToSdl::Pcm(pcm));
            }
            Err(e) => self.screen.log(2, &format!("sound: {e}")),
        }
    }

    fn log(&self, level: i32, text: &str) {
        self.screen.log(level, text);
    }
}

impl Player {
    /// Connects to the registered PS5 at `host` and plays: picture to `screen`, sound to the
    /// speakers (as `sound` says, while it plays), controllers to the PS5.
    pub fn start(host: &str, regist_key: &[u8; 16], rp_key: &[u8; 16], video: Video, decode: crate::hwdec::Decode, sound: Arc<Sound>, screen: Arc<dyn Screen>) -> Result<Self, Error> {
        // (made here so a failure is told before connecting; the system decoder is made on the
        // decoder thread itself)
        drop(crate::hwdec::VideoDecoder::software().map_err(Error)?);
        let (tx, rx) = channel();
        let (to_dec, from_net) = sync_channel(DECODE_QUEUE);
        let resync = Arc::new(AtomicBool::new(false));
        let stats = Arc::new(Stats::default());
        let inner = Arc::new(Inner {
            screen: screen.clone(),
            to_decoder: Mutex::new(to_dec),
            resync: resync.clone(),
            opus: Mutex::new(None),
            to_sdl: Mutex::new(tx),
            stats: stats.clone(),
        });
        let session = Arc::new(Mutex::new(Some(Session::start(host, regist_key, rp_key, video, inner)?)));
        let stop = Arc::new(AtomicBool::new(false));
        let decoder = {
            let (session, stop, screen, stats) = (session.clone(), stop.clone(), screen.clone(), stats.clone());
            std::thread::Builder::new()
                .name("ps5-video".into())
                .spawn(move || decode_thread(from_net, decode, resync, session, stop, screen, stats))
                .ok()
        };
        let sdl = {
            let (session, stop, screen, stats) = (session.clone(), stop.clone(), screen.clone(), stats.clone());
            std::thread::Builder::new()
                .name("ps5-sdl".into())
                .spawn(move || sdl_thread(rx, session, stop, sound, screen, stats))
                .ok()
        };
        Ok(Player { session, stop, sdl, decoder, stats })
    }

    /// The account's login passcode, when the PS5 asks for it.
    pub fn login_pin(&self, pin: &str) {
        if let Some(s) = self.session.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_ref() {
            s.login_pin(pin);
        }
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.sdl.take() {
            let _ = t.join();
        }
        // (ends the stream: chiaki's threads finish before the decoder goes)
        let s = self.session.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
        drop(s);
        if let Some(t) = self.decoder.take() {
            let _ = t.join();
        }
    }
}

/// The picture decoded on its own thread, frame by frame in the order they came.
fn decode_thread(
    rx: Receiver<(Vec<u8>, i32)>,
    decode: crate::hwdec::Decode,
    resync: Arc<AtomicBool>,
    session: Arc<Mutex<Option<Session>>>,
    stop: Arc<AtomicBool>,
    screen: Arc<dyn Screen>,
    stats: Arc<Stats>,
) {
    let mut dec = match crate::hwdec::VideoDecoder::new(decode) {
        Ok(d) => d,
        Err(e) => return screen.log(1, &e),
    };
    screen.log(3, &format!("[video] decoder: {}", dec.name()));
    let named = |d: &crate::hwdec::VideoDecoder| *stats.decoder.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = d.name().to_string();
    named(&dec);
    // (asked at most twice a second: a burst of bad frames is one loss, not twenty)
    let mut last_ask = Instant::now() - Duration::from_secs(1);
    let mut key_frame = |why: &str| {
        if last_ask.elapsed() >= Duration::from_millis(500) {
            last_ask = Instant::now();
            screen.log(3, &format!("video: {why}, asking for a key frame"));
            if let Some(s) = session.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_ref() {
                s.request_key_frame();
            }
        }
    };
    let mut backlog = Backlog::new(Instant::now());
    let mut waiting_key: Option<Instant> = None;
    while !stop.load(Ordering::Relaxed) {
        let got = rx.recv_timeout(Duration::from_millis(100));
        let now = Instant::now();
        if let Some(line) = backlog.log_due(now) {
            screen.log(3, &line);
        }
        let (mut frame, mut index) = match got {
            Ok(f) => f,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        dequeued(&stats);
        if resync.swap(false, Ordering::Relaxed) {
            // (frames were dropped: what waits is decoded from a picture that isn't there)
            while rx.try_recv().is_ok() {
                dequeued(&stats);
            }
            key_frame("decoding fell behind");
            waiting_key = Some(now);
            continue;
        }
        // (frames waited too long: the newest key frame waiting is decoded and what's before it
        // dropped; with none waiting, the PS5 is asked for one and frames skipped until it comes)
        if backlog.note(stats.queued.load(Ordering::Relaxed) + 1, now) {
            let mut dropped = 0u64;
            let mut key: Option<(Vec<u8>, i32)> = is_key_frame(&frame).then(|| (std::mem::take(&mut frame), index));
            while let Ok(f) = rx.try_recv() {
                dequeued(&stats);
                if is_key_frame(&f.0) {
                    dropped += key.is_some() as u64;
                    key = Some(f);
                } else {
                    dropped += 1;
                }
            }
            match key {
                Some((f, i)) => {
                    frame = f;
                    index = i;
                    waiting_key = None;
                }
                None => {
                    dropped += 1;
                    stats.idr_requests.fetch_add(1, Ordering::Relaxed);
                    if let Some(s) = session.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_ref() {
                        s.request_key_frame();
                    }
                    waiting_key = Some(now);
                }
            }
            stats.backlog_dropped.fetch_add(dropped, Ordering::Relaxed);
            backlog.dropped(dropped);
            if waiting_key.is_some() {
                continue;
            }
        }
        if let Some(since) = waiting_key {
            // (a frame after the missing ones only breaks the picture: wait for the key frame, but
            // not for ever if the PS5 doesn't send one)
            if !is_key_frame(&frame) && since.elapsed() < Duration::from_secs(2) {
                stats.backlog_dropped.fetch_add(1, Ordering::Relaxed);
                continue;
            }
            waiting_key = None;
        }
        match dec.decode(&frame) {
            Ok(Some(f)) => {
                stats.frames.fetch_add(1, Ordering::Relaxed);
                if screen.wants_frame() {
                    screen.frame(f);
                }
            }
            // (no picture yet: the decoder waits for more)
            Ok(None) => {}
            Err(e) if dec.is_hardware() && e.ends_with("fall back") => {
                stats.bad_frames.fetch_add(1, Ordering::Relaxed);
                screen.log(2, &format!("[video] {}: {e}: OpenH264 from now on", dec.name()));
                if let Err(e) = dec.fall_back() {
                    return screen.log(1, &e);
                }
                named(&dec);
                key_frame("the decoder changed");
            }
            Err(e) => {
                stats.bad_frames.fetch_add(1, Ordering::Relaxed);
                screen.log(2, &format!("video: {e}"));
                // (the PS5 told which frame, as chiaki does when its own decoder fails; and a key frame
                // asked for if that isn't known)
                if index >= 0 {
                    if let Some(s) = session.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_ref() {
                        s.report_corrupt(index, index);
                    }
                } else {
                    key_frame("a frame could not be decoded");
                }
            }
        }
    }
}

fn dequeued(stats: &Stats) {
    let _ = stats.queued.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| Some(n.saturating_sub(1)));
}

/// Whether an H.264 frame (Annex B) starts a new picture on its own: an IDR slice, or the
/// sequence parameters the PS5 sends just before one.
pub(crate) fn is_key_frame(frame: &[u8]) -> bool {
    let mut i = 0;
    while i + 3 < frame.len() {
        if frame[i] == 0 && frame[i + 1] == 0 && frame[i + 2] == 1 {
            let t = frame[i + 3] & 0x1f;
            if t == 5 || t == 7 {
                return true;
            }
            i += 3;
        } else {
            i += 1;
        }
    }
    false
}

/// How many frames wait for the decoder: more than [`Backlog::DEPTH`] for
/// [`Backlog::FOR`] means this computer fell behind, and the waiting frames are dropped rather
/// than shown late. Also what's logged every ten seconds.
pub(crate) struct Backlog {
    over_since: Option<Instant>,
    max: usize,
    sum: u64,
    samples: u64,
    dropped: u64,
    drops: u64,
    last_log: Instant,
}

impl Backlog {
    pub(crate) const DEPTH: usize = 6;
    pub(crate) const FOR: Duration = Duration::from_millis(500);
    const LOG_EVERY: Duration = Duration::from_secs(10);

    pub(crate) fn new(now: Instant) -> Self {
        Backlog { over_since: None, max: 0, sum: 0, samples: 0, dropped: 0, drops: 0, last_log: now }
    }

    /// One frame taken with `depth` waiting (itself included); true: drop what waits now.
    pub(crate) fn note(&mut self, depth: usize, now: Instant) -> bool {
        self.max = self.max.max(depth);
        self.sum += depth as u64;
        self.samples += 1;
        if depth <= Self::DEPTH {
            self.over_since = None;
            return false;
        }
        let since = *self.over_since.get_or_insert(now);
        if now.duration_since(since) >= Self::FOR {
            self.over_since = None;
            return true;
        }
        false
    }

    pub(crate) fn dropped(&mut self, n: u64) {
        self.dropped += n;
        self.drops += 1;
    }

    /// The ten-second line, once it's due (counts start again after it).
    pub(crate) fn log_due(&mut self, now: Instant) -> Option<String> {
        if now.duration_since(self.last_log) < Self::LOG_EVERY {
            return None;
        }
        self.last_log = now;
        let avg = if self.samples > 0 { self.sum as f64 / self.samples as f64 } else { 0.0 };
        let line = format!("[video] queue max {} avg {avg:.1}, dropped {}, idr requests {}", self.max, self.dropped, self.drops);
        *self = Backlog { last_log: now, ..Backlog::new(now) };
        Some(line)
    }
}

#[cfg(test)]
mod backlog_tests {
    use super::*;

    #[test]
    fn key_frames_are_found() {
        assert!(is_key_frame(&[0, 0, 0, 1, 0x67, 1, 2, 0, 0, 1, 0x68, 0, 0, 1, 0x65]));
        assert!(is_key_frame(&[0, 0, 1, 0x65, 9]));
        assert!(!is_key_frame(&[0, 0, 0, 1, 0x41, 1, 2, 3]));
        assert!(!is_key_frame(&[]));
    }

    #[test]
    fn a_short_burst_is_kept_a_long_one_dropped() {
        let t0 = Instant::now();
        let mut b = Backlog::new(t0);
        // (a burst under half a second: nothing dropped)
        for ms in (0..400).step_by(16) {
            assert!(!b.note(12, t0 + Duration::from_millis(ms)));
        }
        assert!(!b.note(2, t0 + Duration::from_millis(416)));
        // (more than six waiting for half a second: dropped once, then the count starts again)
        let mut drops = 0;
        for ms in (500..1100).step_by(16) {
            drops += b.note(9, t0 + Duration::from_millis(ms)) as u32;
        }
        assert_eq!(drops, 1);
        // (at six or fewer, never)
        for ms in (1100..5000).step_by(16) {
            assert!(!b.note(6, t0 + Duration::from_millis(ms)));
        }
    }

    #[test]
    fn the_log_line_comes_every_ten_seconds() {
        let t0 = Instant::now();
        let mut b = Backlog::new(t0);
        b.note(3, t0);
        b.note(9, t0);
        b.dropped(5);
        assert!(b.log_due(t0 + Duration::from_secs(9)).is_none());
        let line = b.log_due(t0 + Duration::from_secs(10)).unwrap_or_default();
        assert_eq!(line, "[video] queue max 9 avg 6.0, dropped 5, idr requests 1");
        assert!(b.log_due(t0 + Duration::from_secs(11)).is_none());
    }
}

/// Controllers in and sound out, on SDL's own thread (SDL's objects stay on the thread that made
/// them). Controllers work whichever window is in front: the player plays in the app's window.
fn sdl_thread(rx: Receiver<ToSdl>, session: Arc<Mutex<Option<Session>>>, stop: Arc<AtomicBool>, sound: Arc<Sound>, screen: Arc<dyn Screen>, stats: Arc<Stats>) {
    sdl2::hint::set("SDL_JOYSTICK_ALLOW_BACKGROUND_EVENTS", "1");
    sdl2::hint::set("SDL_JOYSTICK_HIDAPI_PS5_RUMBLE", "1");
    sdl2::hint::set("SDL_JOYSTICK_HIDAPI_PS4_RUMBLE", "1");
    let sdl = match sdl2::init() {
        Ok(s) => s,
        Err(e) => return screen.log(1, &format!("SDL: {e}")),
    };
    let gc = sdl.game_controller().map_err(|e| screen.log(1, &format!("controllers: {e}"))).ok();
    let audio = sdl.audio().map_err(|e| screen.log(1, &format!("sound: {e}"))).ok();
    let mut events = match sdl.event_pump() {
        Ok(e) => e,
        Err(e) => return screen.log(1, &format!("SDL events: {e}")),
    };
    let mut pads: Vec<sdl2::controller::GameController> = Vec::new();
    let mut queue: Option<(sdl2::audio::AudioQueue<i16>, u32, u32)> = None;
    let mut last = Pad::default();
    let mut last_sent = Instant::now() - Duration::from_secs(1);
    while !stop.load(Ordering::Relaxed) {
        for ev in events.poll_iter() {
            match ev {
                sdl2::event::Event::ControllerDeviceAdded { which, .. } => {
                    if let Some(c) = gc.as_ref().and_then(|g| g.open(which).ok()) {
                        screen.log(4, &format!("controller: {}", c.name()));
                        pads.push(c);
                    }
                }
                sdl2::event::Event::ControllerDeviceRemoved { which, .. } => pads.retain(|c| c.instance_id() != which),
                _ => {}
            }
        }
        stats.controllers.store(pads.len() as u64, Ordering::Relaxed);
        // every controller's buttons and sticks, together
        let mut p = Pad::default();
        for c in &pads {
            let s = pad_state(c);
            p.buttons |= s.buttons;
            p.l2 = p.l2.max(s.l2);
            p.r2 = p.r2.max(s.r2);
            for (a, b) in [(&mut p.lx, s.lx), (&mut p.ly, s.ly), (&mut p.rx, s.rx), (&mut p.ry, s.ry)] {
                if b.unsigned_abs() > a.unsigned_abs() {
                    *a = b;
                }
            }
        }
        // (and the app's own press, while it lasts)
        if let Some((v, until)) = *VIRTUAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
            && Instant::now() < until
        {
            p.buttons |= v.buttons;
            p.l2 = p.l2.max(v.l2);
            p.r2 = p.r2.max(v.r2);
            for (a, b) in [(&mut p.lx, v.lx), (&mut p.ly, v.ly), (&mut p.rx, v.rx), (&mut p.ry, v.ry)] {
                if b != 0 {
                    *a = b;
                }
            }
        }
        // (sent when it changes, and every half second anyway; never while read only, as a still
        // pad sent then would fight the player's own controller on the PS5)
        if READ_ONLY.load(Ordering::Relaxed) {
            last = Pad::default();
        } else if p != last || last_sent.elapsed() > Duration::from_millis(500) {
            if let Some(s) = session.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_ref() {
                s.controller(p);
            }
            last = p;
            last_sent = Instant::now();
        }
        while let Ok(m) = rx.try_recv() {
            match m {
                ToSdl::AudioFormat { channels, rate } => {
                    queue = audio.as_ref().and_then(|a| {
                        let want = sdl2::audio::AudioSpecDesired { freq: Some(rate as i32), channels: Some(channels as u8), samples: Some(512) };
                        match a.open_queue::<i16, _>(None, &want) {
                            Ok(q) => {
                                q.resume();
                                Some((q, channels, rate))
                            }
                            Err(e) => {
                                screen.log(1, &format!("sound: {e}"));
                                None
                            }
                        }
                    });
                }
                ToSdl::Pcm(mut pcm) => {
                    if let Some((q, ch, rate)) = &queue {
                        let gain = sound.gain();
                        if gain == 0.0 {
                            // (off: what is waiting goes too, so turning it on is heard at once)
                            q.clear();
                            continue;
                        }
                        scale(&mut pcm, gain);
                        // (more than ~150 ms waiting: drop the oldest, so the sound never lags the picture)
                        let max = (*rate as usize * *ch as usize * 2) * 150 / 1000;
                        if q.size() as usize > max {
                            q.clear();
                        }
                        let _ = q.queue_audio(&pcm);
                    }
                }
                ToSdl::Rumble { left, right } => {
                    for c in pads.iter_mut() {
                        let _ = c.set_rumble((left as u16) << 8, (right as u16) << 8, 5000);
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_millis(4));
    }
}

/// A controller's buttons and sticks as the PS5 takes them (SDL's names are the Xbox layout's).
fn pad_state(c: &sdl2::controller::GameController) -> Pad {
    use sdl2::controller::{Axis, Button};
    let map = [
        (Button::A, buttons::CROSS),
        (Button::B, buttons::CIRCLE),
        (Button::X, buttons::SQUARE),
        (Button::Y, buttons::TRIANGLE),
        (Button::DPadLeft, buttons::LEFT),
        (Button::DPadRight, buttons::RIGHT),
        (Button::DPadUp, buttons::UP),
        (Button::DPadDown, buttons::DOWN),
        (Button::LeftShoulder, buttons::L1),
        (Button::RightShoulder, buttons::R1),
        (Button::LeftStick, buttons::L3),
        (Button::RightStick, buttons::R3),
        (Button::Start, buttons::OPTIONS),
        (Button::Back, buttons::SHARE),
        (Button::Guide, buttons::PS),
        (Button::Touchpad, buttons::TOUCHPAD),
        (Button::Misc1, buttons::SHARE),
    ];
    let mut b = 0;
    for (sdl_button, ps) in map {
        if c.button(sdl_button) {
            b |= ps;
        }
    }
    let trigger = |a: Axis| (c.axis(a).max(0) >> 7) as u8;
    Pad {
        buttons: b,
        l2: trigger(Axis::TriggerLeft),
        r2: trigger(Axis::TriggerRight),
        lx: c.axis(Axis::LeftX),
        ly: c.axis(Axis::LeftY),
        rx: c.axis(Axis::RightX),
        ry: c.axis(Axis::RightY),
    }
}

/// Picture planes (studio-range BT.709 4:2:0) as YUY2 rows (Y0 U Y1 V), the layout the app's
/// preview and reading take. `stretch`: the PS5's picture is studio range twice over (black at ~16,
/// white at ~235 after the usual conversion), so the levels are stretched once more here, or its
/// dark menus come out grey and the pause menu isn't recognised.
pub fn i420_to_yuy2(f: &I420, stretch: bool) -> Vec<u8> {
    let (w, h) = (f.w as usize & !1, f.h as usize);
    let mut out = vec![0u8; w * h * 2];
    // Y' = (Y − 16)·255/219 and C' = 128 + (C − 128)·255/219: the usual studio-range conversion of
    // (Y', C') then gives the twice-stretched colour
    let mut ly = [0u8; 256];
    let mut lc = [0u8; 256];
    for i in 0..256 {
        ly[i] = if stretch { (((i as i32 - 16) * 255 + 109) / 219).clamp(0, 255) as u8 } else { i as u8 };
        lc[i] = if stretch { (128 + ((i as i32 - 128) * 255) / 219).clamp(0, 255) as u8 } else { i as u8 };
    }
    for y in 0..h {
        let yr = &f.y[y * f.y_stride..];
        let ur = &f.u[(y / 2) * f.uv_stride..];
        let vr = &f.v[(y / 2) * f.uv_stride..];
        let o = &mut out[y * w * 2..(y + 1) * w * 2];
        for x in (0..w).step_by(2) {
            o[x * 2] = ly[yr[x] as usize];
            o[x * 2 + 1] = lc[ur[x / 2] as usize];
            o[x * 2 + 2] = ly[yr[x + 1] as usize];
            o[x * 2 + 3] = lc[vr[x / 2] as usize];
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_volume_scales_the_sound() {
        let s = Sound::new(true, 50);
        assert!((s.gain() - 0.25).abs() < 1e-6);
        let mut pcm = [1000i16, -1000, i16::MAX];
        scale(&mut pcm, s.gain());
        assert_eq!(pcm, [250, -250, 8192]);
        s.on.store(false, Ordering::Relaxed);
        assert_eq!(s.gain(), 0.0);
        assert_eq!(Sound::new(true, 100).gain(), 1.0);
    }

    #[test]
    fn planes_become_yuy2_rows() {
        // 4 x 2: luma 10..17, one chroma pair per 2 x 2 block
        let f = I420 { w: 4, h: 2, y: (10..18).collect(), u: vec![100, 110], v: vec![200, 210], y_stride: 4, uv_stride: 2 };
        let o = i420_to_yuy2(&f, false);
        assert_eq!(&o[..8], &[10, 100, 11, 200, 12, 110, 13, 210]);
        assert_eq!(&o[8..], &[14, 100, 15, 200, 16, 110, 17, 210]);
    }

    #[test]
    fn the_second_stretch_makes_studio_black_and_white_full() {
        let f = I420 { w: 2, h: 1, y: vec![16, 235], u: vec![128], v: vec![128], y_stride: 2, uv_stride: 1 };
        let o = i420_to_yuy2(&f, true);
        // studio black and white of the picture land on studio black and white of the conversion
        // after one more stretch: 16 → 0 and 235 → 255 here, i.e. 0 and 255 after the usual one
        assert_eq!((o[0], o[2]), (0, 255));
        assert_eq!((o[1], o[3]), (128, 128), "grey stays grey");
    }
}
