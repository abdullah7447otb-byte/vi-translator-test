//! Auto-update: "Version X is available — Update now".
//!
//! On start (after a short wait) and every hour, the app asks GitHub for the latest release of
//! the app. If it is newer than this build, the window shows a banner. "Update now" downloads this
//! platform's executable and its signature, checks the signature against the public key built in
//! below (the private key exists only as a secret of the release workflow), puts the new
//! executable in place of the running one (the old one is moved aside as "<name>.old" and removed
//! on the next start) and restarts. Anything that doesn't check out leaves the app as it is.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::config::Config;

/// Where releases are published.
const RELEASES: &str = "https://api.github.com/repos/vi-translator/vi-translator-desktop/releases/latest";
/// Ed25519 public key that release files are signed with, and the check (in the core: page
/// scripts are signed with the same key).
pub(crate) use vi_translator_core::sign::{PUBLIC_KEY, verify};
const CHECK_EVERY: Duration = Duration::from_secs(60 * 60);

/// The release file for this platform: vi-translator-<windows|linux|macos>-<x64|arm64>[.exe]
pub fn asset_name() -> String {
    let os = if cfg!(windows) { "windows" } else if cfg!(target_os = "macos") { "macos" } else { "linux" };
    let arch = if cfg!(target_arch = "aarch64") { "arm64" } else { "x64" };
    format!("vi-translator-{os}-{arch}{}", if cfg!(windows) { ".exe" } else { "" })
}

/// "1.2.3" or "v1.2.3" -> (1, 2, 3)
pub fn parse_version(v: &str) -> Option<(u64, u64, u64)> {
    let mut it = v.trim().trim_start_matches('v').split('.').map(|p| p.parse::<u64>().ok());
    let r = (it.next()??, it.next()??, it.next()??);
    it.next().is_none().then_some(r)
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Update {
    pub version: String,
    pub notes: String,
    /// The release's page (where VI Translator.app's new .dmg is).
    pub page: String,
    /// Running as VI Translator.app: the update is the new .dmg, from `page` (`in_app_bundle`).
    pub app_bundle: bool,
    #[serde(skip)]
    pub url: String,
    #[serde(skip)]
    pub sig_url: String,
}

/// The newest release, if it is newer than `current` and has a file (and signature) for this platform.
pub fn newer_release(json: &serde_json::Value, current: &str) -> Option<Update> {
    let tag = json["tag_name"].as_str()?;
    if json["draft"].as_bool() == Some(true) || json["prerelease"].as_bool() == Some(true) {
        return None;
    }
    if parse_version(tag)? <= parse_version(current)? {
        return None;
    }
    let assets = json["assets"].as_array()?;
    let url_of = |name: &str| {
        assets.iter().find(|a| a["name"].as_str() == Some(name)).and_then(|a| a["browser_download_url"].as_str()).map(String::from)
    };
    let name = asset_name();
    Some(Update {
        version: tag.trim_start_matches('v').to_string(),
        notes: json["body"].as_str().unwrap_or("").chars().take(2000).collect(),
        page: json["html_url"].as_str().unwrap_or(RELEASES_PAGE).to_string(),
        app_bundle: in_app_bundle(),
        url: url_of(&name)?,
        sig_url: url_of(&format!("{name}.sig"))?,
    })
}

/// What a release file's signature (`.sig`) signs: what it's for, the file's name (its system), the
/// version and the file's SHA-256. So a signed file can't pass for another version (an older one)
/// or another system's, and nothing else signed with the same key (a page script) for an update.
pub fn signed_text(asset: &str, version: &str, data: &[u8]) -> String {
    use sha2::Digest as _;
    format!("vi-translator-update-v1\n{asset}\n{version}\n{:x}\n", sha2::Sha256::digest(data))
}

fn http() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .user_agent(concat!("vi-translator/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(300))
        .build()
        .expect("http client")
}

/// The latest release, if newer than this build.
pub fn check() -> Result<Option<Update>> {
    let r = http().get(RELEASES).header("Accept", "application/vnd.github+json").timeout(Duration::from_secs(20)).send()?;
    if r.status().as_u16() == 404 {
        return Ok(None); // nothing released yet
    }
    let json: serde_json::Value = r.error_for_status()?.json()?;
    Ok(newer_release(&json, env!("CARGO_PKG_VERSION")))
}

/// The releases, for a release without its own page.
const RELEASES_PAGE: &str = "https://github.com/vi-translator/vi-translator-desktop/releases/latest";

/// Running as VI Translator.app (the macOS .dmg's), not as the bare program. The app's program
/// can't be swapped for a release file: the app is signed as a whole (a new program, or the old one
/// left beside it, breaks its signature) and runs on both kinds of Mac (the release file is for
/// one). Its update is the new .dmg.
pub fn in_app_bundle() -> bool {
    std::env::current_exe().ok().is_some_and(|p| is_app_bundle_exe(&p))
}

fn is_app_bundle_exe(exe: &Path) -> bool {
    exe.to_str().is_some_and(|s| s.contains(".app/Contents/MacOS/"))
}

/// Download, verify, swap the executable. Returns the path of the new executable (to restart).
pub fn install(u: &Update) -> Result<PathBuf> {
    if in_app_bundle() {
        bail!("VI Translator.app is updated with its new .dmg, from the release's page");
    }
    let c = http();
    let data = c.get(&u.url).send()?.error_for_status()?.bytes().context("download")?;
    let sig = c.get(&u.sig_url).send()?.error_for_status()?.text().context("download signature")?;
    verify(signed_text(&asset_name(), &u.version, &data).as_bytes(), &sig, PUBLIC_KEY)
        .context("the downloaded update is not signed by VI Translator for this version and system")?;
    let exe = std::env::current_exe()?;
    replace_exe(&exe, &data)?;
    Ok(exe)
}

/// Put `data` in place of the executable at `exe`: the running file is renamed (allowed on
/// Windows while it runs) and the new one written under the original name.
fn replace_exe(exe: &Path, data: &[u8]) -> Result<()> {
    let old = old_path(exe);
    let _ = std::fs::remove_file(&old);
    let tmp = exe.with_extension("new");
    std::fs::write(&tmp, data).context("write the new version")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
    }
    std::fs::rename(exe, &old).context("move the old version aside")?;
    if let Err(e) = std::fs::rename(&tmp, exe) {
        let _ = std::fs::rename(&old, exe); // put the old one back
        bail!("put the new version in place: {e}");
    }
    Ok(())
}

fn old_path(exe: &Path) -> PathBuf {
    let mut p = exe.as_os_str().to_owned();
    p.push(".old");
    PathBuf::from(p)
}

/// Remove what the last update left behind.
pub fn clean_up() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::fs::remove_file(old_path(&exe));
        let _ = std::fs::remove_file(exe.with_extension("new"));
    }
}

/// Background check: on start (after a minute) and every hour; tells the window when an update exists.
/// A test build (built with VIT_TEST_BUILD set, as test-build.yml does): it never looks for the
/// official version, which would replace it.
pub const TEST_BUILD: bool = option_env!("VIT_TEST_BUILD").is_some();

pub fn run(app: AppHandle, cfg: Arc<Mutex<Config>>, found: Arc<Mutex<Option<Update>>>) {
    clean_up();
    if TEST_BUILD {
        vi_translator_core::vlog!("[update] off: this is a test build");
        return;
    }
    std::thread::sleep(Duration::from_secs(60));
    loop {
        if cfg.lock().unwrap_or_else(std::sync::PoisonError::into_inner).update_check {
            if let Ok(Some(u)) = check() {
                *found.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(u.clone());
                let _ = app.emit("update-available", &u);
            }
        }
        std::thread::sleep(CHECK_EVERY);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn knows_the_mac_app_from_the_bare_program() {
        use std::path::Path;
        assert!(super::is_app_bundle_exe(Path::new("/Applications/VI Translator.app/Contents/MacOS/vi-translator")));
        assert!(!super::is_app_bundle_exe(Path::new("/Users/me/Downloads/vi-translator-macos-arm64")));
        assert!(!super::is_app_bundle_exe(Path::new(r"C:\Users\me\vi-translator-windows-x64.exe")));
    }

    #[test]
    fn versions() {
        assert_eq!(parse_version("v1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_version("0.10.0"), Some((0, 10, 0)));
        assert!(parse_version("v1.2").is_none() && parse_version("1.2.3-beta").is_none() && parse_version("1.2.3.4").is_none());
        assert!(parse_version("0.10.0") > parse_version("0.9.9"));
    }

    fn release(tag: &str, with_sig: bool) -> serde_json::Value {
        let name = asset_name();
        let mut assets = vec![serde_json::json!({ "name": name, "browser_download_url": "https://x/bin" })];
        if with_sig {
            assets.push(serde_json::json!({ "name": format!("{name}.sig"), "browser_download_url": "https://x/sig" }));
        }
        serde_json::json!({ "tag_name": tag, "draft": false, "prerelease": false, "body": "notes", "assets": assets })
    }

    #[test]
    fn only_newer_signed_releases() {
        let u = newer_release(&release("v0.2.0", true), "0.1.0").expect("newer");
        assert_eq!((u.version.as_str(), u.url.as_str(), u.sig_url.as_str()), ("0.2.0", "https://x/bin", "https://x/sig"));
        assert!(newer_release(&release("v0.1.0", true), "0.1.0").is_none(), "same version");
        assert!(newer_release(&release("v0.0.9", true), "0.1.0").is_none(), "older");
        assert!(newer_release(&release("v0.2.0", false), "0.1.0").is_none(), "no signature: no update");
        let mut pre = release("v0.2.0", true);
        pre["prerelease"] = serde_json::json!(true);
        assert!(newer_release(&pre, "0.1.0").is_none(), "pre-release");
    }

    #[test]
    fn signatures() {
        use base64::Engine;
        use ed25519_dalek::{Signer, SigningKey};
        let b64 = base64::engine::general_purpose::STANDARD;
        let key = SigningKey::from_bytes(&[7u8; 32]);
        let public = b64.encode(key.verifying_key().to_bytes());
        let data = b"new version";
        let sig = b64.encode(key.sign(data).to_bytes());
        assert!(verify(data, &sig, &public).is_ok());
        assert!(verify(b"tampered", &sig, &public).is_err(), "changed file");
        assert!(verify(data, &sig, PUBLIC_KEY).is_err(), "signed with another key");
        assert!(verify(data, "not base64!", &public).is_err());
    }

    #[test]
    fn swaps_the_executable() {
        let dir = std::env::temp_dir().join(format!("vit-upd-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("vi-translator.exe");
        std::fs::write(&exe, b"old").unwrap();
        replace_exe(&exe, b"new").unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"new");
        assert_eq!(std::fs::read(old_path(&exe)).unwrap(), b"old");
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[cfg(test)]
mod openssl_signature {
    /// A file signed the way the release workflow signs (`signed_text` of it, with openssl pkeyutl
    /// -sign -rawin, base64): set VIT_SIGNED_FILE to its path (its signature is next to it,
    /// "<path>.sig", and the file keeps its release name) and VIT_SIGNED_VERSION to its version.
    #[test]
    #[ignore]
    fn verifies_with_the_built_in_key() {
        // VIT_SIGNED_FILE: a release file (its .sig next to it); VIT_SIGNED_VERSION: its version
        let path = std::env::var("VIT_SIGNED_FILE").expect("VIT_SIGNED_FILE");
        let version = std::env::var("VIT_SIGNED_VERSION").expect("VIT_SIGNED_VERSION");
        let name = std::path::Path::new(&path).file_name().unwrap().to_string_lossy().to_string();
        let data = std::fs::read(&path).unwrap();
        let sig = std::fs::read_to_string(format!("{path}.sig")).unwrap();
        super::verify(super::signed_text(&name, &version, &data).as_bytes(), &sig, super::PUBLIC_KEY).unwrap();
        let mut bad = data.clone();
        bad[0] ^= 1;
        assert!(super::verify(super::signed_text(&name, &version, &bad).as_bytes(), &sig, super::PUBLIC_KEY).is_err());
    }

    #[test]
    fn a_signature_is_for_one_file_version_and_system() {
        use base64::Engine as _;
        use ed25519_dalek::Signer as _;
        let b64 = base64::engine::general_purpose::STANDARD;
        let key = ed25519_dalek::SigningKey::from_bytes(&[7u8; 32]);
        let public = b64.encode(key.verifying_key().to_bytes());
        let data = b"the new version".to_vec();
        let sig = b64.encode(key.sign(super::signed_text("vi-translator-windows-x64.exe", "1.2.0", &data).as_bytes()).to_bytes());
        let ok = |asset: &str, version: &str, data: &[u8]| super::verify(super::signed_text(asset, version, data).as_bytes(), &sig, &public).is_ok();
        assert!(ok("vi-translator-windows-x64.exe", "1.2.0", &data));
        assert!(!ok("vi-translator-linux-x64", "1.2.0", &data), "another system's file");
        assert!(!ok("vi-translator-windows-x64.exe", "1.1.0", &data), "another version");
        assert!(!ok("vi-translator-windows-x64.exe", "1.2.0", b"something else"), "another file");
        // a signature of the bare file (what a page script's is) doesn't do
        let raw = b64.encode(key.sign(&data).to_bytes());
        assert!(super::verify(super::signed_text("vi-translator-windows-x64.exe", "1.2.0", &data).as_bytes(), &raw, &public).is_err());
    }
}
