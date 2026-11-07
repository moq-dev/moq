//! DTLS 1.3 packet reordering and duplicate tests.

use std::sync::Arc;
use std::time::{Duration, Instant};

use dimpl::Dtls;

use crate::common::*;

#[test]
#[cfg(feature = "rcgen")]
fn dtls13_handles_duplicate_packets() {
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

    for _ in 0..40 {
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

        // Deliver packets twice (simulating duplicates)
        deliver_packets(&client_out.packets, &mut server);
        deliver_packets(&client_out.packets, &mut server); // Duplicate!

        deliver_packets(&server_out.packets, &mut client);
        deliver_packets(&server_out.packets, &mut client); // Duplicate!

        if client_connected && server_connected {
            break;
        }

        now += Duration::from_millis(10);
    }

    assert!(
        client_connected,
        "Client should connect despite duplicate packets"
    );
    assert!(
        server_connected,
        "Server should connect despite duplicate packets"
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls13_handles_out_of_order_packets() {
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

    for _ in 0..40 {
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

        // Deliver packets in reverse order
        let mut client_packets = client_out.packets.clone();
        let mut server_packets = server_out.packets.clone();
        client_packets.reverse();
        server_packets.reverse();

        deliver_packets(&client_packets, &mut server);
        deliver_packets(&server_packets, &mut client);

        if client_connected && server_connected {
            break;
        }

        now += Duration::from_millis(10);
    }

    assert!(
        client_connected,
        "Client should connect with out-of-order packets"
    );
    assert!(
        server_connected,
        "Server should connect with out-of-order packets"
    );
}

/// Test severely reordered packets - deliver packets in reverse order
/// Uses deterministic reordering pattern with sufficient rounds for retransmissions.
#[test]
#[cfg(feature = "rcgen")]
fn dtls13_handles_severely_reordered_packets() {
    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    // Use default MTU - we'll accumulate packets for reordering
    let config = dtls13_config();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    let mut now = Instant::now();

    let mut client = Dtls::new_13(Arc::clone(&config), client_cert, now);
    client.set_active(true);
    let mut server = Dtls::new_13(config, server_cert, now);
    server.set_active(false);
    let mut client_connected = false;
    let mut server_connected = false;

    // Buffer to hold packets for reordering
    let mut server_buffer: Vec<Vec<u8>> = Vec::new();
    let mut packets_reordered = 0;

    // Use many rounds with very small time steps for reliability
    for round in 0..500 {
        client.handle_timeout(now).expect("client timeout");
        server.handle_timeout(now).expect("server timeout");

        let client_out = drain_outputs(&mut client);
        let server_out = drain_outputs(&mut server);

        // Deliver client packets normally
        deliver_packets(&client_out.packets, &mut server);

        // Accumulate server packets
        server_buffer.extend(server_out.packets);

        // Every 5 rounds or when we have accumulated enough, deliver in reverse order
        if (round % 5 == 4 || server_buffer.len() >= 3) && !server_buffer.is_empty() {
            packets_reordered += server_buffer.len();
            for p in server_buffer.iter().rev() {
                let _ = client.handle_packet(p);
            }
            server_buffer.clear();
        }

        client_connected |= client_out.connected;
        server_connected |= server_out.connected;

        if client_connected && server_connected {
            // Deliver any remaining buffered packets
            for p in server_buffer.iter().rev() {
                let _ = client.handle_packet(p);
            }
            break;
        }

        now += Duration::from_millis(10);
    }

    assert!(client_connected, "Client should connect despite reordering");
    assert!(server_connected, "Server should connect despite reordering");
    assert!(packets_reordered > 0, "Should have reordered some packets");

    eprintln!(
        "SUCCESS: Handshake completed with {} packets delivered in reordered batches",
        packets_reordered
    );
}

/// Test delayed packets - hold packets for several rounds then deliver all at once
/// Uses deterministic hold pattern with sufficient rounds for retransmissions.
#[test]
#[cfg(feature = "rcgen")]
fn dtls13_handles_delayed_burst_delivery() {
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

    // Hold packets for delayed delivery
    let mut held_server_packets: Vec<Vec<u8>> = Vec::new();
    let mut held_client_packets: Vec<Vec<u8>> = Vec::new();
    let mut hold_rounds = 0;
    const HOLD_DURATION: usize = 3; // Hold packets for 3 rounds before delivering

    // Use more rounds with shorter time steps for reliability
    for _round in 0..200 {
        client.handle_timeout(now).expect("client timeout");
        server.handle_timeout(now).expect("server timeout");

        let client_out = drain_outputs(&mut client);
        let server_out = drain_outputs(&mut server);

        // Collect packets
        held_server_packets.extend(server_out.packets.iter().cloned());
        held_client_packets.extend(client_out.packets.iter().cloned());

        hold_rounds += 1;

        // Deliver burst every HOLD_DURATION rounds
        if hold_rounds >= HOLD_DURATION {
            // Deliver all held packets at once
            for p in &held_client_packets {
                let _ = server.handle_packet(p);
            }
            for p in &held_server_packets {
                let _ = client.handle_packet(p);
            }

            held_server_packets.clear();
            held_client_packets.clear();
            hold_rounds = 0;
        }

        client_connected |= client_out.connected;
        server_connected |= server_out.connected;

        if client_connected && server_connected {
            // Deliver any remaining packets
            for p in &held_client_packets {
                let _ = server.handle_packet(p);
            }
            for p in &held_server_packets {
                let _ = client.handle_packet(p);
            }
            break;
        }

        now += Duration::from_millis(20);
    }

    assert!(
        client_connected,
        "Client should connect despite delayed delivery"
    );
    assert!(
        server_connected,
        "Server should connect despite delayed delivery"
    );

    eprintln!("SUCCESS: Handshake completed with delayed burst delivery");
}

/// Test interleaved old and new packets (simulating network path changes)
/// Uses deterministic replay pattern with sufficient rounds for retransmissions.
#[test]
#[cfg(feature = "rcgen")]
fn dtls13_handles_interleaved_old_and_new_packets() {
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

    // Store some packets to replay later (simulating delayed path)
    let mut old_server_packets: Vec<Vec<u8>> = Vec::new();
    let mut captured_old = false;

    // Use more rounds with shorter time steps for reliability
    for round in 0..200 {
        client.handle_timeout(now).expect("client timeout");
        server.handle_timeout(now).expect("server timeout");

        let client_out = drain_outputs(&mut client);
        let server_out = drain_outputs(&mut server);

        deliver_packets(&client_out.packets, &mut server);

        // Capture some early packets
        if !captured_old && !server_out.packets.is_empty() && round < 5 {
            old_server_packets = server_out.packets.clone();
            captured_old = true;
        }

        // Normal delivery
        deliver_packets(&server_out.packets, &mut client);

        // Interleave old packets with new ones (replay old packets periodically)
        // Use deterministic pattern: replay at rounds 7, 14, 21, ...
        if captured_old && round % 7 == 0 && round > 0 && !old_server_packets.is_empty() {
            for p in &old_server_packets {
                // These should be safely ignored (duplicates/old epoch)
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
        "Client should connect despite interleaved old packets"
    );
    assert!(
        server_connected,
        "Server should connect despite interleaved old packets"
    );

    eprintln!("SUCCESS: Handshake completed with interleaved old/new packets");
}

/// After handshake, send 100 app data messages from client to server, delivering
/// each one. Then replay the very first packet. The replayed packet should be
/// silently dropped because it falls outside the 64-packet replay window.
#[test]
#[cfg(feature = "rcgen")]
fn dtls13_replay_window_rejects_old_record() {
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

    // Complete handshake
    for _ in 0..40 {
        client.handle_timeout(now).expect("client timeout");
        server.handle_timeout(now).expect("server timeout");

        let client_out = drain_outputs(&mut client);
        let server_out = drain_outputs(&mut server);

        deliver_packets(&client_out.packets, &mut server);
        deliver_packets(&server_out.packets, &mut client);

        client_connected |= client_out.connected;
        server_connected |= server_out.connected;

        if client_connected && server_connected {
            break;
        }

        now += Duration::from_millis(10);
    }

    assert!(client_connected, "Client should connect");
    assert!(server_connected, "Server should connect");

    // Send 100 app data messages from client to server, delivering each one.
    // Capture the very first packet for replay later.
    let mut first_packet: Option<Vec<u8>> = None;

    for i in 0..100 {
        let msg = format!("Message {}", i);
        client
            .send_application_data(msg.as_bytes())
            .expect("client send");

        let client_out = drain_outputs(&mut client);
        deliver_packets(&client_out.packets, &mut server);

        if i == 0 {
            // unwrap: first message always produces at least one packet
            first_packet = Some(client_out.packets[0].clone());
        }

        let server_out = drain_outputs(&mut server);
        assert!(
            !server_out.app_data.is_empty(),
            "Server should receive message {}",
            i
        );

        now += Duration::from_millis(1);
    }

    // Now replay the very first packet — it is well outside the 64-packet window
    let replayed = first_packet.expect("should have captured first packet");
    let _ = server.handle_packet(&replayed);

    let server_out = drain_outputs(&mut server);
    assert!(
        server_out.app_data.is_empty(),
        "Replayed old packet should be silently dropped"
    );
}

/// After handshake, send one app data message and deliver the packet. Then
/// deliver the exact same packet again. Only one copy of the data should be
/// received by the server — the duplicate is rejected by the replay window.
#[test]
#[cfg(feature = "rcgen")]
fn dtls13_replay_window_rejects_duplicate_app_data() {
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

    // Complete handshake
    for _ in 0..40 {
        client.handle_timeout(now).expect("client timeout");
        server.handle_timeout(now).expect("server timeout");

        let client_out = drain_outputs(&mut client);
        let server_out = drain_outputs(&mut server);

        deliver_packets(&client_out.packets, &mut server);
        deliver_packets(&server_out.packets, &mut client);

        client_connected |= client_out.connected;
        server_connected |= server_out.connected;

        if client_connected && server_connected {
            break;
        }

        now += Duration::from_millis(10);
    }

    assert!(client_connected, "Client should connect");
    assert!(server_connected, "Server should connect");

    // Send one app data message
    client
        .send_application_data(b"Hello once")
        .expect("client send");

    let client_out = drain_outputs(&mut client);
    let packets = client_out.packets.clone();

    // Deliver the packet — server should receive the data
    deliver_packets(&packets, &mut server);
    let server_out = drain_outputs(&mut server);
    assert_eq!(
        server_out.app_data.len(),
        1,
        "Server should receive exactly one message"
    );
    assert_eq!(&server_out.app_data[0][..], b"Hello once");

    // Deliver the exact same packet again — duplicate should be rejected
    deliver_packets(&packets, &mut server);
    let server_out = drain_outputs(&mut server);
    assert!(
        server_out.app_data.is_empty(),
        "Duplicate packet should be silently dropped"
    );
}

/// After handshake, send several app data messages. Deliver them out of order
/// (e.g., deliver message 3 before message 2, but both within the 64-packet
/// window). Both should be accepted and the data received.
#[test]
#[cfg(feature = "rcgen")]
fn dtls13_replay_window_accepts_within_range() {
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

    // Complete handshake
    for _ in 0..40 {
        client.handle_timeout(now).expect("client timeout");
        server.handle_timeout(now).expect("server timeout");

        let client_out = drain_outputs(&mut client);
        let server_out = drain_outputs(&mut server);

        deliver_packets(&client_out.packets, &mut server);
        deliver_packets(&server_out.packets, &mut client);

        client_connected |= client_out.connected;
        server_connected |= server_out.connected;

        if client_connected && server_connected {
            break;
        }

        now += Duration::from_millis(10);
    }

    assert!(client_connected, "Client should connect");
    assert!(server_connected, "Server should connect");

    // Send 5 app data messages, collecting all packets without delivering
    let mut all_packets: Vec<Vec<u8>> = Vec::new();
    let messages = vec![
        b"Message 1".to_vec(),
        b"Message 2".to_vec(),
        b"Message 3".to_vec(),
        b"Message 4".to_vec(),
        b"Message 5".to_vec(),
    ];

    for msg in &messages {
        client.send_application_data(msg).expect("client send");
        let client_out = drain_outputs(&mut client);
        all_packets.extend(client_out.packets);
    }

    // Deliver out of order: swap packets so that later ones arrive first.
    // Reverse the packet order — all are within the 64-packet window.
    let mut reordered = all_packets.clone();
    reordered.reverse();

    let mut server_received: Vec<Vec<u8>> = Vec::new();
    deliver_packets(&reordered, &mut server);
    let server_out = drain_outputs(&mut server);
    server_received.extend(server_out.app_data);

    assert_eq!(
        server_received.len(),
        messages.len(),
        "All out-of-order messages within window should be accepted"
    );

    // Verify all expected message contents were received (order may differ)
    let mut received_sorted: Vec<Vec<u8>> = server_received.clone();
    received_sorted.sort();
    let mut expected_sorted = messages.clone();
    expected_sorted.sort();
    assert_eq!(
        received_sorted, expected_sorted,
        "All message contents should be received"
    );
}

/// After handshake, send 10 app data messages from client. Collect all packets.
/// Deliver them in reverse order. All 10 messages should be received by the
/// server since they are all within the 64-packet replay window.
#[test]
#[cfg(feature = "rcgen")]
fn dtls13_reorder_app_data_after_handshake() {
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

    // Complete handshake
    for _ in 0..40 {
        client.handle_timeout(now).expect("client timeout");
        server.handle_timeout(now).expect("server timeout");

        let client_out = drain_outputs(&mut client);
        let server_out = drain_outputs(&mut server);

        deliver_packets(&client_out.packets, &mut server);
        deliver_packets(&server_out.packets, &mut client);

        client_connected |= client_out.connected;
        server_connected |= server_out.connected;

        if client_connected && server_connected {
            break;
        }

        now += Duration::from_millis(10);
    }

    assert!(client_connected, "Client should connect");
    assert!(server_connected, "Server should connect");

    // Send 10 app data messages, collecting all packets without delivering
    let mut all_packets: Vec<Vec<u8>> = Vec::new();
    let message_count = 10;

    for i in 0..message_count {
        let msg = format!("Reverse message {}", i);
        client
            .send_application_data(msg.as_bytes())
            .expect("client send");
        let client_out = drain_outputs(&mut client);
        all_packets.extend(client_out.packets);
    }

    // Deliver all packets in reverse order
    let mut reversed = all_packets.clone();
    reversed.reverse();

    deliver_packets(&reversed, &mut server);
    let server_out = drain_outputs(&mut server);

    assert_eq!(
        server_out.app_data.len(),
        message_count,
        "All {} messages should be received when delivered in reverse order",
        message_count
    );

    // Verify all expected message contents were received (order may differ)
    let mut received_sorted: Vec<String> = server_out
        .app_data
        .iter()
        .map(|d| String::from_utf8_lossy(d).to_string())
        .collect();
    received_sorted.sort();

    let mut expected_sorted: Vec<String> = (0..message_count)
        .map(|i| format!("Reverse message {}", i))
        .collect();
    expected_sorted.sort();

    assert_eq!(
        received_sorted, expected_sorted,
        "All message contents should be received"
    );
}
