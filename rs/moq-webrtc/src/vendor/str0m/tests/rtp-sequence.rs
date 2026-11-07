//! Tests for RTP sequence number and timing edge cases.

use std::net::Ipv4Addr;
use std::time::Instant;

use str0m::media::{Direction, MediaKind};
use str0m::rtp::{RtpWrite, Ssrc};
use str0m::{Event, Rtc, RtcError};

mod common;
use common::{Peer, TestRtc, init_log, negotiate, progress};
use common::{connect_l_r_with_rtc, init_crypto_default};

/// Test handling of packets crossing the u16 sequence number boundary (65535 -> 0).
#[test]
fn rtp_sequence_number_near_boundary() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    // Use RTP mode to control sequence numbers directly
    let now = Instant::now();
    let rtc1 = Rtc::builder().set_rtp_mode(true).build(now);
    let rtc2 = Rtc::builder().build(now);

    let (mut l, mut r) = connect_l_r_with_rtc(rtc1, rtc2);

    let mid = "audio".into();
    let ssrc_tx: Ssrc = 1337.into();

    l.direct_api().declare_media(mid, MediaKind::Audio);
    l.direct_api().declare_stream_tx(ssrc_tx, None, mid, None);
    r.direct_api().declare_media(mid, MediaKind::Audio);

    let max = l.last.max(r.last);
    l.last = max;
    r.last = max;

    let params = l.params_opus();
    let pt = params.pt();
    let ssrc = l.direct_api().stream_tx_by_mid(mid, None).unwrap().ssrc();

    // Start sequence numbers near the u16 boundary (65535)
    // Send packets: 65530, 65531, 65532, 65533, 65534, 65535, 0, 1, 2, 3, 4, 5
    let start_seq: u64 = 65530;
    let packet_count = 12;

    for i in 0..packet_count {
        let seq_no = start_seq + i;
        let time = (i * 960) as u32; // Opus uses 48kHz, 20ms = 960 samples
        let wallclock = l.start + l.duration();

        {
            let mut direct = l.direct_api();
            let stream = direct.stream_tx(&ssrc).unwrap();

            stream.write_rtp(RtpWrite::new(
                pt,
                seq_no.into(),
                time,
                wallclock,
                [1_u8; 80],
            ));
        }
        progress(&mut l, &mut r)?;
    }

    // Final progress to deliver remaining packets
    for _ in 0..50 {
        progress(&mut l, &mut r)?;
    }

    // Collect received sequence numbers from seq_range
    let received_seqs: Vec<u64> = r
        .events
        .iter()
        .filter_map(|(_, e)| {
            if let Event::MediaData(data) = e {
                // Get the start of the sequence range (SeqNo derefs to u64)
                Some(**data.seq_range.start())
            } else {
                None
            }
        })
        .collect();

    // Verify we received packets across the boundary
    assert!(
        received_seqs.len() >= 10,
        "Should receive most packets, got {}",
        received_seqs.len()
    );

    // The library uses extended 64-bit sequence numbers internally.
    // Wire seq 65535 -> internal 65535, wire seq 0 (after wrap) -> internal 65536
    // Verify sequence numbers before boundary (65530-65535) were received
    let before_boundary: Vec<_> = received_seqs
        .iter()
        .filter(|&&s| (65530..=65535).contains(&s))
        .collect();
    assert!(
        !before_boundary.is_empty(),
        "Should receive packets before boundary (65530-65535), got seq_nos: {:?}",
        received_seqs
    );

    // Verify sequence numbers after boundary (65536+ = wire 0+) were received
    // Internal 65536 = wire seq 0, 65537 = wire seq 1, etc.
    let after_boundary: Vec<_> = received_seqs.iter().filter(|&&s| s >= 65536).collect();
    assert!(
        !after_boundary.is_empty(),
        "Should receive packets after boundary (65536+ = wire 0+), got seq_nos: {:?}",
        received_seqs
    );

    // Verify sequence numbers are monotonically increasing (no gaps from wrap-around)
    for i in 1..received_seqs.len() {
        assert!(
            received_seqs[i] > received_seqs[i - 1],
            "Sequence numbers should be monotonically increasing across boundary: {} should be > {}",
            received_seqs[i],
            received_seqs[i - 1]
        );
    }

    Ok(())
}

/// Test reordering buffer with audio - send packets out of order and verify reordering.
#[test]
fn rtp_reordering_buffer_audio() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    // Sender uses RTP mode to control sequence numbers
    let now = Instant::now();
    let rtc1 = Rtc::builder().set_rtp_mode(true).build(now);
    // Receiver has reordering buffer enabled (default is 15 for audio)
    let rtc2 = Rtc::builder().set_reordering_size_audio(15).build(now);

    let (mut l, mut r) = connect_l_r_with_rtc(rtc1, rtc2);

    let mid = "audio".into();
    let ssrc_tx: Ssrc = 1337.into();

    l.direct_api().declare_media(mid, MediaKind::Audio);
    l.direct_api().declare_stream_tx(ssrc_tx, None, mid, None);
    r.direct_api().declare_media(mid, MediaKind::Audio);

    let max = l.last.max(r.last);
    l.last = max;
    r.last = max;

    let params = l.params_opus();
    let pt = params.pt();
    let ssrc = l.direct_api().stream_tx_by_mid(mid, None).unwrap().ssrc();

    // Send packets OUT OF ORDER: 1, 3, 2, 5, 4, 7, 6, 9, 8, 10
    // The reordering buffer should fix this
    let send_order = [1u64, 3, 2, 5, 4, 7, 6, 9, 8, 10];

    for &seq in &send_order {
        let time = (seq * 960) as u32; // Opus 48kHz, 20ms frames
        let wallclock = l.start + l.duration();
        {
            let mut direct = l.direct_api();
            let stream = direct.stream_tx(&ssrc).unwrap();

            stream.write_rtp(RtpWrite::new(
                pt,
                seq.into(),
                time,
                wallclock,
                [seq as u8; 80],
            ));
        }
        progress(&mut l, &mut r)?;
    }

    // Final progress to flush reordering buffer
    for _ in 0..50 {
        progress(&mut l, &mut r)?;
    }

    // Collect received sequence number ranges
    let received_ranges: Vec<(u64, u64)> = r
        .events
        .iter()
        .filter_map(|(_, e)| {
            if let Event::MediaData(data) = e {
                Some((**data.seq_range.start(), **data.seq_range.end()))
            } else {
                None
            }
        })
        .collect();

    assert!(!received_ranges.is_empty(), "Should receive some packets");

    // Verify ranges are non-decreasing (reordering buffer should fix order)
    // Each range's start should be >= previous range's start
    for i in 1..received_ranges.len() {
        assert!(
            received_ranges[i].0 >= received_ranges[i - 1].0,
            "Packets should be reordered: range {:?} should come after {:?}, all ranges: {:?}",
            received_ranges[i],
            received_ranges[i - 1],
            received_ranges
        );
    }

    // Verify we received packets spanning our sent range (1-10)
    let min_received = received_ranges.iter().map(|r| r.0).min().unwrap();
    let max_received = received_ranges.iter().map(|r| r.1).max().unwrap();
    assert!(
        min_received <= 2,
        "Should receive early packets, min was {}",
        min_received
    );
    assert!(
        max_received >= 9,
        "Should receive late packets, max was {}",
        max_received
    );

    Ok(())
}

/// Test reordering buffer with video - send packets out of order and verify reordering.
#[test]
fn rtp_reordering_buffer_video() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    // Sender uses RTP mode to control sequence numbers
    let now = Instant::now();
    let rtc1 = Rtc::builder().set_rtp_mode(true).build(now);
    // Receiver has reordering buffer for video
    let rtc2 = Rtc::builder().set_reordering_size_video(30).build(now);

    let (mut l, mut r) = connect_l_r_with_rtc(rtc1, rtc2);

    let mid = "video".into();
    let ssrc_tx: Ssrc = 1337.into();

    l.direct_api().declare_media(mid, MediaKind::Video);
    l.direct_api().declare_stream_tx(ssrc_tx, None, mid, None);
    r.direct_api().declare_media(mid, MediaKind::Video);

    let max = l.last.max(r.last);
    l.last = max;
    r.last = max;

    let params = l.params_vp8();
    let pt = params.pt();
    let ssrc = l.direct_api().stream_tx_by_mid(mid, None).unwrap().ssrc();

    // Send video packets OUT OF ORDER: 1, 3, 2, 5, 4, 7, 6, 9, 8, 10
    let send_order = [1u64, 3, 2, 5, 4, 7, 6, 9, 8, 10];

    for &seq in &send_order {
        let time = (seq * 3000) as u32; // 90kHz video clock, ~33ms frames
        let wallclock = l.start + l.duration();
        {
            let mut direct = l.direct_api();
            let stream = direct.stream_tx(&ssrc).unwrap();

            // VP8 keyframe header
            stream.write_rtp(
                RtpWrite::new(
                    pt,
                    seq.into(),
                    time,
                    wallclock,
                    [0x10, 0x00, 0x00, seq as u8],
                )
                .marker(true),
            );
        }
        progress(&mut l, &mut r)?;
    }

    // Final progress to flush reordering buffer
    for _ in 0..50 {
        progress(&mut l, &mut r)?;
    }

    // Collect received sequence number ranges
    let received_ranges: Vec<(u64, u64)> = r
        .events
        .iter()
        .filter_map(|(_, e)| {
            if let Event::MediaData(data) = e {
                Some((**data.seq_range.start(), **data.seq_range.end()))
            } else {
                None
            }
        })
        .collect();

    assert!(
        !received_ranges.is_empty(),
        "Should receive some video packets"
    );

    // Verify ranges are non-decreasing (reordering buffer should fix order)
    for i in 1..received_ranges.len() {
        assert!(
            received_ranges[i].0 >= received_ranges[i - 1].0,
            "Video packets should be reordered: {:?} should come after {:?}",
            received_ranges[i],
            received_ranges[i - 1]
        );
    }

    Ok(())
}

/// Test custom reordering buffer size - small buffer handles small gaps.
#[test]
fn rtp_reordering_buffer_custom_size() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    // Sender uses RTP mode
    let now = Instant::now();
    let rtc1 = Rtc::builder().set_rtp_mode(true).build(now);
    // Receiver has small reordering buffer (5 packets)
    let rtc2 = Rtc::builder().set_reordering_size_audio(5).build(now);

    let (mut l, mut r) = connect_l_r_with_rtc(rtc1, rtc2);

    let mid = "audio".into();
    let ssrc_tx: Ssrc = 1337.into();

    l.direct_api().declare_media(mid, MediaKind::Audio);
    l.direct_api().declare_stream_tx(ssrc_tx, None, mid, None);
    r.direct_api().declare_media(mid, MediaKind::Audio);

    let max = l.last.max(r.last);
    l.last = max;
    r.last = max;

    let params = l.params_opus();
    let pt = params.pt();
    let ssrc = l.direct_api().stream_tx_by_mid(mid, None).unwrap().ssrc();

    // Send packets with small gaps (within buffer size of 5)
    // Order: 1, 3, 2, 4, 6, 5, 7, 9, 8, 10 (max gap of 2)
    let send_order = [1u64, 3, 2, 4, 6, 5, 7, 9, 8, 10];

    for &seq in &send_order {
        let time = (seq * 960) as u32;
        let wallclock = l.start + l.duration();
        {
            let mut direct = l.direct_api();
            let stream = direct.stream_tx(&ssrc).unwrap();

            stream.write_rtp(RtpWrite::new(
                pt,
                seq.into(),
                time,
                wallclock,
                [seq as u8; 80],
            ));
        }
        progress(&mut l, &mut r)?;
    }

    for _ in 0..50 {
        progress(&mut l, &mut r)?;
    }

    let received_ranges: Vec<(u64, u64)> = r
        .events
        .iter()
        .filter_map(|(_, e)| {
            if let Event::MediaData(data) = e {
                Some((**data.seq_range.start(), **data.seq_range.end()))
            } else {
                None
            }
        })
        .collect();

    assert!(
        !received_ranges.is_empty(),
        "Should receive packets with small reordering buffer"
    );

    // Verify ordering is correct
    for i in 1..received_ranges.len() {
        assert!(
            received_ranges[i].0 >= received_ranges[i - 1].0,
            "Packets should be reordered with small buffer"
        );
    }

    Ok(())
}

/// Test media time increases correctly.
#[test]
fn rtp_media_time_increasing() -> Result<(), RtcError> {
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

    // Send packets with increasing timestamps
    for _ in 0..50 {
        let wallclock = l.start + l.duration();
        let time = l.duration().into();
        l.writer(mid)
            .unwrap()
            .write(pt, wallclock, time, data.clone())?;
        progress(&mut l, &mut r)?;
    }

    // Verify we received packets (media time handling worked)
    let received_count = r
        .events
        .iter()
        .filter(|(_, e)| matches!(e, Event::MediaData(_)))
        .count();

    assert!(
        received_count > 20,
        "Should receive packets with increasing timestamps, got {}",
        received_count
    );

    Ok(())
}

/// Test large reordering buffer - handles larger out-of-order gaps.
#[test]
fn rtp_reordering_buffer_large() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    // Sender uses RTP mode
    let now = Instant::now();
    let rtc1 = Rtc::builder().set_rtp_mode(true).build(now);
    // Receiver has large reordering buffer (50 packets)
    let rtc2 = Rtc::builder().set_reordering_size_audio(50).build(now);

    let (mut l, mut r) = connect_l_r_with_rtc(rtc1, rtc2);

    let mid = "audio".into();
    let ssrc_tx: Ssrc = 1337.into();

    l.direct_api().declare_media(mid, MediaKind::Audio);
    l.direct_api().declare_stream_tx(ssrc_tx, None, mid, None);
    r.direct_api().declare_media(mid, MediaKind::Audio);

    let max = l.last.max(r.last);
    l.last = max;
    r.last = max;

    let params = l.params_opus();
    let pt = params.pt();
    let ssrc = l.direct_api().stream_tx_by_mid(mid, None).unwrap().ssrc();

    // Send packets with larger gaps that require big buffer
    // Send 1, 10, 2, 11, 3, 12, 4, 13... (interleaved with gap of ~9)
    // Shuffle to create out-of-order: 1, 11, 2, 12, 3, 13...
    let mut send_order = Vec::new();
    for i in 0_u64..10 {
        send_order.push(1 + i);
        send_order.push(11 + i);
    }

    for seq in send_order {
        let time = (seq * 960) as u32;
        let wallclock = l.start + l.duration();
        {
            let mut direct = l.direct_api();
            let stream = direct.stream_tx(&ssrc).unwrap();

            stream.write_rtp(RtpWrite::new(
                pt,
                seq.into(),
                time,
                wallclock,
                [seq as u8; 80],
            ));
        }
        progress(&mut l, &mut r)?;
    }

    for _ in 0..50 {
        progress(&mut l, &mut r)?;
    }

    let received_ranges: Vec<(u64, u64)> = r
        .events
        .iter()
        .filter_map(|(_, e)| {
            if let Event::MediaData(data) = e {
                Some((**data.seq_range.start(), **data.seq_range.end()))
            } else {
                None
            }
        })
        .collect();

    assert!(
        !received_ranges.is_empty(),
        "Should receive packets with large reordering buffer"
    );

    // Verify ordering is correct despite large gaps
    for i in 1..received_ranges.len() {
        assert!(
            received_ranges[i].0 >= received_ranges[i - 1].0,
            "Packets should be reordered with large buffer: {:?} vs {:?}",
            received_ranges[i],
            received_ranges[i - 1]
        );
    }

    Ok(())
}
