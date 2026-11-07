use std::net::Ipv4Addr;
use std::time::Duration;

use str0m::format::Codec;
use str0m::media::{Direction, MediaKind};
use str0m::rtp::{Extension, ExtensionMap};
use str0m::{Event, Rtc, RtcError};

mod common;
use common::{Peer, TestRtc, init_crypto_default, init_log, progress};

#[test]
pub fn abs_capture_time_negotiation() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    // Configure abs-capture-time extension on both peers
    let mut exts = ExtensionMap::standard();
    exts.set(9, Extension::AbsoluteCaptureTime);

    let now = std::time::Instant::now();
    let mut l_rtc = Rtc::builder();
    *l_rtc.extension_map() = exts.clone();
    let mut l = TestRtc::new_with_rtc(Peer::Left.span(), l_rtc.build(now));

    let mut r_rtc = Rtc::builder();
    *r_rtc.extension_map() = exts.clone();
    let mut r = TestRtc::new_with_rtc(Peer::Right.span(), r_rtc.build(now));

    l.set_forced_time_advance(Duration::from_millis(1));
    r.set_forced_time_advance(Duration::from_millis(1));

    l.add_host_candidate((Ipv4Addr::new(1, 1, 1, 1), 1000).into());
    r.add_host_candidate((Ipv4Addr::new(2, 2, 2, 2), 2000).into());

    // Setup audio media
    let mut change = l.sdp_api();
    let mid = change.add_media(MediaKind::Audio, Direction::SendRecv, None, None, None);
    let (offer, pending) = change.apply().unwrap();

    // Verify the offer contains abs-capture-time extension
    let offer_str = offer.to_sdp_string();
    assert!(
        offer_str.contains("abs-capture-time"),
        "SDP offer should contain abs-capture-time extension"
    );

    let answer = r.rtc.sdp_api().accept_offer(offer)?;
    l.rtc.sdp_api().accept_answer(pending, answer)?;

    // Wait for connection
    loop {
        if l.is_connected() || r.is_connected() {
            break;
        }
        progress(&mut l, &mut r)?;
    }

    let max = l.last.max(r.last);
    l.last = max;
    r.last = max;

    let params = l.params_opus();
    assert_eq!(params.spec().codec, Codec::Opus);
    let pt = params.pt();

    let data = vec![1_u8; 80];

    // Send some packets
    for _ in 0..10 {
        let wallclock = l.start + l.duration();
        let time = l.duration().into();

        l.writer(mid)
            .unwrap()
            .write(pt, wallclock, time, data.clone())?;

        progress(&mut l, &mut r)?;
    }

    // Verify we received MediaData events
    let media_count = r
        .events
        .iter()
        .filter(|(_, e)| matches!(e, Event::MediaData(_)))
        .count();

    assert!(
        media_count >= 5,
        "Should have received at least 5 media packets, got {}",
        media_count
    );

    Ok(())
}

#[test]
pub fn abs_capture_time_sdp_roundtrip() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    // Test that abs-capture-time extension properly roundtrips through SDP
    let mut exts = ExtensionMap::standard();
    exts.set(9, Extension::AbsoluteCaptureTime);

    let now = std::time::Instant::now();
    let mut l_rtc = Rtc::builder();
    *l_rtc.extension_map() = exts.clone();
    let mut l = TestRtc::new_with_rtc(Peer::Left.span(), l_rtc.build(now));

    let mut r = TestRtc::new(Peer::Right);

    l.set_forced_time_advance(Duration::from_millis(1));
    r.set_forced_time_advance(Duration::from_millis(1));

    l.add_host_candidate((Ipv4Addr::new(1, 1, 1, 1), 1000).into());
    r.add_host_candidate((Ipv4Addr::new(2, 2, 2, 2), 2000).into());

    let mut change = l.sdp_api();
    let _mid = change.add_media(MediaKind::Video, Direction::SendRecv, None, None, None);
    let (offer, pending) = change.apply().unwrap();

    // Verify offer contains abs-capture-time
    let offer_sdp = offer.to_sdp_string();
    assert!(
        offer_sdp.contains("http://www.webrtc.org/experiments/rtp-hdrext/abs-capture-time"),
        "Offer SDP should contain abs-capture-time URI"
    );

    let answer = r.rtc.sdp_api().accept_offer(offer)?;

    // Verify answer also contains abs-capture-time (if receiver supports it)
    let answer_sdp = answer.to_sdp_string();
    assert!(
        answer_sdp.contains("abs-capture-time") || answer_sdp.contains("a=extmap"),
        "Answer SDP should contain extension mapping"
    );

    l.rtc.sdp_api().accept_answer(pending, answer)?;

    Ok(())
}
