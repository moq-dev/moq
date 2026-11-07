use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use str0m::format::Codec;
use str0m::media::MediaKind;
use str0m::net::Receive;
use str0m::rtp::{RtpWrite, Ssrc};
use str0m::{Candidate, Event, Input, Output, Rtc, RtcError};

mod common;
use common::{Peer, PendingPacket, TestRtc, init_crypto_default, init_log, progress};

fn connect_direct() -> Result<(TestRtc, TestRtc), RtcError> {
    let now = Instant::now();
    let mut l = TestRtc::new_with_rtc(Peer::Left.span(), Rtc::new(now));
    // These tests hold a fragment across long pauses before completing it.
    let mut r = TestRtc::new_with_rtc(
        Peer::Right.span(),
        Rtc::builder().set_reordering_size_video(10).build(now),
    );

    let host1 = Candidate::host((Ipv4Addr::new(1, 1, 1, 1), 1000).into(), "udp")?.clone();
    let host2 = Candidate::host((Ipv4Addr::new(2, 2, 2, 2), 2000).into(), "udp")?.clone();
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

    l.direct_api().start_dtls(true)?;
    r.direct_api().start_dtls(false)?;
    l.direct_api().start_sctp(true);
    r.direct_api().start_sctp(false);

    while !l.is_connected() || !r.is_connected() {
        progress(&mut l, &mut r)?;
    }

    Ok((l, r))
}

fn flush_transmits(rtc: &mut TestRtc, time: Instant) -> Result<Vec<PendingPacket>, RtcError> {
    let mut packets = Vec::new();

    rtc.rtc.handle_input(Input::Timeout(time))?;

    loop {
        match rtc.rtc.poll_output()? {
            Output::Timeout(v) => {
                let tick = rtc.last + rtc.forced_time_advance;
                rtc.last = if v == rtc.last { tick } else { tick.min(v) };
                break;
            }
            Output::Transmit(v) => {
                packets.push(PendingPacket {
                    proto: v.proto,
                    source: v.source,
                    destination: v.destination,
                    contents: v.contents.to_vec(),
                });
            }
            Output::Event(v) => rtc.events.push((time, v)),
        }
    }

    Ok(packets)
}

fn wait_for_transmits(
    rtc: &mut TestRtc,
    earliest: Instant,
    max_wait: Duration,
) -> Result<(Instant, Vec<PendingPacket>), RtcError> {
    let deadline = earliest + max_wait;

    loop {
        let time = rtc.last.max(earliest);
        let packets = flush_transmits(rtc, time)?;

        if !packets.is_empty() {
            return Ok((time, packets));
        }

        assert!(
            time < deadline,
            "expected at least one packet by {:?}, but sender emitted none",
            deadline.saturating_duration_since(earliest)
        );
    }
}

fn deliver_packet(rtc: &mut TestRtc, time: Instant, packet: PendingPacket) -> Result<(), RtcError> {
    let input = Input::Receive(
        time,
        Receive {
            proto: packet.proto,
            source: packet.source,
            destination: packet.destination,
            contents: (&packet.contents[..]).try_into()?,
        },
    );

    rtc.rtc.handle_input(input)?;

    loop {
        match rtc.rtc.poll_output()? {
            Output::Timeout(v) => {
                let tick = rtc.last + rtc.forced_time_advance;
                rtc.last = if v == rtc.last { tick } else { tick.min(v) };
                break;
            }
            Output::Transmit(_) => {}
            Output::Event(v) => rtc.events.push((time, v)),
        }
    }

    Ok(())
}

fn measure_fresh_age_after_pause(pause_duration: Duration) -> Result<Duration, RtcError> {
    let (mut l, mut r) = connect_direct()?;

    let mid = "vid".into();
    let ssrc: Ssrc = 42.into();

    l.direct_api().declare_media(mid, MediaKind::Video);
    l.direct_api().declare_stream_tx(ssrc, None, mid, None);

    r.direct_api().declare_media(mid, MediaKind::Video);
    r.direct_api().expect_stream_rx(ssrc, None, mid, None);

    let max = l.last.max(r.last);
    l.last = max;
    r.last = max;

    let pt = l.params_vp8().pt();
    assert_eq!(l.params_vp8().spec().codec, Codec::Vp8);

    let frame_timestamp = 90_000u32;
    let first_write_at = l.last + Duration::from_millis(20);

    {
        let mut direct = l.direct_api();
        let tx = direct.stream_tx_by_mid(mid, None).unwrap();
        tx.write_rtp(
            RtpWrite::new(
                pt,
                10_000u64.into(),
                frame_timestamp,
                first_write_at,
                [0x10, 0x00, 0xAA, 0xBB],
            )
            .nackable(true),
        );
    }
    let (first_emit_at, mut first_packets) =
        wait_for_transmits(&mut l, first_write_at, Duration::from_millis(50))?;
    assert_eq!(first_packets.len(), 1, "expected one first-fragment packet");
    deliver_packet(&mut r, first_emit_at, first_packets.remove(0))?;

    {
        let mut direct = l.direct_api();
        let tx = direct.stream_tx_by_mid(mid, None).unwrap();
        tx.write_rtp(
            RtpWrite::new(
                pt,
                10_001u64.into(),
                frame_timestamp,
                first_write_at + Duration::from_millis(5),
                [0x00, 0xCC, 0xDD],
            )
            .marker(true)
            .nackable(true),
        );
    }
    let (_, delayed_packets) = wait_for_transmits(
        &mut l,
        first_write_at + Duration::from_millis(5),
        Duration::from_millis(50),
    )?;
    assert!(
        !delayed_packets.is_empty(),
        "expected at least one packet when flushing delayed tail"
    );

    let pause_end = l.last + pause_duration;
    while l.last < pause_end || r.last < pause_end {
        progress(&mut l, &mut r)?;
    }

    assert!(
        r.events
            .iter()
            .any(|(_, e)| matches!(e, Event::StreamPaused(p) if p.paused)),
        "receiver never entered paused state"
    );

    let before_tail_event_count = r.events.len();
    let delivery_time = pause_end + Duration::from_millis(20);
    for packet in delayed_packets {
        deliver_packet(&mut r, delivery_time, packet)?;
    }

    let resumed = r.events[before_tail_event_count..]
        .iter()
        .find_map(|(_, event)| match event {
            Event::MediaData(frame) => Some(frame),
            _ => None,
        })
        .expect("expected the frame spanning the pause to complete");
    assert_eq!(*resumed.seq_range.start(), 10_000u64.into());
    assert_eq!(*resumed.seq_range.end(), 10_001u64.into());

    let fresh_first_at = delivery_time + Duration::from_millis(20);
    let fresh_frame_timestamp = frame_timestamp + 3_000;

    {
        let mut direct = l.direct_api();
        let tx = direct.stream_tx_by_mid(mid, None).unwrap();
        tx.write_rtp(
            RtpWrite::new(
                pt,
                10_002u64.into(),
                fresh_frame_timestamp,
                fresh_first_at,
                [0x10, 0x00, 0x11, 0x22],
            )
            .nackable(true),
        );
    }
    let (fresh_first_emit_at, mut fresh_first_packets) =
        wait_for_transmits(&mut l, fresh_first_at, Duration::from_millis(50))?;
    assert_eq!(
        fresh_first_packets.len(),
        1,
        "expected one fresh first-fragment packet"
    );
    deliver_packet(&mut r, fresh_first_emit_at, fresh_first_packets.remove(0))?;

    {
        let mut direct = l.direct_api();
        let tx = direct.stream_tx_by_mid(mid, None).unwrap();
        tx.write_rtp(
            RtpWrite::new(
                pt,
                10_003u64.into(),
                fresh_frame_timestamp,
                fresh_first_at + Duration::from_millis(5),
                [0x00, 0x33, 0x44],
            )
            .marker(true)
            .nackable(true),
        );
    }
    let (fresh_tail_emit_at, fresh_tail_packets) = wait_for_transmits(
        &mut l,
        fresh_first_at + Duration::from_millis(5),
        Duration::from_millis(50),
    )?;
    let before_fresh_event_count = r.events.len();
    for packet in fresh_tail_packets {
        deliver_packet(&mut r, fresh_tail_emit_at, packet)?;
    }

    let media = r.events[before_fresh_event_count..]
        .iter()
        .find_map(|(event_time, e)| match e {
            Event::MediaData(data) => Some((*event_time, data)),
            _ => None,
        })
        .expect("expected MediaData from the fresh post-pause frame");

    let age = media.0.saturating_duration_since(media.1.network_time);

    assert_eq!(*media.1.seq_range.start(), 10_002u64.into());
    assert_eq!(*media.1.seq_range.end(), 10_003u64.into());

    Ok(age)
}

fn collect_fresh_ages_after_repeated_pauses(
    pause_durations: &[Duration],
) -> Result<Vec<Duration>, RtcError> {
    let (mut l, mut r) = connect_direct()?;

    let mid = "vid".into();
    let ssrc: Ssrc = 42.into();

    l.direct_api().declare_media(mid, MediaKind::Video);
    l.direct_api().declare_stream_tx(ssrc, None, mid, None);

    r.direct_api().declare_media(mid, MediaKind::Video);
    r.direct_api().expect_stream_rx(ssrc, None, mid, None);

    let max = l.last.max(r.last);
    l.last = max;
    r.last = max;

    let pt = l.params_vp8().pt();
    assert_eq!(l.params_vp8().spec().codec, Codec::Vp8);

    let mut fresh_ages = Vec::new();

    for (i, pause_duration) in pause_durations.iter().copied().enumerate() {
        let seq = 20_000u64 + (i as u64) * 4;
        let frame_timestamp = 90_000u32 + (i as u32) * 3_000;
        let first_write_at = l.last + Duration::from_millis(20);

        {
            let mut direct = l.direct_api();
            let tx = direct.stream_tx_by_mid(mid, None).unwrap();
            tx.write_rtp(
                RtpWrite::new(
                    pt,
                    seq.into(),
                    frame_timestamp,
                    first_write_at,
                    [0x10, 0x00, 0xAA, 0xBB, i as u8],
                )
                .nackable(true),
            );
        }
        let (first_emit_at, mut first_packets) =
            wait_for_transmits(&mut l, first_write_at, Duration::from_millis(50))?;
        assert_eq!(first_packets.len(), 1, "expected one first-fragment packet");
        deliver_packet(&mut r, first_emit_at, first_packets.remove(0))?;

        {
            let mut direct = l.direct_api();
            let tx = direct.stream_tx_by_mid(mid, None).unwrap();
            tx.write_rtp(
                RtpWrite::new(
                    pt,
                    (seq + 1).into(),
                    frame_timestamp,
                    first_write_at + Duration::from_millis(5),
                    [0x00, 0xCC, 0xDD, i as u8],
                )
                .marker(true)
                .nackable(true),
            );
        }
        let (_, packets) = wait_for_transmits(
            &mut l,
            first_write_at + Duration::from_millis(5),
            Duration::from_millis(50),
        )?;
        assert!(
            !packets.is_empty(),
            "expected at least one packet when flushing delayed tail"
        );
        let pause_end = l.last + pause_duration;
        while l.last < pause_end || r.last < pause_end {
            progress(&mut l, &mut r)?;
        }

        assert!(
            r.events
                .iter()
                .any(|(_, e)| matches!(e, Event::StreamPaused(p) if p.paused)),
            "receiver never entered paused state"
        );

        let before_tail_event_count = r.events.len();
        let delivery_time = pause_end + Duration::from_millis(20);
        for packet in packets {
            deliver_packet(&mut r, delivery_time, packet)?;
        }

        let resumed = r.events[before_tail_event_count..]
            .iter()
            .find_map(|(_, event)| match event {
                Event::MediaData(frame) => Some(frame),
                _ => None,
            })
            .expect("expected the frame spanning the pause to complete");
        assert_eq!(*resumed.seq_range.start(), seq.into());
        assert_eq!(*resumed.seq_range.end(), (seq + 1).into());

        let fresh_first_at = delivery_time + Duration::from_millis(20);
        let fresh_frame_timestamp = frame_timestamp + 3_000;
        let fresh_seq = seq + 2;

        {
            let mut direct = l.direct_api();
            let tx = direct.stream_tx_by_mid(mid, None).unwrap();
            tx.write_rtp(
                RtpWrite::new(
                    pt,
                    fresh_seq.into(),
                    fresh_frame_timestamp,
                    fresh_first_at,
                    [0x10, 0x00, 0x55, 0x66, i as u8],
                )
                .nackable(true),
            );
        }
        let (fresh_first_emit_at, mut fresh_first_packets) =
            wait_for_transmits(&mut l, fresh_first_at, Duration::from_millis(50))?;
        assert_eq!(
            fresh_first_packets.len(),
            1,
            "expected one fresh first-fragment packet"
        );
        deliver_packet(&mut r, fresh_first_emit_at, fresh_first_packets.remove(0))?;

        {
            let mut direct = l.direct_api();
            let tx = direct.stream_tx_by_mid(mid, None).unwrap();
            tx.write_rtp(
                RtpWrite::new(
                    pt,
                    (fresh_seq + 1).into(),
                    fresh_frame_timestamp,
                    fresh_first_at + Duration::from_millis(5),
                    [0x00, 0x77, 0x88, i as u8],
                )
                .marker(true)
                .nackable(true),
            );
        }
        let (fresh_tail_emit_at, fresh_tail_packets) = wait_for_transmits(
            &mut l,
            fresh_first_at + Duration::from_millis(5),
            Duration::from_millis(50),
        )?;
        let before_fresh_event_count = r.events.len();
        for packet in fresh_tail_packets {
            deliver_packet(&mut r, fresh_tail_emit_at, packet)?;
        }

        let (event_time, media) = r.events[before_fresh_event_count..]
            .iter()
            .find_map(|(event_time, e)| match e {
                Event::MediaData(data) => Some((*event_time, data)),
                _ => None,
            })
            .expect("expected MediaData from the fresh post-pause frame");

        assert_eq!(*media.seq_range.start(), fresh_seq.into());
        assert_eq!(*media.seq_range.end(), (fresh_seq + 1).into());
        fresh_ages.push(event_time.saturating_duration_since(media.network_time));
    }

    Ok(fresh_ages)
}

#[test]
fn delayed_vp8_tail_packet_after_pause_completes_frame() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let age = measure_fresh_age_after_pause(Duration::from_millis(2200))?;

    assert!(
        age < Duration::from_millis(200),
        "expected fresh MediaData after pause recovery, got age {:?}",
        age
    );

    Ok(())
}

#[test]
fn longer_pauses_still_recover_with_fresh_media() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let pause_durations = [
        Duration::from_millis(2200),
        Duration::from_millis(4200),
        Duration::from_millis(8200),
    ];

    let observed_ages: Vec<_> = pause_durations
        .into_iter()
        .map(measure_fresh_age_after_pause)
        .collect::<Result<_, _>>()?;

    assert!(
        observed_ages
            .iter()
            .all(|age| *age < Duration::from_millis(200)),
        "expected fresh recovery after long pauses, got {:?}",
        observed_ages
    );

    Ok(())
}

#[test]
fn repeated_pause_cycles_recover_without_stale_media() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let ages = collect_fresh_ages_after_repeated_pauses(&[
        Duration::from_millis(2200),
        Duration::from_millis(4200),
        Duration::from_millis(6200),
    ])?;

    assert!(
        ages.iter().all(|age| *age < Duration::from_millis(200)),
        "expected fresh media after each pause cycle, got {:?}",
        ages
    );

    Ok(())
}
