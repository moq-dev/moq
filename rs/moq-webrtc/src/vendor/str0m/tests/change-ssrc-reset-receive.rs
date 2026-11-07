use std::collections::VecDeque;
use std::time::{Duration, Instant};

use str0m::format::Codec;
use str0m::media::{Frequency, MediaKind, MediaTime, Mid, Pt, Rid};
use str0m::rtp::{ExtensionValues, RtpWrite, Ssrc};
use str0m::{Event, Rtc, RtcError};
use tracing::info;

mod common;
use common::{TestRtc, connect_l_r, connect_l_r_with_rtc, init_crypto_default, init_log, progress};

fn send_frame(
    l: &mut TestRtc,
    r: &mut TestRtc,
    mid: Mid,
    rid: Rid,
    pt: Pt,
    seq: u64,
    timestamp: u32,
    payload: &'static [u8],
) -> Result<(), RtcError> {
    let at = l.last.max(r.last) + Duration::from_millis(10);
    l.direct_api()
        .stream_tx_by_mid(mid, Some(rid))
        .unwrap()
        .write_rtp(RtpWrite::new(pt, seq.into(), timestamp, at, payload).marker(true));
    for _ in 0..100 {
        progress(l, r)?;
    }
    Ok(())
}

fn received_frame(r: &TestRtc, pt: Pt, seq: u64) -> bool {
    r.events.iter().any(|(_, event)| match event {
        Event::MediaData(frame) => frame.pt == pt && frame.seq_range.contains(&seq.into()),
        _ => false,
    })
}

#[test]
fn ssrc_change_resets_every_payload_type_on_the_stream() -> Result<(), RtcError> {
    init_crypto_default();
    let now = Instant::now();
    let (mut l, mut r) = connect_l_r_with_rtc(
        Rtc::builder().set_rtp_mode(true).build(now),
        Rtc::builder().build(now),
    );
    let mid = "vid".into();
    let rid = "hi".into();
    l.direct_api().declare_media(mid, MediaKind::Video);
    l.direct_api()
        .declare_stream_tx(42.into(), None, mid, Some(rid));
    r.direct_api()
        .declare_media(mid, MediaKind::Video)
        .expect_rid_rx(rid);
    let vp8_pt = l.params_vp8().pt();
    let h264_pt = l.params_h264().pt();

    send_frame(
        &mut l,
        &mut r,
        mid,
        rid,
        vp8_pt,
        100,
        90_000,
        &[0x10, 0x01, 0xaa],
    )?;
    assert!(received_frame(&r, vp8_pt, 100));

    l.direct_api()
        .reset_stream_tx(mid, Some(rid), 84.into(), None)
        .expect("SSRC reset");
    // The first packet on the new SSRC uses H.264, so it resets that depacketizer.
    send_frame(&mut l, &mut r, mid, rid, h264_pt, 1, 180_000, &[0x65, 0xaa])?;
    assert!(received_frame(&r, h264_pt, 1));
    // VP8 must also start fresh on the new SSRC, even though it was not the
    // payload type of the packet that signaled the change.
    send_frame(
        &mut l,
        &mut r,
        mid,
        rid,
        vp8_pt,
        2,
        270_000,
        &[0x10, 0x01, 0xbb],
    )?;
    assert!(
        received_frame(&r, vp8_pt, 2),
        "VP8 frame on the replacement SSRC was lost to the old depacketizer state"
    );
    Ok(())
}

#[test]
pub fn change_ssrc_reset_receive() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let (mut l, mut r) = connect_l_r();

    let mid = "aud".into();
    let rid = "hi".into();

    // Initial SSRC
    let ssrc_tx_initial: Ssrc = 42.into();
    // New SSRC to use after reset
    let ssrc_tx_new: Ssrc = 84.into();

    l.direct_api().declare_media(mid, MediaKind::Audio);
    l.direct_api()
        .declare_stream_tx(ssrc_tx_initial, None, mid, Some(rid));

    r.direct_api()
        .declare_media(mid, MediaKind::Audio)
        .expect_rid_rx(rid);

    // Set initial timing
    let max = l.last.max(r.last);
    l.last = max;
    r.last = max;

    let params = l.params_opus();
    let ssrc = l
        .direct_api()
        .stream_tx_by_mid(mid, Some(rid))
        .unwrap()
        .ssrc();
    assert_eq!(
        ssrc, ssrc_tx_initial,
        "Initial SSRC should match what we set"
    );
    assert_eq!(params.spec().codec, Codec::Opus);
    let pt = params.pt();

    // First batch of packets with initial SSRC
    // We'll create a batch with several packets to cause timestamp rollover
    let first_batch: Vec<&[u8]> = vec![
        &[0x1, 0x2, 0x3, 0x4], // packet at a timestamp close to rollover
        &[0x5, 0x6, 0x7, 0x8], // packet at rollover point
        &[0x9, 0xa, 0xb, 0xc], // packet after rollover
        &[0xd, 0xe, 0xf, 0x0], // another packet after rollover
    ];

    let mut first_batch: VecDeque<_> = first_batch.into();

    // Create a timestamp sequence that will cross the rollover boundary (close to 2^32)
    // 32-bit timestamp max is 4,294,967,295 (0xFFFFFFFF)
    // Start at a value very close to rollover
    let rollover_threshold: u32 = 0xFFFFFFFF;
    let timestamp_increment: u32 = 1000; // standard increment (e.g., for Opus)

    // Start our timeline near the rollover point
    let start_timestamp: u32 = rollover_threshold - timestamp_increment * 2;

    // Counts for each packet - they'll increase, causing timestamp to roll over
    let mut first_counts: Vec<u64> = vec![0, 1, 2, 3];
    let mut write_at = l.last + Duration::from_millis(300);

    // Send first batch with initial SSRC
    loop {
        if l.start + l.duration() > write_at {
            write_at = l.last + Duration::from_millis(300);
            if let Some(packet) = first_batch.pop_front() {
                let wallclock = l.start + l.duration();
                let mut direct = l.direct_api();
                let stream = direct.stream_tx_by_mid(mid, Some(rid)).unwrap();

                let count = first_counts.remove(0);
                // Calculate timestamp to create rollover scenario
                let time = start_timestamp.wrapping_add(count as u32 * timestamp_increment);

                info!(
                    "Sending packet {} with timestamp: {} on SSRC: {}",
                    count, time, ssrc_tx_initial
                );
                let seq_no = (47_000 + count).into();

                let exts = ExtensionValues {
                    audio_level: Some(-42 - count as i8),
                    voice_activity: Some(false),
                    ..Default::default()
                };

                stream.write_rtp(RtpWrite::new(pt, seq_no, time, wallclock, packet).ext_vals(exts));
            }
        }

        progress(&mut l, &mut r)?;

        if first_batch.is_empty() && first_counts.is_empty() {
            break;
        }
    }

    // Run a bit longer to ensure first batch of packets arrive
    for _ in 0..20 {
        progress(&mut l, &mut r)?;
    }

    info!("First batch sent with SSRC: {}", ssrc_tx_initial);

    // Reset the SSRC with None for RTX since the stream doesn't use RTX
    let mut api = l.direct_api();
    let result = api.reset_stream_tx(mid, Some(rid), ssrc_tx_new, None);
    assert!(result.is_some(), "Reset should succeed with valid new SSRC");

    // Verify the SSRC was changed
    let updated_ssrc = l
        .direct_api()
        .stream_tx_by_mid(mid, Some(rid))
        .unwrap()
        .ssrc();
    assert_eq!(
        updated_ssrc, ssrc_tx_new,
        "SSRC should be updated to new value"
    );

    // Wait to ensure pause detection occurs (default threshold is 1.5s)
    let pause_duration = Duration::from_millis(2000);
    let pause_end = l.last + pause_duration;
    while l.last < pause_end {
        progress(&mut l, &mut r)?;
    }

    info!(
        "Advanced time by {} ms to potentially trigger pause",
        pause_duration.as_millis()
    );

    // Check if the stream was paused
    let mut seen_paused = false;
    for (_, event) in &r.events {
        if let Event::StreamPaused(stream_paused) = event {
            if stream_paused.paused {
                seen_paused = true;
                info!("Confirmed stream is paused");
                break;
            }
        }
    }

    // Second batch of packets with new SSRC, but with timestamps that would appear to go
    // backward if last_time is not reset during change_ssrc
    let second_batch: Vec<&[u8]> = vec![&[0x9, 0xa, 0xb, 0xc], &[0xd, 0xe, 0xf, 0x10]];

    let mut second_batch: VecDeque<_> = second_batch.into();
    let mut second_counts: Vec<u64> = vec![0, 1];
    write_at = l.last + Duration::from_millis(300);

    // Calculate a timestamp that would appear to go backward if last_time isn't reset
    // The first batch used timestamps starting at 47_000_000
    // We'll use a timestamp that's lower, which would be interpreted as going backward
    // if the internal state isn't reset
    let backwards_base_timestamp: u32 = 1_000_000;

    // Send second batch with new SSRC and potentially problematic timestamps
    loop {
        if l.start + l.duration() > write_at {
            write_at = l.last + Duration::from_millis(300);
            if let Some(packet) = second_batch.pop_front() {
                let wallclock = l.start + l.duration();
                let mut direct = l.direct_api();
                let stream = direct.stream_tx_by_mid(mid, Some(rid)).unwrap();

                let count = second_counts.remove(0);
                // Use a timestamp that would appear to go backward if last_time isn't reset
                let time = (count as u32) * 1000 + backwards_base_timestamp;
                let seq_no = (48_000 + count).into(); // Different seq range

                let exts = ExtensionValues {
                    audio_level: Some(-52 - count as i8),
                    voice_activity: Some(false),
                    ..Default::default()
                };

                info!(
                    "Sending packet with potentially backward timestamp: {} on SSRC: {}",
                    time, ssrc_tx_new
                );

                stream.write_rtp(RtpWrite::new(pt, seq_no, time, wallclock, packet).ext_vals(exts));
            }
        }

        progress(&mut l, &mut r)?;

        if second_batch.is_empty() && second_counts.is_empty() {
            // Run a bit longer to ensure packets arrive
            for _ in 0..20 {
                progress(&mut l, &mut r)?;
            }
            break;
        }
    }

    // Collect all received media packets and Media events
    let media_packets: Vec<_> = r
        .events
        .iter()
        .filter_map(|(_, e)| {
            if let Event::RtpPacket(v) = e {
                Some(v)
            } else {
                None
            }
        })
        .collect();

    info!("Collected {} RTP packets", media_packets.len(),);

    // Should have received all 6 packets (4 from first batch + 2 from second batch)
    assert_eq!(media_packets.len(), 6, "Should have received all 6 packets");

    // Verify that we have packets with both SSRCs
    let first_ssrc_packets: Vec<_> = media_packets
        .iter()
        .filter(|p| p.header.ssrc == ssrc_tx_initial)
        .collect();
    let second_ssrc_packets: Vec<_> = media_packets
        .iter()
        .filter(|p| p.header.ssrc == ssrc_tx_new)
        .collect();

    assert_eq!(
        first_ssrc_packets.len(),
        4,
        "Should have 4 packets with initial SSRC"
    );
    assert_eq!(
        second_ssrc_packets.len(),
        2,
        "Should have 2 packets with new SSRC"
    );

    // Verify the stream was unpaused after receiving the second batch with new SSRC
    let mut stream_unpaused_after_new_ssrc = false;
    for (_, event) in &r.events {
        if let Event::StreamPaused(stream_paused) = event {
            if !stream_paused.paused && stream_paused.ssrc == ssrc_tx_new {
                stream_unpaused_after_new_ssrc = true;
                info!("Confirmed stream unpaused after new SSRC packets");
                break;
            }
        }
    }

    // If we saw a pause event, we should also see an unpause event after the new packets
    if seen_paused {
        assert!(
            stream_unpaused_after_new_ssrc,
            "Stream should have unpaused after receiving packets with new SSRC"
        );
    }

    // Verify the first batch packets sequence numbers
    for (i, packet) in first_ssrc_packets.iter().enumerate() {
        assert_eq!(
            packet.header.sequence_number,
            47000 + i as u16,
            "First batch packet {} should have correct sequence number",
            i
        );
    }

    // Verify first batch timestamps - they should cross the rollover boundary
    // First two packets should be before rollover, second two after rollover
    // Calculating expected timestamps for verification
    let expected_timestamps: Vec<u32> = (0..4)
        .map(|i| start_timestamp.wrapping_add(i as u32 * timestamp_increment))
        .collect();

    // Verify the rollover pattern
    // Based on the logs, we can see that packets 1, 2, and 3 have timestamps close to max,
    // and only the 4th packet rolls over
    info!("Checking timestamp pattern for packets crossing rollover");
    for (i, ts) in expected_timestamps.iter().enumerate() {
        info!("Packet {} timestamp: {}", i, ts);
    }

    // Verify the actual timestamps match our expected values
    for (i, packet) in first_ssrc_packets.iter().enumerate() {
        assert_eq!(
            packet.header.timestamp, expected_timestamps[i],
            "First batch packet {} should have expected timestamp after rollover",
            i
        );
    }

    // Verify second batch (new SSRC) sequence numbers
    for (i, packet) in second_ssrc_packets.iter().enumerate() {
        assert_eq!(
            packet.header.sequence_number,
            48000 + i as u16,
            "Second batch packet {} should have correct sequence number",
            i
        );
    }

    // The timestamps for the second batch are much lower, but should be interpreted correctly
    // because the internal state was reset by change_ssrc()
    assert_eq!(
        second_ssrc_packets[0].header.timestamp, backwards_base_timestamp,
        "First packet with new SSRC should have expected timestamp"
    );
    assert_eq!(
        second_ssrc_packets[1].header.timestamp,
        backwards_base_timestamp + 1000,
        "Second packet with new SSRC should have expected timestamp"
    );

    let mut api = r.direct_api();
    let rx = api.stream_rx_by_mid(mid, Some(rid)).unwrap();

    // If we don't reset the state properly on SSRC change, this will be
    // a crazy value.
    assert_eq!(
        rx.last_time(),
        Some(MediaTime::new(1001000, Frequency::FORTY_EIGHT_KHZ))
    );

    // This test verifies two key behaviors:
    // 1. Timestamp rollover is handled correctly within a single SSRC
    // 2. After SSRC change, timestamp history is reset so a low timestamp
    //    doesn't appear to go backward from the high timestamp of the previous SSRC

    Ok(())
}
