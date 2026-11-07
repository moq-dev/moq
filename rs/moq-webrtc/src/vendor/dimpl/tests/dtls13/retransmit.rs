//! DTLS 1.3 retransmission tests.

use std::sync::Arc;
use std::time::{Duration, Instant};

use dimpl::{Config, Dtls};

use crate::common::*;

#[test]
#[cfg(feature = "rcgen")]
fn dtls13_client_retransmits_on_timeout() {
    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    let config = dtls13_config();

    let mut now = Instant::now();

    let mut client = Dtls::new_13(Arc::clone(&config), client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_13(config, server_cert, now);
    server.set_active(false);

    // Get initial ClientHello
    client.handle_timeout(now).expect("client start");
    client.handle_timeout(now).expect("client arm");
    let initial_packets = collect_packets(&mut client);
    assert!(
        !initial_packets.is_empty(),
        "Client should send ClientHello"
    );

    // Don't deliver to server, trigger timeout
    trigger_timeout(&mut client, &mut now);

    // Should get retransmitted packets
    let retransmit_packets = collect_packets(&mut client);
    assert!(
        !retransmit_packets.is_empty(),
        "Client should retransmit on timeout"
    );

    // Retransmit should have same number of packets (same flight)
    assert_eq!(
        initial_packets.len(),
        retransmit_packets.len(),
        "Retransmit should have same packet count"
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls13_handshake_completes_after_packet_loss() {
    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    let config = dtls13_config();

    let mut now = Instant::now();

    let mut client = Dtls::new_13(Arc::clone(&config), client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_13(config, server_cert, now);
    server.set_active(false);

    let mut client_connected = false;
    let mut server_connected = false;
    let mut drop_next_client_packet = true; // Drop first ClientHello

    for i in 0..60 {
        client.handle_timeout(now).expect("client timeout");
        server.handle_timeout(now).expect("server timeout");

        let client_out = drain_outputs(&mut client);
        let server_out = drain_outputs(&mut server);

        if client_out.connected {
            client_connected = true;
        }
        if server_out.connected {
            server_connected = true;
        }

        // Simulate packet loss: drop first client packet
        if !client_out.packets.is_empty() && drop_next_client_packet {
            drop_next_client_packet = false;
            // Don't deliver client packets this round
        } else {
            deliver_packets(&client_out.packets, &mut server);
        }

        deliver_packets(&server_out.packets, &mut client);

        if client_connected && server_connected {
            break;
        }

        // Advance time to trigger retransmissions
        if i % 5 == 4 {
            now += Duration::from_secs(2);
        } else {
            now += Duration::from_millis(10);
        }
    }

    assert!(
        client_connected,
        "Client should connect despite initial packet loss"
    );
    assert!(
        server_connected,
        "Server should connect despite initial packet loss"
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls13_handshake_completes_with_early_packet_loss() {
    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    // Use a config with more retries to handle packet loss
    let config = Arc::new(
        Config::builder()
            .flight_retries(8)
            .build()
            .expect("Failed to build DTLS 1.3 config"),
    );

    let mut now = Instant::now();

    let mut client = Dtls::new_13(Arc::clone(&config), client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_13(config, server_cert, now);
    server.set_active(false);

    let mut client_connected = false;
    let mut server_connected = false;

    // Drop first 2 client packets and first 2 server packets to test retransmission
    let mut client_packets_to_drop = 2;
    let mut server_packets_to_drop = 2;

    for i in 0..60 {
        client.handle_timeout(now).expect("client timeout");
        server.handle_timeout(now).expect("server timeout");

        let client_out = drain_outputs(&mut client);
        let server_out = drain_outputs(&mut server);

        if client_out.connected {
            client_connected = true;
        }
        if server_out.connected {
            server_connected = true;
        }

        // Deliver client packets, dropping first N
        for packet in &client_out.packets {
            if client_packets_to_drop > 0 {
                client_packets_to_drop -= 1;
            } else {
                let _ = server.handle_packet(packet);
            }
        }

        // Deliver server packets, dropping first N
        for packet in &server_out.packets {
            if server_packets_to_drop > 0 {
                server_packets_to_drop -= 1;
            } else {
                let _ = client.handle_packet(packet);
            }
        }

        if client_connected && server_connected {
            break;
        }

        // Trigger retransmissions periodically
        if i % 5 == 4 {
            now += Duration::from_secs(2);
        } else {
            now += Duration::from_millis(10);
        }
    }

    assert!(
        client_connected,
        "Client should connect despite early packet loss"
    );
    assert!(
        server_connected,
        "Server should connect despite early packet loss"
    );
}

/// Test packet loss on both directions simultaneously (moderate loss rate)
/// Uses a deterministic drop pattern: drop packets only in specific rounds,
/// ensuring retransmissions in later rounds get through.
#[test]
#[cfg(feature = "rcgen")]
fn dtls13_handles_bidirectional_packet_loss() {
    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    let config = dtls13_config_with_mtu(220);

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    let mut now = Instant::now();

    let mut client = Dtls::new_13(Arc::clone(&config), client_cert, now);
    client.set_active(true);
    let mut server = Dtls::new_13(config, server_cert, now);
    server.set_active(false);

    let mut client_connected = false;
    let mut server_connected = false;
    let mut dropped_client = 0;
    let mut dropped_server = 0;
    let mut total_client_packets = 0;
    let mut total_server_packets = 0;

    // Run for plenty of rounds to allow retransmissions
    for round in 0..300 {
        client.handle_timeout(now).expect("client timeout");
        server.handle_timeout(now).expect("server timeout");

        let client_out = drain_outputs(&mut client);
        let server_out = drain_outputs(&mut server);

        // Drop pattern: drop every other packet, but only in rounds 0-4 and 8-12
        // This simulates burst loss with recovery windows
        let is_loss_window = round < 5 || (8..13).contains(&round);

        for (i, p) in client_out.packets.iter().enumerate() {
            total_client_packets += 1;
            // Drop odd-indexed packets during loss windows
            if is_loss_window && i % 2 == 1 {
                dropped_client += 1;
            } else {
                let _ = server.handle_packet(p);
            }
        }

        for (i, p) in server_out.packets.iter().enumerate() {
            total_server_packets += 1;
            // Drop even-indexed packets during loss windows (different pattern)
            if is_loss_window && i % 2 == 0 && server_out.packets.len() > 1 {
                dropped_server += 1;
            } else {
                let _ = client.handle_packet(p);
            }
        }

        client_connected |= client_out.connected;
        server_connected |= server_out.connected;

        if client_connected && server_connected {
            break;
        }

        now += Duration::from_millis(20);
    }

    assert!(
        client_connected,
        "Client should connect despite bidirectional loss"
    );
    assert!(
        server_connected,
        "Server should connect despite bidirectional loss"
    );

    // Verify we actually dropped some packets
    assert!(
        dropped_client > 0 || dropped_server > 0,
        "Test should have dropped some packets"
    );

    eprintln!(
        concat!(
            "SUCCESS: Handshake completed with bidirectional loss. Dropped: ",
            "client→server={}/{}, server→client={}/{}"
        ),
        dropped_client, total_client_packets, dropped_server, total_server_packets
    );
}

/// Test random packet loss pattern (chaos test)
#[test]
#[cfg(feature = "rcgen")]
fn dtls13_survives_random_packet_loss_pattern() {
    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    let config = dtls13_config_with_mtu(220);

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    let mut now = Instant::now();

    let mut client = Dtls::new_13(Arc::clone(&config), client_cert, now);
    client.set_active(true);
    let mut server = Dtls::new_13(config, server_cert, now);
    server.set_active(false);

    let mut client_connected = false;
    let mut server_connected = false;
    let mut total_dropped = 0;
    let mut total_delivered = 0;

    // Deterministic "random-like" loss pattern
    // Drop only specific packets that won't kill the handshake
    let should_drop = |round: usize, packet_idx: usize| -> bool {
        // Only drop on certain rounds, and only if there are multiple packets
        // This ensures we don't drop critical single-packet flights
        round > 2 && round.is_multiple_of(7) && packet_idx == 0
    };

    for round in 0..100 {
        client.handle_timeout(now).expect("client timeout");
        server.handle_timeout(now).expect("server timeout");

        let client_out = drain_outputs(&mut client);
        let server_out = drain_outputs(&mut server);

        // Deliver with controlled drops
        for (i, p) in client_out.packets.iter().enumerate() {
            if !should_drop(round, i) || client_out.packets.len() == 1 {
                let _ = server.handle_packet(p);
                total_delivered += 1;
            } else {
                total_dropped += 1;
            }
        }

        for (i, p) in server_out.packets.iter().enumerate() {
            if !should_drop(round, i) || server_out.packets.len() == 1 {
                let _ = client.handle_packet(p);
                total_delivered += 1;
            } else {
                total_dropped += 1;
            }
        }

        client_connected |= client_out.connected;
        server_connected |= server_out.connected;

        if client_connected && server_connected {
            break;
        }

        now += Duration::from_millis(30);
    }

    assert!(client_connected, "Client should eventually connect");
    assert!(server_connected, "Server should eventually connect");

    let drop_rate = if total_dropped + total_delivered > 0 {
        total_dropped as f64 / (total_dropped + total_delivered) as f64 * 100.0
    } else {
        0.0
    };
    eprintln!(
        "SUCCESS: Handshake completed with controlled loss. Dropped: {}, Delivered: {}, Drop rate: {:.1}%",
        total_dropped, total_delivered, drop_rate
    );
}

/// Test selective retransmit: verify that only unACKed records are retransmitted.
/// This test carefully controls packet delivery to verify the actual retransmit behavior.
#[test]
#[cfg(feature = "rcgen")]
fn dtls13_selective_retransmit_only_missing_records() {
    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    fn count_epoch2_records(packet: &[u8]) -> usize {
        let mut i = 0usize;
        let mut count = 0usize;
        while i < packet.len() {
            let b0 = packet[i];
            if (b0 & 0b1110_0000) == 0b0010_0000 {
                let c = (b0 & 0b0001_0000) != 0;
                let s16 = (b0 & 0b0000_1000) != 0;
                let l = (b0 & 0b0000_0100) != 0;
                let epoch_bits = b0 & 0b0000_0011;
                if c {
                    break;
                }
                let mut header_len = 1 + if s16 { 2 } else { 1 };
                if l {
                    header_len += 2;
                }
                if i + header_len > packet.len() {
                    break;
                }
                let ciphertext_len = if l {
                    let off = i + 1 + if s16 { 2 } else { 1 };
                    u16::from_be_bytes([packet[off], packet[off + 1]]) as usize
                } else {
                    packet.len() - (i + header_len)
                };
                if epoch_bits == 2 {
                    count += 1;
                }
                i += header_len.saturating_add(ciphertext_len);
                continue;
            }
            if i + 13 > packet.len() {
                break;
            }
            let len = u16::from_be_bytes([packet[i + 11], packet[i + 12]]) as usize;
            i += 13 + len;
        }
        count
    }

    const ATTEMPTS: usize = 12;

    let mut success = None;
    let mut attempts_with_drop = 0usize;
    let mut attempts_with_retransmit = 0usize;
    let mut attempts_with_client_epoch2 = 0usize;
    let mut attempts_connected = 0usize;

    for attempt in 0..ATTEMPTS {
        // Small MTU to force multi-packet flights.
        // Vary deterministic seeds and dropped epoch-2 index across attempts so
        // we exercise different fragmentation layouts without relying on runtime RNG.
        let client_config = Arc::new(
            Config::builder()
                .mtu(220)
                .dangerously_set_rng_seed(0xC1A0_C1A0u64.wrapping_add(attempt as u64 * 17))
                .build()
                .expect("Failed to build DTLS 1.3 client config"),
        );
        let server_config = Arc::new(
            Config::builder()
                .mtu(220)
                .dangerously_set_rng_seed(0x5E8E_5E8Eu64.wrapping_add(attempt as u64 * 29))
                .build()
                .expect("Failed to build DTLS 1.3 server config"),
        );

        let client_cert = generate_self_signed_certificate().expect("gen client cert");
        let server_cert = generate_self_signed_certificate().expect("gen server cert");

        let mut now = Instant::now();

        let mut client = Dtls::new_13(Arc::clone(&client_config), client_cert, now);
        client.set_active(true);
        let mut server = Dtls::new_13(server_config, server_cert, now);
        server.set_active(false);

        let mut dropped_packet: Option<Vec<u8>> = None;
        let mut original_flight_size = 0usize;
        let mut saw_any_retransmit = false;
        let mut selective_retransmit_flight_size = None;
        let mut delivered_client_epoch2_after_drop = 0usize;
        let mut client_connected = false;
        let mut server_connected = false;

        for round in 0..220 {
            client.handle_timeout(now).expect("client timeout");
            server.handle_timeout(now).expect("server timeout");

            let client_out = drain_outputs(&mut client);
            let server_out = drain_outputs(&mut server);

            client_connected |= client_out.connected;
            server_connected |= server_out.connected;

            for p in &client_out.packets {
                if dropped_packet.is_some() && count_epoch2_records(p) > 0 {
                    delivered_client_epoch2_after_drop += 1;
                }
                let _ = server.handle_packet(p);
            }

            // Phase 1: Find a multi-packet epoch-2 flight and drop one packet.
            if dropped_packet.is_none() {
                let epoch2_indices: Vec<usize> = server_out
                    .packets
                    .iter()
                    .enumerate()
                    .filter(|(_, p)| count_epoch2_records(p) > 0)
                    .map(|(i, _)| i)
                    .collect();

                if epoch2_indices.len() >= 3 {
                    original_flight_size = epoch2_indices.len();

                    let drop_epoch2_idx = attempt % epoch2_indices.len();
                    let drop_packet_idx = epoch2_indices[drop_epoch2_idx];
                    dropped_packet = Some(server_out.packets[drop_packet_idx].clone());

                    for (i, p) in server_out.packets.iter().enumerate() {
                        if i != drop_packet_idx {
                            let _ = client.handle_packet(p);
                        }
                    }

                    eprintln!(
                        "Attempt {} Round {}: Dropped packet {} of {}",
                        attempt, round, drop_epoch2_idx, original_flight_size
                    );
                } else {
                    deliver_packets(&server_out.packets, &mut client);
                }
            }
            // Phase 2: After dropping, wait for retransmit and count packets.
            // The first resend can be full-flight (dupe-triggered), so keep waiting.
            else if selective_retransmit_flight_size.is_none() {
                let epoch2_packets: Vec<&Vec<u8>> = server_out
                    .packets
                    .iter()
                    .filter(|p| count_epoch2_records(p) > 0)
                    .collect();

                if !epoch2_packets.is_empty() {
                    let retransmit_flight_size = epoch2_packets.len();
                    saw_any_retransmit = true;

                    eprintln!(
                        "Attempt {} Round {}: Retransmit flight has {} packets (original had {})",
                        attempt, round, retransmit_flight_size, original_flight_size
                    );

                    if retransmit_flight_size < original_flight_size {
                        selective_retransmit_flight_size = Some(retransmit_flight_size);
                    } else {
                        eprintln!(
                            "Attempt {} Round {}: Full-flight resend observed before selective resend",
                            attempt, round
                        );
                    }
                }

                deliver_packets(&server_out.packets, &mut client);
            } else {
                deliver_packets(&server_out.packets, &mut client);
            }

            if selective_retransmit_flight_size.is_some() && client_connected && server_connected {
                break;
            }

            now += Duration::from_millis(150);
        }

        if dropped_packet.is_some() {
            attempts_with_drop += 1;
        }
        if saw_any_retransmit {
            attempts_with_retransmit += 1;
        }
        if delivered_client_epoch2_after_drop > 0 {
            attempts_with_client_epoch2 += 1;
        }
        if client_connected && server_connected {
            attempts_connected += 1;
        }

        if let Some(retransmit_flight_size) = selective_retransmit_flight_size
            && client_connected
            && server_connected
        {
            success = Some((attempt, original_flight_size, retransmit_flight_size));
            break;
        }
    }

    let Some((attempt, original_flight_size, retransmit_flight_size)) = success else {
        panic!(
            "Did not observe selective retransmit across {} attempts \
             (drop={}, retransmit={}, client_epoch2_after_drop={}, connected={})",
            ATTEMPTS,
            attempts_with_drop,
            attempts_with_retransmit,
            attempts_with_client_epoch2,
            attempts_connected
        );
    };

    eprintln!(
        "SUCCESS: Selective retransmit verified on attempt {}. \
         Original flight: {} packets, Retransmit: {} packets",
        attempt, original_flight_size, retransmit_flight_size
    );
}

/// Test that retransmission timeouts increase exponentially.
/// Start a handshake but never deliver packets to the server.
/// Record each timeout value and verify they increase monotonically.
#[test]
#[cfg(feature = "rcgen")]
fn dtls13_retransmit_exponential_backoff() {
    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    // Use enough retries to observe several backoff steps
    let config = Arc::new(
        Config::builder()
            .dangerously_set_rng_seed(42)
            .flight_retries(6)
            .handshake_timeout(Duration::from_secs(300))
            .build()
            .expect("Failed to build config"),
    );

    let mut now = Instant::now();

    let mut client = Dtls::new_13(Arc::clone(&config), client_cert, now);
    client.set_active(true);

    let _server = Dtls::new_13(config, server_cert, now);

    // Kick off the handshake
    client.handle_timeout(now).expect("client start");
    client.handle_timeout(now).expect("client arm");

    // Collect initial packets (ClientHello) without delivering them
    let _initial_packets = collect_packets(&mut client);

    // Record successive timeout values by triggering retransmissions
    let mut timeouts: Vec<Duration> = Vec::new();

    for _ in 0..6 {
        // Drain to get the current timeout instant
        let out = drain_outputs(&mut client);
        let timeout_instant = out.timeout.expect("Should have a timeout scheduled");

        let wait = timeout_instant.duration_since(now);
        timeouts.push(wait);

        // Advance past the timeout to trigger retransmission
        now = timeout_instant;
        client.handle_timeout(now).expect("client timeout");

        // Consume retransmitted packets without delivering
        let _retransmit = collect_packets(&mut client);
    }

    // Verify we collected multiple timeout values
    assert!(
        timeouts.len() >= 4,
        "Should have at least 4 timeout values, got {}",
        timeouts.len()
    );

    // Verify each timeout is larger than the previous (exponential backoff)
    for i in 1..timeouts.len() {
        assert!(
            timeouts[i] > timeouts[i - 1],
            "Timeout {} ({:?}) should be larger than timeout {} ({:?})",
            i,
            timeouts[i],
            i - 1,
            timeouts[i - 1]
        );
    }

    for (attempt, timeout) in timeouts.iter().enumerate() {
        let nominal = Duration::from_secs(1 << attempt);
        assert!(
            *timeout >= nominal.mul_f64(0.75) && *timeout <= nominal.mul_f64(1.25),
            "Timeout {attempt} ({timeout:?}) must be within +/-25% of {nominal:?}"
        );
    }

    eprintln!(
        "SUCCESS: Exponential backoff verified. Timeouts: {:?}",
        timeouts
    );
}

/// Test that when the server's ACK is lost, the client falls back to its
/// retransmission timer and the handshake still completes.
#[test]
#[cfg(feature = "rcgen")]
fn dtls13_ack_loss_falls_back_to_timer() {
    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    let config = Arc::new(
        Config::builder()
            .flight_retries(8)
            .build()
            .expect("Failed to build config"),
    );

    let mut now = Instant::now();

    let mut client = Dtls::new_13(Arc::clone(&config), client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_13(config, server_cert, now);
    server.set_active(false);

    let mut client_connected = false;
    let mut server_connected = false;
    let mut client_final_flight_seen = false;
    let mut ack_dropped = false;

    for round in 0..100 {
        client.handle_timeout(now).expect("client timeout");
        server.handle_timeout(now).expect("server timeout");

        let client_out = drain_outputs(&mut client);
        let server_out = drain_outputs(&mut server);

        client_connected |= client_out.connected;
        server_connected |= server_out.connected;

        // Always deliver client packets to server
        deliver_packets(&client_out.packets, &mut server);

        // Track the handshake phases:
        // Once the client has sent packets after receiving the server's flight,
        // the next server response is likely an ACK — drop it once.
        if client_final_flight_seen && !ack_dropped && !server_out.packets.is_empty() {
            // Drop this server response (the ACK)
            ack_dropped = true;
            eprintln!("Round {}: Dropped server ACK", round);
        } else {
            deliver_packets(&server_out.packets, &mut client);
        }

        // Detect client's final flight (client sends after having received server's flight)
        if !client_final_flight_seen && !client_out.packets.is_empty() && round > 2 {
            client_final_flight_seen = true;
        }

        if client_connected && server_connected {
            eprintln!(
                "Round {}: Both connected (ack_dropped={})",
                round, ack_dropped
            );
            break;
        }

        // Advance time: use larger steps periodically to trigger retransmissions
        if round % 5 == 4 {
            now += Duration::from_secs(2);
        } else {
            now += Duration::from_millis(10);
        }
    }

    assert!(client_connected, "Client should connect despite ACK loss");
    assert!(server_connected, "Server should connect despite ACK loss");
    assert!(ack_dropped, "Test should have dropped an ACK packet");
}

/// Test retransmission during a HelloRetryRequest flow.
/// The default DTLS 1.3 config triggers HRR (via cookie). Drop packets during
/// the HRR exchange and verify the handshake still completes via retransmission.
#[test]
#[cfg(feature = "rcgen")]
fn dtls13_retransmit_during_hrr_flow() {
    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    let config = Arc::new(
        Config::builder()
            .flight_retries(8)
            .build()
            .expect("Failed to build config"),
    );

    let mut now = Instant::now();

    let mut client = Dtls::new_13(Arc::clone(&config), client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_13(config, server_cert, now);
    server.set_active(false);

    let mut client_connected = false;
    let mut server_connected = false;
    let mut client_flight_count = 0;
    let mut dropped_server_hrr = false;

    for round in 0..100 {
        client.handle_timeout(now).expect("client timeout");
        server.handle_timeout(now).expect("server timeout");

        let client_out = drain_outputs(&mut client);
        let server_out = drain_outputs(&mut server);

        client_connected |= client_out.connected;
        server_connected |= server_out.connected;

        if !client_out.packets.is_empty() {
            client_flight_count += 1;
        }

        // Deliver client packets to server
        deliver_packets(&client_out.packets, &mut server);

        // Drop the first server response (the HRR) to force retransmission
        if !dropped_server_hrr && !server_out.packets.is_empty() && client_flight_count <= 1 {
            dropped_server_hrr = true;
            eprintln!("Round {}: Dropped server HRR response", round);
            // Don't deliver — force client to retransmit and server to re-send HRR
        } else {
            deliver_packets(&server_out.packets, &mut client);
        }

        if client_connected && server_connected {
            break;
        }

        // Advance time: larger steps periodically to trigger retransmissions
        if round % 5 == 4 {
            now += Duration::from_secs(2);
        } else {
            now += Duration::from_millis(10);
        }
    }

    assert!(
        client_connected,
        "Client should connect despite HRR packet loss"
    );
    assert!(
        server_connected,
        "Server should connect despite HRR packet loss"
    );
    assert!(
        dropped_server_hrr,
        "Test should have dropped the HRR response"
    );
}

/// Test that a short handshake timeout causes the client to abort.
/// Configure a 5-second handshake timeout, never deliver any packets,
/// and verify the client eventually returns a timeout error.
#[test]
#[cfg(feature = "rcgen")]
fn dtls13_handshake_timeout_aborts() {
    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    let config = Arc::new(
        Config::builder()
            .handshake_timeout(Duration::from_secs(5))
            .flight_retries(10) // plenty of retries so we hit connect timeout, not flight exhaustion
            .flight_start_rto(Duration::from_millis(200))
            .build()
            .expect("Failed to build config"),
    );

    let mut now = Instant::now();
    let start = now;

    let mut client = Dtls::new_13(Arc::clone(&config), client_cert, now);
    client.set_active(true);

    let _server = Dtls::new_13(config, server_cert, now);

    // Kick off the handshake
    client.handle_timeout(now).expect("client start");
    client.handle_timeout(now).expect("client arm");
    let _ = collect_packets(&mut client);

    let mut got_timeout_error = false;

    for _ in 0..200 {
        // Drain to find the next timeout
        let out = drain_outputs(&mut client);
        let Some(timeout_instant) = out.timeout else {
            break;
        };

        // Advance to the timeout
        now = timeout_instant;

        match client.handle_timeout(now) {
            Ok(()) => {
                // Consume retransmitted packets without delivering
                let _ = collect_packets(&mut client);
            }
            Err(e) => {
                let msg = format!("{}", e);
                assert!(
                    msg.contains("timeout"),
                    "Expected timeout error, got: {}",
                    msg
                );
                got_timeout_error = true;
                break;
            }
        }
    }

    assert!(
        got_timeout_error,
        "Client should have aborted with a timeout error"
    );

    let elapsed = now.duration_since(start);
    assert!(
        elapsed <= Duration::from_secs(10),
        "Timeout should have fired within a reasonable time, took {:?}",
        elapsed
    );

    eprintln!(
        "SUCCESS: Handshake aborted after {:?} with timeout error",
        elapsed
    );
}
