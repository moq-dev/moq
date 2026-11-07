#![allow(unused)]
use std::cell::Cell;
use std::io::Cursor;
use std::net::{Ipv4Addr, SocketAddr};
use std::ops::{Deref, DerefMut};
use std::sync::{Arc, Once};
use std::time::{Duration, Instant};

use netem::{Input as NetemInput, Netem, NetemConfig, Output as NetemOutput};

use pcap_file::pcap::PcapReader;
use str0m::Candidate;
use str0m::change::SdpApi;
use str0m::crypto::CryptoProvider;
use str0m::format::Codec;
use str0m::format::PayloadParams;
use str0m::net::Protocol;
use str0m::net::Receive;
use str0m::rtp::ExtensionMap;
use str0m::rtp::RtpHeader;
use str0m::{Event, Input, Output, Rtc, RtcConfig, RtcError};
use tracing::Span;
use tracing::info_span;

/// Peer for test peers - Left or Right.
/// Used to determine which crypto provider to use based on environment variables.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Peer {
    Left,
    Right,
}

impl Peer {
    /// Create a tracing span for this peer.
    pub fn span(&self) -> Span {
        match self {
            Peer::Left => info_span!("L"),
            Peer::Right => info_span!("R"),
        }
    }

    /// Get the crypto provider for this peer based on environment variables.
    /// Returns None if no environment variable is set.
    pub fn crypto_provider(&self) -> Option<Arc<CryptoProvider>> {
        let env_var = match self {
            Peer::Left => "L_CRYPTO",
            Peer::Right => "R_CRYPTO",
        };

        if let Ok(crypto_name) = std::env::var(env_var) {
            Some(Arc::new(get_crypto_provider_by_name(&crypto_name)))
        } else {
            None
        }
    }
}

/// Owned version of Receive for queueing.
#[derive(Clone)]
pub struct PendingPacket {
    pub proto: Protocol,
    pub source: SocketAddr,
    pub destination: SocketAddr,
    pub contents: Vec<u8>,
}

impl AsRef<[u8]> for PendingPacket {
    fn as_ref(&self) -> &[u8] {
        &self.contents
    }
}

pub struct TestRtc {
    pub span: Span,
    pub rtc: Rtc,
    pub start: Instant,
    pub last: Instant,
    pub events: Vec<(Instant, Event)>,
    pub pending: Netem<PendingPacket>,
    pub forced_time_advance: Duration,
}

impl TestRtc {
    pub fn new(peer: Peer) -> Self {
        let now = Instant::now();
        let rtc = if let Some(crypto) = peer.crypto_provider() {
            Rtc::builder().set_crypto_provider(crypto).build(now)
        } else {
            Rtc::new(now)
        };

        Self::new_with_rtc(peer.span(), rtc)
    }

    /// Like [`TestRtc::new`], but lets the caller customise the `RtcConfig`
    /// builder — e.g. to enable a non-default codec such as H.266.
    pub fn new_with_config(peer: Peer, f: impl FnOnce(RtcConfig) -> RtcConfig) -> Self {
        let now = Instant::now();
        let builder = Rtc::builder();
        let builder = if let Some(crypto) = peer.crypto_provider() {
            builder.set_crypto_provider(crypto)
        } else {
            builder
        };
        let rtc = f(builder).build(now);
        Self::new_with_rtc(peer.span(), rtc)
    }

    pub fn new_with_rtc(span: Span, rtc: Rtc) -> Self {
        let now = Instant::now();
        TestRtc {
            span,
            rtc,
            start: now,
            last: now,
            events: vec![],
            pending: Netem::new(NetemConfig::new()),
            forced_time_advance: Duration::from_millis(10),
        }
    }

    /// Set the forced time advance duration when RTC returns v==rtc.last.
    /// This prevents the test from getting stuck when RTC has no pending timeouts.
    /// Should be set to the packet interval for the target bitrate (e.g., 0.2ms for 50 Mbps).
    pub fn set_forced_time_advance(&mut self, duration: Duration) {
        self.forced_time_advance = duration;
    }

    /// Configure network emulation for incoming traffic to this RTC.
    /// Call this on the RECEIVER to affect traffic coming TO this peer.
    /// This preserves any packets already queued in the netem.
    pub fn set_netem(&mut self, config: NetemConfig) {
        self.pending.set_config(config);
    }

    pub fn add_host_candidate(&mut self, socket: SocketAddr) -> Candidate {
        self.rtc
            .add_local_candidate(Candidate::host(socket, "udp").unwrap())
            .unwrap()
            .clone()
    }

    pub fn duration(&self) -> Duration {
        self.last - self.start
    }

    pub fn params_opus(&self) -> PayloadParams {
        self.rtc
            .codec_config()
            .find(|p| p.spec().codec == Codec::Opus)
            .cloned()
            .unwrap()
    }

    pub fn params_vp8(&self) -> PayloadParams {
        self.rtc
            .codec_config()
            .find(|p| p.spec().codec == Codec::Vp8)
            .cloned()
            .unwrap()
    }

    pub fn params_vp9(&self) -> PayloadParams {
        self.rtc
            .codec_config()
            .find(|p| p.spec().codec == Codec::Vp9)
            .cloned()
            .unwrap()
    }

    pub fn params_h264(&self) -> PayloadParams {
        self.rtc
            .codec_config()
            .find(|p| p.spec().codec == Codec::H264)
            .cloned()
            .unwrap()
    }

    pub fn params_av1(&self) -> PayloadParams {
        self.rtc
            .codec_config()
            .find(|p| p.spec().codec == Codec::Av1)
            .cloned()
            .unwrap()
    }

    pub fn params_h265(&self) -> PayloadParams {
        self.rtc
            .codec_config()
            .find(|p| p.spec().codec == Codec::H265)
            .cloned()
            .unwrap()
    }

    pub fn params_h266(&self) -> PayloadParams {
        self.rtc
            .codec_config()
            .find(|p| p.spec().codec == Codec::H266)
            .cloned()
            .unwrap()
    }
}

/// Progress time forward by processing the next event.
///
/// We have 4 event sources:
/// - l.last: l's rtc timeout
/// - r.last: r's rtc timeout
/// - l.pending: packet ready to deliver to l
/// - r.pending: packet ready to deliver to r
///
/// Pick the earliest, process it, then try to progress again for any
/// more even that is within 5ms of the first time.
pub fn progress(l: &mut TestRtc, r: &mut TestRtc) -> Result<(), RtcError> {
    let mut first_time = None;

    loop {
        // Find earliest event
        let l_netem = l.pending.poll_timeout();
        let r_netem = r.pending.poll_timeout();

        // Determine which event is next: (time, is_l, is_netem)
        let mut next = (l.last, true, false); // default: l's rtc

        if r.last < next.0 {
            next = (r.last, false, false);
        }
        if l_netem < next.0 {
            next = (l_netem, true, true);
        }
        if r_netem < next.0 {
            next = (r_netem, false, true);
        }

        let (time, is_l, is_netem) = next;

        if let Some(first_time) = first_time {
            // The idea is that we try to advance all the components that might be
            // within some distance of each other.
            let elapsed = time.saturating_duration_since(first_time);
            if elapsed >= Duration::from_millis(5) {
                break;
            }
        } else {
            first_time = Some(time);
        }

        progress_one(l, r, time, is_l, is_netem)?;
    }

    Ok(())
}

thread_local! {
    /// When set, [`rtc_poll_to_timeout`] asserts every [`Output::Transmit`]
    /// for which `contents.len() <= mtu`.
    static STRICT_MTU: Cell<Option<usize>> = const { Cell::new(None) };
}

struct StrictMtuGuard(Option<usize>);
impl Drop for StrictMtuGuard {
    fn drop(&mut self) {
        STRICT_MTU.with(|c| c.set(self.0));
    }
}

/// Like [`progress`] but asserts every outgoing datagram is `<= mtu` bytes.
pub fn progress_strict_mtu(l: &mut TestRtc, r: &mut TestRtc, mtu: usize) -> Result<(), RtcError> {
    let prev = STRICT_MTU.with(|c| c.replace(Some(mtu)));
    let _guard = StrictMtuGuard(prev);
    progress(l, r)
}

fn progress_one(
    l: &mut TestRtc,
    r: &mut TestRtc,
    time: Instant,
    is_l: bool,
    is_netem: bool,
) -> Result<(), RtcError> {
    if is_netem {
        // Deliver packet from netem to rtc (no timeout processing)
        if is_l {
            netem_to_rtc(l, time, &mut r.pending)?;
        } else {
            netem_to_rtc(r, time, &mut l.pending)?;
        }
    } else {
        // Process rtc timeout and poll outputs
        if is_l {
            rtc_timeout(l, time, &mut r.pending)?;
        } else {
            rtc_timeout(r, time, &mut l.pending)?;
        }
    }
    Ok(())
}

/// Deliver one packet from rtc.pending to rtc. No timeout processing.
fn netem_to_rtc(
    rtc: &mut TestRtc,
    time: Instant,
    other_netem: &mut Netem<PendingPacket>,
) -> Result<(), RtcError> {
    rtc.pending.handle_input(NetemInput::Timeout(time));

    let Some(NetemOutput::Packet(packet)) = rtc.pending.poll_output() else {
        return Ok(());
    };

    let input = Input::Receive(
        time,
        Receive {
            proto: packet.proto,
            source: packet.source,
            destination: packet.destination,
            contents: (&packet.contents[..]).try_into()?,
        },
    );
    rtc.span.in_scope(|| rtc.rtc.handle_input(input))?;

    rtc_poll_to_timeout(rtc, time, other_netem)?;

    Ok(())
}

/// Process rtc timeout and poll until next timeout, queueing transmits in other_netem.
fn rtc_timeout(
    rtc: &mut TestRtc,
    time: Instant,
    other_netem: &mut Netem<PendingPacket>,
) -> Result<(), RtcError> {
    rtc.span
        .in_scope(|| rtc.rtc.handle_input(Input::Timeout(time)))?;

    rtc_poll_to_timeout(rtc, time, other_netem)?;

    Ok(())
}

fn rtc_poll_to_timeout(
    rtc: &mut TestRtc,
    time: Instant,
    other_netem: &mut Netem<PendingPacket>,
) -> Result<(), RtcError> {
    loop {
        let next = rtc.span.in_scope(|| rtc.rtc.poll_output())?;
        // println!("next: {:?}", next);
        match next {
            Output::Timeout(v) => {
                let tick = rtc.last + rtc.forced_time_advance;
                rtc.last = if v == rtc.last { tick } else { tick.min(v) };
                break;
            }
            Output::Transmit(v) => {
                if let Some(mtu) = STRICT_MTU.with(|c| c.get()) {
                    assert!(
                        v.contents.len() <= mtu,
                        "outgoing datagram {} bytes exceeds strict mtu {} (proto={:?}, {}->{})",
                        v.contents.len(),
                        mtu,
                        v.proto,
                        v.source,
                        v.destination,
                    );
                }
                let packet = PendingPacket {
                    proto: v.proto,
                    source: v.source,
                    destination: v.destination,
                    contents: v.contents.to_vec(),
                };
                other_netem.handle_input(NetemInput::Packet(time, packet));
            }
            Output::Event(v) => {
                rtc.events.push((rtc.last, v));
            }
        }
    }
    Ok(())
}

/// Perform a change to the session via an offer and answer.
///
/// The closure is passed the [`SdpApi`] for the offer side to make any changes, these are then
/// applied locally and the offer is negotiated with the answerer.
pub fn negotiate<F, R>(offerer: &mut TestRtc, answerer: &mut TestRtc, mut do_change: F) -> R
where
    F: FnMut(&mut SdpApi) -> R,
{
    let (offer, pending, result) = offerer.span.in_scope(|| {
        let mut change = offerer.rtc.sdp_api();

        let result = do_change(&mut change);

        let (offer, pending) = change.apply().unwrap();

        (offer, pending, result)
    });

    let answer = answerer
        .span
        .in_scope(|| answerer.rtc.sdp_api().accept_offer(offer).unwrap());

    offerer.span.in_scope(|| {
        offerer
            .rtc
            .sdp_api()
            .accept_answer(pending, answer)
            .unwrap();
    });

    result
}

impl Deref for TestRtc {
    type Target = Rtc;

    fn deref(&self) -> &Self::Target {
        &self.rtc
    }
}

impl DerefMut for TestRtc {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.rtc
    }
}

pub fn init_log() {
    use tracing_subscriber::{EnvFilter, fmt, prelude::*};

    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("off"));

    static START: Once = Once::new();

    START.call_once(|| {
        tracing_subscriber::registry()
            .with(fmt::layer())
            .with(env_filter)
            .init();
    });
}

pub fn init_crypto_default() {
    str0m::crypto::from_feature_flags().install_process_default();
}

/// Create a crypto provider from a string name.
/// Supported names: "aws-lc-rs", "rust-crypto", "openssl", "wincrypto", "apple-crypto"
fn get_crypto_provider_by_name(name: &str) -> CryptoProvider {
    match name {
        #[cfg(feature = "aws-lc-rs")]
        "aws-lc-rs" | "aws" => str0m_aws_lc_rs::default_provider(),

        #[cfg(feature = "rust-crypto")]
        "rust-crypto" => str0m_rust_crypto::default_provider(),

        // If both openssl and openssl-dimpl are declared, only openssl-dimpl is available.
        #[cfg(all(feature = "openssl", not(feature = "openssl-dimpl")))]
        "openssl" => str0m_openssl::default_provider(),

        #[cfg(feature = "openssl-dimpl")]
        "openssl-dimpl" => str0m_openssl::default_provider(),

        #[cfg(all(feature = "wincrypto", target_os = "windows"))]
        "wincrypto" => str0m_wincrypto::default_provider(),

        #[cfg(all(feature = "apple-crypto", target_vendor = "apple"))]
        "apple-crypto" => str0m_apple_crypto::default_provider(),

        _ => {
            let mut available: Vec<&'static str> = Vec::new();
            #[cfg(feature = "aws-lc-rs")]
            available.push("aws-lc-rs");
            #[cfg(feature = "rust-crypto")]
            available.push("rust-crypto");
            // If both openssl and openssl-dimpl are declared, only openssl-dimpl is available.
            #[cfg(all(feature = "openssl", not(feature = "openssl-dimpl")))]
            available.push("openssl");
            #[cfg(feature = "openssl-dimpl")]
            available.push("openssl-dimpl");
            #[cfg(all(feature = "wincrypto", target_os = "windows"))]
            available.push("wincrypto");
            #[cfg(all(feature = "apple-crypto", target_vendor = "apple"))]
            available.push("apple-crypto");

            panic!(
                "Unknown or unavailable crypto provider '{}'. Available providers: [{}]",
                name,
                available.join(", ")
            )
        }
    }
}

pub fn connect_l_r() -> (TestRtc, TestRtc) {
    let mut rtc1_builder = Rtc::builder().set_rtp_mode(true).enable_raw_packets(true);

    if let Some(crypto) = Peer::Left.crypto_provider() {
        rtc1_builder = rtc1_builder.set_crypto_provider(crypto);
    }

    let mut rtc2_builder = Rtc::builder().set_rtp_mode(true).enable_raw_packets(true);

    if let Some(crypto) = Peer::Right.crypto_provider() {
        rtc2_builder = rtc2_builder.set_crypto_provider(crypto);
    }

    let now = Instant::now();
    connect_l_r_with_rtc(rtc1_builder.build(now), rtc2_builder.build(now))
}

pub fn connect_l_r_with_rtc(rtc1: Rtc, rtc2: Rtc) -> (TestRtc, TestRtc) {
    let mut l = TestRtc::new_with_rtc(info_span!("L"), rtc1);
    let mut r = TestRtc::new_with_rtc(info_span!("R"), rtc2);

    let host1 = Candidate::host((Ipv4Addr::new(1, 1, 1, 1), 1000).into(), "udp").unwrap();
    let host2 = Candidate::host((Ipv4Addr::new(2, 2, 2, 2), 2000).into(), "udp").unwrap();
    l.add_local_candidate(host1.clone());
    l.add_remote_candidate(host2.clone());
    r.add_local_candidate(host2);
    r.add_remote_candidate(host1);

    let finger_l = l.direct_api().local_dtls_fingerprint().clone();
    let finger_r = r.direct_api().local_dtls_fingerprint().clone();

    l.direct_api().set_remote_fingerprint(finger_r);
    r.direct_api().set_remote_fingerprint(finger_l);

    let creds_l = l.direct_api().local_ice_credentials();
    let creds_r = r.direct_api().local_ice_credentials();

    l.direct_api().set_remote_ice_credentials(creds_r);
    r.direct_api().set_remote_ice_credentials(creds_l);

    l.direct_api().set_ice_controlling(true);
    r.direct_api().set_ice_controlling(false);

    l.direct_api().start_dtls(true).unwrap();
    r.direct_api().start_dtls(false).unwrap();

    l.direct_api().start_sctp(true);
    r.direct_api().start_sctp(false);

    loop {
        if l.is_connected() || r.is_connected() {
            break;
        }
        progress(&mut l, &mut r).expect("clean progress");
    }

    (l, r)
}

pub type PcapData = Vec<(Duration, RtpHeader, Vec<u8>)>;

pub fn vp8_data() -> PcapData {
    load_pcap_data(include_bytes!("data/vp8.pcap"))
}

pub fn vp9_contiguous_data() -> PcapData {
    load_pcap_data(include_bytes!("data/contiguous_vp9.pcap"))
}

pub fn vp9_data() -> PcapData {
    load_pcap_data(include_bytes!("data/vp9.pcap"))
}

pub fn h264_data() -> PcapData {
    load_pcap_data(include_bytes!("data/h264.pcap"))
}

pub fn av1_data() -> PcapData {
    load_pcap_data(include_bytes!("data/av1.pcap"))
}

pub fn h265_data() -> PcapData {
    load_pcap_data(include_bytes!("data/h265.pcap"))
}

pub fn h266_data() -> PcapData {
    load_pcap_data(include_bytes!("data/h266.pcap"))
}

// ---------------------------------------------------------------------------
// SNAP (SCTP Negotiation Acceleration Protocol) test helpers
// ---------------------------------------------------------------------------

/// Extract the `a=sctp-init:<value>` from an SDP string, if present.
pub fn extract_sctp_init(sdp: &str) -> Option<String> {
    sdp.lines()
        .find(|l| l.starts_with("a=sctp-init:"))
        .map(|l| l.trim_start_matches("a=sctp-init:").to_string())
}

/// Replace the `a=sctp-init:` attribute value in an SDP string.
///
/// Preserves original line endings (`\r\n`).
pub fn replace_sctp_init(sdp: &str, replacement: &str) -> String {
    let mut result = String::with_capacity(sdp.len());
    // split("\r\n") produces an extra empty element after a trailing CRLF;
    // skip it to avoid appending a spurious double-CRLF at the end.
    for line in sdp.split("\r\n") {
        if line.is_empty() && result.ends_with("\r\n") {
            continue;
        }
        if line.starts_with("a=sctp-init:") {
            result.push_str(&format!("a=sctp-init:{replacement}\r\n"));
        } else {
            result.push_str(line);
            result.push_str("\r\n");
        }
    }
    result
}

/// Remove the `a=sctp-init:` attribute from an SDP string.
///
/// Preserves original line endings (`\r\n`).
pub fn remove_sctp_init(sdp: &str) -> String {
    let mut result = String::with_capacity(sdp.len());
    // split("\r\n") produces an extra empty element after a trailing CRLF;
    // skip it to avoid appending a spurious double-CRLF at the end.
    for line in sdp.split("\r\n") {
        if line.is_empty() && result.ends_with("\r\n") {
            continue;
        }
        if !line.starts_with("a=sctp-init:") {
            result.push_str(line);
            result.push_str("\r\n");
        }
    }
    result
}

/// Generate SNAP init data for out-of-band SCTP negotiation.
///
/// Returns `Some((local_init_bytes, SctpInitData))` when `use_snap` is true, `None` otherwise.
pub fn snap_init_data(use_snap: bool) -> Option<(Vec<u8>, str0m::channel::SctpInitData)> {
    if use_snap {
        let mut data = str0m::channel::SctpInitData::new();
        let local_init = data
            .local_init_chunk()
            .expect("generate_snap_token should not fail");
        Some((local_init, data))
    } else {
        None
    }
}

pub fn load_pcap_data(data: &[u8]) -> PcapData {
    let reader = Cursor::new(data);
    let mut r = PcapReader::new(reader).expect("pcap reader");

    let exts = ExtensionMap::standard();

    let mut ret = vec![];

    let mut first = None;

    while let Some(pkt) = r.next_packet() {
        let pkt = pkt.unwrap();

        if first.is_none() {
            first = Some(pkt.timestamp);
        }
        let relative_time = pkt.timestamp - first.unwrap();

        // This magic number 42 is the ethernet/IP/UDP framing of the packet.
        let rtp_data = &pkt.data[42..];

        let header = RtpHeader::_parse(rtp_data, &exts).unwrap();
        let payload = &rtp_data[header.header_len..];

        ret.push((relative_time, header, payload.to_vec()));
    }

    ret
}
