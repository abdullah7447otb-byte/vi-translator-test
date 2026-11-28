//! TVs that show the overlays over the game: the VI Translator app on the TV on a Google TV or Android TV.
//!
//! Connecting starts here, in the main window: the app finds the TVs on the home network (the TV
//! app answers on `TV_PORT` while it waits to be connected), and the user types the code the TV
//! shows. The TV checks the code; if it matches, it keeps this PC's address and a key made for it,
//! and from then on asks the home-network server (lan.rs) for the TV page with that key. A TV
//! without a key, or with a key removed here, gets nothing.
//!
//! A Samsung (Tizen) TV can't be reached on the home network (its apps can't listen), so it finds
//! this PC instead: while it shows a code, it offers it to the home-network server (lan.rs,
//! `POST /vit-tv/offer`), and shows up among the TVs found here. Once the user types its code here,
//! its next offer with that code gets this PC's key for it (`Tvs::offer`); the TV keeps the
//! address it offered at.
//!
//! The keys are kept in their own file (tvs.json), not in the settings: the settings window saves
//! the whole settings back and never needs to see them.

use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// Where the TV app answers while it waits to be connected.
pub const TV_PORT: u16 = 8792;
/// Sent with every request to a TV: a web page can't send it to the TV from a browser without the
/// TV agreeing first (it never does), so only this app can connect a TV.
const HEADER: &str = "X-VIT";

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct PairedTv {
    /// the TV app's own id (the same TV connected again replaces its old key)
    pub id: String,
    pub name: String,
    pub token: String,
    /// switched off in the app: the TV stays connected but shows nothing until switched on again
    /// (missing in an older tvs.json: on)
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub off: bool,
}

/// A connected TV, for the window (never its key).
#[derive(Serialize, Clone, Debug)]
pub struct TvInfo {
    pub id: String,
    pub name: String,
    /// showing the overlays now: its page asked for them in the last few seconds (it asks about
    /// once a second while the TV app is open)
    pub online: bool,
    /// shows the translations (the app's switch for this TV); off, it stays connected and blank
    pub on: bool,
}

/// A TV that asked for the overlays this recently is online.
const ONLINE_FOR: Duration = Duration::from_secs(6);

/// A TV on the home network that runs the TV app.
#[derive(Serialize, Clone, Debug)]
pub struct FoundTv {
    pub address: String,
    pub id: String,
    pub name: String,
    /// already connected to this PC
    pub connected: bool,
}

/// A TV that offered its code lately (a Tizen TV showing a code).
struct Offered {
    id: String,
    name: String,
    at: Instant,
}

/// Connecting a TV that offers its code: the code typed here, waiting for the TV's next offer.
struct Pending {
    id: String,
    code: String,
    token: String,
    pc_name: String,
    /// how the offer went (set by `Tvs::offer`, read by `pair_offered`)
    result: Option<Result<String, PairError>>,
}

#[derive(Default)]
pub struct Tvs {
    path: Mutex<Option<PathBuf>>,
    list: Mutex<Vec<PairedTv>>,
    offered: Mutex<Vec<Offered>>,
    pending: Mutex<Vec<Pending>>,
    /// wrong codes in a row, by TV (it's told to show a new code after `TRIES`)
    wrong: Mutex<Vec<(String, u32)>>,
    /// a key for this device's own viewer of the TV page (`local_key`): never saved or listed
    local: Mutex<String>,
    /// when each connected TV's page last asked for the overlays, by TV id (not saved)
    seen: Mutex<Vec<(String, Instant)>>,
}

/// An offer this old is from a TV that stopped showing its code.
const OFFER_FRESH: Duration = Duration::from_secs(30);
/// How long connecting waits for the TV's next offer (it offers every 2 seconds).
const OFFER_WAIT: Duration = Duration::from_secs(12);
/// Wrong codes in a row before the TV is told to show a new one.
const TRIES: u32 = 5;
/// The address `find` gives a TV that offers its code (there's no address to call it at): this,
/// then its id.
const OFFERED: &str = "offered:";

impl Tvs {
    /// Read the connected TVs from `path`, and keep them there from now on.
    pub fn load(&self, path: PathBuf) {
        let list: Vec<PairedTv> = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        *self.list.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = list.into_iter().filter(|t| !t.token.is_empty()).collect();
        *self.path.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(path);
    }

    fn save(&self, list: &[PairedTv]) {
        if let Some(p) = self.path.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_ref() {
            if let Ok(b) = serde_json::to_vec_pretty(list) {
                let tmp = p.with_extension("json.tmp");
                if std::fs::write(&tmp, b).is_ok() {
                    let _ = std::fs::rename(&tmp, p);
                }
            }
        }
    }

    /// Is this a key of a connected TV?
    pub fn allowed(&self, token: &str) -> bool {
        if token.is_empty() {
            return false;
        }
        let local = self.local.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        (!local.is_empty() && same(local.as_bytes(), token.as_bytes())) || self.list.lock().unwrap_or_else(std::sync::PoisonError::into_inner).iter().any(|t| same(t.token.as_bytes(), token.as_bytes()))
    }

    /// A key for this device's own viewer of the TV page (a phone showing the translations over its
    /// picture as the TVs do): `/tv?t=<key>` from 127.0.0.1. Made once per run, never saved, not
    /// among the connected TVs; `/tv/forget` with it does nothing.
    pub fn local_key(&self) -> String {
        let mut k = self.local.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if k.is_empty() {
            *k = format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple());
        }
        k.clone()
    }

    pub fn list(&self) -> Vec<TvInfo> {
        let seen = self.seen.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let online = |id: &str| seen.iter().any(|(s, at)| s == id && at.elapsed() < ONLINE_FOR);
        self.list.lock().unwrap_or_else(std::sync::PoisonError::into_inner).iter().map(|t| TvInfo { id: t.id.clone(), name: t.name.clone(), online: online(&t.id), on: !t.off }).collect()
    }

    /// Switch a connected TV's translations on or off (kept in tvs.json). False when there's no such TV.
    pub fn set_on(&self, id: &str, on: bool) -> bool {
        let mut l = self.list.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(t) = l.iter_mut().find(|t| t.id == id) else { return false };
        if t.off == !on {
            return true;
        }
        t.off = !on;
        self.save(&l);
        true
    }

    /// Is this the key of this device's own viewer (`local_key`)?
    pub fn is_local(&self, token: &str) -> bool {
        let local = self.local.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        !local.is_empty() && same(local.as_bytes(), token.as_bytes())
    }

    /// Does the page with this key show the translations now? Off only for a connected TV switched
    /// off in the app (this device's own viewer follows its own setting, not this).
    pub fn showing(&self, token: &str) -> bool {
        !self.list.lock().unwrap_or_else(std::sync::PoisonError::into_inner).iter().any(|t| t.off && same(t.token.as_bytes(), token.as_bytes()))
    }

    /// The connected TV with this key asked for its page or the overlays now (for `TvInfo::online`).
    pub fn touch(&self, token: &str) {
        let Some(id) = self.list.lock().unwrap_or_else(std::sync::PoisonError::into_inner).iter().find(|t| same(t.token.as_bytes(), token.as_bytes())).map(|t| t.id.clone()) else { return };
        let mut seen = self.seen.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        seen.retain(|(s, _)| *s != id);
        seen.push((id, Instant::now()));
    }

    /// (the same TV connected again replaces its old key, and shows the translations again)
    pub(crate) fn add(&self, tv: PairedTv) {
        let mut l = self.list.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        l.retain(|t| t.id != tv.id);
        l.push(tv);
        self.save(&l);
    }

    /// Removed here: the TV's key stops working (the TV notices and shows a new code).
    pub fn remove_id(&self, id: &str) -> bool {
        let mut l = self.list.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let before = l.len();
        l.retain(|t| t.id != id);
        let changed = l.len() != before;
        if changed {
            self.save(&l);
        }
        changed
    }

    /// The TV disconnected itself.
    pub fn remove_token(&self, token: &str) -> bool {
        let mut l = self.list.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let before = l.len();
        l.retain(|t| !same(t.token.as_bytes(), token.as_bytes()));
        let changed = l.len() != before;
        if changed {
            self.save(&l);
        }
        changed
    }
}

/// Compared in the same time whatever the bytes (a key can't be guessed a byte at a time).
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[derive(Deserialize)]
struct Hello {
    app: String,
    id: String,
    name: String,
}

fn client(total: Duration) -> Option<reqwest::blocking::Client> {
    crate::net::http_client()
        .connect_timeout(Duration::from_millis(400))
        .timeout(total)
        .no_proxy()
        .build()
        .ok()
}

/// The TV app at `ip`, if it answers there.
fn hello(c: &reqwest::blocking::Client, ip: Ipv4Addr) -> Option<(String, Hello)> {
    let address = format!("{ip}:{TV_PORT}");
    let r = c.get(format!("http://{address}/vit-tv")).header(HEADER, "1").send().ok()?;
    if !r.status().is_success() {
        return None;
    }
    let h: Hello = r.json().ok()?;
    (h.app == "vi-translator-tv" && !h.id.is_empty()).then_some((address, h))
}

/// Addresses asked at most in one search (a /22's hosts, and a little more).
const MAX_SCAN: usize = 1024;
/// Addresses asked at once, at most (each a TCP connection: the iPhone allows an app 256 files).
const MAX_AT_ONCE: usize = 128;

/// The other addresses on `me`'s network worth asking, nearest first: its /24 (its whole /24 when
/// the network is smaller, as a home router's always is), then, on a wider network, the rest of
/// it up to a /22 around `me` (1022 hosts). Never the network's own address or its broadcast one.
pub(crate) fn hosts_around(me: Ipv4Addr, prefix: u8) -> (Vec<Ipv4Addr>, Vec<Ipv4Addr>) {
    let ip = u32::from(me);
    let (net, broadcast) = (ip & crate::net::mask(prefix), ip | !crate::net::mask(prefix));
    let ok = |a: u32| a != ip && a != net && a != broadcast;
    let base24 = ip & crate::net::mask(24);
    if prefix >= 24 {
        let near = (1..=254u32).map(|i| base24 | i).filter(|a| *a != ip).map(Ipv4Addr::from).collect();
        return (near, Vec::new());
    }
    let near = (0..=255u32).map(|i| base24 | i).filter(|a| ok(*a)).map(Ipv4Addr::from).collect();
    let block = ip & crate::net::mask(prefix.max(22));
    let mut wider: Vec<u32> = (0..(1u32 << (32 - u32::from(prefix.max(22))))).map(|i| block | i).filter(|a| a & crate::net::mask(24) != base24 && ok(*a)).collect();
    wider.sort_by_key(|a| a.abs_diff(ip));
    (near, wider.into_iter().map(Ipv4Addr::from).collect())
}

/// The addresses to ask on all of `mine` (best first, see `net::order_lan`): every network's /24
/// first, then the wider networks' other hosts, once each, at most `MAX_SCAN`.
pub(crate) fn scan_list(mine: &[crate::net::LanAddr]) -> Vec<Ipv4Addr> {
    let around: Vec<_> = mine.iter().map(|a| hosts_around(a.ip, a.prefix)).collect();
    let own: Vec<Ipv4Addr> = mine.iter().map(|a| a.ip).collect();
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for ip in around.iter().flat_map(|(near, _)| near).chain(around.iter().flat_map(|(_, wider)| wider)) {
        if out.len() >= MAX_SCAN {
            break;
        }
        if !own.contains(ip) && seen.insert(*ip) {
            out.push(*ip);
        }
    }
    out
}

/// The TVs waiting to be connected on this device's home networks (each Wi-Fi/Ethernet network
/// it's on, not only the one reaching the internet: see `scan_list`): every address at once, a
/// couple of seconds in all.
pub fn find(tvs: &Tvs) -> Vec<FoundTv> {
    let mut mine = crate::net::lan_addrs();
    if mine.is_empty() {
        // (the addresses couldn't be listed: the one reaching the internet, as a /24)
        let Some(ip) = crate::net::lan_ip().and_then(|ip| ip.parse::<Ipv4Addr>().ok()) else { return Vec::new() };
        mine.push(crate::net::LanAddr { name: String::new(), ip, prefix: 24 });
    }
    let Some(c) = client(Duration::from_millis(1500)) else { return Vec::new() };
    let hosts = scan_list(&mine);
    // 8 addresses one after the other per thread, as many threads as that takes (at most MAX_AT_ONCE)
    let per = hosts.len().div_ceil(MAX_AT_ONCE).max(8);
    let found = Mutex::new(Vec::new());
    std::thread::scope(|s| {
        for chunk in hosts.chunks(per) {
            let (c, found) = (&c, &found);
            s.spawn(move || {
                for ip in chunk {
                    if let Some((address, h)) = hello(c, *ip) {
                        found.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push((*ip, address, h));
                    }
                }
            });
        }
    });
    let connected: Vec<String> = tvs.list().into_iter().map(|t| t.id).collect();
    let mut found = found.into_inner().unwrap();
    found.sort_by_key(|(ip, _, _)| hosts.iter().position(|h| h == ip));
    let mut out: Vec<FoundTv> = Vec::new();
    for (_, address, h) in found {
        // (a TV on two of the networks: once, at its address on the best one)
        if !out.iter().any(|f| f.id == h.id) {
            out.push(FoundTv { connected: connected.contains(&h.id), address, id: h.id, name: h.name });
        }
    }
    for (id, name) in tvs.offered() {
        if !out.iter().any(|f| f.id == id) {
            out.push(FoundTv { connected: connected.contains(&id), address: format!("{OFFERED}{id}"), id, name });
        }
    }
    out.sort_by(|x, y| x.name.cmp(&y.name).then(x.address.cmp(&y.address)));
    out
}

#[derive(Deserialize)]
struct Paired {
    id: String,
    name: String,
}

/// Why connecting didn't work (the window says it in the user's language).
#[derive(Debug, PartialEq)]
pub enum PairError {
    /// not the code the TV shows
    WrongCode,
    /// too many wrong codes: the TV shows a new one
    TooMany,
    /// no answer, or not the TV app
    NoAnswer,
    /// the home-network sharing isn't running (the TV would have nothing to load)
    NotSharing,
}

impl PairError {
    pub fn code(&self) -> &'static str {
        match self {
            PairError::WrongCode => "wrong_code",
            PairError::TooMany => "too_many",
            PairError::NoAnswer => "no_answer",
            PairError::NotSharing => "not_sharing",
        }
    }
}

/// Connect the TV at `address` (as `find` gave it) with the code it shows: it gets this PC's
/// address (`pc`, as "ip:port") and a new key, and is kept as connected. Its name, when it worked.
pub fn pair(tvs: &Tvs, address: &str, code: &str, pc: &str, pc_name: &str) -> Result<String, PairError> {
    if let Some(id) = address.strip_prefix(OFFERED) {
        return pair_offered(tvs, id, code, pc_name);
    }
    // (an address on the home network as find() gives it: the TV app's port, an IPv4 address)
    let ok_address = address
        .strip_suffix(&format!(":{TV_PORT}"))
        .and_then(|ip| ip.parse::<Ipv4Addr>().ok())
        .is_some_and(|ip| ip.is_private() || ip.is_link_local());
    let code: String = code.chars().filter(|c| c.is_ascii_digit()).collect();
    if !ok_address || code.len() != 6 {
        return Err(if ok_address { PairError::WrongCode } else { PairError::NoAnswer });
    }
    let token = format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple());
    let c = client(Duration::from_secs(5)).ok_or(PairError::NoAnswer)?;
    let body = serde_json::json!({ "code": code, "pc": pc, "pc_name": pc_name, "token": token });
    let r = c.post(format!("http://{address}/vit-tv/pair")).header(HEADER, "1").json(&body).send().map_err(|_| PairError::NoAnswer)?;
    match r.status().as_u16() {
        200 => {
            let p: Paired = r.json().map_err(|_| PairError::NoAnswer)?;
            let name = if p.name.trim().is_empty() { "TV".to_string() } else { p.name.chars().take(60).collect() };
            tvs.add(PairedTv { id: p.id.chars().take(64).collect(), name: name.clone(), token, off: false });
            Ok(name)
        }
        403 => Err(PairError::WrongCode),
        429 => Err(PairError::TooMany),
        _ => Err(PairError::NoAnswer),
    }
}

impl Tvs {
    /// The TVs that offered their code lately: id and name.
    fn offered(&self) -> Vec<(String, String)> {
        let mut o = self.offered.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        o.retain(|x| x.at.elapsed() < OFFER_FRESH);
        o.iter().map(|x| (x.id.clone(), x.name.clone())).collect()
    }

    /// A TV offers the code it shows (`POST /vit-tv/offer`, its JSON body): the HTTP status and
    /// body to answer. 404 while it isn't being connected here, 403 for another code than the one
    /// typed, 429 after `TRIES` wrong ones in a row (the TV shows a new code), 200 with this PC's
    /// key for it when it's the code typed.
    pub fn offer(&self, body: &[u8]) -> (u16, String) {
        #[derive(Deserialize)]
        struct Offer {
            id: String,
            #[serde(default)]
            name: String,
            code: String,
        }
        let Ok(o) = serde_json::from_slice::<Offer>(body) else { return (400, "{}".into()) };
        let ok_id = !o.id.is_empty() && o.id.len() <= 64 && o.id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
        if !ok_id || o.code.len() != 6 || !o.code.chars().all(|c| c.is_ascii_digit()) {
            return (400, "{}".into());
        }
        let name = match o.name.trim() {
            "" => "TV".to_string(),
            n => n.chars().take(60).collect(),
        };
        {
            let mut offered = self.offered.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            offered.retain(|x| x.id != o.id && x.at.elapsed() < OFFER_FRESH);
            offered.push(Offered { id: o.id.clone(), name: name.clone(), at: Instant::now() });
        }
        let mut pending = self.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(p) = pending.iter_mut().find(|p| p.id == o.id && p.result.is_none()) else { return (404, "{}".into()) };
        let mut wrong = self.wrong.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if !same(p.code.as_bytes(), o.code.as_bytes()) {
            let n = match wrong.iter_mut().find(|(id, _)| *id == o.id) {
                Some((_, n)) => {
                    *n += 1;
                    *n
                }
                None => {
                    wrong.push((o.id.clone(), 1));
                    1
                }
            };
            if n >= TRIES {
                wrong.retain(|(id, _)| *id != o.id);
                p.result = Some(Err(PairError::TooMany));
                return (429, "{}".into());
            }
            p.result = Some(Err(PairError::WrongCode));
            return (403, "{}".into());
        }
        wrong.retain(|(id, _)| *id != o.id);
        self.add(PairedTv { id: o.id.clone(), name: name.clone(), token: p.token.clone(), off: false });
        self.offered.lock().unwrap_or_else(std::sync::PoisonError::into_inner).retain(|x| x.id != o.id);
        p.result = Some(Ok(name));
        (200, serde_json::json!({ "pc_name": p.pc_name, "token": p.token }).to_string())
    }
}

/// Connect a TV that offers its code: wait for its next offer, and answer it with a new key if
/// it's the code typed here.
fn pair_offered(tvs: &Tvs, id: &str, code: &str, pc_name: &str) -> Result<String, PairError> {
    pair_offered_within(tvs, id, code, pc_name, OFFER_WAIT)
}

fn pair_offered_within(tvs: &Tvs, id: &str, code: &str, pc_name: &str, wait: Duration) -> Result<String, PairError> {
    let code: String = code.chars().filter(|c| c.is_ascii_digit()).collect();
    if code.len() != 6 {
        return Err(PairError::WrongCode);
    }
    if !tvs.offered().iter().any(|(o, _)| o == id) {
        return Err(PairError::NoAnswer);
    }
    let token = format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple());
    {
        let mut pending = tvs.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        pending.retain(|p| p.id != id);
        pending.push(Pending { id: id.to_string(), code, token: token.clone(), pc_name: pc_name.to_string(), result: None });
    }
    let start = Instant::now();
    loop {
        std::thread::sleep(Duration::from_millis(50));
        let mut pending = tvs.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        // (connected again meanwhile: this one was replaced)
        let Some(i) = pending.iter().position(|p| p.token == token) else { return Err(PairError::NoAnswer) };
        if pending[i].result.is_some() || start.elapsed() >= wait {
            return pending.remove(i).result.unwrap_or(Err(PairError::NoAnswer));
        }
    }
}

/// This PC's name, as the TV shows it ("Connected to …").
pub fn pc_name() -> String {
    std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME")).unwrap_or_else(|_| "PC".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> Ipv4Addr {
        s.parse().unwrap()
    }

    fn lan(name: &str, a: &str, prefix: u8) -> crate::net::LanAddr {
        crate::net::LanAddr { name: name.into(), ip: ip(a), prefix }
    }

    #[test]
    fn a_home_network_is_its_24() {
        let (near, wider) = hosts_around(ip("192.168.1.30"), 24);
        assert_eq!(near.len(), 253);
        assert!(wider.is_empty());
        assert_eq!((near[0], near[252]), (ip("192.168.1.1"), ip("192.168.1.254")));
        assert!(!near.contains(&ip("192.168.1.30")));
        // a smaller network (a phone's hotspot, /28): its whole /24 still, as before
        let (near, wider) = hosts_around(ip("172.20.10.4"), 28);
        assert_eq!((near.len(), wider.len()), (253, 0));
    }

    #[test]
    fn a_wider_network_is_asked_up_to_a_22() {
        // a /23: both /24s, but not the network's own address or its broadcast one
        let (near, wider) = hosts_around(ip("192.168.0.20"), 23);
        assert_eq!((near.len(), wider.len()), (254, 255));
        assert!(near.contains(&ip("192.168.0.255")) && !near.contains(&ip("192.168.0.0")) && !near.contains(&ip("192.168.0.20")));
        assert!(wider.contains(&ip("192.168.1.0")) && !wider.contains(&ip("192.168.1.255")));
        assert!(wider.iter().all(|a| a.octets()[2] == 1));
        // a /22: 1022 hosts in all, the nearest first
        let (near, wider) = hosts_around(ip("10.0.5.9"), 22);
        assert_eq!(near.len() + wider.len(), 1021);
        assert_eq!(wider[0], ip("10.0.4.255"), "next to 10.0.5.x");
        assert!(wider.iter().all(|a| (4..=7).contains(&a.octets()[2])));
        // a /16: the /22 around the address, nothing further
        let (near, wider) = hosts_around(ip("10.1.200.7"), 16);
        assert_eq!(near.len() + wider.len(), 1023);
        assert!(near.iter().chain(&wider).all(|a| (200..=203).contains(&a.octets()[2]) && a.octets()[1] == 1));
        // the /22 at the start of a /8: not its network address
        let (near, wider) = hosts_around(ip("10.0.0.5"), 8);
        assert!(!near.contains(&ip("10.0.0.0")) && near.contains(&ip("10.0.0.255")) && wider.contains(&ip("10.0.3.255")));
    }

    #[test]
    fn every_home_network_is_asked_within_bounds() {
        // Wi-Fi and a second network: both /24s, the best one first
        let both = scan_list(&[lan("wlan0", "192.168.1.30", 24), lan("eth0", "192.168.50.2", 24)]);
        assert_eq!(both.len(), 506);
        assert_eq!((both[0], both[253]), (ip("192.168.1.1"), ip("192.168.50.1")));
        assert!(!both.contains(&ip("192.168.1.30")) && !both.contains(&ip("192.168.50.2")));
        // two addresses on one network: asked once, neither of them
        let one = scan_list(&[lan("wlan0", "192.168.1.30", 24), lan("wlan0", "192.168.1.31", 24)]);
        assert_eq!(one.len(), 252);
        // two wide networks: every /24 before the rest, and never more than MAX_SCAN
        let wide = scan_list(&[lan("eth0", "10.0.5.9", 16), lan("wlan0", "172.16.8.3", 20)]);
        assert_eq!(wide.len(), MAX_SCAN);
        assert_eq!((wide[0], wide[255]), (ip("10.0.5.0"), ip("172.16.8.0")), "the second /24 right after the first");
        assert!(wide.contains(&ip("172.16.8.255")));
        assert!(scan_list(&[]).is_empty());
    }

    #[test]
    fn keys_are_kept_and_removed() {
        let dir = std::env::temp_dir().join(format!("vit-tvs-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tvs.json");
        let tvs = Tvs::default();
        tvs.load(path.clone());
        assert!(!tvs.allowed("") && !tvs.allowed("x"));
        tvs.add(PairedTv { id: "tv1".into(), name: "Living room".into(), token: "k1".into(), off: false });
        tvs.add(PairedTv { id: "tv1".into(), name: "Living room".into(), token: "k2".into(), off: false }); // connected again
        assert!(!tvs.allowed("k1") && tvs.allowed("k2"));
        let again = Tvs::default();
        again.load(path.clone());
        assert!(again.allowed("k2"), "kept in the file");
        assert_eq!(again.list().len(), 1);
        assert!(again.remove_token("k2") && !again.allowed("k2"));
        // switched off: connected still, its page blank; on again, and kept in the file
        assert!(tvs.list()[0].on && tvs.showing("k2"));
        assert!(tvs.set_on("tv1", false) && !tvs.list()[0].on && !tvs.showing("k2") && tvs.allowed("k2"));
        let again = Tvs::default();
        again.load(path.clone());
        assert!(!again.list()[0].on && !again.showing("k2"));
        assert!(!tvs.set_on("nope", true) && tvs.showing("anything-else"));
        assert!(tvs.set_on("tv1", true) && tvs.showing("k2"));
        tvs.add(PairedTv { id: "tv2".into(), name: "Bedroom".into(), token: "k3".into(), off: false });
        assert!(tvs.remove_id("tv2") && !tvs.allowed("k3"));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A TV offering its code every few milliseconds, as the Tizen app does every 2 seconds.
    fn offering(tvs: &'static Tvs, code: &'static str) -> std::thread::JoinHandle<Vec<(u16, String)>> {
        std::thread::spawn(move || {
            let body = format!(r#"{{"app":"vi-translator-tizen","id":"tz-1","name":"Samsung Q60","code":"{code}"}}"#);
            let mut answers = Vec::new();
            for _ in 0..40 {
                let a = tvs.offer(body.as_bytes());
                let done = a.0 != 404;
                answers.push(a);
                if done {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            answers
        })
    }

    #[test]
    fn a_tv_offering_its_code_is_found_and_connected() {
        let tvs: &'static Tvs = Box::leak(Box::default());
        assert_eq!(tvs.offer(b"{}").0, 400);
        assert_eq!(tvs.offer(br#"{"id":"tz-1","name":"x","code":"12345"}"#).0, 400);
        // not being connected: 404, and it's among the TVs found
        assert_eq!(tvs.offer(br#"{"id":"tz-1","name":"Samsung Q60","code":"482913"}"#).0, 404);
        assert_eq!(tvs.offered(), vec![("tz-1".to_string(), "Samsung Q60".to_string())]);
        // the wrong code typed: the TV's next offer is 403, and connecting says so
        let tv = offering(tvs, "482913");
        assert_eq!(pair_offered_within(tvs, "tz-1", "111 111", "PC", Duration::from_secs(5)), Err(PairError::WrongCode));
        assert_eq!(tv.join().unwrap().last().unwrap().0, 403);
        // the right one: 200 with the key, kept as connected
        let tv = offering(tvs, "482913");
        assert_eq!(pair_offered_within(tvs, "tz-1", "482 913", "MY-PC", Duration::from_secs(5)), Ok("Samsung Q60".to_string()));
        let (status, body) = tv.join().unwrap().pop().unwrap();
        assert_eq!(status, 200);
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["pc_name"], "MY-PC");
        assert!(tvs.allowed(v["token"].as_str().unwrap()));
        // a TV that stopped offering: no answer
        assert_eq!(pair_offered_within(tvs, "tz-2", "482913", "PC", Duration::from_millis(200)), Err(PairError::NoAnswer));
    }

    #[test]
    fn too_many_wrong_codes_give_a_new_code() {
        let tvs: &'static Tvs = Box::leak(Box::default());
        assert_eq!(tvs.offer(br#"{"id":"tz-1","name":"Samsung Q60","code":"482913"}"#).0, 404);
        for i in 1..=TRIES {
            let tv = offering(tvs, "482913");
            let r = pair_offered_within(tvs, "tz-1", "000000", "PC", Duration::from_secs(5));
            let last = tv.join().unwrap().last().unwrap().0;
            if i < TRIES {
                assert_eq!((r, last), (Err(PairError::WrongCode), 403));
            } else {
                assert_eq!((r, last), (Err(PairError::TooMany), 429));
            }
        }
    }

    #[test]
    fn only_home_addresses_and_six_digit_codes() {
        let tvs = Tvs::default();
        assert_eq!(pair(&tvs, "8.8.8.8:8792", "123456", "x", "pc"), Err(PairError::NoAnswer));
        assert_eq!(pair(&tvs, "192.168.1.5:80", "123456", "x", "pc"), Err(PairError::NoAnswer));
        assert_eq!(pair(&tvs, "192.168.1.5:8792", "12345", "x", "pc"), Err(PairError::WrongCode));
    }
}
