//! Builds chiaki-ng's library (third-party/chiaki-ng, AGPL-3.0) with the C compiler alone: no CMake,
//! protoc or Python. Its protobuf code is generated once (ps/generated, from nanopb's generator), its
//! crypto is Mbed TLS (third-party/mbedtls, built here too), and Remote Play over the internet is
//! left out (ps/native/stubs.c), with the curl, json-c and miniupnpc it would need.

use std::path::{Path, PathBuf};

fn main() {
    let root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let third = root.join("../third-party");
    let chiaki = third.join("chiaki-ng");
    let lib = chiaki.join("lib");
    let mbedtls = third.join("mbedtls");
    if !lib.join("src/session.c").exists() || !mbedtls.join("library/aes.c").exists() {
        panic!("third-party/chiaki-ng or third-party/mbedtls is missing: git submodule update --init --recursive");
    }
    let windows = std::env::var("CARGO_CFG_TARGET_OS").unwrap() == "windows";
    let msvc = std::env::var("CARGO_CFG_TARGET_ENV").unwrap() == "msvc";

    // Windows: clang-cl, as chiaki-ng's own Windows build (MSVC's compiler has no variable-length arrays)
    let clang_cl = msvc.then(find_clang_cl);
    let base = |b: &mut cc::Build| {
        b.warnings(false).opt_level(2);
        if let Some(cl) = &clang_cl {
            b.compiler(cl);
            b.flag("/std:c11").define("_CRT_SECURE_NO_WARNINGS", None).define("WIN32_LEAN_AND_MEAN", None);
        } else {
            b.flag_if_supported("-std=gnu11");
        }
    };

    // Mbed TLS (its crypto only is used)
    let mut b = cc::Build::new();
    base(&mut b);
    b.include(mbedtls.join("include"));
    for f in c_files(&mbedtls.join("library")) {
        b.file(f);
    }
    b.cargo_metadata(false).compile("vit_mbedtls");

    // Reed-Solomon for the video's error correction: gf-complete and jerasure
    let gf = chiaki.join("third-party/gf-complete");
    let je = chiaki.join("third-party/jerasure");
    let mut b = cc::Build::new();
    base(&mut b);
    b.include(gf.join("include")).include(je.join("include"));
    for f in ["gf.c", "gf_wgen.c", "gf_w4.c", "gf_w8.c", "gf_w16.c", "gf_w32.c", "gf_w64.c", "gf_w128.c", "gf_rand.c", "gf_general.c", "gf_cpu.c"] {
        b.file(gf.join("src").join(f));
    }
    for f in ["galois.c", "jerasure.c", "reed_sol.c", "cauchy.c", "liberation.c"] {
        b.file(je.join("src").join(f));
    }
    if msvc {
        // (gf_cpu.c's cpuid needs intrin.h with MSVC: chiaki-ng's scripts/windows-vc/gf-complete.patch)
        b.flag("/FIintrin.h");
    }
    b.cargo_metadata(false).compile("vit_jerasure");

    // chiaki-ng's library, its protobuf messages (nanopb) and VI Translator's C face of it
    let nanopb = chiaki.join("third-party/nanopb");
    let mut b = cc::Build::new();
    base(&mut b);
    b.include(root.join("native/include")) // config.h and the curl stand-in, before chiaki's own
        .include(lib.join("include"))
        .include(root.join("generated"))
        .include(&nanopb)
        .include(gf.join("include"))
        .include(je.join("include"))
        .include(mbedtls.join("include"))
        .define("CHIAKI_LIB_ENABLE_MBEDTLS", None)
        .define("PB_C99_STATIC_ASSERT", None);
    if !windows {
        b.define("_GNU_SOURCE", None);
    }
    // everything but Remote Play over the internet (holepunch, AIA) and the decoders the app doesn't use
    // (and takion.c: Barqawi's copy is built instead, native/takion.c — a bigger receive buffer on a Mac)
    let leave_out = ["aia.c", "aia_openssl.c", "holepunch.c", "holepunch-test.c", "ffmpegdecoder.c", "pidecoder.c", "takion.c"];
    for f in c_files(&lib.join("src")).into_iter().chain(c_files(&lib.join("src/remote"))) {
        if !leave_out.contains(&f.file_name().unwrap().to_str().unwrap()) {
            b.file(f);
        }
    }
    for f in ["pb_common.c", "pb_encode.c", "pb_decode.c"] {
        b.file(nanopb.join(f));
    }
    b.file(root.join("generated/takion.pb.c"));
    b.file(root.join("native/stubs.c")).file(root.join("native/vit_ps.c")).file(root.join("native/takion.c"));
    b.cargo_metadata(false).compile("vit_chiaki");

    // linked users first (GNU ld reads static libraries once, in order: chiaki needs jerasure and Mbed TLS)
    println!("cargo:rustc-link-search=native={}", std::env::var("OUT_DIR").unwrap());
    for l in ["vit_chiaki", "vit_jerasure", "vit_mbedtls"] {
        println!("cargo:rustc-link-lib=static={l}");
    }
    if windows {
        for l in ["ws2_32", "wsock32", "bcrypt", "iphlpapi", "advapi32"] {
            println!("cargo:rustc-link-lib={l}");
        }
    }
    // the computer's own video decoder: VideoToolbox on a Mac, Media Foundation on Windows
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap();
    if os == "macos" || windows {
        let mut b = cc::Build::new();
        base(&mut b);
        b.file(root.join(if windows { "native/vit_mf.c" } else { "native/vit_vt.c" }));
        b.cargo_metadata(false).compile("vit_hwdec");
        println!("cargo:rustc-link-lib=static=vit_hwdec");
    }
    // motion smoothing on a Mac: VideoToolbox's frame interpolation, Objective-C, looked up at run time
    if os == "macos" {
        let mut b = cc::Build::new();
        base(&mut b);
        b.file(root.join("native/vit_fi.m")).flag("-fobjc-arc");
        b.cargo_metadata(false).compile("vit_fi");
        println!("cargo:rustc-link-lib=static=vit_fi");
    }
    if windows {
        for l in ["mfplat", "mfuuid", "d3d11", "dxguid", "ole32"] {
            println!("cargo:rustc-link-lib={l}");
        }
    }
    if os == "macos" {
        for f in ["CoreServices", "VideoToolbox", "CoreMedia", "CoreVideo", "CoreFoundation", "Foundation"] {
            println!("cargo:rustc-link-lib=framework={f}");
        }
        println!("cargo:rustc-link-lib=objc");
    }
    println!("cargo:rerun-if-changed=native");
    println!("cargo:rerun-if-changed=generated");
    println!("cargo:rerun-if-changed=build.rs");
}

/// clang-cl: $CC if it is one, else on the PATH, else Visual Studio's own (its "C++ Clang tools"
/// part), else LLVM's installer's.
fn find_clang_cl() -> PathBuf {
    println!("cargo:rerun-if-env-changed=VIT_CLANG_CL");
    if let Some(p) = std::env::var_os("VIT_CLANG_CL") {
        return p.into();
    }
    let exe = "clang-cl.exe";
    if let Some(p) = std::env::var_os("PATH").and_then(|path| std::env::split_paths(&path).map(|d| d.join(exe)).find(|p| p.exists())) {
        return p;
    }
    let host = if cfg!(target_arch = "aarch64") { "ARM64" } else { "x64" };
    for pf in ["ProgramFiles", "ProgramFiles(x86)"].iter().filter_map(std::env::var_os) {
        let vs = Path::new(&pf).join("Microsoft Visual Studio");
        for year in std::fs::read_dir(&vs).into_iter().flatten().flatten() {
            for edition in std::fs::read_dir(year.path()).into_iter().flatten().flatten() {
                let p = edition.path().join("VC/Tools/Llvm").join(host).join("bin").join(exe);
                if p.exists() {
                    return p;
                }
            }
        }
        let p = Path::new(&pf).join("LLVM/bin").join(exe);
        if p.exists() {
            return p;
        }
    }
    panic!("clang-cl not found: install Visual Studio's \"C++ Clang tools for Windows\" or LLVM, or set VIT_CLANG_CL");
}

/// The .c files in a folder (not its subfolders), in a fixed order.
fn c_files(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "c"))
        .collect();
    v.sort();
    v
}
