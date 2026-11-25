//! The capture card's sound, played on this PC's speakers while the card is the picture source (the
//! app reads only the card's picture, so without this the game is silent when watched in the app).
//! The card's sound input is found by its name (it carries the card's name: "Digital Audio Interface
//! (USB3.0 Capture)" for the card "USB3.0 Capture"), or the one the player picked.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::capture::Shared;
use crate::config::Config;

/// What the player picked, as the sound loop needs it.
#[derive(Clone, PartialEq)]
struct Want {
    card: String,
    input: String,
    volume: f32,
}

/// The input the card's sound comes in on: the one picked, else the one whose name holds the card's
/// name (or most of its words).
pub fn match_input<'a>(card: &str, picked: &str, inputs: &'a [String]) -> Option<&'a String> {
    if !picked.is_empty() {
        return inputs.iter().find(|n| n.as_str() == picked);
    }
    let low = card.to_lowercase();
    if let Some(n) = inputs.iter().find(|n| n.to_lowercase().contains(&low)) {
        return Some(n);
    }
    // the words of the card's name (not short ones) found in each input's name
    let words: Vec<String> = low.split(|c: char| !c.is_alphanumeric()).filter(|w| w.len() >= 3).map(str::to_string).collect();
    if words.is_empty() {
        return None;
    }
    let score = |n: &String| {
        let n = n.to_lowercase();
        words.iter().filter(|w| n.contains(w.as_str())).count()
    };
    inputs.iter().filter(|n| score(n) * 2 > words.len()).max_by_key(|n| score(n))
}

/// The ring between the card's sound and the speakers: samples in the speakers' layout (channels and
/// rate), kept short so the sound stays with the picture.
#[cfg_attr(target_os = "linux", allow(dead_code))] // (the speakers' side: not on Linux yet)
struct Ring {
    buf: std::collections::VecDeque<f32>,
    /// at most this many samples are kept (older ones are dropped: the sound never lags behind)
    max: usize,
}

/// Turn a block of input samples (`in_ch` channels at `in_rate`) into the output layout, resampled
/// (linear) and with the channels matched: mono to every channel, more channels down to the first ones.
#[cfg_attr(target_os = "linux", allow(dead_code))]
pub fn convert(input: &[f32], in_ch: usize, in_rate: u32, out_ch: usize, out_rate: u32, phase: &mut f64, last: &mut Vec<f32>, out: &mut Vec<f32>) {
    let frames = if in_ch == 0 { 0 } else { input.len() / in_ch };
    if frames == 0 || out_ch == 0 || in_rate == 0 || out_rate == 0 {
        return;
    }
    if last.len() != in_ch {
        *last = vec![0.0; in_ch];
    }
    let step = in_rate as f64 / out_rate as f64;
    // input frame `i` of this block; −1 is the last block's final frame
    let at = |i: isize, c: usize| if i < 0 { last[c] } else { input[i as usize * in_ch + c] };
    // `phase`: where the next output frame falls, in input frames from this block's start (−1 .. 0
    // falls between the last block and this one)
    while *phase <= (frames - 1) as f64 {
        let i = phase.floor() as isize;
        let t = (*phase - phase.floor()) as f32;
        for c in 0..out_ch {
            let src = if in_ch == 1 { 0 } else { c.min(in_ch - 1) };
            let a = at(i, src);
            let b = if i + 1 <= frames as isize - 1 { at(i + 1, src) } else { a };
            out.push(a + (b - a) * t);
        }
        *phase += step;
    }
    last.copy_from_slice(&input[(frames - 1) * in_ch..frames * in_ch]);
    *phase -= frames as f64;
}

pub fn run(cfg: Arc<Mutex<Config>>, shared: Arc<Shared>) {
    let mut running: Option<(Want, platform::Playing)> = None;
    loop {
        let want = {
            let c = cfg.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            c.window_title.strip_prefix(crate::device::PREFIX).filter(|_| c.card_sound).map(|card| Want {
                card: card.to_string(),
                input: c.card_sound_input.clone(),
                volume: (c.card_sound_volume.min(100) as f32) / 100.0,
            })
        };
        match (&want, &running) {
            (None, _) => {
                if running.take().is_some() || !shared.sound_status.lock().unwrap_or_else(std::sync::PoisonError::into_inner).is_empty() {
                    *shared.sound_status.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = String::new();
                }
            }
            (Some(w), Some((r, p))) if r.card == w.card && r.input == w.input && p.alive() => {
                if r.volume != w.volume {
                    p.set_volume(w.volume);
                    running.as_mut().unwrap().0.volume = w.volume;
                }
            }
            (Some(w), _) => {
                running = None;
                let inputs = platform::inputs();
                match match_input(&w.card, &w.input, &inputs) {
                    None => {
                        // (statuses are "what|detail": the window words them in its language)
                        *shared.sound_status.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = if w.input.is_empty() { "none".into() } else { format!("missing|{}", w.input) };
                    }
                    Some(name) => match platform::play(name, w.volume) {
                        Ok(p) => {
                            *shared.sound_status.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = format!("playing|{name}");
                            running = Some((w.clone(), p));
                        }
                        Err(e) => *shared.sound_status.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = format!("error|{e}"),
                    },
                }
            }
        }
        std::thread::sleep(Duration::from_millis(if running.is_some() { 500 } else { 1500 }));
    }
}

/// Names of the sound inputs (a capture card's own, microphones …).
pub fn list_inputs() -> Vec<String> {
    platform::inputs()
}

/// `--test-sound ["input"]`: the inputs, then 2 s from that one (or the first) to the speakers at
/// volume 0 (silent), and how many samples went each way.
#[cfg(not(target_os = "linux"))]
pub fn test(name: Option<&str>) {
    let inputs = platform::inputs();
    println!("inputs: {inputs:?}");
    let Some(name) = name.map(str::to_string).or_else(|| inputs.first().cloned()) else { return };
    match platform::play(&name, 0.0) {
        Ok(p) => {
            std::thread::sleep(Duration::from_secs(2));
            use std::sync::atomic::Ordering::Relaxed;
            println!("\"{name}\": {} samples in, {} out, alive {}", p.counts.0.load(Relaxed), p.counts.1.load(Relaxed), p.alive());
        }
        Err(e) => println!("\"{name}\": {e}"),
    }
}

#[cfg(not(target_os = "linux"))]
mod platform {
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use std::sync::{Arc, Mutex};

    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

    use super::{Ring, convert};

    fn name_of(d: &cpal::Device) -> Option<String> {
        d.description().ok().map(|x| x.name().to_string())
    }

    pub fn inputs() -> Vec<String> {
        let host = cpal::default_host();
        let mut out: Vec<String> = host.input_devices().map(|it| it.filter_map(|d| name_of(&d)).collect()).unwrap_or_default();
        out.sort();
        out.dedup();
        out
    }

    /// The card's sound going to the speakers. Dropping it stops both.
    pub struct Playing {
        _input: cpal::Stream,
        _output: cpal::Stream,
        volume: Arc<AtomicU32>,
        failed: Arc<AtomicBool>,
        /// samples that came in from the card, and went out to the speakers (for `--test-sound`)
        pub counts: Arc<(std::sync::atomic::AtomicU64, std::sync::atomic::AtomicU64)>,
    }

    impl Playing {
        pub fn alive(&self) -> bool {
            !self.failed.load(Ordering::Relaxed)
        }
        pub fn set_volume(&self, v: f32) {
            self.volume.store(v.to_bits(), Ordering::Relaxed);
        }
    }

    pub fn play(name: &str, volume: f32) -> Result<Playing, String> {
        let host = cpal::default_host();
        let input = host
            .input_devices()
            .map_err(|e| e.to_string())?
            .find(|d| name_of(d).as_deref() == Some(name))
            .ok_or_else(|| format!("\"{name}\" not found"))?;
        let output = host.default_output_device().ok_or("no speakers or headphones")?;
        let in_cfg = input.default_input_config().map_err(|e| e.to_string())?;
        let out_cfg = output.default_output_config().map_err(|e| e.to_string())?;
        let (in_ch, in_rate) = (in_cfg.channels() as usize, in_cfg.sample_rate());
        let (out_ch, out_rate) = (out_cfg.channels() as usize, out_cfg.sample_rate());
        // 120 ms at most between the card and the speakers
        let ring = Arc::new(Mutex::new(Ring { buf: Default::default(), max: out_rate as usize * out_ch * 12 / 100 }));
        let failed = Arc::new(AtomicBool::new(false));
        let vol = Arc::new(AtomicU32::new(volume.to_bits()));

        let counts = Arc::new((std::sync::atomic::AtomicU64::new(0), std::sync::atomic::AtomicU64::new(0)));
        let (c_in, c_out) = (counts.clone(), counts.clone());
        let r_in = ring.clone();
        let mut state = (0.0f64, Vec::new(), Vec::new());
        let mut push = move |data: &[f32]| {
            let (phase, last, out) = &mut state;
            c_in.0.fetch_add(data.len() as u64, Ordering::Relaxed);
            out.clear();
            convert(data, in_ch, in_rate, out_ch, out_rate, phase, last, out);
            let mut r = r_in.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            r.buf.extend(out.iter().copied());
            let over = r.buf.len().saturating_sub(r.max);
            r.buf.drain(..over - over % out_ch);
        };
        let f_in = failed.clone();
        let on_err = move |_e: cpal::Error| f_in.store(true, Ordering::Relaxed);
        let in_stream = match in_cfg.sample_format() {
            cpal::SampleFormat::F32 => input.build_input_stream(in_cfg.config(), move |d: &[f32], _: &_| push(d), on_err, None),
            cpal::SampleFormat::I16 => {
                let mut tmp = Vec::new();
                input.build_input_stream(
                    in_cfg.config(),
                    move |d: &[i16], _: &_| {
                        tmp.clear();
                        tmp.extend(d.iter().map(|s| *s as f32 / 32768.0));
                        push(&tmp)
                    },
                    on_err,
                    None,
                )
            }
            f => return Err(format!("the card's sound comes as {f:?}, which isn't supported")),
        }
        .map_err(|e| e.to_string())?;

        let r_out = ring.clone();
        let v_out = vol.clone();
        // wait until a little sound has gathered (so short gaps don't crackle), then keep playing
        let mut primed = false;
        let mut pull = move |data: &mut [f32]| {
            let mut r = r_out.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let v = f32::from_bits(v_out.load(Ordering::Relaxed));
            if !primed && r.buf.len() >= out_rate as usize * out_ch * 3 / 100 {
                primed = true;
            }
            for s in data.iter_mut() {
                *s = if primed { r.buf.pop_front().map(|x| x * v).unwrap_or(0.0) } else { 0.0 };
            }
            if primed {
                c_out.1.fetch_add(data.len() as u64, Ordering::Relaxed);
            }
            if r.buf.is_empty() {
                primed = false;
            }
        };
        let f_out = failed.clone();
        let on_err_out = move |_e: cpal::Error| f_out.store(true, Ordering::Relaxed);
        let out_stream = match out_cfg.sample_format() {
            cpal::SampleFormat::F32 => output.build_output_stream(out_cfg.config(), move |d: &mut [f32], _: &_| pull(d), on_err_out, None),
            cpal::SampleFormat::I16 => {
                let mut tmp = Vec::new();
                output.build_output_stream(
                    out_cfg.config(),
                    move |d: &mut [i16], _: &_| {
                        tmp.resize(d.len(), 0.0);
                        pull(&mut tmp);
                        for (o, s) in d.iter_mut().zip(&tmp) {
                            *o = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
                        }
                    },
                    on_err_out,
                    None,
                )
            }
            f => return Err(format!("the speakers take {f:?}, which isn't supported")),
        }
        .map_err(|e| e.to_string())?;
        in_stream.play().map_err(|e| e.to_string())?;
        out_stream.play().map_err(|e| e.to_string())?;
        Ok(Playing { _input: in_stream, _output: out_stream, volume: vol, failed, counts })
    }
}

#[cfg(target_os = "linux")]
mod platform {
    pub fn inputs() -> Vec<String> {
        Vec::new()
    }
    pub struct Playing;
    impl Playing {
        pub fn alive(&self) -> bool {
            true
        }
        pub fn set_volume(&self, _v: f32) {}
    }
    pub fn play(_name: &str, _volume: f32) -> Result<Playing, String> {
        Err("playing the card's sound isn't on Linux yet".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cards_sound_input_is_found_by_its_name() {
        let inputs: Vec<String> = ["Microphone (Realtek Audio)", "Digital Audio Interface (USB3.0 Capture)", "Elgato 4K X Audio"].map(String::from).to_vec();
        assert_eq!(match_input("USB3.0 Capture", "", &inputs).map(String::as_str), Some("Digital Audio Interface (USB3.0 Capture)"));
        assert_eq!(match_input("Elgato 4K X", "", &inputs).map(String::as_str), Some("Elgato 4K X Audio"));
        assert_eq!(match_input("Cam Link 4K", "", &inputs), None);
        // a fifine card: "fifine Video Capture" and "Digital Audio Interface (fifine Audio Capture)"
        let fifine: Vec<String> = ["Line In (Realtek USB Audio)", "Digital Audio Interface (fifine Audio Capture)"].map(String::from).to_vec();
        assert_eq!(match_input("fifine Video Capture", "", &fifine).map(String::as_str), Some("Digital Audio Interface (fifine Audio Capture)"));
        assert_eq!(match_input("USB Video", "", &fifine), None, "one shared word (USB) isn't enough");
        assert_eq!(match_input("USB3.0 Capture", "Microphone (Realtek Audio)", &inputs).map(String::as_str), Some("Microphone (Realtek Audio)"));
    }

    #[test]
    fn sound_is_resampled_and_its_channels_matched() {
        // 48 kHz stereo -> 48 kHz stereo: the same samples, one block after another
        let (mut phase, mut last, mut out) = (0.0, Vec::new(), Vec::new());
        let block: Vec<f32> = (0..8).map(|i| i as f32).collect(); // 4 frames L R
        convert(&block, 2, 48000, 2, 48000, &mut phase, &mut last, &mut out);
        convert(&block, 2, 48000, 2, 48000, &mut phase, &mut last, &mut out);
        assert_eq!(out.len(), 16, "{out:?}");
        // mono 24 kHz -> stereo 48 kHz: twice as many frames, both channels the same
        let (mut phase, mut last, mut out) = (0.0, Vec::new(), Vec::new());
        convert(&[0.0, 1.0, 0.0, 1.0], 1, 24000, 2, 48000, &mut phase, &mut last, &mut out);
        convert(&[0.0, 1.0, 0.0, 1.0], 1, 24000, 2, 48000, &mut phase, &mut last, &mut out);
        assert!((28..=34).contains(&out.len()), "{} samples", out.len());
        assert!(out.chunks(2).all(|f| f[0] == f[1]));
        assert!(out.iter().all(|s| (0.0..=1.0).contains(s)));
    }
}
