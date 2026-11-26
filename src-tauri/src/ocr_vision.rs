//! OCR engine on macOS: Apple Vision (VNRecognizeTextRequest, English, accurate).
//!
//! Vision reports lines; each word's box is Vision's own for its characters (boundingBoxForRange),
//! else the line's box shared out between its words by character position. The game's names go in as
//! Vision's own words (`customWords`, used with language correction), so "Lucia" or "Leonida" aren't
//! "corrected" into English words. Lines Vision gives twice are merged in the core (ocr::without_repeated_lines).

use anyhow::{Result, anyhow};
use objc2::AllocAnyThread;
use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::AnyObject;
use objc2_core_foundation::CFData;
use objc2_core_graphics::{CGBitmapInfo, CGColorRenderingIntent, CGColorSpace, CGDataProvider, CGImage, CGImageAlphaInfo};
use objc2_foundation::{NSArray, NSDictionary, NSRange, NSString};
use objc2_vision::{
    VNImageOption, VNImageRequestHandler, VNRecognizeTextRequest, VNRequest, VNRequestTextRecognitionLevel,
};

use vi_translator_core::ocr::RawWord;

pub struct Engine {
    /// the game's names, for Vision's customWords
    names: Vec<String>,
}

impl Engine {
    pub fn new() -> Result<Self> {
        Ok(Self { names: vi_translator_core::english::game_names() })
    }

    pub fn init_thread() {}

    pub fn max_dim() -> u32 {
        4000
    }

    /// Words in a dark-on-light image (4 bytes per pixel, grey), with line numbers and boxes in
    /// image pixels.
    pub fn read(&self, bgra: &[u8], w: u32, h: u32) -> Result<Vec<RawWord>> {
        // the prepared image is grey: one channel is enough
        let grey: Vec<u8> = bgra.chunks_exact(4).map(|p| p[0]).collect();
        autoreleasepool(|_| unsafe { recognize(&grey, w, h, &self.names) })
    }
}

unsafe fn grey_image(grey: &[u8], w: u32, h: u32) -> Result<objc2_core_foundation::CFRetained<CGImage>> {
    let data = CFData::from_bytes(grey);
    let provider = CGDataProvider::with_cf_data(Some(&data)).ok_or_else(|| anyhow!("OCR: no data provider"))?;
    let space = CGColorSpace::new_device_gray().ok_or_else(|| anyhow!("OCR: no colour space"))?;
    unsafe {
        CGImage::new(
            w as usize,
            h as usize,
            8,
            8,
            w as usize,
            Some(&space),
            CGBitmapInfo(CGImageAlphaInfo::None.0),
            Some(&provider),
            std::ptr::null(),
            false,
            CGColorRenderingIntent::RenderingIntentDefault,
        )
    }
    .ok_or_else(|| anyhow!("OCR: could not make an image"))
}

unsafe fn recognize(grey: &[u8], w: u32, h: u32, names: &[String]) -> Result<Vec<RawWord>> {
    let image = unsafe { grey_image(grey, w, h)? };
    let request = VNRecognizeTextRequest::new();
    request.setRecognitionLevel(VNRequestTextRecognitionLevel::Accurate);
    request.setRecognitionLanguages(&NSArray::from_retained_slice(&[NSString::from_str("en-US")]));
    request.setUsesLanguageCorrection(true);
    if !names.is_empty() {
        let list: Vec<Retained<NSString>> = names.iter().map(|n| NSString::from_str(n)).collect();
        request.setCustomWords(&NSArray::from_retained_slice(&list));
    }
    // (text as small as a 720p stream's subtitles in a tall crop: below Vision's default of 1/32 of the height)
    request.setMinimumTextHeight(0.02);
    let options: Retained<NSDictionary<VNImageOption, AnyObject>> = NSDictionary::new();
    let handler = unsafe { VNImageRequestHandler::initWithCGImage_options(VNImageRequestHandler::alloc(), &image, &options) };
    let as_request: &VNRequest = request.as_ref();
    handler.performRequests_error(&NSArray::from_slice(&[as_request])).map_err(|e| anyhow!("OCR failed: {e}"))?;

    let mut words = Vec::new();
    let Some(results) = request.results() else { return Ok(words) };
    for (line_no, obs) in results.iter().enumerate() {
        let Some(best) = obs.topCandidates(1).firstObject() else { continue };
        let text = best.string().to_string();
        // normalised box, origin at the bottom left
        let b = unsafe { obs.boundingBox() };
        let (lx, ly, lw, lh) = (
            b.origin.x * w as f64,
            (1.0 - b.origin.y - b.size.height) * h as f64,
            b.size.width * w as f64,
            b.size.height * h as f64,
        );
        let total = text.chars().count().max(1) as f64;
        let mut pos = 0usize;
        // (where each word starts in UTF-16 units, as Vision counts)
        let mut utf16_at = 0usize;
        for part in text.split(' ') {
            let len = part.chars().count();
            let len16 = part.encode_utf16().count();
            if !part.is_empty() {
                // the word's own box from Vision; else the line's width shared out by characters
                let own = unsafe { best.boundingBoxForRange_error(NSRange::new(utf16_at, len16)) }.ok().map(|r| unsafe { r.boundingBox() });
                let (x, y, ww, hh) = match own.filter(|r| r.size.width > 0.0 && r.size.height > 0.0) {
                    Some(r) => (r.origin.x * w as f64, (1.0 - r.origin.y - r.size.height) * h as f64, r.size.width * w as f64, r.size.height * h as f64),
                    None => (lx + lw * pos as f64 / total, ly, lw * len as f64 / total, lh),
                };
                words.push(RawWord { line: line_no as u32, text: part.to_string(), x, y, w: ww, h: hh });
            }
            pos += len + 1;
            utf16_at += len16 + 1;
        }
    }
    Ok(words)
}
