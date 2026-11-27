//! Xbox Remote Play's web API ("xHome" on Microsoft's gssv servers), as xbox.com's player uses it:
//! the account's consoles, and a stream session's steps.

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::auth::Login;
use crate::{Error, Result, client, send};

/// An Xbox the account can stream from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Console {
    pub id: String,
    pub name: String,
    /// "XboxSeriesX", "XboxSeriesS", "XboxOne", …
    #[serde(default)]
    pub kind: String,
    /// "On", "ConnectedStandby", "Off", "Unknown" (as listed; not kept)
    #[serde(default, skip_serializing)]
    pub power: String,
}

impl Console {
    pub fn asleep(&self) -> bool {
        self.power.eq_ignore_ascii_case("ConnectedStandby") || self.power.eq_ignore_ascii_case("Off")
    }
}

/// The consoles in /v6/servers/home's answer.
pub fn consoles_of(body: &str) -> Result<Vec<Console>> {
    let o: serde_json::Value = serde_json::from_str(body)?;
    Ok(o["results"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|c| {
                    Some(Console {
                        id: c["serverId"].as_str()?.to_string(),
                        name: c["deviceName"].as_str().unwrap_or("Xbox").to_string(),
                        kind: c["consoleType"].as_str().unwrap_or_default().to_string(),
                        power: c["powerState"].as_str().unwrap_or_default().to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default())
}

/// A session's state, and the console's error text if it failed.
#[derive(Debug, Clone, PartialEq)]
pub struct State {
    pub state: String,
    pub error: Option<String>,
}

/// One console's stream session on Microsoft's servers.
pub struct Home {
    http: reqwest::Client,
    login: Login,
    base: String,
    /// 1080 or 720
    resolution: u32,
    session: Option<String>,
}

impl Home {
    pub fn new(login: Login, resolution: u32) -> Result<Self> {
        let base = login.base_uris[0].clone();
        Ok(Home { http: client()?, login, base, resolution, session: None })
    }

    /// Microsoft picks the stream's size by the client's system: "android" 720p, "windows" 1080p.
    fn os_name(&self) -> &'static str {
        os_name(self.resolution)
    }

    fn device_info(&self) -> String {
        json!({
            "appInfo": { "env": { "clientAppId": "www.xbox.com", "clientAppType": "browser", "clientAppVersion": "29.9.35",
                "clientSdkVersion": "10.6.8", "httpEnvironment": "prod", "sdkInstallId": "" } },
            "dev": {
                "hw": { "make": "Microsoft", "model": "unknown", "platformType": "desktop", "sdktype": "web" },
                "os": { "name": self.os_name(), "ver": "22631.2715", "platform": "desktop" },
                "displayInfo": { "dimensions": { "widthInPixels": 1920, "heightInPixels": 1080 }, "pixelDensity": { "dpiX": 1, "dpiY": 1 } },
                "browser": { "browserName": "edge", "browserVersion": "140.0.3485.66" },
            },
        })
        .to_string()
    }

    fn auth(&self, r: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        r.header("Authorization", format!("Bearer {}", self.login.gs_token)).header("Content-Type", "application/json").header("X-MS-Device-Info", self.device_info())
    }

    async fn get(&self, path: &str) -> Result<String> {
        send(self.auth(self.http.get(format!("{}/{path}", self.base)))).await
    }

    async fn post(&self, path: &str, body: serde_json::Value) -> Result<String> {
        send(self.auth(self.http.post(format!("{}/{path}", self.base))).body(body.to_string())).await
    }

    fn session(&self) -> Result<&str> {
        self.session.as_deref().ok_or_else(|| Error::Other("no session".into()))
    }

    /// The consoles, from the first region that lists any (that region is used from then on).
    pub async fn consoles(&mut self) -> Result<Vec<Console>> {
        let bases = self.login.base_uris.clone();
        for (i, base) in bases.iter().enumerate() {
            self.base = base.clone();
            let list = match self.get("v6/servers/home?mr=50").await {
                Ok(b) => consoles_of(&b)?,
                Err(e) if i + 1 == bases.len() => return Err(e),
                Err(_) => continue,
            };
            if !list.is_empty() {
                return Ok(list);
            }
        }
        self.base = bases[0].clone();
        Ok(Vec::new())
    }

    /// Starts a session with the console (it then gets ready: [`Home::state`]).
    pub async fn play(&mut self, server_id: &str, tz_minutes: i32) -> Result<()> {
        let body = json!({
            "clientSessionId": "", "titleId": "", "systemUpdateGroup": "",
            "settings": { "nanoVersion": "V3;WebrtcTransport.dll", "enableTextToSpeech": false, "highContrast": 0,
                "locale": "en-US", "useIceConnection": false, "timezoneOffsetMinutes": tz_minutes,
                "sdkType": "web", "osName": self.os_name() },
            "serverId": server_id, "fallbackRegionNames": [],
        });
        let o: serde_json::Value = serde_json::from_str(&self.post("v5/sessions/home/play", body).await?)?;
        let path = o["sessionPath"].as_str().ok_or_else(|| Error::Other("Remote Play gave no session".into()))?;
        self.session = Some(path.trim_start_matches('/').to_string());
        Ok(())
    }

    pub async fn state(&self) -> Result<State> {
        let o: serde_json::Value = serde_json::from_str(&self.get(&format!("{}/state", self.session()?)).await?)?;
        Ok(State { state: o["state"].as_str().unwrap_or_default().to_string(), error: error_message(&o) })
    }

    /// The console asks for the user's transfer token (state "ReadyToConnect").
    pub async fn connect(&self, user_token: &str) -> Result<()> {
        self.post(&format!("{}/connect", self.session()?), json!({ "userToken": user_token })).await?;
        Ok(())
    }

    /// Seconds between keep-alives, from the session's configuration (5–300, else 20).
    pub async fn keep_alive_seconds(&self) -> u64 {
        match self.get(&format!("{}/configuration", self.session().unwrap_or_default())).await {
            Ok(b) => serde_json::from_str::<serde_json::Value>(&b).ok().and_then(|o| o["keepAlivePulseInSeconds"].as_u64()).unwrap_or(20).clamp(5, 300),
            Err(_) => 20,
        }
    }

    /// Sends the WebRTC offer; the console's answer SDP.
    pub async fn exchange_sdp(&self, offer: &str) -> Result<String> {
        let range = |a: u32, b: u32| json!({ "minVersion": a, "maxVersion": b });
        let body = json!({
            "messageType": "offer", "sdp": offer,
            "configuration": {
                "chatConfiguration": { "bytesPerSample": 2, "expectedClipDurationMs": 20, "format": { "codec": "opus", "container": "webm" },
                    "numChannels": 1, "sampleFrequencyHz": 24000 },
                "chat": range(1, 1), "control": range(1, 3), "input": range(1, 8), "message": range(1, 1),
            },
        });
        let path = format!("{}/sdp", self.session()?);
        self.post(&path, body).await?;
        let a: serde_json::Value = serde_json::from_str(&self.poll(&path, Duration::from_secs(30)).await?)?;
        if let Some(s) = a["status"].as_str().filter(|s| !s.is_empty() && *s != "success") {
            return Err(Error::Other(format!("the console refused the stream: {s}")));
        }
        Ok(a["sdp"].as_str().ok_or_else(|| Error::Other("the console sent no answer".into()))?.to_string())
    }

    /// Sends this computer's network candidates; the console's.
    pub async fn exchange_ice(&self, mine: &[Candidate]) -> Result<Vec<Candidate>> {
        let list: Vec<serde_json::Value> = mine
            .iter()
            .map(|c| json!({ "candidate": c.candidate, "messageType": "iceCandidate", "sdpMLineIndex": c.mline.to_string(), "sdpMid": c.mid }))
            .collect();
        let path = format!("{}/ice", self.session()?);
        self.post(&path, json!({ "messageType": "iceCandidate", "candidate": list })).await?;
        candidates_of(&self.poll(&path, Duration::from_secs(30)).await?)
    }

    /// GETs `path` each second until it carries an `exchangeResponse` (a JSON string): that.
    async fn poll(&self, path: &str, limit: Duration) -> Result<String> {
        let until = Instant::now() + limit;
        while Instant::now() < until {
            let body = self.get(path).await?;
            if !body.trim().is_empty() {
                let o: serde_json::Value = serde_json::from_str(&body)?;
                if let Some(r) = o["exchangeResponse"].as_str().filter(|r| !r.is_empty()) {
                    return Ok(r.to_string());
                }
                if let Some(e) = error_message(&o) {
                    return Err(Error::Other(e));
                }
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        Err(Error::Other("the console didn't answer".into()))
    }

    pub async fn keep_alive(&self) -> Result<()> {
        self.post(&format!("{}/keepalive", self.session()?), json!({})).await?;
        Ok(())
    }

    /// Ends the session (quietly: the console may have ended it already).
    pub async fn stop(&mut self) {
        if let Some(s) = self.session.take() {
            let _ = send(self.auth(self.http.delete(format!("{}/{s}", self.base)))).await;
        }
    }
}

pub fn os_name(resolution: u32) -> &'static str {
    if resolution >= 1080 { "windows" } else { "android" }
}

fn error_message(o: &serde_json::Value) -> Option<String> {
    o["errorDetails"]["message"].as_str().filter(|m| !m.is_empty()).map(str::to_string)
}

/// A network candidate: its line ("candidate:…" or "a=candidate:…"), mid and m-line index.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub candidate: String,
    pub mid: String,
    pub mline: u16,
}

impl Candidate {
    /// Nothing to add: the end marker, empty, or a UDP line with a TCP type (as xbox.com's player drops).
    pub fn skip(&self) -> bool {
        let c = &self.candidate;
        c.trim().is_empty() || c.contains("end-of-candidates") || (c.to_ascii_uppercase().contains("UDP") && c.contains("tcptype"))
    }
}

/// The console's candidates in /ice's `exchangeResponse` (a JSON array).
pub fn candidates_of(s: &str) -> Result<Vec<Candidate>> {
    let a: Vec<serde_json::Value> = serde_json::from_str(s)?;
    Ok(a.iter()
        .map(|c| Candidate {
            candidate: c["candidate"].as_str().unwrap_or_default().to_string(),
            mid: c["sdpMid"].as_str().unwrap_or("0").to_string(),
            mline: match &c["sdpMLineIndex"] {
                serde_json::Value::Number(n) => n.as_u64().unwrap_or(0) as u16,
                v => v.as_str().and_then(|s| s.parse().ok()).unwrap_or(0),
            },
        })
        .collect())
}

/// The console's candidates with the extra ones its Teredo (IPv6 over IPv4) address stands for: its
/// IPv4 addresses, at its port and at 3074, as xbox.com's player adds them.
pub fn with_teredo(list: Vec<Candidate>) -> Vec<Candidate> {
    let mut out = Vec::new();
    let mut n = 100;
    for c in list {
        let parts: Vec<&str> = c.candidate.trim_start_matches("a=").split(' ').collect();
        let extra = (parts.len() >= 6).then(|| (teredo(parts[4]), parts[5].parse::<u16>().unwrap_or(9002)));
        out.push(c.clone());
        let Some((Some((server, client, udp)), port)) = extra else { continue };
        let mut adds = vec![(client, udp), (server, port), (client, port)];
        if port != 3074 {
            adds.extend([(server, 3074), (client, 3074)]);
        }
        for (a, p) in adds {
            out.push(Candidate { candidate: format!("a=candidate:{n} 1 UDP 100 {a} {p} typ host"), mid: c.mid.clone(), mline: c.mline });
            n += 1;
        }
    }
    out
}

/// A Teredo address (2001:0::/32): its server's IPv4, the client's IPv4, and the client's port.
pub fn teredo(ip: &str) -> Option<(std::net::Ipv4Addr, std::net::Ipv4Addr, u16)> {
    let ip: std::net::Ipv6Addr = ip.split('%').next()?.trim_matches(['[', ']']).parse().ok()?;
    let h = ip.segments();
    if h[0] != 0x2001 || h[1] != 0 {
        return None;
    }
    let server = std::net::Ipv4Addr::new((h[2] >> 8) as u8, h[2] as u8, (h[3] >> 8) as u8, h[3] as u8);
    let (c6, c7) = (h[6] ^ 0xffff, h[7] ^ 0xffff);
    let client = std::net::Ipv4Addr::new((c6 >> 8) as u8, c6 as u8, (c7 >> 8) as u8, c7 as u8);
    Some((server, client, h[5] ^ 0xffff))
}

/// What the console's error means for the player (English; the app translates it).
pub fn failure(error: Option<&str>) -> String {
    let refused = "The Xbox refused the stream.";
    match error {
        None => refused.into(),
        Some(e) if e.to_ascii_lowercase().contains("disabled streaming") => {
            "Remote features are off on the Xbox: Settings › Devices & connections › Remote features.".into()
        }
        Some(e) if e.contains("WaitingForServerToRegister") => {
            "The Xbox isn't connected to Xbox services. Restart the Xbox (hold its power button for 10 s), then try again.".into()
        }
        Some(e) if e.to_ascii_lowercase().contains("xboxstreaminghelper") => {
            "The Xbox's streaming stopped working. Restart the Xbox (hold its power button for 10 s), then try again.".into()
        }
        Some(e) => format!("{refused} ({e})"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consoles_are_read_from_the_list() {
        let l = consoles_of(r#"{"results":[{"serverId":"F4001D0F950AA28A","deviceName":"Living room","consoleType":"XboxSeriesX","powerState":"ConnectedStandby"},{"deviceName":"no id"}]}"#).unwrap();
        assert_eq!(l.len(), 1);
        assert_eq!((l[0].id.as_str(), l[0].name.as_str(), l[0].kind.as_str()), ("F4001D0F950AA28A", "Living room", "XboxSeriesX"));
        assert!(l[0].asleep());
        assert!(consoles_of(r#"{"results":[]}"#).unwrap().is_empty());
        // (the power state isn't kept with the chosen console)
        assert!(!serde_json::to_string(&l[0]).unwrap().contains("power"));
    }

    #[test]
    fn the_resolution_is_asked_by_system_name() {
        assert_eq!(os_name(1080), "windows");
        assert_eq!(os_name(720), "android");
    }

    #[test]
    fn teredo_addresses_give_their_ipv4_addresses() {
        // RFC 4380's example: server 65.54.227.120, client 192.0.2.45 port 40000
        let (s, c, p) = teredo("2001:0000:4136:e378:8000:63bf:3fff:fdd2").unwrap();
        assert_eq!(s.to_string(), "65.54.227.120");
        assert_eq!(c.to_string(), "192.0.2.45");
        assert_eq!(p, 40000);
        assert!(teredo("2002::1").is_none());
        assert!(teredo("192.168.1.2").is_none());
    }

    #[test]
    fn a_teredo_candidate_brings_its_ipv4_ones() {
        let c = Candidate { candidate: "a=candidate:1 1 UDP 1 2001:0:4136:e378:8000:63bf:3fff:fdd2 9002 typ host".into(), mid: "0".into(), mline: 0 };
        let host = Candidate { candidate: "a=candidate:2 1 UDP 1 192.168.1.9 9002 typ host".into(), mid: "0".into(), mline: 0 };
        let out = with_teredo(vec![c.clone(), host.clone()]);
        assert_eq!(out.len(), 1 + 5 + 1);
        assert_eq!(out[0], c);
        assert_eq!(out[1].candidate, "a=candidate:100 1 UDP 100 192.0.2.45 40000 typ host");
        assert_eq!(out[2].candidate, "a=candidate:101 1 UDP 100 65.54.227.120 9002 typ host");
        assert_eq!(out[4].candidate, "a=candidate:103 1 UDP 100 65.54.227.120 3074 typ host");
        assert_eq!(out[6], host);
    }

    #[test]
    fn the_console_s_candidates_are_read() {
        let l = candidates_of(r#"[{"candidate":"a=candidate:1 1 UDP 1 1.2.3.4 9002 typ host","sdpMid":"0","sdpMLineIndex":"0"},{"candidate":"a=end-of-candidates","sdpMid":"0","sdpMLineIndex":0}]"#).unwrap();
        assert_eq!(l.len(), 2);
        assert!(!l[0].skip());
        assert!(l[1].skip());
        assert!(Candidate { candidate: "candidate:1 1 udp 1 1.2.3.4 9 typ host tcptype active".into(), mid: "0".into(), mline: 0 }.skip());
    }

    #[test]
    fn console_errors_are_explained() {
        assert!(failure(Some("x WaitingForServerToRegister y")).contains("Xbox services"));
        assert!(failure(Some("The user has disabled streaming")).contains("Remote features"));
        assert_eq!(failure(None), "The Xbox refused the stream.");
        assert!(failure(Some("odd")).ends_with("(odd)"));
    }
}
