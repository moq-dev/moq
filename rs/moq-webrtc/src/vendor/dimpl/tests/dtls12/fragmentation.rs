//! DTLS 1.2 fragmentation test (MTU-based).

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dimpl::{Config, Dtls, Output};

use crate::common::*;
use crate::ossl_helper::{DtlsCertOptions, DtlsEvent, OsslDtlsCert};

fn run_client_server_with_mtu(mtu: usize) -> (usize, usize) {
    // Initialize logger once across test runs
    let _ = env_logger::try_init();

    // Generate certificates for both client and server
    let client_cert_options = DtlsCertOptions::default();
    let client_cert = OsslDtlsCert::new(client_cert_options);

    let server_cert_options = DtlsCertOptions::default();
    let server_cert = OsslDtlsCert::new(server_cert_options);

    // Create server (OpenSSL-backed)
    let mut server = server_cert
        .new_dtls_impl()
        .expect("Failed to create DTLS server");

    // Server is passive
    server.set_active(false);

    // Initialize client
    let config = Arc::new(
        Config::builder()
            .mtu(mtu)
            .build()
            .expect("Failed to build config"),
    );

    // Get client certificate as DER encoded bytes
    let client_x509_der = client_cert
        .x509
        .to_der()
        .expect("Failed to get client cert DER");
    let client_pkey_der = client_cert
        .pkey
        .private_key_to_der()
        .expect("Failed to get client private key DER");

    let now = Instant::now();

    let mut client = Dtls::new_12(
        config,
        dimpl::DtlsCertificate {
            certificate: client_x509_der,
            private_key: client_pkey_der,
        },
        now,
    );
    client.set_active(true);

    // Server events queue
    let mut server_events = VecDeque::new();

    // State
    let mut client_connected = false;
    let mut server_connected = false;

    // Packet counters
    let mut client_to_server_packets: usize = 0;
    let mut server_to_client_packets: usize = 0;
    let mut max_c2s_len: usize = 0;

    // Test data
    let client_test_data = b"Hello from client";
    let server_test_data = b"Hello from server";

    // Buffers for received data
    let mut client_received_data = Vec::new();
    let mut server_received_data = Vec::new();

    // Drive handshake and data exchange
    let mut out_buf = vec![0u8; mtu + 512];
    for _ in 0..20 {
        client.handle_timeout(Instant::now()).unwrap();

        let mut continue_polling = true;
        while continue_polling {
            let output = client.poll_output(&mut out_buf);
            match output {
                Output::Packet(data) => {
                    client_to_server_packets += 1;
                    if data.len() > max_c2s_len {
                        max_c2s_len = data.len();
                    }
                    if let Err(e) = server.handle_receive(data, &mut server_events) {
                        panic!("Server failed to handle client packet: {:?}", e);
                    }
                }
                Output::Connected => {
                    client_connected = true;
                }
                Output::PeerCert(_cert) => {
                    // ignore for this test
                }
                Output::KeyingMaterial(_km, _profile) => {
                    // After handshake is complete, send test data
                    client
                        .send_application_data(client_test_data)
                        .expect("Failed to send client data");
                }
                Output::ApplicationData(data) => {
                    client_received_data.extend_from_slice(data);
                }
                Output::Timeout(_) => {
                    continue_polling = false;
                }
                _ => {}
            }
        }

        // Process server events
        while let Some(event) = server_events.pop_front() {
            match event {
                DtlsEvent::Connected => {
                    server_connected = true;
                }
                DtlsEvent::RemoteFingerprint(_fp) => {}
                DtlsEvent::SrtpKeyingMaterial(_km, _profile) => {
                    // After handshake is complete, send test data from server
                    server
                        .handle_input(server_test_data)
                        .expect("Failed to send server data");
                }
                DtlsEvent::Data(data) => {
                    server_received_data.extend_from_slice(&data);
                }
            }
        }

        // Send server datagrams to client and count them
        while let Some(datagram) = server.poll_datagram() {
            server_to_client_packets += 1;
            client
                .handle_packet(&datagram)
                .expect("Failed to handle server packet");
        }

        if client_connected
            && server_connected
            && !client_received_data.is_empty()
            && !server_received_data.is_empty()
        {
            break;
        }
    }

    // Basic correctness
    assert!(client_connected, "Client should be connected");
    assert!(server_connected, "Server should be connected");
    assert_eq!(server_received_data, client_test_data);
    assert_eq!(client_received_data, server_test_data);

    // Ensure the client never emits datagrams above its configured MTU
    assert!(
        max_c2s_len <= mtu,
        "client->server datagram length {} exceeds MTU {}",
        max_c2s_len,
        mtu
    );

    (client_to_server_packets, server_to_client_packets)
}

#[test]
fn dtls12_fragmentation_increases_packet_count() {
    // Larger MTU should pack more and send fewer packets
    let (large_c2s, large_s2c) = run_client_server_with_mtu(1400);
    // Smaller MTU forces more fragmentation
    let (small_c2s, small_s2c) = run_client_server_with_mtu(100);

    println!(
        "packet counts: large(c2s={}, s2c={}), small(c2s={}, s2c={})",
        large_c2s, large_s2c, small_c2s, small_s2c
    );

    // Tight-ish bounds informed by expected DTLS handshake/message sizes and packing
    assert!(
        (3..=8).contains(&large_c2s),
        "large MTU client->server packets: {}",
        large_c2s
    );
    assert!(
        (4..=20).contains(&small_c2s),
        "small MTU client->server packets: {}",
        small_c2s
    );
    assert!(
        small_c2s > large_c2s,
        "small MTU should produce more client->server packets"
    );

    // Optional checks for server->client direction with similarly tight bounds
    assert!(
        (3..=10).contains(&large_s2c),
        "large MTU server->client packets: {}",
        large_s2c
    );
    assert!(
        (5..=20).contains(&small_s2c),
        "small MTU server->client packets: {}",
        small_s2c
    );
    assert!(
        small_s2c >= large_s2c,
        "small MTU should produce at least as many server->client packets"
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls12_fragmentation_dimpl_to_dimpl() {
    //! Verify that a dimpl-to-dimpl handshake completes successfully with a
    //! small MTU that forces handshake message fragmentation.

    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    let config = dtls12_config_with_mtu(200);

    let mut now = Instant::now();

    let mut client = Dtls::new_12(Arc::clone(&config), client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_12(config, server_cert, now);
    server.set_active(false);

    let mut client_connected = false;
    let mut server_connected = false;
    let mut max_packet_size = 0usize;

    for _ in 0..40 {
        client.handle_timeout(now).expect("client timeout");
        server.handle_timeout(now).expect("server timeout");

        let client_out = drain_outputs(&mut client);
        let server_out = drain_outputs(&mut server);

        // Track max packet size to verify MTU compliance
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
        "Client should connect with small MTU (dimpl-to-dimpl)"
    );
    assert!(
        server_connected,
        "Server should connect with small MTU (dimpl-to-dimpl)"
    );
    assert!(
        max_packet_size <= 200,
        "All packets should respect 200-byte MTU: max was {}",
        max_packet_size
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls12_fragmented_handshake_with_packet_loss() {
    //! Use a small MTU so flights are fragmented into multiple packets.
    //! Drop the first packet of the first flight, then trigger a retransmit.
    //! Verify the handshake still completes.

    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    let config = dtls12_config_with_mtu(200);

    let mut now = Instant::now();

    let mut client = Dtls::new_12(Arc::clone(&config), client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_12(config, server_cert, now);
    server.set_active(false);

    // FLIGHT 1: Client sends ClientHello (possibly fragmented).
    // Trigger the initial timeout to start the handshake.
    client.handle_timeout(now).expect("client timeout start");
    client.handle_timeout(now).expect("client arm flight 1");
    let flight1 = collect_packets(&mut client);
    assert!(
        !flight1.is_empty(),
        "Client should emit at least one packet for flight 1"
    );

    // Drop the first packet of flight 1 and deliver the rest (if any).
    deliver_packets(&flight1[1..], &mut server);

    // Trigger retransmit on the client so it resends the full flight.
    trigger_timeout(&mut client, &mut now);
    let retransmit1 = collect_packets(&mut client);
    assert!(!retransmit1.is_empty(), "Client should retransmit flight 1");

    // Deliver the full retransmitted flight to the server.
    deliver_packets(&retransmit1, &mut server);

    // Now continue the handshake normally until completion.
    let mut client_connected = false;
    let mut server_connected = false;

    for _ in 0..40 {
        server.handle_timeout(now).expect("server timeout");
        let server_out = drain_outputs(&mut server);
        if server_out.connected {
            server_connected = true;
        }
        deliver_packets(&server_out.packets, &mut client);

        client.handle_timeout(now).expect("client timeout");
        let client_out = drain_outputs(&mut client);
        if client_out.connected {
            client_connected = true;
        }
        deliver_packets(&client_out.packets, &mut server);

        if client_connected && server_connected {
            break;
        }

        now += Duration::from_millis(10);
    }

    assert!(
        client_connected,
        "Client should connect after retransmit of dropped fragment"
    );
    assert!(
        server_connected,
        "Server should connect after retransmit of dropped fragment"
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls12_out_of_order_fragments() {
    //! Use a small MTU to force fragmentation. Before delivering each flight,
    //! reverse the packet order. Verify the handshake still completes, proving
    //! fragment reassembly handles out-of-order delivery.

    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    let config = dtls12_config_with_mtu(200);

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

        // Reverse packet order before delivery to simulate out-of-order fragments
        let mut client_packets = client_out.packets;
        let mut server_packets = server_out.packets;
        client_packets.reverse();
        server_packets.reverse();

        deliver_packets(&client_packets, &mut server);
        deliver_packets(&server_packets, &mut client);

        if client_connected && server_connected {
            break;
        }

        // Reordered protected records may be discarded until CCS is processed.
        // Give the retransmission timer enough time to recover those records.
        now += Duration::from_secs(1);
    }

    assert!(
        client_connected,
        "Client should connect with out-of-order fragments"
    );
    assert!(
        server_connected,
        "Server should connect with out-of-order fragments"
    );
}
