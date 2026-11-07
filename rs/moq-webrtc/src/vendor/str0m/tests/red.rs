use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use netem::{NetemConfig, Probability, RandomLoss};
use str0m::change::SdpOffer;
use str0m::format::Codec;
use str0m::media::{Direction, MediaKind};
use str0m::rtp::RedDecoder;
use str0m::{Event, Rtc};

mod common;
use common::{Peer, TestRtc, init_crypto_default, init_log, progress};

/// Build a `TestRtc` whose `Rtc` has RFC 2198 RED enabled, honouring the per-peer crypto
/// provider env vars like `TestRtc::new` does.
fn rtc_with_red(peer: Peer, stats: bool) -> TestRtc {
    let now = Instant::now();
    let mut builder = Rtc::builder().enable_opus(true, true);
    if stats {
        builder = builder.set_stats_interval(Some(Duration::from_millis(100)));
    }
    if let Some(crypto) = peer.crypto_provider() {
        builder = builder.set_crypto_provider(crypto);
    }
    TestRtc::new_with_rtc(peer.span(), builder.build(now))
}

fn media_count(r: &TestRtc) -> usize {
    r.events
        .iter()
        .filter(|(_, e)| matches!(e, Event::MediaData(_)))
        .count()
}

/// Connect L and R for SendRecv audio, then send `secs` seconds of Opus frames. `red_l`/`red_r`
/// enable RED on each side; `media_loss` is applied to R's incoming queue *after* the handshake
/// so only the media phase is lossy. Returns both peers (with their collected events).
fn run_audio(
    red_l: bool,
    red_r: bool,
    media_loss: Option<NetemConfig>,
    secs: u64,
) -> (TestRtc, TestRtc) {
    run_audio_options(red_l, red_r, media_loss, secs, false, false)
}

fn run_audio_options(
    red_l: bool,
    red_r: bool,
    media_loss: Option<NetemConfig>,
    secs: u64,
    stats: bool,
    frame_metadata: bool,
) -> (TestRtc, TestRtc) {
    init_log();
    init_crypto_default();

    let mut l = if red_l {
        rtc_with_red(Peer::Left, stats)
    } else {
        TestRtc::new(Peer::Left)
    };
    let mut r = if red_r {
        rtc_with_red(Peer::Right, stats)
    } else {
        TestRtc::new(Peer::Right)
    };

    l.set_forced_time_advance(Duration::from_millis(1));
    r.set_forced_time_advance(Duration::from_millis(1));

    l.add_host_candidate((Ipv4Addr::new(1, 1, 1, 1), 1000).into());
    r.add_host_candidate((Ipv4Addr::new(2, 2, 2, 2), 2000).into());

    let mut change = l.sdp_api();
    let mid = change.add_media(MediaKind::Audio, Direction::SendRecv, None, None, None);
    let (offer, pending) = change.apply().unwrap();
    let answer = r.rtc.sdp_api().accept_offer(offer).unwrap();
    l.rtc.sdp_api().accept_answer(pending, answer).unwrap();

    loop {
        if l.is_connected() || r.is_connected() {
            break;
        }
        progress(&mut l, &mut r).unwrap();
    }

    let max = l.last.max(r.last);
    l.last = max;
    r.last = max;

    // Apply loss only to the media phase, not the DTLS/ICE handshake.
    if let Some(cfg) = media_loss {
        r.set_netem(cfg);
    }

    let pt = l.params_opus().pt();
    let mut start_of_talk_spurt = true;
    let mut frame_index = 0_u8;
    loop {
        let wallclock = l.start + l.duration();
        let time = l.duration().into();
        let value = if frame_metadata { frame_index % 100 } else { 1 };
        let mut writer = l
            .writer(mid)
            .unwrap()
            .start_of_talkspurt(start_of_talk_spurt);
        if frame_metadata {
            writer = writer.audio_level(-(value as i8), true);
        }
        writer.write(pt, wallclock, time, vec![value; 80]).unwrap();
        start_of_talk_spurt = false;
        frame_index = frame_index.wrapping_add(1);

        progress(&mut l, &mut r).unwrap();

        if l.duration() > Duration::from_secs(secs) {
            break;
        }
    }

    // Drain (stats only): let every in-flight packet deliver and both peers emit a final stats
    // snapshot that includes all of them, so cumulative tx/rx byte counters are compared at aligned
    // points. Otherwise the last tx and rx snapshots (each on its own timer) can land a packet apart
    // and the exact-equality check is off by one packet's worth of bytes.
    if stats {
        let drain_until = l.duration() + Duration::from_millis(300);
        while l.duration() < drain_until {
            progress(&mut l, &mut r).unwrap();
        }
    }

    (l, r)
}

#[test]
fn red_receive_stats_include_the_complete_wire_payload() {
    let (l, r) = run_audio_options(true, true, None, 2, true, false);

    let sent = l.events.iter().rev().find_map(|(_, e)| match e {
        Event::PeerStats(s) if s.bytes_tx > 0 => Some(s.bytes_tx),
        _ => None,
    });
    let received = r.events.iter().rev().find_map(|(_, e)| match e {
        Event::PeerStats(s) if s.bytes_rx > 0 => Some(s.bytes_rx),
        _ => None,
    });

    assert_eq!(
        received, sent,
        "RED receive accounting must include headers and redundant media counted by the sender"
    );
}

#[test]
fn red_recovered_frames_do_not_inherit_the_carrier_audio_level() {
    let loss = NetemConfig::new()
        .loss(RandomLoss::new(Probability::new(0.08)))
        .seed(7);
    let (_, r) = run_audio_options(true, true, Some(loss), 4, false, true);

    // A RED-recovered frame carries no audio level of its own: RFC 2198 redundant blocks carry
    // only codec payload, not RTP header extensions, so the frame's original audio level is not on
    // the wire and is delivered as None. What it must never do is inherit the carrier packet's
    // level, so flag only a frame that carries a level different from its own.
    let mismatched = r.events.iter().find_map(|(_, e)| match e {
        Event::MediaData(m) => {
            let expected = -(m.data[0] as i8);
            m.ext_vals
                .audio_level
                .filter(|&lvl| lvl != expected)
                .map(|lvl| (m.data[0], lvl, expected))
        }
        _ => None,
    });

    assert!(
        mismatched.is_none(),
        "recovered frame inherited carrier metadata: {mismatched:?}"
    );
}

/// With RED enabled on both peers, audio flows transparently: the application writes and reads
/// plain Opus while RED wrapping/unwrapping happens on the wire.
#[test]
pub fn red_transparent_roundtrip() {
    let (l, r) = run_audio(true, true, None, 5);

    // RED must have been negotiated on both sides.
    assert!(l.params_opus().red().is_some(), "L should negotiate RED");
    assert!(r.params_opus().red().is_some(), "R should negotiate RED");

    let media: Vec<_> = r
        .events
        .iter()
        .filter_map(|(_, e)| match e {
            Event::MediaData(m) => Some(m),
            _ => None,
        })
        .collect();

    // The app sees Opus, never RED, and the payload round-trips.
    assert!(media.len() > 100, "Not enough MediaData: {}", media.len());
    assert_eq!(media[0].params.spec().codec, Codec::Opus);
    assert_eq!(media[0].pt, l.params_opus().pt());
    assert_eq!(&media[0].data[..], &[1_u8; 80][..]);
}

/// The offer must list the RED payload type in the m-line `fmt` list, not only as an `a=rtpmap`.
/// A strict peer (e.g. a browser) ignores an rtpmap whose PT is absent from the m-line, so this
/// is required for RED to negotiate at all. Regression test: str0m-to-str0m is lenient and would
/// otherwise hide the omission.
#[test]
pub fn red_offer_lists_red_pt_in_mline() {
    init_log();
    init_crypto_default();

    let mut l = rtc_with_red(Peer::Left, false);
    l.add_host_candidate((Ipv4Addr::new(1, 1, 1, 1), 1000).into());

    let mut change = l.sdp_api();
    change.add_media(MediaKind::Audio, Direction::SendRecv, None, None, None);
    let (offer, _pending) = change.apply().unwrap();

    let red_pt = l.params_opus().red().expect("RED configured");
    let sdp = offer.to_sdp_string();
    let mline = sdp
        .lines()
        .find(|line| line.starts_with("m=audio"))
        .expect("m=audio line");
    // m=<media> <port> <proto> <fmt>...
    let fmts: Vec<&str> = mline.split_whitespace().skip(3).collect();

    assert!(
        fmts.contains(&red_pt.to_string().as_str()),
        "m-line fmt list must include the RED pt {red_pt}; got: {mline}"
    );
}

/// The m-line's payload types, in order.
fn audio_fmts(sdp: &str) -> Vec<String> {
    let mline = sdp
        .lines()
        .find(|line| line.starts_with("m=audio"))
        .expect("m=audio line");
    mline
        .split_whitespace()
        .skip(3)
        .map(str::to_owned)
        .collect()
}

/// libwebrtc enables RED only when it precedes the selected audio codec. Chrome offers Opus
/// before RED, so an answer that keeps that order negotiates RED but does not make Chrome send it.
/// The answer lists RED before Opus, as does our own offer.
#[test]
pub fn red_listed_before_its_primary() {
    init_log();
    init_crypto_default();

    let mut l = rtc_with_red(Peer::Left, false);
    let mut r = rtc_with_red(Peer::Right, false);
    l.add_host_candidate((Ipv4Addr::new(1, 1, 1, 1), 1000).into());
    r.add_host_candidate((Ipv4Addr::new(2, 2, 2, 2), 2000).into());

    let mut change = l.sdp_api();
    change.add_media(MediaKind::Audio, Direction::SendRecv, None, None, None);
    let (offer, pending) = change.apply().unwrap();

    let opus = l.params_opus().pt().to_string();
    let red = l.params_opus().red().expect("RED configured").to_string();
    let position = |fmts: &[String], pt: &str| fmts.iter().position(|f| f == pt).unwrap();
    let fmts = audio_fmts(&offer.to_sdp_string());
    assert!(
        position(&fmts, &red) < position(&fmts, &opus),
        "offer: {fmts:?}"
    );

    // As Chrome offers it: Opus first, then RED.
    let sdp = offer.to_sdp_string();
    let mline = sdp
        .lines()
        .find(|line| line.starts_with("m=audio"))
        .unwrap();
    let mut words: Vec<&str> = mline.split_whitespace().collect();
    let (i, j) = (
        words.iter().position(|w| *w == opus).unwrap(),
        words.iter().position(|w| *w == red).unwrap(),
    );
    words.swap(i, j);
    let browser_like = sdp.replace(mline, &words.join(" "));
    let fmts = audio_fmts(&browser_like);
    assert!(position(&fmts, &opus) < position(&fmts, &red));

    let offer = SdpOffer::from_sdp_string(&browser_like).unwrap();
    let answer = r.rtc.sdp_api().accept_offer(offer).unwrap();
    let fmts = audio_fmts(&answer.to_sdp_string());
    assert!(
        position(&fmts, &red) < position(&fmts, &opus),
        "answer: {fmts:?}"
    );

    l.rtc.sdp_api().accept_answer(pending, answer).unwrap();
    assert!(l.params_opus().red().is_some(), "RED stays negotiated");
}

/// Under independent (isolated) packet loss, RED recovers the lost frames from the next packet's
/// redundancy. With the same loss seed, the RED receiver gets strictly more frames than a plain
/// Opus receiver, which has no way to fill the gaps.
#[test]
pub fn red_recovers_single_loss() {
    let loss = || {
        NetemConfig::new()
            .loss(RandomLoss::new(Probability::new(0.08)))
            .seed(7)
    };

    let (_, r_red) = run_audio(true, true, Some(loss()), 4);
    let (_, r_plain) = run_audio(false, false, Some(loss()), 4);

    let red = media_count(&r_red);
    let plain = media_count(&r_plain);

    assert!(
        red > plain,
        "RED should recover isolated losses: red={red} plain={plain}"
    );
}

/// If only one side enables RED, negotiation falls back to plain Opus (RED is opt-in and kept
/// only when both peers offer it). Media still flows.
#[test]
pub fn red_interop_fallback() {
    let (l, r) = run_audio(true, false, None, 2);

    assert_eq!(
        l.params_opus().red(),
        None,
        "L must drop RED when R does not offer it"
    );
    assert_eq!(r.params_opus().red(), None, "R never enabled RED");
    assert!(media_count(&r) > 50, "media should still flow without RED");
}

/// Mirror of the fallback: the offerer has RED off and the answerer has it on. RED must still be
/// dropped (it is kept only when the offer carries it), and media flows as plain Opus.
#[test]
pub fn red_interop_fallback_mirror() {
    let (l, r) = run_audio(false, true, None, 2);

    assert_eq!(l.params_opus().red(), None, "L never offered RED");
    assert_eq!(
        r.params_opus().red(),
        None,
        "R must drop RED when the offer does not carry it"
    );
    assert!(media_count(&r) > 50, "media should still flow without RED");
}

/// The runtime send-side toggle turns RED wrapping on and off with no renegotiation. With R in RTP
/// mode we read the wire PT directly: RED-wrapped packets carry the RED PT, unwrapped packets carry
/// the plain Opus PT. Toggling mid-session flips which PT R receives while RED stays negotiated the
/// whole time, matching the SFU case of enabling redundancy on a lossy leg and disabling it later.
#[test]
pub fn red_send_toggle_no_renegotiation() {
    init_log();
    init_crypto_default();

    let mut l = rtc_with_red(Peer::Left, false); // frame mode, RED on: wraps outgoing Opus

    let now = Instant::now();
    let mut r_builder = Rtc::builder().set_rtp_mode(true).enable_opus(true, true);
    if let Some(crypto) = Peer::Right.crypto_provider() {
        r_builder = r_builder.set_crypto_provider(crypto);
    }
    let mut r = TestRtc::new_with_rtc(Peer::Right.span(), r_builder.build(now));

    l.set_forced_time_advance(Duration::from_millis(1));
    r.set_forced_time_advance(Duration::from_millis(1));

    l.add_host_candidate((Ipv4Addr::new(1, 1, 1, 1), 1000).into());
    r.add_host_candidate((Ipv4Addr::new(2, 2, 2, 2), 2000).into());

    let mut change = l.sdp_api();
    let mid = change.add_media(MediaKind::Audio, Direction::SendRecv, None, None, None);
    let (offer, pending) = change.apply().unwrap();
    let answer = r.rtc.sdp_api().accept_offer(offer).unwrap();
    l.rtc.sdp_api().accept_answer(pending, answer).unwrap();

    loop {
        if l.is_connected() || r.is_connected() {
            break;
        }
        progress(&mut l, &mut r).unwrap();
    }

    let max = l.last.max(r.last);
    l.last = max;
    r.last = max;

    let red_pt = l.params_opus().red().expect("RED negotiated");
    let opus_pt = l.params_opus().pt();
    let data = vec![1_u8; 80];

    // Phase 1 (< 1s): RED on, wire carries the RED pt. At 1s, toggle RED sending off with no
    // renegotiation. Phase 2 (1s..3s): the same media goes out as plain Opus.
    let mut toggled = false;
    let mut start_of_talk_spurt = true;
    loop {
        let wallclock = l.start + l.duration();
        let time = l.duration().into();
        l.writer(mid)
            .unwrap()
            .start_of_talkspurt(start_of_talk_spurt)
            .write(opus_pt, wallclock, time, data.clone())
            .unwrap();
        start_of_talk_spurt = false;

        progress(&mut l, &mut r).unwrap();

        if !toggled && l.duration() > Duration::from_secs(1) {
            l.rtc.direct_api().set_red_send(mid, false);
            toggled = true;
        }
        if l.duration() > Duration::from_secs(3) {
            break;
        }
    }

    // RED is still negotiated: the toggle never touched SDP.
    assert!(l.params_opus().red().is_some(), "RED must stay negotiated");

    let saw_red = r
        .events
        .iter()
        .any(|(_, e)| matches!(e, Event::RtpPacket(p) if p.header.payload_type == red_pt));
    let saw_opus = r
        .events
        .iter()
        .any(|(_, e)| matches!(e, Event::RtpPacket(p) if p.header.payload_type == opus_pt));

    assert!(saw_red, "phase 1 should wrap outgoing media in RED");
    assert!(saw_opus, "phase 2 should send plain Opus after the toggle");
}

/// In RTP mode the RED packets are forwarded as-is: the receiver sees `Event::RtpPacket` carrying
/// the RED payload type, and the RED structure is parseable with the public decoder.
#[test]
pub fn red_rtp_mode_passthrough() {
    init_log();
    init_crypto_default();

    let mut l = rtc_with_red(Peer::Left, false); // frame mode, RED on: wraps outgoing Opus

    let now = Instant::now();
    let mut r_builder = Rtc::builder().set_rtp_mode(true).enable_opus(true, true);
    if let Some(crypto) = Peer::Right.crypto_provider() {
        r_builder = r_builder.set_crypto_provider(crypto);
    }
    let mut r = TestRtc::new_with_rtc(Peer::Right.span(), r_builder.build(now));

    l.set_forced_time_advance(Duration::from_millis(1));
    r.set_forced_time_advance(Duration::from_millis(1));

    l.add_host_candidate((Ipv4Addr::new(1, 1, 1, 1), 1000).into());
    r.add_host_candidate((Ipv4Addr::new(2, 2, 2, 2), 2000).into());

    let mut change = l.sdp_api();
    let mid = change.add_media(MediaKind::Audio, Direction::SendRecv, None, None, None);
    let (offer, pending) = change.apply().unwrap();
    let answer = r.rtc.sdp_api().accept_offer(offer).unwrap();
    l.rtc.sdp_api().accept_answer(pending, answer).unwrap();

    loop {
        if l.is_connected() || r.is_connected() {
            break;
        }
        progress(&mut l, &mut r).unwrap();
    }

    let max = l.last.max(r.last);
    l.last = max;
    r.last = max;

    let red_pt = l.params_opus().red().expect("RED negotiated");
    let pt = l.params_opus().pt();
    let data = vec![1_u8; 80];

    let mut start_of_talk_spurt = true;
    loop {
        let wallclock = l.start + l.duration();
        let time = l.duration().into();
        l.writer(mid)
            .unwrap()
            .start_of_talkspurt(start_of_talk_spurt)
            .write(pt, wallclock, time, data.clone())
            .unwrap();
        start_of_talk_spurt = false;

        progress(&mut l, &mut r).unwrap();

        if l.duration() > Duration::from_secs(2) {
            break;
        }
    }

    // R is in RTP mode: it sees the raw RED packets (PT == red), not Opus MediaData.
    let red_packets: Vec<_> = r
        .events
        .iter()
        .filter_map(|(_, e)| match e {
            Event::RtpPacket(p) if p.header.payload_type == red_pt => Some(p),
            _ => None,
        })
        .collect();

    assert!(
        !red_packets.is_empty(),
        "R should receive raw RED RtpPackets in rtp mode"
    );
    // The RED payload is well-formed and parseable with the public decoder.
    assert!(RedDecoder::decode(&red_packets[0].payload).is_ok());
}

// ---------------------------------------------------------------------------------------------
// Hand-crafted RED on the wire.
//
// L runs in RTP mode and writes RED packets built with the public encoder, so the tests control
// the exact redundancy pattern, the frame timing and which sequence numbers are lost. R runs in
// frame mode and has to unwrap and recover. Frame `k` carries payload `[k; 80]` so every emitted
// `MediaData` identifies which frame it is, and `seq_range`/`time` show where R placed it.
// ---------------------------------------------------------------------------------------------

mod raw {
    use super::*;
    use common::connect_l_r_with_rtc;
    use str0m::Rtc;
    use str0m::media::{Frequency, MediaData, Mid, Pt};
    use str0m::rtp::rtcp::Rtcp;
    use str0m::rtp::{RawPacket, RedEncoder, RedundantBlock, RtpWrite, SeqNo, Ssrc};

    pub const BASE_SEQ: u64 = 47_000;
    pub const BASE_TS: u32 = 10_000;
    pub const FRAME: u32 = 960; // Opus 20 ms @ 48 kHz

    pub struct RawRed {
        pub l: TestRtc,
        pub r: TestRtc,
        pub mid: Mid,
        pub ssrc: Ssrc,
        pub opus_pt: Pt,
        pub red_pt: Pt,
    }

    /// Connect L (RTP mode) and R (frame mode, `r_reordering` = `set_reordering_size_audio`)
    /// with Opus + RED over DirectApi.
    pub fn connect(r_reordering: Option<usize>) -> RawRed {
        init_log();
        init_crypto_default();
        let now = Instant::now();

        let mut lb = Rtc::builder()
            .set_rtp_mode(true)
            .enable_raw_packets(true)
            .enable_opus(true, true);
        if let Some(c) = Peer::Left.crypto_provider() {
            lb = lb.set_crypto_provider(c);
        }
        let mut rb = Rtc::builder()
            .enable_raw_packets(true)
            .enable_opus(true, true);
        if let Some(n) = r_reordering {
            rb = rb.set_reordering_size_audio(n);
        }
        if let Some(c) = Peer::Right.crypto_provider() {
            rb = rb.set_crypto_provider(c);
        }

        let (mut l, mut r) = connect_l_r_with_rtc(lb.build(now), rb.build(now));

        let mid: Mid = "aud".into();
        let ssrc: Ssrc = 42.into();

        l.direct_api().declare_media(mid, MediaKind::Audio);
        l.direct_api().declare_stream_tx(ssrc, None, mid, None);
        r.direct_api().declare_media(mid, MediaKind::Audio);
        r.direct_api().expect_stream_rx(ssrc, None, mid, None);

        let max = l.last.max(r.last);
        l.last = max;
        r.last = max;

        let opus = l.params_opus();
        let red_pt = opus.red().expect("RED enabled on L");
        assert!(r.params_opus().red().is_some(), "RED enabled on R");

        RawRed {
            l,
            r,
            mid,
            ssrc,
            opus_pt: opus.pt(),
            red_pt,
        }
    }

    impl RawRed {
        /// Send frames `0..ts.len()` at the given RTP timestamps, seq `BASE_SEQ + k`. Each packet
        /// carries, as redundancy, the frames `k - d` for every `d` in `distances` (oldest first).
        /// Frames whose index is in `lost` are built (so their redundancy history is right) but
        /// never written. Then lets everything settle.
        pub fn send(&mut self, ts: &[u32], distances: &[u8], lost: &[u8]) {
            let payload = |k: u8| vec![k; 80];
            let n = ts.len() as u8;

            for k in 0..n {
                let mut blocks: Vec<RedundantBlock> = distances
                    .iter()
                    .filter_map(|&d| k.checked_sub(d))
                    .map(|kk| RedundantBlock {
                        pt: *self.opus_pt,
                        timestamp_offset: ts[k as usize] - ts[kk as usize],
                        payload: payload(kk),
                    })
                    .collect();
                // RFC 2198: oldest first.
                blocks.sort_by_key(|b| std::cmp::Reverse(b.timestamp_offset));

                let red = RedEncoder::encode(*self.opus_pt, &payload(k), &blocks);

                if !lost.contains(&k) {
                    let wallclock = self.l.start + self.l.duration();
                    let seq_no: SeqNo = (BASE_SEQ + k as u64).into();
                    let mut direct = self.l.direct_api();
                    let stream = direct.stream_tx(&self.ssrc).unwrap();
                    stream.write_rtp(
                        RtpWrite::new(self.red_pt, seq_no, ts[k as usize], wallclock, red)
                            .marker(k == 0),
                    );
                }

                progress(&mut self.l, &mut self.r).unwrap();
            }

            let settle = self.l.duration() + Duration::from_secs(2);
            while self.l.duration() < settle {
                progress(&mut self.l, &mut self.r).unwrap();
            }
        }

        pub fn media(&self) -> Vec<&MediaData> {
            self.r
                .events
                .iter()
                .filter_map(|(_, e)| match e {
                    Event::MediaData(m) if m.mid == self.mid => Some(m),
                    _ => None,
                })
                .collect()
        }

        /// Every delivered frame must sit at its own sequence number and RTP time. A RED
        /// recovery that maps a redundant block to the wrong seq shows up here.
        pub fn assert_consistent(&self, ts: &[u32]) {
            for m in self.media() {
                let k = m.data[0];
                let seq = *m.seq_range.start();
                let time = m.time.rebase(Frequency::FORTY_EIGHT_KHZ).numer();
                assert_eq!(
                    *seq,
                    BASE_SEQ + k as u64,
                    "frame {k} delivered at seq {seq} (expected {})",
                    BASE_SEQ + k as u64
                );
                assert_eq!(
                    time, ts[k as usize] as u64,
                    "frame {k} delivered at rtp time {time} (expected {})",
                    ts[k as usize]
                );
            }
        }

        pub fn delivered(&self, k: u8) -> usize {
            self.media().iter().filter(|m| m.data[0] == k).count()
        }

        /// Sequence numbers R asked to have retransmitted (extended against `BASE_SEQ`).
        pub fn nacked_seqs(&self) -> Vec<u64> {
            self.r
                .events
                .iter()
                .filter_map(|(_, e)| match e.as_raw_packet() {
                    Some(RawPacket::RtcpTx(Rtcp::Nack(n))) => Some(n),
                    _ => None,
                })
                .flat_map(|n| {
                    n.reports
                        .iter()
                        .flat_map(|r| r.into_iter(SeqNo::from(BASE_SEQ)).map(|s| *s))
                })
                .collect()
        }
    }

    pub fn uniform(n: usize) -> Vec<u32> {
        (0..n as u32).map(|k| BASE_TS + k * FRAME).collect()
    }
}

use raw::{BASE_SEQ, FRAME};

/// A remote sender is not bound by our `set_red_distances` sanitiser. With a `[2, 4]` pattern and
/// no main-1 block, the receiver must not assume the closest block is one packet back: after
/// losing frames 5 and 6, frame 7's closest block is frame 5, which must never be delivered as
/// seq 6.
#[test]
fn red_recovery_never_misplaces_frames_for_a_pattern_without_main_minus_one() {
    let mut t = raw::connect(None);
    let ts = raw::uniform(24);

    t.send(&ts, &[2, 4], &[5, 6]);

    t.assert_consistent(&ts);
}

/// Opus frame duration is not constant (10/20/40/60 ms, adaptive ptime, DTX). Offsets alone then
/// don't give a sequence distance: with a libwebrtc-like `[1, 2, 3]` pattern, where frame 4 is a
/// 40 ms frame and frames 3..=6 are lost, frame 7's main-3 block (frame 4) sits 3840 ticks back,
/// which looks like four 20 ms packets. It must not be delivered as seq 3.
#[test]
fn red_recovery_never_misplaces_frames_when_frame_duration_varies() {
    let mut t = raw::connect(None);

    let mut ts = raw::uniform(24);
    // Frame 4 lasts 40 ms: everything after it shifts by one extra frame.
    for v in ts.iter_mut().skip(5) {
        *v += FRAME;
    }

    t.send(&ts, &[1, 2, 3], &[3, 4, 5, 6]);

    t.assert_consistent(&ts);
}

/// A sequence number recovered from RED was never on the wire, but the receiver has the frame,
/// so it must not ask for a retransmission of it. Frame 5 is recovered from frame 6. Frame 9 is
/// lost together with frame 10 (which carried its redundancy) and is therefore genuinely
/// missing: that one must still be NACKed, proving the NACK path is live.
#[test]
fn red_recovered_seq_no_is_not_nacked() {
    let mut t = raw::connect(None);
    let ts = raw::uniform(60);

    t.send(&ts, &[1], &[5, 9, 10]);

    assert_eq!(t.delivered(5), 1, "frame 5 recovered from RED");
    assert_eq!(t.delivered(9), 0, "frame 9 had no surviving redundancy");

    let nacked = t.nacked_seqs();
    assert!(
        nacked.contains(&(BASE_SEQ + 9)),
        "genuinely lost seq must be NACKed: {nacked:?}"
    );
    assert!(
        !nacked.contains(&(BASE_SEQ + 5)),
        "RED-recovered seq must not be NACKed: {nacked:?}"
    );
}

/// With multi-level redundancy the same lost frame is carried by several later packets. It must
/// be delivered once, also when the application has turned the reordering buffer off
/// (`set_reordering_size_audio(0)`), which is exactly the configuration that does not
/// de-duplicate late arrivals.
#[test]
fn red_recovered_frame_is_delivered_once_without_reordering_buffer() {
    let mut t = raw::connect(Some(0));
    let ts = raw::uniform(30);

    t.send(&ts, &[1, 3], &[5]);

    t.assert_consistent(&ts);
    assert_eq!(t.delivered(5), 1, "frame 5 delivered exactly once");
    for k in 0..30u8 {
        assert!(
            t.delivered(k) <= 1,
            "frame {k} delivered {} times",
            t.delivered(k)
        );
    }
}

/// A burst of three losses under a `[1, 3, 5]` pattern is filled in over several later packets
/// (8 and 10 from packet 11, then 9 from packet 12 once 8 and 10 are known). Each frame must be
/// delivered exactly once, at its own sequence number, and nothing gets NACKed.
#[test]
fn red_recovers_a_burst_over_several_packets() {
    let mut t = raw::connect(None);
    let ts = raw::uniform(30);

    t.send(&ts, &[1, 3, 5], &[8, 9, 10]);

    t.assert_consistent(&ts);
    for k in 0..30u8 {
        assert_eq!(t.delivered(k), 1, "frame {k} delivered exactly once");
    }
    assert!(t.nacked_seqs().is_empty(), "nothing to NACK");
}

/// The per-mid ingress stats must count the full RED wire payload like the per-mid egress stats
/// on the sender do (and like `PeerStats` already does), not just the unwrapped primary block.
#[test]
fn red_media_ingress_stats_match_media_egress_stats() {
    let (l, r) = run_audio_options(true, true, None, 2, true, false);

    let sent = l.events.iter().rev().find_map(|(_, e)| match e {
        Event::MediaEgressStats(s) if s.bytes > 0 => Some(s.bytes),
        _ => None,
    });
    let received = r.events.iter().rev().find_map(|(_, e)| match e {
        Event::MediaIngressStats(s) if s.bytes > 0 => Some(s.bytes),
        _ => None,
    });

    assert!(sent.is_some(), "egress stats emitted");
    assert_eq!(
        received, sent,
        "RED ingress accounting must include the RED headers and redundant blocks"
    );
}
