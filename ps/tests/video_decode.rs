//! The stream's decoders on a clip made here (OpenH264's encoder, a moving picture with text-like
//! edges, a key frame a second as the PS5 sends): the computer's own decoder (VideoToolbox on a
//! Mac, Media Foundation on Windows) must give the same picture as OpenH264, and how long each
//! takes a frame is written to $VIT_BENCH_OUT (video-bench.md) when it's set.

use openh264::encoder::{BitRate, Encoder, EncoderConfig, FrameRate, IntraFramePeriod};
use openh264::formats::YUVBuffer;
use openh264::OpenH264API;
use std::time::{Duration, Instant};
use vi_translator_ps::hwdec::{VideoDecoder, nv12_to_i420};
use vi_translator_ps::play::I420;

const W: usize = 1920;
const H: usize = 1080;

fn clip(frames: usize) -> Vec<Vec<u8>> {
    let cfg = EncoderConfig::new()
        .bitrate(BitRate::from_bps(15_000_000))
        .max_frame_rate(FrameRate::from_hz(60.0))
        .intra_frame_period(IntraFramePeriod::from_num_frames(60))
        .skip_frames(false);
    let mut enc = Encoder::with_api_config(OpenH264API::from_source(), cfg).expect("encoder");
    let mut out = Vec::with_capacity(frames);
    let mut rgb = vec![0u8; W * H * 3];
    for i in 0..frames {
        for y in 0..H {
            for x in 0..W {
                let p = (y * W + x) * 3;
                // (a moving gradient, and bars like lines of text that scroll)
                let text = ((y / 24) % 3 == 0) && ((x + i * 4) / 9) % 5 != 0 && y % 24 < 14;
                let v = if text { 245 } else { ((x + i * 3) % 256) as u8 / 2 + ((y * 255) / H) as u8 / 3 };
                rgb[p] = v;
                rgb[p + 1] = if text { 245 } else { v / 2 + 40 };
                rgb[p + 2] = if text { 245 } else { 255 - v };
            }
        }
        let yuv = YUVBuffer::from_rgb8_source(openh264::formats::RgbSliceU8::new(&rgb, (W, H)));
        let bs = enc.encode(&yuv).expect("encode");
        out.push(bs.to_vec());
    }
    out
}

fn psnr_y(a: &I420, b: &I420) -> f64 {
    assert_eq!((a.w, a.h), (b.w, b.h), "the decoders gave pictures of different sizes");
    let (w, h) = (a.w as usize, a.h as usize);
    let mut se = 0f64;
    for y in 0..h {
        for x in 0..w {
            let d = a.y[y * a.y_stride + x] as f64 - b.y[y * b.y_stride + x] as f64;
            se += d * d;
        }
    }
    let mse = se / (w * h) as f64;
    if mse == 0.0 { 99.0 } else { 10.0 * (255.0 * 255.0 / mse).log10() }
}

struct Run {
    name: String,
    hardware: bool,
    pictures: Vec<I420>,
    times: Vec<Duration>,
    /// CPU time of the whole process while decoding, as a share of one core's wall time
    cpu_pct: f64,
}

fn run(mut d: VideoDecoder, frames: &[Vec<u8>]) -> Run {
    let name = d.name().to_string();
    let mut pictures = Vec::new();
    let mut times = Vec::new();
    let (cpu0, wall0) = (cpu_time::ProcessTime::now(), Instant::now());
    for f in frames {
        let t = Instant::now();
        let p = d.decode(f).unwrap_or_else(|e| panic!("{name}: {e}"));
        times.push(t.elapsed());
        if let Some(p) = p {
            pictures.push(p);
        }
    }
    let cpu_pct = cpu0.elapsed().as_secs_f64() / wall0.elapsed().as_secs_f64().max(1e-9) * 100.0;
    // (known only once it has decoded: VideoToolbox in a virtual Mac, Media Foundation without a
    // graphics card, decode on the CPU)
    let hardware = d.decodes_in_hardware();
    let name = d.name().to_string();
    Run { name, hardware, pictures, times, cpu_pct }
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// (avg, p95, max) ms a frame
fn stats(r: &Run) -> (f64, f64, f64) {
    let mut t: Vec<f64> = r.times.iter().map(|d| ms(*d)).collect();
    t.sort_by(f64::total_cmp);
    let avg = t.iter().sum::<f64>() / t.len() as f64;
    (avg, t[(t.len() * 95 / 100).min(t.len() - 1)], t[t.len() - 1])
}

fn summary(r: &Run) -> String {
    let (avg, p95, max) = stats(r);
    format!("| {} | {} | {} | {avg:.2} | {p95:.2} | {max:.2} | {:.0} % |", r.name, if r.hardware { "yes" } else { "no" }, r.pictures.len(), r.cpu_pct)
}

const FRAMES: usize = 1200;

#[test]
fn nv12_round_trip_keeps_the_picture() {
    let (w, h) = (6usize, 4usize);
    let y: Vec<u8> = (0..w * h).map(|i| i as u8).collect();
    let uv: Vec<u8> = (0..w * h / 2).map(|i| 100 + i as u8).collect();
    let f = nv12_to_i420(w, h, &y, w, &uv, w);
    assert_eq!(f.u, vec![100, 102, 104, 106, 108, 110]);
    assert_eq!(f.v, vec![101, 103, 105, 107, 109, 111]);
}

#[test]
fn the_system_decoder_matches_openh264() {
    if cfg!(debug_assertions) {
        // (the clip takes minutes to make unoptimized: cargo test --release)
        return;
    }
    let frames = clip(FRAMES);
    let soft = run(VideoDecoder::software().expect("OpenH264"), &frames);
    assert!(soft.pictures.len() >= FRAMES - 2, "OpenH264 gave {} pictures of {FRAMES}", soft.pictures.len());
    let mut lines = vec![
        "# Video decoding benchmark".to_string(),
        String::new(),
        format!("Clip: {W}×{H}, {FRAMES} frames (20 s at 60), 15 Mbit/s, a key frame every 60: a moving gradient with scrolling text-like bars, made with OpenH264's encoder in the test (no ffmpeg on the runners)."),
        String::new(),
        "| Decoder | Hardware | Pictures | Avg ms/frame | p95 ms | Max ms | CPU (one core = 100 %) |".to_string(),
        "|---|---|---|---|---|---|---|".to_string(),
        summary(&soft),
    ];
    let mut note = String::new();
    match VideoDecoder::system_for_test() {
        Some(d) => {
            let sys = run(d, &frames);
            lines.push(summary(&sys));
            // (the system decoder may hold back its first picture or two; the rest line up)
            let n = sys.pictures.len().min(soft.pictures.len());
            assert!(n >= FRAMES - 10, "{} gave {} pictures of {FRAMES}", sys.name, sys.pictures.len());
            let lag = soft.pictures.len() - n;
            let mut worst = 99.0f64;
            for i in 0..n {
                worst = worst.min(psnr_y(&sys.pictures[i], &soft.pictures[i + lag]));
            }
            note = format!("Same picture: lowest PSNR (Y) between {} and OpenH264 over {n} pictures: {worst:.1} dB (99 = identical).", sys.name);
            assert!(worst >= 40.0, "{note}");
            let ((s_avg, _, _), (h_avg, h_p95, _)) = (stats(&soft), stats(&sys));
            if sys.hardware {
                // (the hardware decoder must not be slower than the CPU's, and on the Mac a frame's p95 at most 8 ms)
                assert!(h_avg <= s_avg, "{} ({h_avg:.2} ms a frame) is slower than OpenH264 ({s_avg:.2} ms)", sys.name);
                if cfg!(target_os = "macos") {
                    assert!(h_p95 <= 8.0, "{} p95 {h_p95:.2} ms > 8 ms", sys.name);
                }
            } else {
                note.push_str(&format!(" {} decodes on the CPU on this runner (a virtual machine: no media engine or graphics card), so the speed checks, which are for hardware, do not apply; the app itself uses OpenH264 in that case.", sys.name));
            }
        }
        None => note.push_str("No system decoder on this computer (Linux, or none available): OpenH264 only."),
    }
    lines.push(String::new());
    lines.push(note);
    let md = lines.join("\n") + "\n";
    println!("{md}");
    if let Some(p) = std::env::var_os("VIT_BENCH_OUT") {
        std::fs::write(p, md).expect("writing the benchmark");
    }
}
