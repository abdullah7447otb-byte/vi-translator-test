//! HEVC through VideoToolbox (the Mac): a clip made here with ffmpeg (Annex B, a key frame a second as the
//! PS5 sends), split into pictures the way chiaki hands them over, must decode picture for picture at its
//! size. Elsewhere, and where ffmpeg can't make HEVC, it says so and passes.

#[cfg(target_os = "macos")]
fn units(data: &[u8]) -> Vec<(usize, usize)> {
    // (start, end) of each NAL unit's bytes after its start code
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    starts.iter().enumerate().map(|(k, &s)| (s, starts.get(k + 1).map_or(data.len(), |n| n - 3))).collect()
}

/// The stream cut into pictures: a picture starts at the first non-picture unit (parameter sets, SEI, a
/// delimiter) after a picture's slices, or at a slice that starts a picture (first_slice_segment_in_pic_flag)
/// right after another picture's slices.
#[cfg(target_os = "macos")]
fn pictures(data: &[u8]) -> Vec<Vec<u8>> {
    let mut out: Vec<Vec<u8>> = Vec::new();
    let mut cur: Vec<u8> = Vec::new();
    // the last unit was a slice (a picture's data)
    let mut after_slice = false;
    for (s, e) in units(data) {
        if s >= e {
            continue;
        }
        let t = (data[s] >> 1) & 0x3f;
        let vcl = t < 32;
        let first_slice = vcl && e > s + 2 && data[s + 2] & 0x80 != 0;
        if after_slice && (!vcl || first_slice) && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
        after_slice = vcl;
        cur.extend_from_slice(&[0, 0, 0, 1]);
        cur.extend_from_slice(&data[s..e]);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

#[test]
fn hevc_decodes_on_the_mac() {
    #[cfg(not(target_os = "macos"))]
    {
        println!("HEVC decoding: the Mac only (skipped here)");
    }
    #[cfg(target_os = "macos")]
    {
        let dir = std::env::temp_dir().join(format!("vit-hevc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let out = dir.join("clip.hevc");
        let mut made = false;
        let encoders: [&[&str]; 2] = [&["-c:v", "libx265", "-x265-params", "keyint=60:min-keyint=60:log-level=error"], &["-c:v", "hevc_videotoolbox", "-g", "60", "-b:v", "8M"]];
        for enc in encoders {
            let ok = std::process::Command::new("ffmpeg")
                .args(["-y", "-loglevel", "error", "-f", "lavfi", "-i", "testsrc2=size=1280x720:rate=60", "-t", "2", "-pix_fmt", "yuv420p"])
                .args(enc)
                .args(["-f", "hevc"])
                .arg(&out)
                .status()
                .is_ok_and(|s| s.success());
            if ok {
                made = true;
                break;
            }
        }
        if !made {
            println!("HEVC decoding: ffmpeg couldn't make an HEVC clip here (skipped)");
            return;
        }
        let data = std::fs::read(&out).expect("clip");
        let pics = pictures(&data);
        assert!(pics.len() >= 100, "the clip cut into {} pictures", pics.len());
        let mut d = vi_translator_ps::hwdec::VideoDecoder::system_for_test_of(true).expect("VideoToolbox for HEVC");
        assert!(d.is_hevc());
        let mut decoded = 0;
        for p in &pics {
            let key = p.windows(4).any(|w| w[..3] == [0, 0, 1] && (32..=34).contains(&((w[3] >> 1) & 0x3f)));
            if let Some(f) = d.decode(p, key).unwrap_or_else(|e| panic!("{}: {e}", d.name())) {
                assert_eq!((f.w, f.h), (1280, 720));
                decoded += 1;
            }
        }
        println!("HEVC decoding: {decoded} of {} pictures by {}", pics.len(), d.name());
        assert!(decoded * 10 >= pics.len() * 9, "only {decoded} of {} pictures decoded", pics.len());
    }
}
