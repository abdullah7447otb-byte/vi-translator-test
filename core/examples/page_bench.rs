//! The page bench: GTA VI's page script, as the app runs it, on saved frames, with a reader that runs
//! anywhere (a Python RapidOCR server, `ocr_server.py`, for the host's Vision or Windows OCR).
//!
//!     cargo run --release --example page_bench -- <ocr_server.py> <out dir> <frame.png>...
//!
//! For each frame: which pages say they're up (`present`), and what each reads (`read`, the layout the
//! overlay draws), into `<out dir>/<n>.json`.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Mutex;

use vi_translator_core::frame::FrameData;
use vi_translator_core::ocr::{Ocr, RawWord, TextEngine};
use vi_translator_core::pagescript::Pages;

struct PyOcr {
    io: Mutex<(Child, ChildStdin, BufReader<ChildStdout>)>,
    n: Mutex<u64>,
}

impl PyOcr {
    fn start(server: &str) -> PyOcr {
        let mut child = Command::new("python3").arg(server).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().expect("the reader");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));
        PyOcr { io: Mutex::new((child, stdin, stdout)), n: Mutex::new(0) }
    }
}

impl TextEngine for PyOcr {
    fn read(&self, bgra: &[u8], w: u32, h: u32) -> anyhow::Result<Vec<RawWord>> {
        let n = {
            let mut n = self.n.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            *n += 1;
            *n
        };
        let path = std::env::temp_dir().join(format!("page_bench_{}_{n}.png", std::process::id()));
        let rgba: Vec<u8> = bgra.chunks_exact(4).flat_map(|p| [p[2], p[1], p[0], 255]).collect();
        image::save_buffer(&path, &rgba, w, h, image::ExtendedColorType::Rgba8)?;
        let line = {
            let mut io = self.io.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            writeln!(io.1, "{}", path.display())?;
            io.1.flush()?;
            let mut s = String::new();
            io.2.read_line(&mut s)?;
            s
        };
        let _ = std::fs::remove_file(&path);
        let v: Vec<serde_json::Value> = serde_json::from_str(line.trim()).unwrap_or_default();
        Ok(v.iter()
            .map(|w| RawWord {
                line: w["line"].as_u64().unwrap_or(0) as u32,
                text: w["text"].as_str().unwrap_or("").to_string(),
                x: w["x"].as_f64().unwrap_or(0.0),
                y: w["y"].as_f64().unwrap_or(0.0),
                w: w["w"].as_f64().unwrap_or(0.0),
                h: w["h"].as_f64().unwrap_or(0.0),
            })
            .collect())
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (server, out) = (&args[0], std::path::PathBuf::from(&args[1]));
    std::fs::create_dir_all(&out).expect("the out dir");
    vi_translator_core::screens::use_words_for("gta6", "ar");
    let pages = match std::env::var("BENCH_SCRIPT") {
        Ok(file) => Pages::load(&std::fs::read_to_string(file).expect("the script")).expect("loads"),
        Err(_) => Pages::for_game("gta6", &std::env::temp_dir()).expect("GTA VI's pages"),
    };
    let ocr = Ocr::with_engine(Box::new(PyOcr::start(server)));
    // (PHONE=1: where the phone is held, per frame)
    if std::env::var_os("PHONE").is_some() {
        for file in &args[2..] {
            let img = image::open(file).expect("a frame").to_rgba8();
            let (w, h) = img.dimensions();
            println!("{file}\t{:?}", vi_translator_core::phone_pose::locate(img.as_raw(), w, h));
        }
        return;
    }
    // (AREA=x,y,w,h: an area's crop read as the engine reads it, its words printed)
    if let Ok(a) = std::env::var("AREA") {
        let v: Vec<f64> = a.split(',').filter_map(|t| t.parse().ok()).collect();
        for file in &args[2..] {
            let img = image::open(file).expect("a frame").to_rgba8();
            let (fw, fh) = img.dimensions();
            let (x, y) = ((v[0] * fw as f64) as u32, (v[1] * fh as f64) as u32);
            let c = image::imageops::crop_imm(&img, x, y, (v[2] * fw as f64) as u32, (v[3] * fh as f64) as u32).to_image();
            let crop = vi_translator_core::ocr::Crop { w: c.width(), h: c.height(), rgba: c.into_raw() };
            match ocr.recognize_checked(&crop) {
                Ok((ws, sure)) => println!("{file}\tsure={sure}\t{:?}", ws.iter().map(|w| (w.line, w.text.clone(), w.rect)).collect::<Vec<_>>()),
                Err(e) => println!("{file}\terror {e}"),
            }
        }
        return;
    }
    for (i, file) in args[2..].iter().enumerate() {
        let img = image::open(file).expect("a frame").to_rgba8();
        let f = FrameData { w: img.width(), h: img.height(), rgba: img.into_raw(), seq: i as u64 + 1 };
        let mut found = serde_json::Map::new();
        for p in &pages.list {
            if let Some(v) = pages.present(&p.id, &f, &ocr) {
                let layout = pages.read(&p.id, &f, &ocr, &v);
                found.insert(p.id.clone(), serde_json::json!({ "present": v, "layout": layout }));
            }
        }
        let names: Vec<&String> = found.keys().collect();
        println!("{i}\t{file}\t{names:?}");
        std::fs::write(out.join(format!("{i}.json")), serde_json::to_string_pretty(&serde_json::Value::Object(found)).unwrap_or_default()).expect("written");
    }
}
