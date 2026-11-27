//! The stream's data channels: the message channel's JSON greetings, the control channel's
//! messages, and the input channel's binary packets (little-endian), as xbox.com's player sends them.

use serde_json::json;

/// Input packet kinds (bits of the header's first field)
pub mod report {
    pub const METADATA: u16 = 1;
    pub const GAMEPAD: u16 = 2;
    pub const CLIENT_METADATA: u16 = 8;
    pub const SERVER_METADATA: u16 = 16;
    pub const VIBRATION: u16 = 128;
}

/// Controller buttons, as the console takes them.
pub mod buttons {
    pub const NEXUS: u16 = 2;
    pub const MENU: u16 = 4;
    pub const VIEW: u16 = 8;
    pub const A: u16 = 16;
    pub const B: u16 = 32;
    pub const X: u16 = 64;
    pub const Y: u16 = 128;
    pub const UP: u16 = 256;
    pub const DOWN: u16 = 512;
    pub const LEFT: u16 = 1024;
    pub const RIGHT: u16 = 2048;
    pub const LB: u16 = 4096;
    pub const RB: u16 = 8192;
    pub const LS: u16 = 16384;
    pub const RS: u16 = 32768;
}

/// One controller's state. Sticks: −32767…32767 with up positive (as on the console); triggers
/// 0…65535.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Gamepad {
    pub buttons: u16,
    pub lx: i16,
    pub ly: i16,
    pub rx: i16,
    pub ry: i16,
    pub lt: u16,
    pub rt: u16,
}

impl Gamepad {
    /// Which inputs are in use ("physicality" bits), as xbox.com's player reports them.
    pub fn physicality(&self) -> u32 {
        use buttons::*;
        let b = self.buttons;
        let map = [
            (b & UP != 0, 0x1),
            (b & DOWN != 0, 0x2),
            (b & LEFT != 0, 0x4),
            (b & RIGHT != 0, 0x8),
            (b & MENU != 0, 0x10),
            (b & VIEW != 0, 0x20),
            (b & LS != 0, 0x40),
            (b & RS != 0, 0x80),
            (b & LB != 0, 0x100),
            (b & RB != 0, 0x200),
            (b & NEXUS != 0, 0x400),
            (b & A != 0, 0x1000),
            (b & B != 0, 0x2000),
            (b & X != 0, 0x4000),
            (b & Y != 0, 0x8000),
            (self.lt > 0, 0x10000),
            (self.rt > 0, 0x20000),
            (self.lx != 0, 0x40000),
            (self.ly != 0, 0x80000),
            (self.rx != 0, 0x100000),
            (self.ry != 0, 0x200000),
        ];
        map.iter().filter(|(on, _)| *on).fold(0, |p, (_, bit)| p | bit)
    }
}

fn header(kind: u16, seq: u32, ms: f64, extra: usize) -> Vec<u8> {
    let mut p = Vec::with_capacity(14 + extra);
    p.extend_from_slice(&kind.to_le_bytes());
    p.extend_from_slice(&seq.to_le_bytes());
    p.extend_from_slice(&ms.to_le_bytes());
    p
}

/// Who this client is (sent once when the input channel opens): no touch points. 15 bytes.
pub fn client_metadata(seq: u32, ms: f64) -> Vec<u8> {
    let mut p = header(report::CLIENT_METADATA, seq, ms, 1);
    p.push(0);
    p
}

/// One frame's timings (made up: nothing is shown at the console's pace), which the console takes as
/// a sign of life. 43 bytes.
pub fn metadata(seq: u32, ms: f64, epoch_ms: u32) -> Vec<u8> {
    let mut p = header(report::METADATA, seq, ms, 29);
    p.push(1);
    let now = ms.max(0.0) as u64 as u32;
    p.extend_from_slice(&epoch_ms.to_le_bytes());
    for _ in 0..5 {
        p.extend_from_slice(&now.to_le_bytes());
    }
    p.extend_from_slice(&epoch_ms.to_le_bytes());
    p
}

/// The controller at `index`, as one gamepad frame. 14 + 1 + 23 bytes.
pub fn gamepad(seq: u32, ms: f64, index: u8, g: &Gamepad) -> Vec<u8> {
    let mut p = header(report::GAMEPAD, seq, ms, 24);
    p.push(1);
    p.push(index);
    p.extend_from_slice(&g.buttons.to_le_bytes());
    for a in [g.lx, g.ly, g.rx, g.ry] {
        p.extend_from_slice(&a.to_le_bytes());
    }
    p.extend_from_slice(&g.lt.to_le_bytes());
    p.extend_from_slice(&g.rt.to_le_bytes());
    p.extend_from_slice(&g.physicality().to_le_bytes());
    p.extend_from_slice(&0u32.to_le_bytes());
    p
}

/// A rumble the console asks for: motors and triggers 0–100 %, for how long.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rumble {
    pub left: u8,
    pub right: u8,
    pub duration_ms: u16,
}

/// A rumble in a packet from the console on the input channel (none if it carries none).
pub fn rumble_in(d: &[u8]) -> Option<Rumble> {
    let kind = u16::from_le_bytes(d.get(0..2)?.try_into().ok()?);
    let mut o = 2;
    if kind & report::SERVER_METADATA != 0 {
        o += 8;
    }
    if kind & report::VIBRATION == 0 {
        return None;
    }
    let v = d.get(o..o + 11)?;
    Some(Rumble { left: v[2].min(100), right: v[3].min(100), duration_ms: u16::from_le_bytes([v[6], v[7]]) })
}

/// The message channel's greeting, when it opens.
pub fn handshake(cv: &str) -> String {
    json!({ "type": "Handshake", "version": "messageV1", "id": uuid::Uuid::new_v4().to_string(), "cv": cv }).to_string()
}

/// The six messages xbox.com's player sends after the console's HandshakeAck (content as a JSON
/// string, as the web player sends it). `cv(i)` numbers each.
pub fn greetings(install_id: &str, mut cv: impl FnMut() -> String) -> Vec<String> {
    let list = [
        ("/streaming/systemUi/configuration", json!({ "version": [0, 2, 0], "systemUis": [10, 19, 31, 27, 32, -44, 40, 41, -43] })),
        ("/streaming/properties/clientappinstallidchanged", json!({ "clientAppInstallId": install_id })),
        ("/streaming/characteristics/orientationchanged", json!({ "orientation": 0 })),
        ("/streaming/characteristics/touchinputenabledchanged", json!({ "touchInputEnabled": false })),
        ("/streaming/characteristics/clientdevicecapabilities", json!({})),
        (
            "/streaming/characteristics/dimensionschanged",
            json!({ "horizontal": 1920, "vertical": 1080, "preferredWidth": 1920, "preferredHeight": 1080,
                    "safeAreaLeft": 0, "safeAreaTop": 0, "safeAreaRight": 1920, "safeAreaBottom": 1080, "supportsCustomResolution": true }),
        ),
    ];
    list.into_iter()
        .map(|(target, content)| {
            json!({ "type": "Message", "content": content.to_string(), "id": uuid::Uuid::new_v4().to_string(), "target": target, "cv": cv() })
                .to_string()
        })
        .collect()
}

/// Is this message the console's HandshakeAck?
pub fn is_handshake_ack(text: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(text).is_ok_and(|v| v["type"] == "HandshakeAck")
}

/// The control channel: allowed in (the web player's key).
pub fn authorization_request() -> String {
    json!({ "message": "authorizationRequest", "accessKey": "4BDB3609-C1F1-4195-9B37-FEFF45DA8B8E" }).to_string()
}

/// The control channel: controller `index` removed or added.
pub fn gamepad_changed(index: u8, added: bool) -> String {
    json!({ "message": "gamepadChanged", "gamepadIndex": index, "wasAdded": added }).to_string()
}

/// The control channel: a whole picture, please.
pub fn keyframe_request() -> String {
    json!({ "message": "videoKeyframeRequested", "ifrRequested": true }).to_string()
}

/// The offer as sent to the console: its sound asked for in stereo, as the web player does.
pub fn stereo(sdp: &str) -> String {
    if sdp.contains("stereo=1") { sdp.to_string() } else { sdp.replace("useinbandfec=1", "useinbandfec=1;stereo=1") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packets_have_the_web_player_s_layout() {
        let c = client_metadata(7, 1.5);
        assert_eq!(c.len(), 15);
        assert_eq!(&c[0..2], &8u16.to_le_bytes());
        assert_eq!(&c[2..6], &7u32.to_le_bytes());
        assert_eq!(&c[6..14], &1.5f64.to_le_bytes());
        assert_eq!(c[14], 0);

        let m = metadata(1, 2000.0, 0xAABBCCDD);
        assert_eq!(m.len(), 43);
        assert_eq!(m[14], 1, "one frame");
        assert_eq!(&m[15..19], &0xAABBCCDDu32.to_le_bytes());
        assert_eq!(&m[19..23], &2000u32.to_le_bytes());
        assert_eq!(&m[39..43], &0xAABBCCDDu32.to_le_bytes());
    }

    #[test]
    fn a_gamepad_frame_is_23_bytes_after_its_count() {
        let g = Gamepad { buttons: buttons::A | buttons::UP, lx: -32767, ly: 100, rx: 0, ry: 0, lt: 65535, rt: 0 };
        let p = gamepad(3, 0.0, 0, &g);
        assert_eq!(p.len(), 14 + 1 + 23);
        assert_eq!(&p[0..2], &2u16.to_le_bytes());
        assert_eq!(p[14], 1, "one frame");
        assert_eq!(p[15], 0, "controller 0");
        assert_eq!(u16::from_le_bytes([p[16], p[17]]), 16 | 256);
        assert_eq!(i16::from_le_bytes([p[18], p[19]]), -32767);
        assert_eq!(i16::from_le_bytes([p[20], p[21]]), 100);
        assert_eq!(u16::from_le_bytes([p[26], p[27]]), 65535);
        let phys = u32::from_le_bytes(p[30..34].try_into().unwrap());
        assert_eq!(phys, 0x1 | 0x1000 | 0x10000 | 0x40000 | 0x80000);
        assert_eq!(&p[34..38], &[0, 0, 0, 0]);
    }

    #[test]
    fn rumbles_are_read() {
        let mut p = vec![];
        p.extend_from_slice(&report::VIBRATION.to_le_bytes());
        p.extend_from_slice(&[0, 0, 80, 40, 0, 0]);
        p.extend_from_slice(&250u16.to_le_bytes());
        p.extend_from_slice(&0u16.to_le_bytes());
        p.push(0);
        assert_eq!(rumble_in(&p), Some(Rumble { left: 80, right: 40, duration_ms: 250 }));
        assert_eq!(rumble_in(&report::SERVER_METADATA.to_le_bytes()), None);
        assert_eq!(rumble_in(&[1]), None);
    }

    #[test]
    fn the_greetings_are_six_numbered_messages() {
        let mut n = 0;
        let g = greetings("id", || {
            n += 1;
            n.to_string()
        });
        assert_eq!(g.len(), 6);
        let first: serde_json::Value = serde_json::from_str(&g[0]).unwrap();
        assert_eq!(first["target"], "/streaming/systemUi/configuration");
        assert_eq!(first["cv"], "1");
        assert!(first["content"].is_string(), "content goes as a JSON string");
        assert!(is_handshake_ack(r#"{"type":"HandshakeAck","version":"messageV1"}"#));
        assert!(!is_handshake_ack("{}"));
    }

    #[test]
    fn the_offer_asks_for_stereo_once() {
        let s = stereo("a=fmtp:111 minptime=10;useinbandfec=1\r\n");
        assert_eq!(s, "a=fmtp:111 minptime=10;useinbandfec=1;stereo=1\r\n");
        assert_eq!(stereo(&s), s);
    }
}
