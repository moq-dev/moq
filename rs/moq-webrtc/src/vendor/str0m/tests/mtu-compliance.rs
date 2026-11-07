use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use str0m::format::Codec;
use str0m::media::{Direction, MediaKind};
use str0m::{DATAGRAM_MTU_TARGET_MAX, DATAGRAM_MTU_TARGET_MIN, Event, Rtc, RtcError};

mod common;
use common::{Peer, TestRtc, init_crypto_default, init_log, progress_strict_mtu};

/// End-to-end MTU compliance: walk a full ICE/DTLS/SCTP/SRTP session and
/// assert every outgoing datagram on either side has `len() <= mtu`. The
/// check fires inside the test pump via [`progress_strict_mtu`], so any
/// oversized datagram fails the test at the source.
fn run_mtu_compliance(mtu: usize) -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let now = Instant::now();
    let l_rtc = Rtc::builder().set_mtu(mtu..=mtu).build(now);
    let r_rtc = Rtc::builder().set_mtu(mtu..=mtu).build(now);

    let mut l = TestRtc::new_with_rtc(Peer::Left.span(), l_rtc);
    let mut r = TestRtc::new_with_rtc(Peer::Right.span(), r_rtc);

    l.add_host_candidate((Ipv4Addr::new(1, 1, 1, 1), 1000).into());
    r.add_host_candidate((Ipv4Addr::new(2, 2, 2, 2), 2000).into());

    // Add an audio m-line and a data channel in the same offer.
    let mut change = l.sdp_api();
    let audio_mid = change.add_media(MediaKind::Audio, Direction::SendRecv, None, None, None);
    let cid = change.add_channel("mtu-test".into());
    let (offer, pending) = change.apply().unwrap();
    let answer = r.rtc.sdp_api().accept_offer(offer)?;
    l.rtc.sdp_api().accept_answer(pending, answer)?;

    // ICE + DTLS handshake under strict MTU.
    loop {
        if l.is_connected() && r.is_connected() {
            break;
        }
        progress_strict_mtu(&mut l, &mut r, mtu)?;
    }

    let max = l.last.max(r.last);
    l.last = max;
    r.last = max;

    // Wait for the data channel to open on L.
    while l.channel(cid).is_none() {
        progress_strict_mtu(&mut l, &mut r, mtu)?;
    }

    // Small data-channel send.
    l.channel(cid)
        .unwrap()
        .write(false, b"hello mtu world")
        .expect("small write");

    // Large data-channel send to exercise SCTP fragmentation.
    let big = vec![0xCDu8; 32 * 1024];
    l.channel(cid)
        .unwrap()
        .write(true, &big)
        .expect("large write");

    // Audio writes (Opus) at typical 20 ms cadence.
    let params = l.params_opus();
    assert_eq!(params.spec().codec, Codec::Opus);
    let pt = params.pt();
    let audio_payload = vec![0xAAu8; 160];

    let deadline = Duration::from_secs(2);
    while l.duration() < deadline {
        let wallclock = l.start + l.duration();
        let time = l.duration().into();
        if let Some(w) = l.writer(audio_mid) {
            w.write(pt, wallclock, time, audio_payload.clone())?;
        }
        progress_strict_mtu(&mut l, &mut r, mtu)?;
    }

    // Sanity: r received the small + part of the large data-channel send.
    let chan_events = r
        .events
        .iter()
        .filter(|(_, e)| matches!(e, Event::ChannelData(_)))
        .count();
    assert!(
        chan_events >= 1,
        "expected at least one ChannelData event at r, got {chan_events}"
    );

    Ok(())
}

#[test]
fn mtu_compliance_min() -> Result<(), RtcError> {
    run_mtu_compliance(DATAGRAM_MTU_TARGET_MIN)
}

#[test]
fn mtu_compliance_mid() -> Result<(), RtcError> {
    run_mtu_compliance((DATAGRAM_MTU_TARGET_MIN + DATAGRAM_MTU_TARGET_MAX) / 2)
}

#[test]
fn mtu_compliance_max() -> Result<(), RtcError> {
    run_mtu_compliance(DATAGRAM_MTU_TARGET_MAX)
}
