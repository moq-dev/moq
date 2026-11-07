//! Tests for PLI/FIR keyframe request handling.

use std::net::Ipv4Addr;

use str0m::media::{Direction, KeyframeRequestKind, MediaKind};
use str0m::{Event, RtcError};

mod common;
use common::{Peer, TestRtc, init_crypto_default, init_log, negotiate, progress};

/// Test PLI (Picture Loss Indication) request is sent and received.
#[test]
fn keyframe_request_pli() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let mut l = TestRtc::new(Peer::Left);
    let mut r = TestRtc::new(Peer::Right);

    l.add_host_candidate((Ipv4Addr::new(1, 1, 1, 1), 1000).into());
    r.add_host_candidate((Ipv4Addr::new(2, 2, 2, 2), 2000).into());

    // L sends video to R
    let mid = negotiate(&mut l, &mut r, |change| {
        change.add_media(MediaKind::Video, Direction::SendRecv, None, None, None)
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

    // Send video from L
    let params = l.params_vp8();
    let pt = params.pt();

    for i in 0..20 {
        let wallclock = l.start + l.duration();
        let time = l.duration().into();
        l.writer(mid)
            .unwrap()
            .write(pt, wallclock, time, vec![0x10, 0x00, 0x00, i as u8])?;
        progress(&mut l, &mut r)?;
    }

    // R requests a keyframe via PLI
    if let Some(mut writer) = r.writer(mid) {
        if writer.is_request_keyframe_possible(KeyframeRequestKind::Pli) {
            writer.request_keyframe(None, KeyframeRequestKind::Pli)?;
        }
    }

    // Progress until L receives the keyframe request
    let mut found_keyframe_request = false;
    for _ in 0..100 {
        progress(&mut l, &mut r)?;

        for (_, event) in &l.events {
            if let Event::KeyframeRequest(req) = event {
                if req.mid == mid && req.kind == KeyframeRequestKind::Pli {
                    found_keyframe_request = true;
                    break;
                }
            }
        }

        if found_keyframe_request {
            break;
        }
    }

    assert!(
        found_keyframe_request,
        "L should receive a PLI keyframe request from R"
    );

    Ok(())
}

/// Test FIR (Full Intra Request) is sent and received.
#[test]
fn keyframe_request_fir() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let mut l = TestRtc::new(Peer::Left);
    let mut r = TestRtc::new(Peer::Right);

    l.add_host_candidate((Ipv4Addr::new(1, 1, 1, 1), 1000).into());
    r.add_host_candidate((Ipv4Addr::new(2, 2, 2, 2), 2000).into());

    let mid = negotiate(&mut l, &mut r, |change| {
        change.add_media(MediaKind::Video, Direction::SendRecv, None, None, None)
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

    // Send video from L
    let params = l.params_vp8();
    let pt = params.pt();

    for i in 0..20 {
        let wallclock = l.start + l.duration();
        let time = l.duration().into();
        l.writer(mid)
            .unwrap()
            .write(pt, wallclock, time, vec![0x10, 0x00, 0x00, i as u8])?;
        progress(&mut l, &mut r)?;
    }

    // R requests a keyframe via FIR
    if let Some(mut writer) = r.writer(mid) {
        if writer.is_request_keyframe_possible(KeyframeRequestKind::Fir) {
            writer.request_keyframe(None, KeyframeRequestKind::Fir)?;
        }
    }

    let mut found_keyframe_request = false;
    for _ in 0..100 {
        progress(&mut l, &mut r)?;

        for (_, event) in &l.events {
            if let Event::KeyframeRequest(req) = event {
                if req.mid == mid && req.kind == KeyframeRequestKind::Fir {
                    found_keyframe_request = true;
                    break;
                }
            }
        }

        if found_keyframe_request {
            break;
        }
    }

    assert!(
        found_keyframe_request,
        "L should receive a FIR keyframe request from R"
    );

    Ok(())
}

/// Test is_request_keyframe_possible() method.
#[test]
fn keyframe_request_possibility_check() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let mut l = TestRtc::new(Peer::Left);
    let mut r = TestRtc::new(Peer::Right);

    l.add_host_candidate((Ipv4Addr::new(1, 1, 1, 1), 1000).into());
    r.add_host_candidate((Ipv4Addr::new(2, 2, 2, 2), 2000).into());

    let mid = negotiate(&mut l, &mut r, |change| {
        change.add_media(MediaKind::Video, Direction::SendRecv, None, None, None)
    });

    loop {
        if l.is_connected() && r.is_connected() {
            break;
        }
        progress(&mut l, &mut r)?;
    }

    // Send some video to establish the stream
    let params = l.params_vp8();
    let pt = params.pt();

    for i in 0..5 {
        let wallclock = l.start + l.duration();
        let time = l.duration().into();
        l.writer(mid)
            .unwrap()
            .write(pt, wallclock, time, vec![0x10, 0x00, 0x00, i as u8])?;
        progress(&mut l, &mut r)?;
    }

    // Check that keyframe requests are possible
    if let Some(writer) = r.writer(mid) {
        let pli_possible = writer.is_request_keyframe_possible(KeyframeRequestKind::Pli);
        let fir_possible = writer.is_request_keyframe_possible(KeyframeRequestKind::Fir);

        // At least one should be possible with default config
        assert!(
            pli_possible || fir_possible,
            "At least PLI or FIR should be possible"
        );
    }

    Ok(())
}
