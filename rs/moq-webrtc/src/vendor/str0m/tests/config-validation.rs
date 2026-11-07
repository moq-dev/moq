//! Tests for configuration edge cases and validation.

use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use str0m::media::{Direction, MediaKind};
use str0m::{Event, RtcConfig, RtcError};
use tracing::info_span;

mod common;
use common::{Peer, TestRtc, init_crypto_default, init_log, negotiate, progress};

/// Test set_reordering_size_audio() and set_reordering_size_video() with custom sizes.
#[test]
fn config_reordering_size_custom() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    // Verify config builder correctly sets and retrieves values
    let config = RtcConfig::new()
        .set_reordering_size_audio(20)
        .set_reordering_size_video(50);

    // Verify the config has correct values before building
    assert_eq!(
        config.reordering_size_audio(),
        20,
        "Audio reordering size should be 20"
    );
    assert_eq!(
        config.reordering_size_video(),
        50,
        "Video reordering size should be 50"
    );

    // Verify default values are different
    let default_config = RtcConfig::new();
    assert_eq!(
        default_config.reordering_size_audio(),
        15,
        "Default audio reordering should be 15"
    );
    assert_eq!(
        default_config.reordering_size_video(),
        30,
        "Default video reordering should be 30"
    );

    // Build and verify the Rtc works with custom config
    let rtc = config.build(Instant::now());
    let mut l = TestRtc::new_with_rtc(info_span!("L"), rtc);
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
        if l.duration() > Duration::from_secs(5) {
            panic!("Failed to connect");
        }
        progress(&mut l, &mut r)?;
    }

    // Send and receive data to verify the config doesn't break functionality
    let params = l.params_opus();
    let pt = params.pt();
    let data = vec![1_u8; 80];

    for _ in 0..20 {
        let wallclock = l.start + l.duration();
        let time = l.duration().into();
        l.writer(mid)
            .unwrap()
            .write(pt, wallclock, time, data.clone())?;
        progress(&mut l, &mut r)?;
    }

    // Verify data was received
    let received_count = r
        .events
        .iter()
        .filter(|(_, e)| matches!(e, Event::MediaData(_)))
        .count();

    assert!(
        received_count > 10,
        "Should receive media data with custom reordering config, got {}",
        received_count
    );

    Ok(())
}

/// Test timeout defaults and configuration round-trips, including large durations.
#[test]
fn config_audio_reordering_timeout_custom() {
    assert_eq!(
        RtcConfig::new().reordering_timeout_audio(),
        Some(Duration::from_secs(1))
    );
    for timeout in [
        None,
        Some(Duration::ZERO),
        Some(Duration::from_millis(250)),
        Some(Duration::from_secs(601)),
        Some(Duration::MAX),
    ] {
        let config = RtcConfig::new().set_reordering_timeout_audio(timeout);
        assert_eq!(config.reordering_timeout_audio(), timeout);
        assert_eq!(config.clone().reordering_timeout_audio(), timeout);
    }
}

/// Test enable_raw_packets(true) produces RawPacket events.
#[test]
fn config_raw_packets_enabled() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let rtc = RtcConfig::new()
        .enable_raw_packets(true)
        .build(Instant::now());

    let mut l = TestRtc::new_with_rtc(info_span!("L"), rtc);

    let rtc_r = RtcConfig::new()
        .enable_raw_packets(true)
        .build(Instant::now());
    let mut r = TestRtc::new_with_rtc(info_span!("R"), rtc_r);

    l.add_host_candidate((Ipv4Addr::new(1, 1, 1, 1), 1000).into());
    r.add_host_candidate((Ipv4Addr::new(2, 2, 2, 2), 2000).into());

    let mid = negotiate(&mut l, &mut r, |change| {
        change.add_media(MediaKind::Audio, Direction::SendRecv, None, None, None)
    });

    loop {
        if l.is_connected() && r.is_connected() {
            break;
        }
        if l.duration() > Duration::from_secs(5) {
            panic!("Failed to connect");
        }
        progress(&mut l, &mut r)?;
    }

    // Clear previous events
    l.events.clear();
    r.events.clear();

    // Send some audio data
    let params = l.params_opus();
    let pt = params.pt();
    let data = vec![1_u8; 80];

    for _ in 0..20 {
        let wallclock = l.start + l.duration();
        let time = l.duration().into();
        l.writer(mid)
            .unwrap()
            .write(pt, wallclock, time, data.clone())?;
        progress(&mut l, &mut r)?;
    }

    // Check for RawPacket events
    let raw_packet_count = r
        .events
        .iter()
        .filter(|(_, e)| matches!(e, Event::RawPacket(_)))
        .count();

    assert!(
        raw_packet_count > 0,
        "Should have RawPacket events with enable_raw_packets(true)"
    );

    Ok(())
}

/// Test set_stats_interval with custom duration.
#[test]
fn config_stats_interval_custom() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let stats_interval = Duration::from_secs(1);
    let rtc = RtcConfig::new()
        .set_stats_interval(Some(stats_interval))
        .build(Instant::now());

    let mut l = TestRtc::new_with_rtc(info_span!("L"), rtc);

    let rtc_r = RtcConfig::new()
        .set_stats_interval(Some(stats_interval))
        .build(Instant::now());
    let mut r = TestRtc::new_with_rtc(info_span!("R"), rtc_r);

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

    l.set_forced_time_advance(Duration::from_millis(1));
    r.set_forced_time_advance(Duration::from_millis(1));

    // Run for 5 seconds to get multiple stats events
    loop {
        let wallclock = l.start + l.duration();
        let time = l.duration().into();
        l.writer(mid)
            .unwrap()
            .write(pt, wallclock, time, data.clone())?;
        progress(&mut l, &mut r)?;
        if l.duration() > Duration::from_secs(5) {
            break;
        }
    }

    // Should have multiple stats events with 1s interval over 5s
    let peer_stats_count = l
        .events
        .iter()
        .filter(|(_, e)| matches!(e, Event::PeerStats(_)))
        .count();

    assert!(
        peer_stats_count >= 3,
        "Expected at least 3 PeerStats events with 1s interval over 5s, got {}",
        peer_stats_count
    );

    Ok(())
}

#[test]
fn config_rtcp_report_intervals() {
    let default_config = RtcConfig::new();
    assert_eq!(
        default_config.rtcp_report_interval_audio(),
        Duration::from_secs(5)
    );
    assert_eq!(
        default_config.rtcp_report_interval_video(),
        Duration::from_secs(1)
    );

    let config = RtcConfig::new()
        .set_rtcp_report_interval_audio(Duration::from_millis(750))
        .set_rtcp_report_interval_video(Duration::from_millis(500));
    assert_eq!(
        config.rtcp_report_interval_audio(),
        Duration::from_millis(750)
    );
    assert_eq!(
        config.rtcp_report_interval_video(),
        Duration::from_millis(500)
    );
}

#[test]
#[should_panic]
fn config_rejects_zero_audio_rtcp_report_interval() {
    RtcConfig::new().set_rtcp_report_interval_audio(Duration::ZERO);
}

#[test]
#[should_panic]
fn config_rejects_zero_video_rtcp_report_interval() {
    RtcConfig::new().set_rtcp_report_interval_video(Duration::ZERO);
}

/// Test set_stats_interval(None) produces no stats events.
#[test]
fn config_stats_disabled() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let rtc = RtcConfig::new()
        .set_stats_interval(None)
        .build(Instant::now());

    let mut l = TestRtc::new_with_rtc(info_span!("L"), rtc);

    let rtc_r = RtcConfig::new()
        .set_stats_interval(None)
        .build(Instant::now());
    let mut r = TestRtc::new_with_rtc(info_span!("R"), rtc_r);

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

    l.set_forced_time_advance(Duration::from_millis(1));
    r.set_forced_time_advance(Duration::from_millis(1));

    loop {
        let wallclock = l.start + l.duration();
        let time = l.duration().into();
        l.writer(mid)
            .unwrap()
            .write(pt, wallclock, time, data.clone())?;
        progress(&mut l, &mut r)?;
        if l.duration() > Duration::from_secs(3) {
            break;
        }
    }

    let peer_stats_count = l
        .events
        .iter()
        .filter(|(_, e)| matches!(e, Event::PeerStats(_)))
        .count();

    assert_eq!(
        peer_stats_count, 0,
        "Should have no PeerStats events when stats disabled"
    );

    Ok(())
}

/// Test set_fingerprint_verification(false) allows connection with wrong fingerprint.
#[test]
fn config_fingerprint_verification_disabled() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    use str0m::Candidate;
    use str0m::crypto::Fingerprint;

    // Create RTCs with fingerprint verification DISABLED
    let rtc_l = RtcConfig::new()
        .set_fingerprint_verification(false)
        .set_rtp_mode(true)
        .build(Instant::now());

    let rtc_r = RtcConfig::new()
        .set_fingerprint_verification(false)
        .set_rtp_mode(true)
        .build(Instant::now());

    let mut l = TestRtc::new_with_rtc(info_span!("L"), rtc_l);
    let mut r = TestRtc::new_with_rtc(info_span!("R"), rtc_r);

    // Set up candidates
    let host1 = Candidate::host((Ipv4Addr::new(1, 1, 1, 1), 1000).into(), "udp").unwrap();
    let host2 = Candidate::host((Ipv4Addr::new(2, 2, 2, 2), 2000).into(), "udp").unwrap();
    l.add_local_candidate(host1.clone()).unwrap();
    l.add_remote_candidate(host2.clone());
    r.add_local_candidate(host2).unwrap();
    r.add_remote_candidate(host1);

    // Create CORRUPTED fingerprints (all zeros - definitely wrong)
    let corrupted_fingerprint = Fingerprint {
        hash_func: "sha-256".to_string(),
        bytes: vec![0u8; 32], // Wrong fingerprint!
    };

    // Set the WRONG fingerprints as remote (this would fail with verification enabled)
    l.direct_api()
        .set_remote_fingerprint(corrupted_fingerprint.clone());
    r.direct_api().set_remote_fingerprint(corrupted_fingerprint);

    // Exchange ICE credentials
    let creds_l = l.direct_api().local_ice_credentials();
    let creds_r = r.direct_api().local_ice_credentials();
    l.direct_api().set_remote_ice_credentials(creds_r);
    r.direct_api().set_remote_ice_credentials(creds_l);

    l.direct_api().set_ice_controlling(true);
    r.direct_api().set_ice_controlling(false);

    // Start DTLS - this should succeed despite wrong fingerprints
    // because verification is disabled
    l.direct_api().start_dtls(true).unwrap();
    r.direct_api().start_dtls(false).unwrap();

    l.direct_api().start_sctp(true);
    r.direct_api().start_sctp(false);

    // Connection should succeed despite corrupted fingerprints
    loop {
        if l.is_connected() && r.is_connected() {
            break;
        }
        if l.duration() > Duration::from_secs(5) {
            panic!("Failed to connect - fingerprint verification should be disabled");
        }
        progress(&mut l, &mut r)?;
    }

    // Verify we actually connected
    assert!(l.is_connected(), "L should be connected");
    assert!(r.is_connected(), "R should be connected");

    Ok(())
}

/// Test that RtcConfig can be cloned and used for multiple instances.
#[test]
fn config_clone_multiple_instances() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    // Create a config with custom settings (not ice_lite since both can't be ice_lite)
    let config = RtcConfig::new()
        .set_reordering_size_audio(20)
        .set_reordering_size_video(40);

    // Clone the config and create multiple instances
    let rtc1 = config.clone().build(Instant::now());
    let rtc2 = config.build(Instant::now());

    let mut l = TestRtc::new_with_rtc(info_span!("L"), rtc1);
    let mut r = TestRtc::new_with_rtc(info_span!("R"), rtc2);

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
            panic!("Failed to connect with cloned config");
        }
        progress(&mut l, &mut r)?;
    }

    Ok(())
}

/// Test set_send_buffer_audio and set_send_buffer_video configuration.
#[test]
fn config_send_buffer_sizes() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    // Verify config builder correctly sets and retrieves values
    let config = RtcConfig::new()
        .set_send_buffer_audio(100)
        .set_send_buffer_video(2000);

    // Verify the config has correct values before building
    assert_eq!(
        config.send_buffer_audio(),
        100,
        "Audio send buffer should be 100"
    );
    assert_eq!(
        config.send_buffer_video(),
        2000,
        "Video send buffer should be 2000"
    );

    // Verify default values are different
    let default_config = RtcConfig::new();
    assert_eq!(
        default_config.send_buffer_audio(),
        50,
        "Default audio send buffer should be 50"
    );
    assert_eq!(
        default_config.send_buffer_video(),
        1000,
        "Default video send buffer should be 1000"
    );

    // Build and verify the Rtc works with custom config
    let rtc = config.build(Instant::now());
    let mut l = TestRtc::new_with_rtc(info_span!("L"), rtc);
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
        if l.duration() > Duration::from_secs(5) {
            panic!("Failed to connect");
        }
        progress(&mut l, &mut r)?;
    }

    // Send and receive data to verify the config doesn't break functionality
    let params = l.params_opus();
    let pt = params.pt();
    let data = vec![1_u8; 80];

    for _ in 0..20 {
        let wallclock = l.start + l.duration();
        let time = l.duration().into();
        l.writer(mid)
            .unwrap()
            .write(pt, wallclock, time, data.clone())?;
        progress(&mut l, &mut r)?;
    }

    // Verify data was received
    let received_count = r
        .events
        .iter()
        .filter(|(_, e)| matches!(e, Event::MediaData(_)))
        .count();

    assert!(
        received_count > 10,
        "Should receive media data with custom send buffer config, got {}",
        received_count
    );

    Ok(())
}
