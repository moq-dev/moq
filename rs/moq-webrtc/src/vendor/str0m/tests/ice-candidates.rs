//! Tests for ICE candidate handling and configuration.

use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use str0m::RtcConfig;
use str0m::RtcError;
use str0m::ice::IceCreds;
use tracing::info_span;

mod common;
use common::{Peer, TestRtc, init_crypto_default, init_log, progress};

/// Test connection with only host candidates.
#[test]
fn ice_candidate_types_host_only() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let mut l = TestRtc::new(Peer::Left);
    let mut r = TestRtc::new(Peer::Right);

    // Add only host candidates (no srflx or relay)
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

    // Should connect using host candidates
    loop {
        if l.is_connected() && r.is_connected() {
            break;
        }
        if l.duration() > Duration::from_secs(5) {
            panic!("Failed to connect with host-only candidates");
        }
        progress(&mut l, &mut r)?;
    }

    Ok(())
}

/// Test trickle ICE - connect with relay candidates, then trickle in host candidates.
/// Verifies that ICE switches to the better (host) candidates after they're added.
#[test]
fn ice_trickle_incremental_candidates() -> Result<(), RtcError> {
    use std::net::SocketAddr;
    use str0m::{Candidate, Output};

    init_log();
    init_crypto_default();

    let mut l = TestRtc::new(Peer::Left);
    let mut r = TestRtc::new(Peer::Right);

    // Define addresses for relay and host candidates
    // Relay addresses (TURN allocated addresses, lower priority)
    let l_relay_addr: SocketAddr = (Ipv4Addr::new(10, 0, 0, 1), 10000).into();
    let r_relay_addr: SocketAddr = (Ipv4Addr::new(10, 0, 0, 2), 20000).into();

    // Local addresses (base addresses for relay candidates)
    let l_local_addr: SocketAddr = (Ipv4Addr::new(192, 168, 1, 1), 1000).into();
    let r_local_addr: SocketAddr = (Ipv4Addr::new(192, 168, 1, 2), 2000).into();

    // Host addresses (higher priority, will be trickled later)
    let l_host_addr: SocketAddr = (Ipv4Addr::new(1, 1, 1, 1), 1000).into();
    let r_host_addr: SocketAddr = (Ipv4Addr::new(2, 2, 2, 2), 2000).into();

    // Create relay candidates (lower priority than host)
    let l_relay = Candidate::relayed(l_relay_addr, l_local_addr, "udp").unwrap();
    let r_relay = Candidate::relayed(r_relay_addr, r_local_addr, "udp").unwrap();

    // Add relay candidates initially
    l.rtc.add_local_candidate(l_relay.clone()).unwrap();
    r.rtc.add_local_candidate(r_relay.clone()).unwrap();

    // Exchange relay candidates
    l.rtc.add_remote_candidate(r_relay.clone());
    r.rtc.add_remote_candidate(l_relay.clone());

    // Create offer/answer
    let (offer, pending) = l.span.in_scope(|| {
        let mut change = l.rtc.sdp_api();
        let _ = change.add_channel("test".into());
        change.apply().unwrap()
    });

    let answer = r.span.in_scope(|| r.rtc.sdp_api().accept_offer(offer))?;
    l.span
        .in_scope(|| l.rtc.sdp_api().accept_answer(pending, answer))?;

    // Connect using relay candidates
    loop {
        if l.is_connected() && r.is_connected() {
            break;
        }
        if l.duration() > Duration::from_secs(5) {
            panic!("Failed to connect with relay candidates");
        }
        progress(&mut l, &mut r)?;
    }

    // Get the data channel id
    let channel_id = l
        .events
        .iter()
        .find_map(|(_, e)| {
            if let str0m::Event::ChannelOpen(id, _) = e {
                Some(*id)
            } else {
                None
            }
        })
        .expect("Should have opened a data channel");

    // Send data to trigger a Transmit and capture the source address
    l.rtc
        .channel(channel_id)
        .unwrap()
        .write(true, b"test")
        .unwrap();

    let mut initial_send_addr: Option<SocketAddr> = None;
    l.rtc.handle_input(str0m::Input::Timeout(l.last)).unwrap();
    loop {
        match l.rtc.poll_output() {
            Ok(Output::Transmit(t)) => {
                initial_send_addr = Some(t.source);
                break;
            }
            Ok(Output::Timeout(_)) => break,
            Ok(_) => continue,
            Err(e) => return Err(e),
        }
    }

    let initial_send_addr = initial_send_addr.expect("Should have a Transmit after sending data");
    assert_eq!(
        initial_send_addr, l_relay_addr,
        "Initial send address should be relay candidate"
    );

    // Now trickle in host candidates (higher priority)
    let l_host = Candidate::host(l_host_addr, "udp").unwrap();
    let r_host = Candidate::host(r_host_addr, "udp").unwrap();

    let l_host_added = l.rtc.add_local_candidate(l_host.clone()).unwrap().clone();
    let r_host_added = r.rtc.add_local_candidate(r_host.clone()).unwrap().clone();

    // Exchange host candidates between peers (simulating trickle ICE signaling)
    l.rtc.add_remote_candidate(r_host_added);
    r.rtc.add_remote_candidate(l_host_added);

    // Progress to allow ICE to discover and switch to better candidates
    for _ in 0..100 {
        progress(&mut l, &mut r)?;
    }

    // Send data again and capture the new source address - should have switched to host
    l.rtc
        .channel(channel_id)
        .unwrap()
        .write(true, b"test2")
        .unwrap();

    let mut final_send_addr: Option<SocketAddr> = None;
    l.rtc.handle_input(str0m::Input::Timeout(l.last)).unwrap();
    loop {
        match l.rtc.poll_output() {
            Ok(Output::Transmit(t)) => {
                final_send_addr = Some(t.source);
                break;
            }
            Ok(Output::Timeout(_)) => break,
            Ok(_) => continue,
            Err(e) => return Err(e),
        }
    }

    let final_send_addr =
        final_send_addr.expect("Should have a Transmit after sending data post-trickle");

    // Verify that ICE switched from relay to host candidates
    assert_eq!(
        final_send_addr, l_host_addr,
        "After trickle, send address should switch to host candidate"
    );

    // Verify the switch actually happened
    assert_ne!(
        initial_send_addr, final_send_addr,
        "Send address should have changed from relay ({}) to host ({})",
        initial_send_addr, final_send_addr
    );

    Ok(())
}

/// Test custom ICE credentials via set_local_ice_credentials().
#[test]
fn ice_custom_credentials() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let custom_creds = IceCreds {
        ufrag: "customufrag123".into(),
        pass: "custompassword456789012".into(),
    };

    let rtc = RtcConfig::new()
        .set_local_ice_credentials(custom_creds.clone())
        .build(Instant::now());

    let mut l = TestRtc::new_with_rtc(info_span!("L"), rtc);
    let mut r = TestRtc::new(Peer::Right);

    l.add_host_candidate((Ipv4Addr::new(1, 1, 1, 1), 1000).into());
    r.add_host_candidate((Ipv4Addr::new(2, 2, 2, 2), 2000).into());

    // Verify custom credentials are used
    let actual_creds = l._local_ice_creds();
    assert_eq!(
        actual_creds.ufrag, custom_creds.ufrag,
        "Custom ufrag should be used"
    );
    assert_eq!(
        actual_creds.pass, custom_creds.pass,
        "Custom password should be used"
    );

    let (offer, pending) = l.span.in_scope(|| {
        let mut change = l.rtc.sdp_api();
        let _ = change.add_channel("test".into());
        change.apply().unwrap()
    });

    // Verify custom ufrag appears in offer SDP
    let offer_str = offer.to_string();
    assert!(
        offer_str.contains(&custom_creds.ufrag),
        "Offer SDP should contain custom ufrag"
    );

    let answer = r.span.in_scope(|| r.rtc.sdp_api().accept_offer(offer))?;
    l.span
        .in_scope(|| l.rtc.sdp_api().accept_answer(pending, answer))?;

    loop {
        if l.is_connected() && r.is_connected() {
            break;
        }
        if l.duration() > Duration::from_secs(5) {
            panic!("Failed to connect with custom credentials");
        }
        progress(&mut l, &mut r)?;
    }

    Ok(())
}

/// Test set_initial_stun_rto() configuration by measuring retransmission timing.
/// Uses a short RTO and verifies retransmissions happen at the expected interval.
#[test]
fn ice_stun_timeout_initial_rto() -> Result<(), RtcError> {
    use str0m::Output;

    init_log();
    init_crypto_default();

    // Set a short initial RTO of 50ms (default is 250ms)
    let custom_rto = Duration::from_millis(50);
    let mut config = RtcConfig::new();
    config.set_initial_stun_rto(custom_rto);
    let start = Instant::now();
    let rtc = config.build(start);

    let mut l = TestRtc::new_with_rtc(info_span!("L"), rtc);
    let mut r = TestRtc::new(Peer::Right);

    // Sync TestRtc time with Rtc creation time
    l.start = start;
    l.last = start;

    l.add_host_candidate((Ipv4Addr::new(1, 1, 1, 1), 1000).into());
    r.add_host_candidate((Ipv4Addr::new(2, 2, 2, 2), 2000).into());

    // Set up SDP exchange
    let (offer, pending) = l.span.in_scope(|| {
        let mut change = l.rtc.sdp_api();
        let _ = change.add_channel("test".into());
        change.apply().unwrap()
    });

    let answer = r.span.in_scope(|| r.rtc.sdp_api().accept_offer(offer))?;
    l.span
        .in_scope(|| l.rtc.sdp_api().accept_answer(pending, answer))?;

    // Track transmit times from L only (don't deliver to R, so L will retransmit)
    let mut transmit_times: Vec<Instant> = Vec::new();

    // Progress L only, capture STUN transmit times
    for _ in 0..30 {
        l.rtc.handle_input(str0m::Input::Timeout(l.last)).unwrap();

        loop {
            match l.rtc.poll_output()? {
                Output::Transmit(_) => {
                    transmit_times.push(l.last);
                }
                Output::Timeout(t) => {
                    l.last = t;
                    break;
                }
                Output::Event(_) => {}
            }
        }

        // Stop once we have enough samples
        if transmit_times.len() >= 3 {
            break;
        }
    }

    assert!(
        transmit_times.len() >= 2,
        "Should have at least 2 STUN transmissions, got {}",
        transmit_times.len()
    );

    // Check the interval between first and second transmit matches initial RTO
    let first_interval = transmit_times[1].duration_since(transmit_times[0]);

    // Allow some tolerance (35-65ms for 50ms RTO)
    let min_expected = custom_rto - Duration::from_millis(15);
    let max_expected = custom_rto + Duration::from_millis(15);

    assert!(
        first_interval >= min_expected && first_interval <= max_expected,
        "First retransmit interval should be ~{}ms (initial RTO), got {}ms",
        custom_rto.as_millis(),
        first_interval.as_millis()
    );

    Ok(())
}

/// Test set_max_stun_rto() configuration by verifying retransmit intervals are capped.
#[test]
fn ice_stun_timeout_max_rto() -> Result<(), RtcError> {
    use str0m::Output;

    init_log();
    init_crypto_default();

    // Set initial RTO to 100ms and max RTO to 150ms
    // Without max cap, RTO would double: 100 -> 200 -> 400...
    // With max 150ms, it should cap at: 100 -> 150 -> 150...
    let initial_rto = Duration::from_millis(100);
    let max_rto = Duration::from_millis(150);

    let mut config = RtcConfig::new();
    config.set_initial_stun_rto(initial_rto);
    config.set_max_stun_rto(max_rto);
    let start = Instant::now();
    let rtc = config.build(start);

    let mut l = TestRtc::new_with_rtc(info_span!("L"), rtc);
    let mut r = TestRtc::new(Peer::Right);

    // Sync TestRtc time with Rtc creation time
    l.start = start;
    l.last = start;

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

    // Track transmit times from L only (don't deliver to R)
    let mut transmit_times: Vec<Instant> = Vec::new();

    for _ in 0..50 {
        l.rtc.handle_input(str0m::Input::Timeout(l.last)).unwrap();

        loop {
            match l.rtc.poll_output()? {
                Output::Transmit(_) => {
                    transmit_times.push(l.last);
                }
                Output::Timeout(t) => {
                    l.last = t;
                    break;
                }
                Output::Event(_) => {}
            }
        }

        if transmit_times.len() >= 4 {
            break;
        }
    }

    assert!(
        transmit_times.len() >= 4,
        "Should have at least 4 transmissions, got {}",
        transmit_times.len()
    );

    // Check intervals - first should be ~100ms, subsequent should be capped at ~150ms
    let interval_1_2 = transmit_times[1].duration_since(transmit_times[0]);
    let interval_2_3 = transmit_times[2].duration_since(transmit_times[1]);
    let interval_3_4 = transmit_times[3].duration_since(transmit_times[2]);

    // First interval should be around initial RTO (100ms)
    assert!(
        interval_1_2 >= Duration::from_millis(85) && interval_1_2 <= Duration::from_millis(115),
        "First interval should be ~100ms (initial RTO), got {}ms",
        interval_1_2.as_millis()
    );

    // Later intervals should be capped at max RTO (150ms), not doubled (200ms)
    // Allow tolerance for timing
    let max_allowed = max_rto + Duration::from_millis(20);
    assert!(
        interval_2_3 <= max_allowed,
        "Second interval should be capped at ~{}ms (max RTO), got {}ms",
        max_rto.as_millis(),
        interval_2_3.as_millis()
    );
    assert!(
        interval_3_4 <= max_allowed,
        "Third interval should be capped at ~{}ms (max RTO), got {}ms",
        max_rto.as_millis(),
        interval_3_4.as_millis()
    );

    Ok(())
}

/// Test set_max_stun_retransmits() configuration by counting actual retransmissions.
#[test]
fn ice_stun_max_retransmits() -> Result<(), RtcError> {
    use str0m::Output;

    init_log();
    init_crypto_default();

    // Set max retransmits to 3 (default is 9) and short RTOs for faster test
    let max_retransmits = 3;
    let mut config = RtcConfig::new();
    config.set_max_stun_retransmits(max_retransmits);
    config.set_initial_stun_rto(Duration::from_millis(20));
    config.set_max_stun_rto(Duration::from_millis(40));
    let start = Instant::now();
    let rtc = config.build(start);

    let mut l = TestRtc::new_with_rtc(info_span!("L"), rtc);
    let mut r = TestRtc::new(Peer::Right);

    // Sync TestRtc time with Rtc creation time
    l.start = start;
    l.last = start;

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

    // Count transmissions from L (without delivering to R)
    let mut transmit_count = 0;

    for _ in 0..100 {
        l.rtc.handle_input(str0m::Input::Timeout(l.last)).unwrap();

        loop {
            match l.rtc.poll_output()? {
                Output::Transmit(_) => {
                    transmit_count += 1;
                }
                Output::Timeout(t) => {
                    l.last = t;
                    break;
                }
                Output::Event(_) => {}
            }
        }

        // Give enough time for all retransmits to happen
        if l.duration() > Duration::from_millis(500) {
            break;
        }
    }

    // With max_retransmits=3, we should see limited retransmissions
    // The exact count depends on implementation details, but should be bounded
    // Key verification: count should be <= max_retransmits + 1 (initial + retransmits)
    let expected_max = max_retransmits + 1 + 2; // +2 for timing tolerance

    assert!(
        transmit_count <= expected_max,
        "Transmit count should be bounded by max_retransmits setting. \
         Expected at most {} (initial + {} retransmits + tolerance), got {}",
        expected_max,
        max_retransmits,
        transmit_count
    );

    // Should have at least some transmissions
    assert!(
        transmit_count >= 2,
        "Should have at least 2 transmissions (initial + at least 1 retransmit), got {}",
        transmit_count
    );

    Ok(())
}

/// Test ICE lite mode connectivity.
#[test]
fn ice_lite_mode() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let mut l = TestRtc::new(Peer::Left);
    let rtc = RtcConfig::new().set_ice_lite(true).build(Instant::now());
    let mut r = TestRtc::new_with_rtc(info_span!("R"), rtc);

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
            panic!("Failed to connect with ICE lite");
        }
        progress(&mut l, &mut r)?;
    }

    Ok(())
}
