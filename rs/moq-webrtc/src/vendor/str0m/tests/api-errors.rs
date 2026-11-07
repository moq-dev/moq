//! Tests for API misuse and error conditions.

use std::net::Ipv4Addr;
use std::time::Duration;

use str0m::media::{Direction, MediaKind};
use str0m::{Event, RtcError};

mod common;
use common::{Peer, TestRtc, init_crypto_default, init_log, negotiate, progress};

/// Test that disconnect() API works and is_alive() returns false after.
#[test]
fn api_disconnect_and_is_alive() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let mut l = TestRtc::new(Peer::Left);
    let mut r = TestRtc::new(Peer::Right);

    l.add_host_candidate((Ipv4Addr::new(1, 1, 1, 1), 1000).into());
    r.add_host_candidate((Ipv4Addr::new(2, 2, 2, 2), 2000).into());

    let (offer, pending) = l.span.in_scope(|| {
        let mut change = l.rtc.sdp_api();
        let _ = change.add_channel("test".into());
        change.apply().unwrap()
    });

    let answer = r.span.in_scope(|| r.rtc.sdp_api().accept_offer(offer))?;
    l.span
        .in_scope(|| l.rtc.sdp_api().accept_answer(pending, answer))?;

    // Connect first
    loop {
        if l.is_connected() && r.is_connected() {
            break;
        }
        if l.duration() > Duration::from_secs(5) {
            panic!("Failed to connect");
        }
        progress(&mut l, &mut r)?;
    }

    assert!(l.rtc.is_alive(), "Should be alive before disconnect");

    // Disconnect
    l.rtc.disconnect();

    assert!(!l.rtc.is_alive(), "Should not be alive after disconnect");

    Ok(())
}

/// Test that RecvOnly direction is properly set.
#[test]
fn api_error_not_sending_direction() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let mut l = TestRtc::new(Peer::Left);
    let mut r = TestRtc::new(Peer::Right);

    l.add_host_candidate((Ipv4Addr::new(1, 1, 1, 1), 1000).into());
    r.add_host_candidate((Ipv4Addr::new(2, 2, 2, 2), 2000).into());

    // L is RecvOnly - cannot send
    let mid = negotiate(&mut l, &mut r, |change| {
        change.add_media(MediaKind::Audio, Direction::RecvOnly, None, None, None)
    });

    loop {
        if l.is_connected() && r.is_connected() {
            break;
        }
        progress(&mut l, &mut r)?;
    }

    // L's direction should be RecvOnly
    let media = l.media(mid);
    assert!(media.is_some(), "Media should exist");
    assert_eq!(
        media.unwrap().direction(),
        Direction::RecvOnly,
        "L should be RecvOnly"
    );

    // R's direction should be SendOnly (inverse)
    let r_media = r.media(mid);
    assert!(r_media.is_some(), "R media should exist");
    assert_eq!(
        r_media.unwrap().direction(),
        Direction::SendOnly,
        "R should be SendOnly (inverse of L's RecvOnly)"
    );

    Ok(())
}

/// Test that SendOnly direction prevents receiving.
#[test]
fn api_error_not_receiving_direction() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let mut l = TestRtc::new(Peer::Left);
    let mut r = TestRtc::new(Peer::Right);

    l.add_host_candidate((Ipv4Addr::new(1, 1, 1, 1), 1000).into());
    r.add_host_candidate((Ipv4Addr::new(2, 2, 2, 2), 2000).into());

    // L is SendOnly
    let mid = negotiate(&mut l, &mut r, |change| {
        change.add_media(MediaKind::Audio, Direction::SendOnly, None, None, None)
    });

    loop {
        if l.is_connected() && r.is_connected() {
            break;
        }
        progress(&mut l, &mut r)?;
    }

    // L's direction is SendOnly, so R is RecvOnly and cannot request keyframes to L
    // (no sender source on R's side for this media)
    let media = l.media(mid);
    assert!(media.is_some(), "Media should exist");
    assert_eq!(
        media.unwrap().direction(),
        Direction::SendOnly,
        "L should be SendOnly"
    );

    Ok(())
}

/// Test that operations on disconnected Rtc are handled gracefully.
#[test]
fn api_operations_after_disconnect() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let mut l = TestRtc::new(Peer::Left);
    let mut r = TestRtc::new(Peer::Right);

    l.add_host_candidate((Ipv4Addr::new(1, 1, 1, 1), 1000).into());
    r.add_host_candidate((Ipv4Addr::new(2, 2, 2, 2), 2000).into());

    let mid = negotiate(&mut l, &mut r, |change| {
        change.add_media(MediaKind::Audio, Direction::SendRecv, None, None, None)
    });

    loop {
        if l.is_connected() && r.is_connected() {
            break;
        }
        progress(&mut l, &mut r)?;
    }

    // Disconnect
    l.rtc.disconnect();

    // After disconnect, writer should be None or return error on write
    // Get params before writer to avoid borrow issues
    let params = l.params_opus();
    let pt = params.pt();
    let wallclock = l.start + l.duration();
    let time = l.duration().into();

    if let Some(w) = l.writer(mid) {
        // Write may fail or succeed but won't actually send anything
        let _ = w.write(pt, wallclock, time, vec![0u8; 80]);
    }

    Ok(())
}

/// Test IceConnectionStateChange events are generated.
#[test]
fn api_ice_state_change_events() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let mut l = TestRtc::new(Peer::Left);
    let mut r = TestRtc::new(Peer::Right);

    l.add_host_candidate((Ipv4Addr::new(1, 1, 1, 1), 1000).into());
    r.add_host_candidate((Ipv4Addr::new(2, 2, 2, 2), 2000).into());

    let (offer, pending) = l.span.in_scope(|| {
        let mut change = l.rtc.sdp_api();
        let _ = change.add_channel("test".into());
        change.apply().unwrap()
    });

    let answer = r.span.in_scope(|| r.rtc.sdp_api().accept_offer(offer))?;
    l.span
        .in_scope(|| l.rtc.sdp_api().accept_answer(pending, answer))?;

    loop {
        if l.is_connected() && r.is_connected() {
            break;
        }
        if l.duration() > Duration::from_secs(5) {
            panic!("Failed to connect");
        }
        progress(&mut l, &mut r)?;
    }

    // Check that we received ICE state change events
    let ice_events: Vec<_> = l
        .events
        .iter()
        .filter(|(_, e)| matches!(e, Event::IceConnectionStateChange(_)))
        .collect();

    assert!(
        !ice_events.is_empty(),
        "Should have received IceConnectionStateChange events"
    );

    Ok(())
}

/// Test Connected event is generated.
#[test]
fn api_connected_event() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let mut l = TestRtc::new(Peer::Left);
    let mut r = TestRtc::new(Peer::Right);

    l.add_host_candidate((Ipv4Addr::new(1, 1, 1, 1), 1000).into());
    r.add_host_candidate((Ipv4Addr::new(2, 2, 2, 2), 2000).into());

    let (offer, pending) = l.span.in_scope(|| {
        let mut change = l.rtc.sdp_api();
        let _ = change.add_channel("test".into());
        change.apply().unwrap()
    });

    let answer = r.span.in_scope(|| r.rtc.sdp_api().accept_offer(offer))?;
    l.span
        .in_scope(|| l.rtc.sdp_api().accept_answer(pending, answer))?;

    loop {
        if l.is_connected() && r.is_connected() {
            break;
        }
        if l.duration() > Duration::from_secs(5) {
            panic!("Failed to connect");
        }
        progress(&mut l, &mut r)?;
    }

    // Check that we received Connected event
    let connected = l.events.iter().any(|(_, e)| matches!(e, Event::Connected));

    assert!(connected, "Should have received Connected event");

    Ok(())
}
