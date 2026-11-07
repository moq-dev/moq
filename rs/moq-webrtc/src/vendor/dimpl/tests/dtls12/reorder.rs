//! DTLS 1.2 packet reordering, replay, and duplicate tests.

use std::sync::Arc;
use std::time::{Duration, Instant};

use dimpl::Dtls;

use crate::common::*;

#[test]
fn dtls12_discards_scrambled_datagram_and_accepts_clean_retransmission() {
    use dimpl::{Config, DtlsCertificate};

    use crate::ossl_helper::{DtlsCertOptions, OsslDtlsCert};

    let certificate = || {
        let cert = OsslDtlsCert::new(DtlsCertOptions::default());
        DtlsCertificate {
            certificate: cert.x509.to_der().expect("certificate DER"),
            private_key: cert.pkey.private_key_to_der().expect("private key DER"),
        }
    };
    let config = Arc::new(
        Config::builder()
            .use_server_cookie(false)
            .build()
            .expect("valid config"),
    );
    let now = Instant::now();
    let mut client = Dtls::new_12(Arc::clone(&config), certificate(), now);
    client.set_active(true);
    let mut server = Dtls::new_12(config, certificate(), now);
    server.set_active(false);
    server.handle_timeout(now).expect("initialize server");
    assert!(drain_outputs(&mut server).packets.is_empty());

    client.handle_timeout(now).expect("start ClientHello");
    let client_hello = drain_outputs(&mut client);
    client.handle_timeout(now).expect("arm client flight timer");
    assert!(drain_outputs(&mut client).packets.is_empty());
    let mut server_flight = Vec::new();
    for packet in &client_hello.packets {
        server.handle_packet(packet).expect("receive ClientHello");
        server_flight.extend(drain_outputs(&mut server).packets);
    }
    server.handle_timeout(now).expect("arm server flight timer");
    assert!(drain_outputs(&mut server).packets.is_empty());
    let mut client_flight = Vec::new();
    for packet in &server_flight {
        client.handle_packet(packet).expect("receive server flight");
        client_flight.extend(drain_outputs(&mut client).packets);
    }
    assert_eq!(
        client_flight.len(),
        1,
        "reproduce reordering within one datagram"
    );
    client
        .handle_timeout(now)
        .expect("arm client final-flight timer");
    let deadline = drain_outputs(&mut client)
        .timeout
        .expect("client retry deadline");

    let mut remaining = &client_flight[0][..];
    let mut records = Vec::new();
    while !remaining.is_empty() {
        let length = 13 + usize::from(u16::from_be_bytes([remaining[11], remaining[12]]));
        records.push(&remaining[..length]);
        remaining = &remaining[length..];
    }
    assert!(
        records.len() >= 5,
        "client flight includes certificate, key exchange, verify, CCS and Finished"
    );
    let scrambled: Vec<u8> = records
        .iter()
        .rev()
        .flat_map(|record| record.iter().copied())
        .collect();
    server
        .handle_packet(&scrambled)
        .expect("silently discard scrambled datagram");
    let discarded = drain_outputs(&mut server);
    assert!(discarded.packets.is_empty());
    assert!(!discarded.connected);

    client
        .handle_timeout(deadline)
        .expect("retransmit clean final flight");
    let retry = drain_outputs(&mut client);
    assert!(!retry.packets.is_empty());
    let mut final_flight = Vec::new();
    let mut server_connected = false;
    for packet in &retry.packets {
        server
            .handle_packet(packet)
            .expect("accept clean retransmission");
        let output = drain_outputs(&mut server);
        server_connected |= output.connected;
        final_flight.extend(output.packets);
    }
    assert!(
        server_connected,
        "scrambled copy must not poison the receive queue"
    );
    let mut client_connected = false;
    for packet in &final_flight {
        client
            .handle_packet(packet)
            .expect("receive server final flight");
        client_connected |= drain_outputs(&mut client).connected;
    }
    assert!(client_connected);
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls12_handles_duplicate_packets() {
    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    let config = dtls12_config();

    let mut now = Instant::now();

    let mut client = Dtls::new_12(Arc::clone(&config), client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_12(config, server_cert, now);
    server.set_active(false);

    let mut client_connected = false;
    let mut server_connected = false;

    for _ in 0..80 {
        client.handle_timeout(now).expect("client timeout");
        server.handle_timeout(now).expect("server timeout");

        let client_out = drain_outputs(&mut client);
        let server_out = drain_outputs(&mut server);

        client_connected |= client_out.connected;
        server_connected |= server_out.connected;

        // Deliver originals first
        deliver_packets(&client_out.packets, &mut server);
        deliver_packets(&server_out.packets, &mut client);

        // Drain any events triggered by the original delivery before
        // injecting duplicates, so the state machine has advanced past
        // the flight boundary.
        let server_mid = drain_outputs(&mut server);
        let client_mid = drain_outputs(&mut client);
        server_connected |= server_mid.connected;
        client_connected |= client_mid.connected;

        // Now deliver duplicates -- these should be tolerated
        deliver_packets(&client_out.packets, &mut server);
        deliver_packets(&server_out.packets, &mut client);

        if client_connected && server_connected {
            break;
        }

        now += Duration::from_millis(20);
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
fn dtls12_handles_out_of_order_packets() {
    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    let config = dtls12_config();

    let mut now = Instant::now();

    let mut client = Dtls::new_12(Arc::clone(&config), client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_12(config, server_cert, now);
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

#[test]
#[cfg(feature = "rcgen")]
fn dtls12_replay_window_rejects_old_app_data() {
    //! After handshake, send 100 app data messages from client to server.
    //! Then replay the very first encrypted packet. The replayed packet
    //! should be silently dropped (not delivered as application data again).

    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    let config = dtls12_config();

    let mut now = Instant::now();

    let mut client = Dtls::new_12(Arc::clone(&config), client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_12(config, server_cert, now);
    server.set_active(false);

    // Complete handshake
    let mut client_connected = false;
    let mut server_connected = false;

    for _ in 0..40 {
        client.handle_timeout(now).expect("client timeout");
        server.handle_timeout(now).expect("server timeout");

        let client_out = drain_outputs(&mut client);
        let server_out = drain_outputs(&mut server);

        client_connected |= client_out.connected;
        server_connected |= server_out.connected;

        deliver_packets(&client_out.packets, &mut server);
        deliver_packets(&server_out.packets, &mut client);

        if client_connected && server_connected {
            break;
        }

        now += Duration::from_millis(10);
    }

    assert!(client_connected, "Client should be connected");
    assert!(server_connected, "Server should be connected");

    // Send the first message and capture its encrypted packet
    client
        .send_application_data(b"message 1")
        .expect("send message 1");
    let first_out = drain_outputs(&mut client);
    assert!(
        !first_out.packets.is_empty(),
        "Should have packets for message 1"
    );
    let early_packet = first_out.packets[0].clone();

    // Deliver and verify it arrives
    deliver_packets(&first_out.packets, &mut server);
    let server_out = drain_outputs(&mut server);
    assert_eq!(
        server_out.app_data.len(),
        1,
        "Server should receive message 1"
    );

    // Send 99 more messages to advance the replay window well past the first
    for i in 2..=100 {
        let msg = format!("message {}", i);
        client
            .send_application_data(msg.as_bytes())
            .expect("send message");

        let client_out = drain_outputs(&mut client);
        deliver_packets(&client_out.packets, &mut server);
        let server_out = drain_outputs(&mut server);
        assert!(
            !server_out.app_data.is_empty(),
            "Server should receive message {}",
            i
        );
    }

    // Now replay the very first encrypted packet
    let _ = server.handle_packet(&early_packet);
    let replay_out = drain_outputs(&mut server);

    assert!(
        replay_out.app_data.is_empty(),
        "Replayed old packet should NOT produce application data (replay protection)"
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls12_replay_window_rejects_duplicate_app_data() {
    //! After handshake, send one app data message. Deliver it, then deliver
    //! the exact same encrypted packet again. Only one copy should be received.

    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    let config = dtls12_config();

    let mut now = Instant::now();

    let mut client = Dtls::new_12(Arc::clone(&config), client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_12(config, server_cert, now);
    server.set_active(false);

    // Complete handshake
    let mut client_connected = false;
    let mut server_connected = false;

    for _ in 0..40 {
        client.handle_timeout(now).expect("client timeout");
        server.handle_timeout(now).expect("server timeout");

        let client_out = drain_outputs(&mut client);
        let server_out = drain_outputs(&mut server);

        client_connected |= client_out.connected;
        server_connected |= server_out.connected;

        deliver_packets(&client_out.packets, &mut server);
        deliver_packets(&server_out.packets, &mut client);

        if client_connected && server_connected {
            break;
        }

        now += Duration::from_millis(10);
    }

    assert!(client_connected, "Client should be connected");
    assert!(server_connected, "Server should be connected");

    // Send a single app data message
    client
        .send_application_data(b"only once")
        .expect("send app data");
    let client_out = drain_outputs(&mut client);
    assert!(
        !client_out.packets.is_empty(),
        "Should have packets for app data"
    );

    // Save the encrypted packet for replay
    let encrypted_packet = client_out.packets[0].clone();

    // First delivery -- should produce app data
    let _ = server.handle_packet(&encrypted_packet);
    let first_out = drain_outputs(&mut server);
    assert_eq!(
        first_out.app_data.len(),
        1,
        "First delivery should produce exactly one app data"
    );
    assert_eq!(first_out.app_data[0], b"only once");

    // Second delivery of exact same packet -- should be silently dropped
    let _ = server.handle_packet(&encrypted_packet);
    let dup_out = drain_outputs(&mut server);
    assert!(
        dup_out.app_data.is_empty(),
        "Duplicate packet should NOT produce application data (replay protection)"
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls12_handles_delayed_burst_delivery() {
    //! During handshake, hold all packets for 3 rounds of timeouts, then
    //! deliver them all at once in a burst. Verify handshake still completes.

    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    let config = dtls12_config();

    let mut now = Instant::now();

    let mut client = Dtls::new_12(Arc::clone(&config), client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_12(config, server_cert, now);
    server.set_active(false);
    let mut client_connected = false;
    let mut server_connected = false;

    // Hold packets for delayed delivery
    let mut held_server_packets: Vec<Vec<u8>> = Vec::new();
    let mut held_client_packets: Vec<Vec<u8>> = Vec::new();
    let mut hold_rounds = 0;
    const HOLD_DURATION: usize = 3; // Hold packets for 3 rounds before delivering

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
}
