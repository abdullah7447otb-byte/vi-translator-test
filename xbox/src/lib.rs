//! Xbox Remote Play ("xHome") for VI Translator, without Microsoft's web player: the steps
//! xbox.com's player takes, as the open-source XStreaming and Greenlight players document them, and
//! as VI Translator's phone apps do them.
//!
//! - [`auth`]: Microsoft's device code sign-in (a short code typed at microsoft.com/link on any
//!   device; no password page in the app), and the Xbox tokens made from it.
//! - [`home`]: the account's consoles and a stream session's steps on Microsoft's servers (start,
//!   wait for the console, the WebRTC offer and network candidates, keep-alive, stop).
//! - [`input`]: the data channels' messages and the input channel's binary packets.
//! - [`play`]: the stream itself (WebRTC, webrtc-rs): the picture decoded (OpenH264), the sound on the
//!   speakers and this computer's controllers sent to the console (SDL).
//!
//! Tokens are never logged; the caller keeps only Microsoft's refresh token.

pub mod auth;
pub mod home;
pub mod input;
pub mod play;

/// What went wrong talking to Microsoft or the console.
#[derive(Debug, Clone, PartialEq)]
pub enum Error {
    /// Microsoft no longer takes the saved sign-in: sign in again.
    SignedOut,
    /// An HTTP answer that wasn't 2xx: its status, the host, and its body (shown shortened).
    Http { code: u16, host: String, body: String },
    Other(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::SignedOut => write!(f, "signed out of Microsoft"),
            Error::Http { code, host, body } => write!(f, "HTTP {code} from {host}: {}", body.chars().take(300).collect::<String>()),
            Error::Other(s) => f.write_str(s),
        }
    }
}

impl std::error::Error for Error {}

impl From<reqwest::Error> for Error {
    fn from(e: reqwest::Error) -> Self {
        // (without the address: reqwest's text names the URL, never a token, but keep it short)
        Error::Other(format!("network: {}", e.without_url()))
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Other(format!("unexpected answer: {e}"))
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// One HTTP client for Microsoft's services.
pub(crate) fn client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .connect_timeout(std::time::Duration::from_secs(15))
        .build()?)
}

/// Sends `r`; the body as text if 2xx, else [`Error::Http`].
pub(crate) async fn send(r: reqwest::RequestBuilder) -> Result<String> {
    let r = r.header("Accept", "application/json").send().await?;
    let code = r.status().as_u16();
    let host = r.url().host_str().unwrap_or_default().to_string();
    let body = r.text().await?;
    if !(200..300).contains(&code) {
        // (the whole body: an OAuth error is read from it; shown shortened)
        return Err(Error::Http { code, host, body });
    }
    Ok(body)
}

/// `application/x-www-form-urlencoded` of `pairs`.
pub(crate) fn form(pairs: &[(&str, &str)]) -> String {
    let enc = |s: &str| {
        let mut o = String::new();
        for b in s.bytes() {
            if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                o.push(b as char);
            } else {
                o.push_str(&format!("%{b:02X}"));
            }
        }
        o
    };
    pairs.iter().map(|(k, v)| format!("{}={}", enc(k), enc(v))).collect::<Vec<_>>().join("&")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forms_are_escaped() {
        assert_eq!(form(&[("a", "x y"), ("scope", "service::http://P")]), "a=x%20y&scope=service%3A%3Ahttp%3A%2F%2FP");
    }
}
