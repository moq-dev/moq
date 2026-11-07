use std::net::Ipv4Addr;
use std::time::Instant;

use str0m::bwe::{Bitrate, BweKind};
use str0m::media::{Direction, KeyframeRequestKind, MediaKind, Mid};
use str0m::net::Receive;
use str0m::{Event, Input, Output, RtcError};

mod common;
use common::{Peer, PendingPacket, TestRtc, init_crypto_default, init_log, negotiate, progress};

/// L sends video to a receive-only R whose stream has NACK suppressed.
fn setup() -> Result<(TestRtc, TestRtc, Mid, Instant), RtcError> {
    init_log();
    init_crypto_default();

    let mut l = TestRtc::new(Peer::Left);
    let mut r = TestRtc::new(Peer::Right);

    l.add_host_candidate((Ipv4Addr::new(1, 1, 1, 1), 1000).into());
    r.add_host_candidate((Ipv4Addr::new(2, 2, 2, 2), 2000).into());

    let mid = negotiate(&mut l, &mut r, |change| {
        change.add_media(MediaKind::Video, Direction::SendOnly, None, None, None)
    });

    while !l.is_connected() || !r.is_connected() {
        progress(&mut l, &mut r)?;
    }

    // Without NACK, no 33ms NACK timer wakes the session.
    r.direct_api()
        .stream_rx_by_mid(mid, None)
        .expect("stream rx")
        .suppress_nack(true);

    let pt = l.params_vp8().pt();
    for i in 0..20 {
        let wallclock = l.start + l.duration();
        let time = l.duration().into();
        l.writer(mid)
            .unwrap()
            .write(pt, wallclock, time, vec![0x10, 0x00, 0x00, i as u8])?;
        progress(&mut l, &mut r)?;
    }

    let now = l.last.max(r.last);
    drain(&mut l, now)?;
    drain(&mut r, now)?;

    Ok((l, r, mid, now))
}

/// Polls without handling any input. Returns transmits and the next timeout.
fn poll(rtc: &mut TestRtc) -> Result<(Vec<PendingPacket>, Instant), RtcError> {
    let mut packets = Vec::new();

    loop {
        match rtc.rtc.poll_output()? {
            Output::Timeout(t) => return Ok((packets, t)),
            Output::Transmit(t) => packets.push(PendingPacket {
                proto: t.proto,
                source: t.source,
                destination: t.destination,
                contents: t.contents.to_vec(),
            }),
            Output::Event(e) => rtc.events.push((rtc.last, e)),
        }
    }
}

fn drain(rtc: &mut TestRtc, now: Instant) -> Result<(Vec<PendingPacket>, Instant), RtcError> {
    rtc.rtc.handle_input(Input::Timeout(now))?;
    poll(rtc)
}

fn deliver(rtc: &mut TestRtc, now: Instant, packets: Vec<PendingPacket>) -> Result<(), RtcError> {
    for p in packets {
        let input = Input::Receive(
            now,
            Receive {
                proto: p.proto,
                source: p.source,
                destination: p.destination,
                contents: (&p.contents[..]).try_into()?,
            },
        );
        rtc.rtc.handle_input(input)?;
        poll(rtc)?;
    }
    Ok(())
}

/// The request must go out on the next tick without time advancing, after which the
/// timeout is no longer immediate.
fn assert_sent_immediately(
    request: impl FnOnce(&mut TestRtc, Mid),
    received: impl Fn(&Event) -> bool,
) -> Result<(), RtcError> {
    let (mut l, mut r, mid, now) = setup()?;

    request(&mut r, mid);
    assert_eq!(poll(&mut r)?.1, now);

    let (packets, next) = drain(&mut r, now)?;
    assert!(next > now);

    deliver(&mut l, now, packets)?;
    assert!(l.events.iter().any(|(_, e)| received(e)));

    Ok(())
}

#[test]
fn keyframe_request_writer_is_immediate() -> Result<(), RtcError> {
    assert_sent_immediately(
        |r, mid| {
            r.writer(mid)
                .unwrap()
                .request_keyframe(None, KeyframeRequestKind::Pli)
                .unwrap()
        },
        |e| matches!(e, Event::KeyframeRequest(r) if r.kind == KeyframeRequestKind::Pli),
    )
}

#[test]
fn keyframe_request_direct_api_is_immediate() -> Result<(), RtcError> {
    assert_sent_immediately(
        |r, mid| {
            r.direct_api()
                .stream_rx_by_mid(mid, None)
                .unwrap()
                .request_keyframe(KeyframeRequestKind::Fir)
        },
        |e| matches!(e, Event::KeyframeRequest(r) if r.kind == KeyframeRequestKind::Fir),
    )
}

#[test]
fn remb_request_is_immediate() -> Result<(), RtcError> {
    assert_sent_immediately(
        |r, mid| {
            r.direct_api()
                .stream_rx_by_mid(mid, None)
                .unwrap()
                .request_remb(Bitrate::bps(123_456))
        },
        |e| matches!(e, Event::EgressBitrateEstimate(BweKind::Remb { .. })),
    )
}
