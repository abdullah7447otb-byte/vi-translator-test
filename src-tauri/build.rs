fn main() {
    // macOS: window capture with ScreenCaptureKit (native/vit_sck.m), linked weakly so the app
    // still opens on a macOS older than 12.3 (it captures the old way there)
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        cc::Build::new().file("native/vit_sck.m").flag("-fobjc-arc").flag("-Wno-unguarded-availability-new").compile("vit_sck");
        println!("cargo:rustc-link-arg=-Wl,-weak_framework,ScreenCaptureKit");
        for f in ["AppKit", "CoreMedia", "CoreVideo", "Foundation"] {
            println!("cargo:rustc-link-lib=framework={f}");
        }
        println!("cargo:rerun-if-changed=native/vit_sck.m");
    }
    tauri_build::build()
}
