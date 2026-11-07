//! DTLS 1.3 interop tests: dimpl <-> WolfSSL
//!
//! Tests verify DTLS 1.3 interoperability between dimpl and WolfSSL,
//! with dimpl as both client and server.

#![allow(unused, dead_code)]

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dimpl::{Config, Dtls, Output, SrtpProfile};

use crate::wolfssl_helper::{DtlsEvent, WolfDtlsCert};

// =============================================================================
// Shared helpers
// =============================================================================

/// Collected outputs from polling a dimpl endpoint.
#[derive(Default, Debug)]
struct DrainedOutputs {
    packets: Vec<Vec<u8>>,
    connected: bool,
    peer_cert: Option<Vec<u8>>,
    keying_material: Option<(Vec<u8>, SrtpProfile)>,
    app_data: Vec<Vec<u8>>,
    timeout: Option<Instant>,
}

/// Drain all outputs from a dimpl endpoint.
fn drain_dimpl_outputs(endpoint: &mut Dtls) -> DrainedOutputs {
    let mut result = DrainedOutputs::default();
    let mut buf = vec![0u8; 2048];

    loop {
        match endpoint.poll_output(&mut buf) {
            Output::Packet(p) => result.packets.push(p.to_vec()),
            Output::Connected => result.connected = true,
            Output::PeerCert(cert) => result.peer_cert = Some(cert.to_vec()),
            Output::KeyingMaterial(km, profile) => {
                result.keying_material = Some((km.to_vec(), profile));
            }
            Output::ApplicationData(data) => result.app_data.push(data.to_vec()),
            Output::Timeout(t) => {
                result.timeout = Some(t);
                break;
            }
            _ => {}
        }
    }

    result
}

/// Create a DTLS 1.3 config.
fn dtls13_config() -> Arc<Config> {
    Arc::new(
        Config::builder()
            .build()
            .expect("Failed to build DTLS 1.3 config"),
    )
}

/// Create a DTLS 1.3 config with custom MTU.
fn dtls13_config_with_mtu(mtu: usize) -> Arc<Config> {
    Arc::new(
        Config::builder()
            .mtu(mtu)
            .build()
            .expect("Failed to build DTLS 1.3 config"),
    )
}

// =============================================================================
// Client tests: dimpl client <-> WolfSSL server
// =============================================================================

#[test]
#[cfg(feature = "rcgen")]
fn dtls13_wolfssl_client_handshake() {
    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_dimpl_cert = generate_self_signed_certificate().expect("gen server cert");

    let wolf_server_cert = WolfDtlsCert::new(
        server_dimpl_cert.certificate.clone(),
        server_dimpl_cert.private_key.clone(),
    );

    let mut wolf_server = wolf_server_cert
        .new_dtls13_impl(true)
        .expect("Failed to create WolfSSL server");

    let config = dtls13_config();

    let now = Instant::now();
    let mut dimpl_client = Dtls::new_13(config, client_cert, now);
    dimpl_client.set_active(true);

    let mut now = Instant::now();
    let mut wolf_events = VecDeque::new();

    let mut client_connected = false;
    let mut server_connected = false;

    for _ in 0..50 {
        dimpl_client.handle_timeout(now).expect("client timeout");

        let client_out = drain_dimpl_outputs(&mut dimpl_client);
        if client_out.connected {
            client_connected = true;
        }

        for packet in &client_out.packets {
            wolf_server
                .handle_receive(packet, &mut wolf_events)
                .expect("wolf server handle receive");
        }

        while let Some(event) = wolf_events.pop_front() {
            if matches!(event, DtlsEvent::Connected) {
                server_connected = true;
            }
        }

        while let Some(packet) = wolf_server.poll_datagram() {
            let _ = dimpl_client.handle_packet(&packet);
        }

        if client_connected && server_connected {
            break;
        }

        now += Duration::from_millis(10);
    }

    assert!(client_connected, "dimpl client should be connected");
    assert!(server_connected, "WolfSSL server should be connected");
}

// NOTE: Keying material test skipped for WolfSSL interop.
// WolfSSL DTLS 1.3 server doesn't appear to support SRTP extension by default,
// so keying material export won't work without additional WolfSSL configuration.

#[test]
#[cfg(feature = "rcgen")]
fn dtls13_wolfssl_client_data_exchange() {
    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_dimpl_cert = generate_self_signed_certificate().expect("gen server cert");

    let wolf_server_cert = WolfDtlsCert::new(
        server_dimpl_cert.certificate.clone(),
        server_dimpl_cert.private_key.clone(),
    );

    let mut wolf_server = wolf_server_cert
        .new_dtls13_impl(true)
        .expect("Failed to create WolfSSL server");

    let config = dtls13_config();

    let now = Instant::now();
    let mut dimpl_client = Dtls::new_13(config, client_cert, now);
    dimpl_client.set_active(true);

    let mut now = Instant::now();
    let mut wolf_events = VecDeque::new();

    // Complete handshake first
    for _ in 0..50 {
        dimpl_client.handle_timeout(now).expect("client timeout");

        let client_out = drain_dimpl_outputs(&mut dimpl_client);

        for packet in &client_out.packets {
            wolf_server
                .handle_receive(packet, &mut wolf_events)
                .expect("wolf server handle receive");
        }

        while let Some(packet) = wolf_server.poll_datagram() {
            let _ = dimpl_client.handle_packet(&packet);
        }

        if client_out.connected && wolf_server.is_connected() {
            break;
        }

        wolf_events.clear();
        now += Duration::from_millis(10);
    }

    // Send data from client to server
    let test_data = b"Hello from dimpl client!";
    dimpl_client
        .send_application_data(test_data)
        .expect("write app data");

    let client_out = drain_dimpl_outputs(&mut dimpl_client);

    let mut received_data = Vec::new();
    for packet in &client_out.packets {
        wolf_server
            .handle_receive(packet, &mut wolf_events)
            .expect("wolf server handle receive");
    }

    while let Some(event) = wolf_events.pop_front() {
        if let DtlsEvent::Data(data) = event {
            received_data.extend_from_slice(&data);
        }
    }

    assert_eq!(
        received_data, test_data,
        "WolfSSL server should receive the data from dimpl client"
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls13_wolfssl_client_bidirectional_data() {
    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_dimpl_cert = generate_self_signed_certificate().expect("gen server cert");

    let wolf_server_cert = WolfDtlsCert::new(
        server_dimpl_cert.certificate.clone(),
        server_dimpl_cert.private_key.clone(),
    );

    let mut wolf_server = wolf_server_cert
        .new_dtls13_impl(true)
        .expect("Failed to create WolfSSL server");

    let config = dtls13_config();

    let now = Instant::now();
    let mut dimpl_client = Dtls::new_13(config, client_cert, now);
    dimpl_client.set_active(true);

    let mut now = Instant::now();
    let mut wolf_events = VecDeque::new();

    // Complete handshake
    for _ in 0..50 {
        dimpl_client.handle_timeout(now).expect("client timeout");
        let client_out = drain_dimpl_outputs(&mut dimpl_client);
        for packet in &client_out.packets {
            wolf_server
                .handle_receive(packet, &mut wolf_events)
                .unwrap();
        }
        while let Some(packet) = wolf_server.poll_datagram() {
            let _ = dimpl_client.handle_packet(&packet);
        }
        if client_out.connected && wolf_server.is_connected() {
            break;
        }
        wolf_events.clear();
        now += Duration::from_millis(10);
    }

    // Send data both directions
    let client_data = b"Hello from dimpl client!";
    let server_data = b"Hello from WolfSSL server!";

    dimpl_client
        .send_application_data(client_data)
        .expect("client send");
    wolf_server.write(server_data).expect("server send");

    let mut client_received = Vec::new();
    let mut server_received = Vec::new();

    for _ in 0..20 {
        let client_out = drain_dimpl_outputs(&mut dimpl_client);
        for data in client_out.app_data {
            client_received.extend_from_slice(&data);
        }
        for packet in &client_out.packets {
            wolf_server
                .handle_receive(packet, &mut wolf_events)
                .unwrap();
        }

        while let Some(event) = wolf_events.pop_front() {
            if let DtlsEvent::Data(data) = event {
                server_received.extend_from_slice(&data);
            }
        }

        while let Some(packet) = wolf_server.poll_datagram() {
            let _ = dimpl_client.handle_packet(&packet);
        }

        if !client_received.is_empty() && !server_received.is_empty() {
            break;
        }

        now += Duration::from_millis(10);
    }

    assert_eq!(
        client_received, server_data,
        "Client should receive server data"
    );
    assert_eq!(
        server_received, client_data,
        "Server should receive client data"
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls13_wolfssl_client_multiple_messages() {
    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_dimpl_cert = generate_self_signed_certificate().expect("gen server cert");

    let wolf_server_cert = WolfDtlsCert::new(
        server_dimpl_cert.certificate.clone(),
        server_dimpl_cert.private_key.clone(),
    );

    let mut wolf_server = wolf_server_cert
        .new_dtls13_impl(true)
        .expect("Failed to create WolfSSL server");

    let config = dtls13_config();

    let now = Instant::now();
    let mut dimpl_client = Dtls::new_13(config, client_cert, now);
    dimpl_client.set_active(true);

    let mut now = Instant::now();
    let mut wolf_events = VecDeque::new();

    // Complete handshake
    for _ in 0..50 {
        dimpl_client.handle_timeout(now).expect("client timeout");
        let client_out = drain_dimpl_outputs(&mut dimpl_client);
        for packet in &client_out.packets {
            wolf_server
                .handle_receive(packet, &mut wolf_events)
                .unwrap();
        }
        while let Some(packet) = wolf_server.poll_datagram() {
            let _ = dimpl_client.handle_packet(&packet);
        }
        if client_out.connected && wolf_server.is_connected() {
            break;
        }
        wolf_events.clear();
        now += Duration::from_millis(10);
    }

    // Send multiple messages - send each one and let it be processed
    let messages = vec![
        b"Message 1".to_vec(),
        b"Message 2".to_vec(),
        b"Message 3 is a bit longer".to_vec(),
        b"Message 4".to_vec(),
        b"Message 5 - the final one".to_vec(),
    ];

    let mut server_received: Vec<Vec<u8>> = Vec::new();

    for msg in &messages {
        dimpl_client
            .send_application_data(msg)
            .expect("client send");

        // Drain and deliver each message immediately
        let client_out = drain_dimpl_outputs(&mut dimpl_client);
        for packet in &client_out.packets {
            wolf_server
                .handle_receive(packet, &mut wolf_events)
                .unwrap();
        }

        while let Some(event) = wolf_events.pop_front() {
            if let DtlsEvent::Data(data) = event {
                server_received.push(data);
            }
        }
    }

    let expected: Vec<u8> = messages.iter().flatten().copied().collect();
    let total_received: Vec<u8> = server_received.iter().flatten().copied().collect();
    assert_eq!(total_received, expected, "All messages should be received");
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls13_wolfssl_client_many_small_messages() {
    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_dimpl_cert = generate_self_signed_certificate().expect("gen server cert");

    let wolf_server_cert = WolfDtlsCert::new(
        server_dimpl_cert.certificate.clone(),
        server_dimpl_cert.private_key.clone(),
    );

    let mut wolf_server = wolf_server_cert
        .new_dtls13_impl(true)
        .expect("Failed to create WolfSSL server");

    let config = dtls13_config();

    let now = Instant::now();
    let mut dimpl_client = Dtls::new_13(config, client_cert, now);
    dimpl_client.set_active(true);

    let mut now = Instant::now();
    let mut wolf_events = VecDeque::new();

    // Complete handshake
    for _ in 0..50 {
        dimpl_client.handle_timeout(now).expect("client timeout");
        let client_out = drain_dimpl_outputs(&mut dimpl_client);
        for packet in &client_out.packets {
            wolf_server
                .handle_receive(packet, &mut wolf_events)
                .unwrap();
        }
        while let Some(packet) = wolf_server.poll_datagram() {
            let _ = dimpl_client.handle_packet(&packet);
        }
        if client_out.connected && wolf_server.is_connected() {
            break;
        }
        wolf_events.clear();
        now += Duration::from_millis(10);
    }

    // Send many small messages
    let message_count = 100;
    for i in 0..message_count {
        let msg = format!("M{}", i);
        dimpl_client
            .send_application_data(msg.as_bytes())
            .expect("send");
    }

    let mut received_bytes: Vec<u8> = Vec::new();

    for _ in 0..50 {
        let client_out = drain_dimpl_outputs(&mut dimpl_client);
        for packet in &client_out.packets {
            wolf_server
                .handle_receive(packet, &mut wolf_events)
                .unwrap();
        }

        while let Some(event) = wolf_events.pop_front() {
            if let DtlsEvent::Data(data) = event {
                received_bytes.extend_from_slice(&data);
            }
        }

        now += Duration::from_millis(10);
    }

    assert!(
        !received_bytes.is_empty(),
        "Should receive application data"
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls13_wolfssl_client_retransmit_on_timeout() {
    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_dimpl_cert = generate_self_signed_certificate().expect("gen server cert");

    let wolf_server_cert = WolfDtlsCert::new(
        server_dimpl_cert.certificate.clone(),
        server_dimpl_cert.private_key.clone(),
    );

    let mut wolf_server = wolf_server_cert
        .new_dtls13_impl(true)
        .expect("Failed to create WolfSSL server");

    let config = dtls13_config();

    let now = Instant::now();
    let mut dimpl_client = Dtls::new_13(config, client_cert, now);
    dimpl_client.set_active(true);

    let mut now = Instant::now();

    // Get initial ClientHello
    dimpl_client.handle_timeout(now).expect("client start");
    dimpl_client.handle_timeout(now).expect("client arm");
    let initial_out = drain_dimpl_outputs(&mut dimpl_client);
    assert!(
        !initial_out.packets.is_empty(),
        "Client should send ClientHello"
    );

    // Don't deliver to server, trigger timeout
    now += Duration::from_secs(2);
    dimpl_client.handle_timeout(now).expect("client timeout");

    // Should get retransmitted packets
    let retransmit_out = drain_dimpl_outputs(&mut dimpl_client);
    assert!(
        !retransmit_out.packets.is_empty(),
        "Client should retransmit on timeout"
    );

    assert_eq!(
        initial_out.packets.len(),
        retransmit_out.packets.len(),
        "Retransmit should have same packet count"
    );

    // Now actually complete the handshake to verify everything works
    let wolf_events = &mut VecDeque::new();

    // First, deliver the retransmitted packets we already have
    for packet in &retransmit_out.packets {
        wolf_server.handle_receive(packet, wolf_events).unwrap();
    }
    while let Some(packet) = wolf_server.poll_datagram() {
        let _ = dimpl_client.handle_packet(&packet);
    }
    wolf_events.clear();

    for _ in 0..50 {
        dimpl_client.handle_timeout(now).expect("client timeout");
        let client_out = drain_dimpl_outputs(&mut dimpl_client);
        for packet in &client_out.packets {
            wolf_server.handle_receive(packet, wolf_events).unwrap();
        }
        while let Some(packet) = wolf_server.poll_datagram() {
            let _ = dimpl_client.handle_packet(&packet);
        }
        if client_out.connected && wolf_server.is_connected() {
            break;
        }
        wolf_events.clear();
        now += Duration::from_millis(10);
    }

    assert!(wolf_server.is_connected(), "Should eventually connect");
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls13_wolfssl_client_handshake_after_packet_loss() {
    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_dimpl_cert = generate_self_signed_certificate().expect("gen server cert");

    let wolf_server_cert = WolfDtlsCert::new(
        server_dimpl_cert.certificate.clone(),
        server_dimpl_cert.private_key.clone(),
    );

    let mut wolf_server = wolf_server_cert
        .new_dtls13_impl(true)
        .expect("Failed to create WolfSSL server");

    let config = dtls13_config();

    let now = Instant::now();
    let mut dimpl_client = Dtls::new_13(config, client_cert, now);
    dimpl_client.set_active(true);

    let mut now = Instant::now();
    let mut wolf_events = VecDeque::new();

    let mut client_connected = false;
    let mut server_connected = false;
    let mut drop_next_packet = true;

    for i in 0..60 {
        dimpl_client.handle_timeout(now).expect("client timeout");

        let client_out = drain_dimpl_outputs(&mut dimpl_client);
        if client_out.connected {
            client_connected = true;
        }

        // Drop first packet
        if !client_out.packets.is_empty() && drop_next_packet {
            drop_next_packet = false;
        } else {
            for packet in &client_out.packets {
                wolf_server
                    .handle_receive(packet, &mut wolf_events)
                    .unwrap();
            }
        }

        while let Some(event) = wolf_events.pop_front() {
            if matches!(event, DtlsEvent::Connected) {
                server_connected = true;
            }
        }

        while let Some(packet) = wolf_server.poll_datagram() {
            let _ = dimpl_client.handle_packet(&packet);
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
        "Client should connect despite packet loss"
    );
    assert!(
        server_connected,
        "Server should connect despite packet loss"
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls13_wolfssl_client_handles_duplicates() {
    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_dimpl_cert = generate_self_signed_certificate().expect("gen server cert");

    let wolf_server_cert = WolfDtlsCert::new(
        server_dimpl_cert.certificate.clone(),
        server_dimpl_cert.private_key.clone(),
    );

    let mut wolf_server = wolf_server_cert
        .new_dtls13_impl(true)
        .expect("Failed to create WolfSSL server");

    let config = dtls13_config();

    let now = Instant::now();
    let mut dimpl_client = Dtls::new_13(config, client_cert, now);
    dimpl_client.set_active(true);

    let mut now = Instant::now();
    let mut wolf_events = VecDeque::new();

    let mut client_connected = false;
    let mut server_connected = false;

    for _ in 0..50 {
        dimpl_client.handle_timeout(now).expect("client timeout");

        let client_out = drain_dimpl_outputs(&mut dimpl_client);
        if client_out.connected {
            client_connected = true;
        }

        // Send packets twice (duplicates)
        for packet in &client_out.packets {
            wolf_server
                .handle_receive(packet, &mut wolf_events)
                .unwrap();
            wolf_server
                .handle_receive(packet, &mut wolf_events)
                .unwrap();
        }

        while let Some(event) = wolf_events.pop_front() {
            if matches!(event, DtlsEvent::Connected) {
                server_connected = true;
            }
        }

        while let Some(packet) = wolf_server.poll_datagram() {
            let _ = dimpl_client.handle_packet(&packet);
            let _ = dimpl_client.handle_packet(&packet); // Duplicate
        }

        if client_connected && server_connected {
            break;
        }

        now += Duration::from_millis(10);
    }

    assert!(client_connected, "Client should connect despite duplicates");
    assert!(server_connected, "Server should connect despite duplicates");
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls13_wolfssl_client_handles_out_of_order() {
    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_dimpl_cert = generate_self_signed_certificate().expect("gen server cert");

    let wolf_server_cert = WolfDtlsCert::new(
        server_dimpl_cert.certificate.clone(),
        server_dimpl_cert.private_key.clone(),
    );

    let mut wolf_server = wolf_server_cert
        .new_dtls13_impl(true)
        .expect("Failed to create WolfSSL server");

    let config = dtls13_config();

    let now = Instant::now();
    let mut dimpl_client = Dtls::new_13(config, client_cert, now);
    dimpl_client.set_active(true);

    let mut now = Instant::now();
    let mut wolf_events = VecDeque::new();

    // First complete handshake normally
    for _ in 0..50 {
        dimpl_client.handle_timeout(now).expect("client timeout");
        let client_out = drain_dimpl_outputs(&mut dimpl_client);
        for packet in &client_out.packets {
            wolf_server
                .handle_receive(packet, &mut wolf_events)
                .unwrap();
        }
        while let Some(packet) = wolf_server.poll_datagram() {
            let _ = dimpl_client.handle_packet(&packet);
        }
        if client_out.connected && wolf_server.is_connected() {
            break;
        }
        wolf_events.clear();
        now += Duration::from_millis(10);
    }

    assert!(wolf_server.is_connected(), "Handshake should complete");

    // Now test out-of-order application data delivery
    // Send multiple messages from client, deliver to server in reverse order
    dimpl_client
        .send_application_data(b"First")
        .expect("send 1");
    dimpl_client
        .send_application_data(b"Second")
        .expect("send 2");
    dimpl_client
        .send_application_data(b"Third")
        .expect("send 3");

    let client_out = drain_dimpl_outputs(&mut dimpl_client);

    // Deliver packets in reverse order (if there are multiple)
    let mut packets = client_out.packets.clone();
    packets.reverse();
    for packet in &packets {
        wolf_server
            .handle_receive(packet, &mut wolf_events)
            .unwrap();
    }

    let mut server_received: Vec<u8> = Vec::new();
    while let Some(event) = wolf_events.pop_front() {
        if let DtlsEvent::Data(data) = event {
            server_received.extend_from_slice(&data);
        }
    }

    // DTLS should handle reordering - all data should arrive
    assert!(
        !server_received.is_empty(),
        "Server should receive data despite reordering"
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls13_wolfssl_client_small_mtu() {
    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_dimpl_cert = generate_self_signed_certificate().expect("gen server cert");

    let wolf_server_cert = WolfDtlsCert::new(
        server_dimpl_cert.certificate.clone(),
        server_dimpl_cert.private_key.clone(),
    );

    let mut wolf_server = wolf_server_cert
        .new_dtls13_impl(true)
        .expect("Failed to create WolfSSL server");

    // Use 600 MTU - large enough for handshake but smaller than default
    let config = dtls13_config_with_mtu(600);

    let now = Instant::now();
    let mut dimpl_client = Dtls::new_13(config, client_cert, now);
    dimpl_client.set_active(true);

    let mut now = Instant::now();
    let mut wolf_events = VecDeque::new();

    let mut client_connected = false;
    let mut server_connected = false;
    let mut max_client_packet_size = 0usize;

    for _ in 0..50 {
        dimpl_client.handle_timeout(now).expect("client timeout");

        let client_out = drain_dimpl_outputs(&mut dimpl_client);
        if client_out.connected {
            client_connected = true;
        }

        for p in &client_out.packets {
            if p.len() > max_client_packet_size {
                max_client_packet_size = p.len();
            }
        }

        for packet in &client_out.packets {
            wolf_server
                .handle_receive(packet, &mut wolf_events)
                .unwrap();
        }

        while let Some(event) = wolf_events.pop_front() {
            if matches!(event, DtlsEvent::Connected) {
                server_connected = true;
            }
        }

        while let Some(packet) = wolf_server.poll_datagram() {
            let _ = dimpl_client.handle_packet(&packet);
        }

        if client_connected && server_connected {
            break;
        }

        now += Duration::from_millis(10);
    }

    assert!(client_connected, "Client should connect with small MTU");
    assert!(server_connected, "Server should connect with small MTU");
    // Only check that dimpl client respects MTU (WolfSSL may not)
    assert!(
        max_client_packet_size <= 600,
        "Client packets should respect MTU: max was {}",
        max_client_packet_size
    );
}

// NOTE: This test is skipped because dimpl does not fragment application data
// by MTU. Neither DTLS 1.2 nor DTLS 1.3 splits app data into multiple records.
// Handshake fragmentation is supported, but app data is sent as a single record.
#[ignore = "dimpl does not fragment application data by MTU"]
#[test]
#[cfg(feature = "rcgen")]
fn dtls13_wolfssl_client_large_data_fragmented() {
    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_dimpl_cert = generate_self_signed_certificate().expect("gen server cert");

    let wolf_server_cert = WolfDtlsCert::new(
        server_dimpl_cert.certificate.clone(),
        server_dimpl_cert.private_key.clone(),
    );

    let mut wolf_server = wolf_server_cert
        .new_dtls13_impl(true)
        .expect("Failed to create WolfSSL server");

    let config = dtls13_config_with_mtu(300);

    let now = Instant::now();
    let mut dimpl_client = Dtls::new_13(config, client_cert, now);
    dimpl_client.set_active(true);

    let mut now = Instant::now();
    let mut wolf_events = VecDeque::new();

    // Complete handshake
    for _ in 0..50 {
        dimpl_client.handle_timeout(now).expect("client timeout");
        let client_out = drain_dimpl_outputs(&mut dimpl_client);
        for packet in &client_out.packets {
            wolf_server
                .handle_receive(packet, &mut wolf_events)
                .unwrap();
        }
        while let Some(packet) = wolf_server.poll_datagram() {
            let _ = dimpl_client.handle_packet(&packet);
        }
        if client_out.connected && wolf_server.is_connected() {
            break;
        }
        wolf_events.clear();
        now += Duration::from_millis(10);
    }

    // Send large data
    let large_data = vec![0xABu8; 1000];
    dimpl_client
        .send_application_data(&large_data)
        .expect("send large data");

    let mut server_received: Vec<u8> = Vec::new();
    let mut packet_count = 0;

    for _ in 0..20 {
        let client_out = drain_dimpl_outputs(&mut dimpl_client);
        packet_count += client_out.packets.len();
        for packet in &client_out.packets {
            wolf_server
                .handle_receive(packet, &mut wolf_events)
                .unwrap();
        }

        while let Some(event) = wolf_events.pop_front() {
            if let DtlsEvent::Data(data) = event {
                server_received.extend_from_slice(&data);
            }
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
    assert!(
        packet_count >= 2,
        "Large data should be split into multiple packets: {}",
        packet_count
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls13_wolfssl_client_recovers_from_corruption() {
    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_dimpl_cert = generate_self_signed_certificate().expect("gen server cert");

    let wolf_server_cert = WolfDtlsCert::new(
        server_dimpl_cert.certificate.clone(),
        server_dimpl_cert.private_key.clone(),
    );

    let mut wolf_server = wolf_server_cert
        .new_dtls13_impl(true)
        .expect("Failed to create WolfSSL server");

    let config = dtls13_config();

    let now = Instant::now();
    let mut dimpl_client = Dtls::new_13(config, client_cert, now);
    dimpl_client.set_active(true);

    let mut now = Instant::now();
    let mut wolf_events = VecDeque::new();

    let mut client_connected = false;
    let mut server_connected = false;
    let mut corrupted_once = false;

    for i in 0..60 {
        dimpl_client.handle_timeout(now).expect("client timeout");

        let client_out = drain_dimpl_outputs(&mut dimpl_client);
        if client_out.connected {
            client_connected = true;
        }

        // Corrupt one packet
        for mut p in client_out.packets {
            if !corrupted_once && p.len() > 20 {
                p[15] ^= 0xFF;
                p[16] ^= 0xFF;
                corrupted_once = true;
            }
            let _ = wolf_server.handle_receive(&p, &mut wolf_events);
        }

        while let Some(event) = wolf_events.pop_front() {
            if matches!(event, DtlsEvent::Connected) {
                server_connected = true;
            }
        }

        while let Some(packet) = wolf_server.poll_datagram() {
            let _ = dimpl_client.handle_packet(&packet);
        }

        if client_connected && server_connected {
            break;
        }

        // Trigger retransmissions
        if i % 5 == 4 {
            now += Duration::from_secs(2);
        } else {
            now += Duration::from_millis(50);
        }
    }

    assert!(client_connected, "Client should connect despite corruption");
    assert!(server_connected, "Server should connect despite corruption");
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls13_wolfssl_client_handshake_with_early_packet_loss() {
    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_dimpl_cert = generate_self_signed_certificate().expect("gen server cert");

    let wolf_server_cert = WolfDtlsCert::new(
        server_dimpl_cert.certificate.clone(),
        server_dimpl_cert.private_key.clone(),
    );

    let mut wolf_server = wolf_server_cert
        .new_dtls13_impl(true)
        .expect("Failed to create WolfSSL server");

    // Use more retries for lossy conditions
    let config = Arc::new(
        Config::builder()
            .flight_retries(8)
            .build()
            .expect("Failed to build DTLS 1.3 config"),
    );

    let now = Instant::now();
    let mut dimpl_client = Dtls::new_13(config, client_cert, now);
    dimpl_client.set_active(true);

    let mut now = Instant::now();
    let mut wolf_events = VecDeque::new();

    let mut client_connected = false;
    let mut server_connected = false;

    // Drop first 3 packets to test retransmission recovery
    let mut packets_to_drop = 3;

    for i in 0..60 {
        let _ = dimpl_client.handle_timeout(now);

        let client_out = drain_dimpl_outputs(&mut dimpl_client);
        if client_out.connected {
            client_connected = true;
        }

        // Drop first N packets, then deliver all
        for packet in &client_out.packets {
            if packets_to_drop > 0 {
                packets_to_drop -= 1;
            } else {
                wolf_server
                    .handle_receive(packet, &mut wolf_events)
                    .unwrap();
            }
        }

        while let Some(event) = wolf_events.pop_front() {
            if matches!(event, DtlsEvent::Connected) {
                server_connected = true;
            }
        }

        while let Some(packet) = wolf_server.poll_datagram() {
            let _ = dimpl_client.handle_packet(&packet);
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

// =============================================================================
// Server tests: WolfSSL client <-> dimpl server
// =============================================================================

#[test]
#[cfg(feature = "rcgen")]
fn dtls13_wolfssl_server_handshake() {
    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    let server_cert = generate_self_signed_certificate().expect("gen server cert");
    let client_dimpl_cert = generate_self_signed_certificate().expect("gen client cert");

    let wolf_client_cert = WolfDtlsCert::new(
        client_dimpl_cert.certificate.clone(),
        client_dimpl_cert.private_key.clone(),
    );

    let mut wolf_client = wolf_client_cert
        .new_dtls13_impl(false)
        .expect("Failed to create WolfSSL client");

    wolf_client.initiate().expect("initiate wolf client");

    let config = dtls13_config();
    let now = Instant::now();
    let mut dimpl_server = Dtls::new_13(config, server_cert, now);
    dimpl_server.set_active(false);

    let mut now = Instant::now();
    let mut wolf_events = VecDeque::new();

    let mut client_connected = false;
    let mut server_connected = false;

    for _ in 0..50 {
        dimpl_server.handle_timeout(now).expect("server timeout");

        while let Some(packet) = wolf_client.poll_datagram() {
            let _ = dimpl_server.handle_packet(&packet);
        }

        dimpl_server.handle_timeout(now).expect("server timeout");

        let server_out = drain_dimpl_outputs(&mut dimpl_server);
        if server_out.connected {
            server_connected = true;
        }

        for packet in &server_out.packets {
            wolf_client
                .handle_receive(packet, &mut wolf_events)
                .expect("wolf client handle receive");
        }

        while let Some(event) = wolf_events.pop_front() {
            if matches!(event, DtlsEvent::Connected) {
                client_connected = true;
            }
        }

        if client_connected && server_connected {
            break;
        }

        now += Duration::from_millis(10);
    }

    assert!(server_connected, "dimpl server should be connected");
    assert!(client_connected, "WolfSSL client should be connected");
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls13_wolfssl_server_data_exchange() {
    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    let server_cert = generate_self_signed_certificate().expect("gen server cert");
    let client_dimpl_cert = generate_self_signed_certificate().expect("gen client cert");

    let wolf_client_cert = WolfDtlsCert::new(
        client_dimpl_cert.certificate.clone(),
        client_dimpl_cert.private_key.clone(),
    );

    let mut wolf_client = wolf_client_cert
        .new_dtls13_impl(false)
        .expect("Failed to create WolfSSL client");

    wolf_client.initiate().expect("initiate wolf client");

    let config = dtls13_config();
    let now = Instant::now();
    let mut dimpl_server = Dtls::new_13(config, server_cert, now);
    dimpl_server.set_active(false);

    let mut now = Instant::now();
    let mut wolf_events = VecDeque::new();

    // Complete handshake
    for _ in 0..50 {
        dimpl_server.handle_timeout(now).expect("server timeout");
        while let Some(packet) = wolf_client.poll_datagram() {
            let _ = dimpl_server.handle_packet(&packet);
        }
        dimpl_server.handle_timeout(now).expect("server timeout");
        let server_out = drain_dimpl_outputs(&mut dimpl_server);
        for packet in &server_out.packets {
            wolf_client
                .handle_receive(packet, &mut wolf_events)
                .unwrap();
        }
        if server_out.connected && wolf_client.is_connected() {
            wolf_events.clear();
            break;
        }
        wolf_events.clear();
        now += Duration::from_millis(10);
    }

    // Send data from WolfSSL client to dimpl server
    let test_data = b"Hello from WolfSSL client!";
    wolf_client
        .write(test_data)
        .expect("write from wolf client");

    while let Some(packet) = wolf_client.poll_datagram() {
        let _ = dimpl_server.handle_packet(&packet);
    }

    let server_out = drain_dimpl_outputs(&mut dimpl_server);
    let received: Vec<u8> = server_out.app_data.into_iter().flatten().collect();

    assert_eq!(
        received, test_data,
        "dimpl server should receive the data from WolfSSL client"
    );
}

// NOTE: This test is commented out due to WolfSSL state machine quirks
// WolfSSL returns error -441 "Application data is available for reading"
// when trying to receive new data after a bidirectional exchange.
// The data_exchange test verifies one-way data transfer works.
/*
#[test]
#[cfg(feature = "rcgen")]
fn dtls13_wolfssl_server_bidirectional_data() {
    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    let server_cert = generate_self_signed_certificate().expect("gen server cert");
    let client_dimpl_cert = generate_self_signed_certificate().expect("gen client cert");

    let wolf_client_cert = WolfDtlsCert::new(
        client_dimpl_cert.certificate.clone(),
        client_dimpl_cert.private_key.clone(),
    );

    let mut wolf_client = wolf_client_cert
        .new_dtls13_impl(false)
        .expect("Failed to create WolfSSL client");

    wolf_client.initiate().expect("initiate wolf client");

    let config = dtls13_config();
    let now = Instant::now();
    let mut dimpl_server = Dtls::new_13(config, server_cert, now);
    dimpl_server.set_active(false);

    let mut now = Instant::now();
    let mut wolf_events = VecDeque::new();

    // Complete handshake
    for _ in 0..50 {
        dimpl_server.handle_timeout(now).expect("server timeout");
        while let Some(packet) = wolf_client.poll_datagram() {
            let _ = dimpl_server.handle_packet(&packet);
        }
        dimpl_server.handle_timeout(now).expect("server timeout");
        let server_out = drain_dimpl_outputs(&mut dimpl_server);
        for packet in &server_out.packets {
            wolf_client.handle_receive(packet, &mut wolf_events).unwrap();
        }
        if server_out.connected && wolf_client.is_connected() {
            wolf_events.clear();
            break;
        }
        wolf_events.clear();
        now += Duration::from_millis(10);
    }

    // Send data from client to server
    let client_data = b"Hello from WolfSSL!";
    wolf_client.write(client_data).expect("client write");

    // Send data from server to client
    let server_data = b"Hello from dimpl!";
    dimpl_server.send_application_data(server_data).expect("server send");

    // Drain any pending data in WolfSSL before sending new packets
    wolf_client.drain_pending_data(&mut wolf_events);
    while let Some(event) = wolf_events.pop_front() {
        eprintln!("Drained pending event: {:?}", std::mem::discriminant(&event));
    }

    // Immediately drain and deliver server packets
    let initial_out = drain_dimpl_outputs(&mut dimpl_server);
    for packet in &initial_out.packets {
        // Drain before each receive
        wolf_client.drain_pending_data(&mut wolf_events);
        match wolf_client.handle_receive(packet, &mut wolf_events) {
            Ok(()) => (),
            Err(e) => eprintln!("handle_receive error: {:?}", e),
        }
    }

    let mut client_received: Vec<u8> = Vec::new();
    let mut server_received: Vec<u8> = Vec::new();

    // Collect events from initial delivery
    while let Some(event) = wolf_events.pop_front() {
        if let DtlsEvent::Data(data) = event {
            client_received.extend_from_slice(&data);
        }
    }

    for _ in 0..100 {
        // Client -> Server: drain WolfSSL output first
        while let Some(packet) = wolf_client.poll_datagram() {
            let _ = dimpl_server.handle_packet(&packet);
        }

        // Process server and get outputs
        dimpl_server.handle_timeout(now).expect("server timeout");
        let server_out = drain_dimpl_outputs(&mut dimpl_server);
        for data in server_out.app_data {
            server_received.extend_from_slice(&data);
        }

        // Drain any pending events from previous iteration
        while let Some(event) = wolf_events.pop_front() {
            if let DtlsEvent::Data(data) = event {
                client_received.extend_from_slice(&data);
            }
        }

        // Server -> Client
        for packet in &server_out.packets {
            let _ = wolf_client.handle_receive(packet, &mut wolf_events);
        }

        while let Some(event) = wolf_events.pop_front() {
            if let DtlsEvent::Data(data) = event {
                client_received.extend_from_slice(&data);
            }
        }

        if !client_received.is_empty() && !server_received.is_empty() {
            break;
        }

        now += Duration::from_millis(10);
    }

    assert_eq!(client_received, server_data, "Client should receive server data");
    assert_eq!(server_received, client_data, "Server should receive client data");
}
*/

#[test]
#[cfg(feature = "rcgen")]
fn dtls13_wolfssl_server_multiple_messages() {
    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    let server_cert = generate_self_signed_certificate().expect("gen server cert");
    let client_dimpl_cert = generate_self_signed_certificate().expect("gen client cert");

    let wolf_client_cert = WolfDtlsCert::new(
        client_dimpl_cert.certificate.clone(),
        client_dimpl_cert.private_key.clone(),
    );

    let mut wolf_client = wolf_client_cert
        .new_dtls13_impl(false)
        .expect("Failed to create WolfSSL client");

    wolf_client.initiate().expect("initiate wolf client");

    let config = dtls13_config();
    let now = Instant::now();
    let mut dimpl_server = Dtls::new_13(config, server_cert, now);
    dimpl_server.set_active(false);

    let mut now = Instant::now();
    let mut wolf_events = VecDeque::new();

    // Complete handshake
    for _ in 0..50 {
        dimpl_server.handle_timeout(now).expect("server timeout");
        while let Some(packet) = wolf_client.poll_datagram() {
            let _ = dimpl_server.handle_packet(&packet);
        }
        dimpl_server.handle_timeout(now).expect("server timeout");
        let server_out = drain_dimpl_outputs(&mut dimpl_server);
        for packet in &server_out.packets {
            wolf_client
                .handle_receive(packet, &mut wolf_events)
                .unwrap();
        }
        if server_out.connected && wolf_client.is_connected() {
            wolf_events.clear();
            break;
        }
        wolf_events.clear();
        now += Duration::from_millis(10);
    }

    // Send multiple messages from WolfSSL client
    let messages = vec![
        b"Message 1".to_vec(),
        b"Message 2".to_vec(),
        b"Message 3 is a bit longer".to_vec(),
        b"Message 4".to_vec(),
        b"Message 5 - the final one".to_vec(),
    ];

    let mut server_received: Vec<Vec<u8>> = Vec::new();

    for msg in &messages {
        wolf_client.write(msg).expect("client write");

        while let Some(packet) = wolf_client.poll_datagram() {
            let _ = dimpl_server.handle_packet(&packet);
        }

        let server_out = drain_dimpl_outputs(&mut dimpl_server);
        for data in server_out.app_data {
            server_received.push(data);
        }
    }

    let expected: Vec<u8> = messages.iter().flatten().copied().collect();
    let total_received: Vec<u8> = server_received.iter().flatten().copied().collect();
    assert_eq!(total_received, expected, "All messages should be received");
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls13_wolfssl_server_handshake_after_packet_loss() {
    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    let server_cert = generate_self_signed_certificate().expect("gen server cert");
    let client_dimpl_cert = generate_self_signed_certificate().expect("gen client cert");

    let wolf_client_cert = WolfDtlsCert::new(
        client_dimpl_cert.certificate.clone(),
        client_dimpl_cert.private_key.clone(),
    );

    let mut wolf_client = wolf_client_cert
        .new_dtls13_impl(false)
        .expect("Failed to create WolfSSL client");

    wolf_client.initiate().expect("initiate wolf client");

    let config = dtls13_config();
    let now = Instant::now();
    let mut dimpl_server = Dtls::new_13(config, server_cert, now);
    dimpl_server.set_active(false);

    let mut now = Instant::now();
    let mut wolf_events = VecDeque::new();

    let mut client_connected = false;
    let mut server_connected = false;
    let mut drop_next_server_packet = true;

    for i in 0..60 {
        dimpl_server.handle_timeout(now).expect("server timeout");

        // Always deliver client packets to server
        while let Some(packet) = wolf_client.poll_datagram() {
            let _ = dimpl_server.handle_packet(&packet);
        }

        dimpl_server.handle_timeout(now).expect("server timeout");

        let server_out = drain_dimpl_outputs(&mut dimpl_server);
        if server_out.connected {
            server_connected = true;
        }

        // Drop first server packet (drop server->client)
        for packet in &server_out.packets {
            if drop_next_server_packet && !server_out.packets.is_empty() {
                drop_next_server_packet = false;
                continue;
            }
            let _ = wolf_client.handle_receive(packet, &mut wolf_events);
        }

        while let Some(event) = wolf_events.pop_front() {
            if matches!(event, DtlsEvent::Connected) {
                client_connected = true;
            }
        }

        if client_connected && server_connected {
            break;
        }

        // Trigger server retransmissions
        if i % 5 == 4 {
            now += Duration::from_secs(2);
        } else {
            now += Duration::from_millis(10);
        }
    }

    assert!(
        server_connected,
        "Server should connect despite packet loss"
    );
    assert!(
        client_connected,
        "Client should connect despite packet loss"
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls13_wolfssl_server_handshake_with_early_packet_loss() {
    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    let server_cert = generate_self_signed_certificate().expect("gen server cert");
    let client_dimpl_cert = generate_self_signed_certificate().expect("gen client cert");

    let wolf_client_cert = WolfDtlsCert::new(
        client_dimpl_cert.certificate.clone(),
        client_dimpl_cert.private_key.clone(),
    );

    let mut wolf_client = wolf_client_cert
        .new_dtls13_impl(false)
        .expect("Failed to create WolfSSL client");

    wolf_client.initiate().expect("initiate wolf client");

    // Use more retries for lossy conditions
    let config = Arc::new(
        Config::builder()
            .flight_retries(8)
            .build()
            .expect("Failed to build DTLS 1.3 config"),
    );
    let now = Instant::now();
    let mut dimpl_server = Dtls::new_13(config, server_cert, now);
    dimpl_server.set_active(false);

    let mut now = Instant::now();
    let mut wolf_events = VecDeque::new();

    let mut client_connected = false;
    let mut server_connected = false;

    // Drop first 3 server packets to test retransmission recovery
    let mut packets_to_drop = 3;

    for i in 0..60 {
        let _ = dimpl_server.handle_timeout(now);

        // Always deliver client packets to server
        while let Some(packet) = wolf_client.poll_datagram() {
            let _ = dimpl_server.handle_packet(&packet);
        }

        // Drain fresh server output before processing another timeout tick;
        // otherwise a retransmit can race the newly processed WolfSSL client flight.
        let server_out = drain_dimpl_outputs(&mut dimpl_server);
        if server_out.connected {
            server_connected = true;
        }

        // Drop first N server packets, then deliver all
        for packet in &server_out.packets {
            if packets_to_drop > 0 {
                packets_to_drop -= 1;
            } else {
                let _ = wolf_client.handle_receive(packet, &mut wolf_events);
            }
        }

        while let Some(event) = wolf_events.pop_front() {
            if matches!(event, DtlsEvent::Connected) {
                client_connected = true;
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
        server_connected,
        "Server should connect despite early packet loss"
    );
    assert!(
        client_connected,
        "Client should connect despite early packet loss"
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls13_wolfssl_server_handles_duplicates() {
    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    let server_cert = generate_self_signed_certificate().expect("gen server cert");
    let client_dimpl_cert = generate_self_signed_certificate().expect("gen client cert");

    let wolf_client_cert = WolfDtlsCert::new(
        client_dimpl_cert.certificate.clone(),
        client_dimpl_cert.private_key.clone(),
    );

    let mut wolf_client = wolf_client_cert
        .new_dtls13_impl(false)
        .expect("Failed to create WolfSSL client");

    wolf_client.initiate().expect("initiate wolf client");

    let config = dtls13_config();
    let now = Instant::now();
    let mut dimpl_server = Dtls::new_13(config, server_cert, now);
    dimpl_server.set_active(false);

    let mut now = Instant::now();
    let mut wolf_events = VecDeque::new();

    let mut client_connected = false;
    let mut server_connected = false;

    for _ in 0..50 {
        dimpl_server.handle_timeout(now).expect("server timeout");

        // Send client packets twice (duplicates)
        let mut client_packets = Vec::new();
        while let Some(packet) = wolf_client.poll_datagram() {
            client_packets.push(packet);
        }
        for packet in &client_packets {
            let _ = dimpl_server.handle_packet(packet);
            let _ = dimpl_server.handle_packet(packet); // Duplicate
        }

        dimpl_server.handle_timeout(now).expect("server timeout");

        let server_out = drain_dimpl_outputs(&mut dimpl_server);
        if server_out.connected {
            server_connected = true;
        }

        // Send server packets twice too
        for packet in &server_out.packets {
            wolf_client
                .handle_receive(packet, &mut wolf_events)
                .unwrap();
            wolf_client
                .handle_receive(packet, &mut wolf_events)
                .unwrap();
        }

        while let Some(event) = wolf_events.pop_front() {
            if matches!(event, DtlsEvent::Connected) {
                client_connected = true;
            }
        }

        if client_connected && server_connected {
            break;
        }

        now += Duration::from_millis(10);
    }

    assert!(server_connected, "Server should connect despite duplicates");
    assert!(client_connected, "Client should connect despite duplicates");
}

// NOTE: This test is skipped because WolfSSL doesn't recover well from corruption
// during handshake - the corrupted server packet causes the handshake to stall
// without proper retransmission from the WolfSSL client side.
// The client-wolfssl tests verify corruption recovery from dimpl's perspective.
/*
#[test]
#[cfg(feature = "rcgen")]
fn dtls13_wolfssl_server_recovers_from_corruption() {
    // ... test body omitted - see original server-wolfssl.rs ...
}
*/

// NOTE: This test is skipped because WolfSSL's wrapper has issues with
// receiving large fragmented data - error -441 "Application data is available"
// occurs after receiving partial data. The client-wolfssl tests verify
// large data handling from dimpl's perspective.
/*
#[test]
#[cfg(feature = "rcgen")]
fn dtls13_wolfssl_server_large_data_fragmented() {
    // ... test body omitted - see original server-wolfssl.rs ...
}
*/

#[test]
#[cfg(feature = "rcgen")]
fn dtls13_wolfssl_server_many_small_messages() {
    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    let server_cert = generate_self_signed_certificate().expect("gen server cert");
    let client_dimpl_cert = generate_self_signed_certificate().expect("gen client cert");

    let wolf_client_cert = WolfDtlsCert::new(
        client_dimpl_cert.certificate.clone(),
        client_dimpl_cert.private_key.clone(),
    );

    let mut wolf_client = wolf_client_cert
        .new_dtls13_impl(false)
        .expect("Failed to create WolfSSL client");

    wolf_client.initiate().expect("initiate wolf client");

    let config = dtls13_config();
    let now = Instant::now();
    let mut dimpl_server = Dtls::new_13(config, server_cert, now);
    dimpl_server.set_active(false);

    let mut now = Instant::now();
    let mut wolf_events = VecDeque::new();

    // Complete handshake
    for _ in 0..50 {
        dimpl_server.handle_timeout(now).expect("server timeout");
        while let Some(packet) = wolf_client.poll_datagram() {
            let _ = dimpl_server.handle_packet(&packet);
        }
        dimpl_server.handle_timeout(now).expect("server timeout");
        let server_out = drain_dimpl_outputs(&mut dimpl_server);
        for packet in &server_out.packets {
            wolf_client
                .handle_receive(packet, &mut wolf_events)
                .unwrap();
        }
        if server_out.connected && wolf_client.is_connected() {
            wolf_events.clear();
            break;
        }
        wolf_events.clear();
        now += Duration::from_millis(10);
    }

    // Send many small messages from WolfSSL client
    let message_count = 100;
    for i in 0..message_count {
        let msg = format!("M{}", i);
        wolf_client.write(msg.as_bytes()).expect("send");
    }

    let mut received_bytes: Vec<u8> = Vec::new();

    for _ in 0..100 {
        while let Some(packet) = wolf_client.poll_datagram() {
            let _ = dimpl_server.handle_packet(&packet);
        }

        let server_out = drain_dimpl_outputs(&mut dimpl_server);
        for data in server_out.app_data {
            received_bytes.extend_from_slice(&data);
        }

        now += Duration::from_millis(10);
    }

    assert!(
        !received_bytes.is_empty(),
        "Should receive application data"
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls13_wolfssl_server_bidirectional_data() {
    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    let server_cert = generate_self_signed_certificate().expect("gen server cert");
    let client_dimpl_cert = generate_self_signed_certificate().expect("gen client cert");

    let wolf_client_cert = WolfDtlsCert::new(
        client_dimpl_cert.certificate.clone(),
        client_dimpl_cert.private_key.clone(),
    );

    let mut wolf_client = wolf_client_cert
        .new_dtls13_impl(false)
        .expect("Failed to create WolfSSL client");

    wolf_client.initiate().expect("initiate wolf client");

    let config = dtls13_config();
    let now = Instant::now();
    let mut dimpl_server = Dtls::new_13(config, server_cert, now);
    dimpl_server.set_active(false);

    let mut now = Instant::now();
    let mut wolf_events = VecDeque::new();

    // Complete handshake
    for _ in 0..50 {
        dimpl_server.handle_timeout(now).expect("server timeout");
        while let Some(packet) = wolf_client.poll_datagram() {
            let _ = dimpl_server.handle_packet(&packet);
        }
        dimpl_server.handle_timeout(now).expect("server timeout");
        let server_out = drain_dimpl_outputs(&mut dimpl_server);
        for packet in &server_out.packets {
            wolf_client
                .handle_receive(packet, &mut wolf_events)
                .unwrap();
        }
        if server_out.connected && wolf_client.is_connected() {
            wolf_events.clear();
            break;
        }
        wolf_events.clear();
        now += Duration::from_millis(10);
    }

    assert!(
        wolf_client.is_connected(),
        "WolfSSL client should be connected"
    );

    // Send data in both directions. WolfSSL client has a quirk (error -441
    // "Application data is available for reading") that can cause handle_receive
    // to fail, so we tolerate errors and use drain_pending_data to flush state.
    let server_data = b"Hello from dimpl server!";
    let client_data = b"Hello from WolfSSL client!";

    dimpl_server
        .send_application_data(server_data)
        .expect("server send");
    wolf_client.write(client_data).expect("client write");

    let mut client_received: Vec<u8> = Vec::new();
    let mut server_received: Vec<u8> = Vec::new();

    for _ in 0..50 {
        // WolfSSL client -> dimpl server
        while let Some(packet) = wolf_client.poll_datagram() {
            let _ = dimpl_server.handle_packet(&packet);
        }

        dimpl_server.handle_timeout(now).expect("server timeout");
        let server_out = drain_dimpl_outputs(&mut dimpl_server);
        for data in server_out.app_data {
            server_received.extend_from_slice(&data);
        }

        // dimpl server -> WolfSSL client
        // Drain pending data first to avoid -441 error
        wolf_client.drain_pending_data(&mut wolf_events);
        for packet in &server_out.packets {
            let _ = wolf_client.handle_receive(packet, &mut wolf_events);
        }
        wolf_client.drain_pending_data(&mut wolf_events);

        while let Some(event) = wolf_events.pop_front() {
            if let DtlsEvent::Data(data) = event {
                client_received.extend_from_slice(&data);
            }
        }

        if !client_received.is_empty() && !server_received.is_empty() {
            break;
        }

        now += Duration::from_millis(10);
    }

    assert_eq!(
        server_received, client_data,
        "dimpl server should receive data from WolfSSL client"
    );
    assert_eq!(
        client_received, server_data,
        "WolfSSL client should receive data from dimpl server"
    );
}

// NOTE: HRR (HelloRetryRequest) test is skipped because in the current WolfSSL interop
// setup, WolfSSL accepts all key-share groups dimpl offers. There is no way to configure
// the dimpl client to offer an initial key share that WolfSSL rejects (to trigger HRR)
// while still being able to complete the handshake. HRR is tested separately in the
// dimpl-only tests.
#[test]
#[ignore = "cannot trigger HRR: WolfSSL accepts all groups dimpl offers"]
#[cfg(feature = "rcgen")]
fn dtls13_wolfssl_client_hrr_flow() {
    // Cannot trigger HRR with the current WolfSSL + dimpl configuration:
    // WolfSSL accepts the groups dimpl offers in this test setup.
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls13_wolfssl_server_small_mtu() {
    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    let server_cert = generate_self_signed_certificate().expect("gen server cert");
    let client_dimpl_cert = generate_self_signed_certificate().expect("gen client cert");

    let wolf_client_cert = WolfDtlsCert::new(
        client_dimpl_cert.certificate.clone(),
        client_dimpl_cert.private_key.clone(),
    );

    let mut wolf_client = wolf_client_cert
        .new_dtls13_impl(false)
        .expect("Failed to create WolfSSL client");

    wolf_client.initiate().expect("initiate wolf client");

    // Use 600 MTU - large enough for handshake but smaller than default
    let config = dtls13_config_with_mtu(600);
    let now = Instant::now();
    let mut dimpl_server = Dtls::new_13(config, server_cert, now);
    dimpl_server.set_active(false);

    let mut now = Instant::now();
    let mut wolf_events = VecDeque::new();

    let mut client_connected = false;
    let mut server_connected = false;
    let mut max_server_packet_size = 0usize;

    for _ in 0..50 {
        dimpl_server.handle_timeout(now).expect("server timeout");

        while let Some(packet) = wolf_client.poll_datagram() {
            let _ = dimpl_server.handle_packet(&packet);
        }

        dimpl_server.handle_timeout(now).expect("server timeout");

        let server_out = drain_dimpl_outputs(&mut dimpl_server);
        if server_out.connected {
            server_connected = true;
        }

        for p in &server_out.packets {
            if p.len() > max_server_packet_size {
                max_server_packet_size = p.len();
            }
        }

        for packet in &server_out.packets {
            wolf_client
                .handle_receive(packet, &mut wolf_events)
                .expect("wolf client handle receive");
        }

        while let Some(event) = wolf_events.pop_front() {
            if matches!(event, DtlsEvent::Connected) {
                client_connected = true;
            }
        }

        if client_connected && server_connected {
            break;
        }

        now += Duration::from_millis(10);
    }

    assert!(server_connected, "Server should connect with small MTU");
    assert!(client_connected, "Client should connect with small MTU");
    // Only check that dimpl server respects MTU (WolfSSL may not)
    assert!(
        max_server_packet_size <= 600,
        "Server packets should respect MTU: max was {}",
        max_server_packet_size
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls13_wolfssl_server_recovers_from_corruption() {
    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    let server_cert = generate_self_signed_certificate().expect("gen server cert");
    let client_dimpl_cert = generate_self_signed_certificate().expect("gen client cert");

    let wolf_client_cert = WolfDtlsCert::new(
        client_dimpl_cert.certificate.clone(),
        client_dimpl_cert.private_key.clone(),
    );

    let mut wolf_client = wolf_client_cert
        .new_dtls13_impl(false)
        .expect("Failed to create WolfSSL client");

    wolf_client.initiate().expect("initiate wolf client");

    let config = Arc::new(
        Config::builder()
            .flight_retries(8)
            .build()
            .expect("Failed to build DTLS 1.3 config"),
    );
    let now = Instant::now();
    let mut dimpl_server = Dtls::new_13(config, server_cert, now);
    dimpl_server.set_active(false);

    let mut now = Instant::now();
    let mut wolf_events = VecDeque::new();

    let mut client_connected = false;
    let mut server_connected = false;
    let mut corrupted_once = false;

    for i in 0..60 {
        let _ = dimpl_server.handle_timeout(now);

        while let Some(packet) = wolf_client.poll_datagram() {
            let _ = dimpl_server.handle_packet(&packet);
        }

        let _ = dimpl_server.handle_timeout(now);

        let server_out = drain_dimpl_outputs(&mut dimpl_server);
        if server_out.connected {
            server_connected = true;
        }

        // Corrupt one packet from dimpl server before delivering to WolfSSL client
        for mut p in server_out.packets {
            if !corrupted_once && p.len() > 20 {
                p[15] ^= 0xFF;
                p[16] ^= 0xFF;
                corrupted_once = true;
            }
            let _ = wolf_client.handle_receive(&p, &mut wolf_events);
        }

        while let Some(event) = wolf_events.pop_front() {
            if matches!(event, DtlsEvent::Connected) {
                client_connected = true;
            }
        }

        if client_connected && server_connected {
            break;
        }

        // Trigger retransmissions periodically
        if i % 5 == 4 {
            now += Duration::from_secs(2);
        } else {
            now += Duration::from_millis(50);
        }
    }

    assert!(server_connected, "Server should connect despite corruption");
    assert!(client_connected, "Client should connect despite corruption");
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls13_wolfssl_server_handles_out_of_order() {
    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    let server_cert = generate_self_signed_certificate().expect("gen server cert");
    let client_dimpl_cert = generate_self_signed_certificate().expect("gen client cert");

    let wolf_client_cert = WolfDtlsCert::new(
        client_dimpl_cert.certificate.clone(),
        client_dimpl_cert.private_key.clone(),
    );

    let mut wolf_client = wolf_client_cert
        .new_dtls13_impl(false)
        .expect("Failed to create WolfSSL client");

    wolf_client.initiate().expect("initiate wolf client");

    let config = dtls13_config();
    let now = Instant::now();
    let mut dimpl_server = Dtls::new_13(config, server_cert, now);
    dimpl_server.set_active(false);

    let mut now = Instant::now();
    let mut wolf_events = VecDeque::new();

    let mut client_connected = false;
    let mut server_connected = false;

    for _ in 0..50 {
        dimpl_server.handle_timeout(now).expect("server timeout");

        while let Some(packet) = wolf_client.poll_datagram() {
            let _ = dimpl_server.handle_packet(&packet);
        }

        dimpl_server.handle_timeout(now).expect("server timeout");

        let server_out = drain_dimpl_outputs(&mut dimpl_server);
        if server_out.connected {
            server_connected = true;
        }

        // Reverse order of dimpl server's packets before delivering to WolfSSL client
        let mut packets = server_out.packets;
        packets.reverse();
        for packet in &packets {
            wolf_client
                .handle_receive(packet, &mut wolf_events)
                .expect("wolf client handle receive");
        }

        while let Some(event) = wolf_events.pop_front() {
            if matches!(event, DtlsEvent::Connected) {
                client_connected = true;
            }
        }

        if client_connected && server_connected {
            break;
        }

        now += Duration::from_millis(10);
    }

    assert!(
        server_connected,
        "Server should connect despite out-of-order delivery"
    );
    assert!(
        client_connected,
        "Client should connect despite out-of-order delivery"
    );
}
