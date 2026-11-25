//! OCR engine on Windows: the one built into Windows (Windows.Media.Ocr, English).

use anyhow::{Context as _, Result, anyhow};
use windows::Globalization::Language;
use windows::Graphics::Imaging::{BitmapPixelFormat, SoftwareBitmap};
use windows::Media::Ocr::OcrEngine;
use windows::Storage::Streams::DataWriter;
use windows::core::HSTRING;

use vi_translator_core::ocr::RawWord;

pub struct Engine {
    engine: OcrEngine,
}

impl Engine {
    /// Must be called on a thread that has WinRT initialised (see `init_thread`).
    pub fn new() -> Result<Self> {
        let lang = Language::CreateLanguage(&HSTRING::from("en-US"))?;
        let engine = OcrEngine::TryCreateFromLanguage(&lang)
            .context("Windows English OCR is not installed (Settings > Time & language > Language > English)")?;
        Ok(Self { engine })
    }

    pub fn init_thread() {
        unsafe {
            let _ = windows::Win32::System::WinRT::RoInitialize(windows::Win32::System::WinRT::RO_INIT_MULTITHREADED);
        }
    }

    /// The largest image side the engine accepts.
    pub fn max_dim() -> u32 {
        OcrEngine::MaxImageDimension().unwrap_or(2600)
    }

    /// Words in a dark-on-light BGRA image, with their line numbers and boxes (image pixels).
    pub fn read(&self, bgra: &[u8], w: u32, h: u32) -> Result<Vec<RawWord>> {
        let writer = DataWriter::new()?;
        writer.WriteBytes(bgra)?;
        let buffer = writer.DetachBuffer()?;
        let bitmap = SoftwareBitmap::CreateCopyFromBuffer(&buffer, BitmapPixelFormat::Bgra8, w as i32, h as i32)?;
        let result = self.engine.RecognizeAsync(&bitmap)?.join().map_err(|e| anyhow!("OCR failed: {e}"))?;
        // Text that is turned (a phone held tilted): Windows reads it straightened, turned by TextAngle round the
        // picture's middle, and gives its words' boxes in that straightened picture. Each box turned back to where
        // the word is in this one (its middle; its size as read). Without it a tilted phone's words were placed
        // up to tens of pixels off, more the further from the middle.
        let angle = result.TextAngle().ok().and_then(|a| a.Value().ok()).unwrap_or(0.0);
        let (cx, cy) = (w as f64 / 2.0, h as f64 / 2.0);
        let (sin, cos) = angle.to_radians().sin_cos();
        let back = |x: f64, y: f64| (cx + (x - cx) * cos - (y - cy) * sin, cy + (x - cx) * sin + (y - cy) * cos);
        let mut words = Vec::new();
        for (line_no, line) in result.Lines()?.into_iter().enumerate() {
            for word in line.Words()? {
                let r = word.BoundingRect()?;
                let (rw, rh) = (r.Width as f64, r.Height as f64);
                let (mx, my) = if angle.abs() > 0.2 { back(r.X as f64 + rw / 2.0, r.Y as f64 + rh / 2.0) } else { (r.X as f64 + rw / 2.0, r.Y as f64 + rh / 2.0) };
                words.push(RawWord {
                    line: line_no as u32,
                    text: word.Text()?.to_string(),
                    x: mx - rw / 2.0,
                    y: my - rh / 2.0,
                    w: rw,
                    h: rh,
                });
            }
        }
        if std::env::var_os("VIT_DISK_DEBUG").is_some() && angle.abs() > 0.2 {
            vi_translator_core::vlog!("[ocr] text angle {angle:.2}°");
        }
        Ok(words)
    }
}
