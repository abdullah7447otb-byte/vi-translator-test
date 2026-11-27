//! Signing in to Xbox with Microsoft's device code: the app shows a short code, the player types it
//! at microsoft.com/link on any device and signs in there (xbox.com's own web client, as XStreaming,
//! Greenlight and the phone apps do). From Microsoft's refresh token come, when a stream starts, the
//! Xbox Live tokens Remote Play takes.

use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::json;
use tokio::sync::Mutex;

use crate::{Error, Result, client, form, send};

/// xbox.com's web client
const CLIENT_ID: &str = "1f907974-e22b-4810-a9de-d9647380c97e";
const SCOPE: &str = "xboxlive.signin openid profile offline_access";
const AAD: &str = "https://login.microsoftonline.com/consumers/oauth2/v2.0";

/// A code for the player to type, and how to wait for it.
#[derive(Debug, Clone)]
pub struct DeviceCode {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    /// seconds between asks
    pub interval: u64,
    pub expires_at: Instant,
}

#[derive(Deserialize)]
struct CodeAnswer {
    device_code: String,
    user_code: String,
    #[serde(default)]
    verification_uri: Option<String>,
    #[serde(default)]
    interval: Option<u64>,
    #[serde(default)]
    expires_in: Option<u64>,
}

/// A new code to type.
pub async fn request_code() -> Result<DeviceCode> {
    let body = send(
        client()?
            .post(format!("{AAD}/devicecode"))
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(form(&[("client_id", CLIENT_ID), ("scope", SCOPE)])),
    )
    .await?;
    let a: CodeAnswer = serde_json::from_str(&body)?;
    Ok(DeviceCode {
        device_code: a.device_code,
        user_code: a.user_code,
        verification_uri: a.verification_uri.unwrap_or_else(|| "https://www.microsoft.com/link".into()),
        interval: a.interval.unwrap_or(5).clamp(1, 30),
        expires_at: Instant::now() + Duration::from_secs(a.expires_in.unwrap_or(900)),
    })
}

/// Microsoft's tokens after a sign-in or a refresh.
#[derive(Clone)]
pub struct Tokens {
    pub access: String,
    pub refresh: String,
    pub expires_at: Instant,
}

impl std::fmt::Debug for Tokens {
    // (never printed: tokens stay out of logs)
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Tokens(…)")
    }
}

#[derive(Deserialize)]
struct TokenAnswer {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
}

impl TokenAnswer {
    fn tokens(self, old_refresh: &str) -> Tokens {
        Tokens {
            access: self.access_token,
            refresh: self.refresh_token.filter(|r| !r.is_empty()).unwrap_or_else(|| old_refresh.to_string()),
            expires_at: Instant::now() + Duration::from_secs(self.expires_in.unwrap_or(3600)),
        }
    }
}

/// What an ask for the code's sign-in said.
#[derive(Debug)]
pub enum Poll {
    /// not typed yet
    Pending,
    /// ask less often
    SlowDown,
    Done(Tokens),
    /// expired, declined, or refused: the reason
    Failed(String),
}

/// Has the player signed in with `dc` yet? (One ask; the caller waits `interval` between asks.)
pub async fn poll_code(dc: &DeviceCode) -> Result<Poll> {
    if Instant::now() >= dc.expires_at {
        return Ok(Poll::Failed("expired_token".into()));
    }
    let r = send(
        client()?
            .post(format!("{AAD}/token"))
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("client_id", CLIENT_ID),
                ("device_code", &dc.device_code),
            ])),
    )
    .await;
    match r {
        Ok(body) => Ok(Poll::Done(serde_json::from_str::<TokenAnswer>(&body)?.tokens(""))),
        Err(Error::Http { body, .. }) => Ok(poll_error(&body)),
        Err(e) => Err(e),
    }
}

/// The OAuth error in a refused device code ask (RFC 8628).
pub fn poll_error(body: &str) -> Poll {
    let err = serde_json::from_str::<serde_json::Value>(body).ok().and_then(|v| v["error"].as_str().map(str::to_string)).unwrap_or_default();
    match err.as_str() {
        "authorization_pending" => Poll::Pending,
        "slow_down" => Poll::SlowDown,
        "" => Poll::Failed("unknown".into()),
        _ => Poll::Failed(err),
    }
}

/// A signed-in Microsoft account: its refresh token (kept by the app: `on_refresh` is told every new
/// one) and the tokens made from it while the app runs.
pub struct Account {
    tokens: Mutex<Tokens>,
    on_refresh: Box<dyn Fn(&str) + Send + Sync>,
}

/// Remote Play's own token and its regions' addresses (the default first).
#[derive(Clone)]
pub struct Login {
    pub gs_token: String,
    pub base_uris: Vec<String>,
}

impl std::fmt::Debug for Login {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Login({:?})", self.base_uris)
    }
}

impl Account {
    /// The account as saved (its refresh token), or just signed in (`tokens`).
    pub fn new(refresh: &str, on_refresh: impl Fn(&str) + Send + Sync + 'static) -> Self {
        Self::with_tokens(Tokens { access: String::new(), refresh: refresh.to_string(), expires_at: Instant::now() }, on_refresh)
    }

    pub fn with_tokens(tokens: Tokens, on_refresh: impl Fn(&str) + Send + Sync + 'static) -> Self {
        Account { tokens: Mutex::new(tokens), on_refresh: Box::new(on_refresh) }
    }

    /// A Microsoft access token with over a minute left (refreshed if not).
    pub async fn access(&self) -> Result<String> {
        let mut t = self.tokens.lock().await;
        if !t.access.is_empty() && t.expires_at > Instant::now() + Duration::from_secs(60) {
            return Ok(t.access.clone());
        }
        if t.refresh.is_empty() {
            return Err(Error::SignedOut);
        }
        let r = send(
            client()?
                .post(format!("{AAD}/token"))
                .header("Content-Type", "application/x-www-form-urlencoded")
                .header("Cache-Control", "no-store, must-revalidate, no-cache")
                .body(form(&[("client_id", CLIENT_ID), ("grant_type", "refresh_token"), ("refresh_token", &t.refresh), ("scope", SCOPE)])),
        )
        .await;
        let body = match r {
            Ok(b) => b,
            // (the refresh token no longer works)
            Err(Error::Http { code: 400 | 401, .. }) => return Err(Error::SignedOut),
            Err(e) => return Err(e),
        };
        let old = t.refresh.clone();
        *t = serde_json::from_str::<TokenAnswer>(&body)?.tokens(&old);
        if t.refresh != old {
            (self.on_refresh)(&t.refresh);
        }
        Ok(t.access.clone())
    }

    /// The "transfer token" the console's /connect step asks for (login.live.com, from the same
    /// refresh token).
    pub async fn transfer_token(&self) -> Result<String> {
        self.access().await?;
        let refresh = self.tokens.lock().await.refresh.clone();
        let body = send(
            client()?.post("https://login.live.com/oauth20_token.srf").header("Content-Type", "application/x-www-form-urlencoded").body(form(&[
                ("client_id", CLIENT_ID),
                ("scope", "service::http://Passport.NET/purpose::PURPOSE_XBOX_CLOUD_CONSOLE_TRANSFER_TOKEN"),
                ("grant_type", "refresh_token"),
                ("refresh_token", &refresh),
            ])),
        )
        .await?;
        Ok(serde_json::from_str::<TokenAnswer>(&body)?.access_token)
    }

    /// An Xbox Live token for `relying_party`, the user hash that goes with it, and the gamertag.
    pub async fn xsts(&self, relying_party: &str) -> Result<(String, String, String)> {
        let access = self.access().await?;
        let http = client()?;
        let xbl = |r: reqwest::RequestBuilder| {
            r.header("x-xbl-contract-version", "1")
                .header("Cache-Control", "no-cache")
                .header("Content-Type", "application/json")
                .header("Origin", "https://www.xbox.com")
                .header("Referer", "https://www.xbox.com/")
        };
        let user = json!({
            "Properties": { "AuthMethod": "RPS", "RpsTicket": format!("d={access}"), "SiteName": "user.auth.xboxlive.com" },
            "RelyingParty": "http://auth.xboxlive.com", "TokenType": "JWT",
        });
        let u: serde_json::Value = serde_json::from_str(&send(xbl(http.post("https://user.auth.xboxlive.com/user/authenticate")).body(user.to_string())).await?)?;
        let user_token = u["Token"].as_str().ok_or_else(|| Error::Other("Xbox Live gave no user token".into()))?;
        let x = json!({
            "Properties": { "SandboxId": "RETAIL", "UserTokens": [user_token] },
            "RelyingParty": relying_party, "TokenType": "JWT",
        });
        let x: serde_json::Value = serde_json::from_str(&send(xbl(http.post("https://xsts.auth.xboxlive.com/xsts/authorize")).body(x.to_string())).await?)?;
        let token = x["Token"].as_str().ok_or_else(|| Error::Other("Xbox Live gave no token".into()))?.to_string();
        let xui = &x["DisplayClaims"]["xui"][0];
        let (uhs, gamertag) = (xui["uhs"].as_str().unwrap_or_default().to_string(), xui["gtg"].as_str().unwrap_or_default().to_string());
        Ok((token, uhs, gamertag))
    }

    /// The account's gamertag (to show who is signed in).
    pub async fn gamertag(&self) -> Result<String> {
        Ok(self.xsts("http://xboxlive.com").await?.2)
    }

    /// Remote Play's token and regions.
    pub async fn streaming(&self) -> Result<Login> {
        let (gssv, _, _) = self.xsts("http://gssv.xboxlive.com/").await?;
        let body = send(
            client()?
                .post("https://xhome.gssv-play-prod.xboxlive.com/v2/login/user")
                .header("Content-Type", "application/json")
                .header("Cache-Control", "no-store, must-revalidate, no-cache")
                .header("x-gssv-client", "XboxComBrowser")
                .body(json!({ "token": gssv, "offeringId": "xhome" }).to_string()),
        )
        .await?;
        login_of(&body)
    }

    /// Wakes the console from sleep (Xbox's own remote power-on).
    pub async fn wake(&self, server_id: &str) -> Result<()> {
        let (token, uhs, _) = self.xsts("http://xboxlive.com").await?;
        let body = json!({
            "destination": "Xbox", "type": "Power", "command": "WakeUp", "sessionId": uuid::Uuid::new_v4().to_string(),
            "sourceId": "com.microsoft.smartglass", "parameters": [], "linkedXboxId": server_id,
        });
        send(
            client()?
                .post("https://xccs.xboxlive.com/commands")
                .header("Content-Type", "application/json")
                .header("Authorization", format!("XBL3.0 x={uhs};{token}"))
                .header("skillplatform", "RemoteManagement")
                .header("x-xbl-contract-version", "4")
                .header("x-xbl-client-name", "XboxApp")
                .header("x-xbl-client-type", "UWA")
                .header("x-xbl-client-version", "39.39.22001.0")
                .header("Accept-Language", "en-US")
                .body(body.to_string()),
        )
        .await?;
        Ok(())
    }
}

/// The gsToken and the regions (the default first) in /v2/login/user's answer.
pub fn login_of(body: &str) -> Result<Login> {
    let o: serde_json::Value = serde_json::from_str(body)?;
    let gs_token = o["gsToken"].as_str().ok_or_else(|| Error::Other("Remote Play gave no token".into()))?.to_string();
    let mut regions: Vec<&serde_json::Value> = o["offeringSettings"]["regions"].as_array().map(|a| a.iter().collect()).unwrap_or_default();
    regions.sort_by_key(|r| !r["isDefault"].as_bool().unwrap_or(false));
    let base_uris: Vec<String> = regions.iter().filter_map(|r| r["baseUri"].as_str()).map(|u| u.trim_end_matches('/').to_string()).collect();
    if base_uris.is_empty() {
        return Err(Error::Other("Remote Play offered no region".into()));
    }
    Ok(Login { gs_token, base_uris })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Microsoft's real endpoint gives a code, and an untyped code is "pending" (network; no account
    /// involved): `cargo test -p vi-translator-xbox -- --ignored`
    #[tokio::test]
    #[ignore]
    async fn microsoft_gives_a_code_to_type() {
        let dc = request_code().await.unwrap();
        assert!(!dc.user_code.is_empty() && !dc.device_code.is_empty());
        assert!(dc.verification_uri.starts_with("https://"));
        let p = poll_code(&dc).await;
        assert!(matches!(p, Ok(Poll::Pending)), "{p:?}");
    }

    #[test]
    fn device_code_errors_follow_rfc_8628() {
        assert!(matches!(poll_error(r#"{"error":"authorization_pending"}"#), Poll::Pending));
        assert!(matches!(poll_error(r#"{"error":"slow_down"}"#), Poll::SlowDown));
        assert!(matches!(poll_error(r#"{"error":"expired_token"}"#), Poll::Failed(e) if e == "expired_token"));
        assert!(matches!(poll_error("not json"), Poll::Failed(_)));
    }

    #[test]
    fn the_default_region_comes_first() {
        let body = r#"{"offeringSettings":{"regions":[
            {"name":"WestUS2","baseUri":"https://wus2.core.gssv-play-prodxhome.xboxlive.com","isDefault":false},
            {"name":"UkSouth","baseUri":"https://uks.core.gssv-play-prodxhome.xboxlive.com/","isDefault":true}]},
            "gsToken":"g","durationInSeconds":14400}"#;
        let l = login_of(body).unwrap();
        assert_eq!(l.gs_token, "g");
        assert_eq!(l.base_uris, ["https://uks.core.gssv-play-prodxhome.xboxlive.com", "https://wus2.core.gssv-play-prodxhome.xboxlive.com"]);
        assert!(login_of(r#"{"gsToken":"g","offeringSettings":{"regions":[]}}"#).is_err());
    }

    #[test]
    fn a_refresh_keeps_the_old_refresh_token_when_none_comes() {
        let a = TokenAnswer { access_token: "a".into(), refresh_token: None, expires_in: Some(10) };
        assert_eq!(a.tokens("old").refresh, "old");
        assert_eq!(format!("{:?}", TokenAnswer { access_token: "secret".into(), refresh_token: None, expires_in: None }.tokens("r")), "Tokens(…)");
    }
}
