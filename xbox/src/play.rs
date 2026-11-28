//! Playing the Xbox on this computer: Remote Play's session on Microsoft's servers, then the console's
//! stream over WebRTC (webrtc-rs): its picture decoded (OpenH264, on the CPU, into planes whose
//! layout is known) and handed to the app through [`Screen`], its sound on the speakers and this
//! computer's controllers sent to it (both through SDL, as for the PS5).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use tokio::sync::Notify;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use webrtc::api::APIBuilder;
use webrtc::api::interceptor_registry::register_default_interceptors;
use webrtc::api::media_engine::MediaEngine;
use webrtc::data_channel::RTCDataChannel;
use webrtc::data_channel::data_channel_init::RTCDataChannelInit;
use webrtc::data_channel::data_channel_message::DataChannelMessage;
use webrtc::data_channel::data_channel_state::RTCDataChannelState;
use webrtc::ice_transport::ice_candidate::{RTCIceCandidate, RTCIceCandidateInit};
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::interceptor::registry::Registry;
use webrtc::media::io::sample_builder::SampleBuilder;
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::rtcp::payload_feedbacks::picture_loss_indication::PictureLossIndication;
use webrtc::rtp::codecs::h264::H264Packet;
use webrtc::rtp_transceiver::RTCRtpTransceiverInit;
use webrtc::rtp_transceiver::rtp_codec::RTPCodecType;
use webrtc::rtp_transceiver::rtp_transceiver_direction::RTCRtpTransceiverDirection;
use webrtc::track::track_remote::TrackRemote;

use crate::auth::Account;
use crate::home::{Candidate, Home, failure, with_teredo};
use crate::input::{self, Gamepad, buttons};
use crate::{Error, Result};

/// One decoded picture, 4:2:0: a full-size luma plane and quarter-size chroma planes, each with its
/// own row length.
pub struct I420 {
    pub w: u32,
    pub h: u32,
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
    pub y_stride: usize,
    pub uv_stride: usize,
}

/// Where the stream's pictures go (on the decoder's thread).
pub trait Screen: Send + Sync {
    fn frame(&self, f: I420);
    fn log(&self, _level: i32, _text: &str) {}
}

/// What the player chose, read while it plays.
pub struct Settings {
    pub sound: AtomicBool,
    /// 0–100
    pub volume: AtomicU32,
    /// this computer's controllers play on the console
    pub controllers: AtomicBool,
}

impl Settings {
    pub fn new(sound: bool, volume: u32, controllers: bool) -> Self {
        Settings { sound: AtomicBool::new(sound), volume: AtomicU32::new(volume.min(100)), controllers: AtomicBool::new(controllers) }
    }

    /// The factor samples are multiplied by: the volume on a curve the ear hears as even steps.
    pub fn gain(&self) -> f32 {
        if !self.sound.load(Ordering::Relaxed) {
            return 0.0;
        }
        let v = self.volume.load(Ordering::Relaxed).min(100) as f32 / 100.0;
        v * v
    }
}

/// Where the stream is.
#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    Starting,
    /// the console was asleep: woken, waiting for it
    Waking,
    /// the session started: waiting for the console to get ready
    Waiting,
    /// WebRTC: offer, answer, network
    Connecting,
    Playing,
    /// over, and why (English; the app translates it)
    Ended(String),
    Failed(String),
}

impl Status {
    pub fn over(&self) -> bool {
        matches!(self, Status::Ended(_) | Status::Failed(_))
    }
}

/// What the player has done so far (for the app's status line).
#[derive(Default)]
pub struct Stats {
    pub frames: AtomicU64,
    pub bad_frames: AtomicU64,
    pub controllers: AtomicU64,
}

const STUN: [&str; 3] = ["stun:worldaz.relay.teams.microsoft.com:3478", "stun:stun.l.google.com:19302", "stun:stun1.l.google.com:19302"];

pub const NO_PICTURE: &str = "The Xbox sent no picture. Check that Remote Play works on this network, then try again.";
pub const NAT_FAILED: &str = "Couldn't reach the Xbox over the network (Remote Play needs the router to allow it: UPnP, or the Xbox's ports open).";
pub const ENDED: &str = "The Xbox ended the stream.";
pub const NOT_LISTED: &str = "This Xbox isn't on your Microsoft account's list any more. Choose it again in Source.";
pub const NOT_READY: &str = "The Xbox didn't get ready to stream. Check that it's on or asleep (not off), then try again.";
pub const SIGNED_OUT: &str = "Signed out of Microsoft. Sign in again in Source.";
pub const REFUSED_SIGN_IN: &str = "Microsoft refused the sign-in for Remote Play. Sign in again in Source.";

/// The stream from the console with its sound and controllers, while it lives.
pub struct Player {
    ctx: Arc<Ctx>,
    threads: Vec<std::thread::JoinHandle<()>>,
    pub stats: Arc<Stats>,
}

/// What the SDL thread is told.
enum ToSdl {
    Pcm(Vec<i16>),
    Rumble(input::Rumble),
}

struct Ctx {
    account: Arc<Account>,
    console: String,
    resolution: u32,
    tz_minutes: i32,
    settings: Arc<Settings>,
    screen: Arc<dyn Screen>,
    stats: Arc<Stats>,
    status: Mutex<Status>,
    stop: AtomicBool,
    wake: Notify,
    started: Instant,
    seq: AtomicU32,
    cv: AtomicU32,
    channels: Mutex<HashMap<&'static str, Arc<RTCDataChannel>>>,
    local: Mutex<Vec<Candidate>>,
    to_decoder: Mutex<Option<Sender<Bytes>>>,
    to_sdl: Mutex<Sender<ToSdl>>,
    /// the decoder lost its way: a whole picture is needed
    keyframe_wanted: AtomicBool,
}

impl Ctx {
    fn set(&self, s: Status) {
        let mut st = self.status.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        // (the first end stays: what ended the stream, not what followed)
        if !st.over() {
            *st = s;
        }
    }
    fn ms(&self) -> f64 {
        self.started.elapsed().as_secs_f64() * 1000.0
    }
    fn next_seq(&self) -> u32 {
        self.seq.fetch_add(1, Ordering::Relaxed)
    }
    fn next_cv(&self) -> String {
        (self.cv.fetch_add(1, Ordering::Relaxed) + 1).to_string()
    }
    fn channel(&self, label: &str) -> Option<Arc<RTCDataChannel>> {
        self.channels.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(label).cloned().filter(|c| c.ready_state() == RTCDataChannelState::Open)
    }
    async fn text(&self, label: &str, s: String) {
        if let Some(c) = self.channel(label) {
            let _ = c.send_text(s).await;
        }
    }
    async fn input(&self, p: Vec<u8>) {
        if let Some(c) = self.channel("input") {
            let _ = c.send(&Bytes::from(p)).await;
        }
    }
    fn log(&self, level: i32, text: &str) {
        self.screen.log(level, text);
    }
}

impl Player {
    /// Starts streaming from the console `console` (its server id) at `resolution` (1080 or 720):
    /// pictures to `screen`, sound and controllers as `settings` say.
    pub fn start(account: Arc<Account>, console: &str, resolution: u32, tz_minutes: i32, settings: Arc<Settings>, screen: Arc<dyn Screen>) -> Player {
        let (to_sdl, from_stream) = channel();
        let (to_decoder, frames) = channel::<Bytes>();
        let (pads_tx, pads_rx) = unbounded_channel();
        let stats = Arc::new(Stats::default());
        let ctx = Arc::new(Ctx {
            account,
            console: console.to_string(),
            resolution,
            tz_minutes,
            settings: settings.clone(),
            screen,
            stats: stats.clone(),
            status: Mutex::new(Status::Starting),
            stop: AtomicBool::new(false),
            wake: Notify::new(),
            started: Instant::now(),
            seq: AtomicU32::new(0),
            cv: AtomicU32::new(0),
            channels: Mutex::new(HashMap::new()),
            local: Mutex::new(Vec::new()),
            to_decoder: Mutex::new(Some(to_decoder)),
            to_sdl: Mutex::new(to_sdl),
            keyframe_wanted: AtomicBool::new(false),
        });
        let mut threads = Vec::new();
        let c = ctx.clone();
        threads.extend(std::thread::Builder::new().name("xbox-stream".into()).spawn(move || stream_thread(c, pads_rx)).ok());
        let c = ctx.clone();
        threads.extend(std::thread::Builder::new().name("xbox-video".into()).spawn(move || decoder_thread(c, frames)).ok());
        let c = ctx.clone();
        threads.extend(std::thread::Builder::new().name("xbox-sdl".into()).spawn(move || sdl_thread(c, from_stream, pads_tx)).ok());
        Player { ctx, threads, stats }
    }

    pub fn status(&self) -> Status {
        self.ctx.status.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.ctx.stop.store(true, Ordering::Relaxed);
        self.ctx.wake.notify_waiters();
        self.ctx.wake.notify_one();
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

fn webrtc_err(e: webrtc::Error) -> Error {
    Error::Other(format!("WebRTC: {e}"))
}

/// The session and the peer on a runtime of their own; at the end the session is closed on
/// Microsoft's side too.
fn stream_thread(ctx: Arc<Ctx>, pads: UnboundedReceiver<Gamepad>) {
    let rt = match tokio::runtime::Builder::new_multi_thread().worker_threads(2).thread_name("xbox-rt").enable_all().build() {
        Ok(r) => r,
        Err(e) => return ctx.set(Status::Failed(format!("{e}"))),
    };
    rt.block_on(async {
        let mut home: Option<Home> = None;
        let mut pc: Option<Arc<RTCPeerConnection>> = None;
        let r = tokio::select! {
            r = session(&ctx, &mut home, &mut pc, pads) => r,
            _ = stopped(&ctx) => Ok(()),
        };
        if let Err(e) = r {
            ctx.log(1, &format!("{e}"));
            ctx.set(Status::Failed(describe(&e)));
        }
        if let Some(pc) = pc {
            let _ = tokio::time::timeout(Duration::from_secs(3), pc.close()).await;
        }
        // (the channels' handlers hold the context: let both go)
        ctx.channels.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clear();
        if let Some(mut h) = home {
            let _ = tokio::time::timeout(Duration::from_secs(5), h.stop()).await;
        }
    });
    // (the decoder's queue ends: its thread finishes)
    ctx.to_decoder.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
    rt.shutdown_timeout(Duration::from_secs(2));
}

async fn stopped(ctx: &Ctx) {
    while !ctx.stop.load(Ordering::Relaxed) {
        let _ = tokio::time::timeout(Duration::from_millis(500), ctx.wake.notified()).await;
    }
}

/// What an error means for the player (English; the app translates it).
fn describe(e: &Error) -> String {
    match e {
        Error::SignedOut => SIGNED_OUT.into(),
        Error::Http { code: 401 | 403, .. } => REFUSED_SIGN_IN.into(),
        e => e.to_string(),
    }
}

async fn session(ctx: &Arc<Ctx>, home_slot: &mut Option<Home>, pc_slot: &mut Option<Arc<RTCPeerConnection>>, mut pads: UnboundedReceiver<Gamepad>) -> Result<()> {
    let login = ctx.account.streaming().await?;
    let home = home_slot.insert(Home::new(login, ctx.resolution)?);
    // the console now (and the region that lists it): asleep, it's woken first
    let now = home.consoles().await?.into_iter().find(|c| c.id == ctx.console).ok_or_else(|| Error::Other(NOT_LISTED.into()))?;
    if now.asleep() {
        ctx.set(Status::Waking);
        if let Err(e) = ctx.account.wake(&ctx.console).await {
            ctx.log(2, &format!("wake: {e}"));
        }
        let until = Instant::now() + Duration::from_secs(45);
        while Instant::now() < until {
            tokio::time::sleep(Duration::from_secs(3)).await;
            if home.consoles().await.ok().and_then(|l| l.into_iter().find(|c| c.id == ctx.console)).is_some_and(|c| !c.asleep()) {
                break;
            }
        }
    }

    ctx.set(Status::Waiting);
    home.play(&ctx.console, ctx.tz_minutes).await?;
    let mut connected = false;
    let until = Instant::now() + Duration::from_secs(90);
    loop {
        if Instant::now() > until {
            return Err(Error::Other(NOT_READY.into()));
        }
        let st = home.state().await?;
        match st.state.as_str() {
            "Provisioned" => break,
            "ReadyToConnect" if !connected => {
                home.connect(&ctx.account.transfer_token().await?).await?;
                connected = true;
            }
            "Failed" => return Err(Error::Other(failure(st.error.as_deref()))),
            _ => {}
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    let keep_alive = home.keep_alive_seconds().await;

    ctx.set(Status::Connecting);
    let pc = pc_slot.insert(peer(ctx).await?).clone();
    let offer = pc.create_offer(None).await.map_err(webrtc_err)?;
    let offer_sdp = offer.sdp.clone();
    let mut gathered = pc.gathering_complete_promise().await;
    pc.set_local_description(offer).await.map_err(webrtc_err)?;
    let answer = home.exchange_sdp(&input::stereo(&offer_sdp)).await?;
    pc.set_remote_description(RTCSessionDescription::answer(answer).map_err(webrtc_err)?).await.map_err(webrtc_err)?;
    // (this computer's candidates: all of them, gathered within 5 s)
    let _ = tokio::time::timeout(Duration::from_secs(5), gathered.recv()).await;
    let mine: Vec<Candidate> = ctx.local.lock().unwrap_or_else(std::sync::PoisonError::into_inner).iter().filter(|c| !c.skip()).cloned().collect();
    let theirs = with_teredo(home.exchange_ice(&mine).await?);
    for c in theirs.into_iter().filter(|c| !c.skip()) {
        let init = RTCIceCandidateInit {
            candidate: c.candidate.trim_start_matches("a=").to_string(),
            sdp_mid: Some(c.mid.clone()),
            sdp_mline_index: Some(c.mline),
            username_fragment: None,
        };
        if let Err(e) = pc.add_ice_candidate(init).await {
            ctx.log(3, &format!("candidate {}: {e}", c.candidate));
        }
    }

    // keep-alive, signs of life on the input channel, controllers, and the end
    let mut alive = tokio::time::interval_at(tokio::time::Instant::now() + Duration::from_secs(keep_alive), Duration::from_secs(keep_alive));
    let mut life = tokio::time::interval_at(tokio::time::Instant::now() + Duration::from_secs(2), Duration::from_secs(10));
    let mut check = tokio::time::interval(Duration::from_millis(500));
    let connecting = Instant::now();
    let mut frames_at_life = 0;
    loop {
        tokio::select! {
            _ = alive.tick() => {
                if let Err(e) = home.keep_alive().await {
                    ctx.log(2, &format!("keep-alive: {e}"));
                }
            }
            _ = life.tick() => {
                let epoch = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u32).unwrap_or(0);
                ctx.input(input::metadata(ctx.next_seq(), ctx.ms(), epoch)).await;
                let frames = ctx.stats.frames.load(Ordering::Relaxed);
                if frames == frames_at_life {
                    ctx.text("control", input::keyframe_request()).await;
                }
                frames_at_life = frames;
            }
            Some(g) = pads.recv() => {
                if ctx.settings.controllers.load(Ordering::Relaxed) {
                    ctx.input(input::gamepad(ctx.next_seq(), ctx.ms(), 0, &g)).await;
                }
            }
            _ = check.tick() => {
                if ctx.status.lock().unwrap_or_else(std::sync::PoisonError::into_inner).over() {
                    return Ok(());
                }
                if ctx.stats.frames.load(Ordering::Relaxed) > 0 {
                    ctx.set(Status::Playing);
                } else if connecting.elapsed() > Duration::from_secs(30) {
                    return Err(Error::Other(NO_PICTURE.into()));
                }
            }
        }
    }
}

/// The peer: video received, sound both ways (as the web player has it; nothing is sent), the four
/// data channels, and what each does.
async fn peer(ctx: &Arc<Ctx>) -> Result<Arc<RTCPeerConnection>> {
    let mut media = MediaEngine::default();
    media.register_default_codecs().map_err(webrtc_err)?;
    let registry = register_default_interceptors(Registry::new(), &mut media).map_err(webrtc_err)?;
    let api = APIBuilder::new().with_media_engine(media).with_interceptor_registry(registry).build();
    let config = RTCConfiguration {
        ice_servers: vec![RTCIceServer { urls: STUN.iter().map(|s| s.to_string()).collect(), ..Default::default() }],
        ..Default::default()
    };
    let pc = Arc::new(api.new_peer_connection(config).await.map_err(webrtc_err)?);
    for (kind, direction) in [(RTPCodecType::Video, RTCRtpTransceiverDirection::Recvonly), (RTPCodecType::Audio, RTCRtpTransceiverDirection::Sendrecv)] {
        pc.add_transceiver_from_kind(kind, Some(RTCRtpTransceiverInit { direction, send_encodings: Vec::new() })).await.map_err(webrtc_err)?;
    }

    for (label, protocol, ordered) in [("input", "1.0", true), ("chat", "chatV1", false), ("control", "controlV1", false), ("message", "messageV1", false)] {
        let init = RTCDataChannelInit { ordered: Some(ordered), protocol: Some(protocol.into()), ..Default::default() };
        let dc = pc.create_data_channel(label, Some(init)).await.map_err(webrtc_err)?;
        ctx.channels.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(label, dc.clone());
        let (c, d) = (ctx.clone(), dc.clone());
        dc.on_open(Box::new(move || {
            Box::pin(async move {
                c.log(4, &format!("channel {label} open"));
                match label {
                    "message" => {
                        let _ = d.send_text(input::handshake(&c.next_cv())).await;
                    }
                    // (who this client is: no touch screen)
                    "input" => {
                        let _ = d.send(&Bytes::from(input::client_metadata(c.next_seq(), c.ms()))).await;
                    }
                    _ => {}
                }
            })
        }));
        let c = ctx.clone();
        dc.on_message(Box::new(move |m: DataChannelMessage| {
            let c = c.clone();
            Box::pin(async move {
                match label {
                    "message" if m.is_string => {
                        if input::is_handshake_ack(&String::from_utf8_lossy(&m.data)) {
                            tokio::spawn(greet(c));
                        }
                    }
                    "input" if !m.is_string => {
                        if let Some(r) = input::rumble_in(&m.data) {
                            let _ = c.to_sdl.lock().unwrap_or_else(std::sync::PoisonError::into_inner).send(ToSdl::Rumble(r));
                        }
                    }
                    _ => {}
                }
            })
        }));
    }

    let c = ctx.clone();
    let p = Arc::downgrade(&pc);
    pc.on_track(Box::new(move |track: Arc<TrackRemote>, _, _| {
        let (c, p) = (c.clone(), p.clone());
        Box::pin(async move {
            c.log(4, &format!("track {} {}", track.kind(), track.codec().capability.mime_type));
            match track.kind() {
                RTPCodecType::Video => {
                    tokio::spawn(video(c, p, track));
                }
                RTPCodecType::Audio => {
                    tokio::spawn(audio(c, track));
                }
                _ => {}
            }
        })
    }));

    let c = ctx.clone();
    pc.on_ice_candidate(Box::new(move |cand: Option<RTCIceCandidate>| {
        let c = c.clone();
        Box::pin(async move {
            if let Some(init) = cand.and_then(|x| x.to_json().ok()) {
                c.local.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(Candidate {
                    candidate: init.candidate,
                    // (webrtc-rs leaves the mid empty: the first m-line's, as a browser names it)
                    mid: init.sdp_mid.filter(|m| !m.is_empty()).unwrap_or_else(|| "0".into()),
                    mline: init.sdp_mline_index.unwrap_or(0),
                });
            }
        })
    }));

    let c = ctx.clone();
    pc.on_peer_connection_state_change(Box::new(move |s: RTCPeerConnectionState| {
        let c = c.clone();
        Box::pin(async move {
            c.log(4, &format!("peer {s}"));
            match s {
                RTCPeerConnectionState::Failed => c.set(Status::Failed(NAT_FAILED.into())),
                // (a short loss of the network, "disconnected", comes back on its own: only a closed peer ends it)
                RTCPeerConnectionState::Closed if !c.stop.load(Ordering::Relaxed) => c.set(Status::Ended(ENDED.into())),
                _ => {}
            }
        })
    }));
    Ok(pc)
}

/// After the console's HandshakeAck: allowed in, a controller slot, the six greetings, a picture.
async fn greet(c: Arc<Ctx>) {
    c.text("control", input::authorization_request()).await;
    c.text("control", input::gamepad_changed(0, false)).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    c.text("control", input::gamepad_changed(0, true)).await;
    let install = uuid::Uuid::new_v4().to_string();
    for m in input::greetings(&install, || c.next_cv()) {
        c.text("message", m).await;
    }
    c.text("control", input::keyframe_request()).await;
}

/// Is there an IDR slice (a whole picture) in this Annex B access unit?
pub fn has_idr(d: &[u8]) -> bool {
    let mut i = 0;
    while i + 3 < d.len() {
        if d[i] == 0 && d[i + 1] == 0 && d[i + 2] == 1 {
            if d[i + 3] & 0x1f == 5 {
                return true;
            }
            i += 3;
        } else {
            i += 1;
        }
    }
    false
}

/// The picture's RTP packets made into whole frames for the decoder. After a loss, frames wait for
/// the next whole picture (asked for with a PLI, at most twice a second).
async fn video(c: Arc<Ctx>, pc: std::sync::Weak<RTCPeerConnection>, track: Arc<TrackRemote>) {
    let ssrc = track.ssrc();
    let mut sb = SampleBuilder::new(512, H264Packet::default(), track.codec().capability.clock_rate.max(1)).with_max_time_delay(Duration::from_millis(220));
    let mut clean = false;
    let mut last_pli: Option<Instant> = None;
    let pli = |last: &mut Option<Instant>| {
        let pc = pc.clone();
        let due = last.is_none_or(|t| t.elapsed() > Duration::from_millis(500));
        if due {
            *last = Some(Instant::now());
        }
        async move {
            if let (true, Some(pc)) = (due, pc.upgrade()) {
                let _ = pc.write_rtcp(&[Box::new(PictureLossIndication { sender_ssrc: 0, media_ssrc: ssrc })]).await;
            }
        }
    };
    while let Ok((packet, _)) = track.read_rtp().await {
        sb.push(packet);
        while let Some(s) = sb.pop() {
            if s.data.is_empty() {
                continue;
            }
            if c.keyframe_wanted.swap(false, Ordering::Relaxed) {
                clean = false;
            }
            let idr = has_idr(&s.data);
            if s.prev_dropped_packets.saturating_sub(s.prev_padding_packets) > 0 && !idr {
                clean = false;
            }
            if !clean && !idr {
                pli(&mut last_pli).await;
                continue;
            }
            clean = true;
            let tx = c.to_decoder.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
            if let Some(tx) = tx {
                let _ = tx.send(s.data);
            }
        }
    }
}

/// The console's sound (Opus, 48 kHz stereo), decoded for the SDL thread.
async fn audio(c: Arc<Ctx>, track: Arc<TrackRemote>) {
    let mut dec = match opus::Decoder::new(48000, opus::Channels::Stereo) {
        Ok(d) => d,
        Err(e) => return c.log(1, &format!("sound: {e}")),
    };
    let mut pcm = vec![0i16; 5760 * 2];
    while let Ok((packet, _)) = track.read_rtp().await {
        if packet.payload.is_empty() || c.settings.gain() == 0.0 {
            continue;
        }
        if let Ok(n) = dec.decode(&packet.payload, &mut pcm, false) {
            let _ = c.to_sdl.lock().unwrap_or_else(std::sync::PoisonError::into_inner).send(ToSdl::Pcm(pcm[..n * 2].to_vec()));
        }
    }
}

/// H.264 to pictures on a thread of its own (a 1080p frame takes a few ms on the CPU).
fn decoder_thread(c: Arc<Ctx>, frames: Receiver<Bytes>) {
    use openh264::formats::YUVSource;
    let mut dec = match openh264::decoder::Decoder::new() {
        Ok(d) => d,
        Err(e) => return c.set(Status::Failed(format!("video decoder: {e}"))),
    };
    while let Ok(first) = frames.recv() {
        // (behind: every frame is decoded, as the decoder needs them all since the last whole
        // picture, but only the newest is shown)
        let mut batch = vec![first];
        batch.extend(frames.try_iter());
        let last = batch.len() - 1;
        for (i, data) in batch.iter().enumerate() {
            match dec.decode(data) {
                Ok(Some(yuv)) if i == last => {
                    let (w, h) = yuv.dimensions();
                    let (ys, us, _) = yuv.strides();
                    let f = I420 {
                        w: w as u32,
                        h: h as u32,
                        y: yuv.y()[..ys * h].to_vec(),
                        u: yuv.u()[..us * h.div_ceil(2)].to_vec(),
                        v: yuv.v()[..us * h.div_ceil(2)].to_vec(),
                        y_stride: ys,
                        uv_stride: us,
                    };
                    c.stats.frames.fetch_add(1, Ordering::Relaxed);
                    c.screen.frame(f);
                }
                Ok(_) => {}
                Err(e) => {
                    c.stats.bad_frames.fetch_add(1, Ordering::Relaxed);
                    c.keyframe_wanted.store(true, Ordering::Relaxed);
                    c.log(3, &format!("video: {e}"));
                }
            }
        }
    }
}

/// Controllers in and sound out, on SDL's own thread (SDL's objects stay on the thread that made
/// them). Controllers work whichever window is in front: the player plays in the app's window.
fn sdl_thread(c: Arc<Ctx>, rx: Receiver<ToSdl>, pads_tx: UnboundedSender<Gamepad>) {
    sdl2::hint::set("SDL_JOYSTICK_ALLOW_BACKGROUND_EVENTS", "1");
    let sdl = match sdl2::init() {
        Ok(s) => s,
        Err(e) => return c.log(1, &format!("SDL: {e}")),
    };
    let gc = sdl.game_controller().map_err(|e| c.log(1, &format!("controllers: {e}"))).ok();
    let audio = sdl.audio().map_err(|e| c.log(1, &format!("sound: {e}"))).ok();
    let mut events = match sdl.event_pump() {
        Ok(e) => e,
        Err(e) => return c.log(1, &format!("SDL events: {e}")),
    };
    let queue = audio.as_ref().and_then(|a| {
        let want = sdl2::audio::AudioSpecDesired { freq: Some(48000), channels: Some(2), samples: Some(512) };
        a.open_queue::<i16, _>(None, &want).map_err(|e| c.log(1, &format!("sound: {e}"))).ok()
    });
    if let Some(q) = &queue {
        q.resume();
    }
    let mut pads: Vec<sdl2::controller::GameController> = Vec::new();
    let mut last = Gamepad::default();
    let mut last_sent = Instant::now();
    while !c.stop.load(Ordering::Relaxed) {
        for ev in events.poll_iter() {
            match ev {
                sdl2::event::Event::ControllerDeviceAdded { which, .. } => {
                    if let Some(p) = gc.as_ref().and_then(|g| g.open(which).ok()) {
                        c.log(4, &format!("controller: {}", p.name()));
                        pads.push(p);
                    }
                }
                sdl2::event::Event::ControllerDeviceRemoved { which, .. } => pads.retain(|p| p.instance_id() != which),
                _ => {}
            }
        }
        c.stats.controllers.store(pads.len() as u64, Ordering::Relaxed);
        // every controller's buttons and sticks, together, as controller 0
        let g = merge(pads.iter().map(pad_state));
        // (sent when it changes, and every 100 ms while a controller is there)
        if g != last || (!pads.is_empty() && last_sent.elapsed() > Duration::from_millis(100)) {
            let _ = pads_tx.send(g);
            last = g;
            last_sent = Instant::now();
        }
        while let Ok(m) = rx.try_recv() {
            match m {
                ToSdl::Pcm(mut pcm) => {
                    let Some(q) = &queue else { continue };
                    let gain = c.settings.gain();
                    if gain == 0.0 {
                        // (off: what is waiting goes too, so turning it on is heard at once)
                        q.clear();
                        continue;
                    }
                    scale(&mut pcm, gain);
                    // (more than ~150 ms waiting: drop it, so the sound never lags the picture)
                    if q.size() as usize > 48000 * 2 * 2 * 150 / 1000 {
                        q.clear();
                    }
                    let _ = q.queue_audio(&pcm);
                }
                ToSdl::Rumble(r) => {
                    if c.settings.controllers.load(Ordering::Relaxed) {
                        let (l, rr) = ((r.left as u32 * 65535 / 100) as u16, (r.right as u32 * 65535 / 100) as u16);
                        for p in pads.iter_mut() {
                            let _ = p.set_rumble(l, rr, r.duration_ms.max(1) as u32);
                        }
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_millis(4));
    }
}

/// Several controllers as one: buttons together, the furthest stick and trigger.
pub fn merge(states: impl Iterator<Item = Gamepad>) -> Gamepad {
    let mut g = Gamepad::default();
    for s in states {
        g.buttons |= s.buttons;
        g.lt = g.lt.max(s.lt);
        g.rt = g.rt.max(s.rt);
        for (a, b) in [(&mut g.lx, s.lx), (&mut g.ly, s.ly), (&mut g.rx, s.rx), (&mut g.ry, s.ry)] {
            if b.unsigned_abs() > a.unsigned_abs() {
                *a = b;
            }
        }
    }
    g
}

/// An SDL axis (−32768…32767, down positive for Y) as the console's (−32767…32767, up positive).
pub fn axis(v: i16, flip: bool) -> i16 {
    let v = v.max(-32767);
    if flip { -v } else { v }
}

/// An SDL trigger (0…32767) as the console's (0…65535).
pub fn trigger(v: i16) -> u16 {
    (v.max(0) as u32 * 65535 / 32767) as u16
}

/// A controller's buttons and sticks (SDL's names are the Xbox layout's).
fn pad_state(p: &sdl2::controller::GameController) -> Gamepad {
    use sdl2::controller::{Axis, Button};
    let map = [
        (Button::A, buttons::A),
        (Button::B, buttons::B),
        (Button::X, buttons::X),
        (Button::Y, buttons::Y),
        (Button::DPadUp, buttons::UP),
        (Button::DPadDown, buttons::DOWN),
        (Button::DPadLeft, buttons::LEFT),
        (Button::DPadRight, buttons::RIGHT),
        (Button::LeftShoulder, buttons::LB),
        (Button::RightShoulder, buttons::RB),
        (Button::LeftStick, buttons::LS),
        (Button::RightStick, buttons::RS),
        (Button::Start, buttons::MENU),
        (Button::Back, buttons::VIEW),
        (Button::Guide, buttons::NEXUS),
    ];
    let b = map.iter().filter(|(sb, _)| p.button(*sb)).fold(0, |b, (_, x)| b | x);
    Gamepad {
        buttons: b,
        lx: axis(p.axis(Axis::LeftX), false),
        ly: axis(p.axis(Axis::LeftY), true),
        rx: axis(p.axis(Axis::RightX), false),
        ry: axis(p.axis(Axis::RightY), true),
        lt: trigger(p.axis(Axis::TriggerLeft)),
        rt: trigger(p.axis(Axis::TriggerRight)),
    }
}

/// Samples at `gain` (1.0: as they are).
pub fn scale(pcm: &mut [i16], gain: f32) {
    if (gain - 1.0).abs() < f32::EPSILON {
        return;
    }
    for s in pcm {
        *s = (*s as f32 * gain).round().clamp(i16::MIN as f32, i16::MAX as f32) as i16;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idr_slices_are_found() {
        assert!(has_idr(&[0, 0, 0, 1, 0x67, 1, 2, 0, 0, 0, 1, 0x68, 3, 0, 0, 1, 0x65, 9]));
        assert!(!has_idr(&[0, 0, 0, 1, 0x41, 1, 2, 3]));
        assert!(!has_idr(&[]));
    }

    #[test]
    fn controllers_become_one() {
        let a = Gamepad { buttons: buttons::A, lx: 100, lt: 10, ..Default::default() };
        let b = Gamepad { buttons: buttons::B, lx: -2000, rt: 5, ..Default::default() };
        let g = merge([a, b].into_iter());
        assert_eq!(g.buttons, buttons::A | buttons::B);
        assert_eq!((g.lx, g.lt, g.rt), (-2000, 10, 5));
        assert_eq!(merge(std::iter::empty()), Gamepad::default());
    }

    #[test]
    fn sdl_axes_become_the_console_s() {
        assert_eq!(axis(-32768, true), 32767, "stick fully up");
        assert_eq!(axis(32767, true), -32767);
        assert_eq!(axis(1000, false), 1000);
        assert_eq!(trigger(32767), 65535);
        assert_eq!(trigger(-5), 0);
    }

    struct NoScreen;
    impl Screen for NoScreen {
        fn frame(&self, _: I420) {}
    }

    fn test_ctx() -> Arc<Ctx> {
        let (to_sdl, _) = channel();
        Arc::new(Ctx {
            account: Arc::new(Account::new("", |_| {})),
            console: "X".into(),
            resolution: 1080,
            tz_minutes: 0,
            settings: Arc::new(Settings::new(true, 100, true)),
            screen: Arc::new(NoScreen),
            stats: Arc::new(Stats::default()),
            status: Mutex::new(Status::Starting),
            stop: AtomicBool::new(false),
            wake: Notify::new(),
            started: Instant::now(),
            seq: AtomicU32::new(0),
            cv: AtomicU32::new(0),
            channels: Mutex::new(HashMap::new()),
            local: Mutex::new(Vec::new()),
            to_decoder: Mutex::new(None),
            to_sdl: Mutex::new(to_sdl),
            keyframe_wanted: AtomicBool::new(false),
        })
    }

    /// The offer the console gets, made by the real peer (no network needed): H.264 video received,
    /// Opus sound both ways in stereo, and the data channels' m-line.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_offer_has_video_sound_and_the_channels() {
        let ctx = test_ctx();
        let pc = peer(&ctx).await.unwrap();
        let offer = pc.create_offer(None).await.unwrap();
        let sdp = input::stereo(&offer.sdp);
        let video = sdp.find("m=video").expect("video");
        let audio = sdp.find("m=audio").expect("audio");
        assert!(video < audio, "video first: mid 0");
        assert!(sdp.contains("m=application"), "data channels");
        assert!(sdp.contains("H264/90000"));
        assert!(sdp[video..audio].contains("a=recvonly"));
        assert!(sdp[audio..].contains("a=sendrecv"));
        assert!(sdp.contains("opus/48000/2"));
        assert!(sdp.contains("stereo=1"));
        assert_eq!(ctx.channels.lock().unwrap_or_else(std::sync::PoisonError::into_inner).len(), 4);
        pc.close().await.unwrap();
        ctx.channels.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clear();
    }

    #[test]
    fn the_volume_scales_the_sound() {
        let s = Settings::new(true, 50, true);
        assert!((s.gain() - 0.25).abs() < 1e-6);
        let mut pcm = [1000i16, -1000];
        scale(&mut pcm, s.gain());
        assert_eq!(pcm, [250, -250]);
        s.sound.store(false, Ordering::Relaxed);
        assert_eq!(s.gain(), 0.0);
    }
}
