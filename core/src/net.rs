//! The small HTTP server pieces the TV page (tv_server.rs) and the desktop's own pages share:
//! reading a request, answering it, capping connections, and this device's home-network address.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

pub fn respond(mut s: TcpStream, status: &str, mime: &str, body: &[u8]) {
    // (no CORS header: the pages are same-origin, and other websites must not read these answers)
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = s.write_all(head.as_bytes());
    let _ = s.write_all(body);
}

/// A request's target and the headers that matter here.
pub struct Request {
    pub method: String,
    pub path: String,
    pub query: String,
    pub host: String,
    pub origin: String,
    /// sent with the X-VIT header (only the VI Translator apps send it: a web page can't, without asking)
    pub vit: bool,
    /// the body, up to 4 KB (empty when longer)
    pub body: Vec<u8>,
}

/// Read a request's first line, headers and a small body: at most 8 KB, 3 s per read (a client
/// that sends more, or nothing, is dropped).
pub fn read_request(s: &TcpStream) -> Option<Request> {
    let _ = s.set_read_timeout(Some(Duration::from_secs(3)));
    let mut r = BufReader::new(s).take(8192);
    let mut line = String::new();
    if r.read_line(&mut line).ok()? == 0 {
        return None;
    }
    // "GET /path?query HTTP/1.1"
    let method = line.split_whitespace().next().unwrap_or("").to_string();
    let target = line.split_whitespace().nth(1).unwrap_or("/").to_string();
    let (path, query) = target.split_once('?').map(|(p, q)| (p.to_string(), q.to_string())).unwrap_or((target, String::new()));
    let (mut host, mut origin, mut vit, mut length) = (String::new(), String::new(), false, 0usize);
    loop {
        let mut h = String::new();
        if r.read_line(&mut h).ok()? == 0 || h.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = h.split_once(':') {
            match name.trim().to_ascii_lowercase().as_str() {
                "host" => host = value.trim().to_string(),
                "origin" => origin = value.trim().to_string(),
                "x-vit" => vit = true,
                "content-length" => length = value.trim().parse().unwrap_or(usize::MAX),
                _ => {}
            }
        }
    }
    let mut body = Vec::new();
    if (1..=4096).contains(&length) {
        body.resize(length, 0);
        if r.read_exact(&mut body).is_err() {
            body.clear();
        }
    }
    Some(Request { method, path, query, host, origin, vit, body })
}

/// Connections being served at once, at most (each has its own thread).
pub const MAX_CONNECTIONS: usize = 64;

/// Counts a connection while it's served (see `MAX_CONNECTIONS`).
pub struct Slot(&'static AtomicUsize);

impl Slot {
    pub fn take(count: &'static AtomicUsize) -> Option<Slot> {
        if count.fetch_add(1, Ordering::AcqRel) >= MAX_CONNECTIONS {
            count.fetch_sub(1, Ordering::AcqRel);
            return None;
        }
        Some(Slot(count))
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Changes every time the app starts: the pages reload themselves when they see a new one, so an
/// OBS Browser Source or a TV always runs the current drawing code (it otherwise keeps the code it
/// first loaded).
pub fn build_id() -> u64 {
    static ID: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *ID.get_or_init(|| std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0))
}

/// One of this device's IPv4 addresses on a home network, with its network's size.
#[derive(Clone, Debug, PartialEq)]
pub struct LanAddr {
    /// the interface's name ("wlan0", "en0", "Wi-Fi")
    pub name: String,
    pub ip: Ipv4Addr,
    /// the network's prefix length (24 for 255.255.255.0)
    pub prefix: u8,
}

impl LanAddr {
    /// `other` is on this address's network.
    pub fn contains(&self, other: Ipv4Addr) -> bool {
        let mask = mask(self.prefix);
        u32::from(self.ip) & mask == u32::from(other) & mask
    }
}

/// The netmask of a prefix length, as a number.
pub(crate) fn mask(prefix: u8) -> u32 {
    if prefix == 0 { 0 } else { u32::MAX << (32 - u32::from(prefix.min(32))) }
}

/// Interfaces that are rarely the home network: mobile data (Android rmnet/ccmni, iPhone pdp_ip),
/// VPNs and tunnels, virtual machines' and containers' networks. Their addresses come after the
/// others (mobile operators often give private 10.x addresses too).
fn unlikely_home(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    const PREFIXES: [&str; 17] =
        ["rmnet", "ccmni", "pdp_ip", "v4-", "clat", "utun", "tun", "tap", "ipsec", "ppp", "wg", "docker", "br-", "veth", "virbr", "vboxnet", "vmnet"];
    const PARTS: [&str; 8] = ["vethernet", "virtualbox", "vmware", "hyper-v", "wsl", "tailscale", "zerotier", "vpn"];
    PREFIXES.iter().any(|p| n.starts_with(p)) || PARTS.iter().any(|p| n.contains(p))
}

/// The addresses a home network can be on, best first: private addresses (10/8, 172.16/12,
/// 192.168/16), the likely Wi-Fi/Ethernet ones before mobile data, VPNs and virtual networks, and
/// among those the one that reaches the internet (`route`) first. Link-local addresses (169.254/16)
/// only when there's no private one; loopback, shared (100.64/10, mobile operators') and public
/// addresses never.
pub fn order_lan(addrs: Vec<LanAddr>, route: Option<Ipv4Addr>) -> Vec<LanAddr> {
    let private: Vec<LanAddr> = addrs.iter().filter(|a| a.ip.is_private()).cloned().collect();
    let mut out: Vec<LanAddr> = if private.is_empty() { addrs.into_iter().filter(|a| a.ip.is_link_local()).collect() } else { private };
    out.sort_by_key(|a| (unlikely_home(&a.name), Some(a.ip) != route));
    let mut seen = Vec::new();
    out.retain(|a| {
        let new = !seen.contains(&a.ip);
        seen.push(a.ip);
        new
    });
    out
}

/// This device's addresses on home networks, best first (see `order_lan`).
pub fn lan_addrs() -> Vec<LanAddr> {
    let addrs = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|i| match i.addr {
            if_addrs::IfAddr::V4(a) if !a.ip.is_loopback() => Some(LanAddr { name: i.name, ip: a.ip, prefix: a.prefixlen }),
            _ => None,
        })
        .collect();
    order_lan(addrs, route_ip(Ipv4Addr::new(8, 8, 8, 8)))
}

/// The local address the system would send from to reach `to` (no packet is sent).
fn route_ip(to: Ipv4Addr) -> Option<Ipv4Addr> {
    let s = UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect((to, 80)).ok()?;
    match s.local_addr().ok()?.ip() {
        std::net::IpAddr::V4(ip) if !ip.is_unspecified() => Some(ip),
        _ => None,
    }
}

/// This device's address on the home network: a private Wi-Fi/Ethernet address before mobile data,
/// VPNs and virtual networks (see `order_lan`); the one used to reach the internet when there's no
/// private address.
pub fn lan_ip() -> Option<String> {
    lan_addrs().first().map(|a| a.ip).or_else(|| route_ip(Ipv4Addr::new(8, 8, 8, 8))).map(|ip| ip.to_string())
}

/// Which of `addrs` a device at `target` reaches this one at: the one on its network, else the one
/// the system sends from to reach it (`route`) when it's a home-network address, else none.
pub fn pick_toward(addrs: &[LanAddr], target: Ipv4Addr, route: Option<Ipv4Addr>) -> Option<Ipv4Addr> {
    addrs.iter().find(|a| a.contains(target)).map(|a| a.ip).or_else(|| route.filter(|r| r.is_private() || r.is_link_local()))
}

/// This device's address as a device at `target` ("192.168.1.30", or "192.168.1.30:8792") reaches
/// it: the address on the same network (not mobile data's or a VPN's); `lan_ip()` when `target`
/// isn't an IPv4 address or no address fits.
pub fn lan_ip_toward(target: &str) -> Option<String> {
    let target = target.trim();
    let ip = target.parse::<Ipv4Addr>().ok().or_else(|| target.parse::<std::net::SocketAddrV4>().ok().map(|a| *a.ip()));
    ip.and_then(|t| pick_toward(&lan_addrs(), t, route_ip(t))).map(|ip| ip.to_string()).or_else(lan_ip)
}

/// An HTTP client builder for the core's downloads and translators. HTTPS is checked by the
/// system's own certificate check, except on Android, where Mozilla's root certificates are used
/// (the system's check there needs a JNI setup from the app).
pub fn http_client() -> reqwest::blocking::ClientBuilder {
    let b = reqwest::blocking::Client::builder();
    #[cfg(target_os = "android")]
    let b = b.tls_certs_only(webpki_root_certs::TLS_SERVER_ROOT_CERTS.iter().filter_map(|c| reqwest::Certificate::from_der(c.as_ref()).ok()));
    b
}

/// A Host header that is an IP address or "localhost" (with or without a port).
pub fn host_is_address(host: &str) -> bool {
    let name = if let Some(rest) = host.strip_prefix('[') {
        rest.split_once(']').map(|(ip, _)| ip).unwrap_or("")
    } else {
        host.rsplit_once(':').map(|(h, _)| h).unwrap_or(host)
    };
    name.eq_ignore_ascii_case("localhost") || name.parse::<std::net::IpAddr>().is_ok()
}

/// A query parameter's value ("" when it isn't there).
pub fn param<'a>(query: &'a str, name: &str) -> &'a str {
    query.split('&').find_map(|kv| kv.split_once('=').filter(|(k, _)| *k == name).map(|(_, v)| v)).unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    fn request(raw: &[u8]) -> Option<Request> {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut c = TcpStream::connect(l.local_addr().unwrap()).unwrap();
        c.write_all(raw).unwrap();
        let (s, _) = l.accept().unwrap();
        read_request(&s)
    }

    #[test]
    fn reads_the_target_and_host() {
        let r = request(b"GET /tv.json?t=k&h=7 HTTP/1.1\r\nHost: 192.168.1.5:8791\r\nX-Other: y\r\n\r\n").unwrap();
        assert_eq!((r.path.as_str(), r.query.as_str(), r.host.as_str()), ("/tv.json", "t=k&h=7", "192.168.1.5:8791"));
        assert_eq!((param(&r.query, "t"), param(&r.query, "h"), param(&r.query, "x")), ("k", "7", ""));
    }

    #[test]
    fn reads_a_small_body_and_the_vit_header() {
        let r = request(b"POST /vit-tv/offer HTTP/1.1\r\nHost: 192.168.1.5:8791\r\nX-VIT: 1\r\nContent-Length: 7\r\n\r\n{\"a\":1}").unwrap();
        assert_eq!((r.method.as_str(), r.path.as_str(), r.vit, r.body.as_slice()), ("POST", "/vit-tv/offer", true, &b"{\"a\":1}"[..]));
        let r = request(b"POST /x HTTP/1.1\r\nContent-Length: 99999\r\n\r\nabc").unwrap();
        assert!(!r.vit && r.body.is_empty(), "too long: not read");
    }

    #[test]
    fn a_huge_request_is_cut_short() {
        // a header with no end: read up to 8 KB, then given up (no unbounded memory)
        let mut raw = b"GET / HTTP/1.1\r\nHost: 127.0.0.1:1\r\nX: ".to_vec();
        raw.extend(std::iter::repeat_n(b'a', 64 * 1024));
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let t = std::thread::spawn(move || {
            let mut c = TcpStream::connect(addr).unwrap();
            let _ = c.write_all(&raw);
        });
        let (s, _) = l.accept().unwrap();
        let r = read_request(&s);
        assert!(r.is_some_and(|r| r.path == "/"), "the first line is read, the endless header isn't");
        let _ = t.join();
    }

    fn addr(name: &str, ip: &str, prefix: u8) -> LanAddr {
        LanAddr { name: name.into(), ip: ip.parse().unwrap(), prefix }
    }

    fn ips(addrs: &[LanAddr]) -> Vec<String> {
        addrs.iter().map(|a| a.ip.to_string()).collect()
    }

    #[test]
    fn home_addresses_come_first() {
        // a phone on mobile data (the operator's private 10.x) and Wi-Fi, mobile data reaching the internet
        let phone = vec![addr("rmnet_data0", "10.45.3.9", 30), addr("lo", "127.0.0.1", 8), addr("wlan0", "192.168.1.30", 24)];
        assert_eq!(ips(&order_lan(phone, Some("10.45.3.9".parse().unwrap()))), ["192.168.1.30", "10.45.3.9"]);
        // an iPhone: mobile data with a shared (CGNAT) address, a VPN, Wi-Fi
        let iphone = vec![addr("pdp_ip0", "100.70.2.1", 32), addr("utun4", "10.8.0.2", 24), addr("en0", "172.20.10.4", 28)];
        assert_eq!(ips(&order_lan(iphone, Some("10.8.0.2".parse().unwrap()))), ["172.20.10.4", "10.8.0.2"]);
        // a PC with Hyper-V's network and two home networks: the one reaching the internet first
        let pc = vec![addr("vEthernet (WSL)", "172.29.16.1", 20), addr("Ethernet", "192.168.0.10", 24), addr("Wi-Fi", "10.0.0.5", 16)];
        assert_eq!(ips(&order_lan(pc.clone(), Some("10.0.0.5".parse().unwrap()))), ["10.0.0.5", "192.168.0.10", "172.29.16.1"]);
        assert_eq!(ips(&order_lan(pc, None)), ["192.168.0.10", "10.0.0.5", "172.29.16.1"], "no internet: as listed");
        // public, shared and loopback addresses never; link-local only without a private one
        let odd = vec![addr("eth0", "203.0.113.5", 24), addr("en5", "169.254.10.2", 16), addr("pdp_ip0", "100.64.0.1", 10)];
        assert_eq!(ips(&order_lan(odd.clone(), None)), ["169.254.10.2"]);
        let with_private = [odd, vec![addr("en0", "192.168.50.2", 24)]].concat();
        assert_eq!(ips(&order_lan(with_private, None)), ["192.168.50.2"]);
        assert!(order_lan(vec![addr("lo", "127.0.0.1", 8)], None).is_empty());
        // the same address twice (two names for one adapter): once
        assert_eq!(ips(&order_lan(vec![addr("a", "192.168.1.2", 24), addr("b", "192.168.1.2", 24)], None)), ["192.168.1.2"]);
    }

    #[test]
    fn the_address_toward_a_tv_is_on_its_network() {
        let addrs = vec![addr("wlan0", "192.168.1.30", 24), addr("eth0", "10.0.4.2", 22), addr("rmnet0", "10.200.0.7", 30)];
        let ip = |s: &str| s.parse::<Ipv4Addr>().unwrap();
        assert_eq!(pick_toward(&addrs, ip("192.168.1.77"), None), Some(ip("192.168.1.30")));
        assert_eq!(pick_toward(&addrs, ip("10.0.6.200"), None), Some(ip("10.0.4.2")), "a /22");
        // not on any of them (a routed home network): the address the system sends from, if a home one
        assert_eq!(pick_toward(&addrs, ip("192.168.7.5"), Some(ip("192.168.1.30"))), Some(ip("192.168.1.30")));
        assert_eq!(pick_toward(&addrs, ip("192.168.7.5"), Some(ip("100.70.2.1"))), None);
        assert!(addr("x", "192.168.1.30", 32).contains(ip("192.168.1.30")) && !addr("x", "192.168.1.30", 32).contains(ip("192.168.1.31")));
        assert!(addr("x", "10.1.2.3", 0).contains(ip("8.8.8.8")));
    }

    #[test]
    fn connections_are_capped() {
        static COUNT: AtomicUsize = AtomicUsize::new(0);
        let slots: Vec<Slot> = (0..MAX_CONNECTIONS).map(|_| Slot::take(&COUNT).unwrap()).collect();
        assert!(Slot::take(&COUNT).is_none());
        drop(slots);
        assert!(Slot::take(&COUNT).is_some());
    }

    #[test]
    fn the_pages_answer_addresses_only() {
        for ok in ["192.168.1.20:8791", "192.168.1.20", "127.0.0.1:8791", "localhost:8791", "[::1]:8791", "[fe80::1]:8791"] {
            assert!(host_is_address(ok), "{ok}");
        }
        for bad in ["evil.example:8791", "evil.example", "", "192.168.1.20.evil.example:8791", "[evil]:8791"] {
            assert!(!host_is_address(bad), "{bad}");
        }
    }

    // Every answer written by hand ends its header lines in CRLF: a line break inside the literal
    // would be a bare LF on the wire (browsers take it, strict HTTP clients don't).
    #[test]
    fn hand_written_answers_use_crlf() {
        for (file, src) in [("net.rs", include_str!("net.rs")), ("tv_server.rs", include_str!("tv_server.rs"))] {
            // (the answers, not the tests that look at them)
            for line in src.lines().take_while(|l| !l.starts_with("#[cfg(test)]")) {
                // (the pattern is split, so this line isn't one)
                if line.contains(concat!("\"HTTP", "/1.1 ")) {
                    assert!(line.contains("\\r\\n"), "{file}: a header line without \\r\\n: {}", line.trim());
                }
            }
        }
    }
}
