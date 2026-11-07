//! Early simulcast RTX padding must receive transport feedback before its main SSRC is known.
//!
//! Negotiate and connect real peers, then inject encrypted packets through `Rtc::handle_input`.
//! Known test keys let us cover each SRTP profile and authenticate deliberately malformed payloads.

use std::net::{Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};

use str0m::_internal_test_exports::TestSrtp;
use str0m::change::SdpOffer;
use str0m::crypto::dtls::{KeyingMaterial, SrtpProfile};
use str0m::media::{Direction, MediaKind, Mid, Simulcast, SimulcastLayer};
use str0m::net::{Protocol, Receive};
use str0m::rtp::rtcp::Rtcp;
use str0m::rtp::{ExtensionValues, RawPacket, RtpHeader};
use str0m::{Event, Input, Output};

mod common;
use common::{Peer, TestRtc, init_crypto_default, init_log, progress};

const RTX_SSRC: u32 = 426592647;
const MAIN_SSRC: u32 = 2869047233;
const L_ADDR: SocketAddr = SocketAddr::new(std::net::IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1)), 1000);
const R_ADDR: SocketAddr = SocketAddr::new(std::net::IpAddr::V4(Ipv4Addr::new(2, 2, 2, 2)), 2000);
const FEEDBACK_WAIT: Duration = Duration::from_millis(50);
const STATS_INTERVAL: Duration = Duration::from_millis(200);

#[test]
fn authenticated_padding_is_reported_before_main_ssrc_discovery() {
    for profile in [
        SrtpProfile::AES128_CM_SHA1_80,
        SrtpProfile::AEAD_AES_128_GCM,
        SrtpProfile::AEAD_AES_256_GCM,
    ] {
        for rtp_mode in [false, true] {
            let (mut receiver, mut srtp, mid) = receiver(rtp_mode, profile);
            // Cross RTP rollover, reorder two packets, and vary the padding PT.
            let receipts = [
                (65534, 40),
                (65535, 41),
                (65537, 43),
                (65536, 42),
                (65540, 44),
            ];
            for (seq, twcc) in receipts {
                let mut header = header(mid, true, seq as u16, twcc);
                if twcc == 40 {
                    header.payload_type = 96.into();
                }
                let packet = protect(&receiver, &mut srtp, header, seq, &[0, 0, 0, 4]);
                inject(&mut receiver, &packet);
                inject(&mut receiver, &packet);
            }

            advance(&mut receiver, FEEDBACK_WAIT);
            let received = feedback(&receiver);
            let expected = [(40, true), (41, true), (42, true), (43, true), (44, true)];
            assert_eq!(received, expected);
            assert_eq!(raw_receipts(&receiver), 5, "replays must be rejected");
            assert_no_stream(&mut receiver, mid);

            advance(&mut receiver, STATS_INTERVAL);
            assert_loss(&receiver, 0.0);
            for (_, event) in &receiver.events {
                match event {
                    Event::PeerStats(stats) => assert_eq!(stats.bytes_rx, 0),
                    Event::MediaIngressStats(_)
                    | Event::MediaData(_)
                    | Event::RtpPacket(_)
                    | Event::StreamPaused(_) => panic!("early padding emitted media: {event:?}"),
                    _ => {}
                }
            }
        }
    }
}

#[test]
fn failed_authentication_and_invalid_padding_are_not_reported_received() {
    let (mut receiver, mut srtp, mid) = receiver(true, SrtpProfile::AEAD_AES_128_GCM);
    let forged_header = header(mid, true, 65530, 39);
    let mut forged = protect(&receiver, &mut srtp, forged_header, 65530, &[0, 0, 0, 4]);
    *forged.last_mut().unwrap() ^= 1;
    inject(&mut receiver, &forged);
    advance(&mut receiver, FEEDBACK_WAIT);
    assert!(feedback(&receiver).is_empty());
    assert_eq!(raw_receipts(&receiver), 0);

    // The forged high sequence number must not advance the ROC used to decrypt these packets.
    for twcc in 40..=43 {
        let header = header(mid, true, twcc, twcc);
        let payload: &[u8] = if twcc == 42 { &[2] } else { &[0, 0, 0, 4] };
        let mut packet = protect(&receiver, &mut srtp, header, twcc as u64, payload);
        if twcc == 41 {
            *packet.last_mut().unwrap() ^= 1;
        }
        inject(&mut receiver, &packet);
    }
    advance(&mut receiver, FEEDBACK_WAIT);
    let received = feedback(&receiver);
    // Input processing can send a report immediately, splitting the valid receipts
    // across reports. Missing numbers need not appear in either report's range.
    let received: Vec<_> = received
        .into_iter()
        .filter(|(_, received)| *received)
        .collect();
    assert_eq!(received, [(40, true), (43, true)]);
    assert_eq!(raw_receipts(&receiver), 2);
    assert_no_stream(&mut receiver, mid);
    advance(&mut receiver, STATS_INTERVAL);
    assert_loss(&receiver, 0.5);
}

#[test]
fn invalid_or_nonpadding_packets_do_not_create_receive_history() {
    let (mut receiver, mut srtp, mid) = receiver(true, SrtpProfile::AEAD_AES_128_GCM);
    for case in 0..6 {
        let mut header = header(mid, true, 100, 40);
        match case {
            0 => header.ext_vals.mid = None,
            1 => header.ext_vals.mid = Some("unknown".into()),
            2 => header.ext_vals.rid_repair = Some("unknown".into()),
            3 => header.payload_type = 127.into(),
            4 => header.has_padding = false,
            5 => {}
            _ => unreachable!(),
        }
        let media: &[u8] = &[0, 101, 0, 1];
        let padding: &[u8] = &[0, 0, 0, 4];
        let payload = if case == 5 { media } else { padding };
        let packet = protect(&receiver, &mut srtp, header, 100, payload);
        inject(&mut receiver, &packet);
    }
    advance(&mut receiver, FEEDBACK_WAIT);
    assert!(feedback(&receiver).is_empty());
    assert_eq!(raw_receipts(&receiver), 0);
    assert_no_stream(&mut receiver, mid);

    // A valid packet with the same sequence number must still be accepted: none of the
    // rejected packets may consume its place in the replay window.
    let header = header(mid, true, 100, 40);
    let packet = protect(&receiver, &mut srtp, header, 100, &[0, 0, 0, 4]);
    inject(&mut receiver, &packet);
    advance(&mut receiver, FEEDBACK_WAIT);
    assert_eq!(feedback(&receiver), [(40, true)]);
    assert_eq!(raw_receipts(&receiver), 1);
}

#[test]
fn main_discovery_preserves_rtx_rollover_and_replay_protection() {
    let (mut receiver, mut srtp, mid) = receiver(true, SrtpProfile::AEAD_AES_128_GCM);
    let mut early = header(mid, true, 65535, 40);
    early.payload_type = 96.into();
    let early_packet = protect(&receiver, &mut srtp, early, 65535, &[0, 0, 0, 4]);
    inject(&mut receiver, &early_packet);
    assert_no_stream(&mut receiver, mid);

    let main = header(mid, false, 100, 41);
    let main_packet = protect(&receiver, &mut srtp, main, 100, &[0x10, 1, 2, 3]);
    inject(&mut receiver, &main_packet);
    drain(&mut receiver);
    assert_media_packets(&receiver, &[(100, &[0x10, 1, 2, 3])]);

    // A delayed padding packet below the early high-water mark must still count.
    let delayed = header(mid, true, 65534, 39);
    let delayed_packet = protect(&receiver, &mut srtp, delayed, 65534, &[0, 0, 0, 4]);
    let raw_count = raw_receipts(&receiver);
    inject(&mut receiver, &delayed_packet);
    inject(&mut receiver, &delayed_packet);
    drain(&mut receiver);
    assert_eq!(raw_receipts(&receiver), raw_count + 1);
    assert_media_packets(&receiver, &[(100, &[0x10, 1, 2, 3])]);

    let mut repair = header(mid, true, 0, 42);
    repair.has_padding = false;
    let payload = &[0, 101, 0x10, 4, 5, 6];
    let packet = protect(&receiver, &mut srtp, repair, 65536, payload);
    inject(&mut receiver, &packet);
    drain(&mut receiver);
    let raw_count = raw_receipts(&receiver);
    inject(&mut receiver, &early_packet);
    advance(&mut receiver, FEEDBACK_WAIT);
    assert_eq!(raw_receipts(&receiver), raw_count);
    let expected: &[(u64, &[u8])] = &[(100, &[0x10, 1, 2, 3]), (101, &[0x10, 4, 5, 6])];
    assert_media_packets(&receiver, expected);
    let received = feedback(&receiver);
    assert_eq!(received, [(39, true), (40, true), (41, true), (42, true)]);
}

fn receiver(rtp_mode: bool, profile: SrtpProfile) -> (TestRtc, TestSrtp, Mid) {
    init_log();
    init_crypto_default();
    let configure = |config: str0m::RtcConfig| {
        config
            .clear_codecs()
            .enable_vp8(true)
            .set_rtp_mode(rtp_mode)
            .set_reordering_size_video(0)
            .set_stats_interval(Some(STATS_INTERVAL))
            .enable_raw_packets(true)
    };
    let mut sender = TestRtc::new_with_config(Peer::Left, configure);
    let mut receiver = TestRtc::new_with_config(Peer::Right, configure);
    sender.add_host_candidate(L_ADDR);
    receiver.add_host_candidate(R_ADDR);
    let mut simulcast = Simulcast::new();
    for rid in ["l", "m", "h"] {
        simulcast.add_send_layer(SimulcastLayer::new(rid));
    }
    let mut change = sender.sdp_api();
    let kind = MediaKind::Video;
    let direction = Direction::SendOnly;
    let mid = change.add_media(kind, direction, None, None, Some(simulcast));
    let (offer, pending) = change.apply().unwrap();
    // Browsers can leave SSRC discovery entirely to MID/RID header extensions.
    let sdp = offer
        .to_sdp_string()
        .lines()
        .filter(|l| !l.starts_with("a=ssrc"))
        .collect::<Vec<_>>()
        .join("\r\n");
    let offer = SdpOffer::from_sdp_string(&(sdp + "\r\n")).unwrap();
    let answer = receiver.sdp_api().accept_offer(offer).unwrap();
    sender.sdp_api().accept_answer(pending, answer).unwrap();
    while !sender.is_connected() || !receiver.is_connected() {
        let timed_out = sender.duration() >= Duration::from_secs(10);
        assert!(!timed_out, "failed to connect");
        progress(&mut sender, &mut receiver).unwrap();
    }

    // ICE and DTLS establish a real transport. Replace only the SRTP keys so the packet
    // fixture can exercise every supported profile without depending on DTLS preference order.
    let len = match profile {
        SrtpProfile::AES128_CM_SHA1_80 => 60,
        SrtpProfile::AEAD_AES_128_GCM => 56,
        SrtpProfile::AEAD_AES_256_GCM => 88,
        _ => unreachable!(),
    };
    let crypto = str0m::crypto::from_feature_flags();
    let material = KeyingMaterial::new(&vec![42; len]);
    let srtp = TestSrtp::new(&crypto, profile, &material);
    receiver._set_srtp_keys(material, profile, false);
    drain(&mut receiver);
    receiver.events.clear();
    assert_no_stream(&mut receiver, mid);
    (receiver, srtp, mid)
}

fn header(mid: Mid, repair: bool, seq: u16, twcc: u16) -> RtpHeader {
    let ssrc = if repair { RTX_SSRC } else { MAIN_SSRC };
    RtpHeader {
        has_extension: true,
        has_padding: repair,
        marker: true,
        ssrc: ssrc.into(),
        payload_type: if repair { 97.into() } else { 96.into() },
        sequence_number: seq,
        timestamp: 3000,
        ext_vals: ExtensionValues {
            mid: Some(mid),
            rid: (!repair).then(|| "h".into()),
            rid_repair: repair.then(|| "h".into()),
            transport_cc: Some(twcc),
            ..Default::default()
        },
        ..Default::default()
    }
}

fn protect(
    receiver: &TestRtc,
    srtp: &mut TestSrtp,
    header: RtpHeader,
    seq: u64,
    payload: &[u8],
) -> Vec<u8> {
    srtp.protect(header, seq, payload, receiver._exts())
}

fn inject(receiver: &mut TestRtc, packet: &[u8]) {
    let receive = Receive {
        proto: Protocol::Udp,
        source: L_ADDR,
        destination: R_ADDR,
        contents: packet.try_into().unwrap(),
    };
    let input = Input::Receive(receiver.last, receive);
    receiver.handle_input(input).unwrap();
}

/// Follow the timeouts requested by `Rtc` so missing feedback scheduling fails the test.
fn advance(receiver: &mut TestRtc, duration: Duration) {
    let end = receiver.last + duration;
    loop {
        let next = drain(receiver);
        if next > end {
            break;
        }
        receiver.last = next.max(receiver.last);
        let input = Input::Timeout(receiver.last);
        receiver.handle_input(input).unwrap();
    }
}

fn drain(receiver: &mut TestRtc) -> Instant {
    loop {
        match receiver.poll_output().unwrap() {
            Output::Event(event) => receiver.events.push((receiver.last, event)),
            Output::Transmit(_) => {}
            Output::Timeout(next) => return next,
        }
    }
}

fn feedback(receiver: &TestRtc) -> Vec<(u64, bool)> {
    let mut received = Vec::new();
    for (_, event) in &receiver.events {
        match event.as_raw_packet() {
            Some(RawPacket::RtcpTx(Rtcp::Twcc(twcc))) => {
                let receipts = twcc.clone().into_iter(receiver.start, 0.into());
                let statuses = receipts.map(|(seq, _, received)| (*seq, received.is_some()));
                received.extend(statuses);
            }
            Some(RawPacket::RtcpTx(Rtcp::Nack(_))) => panic!("padding caused NACK feedback"),
            _ => {}
        }
    }
    // Reordering can cause overlapping TWCC reports. Keep distinct statuses so any
    // incorrect loss report remains visible, but do not count an identical report twice.
    received.sort_unstable();
    received.dedup();
    received
}

fn raw_receipts(receiver: &TestRtc) -> usize {
    receiver
        .events
        .iter()
        .filter(|(_, event)| matches!(event.as_raw_packet(), Some(RawPacket::RtpRx(_, _))))
        .count()
}

fn assert_no_stream(receiver: &mut TestRtc, mid: Mid) {
    let mut direct = receiver.direct_api();
    assert!(direct.stream_rx_by_mid(mid, Some("h".into())).is_none());
}

fn assert_loss(receiver: &TestRtc, expected: f32) {
    let losses: Vec<_> = receiver
        .events
        .iter()
        .filter_map(|(_, event)| {
            let Event::PeerStats(stats) = event else {
                return None;
            };
            stats.ingress_loss_fraction
        })
        .collect();
    assert!(!losses.is_empty(), "no ingress loss stats emitted");
    for loss in losses {
        assert_eq!(loss, expected);
    }
}

fn assert_media_packets(receiver: &TestRtc, expected: &[(u64, &[u8])]) {
    let packets: Vec<_> = receiver
        .events
        .iter()
        .filter_map(|(_, event)| {
            let Event::RtpPacket(packet) = event else {
                return None;
            };
            assert_eq!(packet.header.ssrc, MAIN_SSRC.into());
            Some((*packet.seq_no, &*packet.payload))
        })
        .collect();
    assert_eq!(packets, expected);
}
