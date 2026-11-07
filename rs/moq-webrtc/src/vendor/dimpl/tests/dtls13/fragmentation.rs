//! DTLS 1.3 fragmentation tests.

use std::sync::Arc;
use std::time::{Duration, Instant};

use dimpl::{Config, Dtls};

use crate::common::*;

#[test]
#[cfg(feature = "rcgen")]
fn dtls13_handshake_with_small_mtu() {
    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    // Use small MTU to force fragmentation
    let config = dtls13_config_with_mtu(200);

    let mut now = Instant::now();

    let mut client = Dtls::new_13(Arc::clone(&config), client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_13(config, server_cert, now);
    server.set_active(false);

    let mut client_connected = false;
    let mut server_connected = false;
    let mut max_packet_size = 0usize;

    for _ in 0..40 {
        client.handle_timeout(now).expect("client timeout");
        server.handle_timeout(now).expect("server timeout");

        let client_out = drain_outputs(&mut client);
        let server_out = drain_outputs(&mut server);

        // Track max packet size
        for p in &client_out.packets {
            if p.len() > max_packet_size {
                max_packet_size = p.len();
            }
        }

        if client_out.connected {
            client_connected = true;
        }
        if server_out.connected {
            server_connected = true;
        }

        deliver_packets(&client_out.packets, &mut server);
        deliver_packets(&server_out.packets, &mut client);

        if client_connected && server_connected {
            break;
        }

        now += Duration::from_millis(10);
    }

    assert!(client_connected, "Client should connect with small MTU");
    assert!(server_connected, "Server should connect with small MTU");
    assert!(
        max_packet_size <= 200,
        "Packets should respect MTU: max was {}",
        max_packet_size
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls13_large_application_data_fragmented() {
    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    // Small MTU
    let config = dtls13_config_with_mtu(300);

    let mut now = Instant::now();

    let mut client = Dtls::new_13(Arc::clone(&config), client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_13(config, server_cert, now);
    server.set_active(false);

    // First complete handshake
    for _ in 0..40 {
        client.handle_timeout(now).expect("client timeout");
        server.handle_timeout(now).expect("server timeout");

        let client_out = drain_outputs(&mut client);
        let server_out = drain_outputs(&mut server);

        deliver_packets(&client_out.packets, &mut server);
        deliver_packets(&server_out.packets, &mut client);

        if client_out.connected && server_out.connected {
            break;
        }

        now += Duration::from_millis(10);
    }

    // Send large data (larger than MTU)
    let large_data = vec![0xABu8; 1000];
    client
        .send_application_data(&large_data)
        .expect("client send large data");

    let mut server_received: Vec<u8> = Vec::new();
    let mut _packet_count = 0;

    for _ in 0..20 {
        let client_out = drain_outputs(&mut client);
        _packet_count += client_out.packets.len();
        deliver_packets(&client_out.packets, &mut server);

        let server_out = drain_outputs(&mut server);
        for data in server_out.app_data {
            server_received.extend_from_slice(&data);
        }

        if server_received.len() >= large_data.len() {
            break;
        }

        now += Duration::from_millis(10);
    }

    assert_eq!(
        server_received, large_data,
        "Large data should be received correctly"
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls13_fragmentation_during_hrr() {
    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    // Small MTU to force fragmentation during HRR handshake
    let config = dtls13_config_with_mtu(200);

    let mut now = Instant::now();

    let mut client = Dtls::new_13(Arc::clone(&config), client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_13(config, server_cert, now);
    server.set_active(false);

    let mut client_connected = false;
    let mut server_connected = false;
    let mut max_packet_size = 0usize;
    let mut saw_hrr = false;
    let mut flight_count = 0;

    for _ in 0..40 {
        client.handle_timeout(now).expect("client timeout");
        server.handle_timeout(now).expect("server timeout");

        let client_out = drain_outputs(&mut client);
        let server_out = drain_outputs(&mut server);

        if !client_out.packets.is_empty() {
            flight_count += 1;
        }

        // Track if we see what looks like HRR response (server sends before full handshake)
        if !server_out.packets.is_empty() && !client_connected && flight_count <= 2 {
            saw_hrr = true;
        }

        // Track max packet size
        for p in &client_out.packets {
            if p.len() > max_packet_size {
                max_packet_size = p.len();
            }
        }
        for p in &server_out.packets {
            if p.len() > max_packet_size {
                max_packet_size = p.len();
            }
        }

        if client_out.connected {
            client_connected = true;
        }
        if server_out.connected {
            server_connected = true;
        }

        deliver_packets(&client_out.packets, &mut server);
        deliver_packets(&server_out.packets, &mut client);

        if client_connected && server_connected {
            break;
        }

        now += Duration::from_millis(10);
    }

    assert!(
        client_connected,
        "Client should connect with HRR and small MTU"
    );
    assert!(
        server_connected,
        "Server should connect with HRR and small MTU"
    );
    assert!(
        max_packet_size <= 200,
        "Packets should respect MTU: max was {}",
        max_packet_size
    );
    assert!(
        saw_hrr || flight_count >= 2,
        "Should have seen HRR or multiple client flights"
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls13_fragmented_handshake_with_packet_loss() {
    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    // Small MTU to force fragmentation, extra retries and longer handshake timeout
    // to survive loss and retransmission delays
    let config = Arc::new(
        Config::builder()
            .mtu(200)
            .flight_retries(8)
            .handshake_timeout(Duration::from_secs(120))
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
    let mut dropped_client = 0;
    let mut dropped_server = 0;

    // Track the packet count of the last flight from each side.
    // When the count changes, it's a new flight (different protocol step).
    // We drop the first packet only on the first transmission of each new flight;
    // retransmissions (same packet count) are delivered in full.
    let mut prev_client_count = 0usize;
    let mut client_drop_armed = false;
    let mut prev_server_count = 0usize;
    let mut server_drop_armed = false;

    for i in 0..120 {
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

        // Detect new flight from client: packet count changed from previous
        if !client_out.packets.is_empty() && client_out.packets.len() != prev_client_count {
            client_drop_armed = true;
            prev_client_count = client_out.packets.len();
        }

        // Deliver client packets, dropping the first of each new flight
        if !client_out.packets.is_empty() && client_drop_armed && client_out.packets.len() > 1 {
            client_drop_armed = false;
            dropped_client += 1;
            for p in &client_out.packets[1..] {
                let _ = server.handle_packet(p);
            }
        } else {
            deliver_packets(&client_out.packets, &mut server);
        }

        // Detect new flight from server: packet count changed from previous
        if !server_out.packets.is_empty() && server_out.packets.len() != prev_server_count {
            server_drop_armed = true;
            prev_server_count = server_out.packets.len();
        }

        // Deliver server packets, dropping the first of each new flight
        if !server_out.packets.is_empty() && server_drop_armed && server_out.packets.len() > 1 {
            server_drop_armed = false;
            dropped_server += 1;
            for p in &server_out.packets[1..] {
                let _ = client.handle_packet(p);
            }
        } else {
            deliver_packets(&server_out.packets, &mut client);
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
        "Client should connect despite fragmented packet loss"
    );
    assert!(
        server_connected,
        "Server should connect despite fragmented packet loss"
    );
    assert!(
        dropped_client > 0,
        "Should have dropped at least one client packet"
    );
    assert!(
        dropped_server > 0,
        "Should have dropped at least one server packet"
    );
}

/// Issue 1: Overlapping handshake fragments must be reassembled successfully.
///
/// RFC 9147 allows overlapping handshake fragments. The reassembly logic must
/// tolerate overlaps (e.g., [0..100] then [50..200]) and still consider the
/// message complete once all bytes are covered.
///
/// This test captures a fragmented ClientHello, modifies the second fragment
/// to create a 10-byte overlap, and verifies the server completes the handshake.
#[test]
#[cfg(feature = "rcgen")]
fn dtls13_overlapping_fragments_reassembled_successfully() {
    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    // Small MTU forces ClientHello into multiple fragments while keeping
    // the server's response flight within the flight_saved_records capacity.
    let config = dtls13_config_with_mtu(150);

    let mut now = Instant::now();

    let mut client = Dtls::new_13(Arc::clone(&config), client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_13(config, server_cert, now);
    server.set_active(false);

    // Get the client's first flight (fragmented ClientHello)
    client.handle_timeout(now).expect("client timeout");
    let client_out = drain_outputs(&mut client);

    // Find handshake fragments: DTLSPlaintext records with content_type 0x16
    // and epoch 0 (bytes 3-4 == 0x00 0x00).
    let handshake_packets: Vec<usize> = client_out
        .packets
        .iter()
        .enumerate()
        .filter(|(_, p)| p.len() > 25 && p[0] == 0x16 && p[3] == 0x00 && p[4] == 0x00)
        .map(|(i, _)| i)
        .collect();

    assert!(
        handshake_packets.len() >= 2,
        "ClientHello should be fragmented into at least 2 packets, got {}",
        handshake_packets.len()
    );

    // Create modified packets with overlapping second fragment.
    //
    // DTLSPlaintext layout:
    //   Bytes 0:     content_type (0x16 = handshake)
    //   Bytes 1-2:   version
    //   Bytes 3-4:   epoch
    //   Bytes 5-10:  sequence_number (6 bytes)
    //   Bytes 11-12: record length
    //   Bytes 13:    msg_type (handshake header)
    //   Bytes 14-16: total message length (3 bytes)
    //   Bytes 17-18: message_seq (2 bytes)
    //   Bytes 19-21: fragment_offset (3 bytes, big-endian u24)
    //   Bytes 22-24: fragment_length (3 bytes, big-endian u24)
    //   Bytes 25+:   body
    let mut modified_packets = client_out.packets.clone();
    let first_idx = handshake_packets[0];
    let second_idx = handshake_packets[1];

    // Get overlap data from the end of the first fragment's body
    let first_frag_len = {
        let p = &modified_packets[first_idx];
        ((p[22] as usize) << 16) | ((p[23] as usize) << 8) | (p[24] as usize)
    };
    let overlap_data: Vec<u8> = {
        let p = &modified_packets[first_idx];
        let body_end = 25 + first_frag_len;
        p[body_end - 10..body_end].to_vec()
    };

    // Build a new second packet with the overlap data prepended to its body.
    // This shifts fragment_offset back by 10, increases fragment_length by 10,
    // and updates the record length accordingly.
    let new_packet = {
        let packet = &modified_packets[second_idx];
        let orig_offset =
            ((packet[19] as u32) << 16) | ((packet[20] as u32) << 8) | (packet[21] as u32);
        assert!(
            orig_offset >= 10,
            "Second fragment offset should be >= 10 for overlap test, got {}",
            orig_offset
        );
        let orig_frag_len =
            ((packet[22] as u32) << 16) | ((packet[23] as u32) << 8) | (packet[24] as u32);
        let orig_record_len = u16::from_be_bytes([packet[11], packet[12]]);

        let new_offset = orig_offset - 10;
        let new_frag_len = orig_frag_len + 10;
        let new_record_len = orig_record_len + 10;

        let mut p = Vec::with_capacity(packet.len() + 10);
        // Record header through message_seq (bytes 0-18)
        p.extend_from_slice(&packet[..19]);
        // fragment_offset (3 bytes)
        p.push((new_offset >> 16) as u8);
        p.push((new_offset >> 8) as u8);
        p.push(new_offset as u8);
        // fragment_length (3 bytes)
        p.push((new_frag_len >> 16) as u8);
        p.push((new_frag_len >> 8) as u8);
        p.push(new_frag_len as u8);
        // Overlap bytes + original body
        p.extend_from_slice(&overlap_data);
        p.extend_from_slice(&packet[25..]);
        // Fix record length (bytes 11-12)
        p[11] = (new_record_len >> 8) as u8;
        p[12] = new_record_len as u8;
        p
    };
    modified_packets[second_idx] = new_packet;

    // Deliver overlapping fragments to server
    deliver_packets(&modified_packets, &mut server);

    // Drive the handshake to completion. The server must reassemble the
    // overlapping fragments and complete the handshake with the client.
    let mut client_connected = false;
    let mut server_connected = false;
    for _ in 0..40 {
        now += Duration::from_millis(10);
        client.handle_timeout(now).expect("client timeout");
        match server.handle_timeout(now) {
            Ok(()) => {}
            Err(e) => panic!("Server should not error with overlapping fragments: {}", e),
        }

        let client_out = drain_outputs(&mut client);
        let server_out = drain_outputs(&mut server);

        if client_out.connected {
            client_connected = true;
        }
        if server_out.connected {
            server_connected = true;
        }

        deliver_packets(&client_out.packets, &mut server);
        deliver_packets(&server_out.packets, &mut client);

        if client_connected && server_connected {
            break;
        }
    }

    assert!(
        server_connected,
        "Server should reassemble overlapping fragments and complete the handshake"
    );
}
