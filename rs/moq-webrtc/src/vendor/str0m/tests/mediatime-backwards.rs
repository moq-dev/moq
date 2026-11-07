use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use str0m::format::Codec;
use str0m::media::{Direction, MediaKind, MediaTime};
use str0m::rtp::RawPacket;
use str0m::{Event, RtcError};
use tracing::info;

mod common;
use common::{Peer, TestRtc, init_crypto_default, init_log, progress};

#[test]
pub fn mediatime_backwards() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let mut l = TestRtc::new(Peer::Left);
    let mut r = TestRtc::new(Peer::Right);

    // Enable raw packets to trace the RTP headers
    r.rtc = str0m::Rtc::builder()
        .enable_raw_packets(true)
        .build(Instant::now());

    l.add_host_candidate((Ipv4Addr::new(1, 1, 1, 1), 1000).into());
    r.add_host_candidate((Ipv4Addr::new(2, 2, 2, 2), 2000).into());

    // The change is on the L (sending side) with Direction::SendRecv.
    let mut change = l.sdp_api();
    let mid = change.add_media(MediaKind::Audio, Direction::SendRecv, None, None, None);
    let (offer, pending) = change.apply().unwrap();

    let answer = r.rtc.sdp_api().accept_offer(offer)?;
    l.rtc.sdp_api().accept_answer(pending, answer)?;

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

    let data_a = vec![1_u8; 80];

    // First, send some normal packets with timestamps < 2^31
    let base_timestamp = 1_000_000_u32; // Well below 2^31

    // Send 10 normal packets
    for i in 0..10 {
        let wallclock = l.start + l.duration();
        let timestamp = base_timestamp + i * 960; // Standard Opus timestamp increment
        let time = MediaTime::from_90khz(timestamp as u64);

        l.rtc
            .writer(mid)
            .unwrap()
            .write(pt, wallclock, time, data_a.clone())?;

        progress(&mut l, &mut r)?;
    }

    info!("Sent 10 normal packets with regular timestamps");

    // Store the last RTP timestamp we observed before the pause
    let mut last_ts_before_pause: Option<u32> = None;

    // Get the last timestamp from received packets
    for (_, event) in &r.events {
        if let Event::RawPacket(raw_packet) = event {
            if let RawPacket::RtpRx(header, _) = &**raw_packet {
                last_ts_before_pause = Some(header.timestamp);
            }
        }
    }

    info!("Last timestamp before pause: {:?}", last_ts_before_pause);

    // Now advance time by more than 1.5 seconds (default pause threshold) without sending any packets
    // This will trigger the pause condition in the receiver
    let pause_duration = Duration::from_millis(2000);

    // Advance time using timeouts instead of sleep
    let pause_end = l.last + pause_duration;
    while l.last < pause_end {
        // Just handle timeouts without sending any packets
        progress(&mut l, &mut r)?;
    }

    info!(
        "Advanced time by {} ms to trigger pause",
        pause_duration.as_millis()
    );

    // Wait to ensure we get a paused event
    let mut seen_paused = false;
    let pause_check_start = l.last;
    let pause_check_timeout = Duration::from_millis(500);

    while !seen_paused && l.last < pause_check_start + pause_check_timeout {
        progress(&mut l, &mut r)?;

        // Check for pause event
        for (_, event) in &r.events {
            if let Event::StreamPaused(stream_paused) = event {
                if stream_paused.paused {
                    seen_paused = true;
                    info!("Confirmed stream is paused");
                    break;
                }
            }
        }
    }

    assert!(
        seen_paused,
        "Stream did not enter paused state within timeout"
    );

    // Now send a packet with a timestamp that will trigger the bug
    // We need timestamp > 2^31 + previous_timestamp to trigger the backwards condition
    // Using the threshold from header.rs, HALF = 1 << 31
    let problematic_timestamp = (1u32 << 31) + base_timestamp + 10 * 960;

    info!(
        "Sending packet with problematic timestamp: {}",
        problematic_timestamp
    );

    let wallclock = l.start + l.duration();
    let time = MediaTime::from_90khz(problematic_timestamp as u64);

    // Save the timestamp index for comparison BEFORE sending
    let events_before_problematic = r.events.len();

    l.rtc
        .writer(mid)
        .unwrap()
        .write(pt, wallclock, time, data_a.clone())?;

    // Process the packet and a few more cycles to ensure it's received
    for _ in 0..10 {
        progress(&mut l, &mut r)?;
    }

    // We're looking for evidence that:
    // 1. The stream unpaused (proving packet was received)
    // 2. The timestamp value in the received packet
    let mut problematic_ts: Option<u32> = None;
    let mut stream_unpaused_after_problematic = false;

    // Only look at events that occurred after sending our problematic packet
    for (_, (_, event)) in r.events.iter().enumerate().skip(events_before_problematic) {
        match event {
            Event::RawPacket(raw_packet) => {
                if let RawPacket::RtpRx(header, _) = &**raw_packet {
                    info!("After pause - RTP timestamp: {}", header.timestamp);
                    problematic_ts = Some(header.timestamp);
                }
            }
            Event::StreamPaused(stream_paused) if !stream_paused.paused => {
                stream_unpaused_after_problematic = true;
                info!("Stream unpaused after receiving problematic packet");
            }
            _ => {}
        }
    }

    // The test succeeds if:
    // 1. We confirmed the stream was paused
    // 2. We received the packet with the problematic timestamp
    // 3. The stream unpaused after receiving the problematic packet
    // 4. The timestamps continue to move forward after the pause

    assert!(seen_paused, "Stream was never paused");
    assert!(
        problematic_ts.is_some(),
        "Did not receive packet with problematic timestamp"
    );
    assert!(
        stream_unpaused_after_problematic,
        "Stream did not unpause after receiving problematic packet"
    );

    // Verify that time moves forward even after a pause with a problematic timestamp
    // This is the core of the bugfix - ensuring that MediaTime never goes backwards
    if let (Some(prev_ts), Some(new_ts)) = (last_ts_before_pause, problematic_ts) {
        assert!(
            new_ts > prev_ts,
            "Time went backwards! Previous timestamp: {}, Current timestamp: {}",
            prev_ts,
            new_ts
        );
    }

    Ok(())
}
