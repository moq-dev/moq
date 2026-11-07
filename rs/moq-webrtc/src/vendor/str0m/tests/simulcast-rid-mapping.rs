//! Receiver-side mapping of simulcast SSRCs to their RID between two str0m peers.
//!
//! The receiver learns which SSRC carries which simulcast layer from the RID header
//! extension, which it only accepts for rids the SDP declares. The sender only attaches
//! that extension until a receiver report for the SSRC comes back, so every layer must be
//! bound before the extension disappears, and every SDP describing the sender's track
//! must keep declaring the rids.

use std::net::Ipv4Addr;
use std::time::Duration;

use netem::{GilbertElliot, NetemConfig, Probability, RandomLoss};
use str0m::bwe::Bitrate;
use str0m::change::{SdpAnswer, SdpOffer};
use str0m::format::Codec;
use str0m::media::{Direction, Frequency, MediaKind, MediaTime, Mid, Rid};
use str0m::media::{Simulcast, SimulcastLayer};
use str0m::{Event, RtcConfig, RtcError};

mod common;
use common::{Peer, TestRtc, init_crypto_default, init_log, progress};

/// Layer rids with the frame size sent on each.
const LAYERS: [(&str, usize); 3] = [("l", 300), ("m", 1_500), ("h", 6_000)];

/// (RTX negotiated, BWE enabled on the sender).
const PEER_CONFIGS: [(bool, bool); 4] =
    [(true, false), (false, false), (true, true), (false, true)];

const SIGNALLING: [Signalling; 3] = [
    Signalling::MediaInFirstOffer,
    Signalling::Renegotiated { glare: false },
    Signalling::Renegotiated { glare: true },
];

/// How the simulcast media reaches the receiver.
#[derive(Debug, Clone, Copy)]
enum Signalling {
    /// The first offer carries the media.
    MediaInFirstOffer,
    /// The first offer is data-channel only and the media follows in a re-offer after
    /// connecting. With `glare`, the receiver has its own offer outstanding and drops it
    /// to accept the sender's.
    Renegotiated { glare: bool },
}

/// Which peer offers the renegotiation that follows the simulcast negotiation.
#[derive(Debug, Clone, Copy)]
enum Reoffer {
    /// The sender answers, as when an SFU offers it another peer's media.
    FromReceiver,
    FromSender,
}

#[test]
fn simulcast_survives_answering_reoffer_before_media() -> Result<(), RtcError> {
    let all = all_rids();
    assert_simulcast_survives_reoffer(Reoffer::FromReceiver, Duration::ZERO, &all)
}

#[test]
fn simulcast_survives_answering_reoffer_after_media() -> Result<(), RtcError> {
    let all = all_rids();
    assert_simulcast_survives_reoffer(Reoffer::FromReceiver, Duration::from_secs(1), &all)
}

#[test]
fn simulcast_survives_sender_reoffer_before_media() -> Result<(), RtcError> {
    let all = all_rids();
    assert_simulcast_survives_reoffer(Reoffer::FromSender, Duration::ZERO, &all)
}

#[test]
fn simulcast_survives_sender_reoffer_after_media() -> Result<(), RtcError> {
    let all = all_rids();
    assert_simulcast_survives_reoffer(Reoffer::FromSender, Duration::from_secs(1), &all)
}

#[test]
fn simulcast_partially_accepted_survives_answering_reoffer() -> Result<(), RtcError> {
    let accepted = ["l", "m"];
    assert_simulcast_survives_reoffer(Reoffer::FromReceiver, Duration::from_secs(1), &accepted)
}

#[test]
fn simulcast_partially_accepted_survives_sender_reoffer() -> Result<(), RtcError> {
    let accepted = ["l", "m"];
    assert_simulcast_survives_reoffer(Reoffer::FromSender, Duration::from_secs(1), &accepted)
}

#[test]
fn simulcast_rejected_stays_rejected_after_answering_reoffer() -> Result<(), RtcError> {
    assert_simulcast_survives_reoffer(Reoffer::FromReceiver, Duration::ZERO, &[])
}

#[test]
fn simulcast_rejected_stays_rejected_after_sender_reoffer() -> Result<(), RtcError> {
    assert_simulcast_survives_reoffer(Reoffer::FromSender, Duration::ZERO, &[])
}

/// The receiver asks for simulcast in the first offer and the sender answers.
#[test]
fn simulcast_send_as_answerer() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let mut l = TestRtc::new(Peer::Left);
    let mut r = TestRtc::new(Peer::Right);
    l.add_host_candidate((Ipv4Addr::new(1, 1, 1, 1), 1000).into());
    r.add_host_candidate((Ipv4Addr::new(2, 2, 2, 2), 2000).into());

    let all = all_rids();
    let mut simulcast = Simulcast::new();
    for rid in &all {
        simulcast.add_recv_layer(SimulcastLayer::new(rid));
    }

    let mut change = r.sdp_api();
    let mid = change.add_media(
        MediaKind::Video,
        Direction::RecvOnly,
        None,
        None,
        Some(simulcast),
    );
    let (offer, pending) = change.apply().unwrap();
    let answer = l.rtc.sdp_api().accept_offer(offer)?;
    assert_declares_send_simulcast(&answer.to_sdp_string(), mid, &all);
    r.rtc.sdp_api().accept_answer(pending, answer)?;

    connect(&mut l, &mut r)?;
    send_frames(&mut l, &mut r, mid, &all, Duration::from_secs(2))?;
    assert_rids_delivered(&mut r, mid, &all, 0, "sender answered");

    Ok(())
}

#[test]
fn simulcast_rid_mapping_loopback() -> Result<(), RtcError> {
    for (rtx, bwe) in PEER_CONFIGS {
        for signalling in SIGNALLING {
            assert_all_rids_mapped(Duration::ZERO, NetemConfig::new(), rtx, bwe, signalling)?;
        }
    }
    Ok(())
}

#[test]
fn simulcast_rid_mapping_latency() -> Result<(), RtcError> {
    for (rtx, bwe) in PEER_CONFIGS {
        for signalling in SIGNALLING {
            let latency = Duration::from_millis(25);
            assert_all_rids_mapped(latency, NetemConfig::new(), rtx, bwe, signalling)?;
        }
    }
    Ok(())
}

#[test]
fn simulcast_rid_mapping_sweep() -> Result<(), RtcError> {
    for seed in 0..10 {
        for (rtx, bwe) in PEER_CONFIGS {
            for signalling in SIGNALLING {
                let impairments = [
                    NetemConfig::new()
                        .jitter(Duration::from_millis(10))
                        .seed(seed),
                    NetemConfig::new()
                        .loss(RandomLoss::new(Probability::new(0.1)))
                        .seed(seed),
                    NetemConfig::new()
                        .jitter(Duration::from_millis(15))
                        .loss(GilbertElliot::congested())
                        .duplicate(Probability::new(0.02))
                        .reorder_gap(5)
                        .seed(seed),
                ];
                for impairment in impairments {
                    let latency = Duration::from_millis(30);
                    assert_all_rids_mapped(latency, impairment, rtx, bwe, signalling)?;
                }
            }
        }
    }
    Ok(())
}

/// Negotiates simulcast with the receiver's answer restricted to `accepted` (none
/// rejects simulcast), renegotiates after `send_before` of media, and checks that the SDP
/// describing the sender's track keeps declaring exactly the accepted layers, and that
/// every accepted layer keeps arriving with its rid.
fn assert_simulcast_survives_reoffer(
    reoffer: Reoffer,
    send_before: Duration,
    accepted: &[&str],
) -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let mut l = TestRtc::new(Peer::Left);
    let mut r = TestRtc::new(Peer::Right);
    l.add_host_candidate((Ipv4Addr::new(1, 1, 1, 1), 1000).into());
    r.add_host_candidate((Ipv4Addr::new(2, 2, 2, 2), 2000).into());

    let mid = offer_simulcast(&mut l, &mut r, false, accepted)?;
    connect(&mut l, &mut r)?;
    send_frames(&mut l, &mut r, mid, accepted, send_before)?;

    // An unrelated media makes the renegotiation re-describe the simulcast track.
    let sdp = match reoffer {
        Reoffer::FromReceiver => {
            let mut change = r.sdp_api();
            change.add_media(MediaKind::Audio, Direction::SendOnly, None, None, None);
            let (offer, pending) = change.apply().unwrap();
            let offer = SdpOffer::from_sdp_string(&restrict_sdp(&offer.to_sdp_string(), accepted))
                .expect("restricted offer to parse");
            let answer = l.rtc.sdp_api().accept_offer(offer)?;
            let sdp = answer.to_sdp_string();
            r.rtc.sdp_api().accept_answer(pending, answer)?;
            sdp
        }
        Reoffer::FromSender => {
            let mut change = l.sdp_api();
            change.add_media(MediaKind::Audio, Direction::SendOnly, None, None, None);
            let (offer, pending) = change.apply().unwrap();
            let sdp = offer.to_sdp_string();
            let answer = r.rtc.sdp_api().accept_offer(offer)?;
            l.rtc.sdp_api().accept_answer(pending, answer)?;
            sdp
        }
    };
    assert_declares_send_simulcast(&sdp, mid, accepted);

    if !accepted.is_empty() {
        let events_before = r.events.len();
        send_frames(&mut l, &mut r, mid, accepted, Duration::from_secs(2))?;
        let context = format!("{reoffer:?}");
        assert_rids_delivered(&mut r, mid, accepted, events_before, &context);
    }

    Ok(())
}

/// `impairment` applies on top of `latency` once the sender is connected.
fn assert_all_rids_mapped(
    latency: Duration,
    impairment: NetemConfig,
    rtx: bool,
    bwe: bool,
    signalling: Signalling,
) -> Result<(), RtcError> {
    let netem = impairment.latency(latency);
    init_log();
    init_crypto_default();

    let mut l = TestRtc::new_with_config(Peer::Left, |c| {
        let c = vp8_only(c, rtx);
        if bwe {
            c.enable_bwe(Some(Bitrate::kbps(2_500)))
        } else {
            c
        }
    });
    let mut r = TestRtc::new_with_config(Peer::Right, |c| vp8_only(c, rtx));

    l.add_host_candidate((Ipv4Addr::new(1, 1, 1, 1), 1000).into());
    r.add_host_candidate((Ipv4Addr::new(2, 2, 2, 2), 2000).into());

    // The handshake does not survive the harsher configs; the latency alone keeps the
    // relative timing of the two peers' connection realistic.
    let latency_only = NetemConfig::new().latency(latency);
    l.set_netem(latency_only);
    r.set_netem(latency_only);

    let all = all_rids();
    let mid = match signalling {
        Signalling::MediaInFirstOffer => {
            let mid = offer_simulcast(&mut l, &mut r, false, &all)?;
            connect(&mut l, &mut r)?;
            mid
        }
        Signalling::Renegotiated { glare } => {
            let mut change = l.sdp_api();
            change.add_channel("signaling".into());
            let (offer, pending) = change.apply().unwrap();
            let answer = r.rtc.sdp_api().accept_offer(offer)?;
            l.rtc.sdp_api().accept_answer(pending, answer)?;
            connect(&mut l, &mut r)?;
            offer_simulcast(&mut l, &mut r, glare, &all)?
        }
    };

    l.set_netem(netem);
    r.set_netem(netem);

    send_frames(&mut l, &mut r, mid, &all, Duration::from_secs(5))?;

    let context = format!("rtx: {rtx}, bwe: {bwe}, {signalling:?}, netem: {netem:?}");
    assert_rids_delivered(&mut r, mid, &all, 0, &context);

    Ok(())
}

/// Offers every layer and restricts the receiver's answer to the `accepted` rids.
fn offer_simulcast(
    l: &mut TestRtc,
    r: &mut TestRtc,
    glare: bool,
    accepted: &[&str],
) -> Result<Mid, RtcError> {
    let all = all_rids();
    let mut simulcast = Simulcast::new();
    for rid in &all {
        simulcast.add_send_layer(SimulcastLayer::new(rid));
    }

    let mut change = l.sdp_api();
    let mid = change.add_media(
        MediaKind::Video,
        Direction::SendOnly,
        None,
        None,
        Some(simulcast),
    );
    let (offer, pending) = change.apply().unwrap();
    assert_declares_send_simulcast(&offer.to_sdp_string(), mid, &all);

    if glare {
        let mut change = r.sdp_api();
        change.add_media(MediaKind::Video, Direction::SendOnly, None, None, None);
        let (_ignored_offer, _dropped_pending) = change.apply().unwrap();
    }

    let answer = r.rtc.sdp_api().accept_offer(offer)?;
    let answer = SdpAnswer::from_sdp_string(&restrict_sdp(&answer.to_sdp_string(), accepted))
        .expect("restricted answer to parse");
    l.rtc.sdp_api().accept_answer(pending, answer)?;
    Ok(mid)
}

/// Progresses until the sender is connected. Applications start sending as soon as their
/// own side is connected, which can be well before the remote side is.
fn connect(l: &mut TestRtc, r: &mut TestRtc) -> Result<(), RtcError> {
    while !l.is_connected() {
        if l.duration() > Duration::from_secs(10) {
            panic!("Failed to connect");
        }
        progress(l, r)?;
    }
    Ok(())
}

/// Writes 30fps on each of `rids` for `duration`.
fn send_frames(
    l: &mut TestRtc,
    r: &mut TestRtc,
    mid: Mid,
    rids: &[&str],
    duration: Duration,
) -> Result<(), RtcError> {
    let pt = l.params_vp8().pt();
    let start = l.last;

    let mut frame = 0_u32;
    while l.last - start < duration {
        let now = l.last;
        let rtp_time = MediaTime::new(frame as u64 * 3_000, Frequency::NINETY_KHZ);
        for rid in rids {
            l.writer(mid).unwrap().rid((*rid).into()).write(
                pt,
                now,
                rtp_time,
                vec![frame as u8; frame_size(rid)],
            )?;
        }
        frame += 1;

        let frame_end = now + Duration::from_millis(33);
        while l.last < frame_end {
            progress(l, r)?;
        }
    }
    Ok(())
}

/// Asserts each of `rids` is bound on the receiver, and that the `MediaData` from
/// `events_from` onwards covers each of them and carries no other rid, or none.
fn assert_rids_delivered(
    r: &mut TestRtc,
    mid: Mid,
    rids: &[&str],
    events_from: usize,
    context: &str,
) {
    let rids: Vec<Rid> = rids.iter().map(|rid| (*rid).into()).collect();
    let unmapped: Vec<Rid> = rids
        .iter()
        .copied()
        .filter(|rid| r.direct_api().stream_rx_by_mid(mid, Some(*rid)).is_none())
        .collect();

    let received: Vec<Option<Rid>> = r.events[events_from..]
        .iter()
        .filter_map(|(_, e)| match e {
            Event::MediaData(d) => Some(d.rid),
            _ => None,
        })
        .collect();
    let without_rid = received.iter().filter(|r| r.is_none()).count();
    let unexpected: Vec<Rid> = received
        .iter()
        .flatten()
        .copied()
        .filter(|rid| !rids.contains(rid))
        .collect();
    let per_rid: Vec<(Rid, usize)> = rids
        .iter()
        .map(|rid| (*rid, received.iter().filter(|r| **r == Some(*rid)).count()))
        .collect();

    let context = format!("{context}, MediaData per rid: {per_rid:?}");
    assert!(unmapped.is_empty(), "Unmapped rids {unmapped:?}; {context}");
    assert_eq!(without_rid, 0, "MediaData without rid; {context}");
    assert!(
        unexpected.is_empty(),
        "MediaData for unexpected rids {unexpected:?}; {context}"
    );
    assert!(
        per_rid.iter().all(|(_, n)| *n > 0),
        "Rid with no MediaData; {context}"
    );
}

/// Asserts the m-section for `mid` in `sdp` declares exactly `rids` as send rids, and
/// no simulcast at all when `rids` is empty.
fn assert_declares_send_simulcast(sdp: &str, mid: Mid, rids: &[&str]) {
    let mid_line = format!("a=mid:{mid}");
    let section = sdp
        .split("m=")
        .find(|s| s.lines().any(|l| l == mid_line))
        .unwrap_or_else(|| panic!("No m-section for {mid} in:\n{sdp}"));

    let mut expected: Vec<String> = rids.iter().map(|rid| format!("a=rid:{rid} send")).collect();
    if !rids.is_empty() {
        expected.push(format!("a=simulcast:send {}", rids.join(";")));
    }

    let declared: Vec<&str> = section
        .lines()
        .filter(|l| l.starts_with("a=rid:") || l.starts_with("a=simulcast:"))
        .collect();

    assert_eq!(
        declared, expected,
        "Unexpected simulcast declaration for {mid} in:\nm={section}"
    );
}

/// Removes the rids not in `accepted` from the receiver's `sdp`, and the simulcast
/// declaration when none is accepted, as a receiver narrowing or rejecting the offered
/// layers would describe them in each of its offers and answers.
fn restrict_sdp(sdp: &str, accepted: &[&str]) -> String {
    let restricted: Vec<String> = sdp
        .split("\r\n")
        .filter_map(|line| {
            if let Some(rest) = line.strip_prefix("a=rid:") {
                let rid = rest.split(' ').next().unwrap_or_default();
                accepted.contains(&rid).then(|| line.to_string())
            } else if line.starts_with("a=simulcast:") {
                (!accepted.is_empty()).then(|| format!("a=simulcast:recv {}", accepted.join(";")))
            } else {
                Some(line.to_string())
            }
        })
        .collect();

    restricted.join("\r\n")
}

fn all_rids() -> Vec<&'static str> {
    LAYERS.iter().map(|(rid, _)| *rid).collect()
}

fn frame_size(rid: &str) -> usize {
    LAYERS
        .iter()
        .find(|(layer, _)| *layer == rid)
        .map(|(_, size)| *size)
        .expect("layer for rid")
}

fn vp8_only(config: RtcConfig, rtx: bool) -> RtcConfig {
    let mut config = config.clear_codecs();
    config.codec_config().add_config(
        96.into(),
        rtx.then(|| 97.into()),
        Codec::Vp8,
        Frequency::NINETY_KHZ,
        None,
        Default::default(),
    );
    config
}
