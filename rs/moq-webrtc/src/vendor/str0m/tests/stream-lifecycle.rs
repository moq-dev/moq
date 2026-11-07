//! Tests for stream pause/resume and lifecycle events.

use std::net::Ipv4Addr;
use std::time::Duration;
use std::time::Instant;

use str0m::format::Codec;
use str0m::media::{Direction, MediaKind};
use str0m::rtp::RtpWrite;
use str0m::{Event, Rtc, RtcError};

mod common;
use common::{Peer, TestRtc, init_crypto_default, init_log, negotiate, progress};

/// Test that StreamPaused event is emitted after no packets for ~1.5 seconds.
#[test]
fn stream_pause_detection_timeout() -> Result<(), RtcError> {
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

    let max = l.last.max(r.last);
    l.last = max;
    r.last = max;

    let params = l.params_opus();
    assert_eq!(params.spec().codec, Codec::Opus);
    let pt = params.pt();
    let data = vec![1_u8; 80];

    // Send packets for 500ms
    let send_until = l.duration() + Duration::from_millis(500);
    loop {
        if l.duration() >= send_until {
            break;
        }
        let wallclock = l.start + l.duration();
        let time = l.duration().into();
        l.writer(mid)
            .unwrap()
            .write(pt, wallclock, time, data.clone())?;
        progress(&mut l, &mut r)?;
    }

    // Stop sending and wait for pause detection (>1.5 seconds)
    let pause_wait = l.duration() + Duration::from_secs(3);
    loop {
        if l.duration() >= pause_wait {
            break;
        }
        progress(&mut l, &mut r)?;
    }

    // Check for StreamPaused event on receiver
    let paused_events: Vec<_> = r
        .events
        .iter()
        .filter_map(|(_, e)| {
            if let Event::StreamPaused(p) = e {
                Some(p)
            } else {
                None
            }
        })
        .collect();

    assert!(
        paused_events.iter().any(|p| p.paused && p.mid == mid),
        "Expected StreamPaused event with paused=true for mid {:?}",
        mid
    );

    Ok(())
}

/// Test pause detection followed by resume when packets arrive again.
#[test]
fn stream_pause_resume_cycle() -> Result<(), RtcError> {
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

    let max = l.last.max(r.last);
    l.last = max;
    r.last = max;

    let params = l.params_opus();
    let pt = params.pt();
    let data = vec![1_u8; 80];

    // Phase 1: Send packets
    let send_until = l.duration() + Duration::from_millis(500);
    loop {
        if l.duration() >= send_until {
            break;
        }
        let wallclock = l.start + l.duration();
        let time = l.duration().into();
        l.writer(mid)
            .unwrap()
            .write(pt, wallclock, time, data.clone())?;
        progress(&mut l, &mut r)?;
    }

    // Phase 2: Stop sending and wait for pause
    let pause_wait = l.duration() + Duration::from_secs(2);
    loop {
        if l.duration() >= pause_wait {
            break;
        }
        progress(&mut l, &mut r)?;
    }

    // Phase 3: Resume sending
    let resume_until = l.duration() + Duration::from_millis(500);
    loop {
        if l.duration() >= resume_until {
            break;
        }
        let wallclock = l.start + l.duration();
        let time = l.duration().into();
        l.writer(mid)
            .unwrap()
            .write(pt, wallclock, time, data.clone())?;
        progress(&mut l, &mut r)?;
    }

    // Check for both pause and resume events
    let paused_states: Vec<_> = r
        .events
        .iter()
        .filter_map(|(_, e)| {
            if let Event::StreamPaused(p) = e {
                if p.mid == mid { Some(p.paused) } else { None }
            } else {
                None
            }
        })
        .collect();

    assert!(
        paused_states.contains(&true),
        "Expected StreamPaused event with paused=true"
    );
    assert!(
        paused_states.contains(&false),
        "Expected StreamPaused event with paused=false (resume)"
    );

    Ok(())
}

#[test]
fn pause_preserves_video_contiguity_history() -> Result<(), RtcError> {
    init_crypto_default();

    let mut l = TestRtc::new(Peer::Left);
    let mut r = TestRtc::new_with_config(Peer::Right, |config| {
        config
            .set_pause_threshold(Duration::from_millis(500))
            .set_reordering_size_video(1)
    });
    assert_eq!(
        Rtc::builder().pause_threshold(),
        Duration::from_millis(1500)
    );

    l.add_host_candidate((Ipv4Addr::new(1, 1, 1, 1), 1000).into());
    r.add_host_candidate((Ipv4Addr::new(2, 2, 2, 2), 2000).into());
    let mid = negotiate(&mut l, &mut r, |change| {
        change.add_media(MediaKind::Video, Direction::SendOnly, None, None, None)
    });
    while !l.is_connected() || !r.is_connected() {
        progress(&mut l, &mut r)?;
    }
    let max = l.last.max(r.last);
    l.last = max;
    r.last = max;
    let pt = l.params_vp8().pt();

    // A one-packet frame establishes the sequence and codec history.
    send_vp8(&mut l, &mut r, mid, pt, 10, 90_000)?;
    assert_frame_contiguity(&r, 10, true);

    // An idle interval longer than the configured threshold is a pause, but
    // the next packet can still be the immediate successor.
    advance_both(&mut l, &mut r, Duration::from_millis(200))?;
    assert!(
        !r.events.iter().any(|(_, event)| {
            matches!(event, Event::StreamPaused(p) if p.paused && p.mid == mid)
        })
    );
    advance_both(&mut l, &mut r, Duration::from_millis(450))?;
    assert!(
        r.events.iter().any(|(_, event)| {
            matches!(event, Event::StreamPaused(p) if p.paused && p.mid == mid)
        })
    );
    send_vp8(&mut l, &mut r, mid, pt, 11, 180_000)?;
    assert_frame_contiguity(&r, 11, true);

    // After another pause, packet 12 is lost. The first resumed delta frame
    // must report the gap even though the stream was reported paused.
    advance_both(&mut l, &mut r, Duration::from_millis(650))?;
    send_vp8(&mut l, &mut r, mid, pt, 13, 360_000)?;
    assert_frame_contiguity(&r, 13, false);

    Ok(())
}

#[test]
fn sparse_audio_resumes_after_packet_loss() -> Result<(), RtcError> {
    init_crypto_default();

    let mut l = TestRtc::new(Peer::Left);
    let mut r = TestRtc::new(Peer::Right);
    l.add_host_candidate((Ipv4Addr::new(1, 1, 1, 1), 1000).into());
    r.add_host_candidate((Ipv4Addr::new(2, 2, 2, 2), 2000).into());
    let mid = negotiate(&mut l, &mut r, |change| {
        change.add_media(MediaKind::Audio, Direction::SendOnly, None, None, None)
    });
    while !l.is_connected() || !r.is_connected() {
        progress(&mut l, &mut r)?;
    }
    let max = l.last.max(r.last);
    l.last = max;
    r.last = max;
    let pt = l.params_opus().pt();

    let send = |l: &mut TestRtc, r: &mut TestRtc, seq: u64| -> Result<(), RtcError> {
        let at = l.last.max(r.last) + Duration::from_millis(10);
        l.direct_api()
            .stream_tx_by_mid(mid, None)
            .unwrap()
            .write_rtp(RtpWrite::new(
                pt,
                seq.into(),
                seq as u32 * 960,
                at,
                [0xf8, 0xff, 0xfe],
            ));
        advance_until(l, r, at + Duration::from_millis(100))
    };

    send(&mut l, &mut r, 10)?;
    assert_frame_contiguity(&r, 10, true);
    advance_both(&mut l, &mut r, Duration::from_secs(2))?;
    assert!(
        r.events.iter().any(|(_, event)| {
            matches!(event, Event::StreamPaused(p) if p.paused && p.mid == mid)
        })
    );

    // Sequence 11 is lost. A sparse sender may send only one packet on resume.
    send(&mut l, &mut r, 12)?;
    advance_both(&mut l, &mut r, Duration::from_millis(1200))?;
    assert_frame_contiguity(&r, 12, false);
    Ok(())
}

fn send_vp8(
    l: &mut TestRtc,
    r: &mut TestRtc,
    mid: str0m::media::Mid,
    pt: str0m::media::Pt,
    seq: u64,
    timestamp: u32,
) -> Result<(), RtcError> {
    let at = l.last.max(r.last) + Duration::from_millis(10);
    l.direct_api()
        .stream_tx_by_mid(mid, None)
        .unwrap()
        .write_rtp(RtpWrite::new(pt, seq.into(), timestamp, at, [0x10, 0x01, 0xaa]).marker(true));
    advance_until(l, r, at + Duration::from_millis(200))
}

fn advance_both(l: &mut TestRtc, r: &mut TestRtc, duration: Duration) -> Result<(), RtcError> {
    let until = l.last.max(r.last) + duration;
    advance_until(l, r, until)
}

fn advance_until(l: &mut TestRtc, r: &mut TestRtc, until: Instant) -> Result<(), RtcError> {
    while l.last < until || r.last < until {
        progress(l, r)?;
    }
    Ok(())
}

fn assert_frame_contiguity(r: &TestRtc, seq: u64, expected: bool) {
    let frame = r.events.iter().find_map(|(_, event)| match event {
        Event::MediaData(frame) if frame.seq_range.contains(&seq.into()) => Some(frame),
        _ => None,
    });
    assert_eq!(
        frame.map(|frame| frame.contiguous),
        Some(expected),
        "frame {seq}"
    );
}

/// Test changing media direction to SendOnly.
#[test]
fn stream_direction_change_sendonly() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let mut l = TestRtc::new(Peer::Left);
    let mut r = TestRtc::new(Peer::Right);

    l.add_host_candidate((Ipv4Addr::new(1, 1, 1, 1), 1000).into());
    r.add_host_candidate((Ipv4Addr::new(2, 2, 2, 2), 2000).into());

    // Initial negotiation with SendRecv
    let mid = negotiate(&mut l, &mut r, |change| {
        change.add_media(MediaKind::Audio, Direction::SendRecv, None, None, None)
    });

    loop {
        if l.is_connected() && r.is_connected() {
            break;
        }
        progress(&mut l, &mut r)?;
    }

    assert_eq!(l.media(mid).unwrap().direction(), Direction::SendRecv);
    assert_eq!(r.media(mid).unwrap().direction(), Direction::SendRecv);

    // Change L to SendOnly
    negotiate(&mut l, &mut r, |change| {
        change.set_direction(mid, Direction::SendOnly);
    });

    assert_eq!(
        l.media(mid).unwrap().direction(),
        Direction::SendOnly,
        "L should be SendOnly"
    );
    assert_eq!(
        r.media(mid).unwrap().direction(),
        Direction::RecvOnly,
        "R should be RecvOnly (opposite of L's SendOnly)"
    );

    Ok(())
}

/// Test changing media direction to RecvOnly.
#[test]
fn stream_direction_change_recvonly() -> Result<(), RtcError> {
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

    assert_eq!(l.media(mid).unwrap().direction(), Direction::SendRecv);
    assert_eq!(r.media(mid).unwrap().direction(), Direction::SendRecv);

    // Change L to RecvOnly
    negotiate(&mut l, &mut r, |change| {
        change.set_direction(mid, Direction::RecvOnly);
    });

    assert_eq!(
        l.media(mid).unwrap().direction(),
        Direction::RecvOnly,
        "L should be RecvOnly"
    );
    assert_eq!(
        r.media(mid).unwrap().direction(),
        Direction::SendOnly,
        "R should be SendOnly (opposite of L's RecvOnly)"
    );

    Ok(())
}

/// Test changing media direction to Inactive.
#[test]
fn stream_direction_change_inactive() -> Result<(), RtcError> {
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

    assert_eq!(l.media(mid).unwrap().direction(), Direction::SendRecv);
    assert_eq!(r.media(mid).unwrap().direction(), Direction::SendRecv);

    // Change L to Inactive
    negotiate(&mut l, &mut r, |change| {
        change.set_direction(mid, Direction::Inactive);
    });

    assert_eq!(
        l.media(mid).unwrap().direction(),
        Direction::Inactive,
        "L should be Inactive"
    );
    assert_eq!(
        r.media(mid).unwrap().direction(),
        Direction::Inactive,
        "R should be Inactive (both sides inactive)"
    );

    Ok(())
}

/// Test MediaChanged event is generated on direction change.
#[test]
fn stream_media_changed_event() -> Result<(), RtcError> {
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

    // Clear previous events
    l.events.clear();
    r.events.clear();

    // Change direction
    negotiate(&mut l, &mut r, |change| {
        change.set_direction(mid, Direction::SendOnly);
    });

    // Progress to process events
    for _ in 0..20 {
        progress(&mut l, &mut r)?;
    }

    // Check for MediaChanged event
    let changed_events: Vec<_> = r
        .events
        .iter()
        .filter_map(|(_, e)| {
            if let Event::MediaChanged(c) = e {
                Some(c)
            } else {
                None
            }
        })
        .collect();

    assert!(
        changed_events.iter().any(|c| c.mid == mid),
        "Expected MediaChanged event for mid {:?}",
        mid
    );

    Ok(())
}
