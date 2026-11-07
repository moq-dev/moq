//! DTLS 1.2 application data tests.

use std::sync::Arc;
use std::time::{Duration, Instant};

use dimpl::Dtls;

use crate::common::*;

#[test]
#[cfg(feature = "rcgen")]
fn dtls12_application_data_exchange() {
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

    let client_data = b"hello";
    let server_data = b"world";

    let mut client_connected = false;
    let mut server_connected = false;
    let mut client_received: Vec<u8> = Vec::new();
    let mut server_received: Vec<u8> = Vec::new();
    let mut client_sent = false;
    let mut server_sent = false;

    for _ in 0..50 {
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

        // Collect received app data
        for data in &client_out.app_data {
            client_received.extend_from_slice(data);
        }
        for data in &server_out.app_data {
            server_received.extend_from_slice(data);
        }

        deliver_packets(&client_out.packets, &mut server);
        deliver_packets(&server_out.packets, &mut client);

        // Send data once connected
        if client_connected && !client_sent {
            client
                .send_application_data(client_data)
                .expect("client send");
            client_sent = true;
        }
        if server_connected && !server_sent {
            server
                .send_application_data(server_data)
                .expect("server send");
            server_sent = true;
        }

        if !client_received.is_empty() && !server_received.is_empty() {
            break;
        }

        now += Duration::from_millis(10);
    }

    assert!(client_connected, "Client should be connected");
    assert!(server_connected, "Server should be connected");
    assert_eq!(
        server_received, client_data,
        "Server should receive client's data"
    );
    assert_eq!(
        client_received, server_data,
        "Client should receive server's data"
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls12_multiple_application_data_messages() {
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

    // First complete handshake
    let mut client_connected = false;
    let mut server_connected = false;

    for _ in 0..50 {
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

    // Now send 5 distinct messages
    let messages = vec![
        b"Message 1".to_vec(),
        b"Message 2".to_vec(),
        b"Message 3 is a bit longer".to_vec(),
        b"Message 4".to_vec(),
        b"Message 5 - the final one".to_vec(),
    ];

    for msg in &messages {
        client.send_application_data(msg).expect("client send");
    }

    let mut server_received: Vec<Vec<u8>> = Vec::new();

    for _ in 0..20 {
        client.handle_timeout(now).expect("client timeout");
        server.handle_timeout(now).expect("server timeout");

        let client_out = drain_outputs(&mut client);
        deliver_packets(&client_out.packets, &mut server);

        let server_out = drain_outputs(&mut server);
        for data in server_out.app_data {
            server_received.push(data);
        }

        if server_received.len() >= messages.len() {
            break;
        }

        now += Duration::from_millis(10);
    }

    // Flatten and compare to verify all messages received in order
    let expected: Vec<u8> = messages.iter().flatten().copied().collect();
    let received: Vec<u8> = server_received.iter().flatten().copied().collect();

    assert_eq!(
        received, expected,
        "All 5 messages should be received in order"
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls12_bidirectional_data_exchange() {
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

    for _ in 0..50 {
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

    // Exchange data in both directions simultaneously for 10 rounds
    let rounds = 10;
    let mut client_received_count = 0;
    let mut server_received_count = 0;

    for i in 0..rounds {
        let client_msg = format!("Client message {}", i);
        let server_msg = format!("Server message {}", i);

        client
            .send_application_data(client_msg.as_bytes())
            .expect("client send");
        server
            .send_application_data(server_msg.as_bytes())
            .expect("server send");

        for _ in 0..10 {
            client.handle_timeout(now).expect("client timeout");
            server.handle_timeout(now).expect("server timeout");

            let client_out = drain_outputs(&mut client);
            let server_out = drain_outputs(&mut server);

            client_received_count += client_out.app_data.len();
            server_received_count += server_out.app_data.len();

            deliver_packets(&client_out.packets, &mut server);
            deliver_packets(&server_out.packets, &mut client);

            now += Duration::from_millis(5);
        }
    }

    assert_eq!(
        client_received_count, rounds,
        "Client should receive all {} server messages",
        rounds
    );
    assert_eq!(
        server_received_count, rounds,
        "Server should receive all {} client messages",
        rounds
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls12_many_small_messages() {
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

    for _ in 0..50 {
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

    // Send 100 small messages from client to server.
    // Drain and deliver after each send to respect the Sans-IO poll-to-timeout
    // contract and the transmit queue capacity.
    let message_count = 100usize;
    let mut received_count = 0usize;

    for i in 0..message_count {
        let msg = format!("M{}", i);
        client.send_application_data(msg.as_bytes()).expect("send");

        let client_pkts = collect_packets(&mut client);
        deliver_packets(&client_pkts, &mut server);

        let server_out = drain_outputs(&mut server);
        for data in &server_out.app_data {
            assert_eq!(data, msg.as_bytes(), "message {} should match", i);
            received_count += 1;
        }
    }

    assert_eq!(
        received_count, message_count,
        "All 100 small messages should be received"
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls12_large_application_data() {
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

    for _ in 0..50 {
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

    // Send data larger than default MTU (1150 bytes)
    let large_data: Vec<u8> = (0..2000).map(|i| (i % 256) as u8).collect();

    client
        .send_application_data(&large_data)
        .expect("client send large data");

    let mut server_received: Vec<u8> = Vec::new();

    for _ in 0..50 {
        client.handle_timeout(now).expect("client timeout");
        server.handle_timeout(now).expect("server timeout");

        let client_out = drain_outputs(&mut client);
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
        "Server should receive the full 2000-byte payload intact"
    );
}
