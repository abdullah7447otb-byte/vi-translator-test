//! The Mac's own network, as it bears on the PS5's picture: is the Ethernet up and at full speed, is Wi-Fi on
//! beside it, and is AWDL (AirDrop, AirPlay, Handoff) active. AWDL makes the Wi-Fi radio hop channels now and
//! then, and packets wait meanwhile; a Mac on a cable with Wi-Fi still on can stutter for it. Only read, never
//! changed: the advice says what the player can do (System Settings, or a cable), nothing runs as root.
//!
//! The readings are `ifconfig` and `networksetup`'s, parsed here (tested on their text); taken at most every
//! 20 s.

use std::sync::Mutex;
use std::time::{Duration, Instant};

/// What was found.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize)]
pub struct Net {
    /// The wired port in use (en0…), its speed in Mb/s and whether full duplex.
    pub ethernet: Option<(String, u32, bool)>,
    /// Wi-Fi's own port is up (associated to a network or not).
    pub wifi_on: bool,
    /// The awdl0 interface is active.
    pub awdl_active: bool,
}

/// The ports `networksetup -listallhardwareports` names: (port, device), "Wi-Fi" and the Ethernet ones.
#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
pub fn ports(listing: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut port: Option<String> = None;
    for line in listing.lines() {
        if let Some(p) = line.strip_prefix("Hardware Port: ") {
            port = Some(p.trim().to_string());
        } else if let (Some(d), Some(p)) = (line.strip_prefix("Device: "), port.take()) {
            out.push((p, d.trim().to_string()));
        }
    }
    out
}

/// One interface's block of `ifconfig -a`: whether it is active, and its media line ("1000baseT <full-duplex>").
#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
pub fn iface<'a>(ifconfig: &'a str, dev: &str) -> Option<(bool, &'a str)> {
    let start = ifconfig.lines().position(|l| l.starts_with(&format!("{dev}:")))?;
    let mut active = false;
    let mut media = "";
    for l in ifconfig.lines().skip(start + 1).take_while(|l| l.starts_with(|c: char| c.is_whitespace())) {
        let l = l.trim();
        if let Some(s) = l.strip_prefix("status: ") {
            active = s.trim() == "active";
        } else if let Some(m) = l.strip_prefix("media: ") {
            media = m;
        }
    }
    Some((active, media))
}

/// A media line's speed (Mb/s) and duplex: "autoselect (1000baseT <full-duplex,flow-control>)" → (1000, true).
#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
pub fn speed(media: &str) -> Option<(u32, bool)> {
    let i = media.find("base")?;
    // ("10Gbase-T": in Gb/s)
    let (head, giga) = match media[..i].strip_suffix('G') {
        Some(h) => (h, true),
        None => (&media[..i], false),
    };
    let digits: String = head.chars().rev().take_while(|c| c.is_ascii_digit()).collect::<Vec<_>>().into_iter().rev().collect();
    let mut n: u32 = digits.parse().ok()?;
    if giga {
        n *= 1000;
    }
    Some((n, media.contains("full-duplex")))
}

/// The readings put together.
#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
pub fn read(ports_listing: &str, ifconfig: &str) -> Net {
    let mut n = Net::default();
    for (port, dev) in ports(ports_listing) {
        let Some((active, media)) = iface(ifconfig, &dev) else { continue };
        if port == "Wi-Fi" || port == "AirPort" {
            n.wifi_on |= active;
        } else if active
            && (port.contains("Ethernet") || port.contains("LAN") || port.contains("Thunderbolt"))
            && n.ethernet.is_none()
            && let Some((mbps, full)) = speed(media)
        {
            n.ethernet = Some((dev, mbps, full));
        }
    }
    n.awdl_active = iface(ifconfig, "awdl0").is_some_and(|(a, _)| a);
    n
}

/// What the player can do, in English (the window translates it): nothing when all is well.
pub fn advice(n: &Net) -> Vec<&'static str> {
    let mut out = Vec::new();
    match &n.ethernet {
        Some((_, mbps, full)) if *mbps < 1000 || !*full => out.push("The Mac's cable runs below 1 Gb/s full duplex: try another cable or port on the router"),
        Some(_) if n.wifi_on => out.push("Wi-Fi is on beside the cable: turn Wi-Fi off in the menu bar while you play (AirDrop and AirPlay stop with it)"),
        None if n.awdl_active => out.push("AirDrop/AirPlay (AWDL) is active on Wi-Fi: it can delay the stream now and then. A cable helps most"),
        _ => {}
    }
    out
}

static LAST: Mutex<Option<(Instant, Net)>> = Mutex::new(None);

/// The Mac's network now (at most 20 s old). Other systems: nothing found.
pub fn now() -> Net {
    let mut last = LAST.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some((at, n)) = last.as_ref()
        && at.elapsed() < Duration::from_secs(20)
    {
        return n.clone();
    }
    let n = take();
    *last = Some((Instant::now(), n.clone()));
    n
}

#[cfg(target_os = "macos")]
fn take() -> Net {
    let run = |cmd: &str, args: &[&str]| {
        std::process::Command::new(cmd).args(args).output().ok().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default()
    };
    read(&run("/usr/sbin/networksetup", &["-listallhardwareports"]), &run("/sbin/ifconfig", &["-a"]))
}

#[cfg(not(target_os = "macos"))]
fn take() -> Net {
    Net::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    const PORTS: &str = "\nHardware Port: Ethernet\nDevice: en0\nEthernet Address: aa:bb\n\nHardware Port: Wi-Fi\nDevice: en1\nEthernet Address: cc:dd\n\nHardware Port: Thunderbolt Bridge\nDevice: bridge0\n";
    const IFCONFIG: &str = "en0: flags=8863<UP,BROADCAST,SMART,RUNNING> mtu 1500\n\toptions=50b<RXCSUM>\n\tmedia: autoselect (1000baseT <full-duplex,flow-control,energy-efficient-ethernet>)\n\tstatus: active\nen1: flags=8863<UP> mtu 1500\n\tmedia: autoselect\n\tstatus: active\nawdl0: flags=8943<UP> mtu 1500\n\tmedia: autoselect\n\tstatus: active\nbridge0: flags=8822 mtu 1500\n\tstatus: inactive\n";

    #[test]
    fn a_wired_mac_with_wifi_on_is_told_to_turn_it_off() {
        let n = read(PORTS, IFCONFIG);
        assert_eq!(n.ethernet, Some(("en0".to_string(), 1000, true)));
        assert!(n.wifi_on && n.awdl_active);
        assert_eq!(advice(&n), vec!["Wi-Fi is on beside the cable: turn Wi-Fi off in the menu bar while you play (AirDrop and AirPlay stop with it)"]);
    }

    #[test]
    fn a_slow_or_half_duplex_cable_is_named() {
        let slow = IFCONFIG.replace("1000baseT <full-duplex", "100baseTX <full-duplex");
        assert_eq!(read(PORTS, &slow).ethernet, Some(("en0".to_string(), 100, true)));
        assert!(advice(&read(PORTS, &slow))[0].contains("below 1 Gb/s"));
        let half = IFCONFIG.replace("full-duplex", "half-duplex");
        assert!(advice(&read(PORTS, &half))[0].contains("below 1 Gb/s"));
        assert_eq!(speed("autoselect (10Gbase-T <full-duplex>)"), Some((10000, true)));
    }

    #[test]
    fn all_well_is_quiet() {
        let wired_only = IFCONFIG.replace("en1: flags=8863<UP> mtu 1500\n\tmedia: autoselect\n\tstatus: active", "en1: flags=8863<UP> mtu 1500\n\tmedia: autoselect\n\tstatus: inactive");
        assert!(advice(&read(PORTS, &wired_only)).is_empty());
        // (no cable, AWDL active: said once, gently)
        let wifi_only = IFCONFIG.replace("\tstatus: active\nen1", "\tstatus: inactive\nen1");
        assert!(advice(&read(PORTS, &wifi_only))[0].contains("AWDL"));
    }
}
