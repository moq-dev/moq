//! DTLS 1.2 edge case and error recovery tests.

use std::sync::Arc;
use std::time::{Duration, Instant};

#[cfg(feature = "rcgen")]
use dimpl::certificate::generate_self_signed_certificate;
use dimpl::crypto::Dtls12CipherSuite;
use dimpl::{Config, Dtls, Output};

use crate::common::*;

fn dtls12_alert_record(seq: u64, level: u8, description: u8) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(21); // Alert
    out.extend_from_slice(&[0xFE, 0xFD]); // DTLS 1.2
    out.extend_from_slice(&0u16.to_be_bytes()); // epoch 0
    out.extend_from_slice(&seq.to_be_bytes()[2..]); // u48 sequence number
    out.extend_from_slice(&2u16.to_be_bytes()); // alert payload length
    out.extend_from_slice(&[level, description]);
    out
}

fn dtls12_epoch1_record(seq: u64, len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(13 + len);
    out.push(23); // ApplicationData
    out.extend_from_slice(&[0xFE, 0xFD]); // DTLS 1.2
    out.extend_from_slice(&1u16.to_be_bytes()); // epoch 1
    out.extend_from_slice(&seq.to_be_bytes()[2..]); // u48 sequence number
    out.extend_from_slice(&(len as u16).to_be_bytes());
    out.resize(13 + len, 0);
    out
}

#[test]
#[cfg(feature = "rcgen")]
fn oversized_application_data_reports_buffer_too_small() {
    let now = Instant::now();
    let (mut client, mut server, _now) = setup_connected_12_pair(now);
    let payload = vec![0x5a; 4000];

    client
        .send_application_data(&payload)
        .expect("send application data");

    let mut large_buf = vec![0u8; 8192];
    let mut packets = Vec::new();
    loop {
        match client.poll_output(&mut large_buf) {
            Output::Packet(packet) => packets.push(packet.to_vec()),
            Output::Timeout(_) => break,
            _ => {}
        }
    }

    deliver_packets(&packets, &mut server);

    let mut small_buf = vec![0u8; 2048];
    match server.poll_output(&mut small_buf) {
        Output::BufferTooSmall { needed } => assert_eq!(needed, payload.len()),
        output => panic!("expected BufferTooSmall, got {output:?}"),
    }

    match server.poll_output(&mut large_buf) {
        Output::ApplicationData(data) => assert_eq!(data, payload.as_slice()),
        output => panic!("expected retained application data, got {output:?}"),
    }
}

fn dtls12_config_for_suite(suite: Dtls12CipherSuite) -> Arc<Config> {
    let mut provider = Config::default().crypto_provider().clone();
    let selected = provider
        .cipher_suites
        .iter()
        .copied()
        .find(|cs| cs.suite() == suite)
        .unwrap_or_else(|| panic!("suite {:?} not found in provider", suite));

    let suites = Box::leak(Box::new([selected]));
    provider.cipher_suites = suites;

    Arc::new(
        Config::builder()
            .with_crypto_provider(provider)
            .build()
            .expect("build config for single suite"),
    )
}

fn dtls12_min_protected_fragment_len(suite: Dtls12CipherSuite) -> usize {
    match suite {
        Dtls12CipherSuite::ECDHE_ECDSA_AES256_GCM_SHA384
        | Dtls12CipherSuite::ECDHE_ECDSA_AES128_GCM_SHA256 => 24,
        Dtls12CipherSuite::ECDHE_ECDSA_CHACHA20_POLY1305_SHA256 => 16,
        Dtls12CipherSuite::PSK_AES128_CCM_8 => 16,
        _ => panic!("unknown cipher suite"),
    }
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls12_malformed_datagram_is_discarded_without_processing_alerts() {
    let _ = env_logger::try_init();

    let server_cert = generate_self_signed_certificate().expect("gen server cert");
    let config = dtls12_config();
    let now = Instant::now();

    let mut server = Dtls::new_12(config, server_cert, now);
    server.set_active(false);

    let mut packet = dtls12_alert_record(1, 2, 40);
    packet.push(0xFF); // trailing truncated record header

    server
        .handle_packet(&packet)
        .expect("malformed datagram should be discarded");

    let mut buf = [0; 1500];
    assert!(!matches!(server.poll_output(&mut buf), Output::CloseNotify));
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls12_too_many_control_records_are_discarded() {
    let _ = env_logger::try_init();

    let server_cert = generate_self_signed_certificate().expect("gen server cert");
    let config = dtls12_config();
    let now = Instant::now();

    let mut server = Dtls::new_12(config, server_cert, now);
    server.set_active(false);

    let mut packet = Vec::new();
    for seq in 1..=9 {
        packet.extend_from_slice(&dtls12_alert_record(seq, 1, 0));
    }

    server
        .handle_packet(&packet)
        .expect("too many records should be discarded");
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls12_recovers_from_corrupted_packet() {
    //! During handshake, corrupt 2 bytes in one packet before delivery so the
    //! DTLS record header is invalid. The receiver drops the corrupted packet.
    //! After a timeout the sender retransmits, and the handshake completes
    //! normally via the retransmission path.

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    let config = dtls12_config();

    let mut now = Instant::now();

    let mut client = Dtls::new_12(Arc::clone(&config), client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_12(config, server_cert, now);
    server.set_active(false);

    // FLIGHT 1: Client sends ClientHello
    client.handle_timeout(now).expect("client timeout start");
    client.handle_timeout(now).expect("client arm flight 1");
    let f1 = collect_packets(&mut client);
    assert!(!f1.is_empty(), "client should emit ClientHello");

    // Corrupt the record header itself (version field at bytes 1-2) so
    // the record is rejected at parse time and nothing enters queue_rx.
    for mut p in f1 {
        if p.len() > 5 {
            p[1] ^= 0xFF;
            p[2] ^= 0xFF;
        }
        // Server should reject the record due to invalid version
        let _ = server.handle_packet(&p);
    }

    // Server has no valid packet yet — arm its timers so it's ready
    server.handle_timeout(now).expect("server arm");
    let s_pkts = collect_packets(&mut server);
    assert!(s_pkts.is_empty(), "server should have nothing to send yet");

    // Trigger client retransmission timeout (initial RTO is ~1s)
    trigger_timeout(&mut client, &mut now);
    let f1_resend = collect_packets(&mut client);
    assert!(
        !f1_resend.is_empty(),
        "client should retransmit ClientHello after timeout"
    );

    // Deliver the clean retransmission to server
    for p in &f1_resend {
        server.handle_packet(p).expect("server recv clean CH");
    }

    // FLIGHT 2: Server sends HelloVerifyRequest
    server.handle_timeout(now).expect("server arm flight 2");
    let f2 = collect_packets(&mut server);
    assert!(!f2.is_empty(), "server should emit HelloVerifyRequest");
    for p in &f2 {
        client.handle_packet(p).expect("client recv HVR");
    }

    // FLIGHT 3: Client sends ClientHello with cookie
    client.handle_timeout(now).expect("client arm flight 3");
    let f3 = collect_packets(&mut client);
    assert!(!f3.is_empty(), "client should emit ClientHello with cookie");
    for p in &f3 {
        server.handle_packet(p).expect("server recv CH+cookie");
    }

    // FLIGHT 4: Server sends ServerHello, Certificate, etc.
    server.handle_timeout(now).expect("server arm flight 4");
    let f4 = collect_packets(&mut server);
    assert!(!f4.is_empty(), "server should emit ServerHello flight");
    for p in &f4 {
        client.handle_packet(p).expect("client recv flight 4");
    }

    // FLIGHT 5: Client sends CKX, CCS, Finished
    client.handle_timeout(now).expect("client arm flight 5");
    let f5 = collect_packets(&mut client);
    assert!(!f5.is_empty(), "client should emit flight 5");
    for p in &f5 {
        server.handle_packet(p).expect("server recv flight 5");
    }

    // FLIGHT 6: Server sends CCS, Finished (Connected may be emitted here)
    server.handle_timeout(now).expect("server arm flight 6");
    let server_out = drain_outputs(&mut server);
    assert!(
        !server_out.packets.is_empty(),
        "server should emit flight 6"
    );
    for p in &server_out.packets {
        client.handle_packet(p).expect("client recv flight 6");
    }

    // Drain client outputs to check for Connected event
    client.handle_timeout(now).expect("client final timeout");
    let client_out = drain_outputs(&mut client);

    assert!(
        client_out.connected,
        "Client should be connected after recovering from corrupted packet"
    );
    assert!(
        server_out.connected,
        "Server should be connected after recovering from corrupted packet"
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls12_discards_wrong_epoch_record() {
    //! After a completed handshake (epoch 1), inject a crafted packet with
    //! epoch 0 and content_type handshake (22). Verify it is silently dropped
    //! and application data exchange still works.

    let _ = env_logger::try_init();
    let mut now = Instant::now();
    let (mut client, mut server, now_hs) = setup_connected_12_pair(now);
    now = now_hs;

    // Craft a DTLS 1.2 record with epoch 0 (pre-handshake) and content_type 22 (handshake).
    // DTLS 1.2 record header: content_type(1) + version(2) + epoch(2) + seq(6) + length(2)
    let bogus = vec![
        22, // content_type: handshake
        0xFE, 0xFD, // version: DTLS 1.2
        0x00, 0x00, // epoch: 0 (wrong — should be 1 post-handshake)
        0x00, 0x00, 0x00, 0x00, 0x00, 0x99, // sequence number
        0x00, 0x05, // length: 5
        0x01, // handshake type: ClientHello
        0x00, 0x00, 0x00, 0x00, // dummy payload
    ];

    // Should be silently discarded (no error)
    client
        .handle_packet(&bogus)
        .expect("wrong epoch record should be silently discarded");

    // Verify application data exchange still works after the bogus packet.
    client
        .send_application_data(b"ping")
        .expect("client send app data");
    client.handle_timeout(now).expect("client timeout");
    let client_out = drain_outputs(&mut client);
    deliver_packets(&client_out.packets, &mut server);

    server.handle_timeout(now).expect("server timeout");
    let server_out = drain_outputs(&mut server);
    assert!(
        server_out.app_data.iter().any(|d| d.as_slice() == b"ping"),
        "Server should receive application data after wrong-epoch bogus packet"
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls12_short_encrypted_records_do_not_panic() {
    let _ = env_logger::try_init();

    for suite in [
        Dtls12CipherSuite::ECDHE_ECDSA_AES128_GCM_SHA256,
        Dtls12CipherSuite::ECDHE_ECDSA_CHACHA20_POLY1305_SHA256,
    ] {
        let client_cert = generate_self_signed_certificate().expect("gen client cert");
        let server_cert = generate_self_signed_certificate().expect("gen server cert");
        let config = dtls12_config_for_suite(suite);

        let mut now = Instant::now();

        let mut client = Dtls::new_12(Arc::clone(&config), client_cert, now);
        client.set_active(true);

        let mut server = Dtls::new_12(config, server_cert, now);
        server.set_active(false);

        now = complete_dtls12_handshake(&mut client, &mut server, now);

        for len in 0..dtls12_min_protected_fragment_len(suite) {
            let short = dtls12_epoch1_record(0x100 + len as u64, len);
            client
                .handle_packet(&short)
                .expect("short encrypted record should be silently discarded");
        }

        server
            .send_application_data(b"still alive")
            .expect("server send app data");
        server.handle_timeout(now).expect("server timeout");
        let server_out = drain_outputs(&mut server);
        deliver_packets(&server_out.packets, &mut client);

        client.handle_timeout(now).expect("client timeout");
        let client_out = drain_outputs(&mut client);
        assert!(
            client_out
                .app_data
                .iter()
                .any(|d| d.as_slice() == b"still alive"),
            "client should receive application data after short encrypted {suite:?} records"
        );
    }
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls12_discards_truncated_record() {
    //! Deliver a 3-byte packet (too short to be a valid DTLS 1.2 record header,
    //! which requires 13 bytes). Verify it is silently dropped and the
    //! handshake/connection continues.

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    let config = dtls12_config();

    let mut now = Instant::now();

    let mut client = Dtls::new_12(Arc::clone(&config), client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_12(config, server_cert, now);
    server.set_active(false);

    // Inject a truncated packet before the handshake begins
    let truncated = vec![0x16, 0xFE, 0xFD]; // 3 bytes — too short for any DTLS record
    let result = client.handle_packet(&truncated);
    // Should either return Ok (silently discarded) or a non-fatal error
    match result {
        Ok(()) => {} // silently discarded — expected
        Err(e) => {
            // Some parse errors are acceptable as long as the endpoint survives
            eprintln!("Truncated packet returned error (non-fatal): {}", e);
        }
    }

    // Now complete the handshake to prove the endpoint is still functional
    now = complete_dtls12_handshake(&mut client, &mut server, now);

    // Also inject a truncated packet after the handshake and verify app data works
    let truncated_post = vec![0x17, 0xFE, 0xFD]; // 3 bytes, content_type = app data
    let result = client.handle_packet(&truncated_post);
    match result {
        Ok(()) => {}
        Err(e) => {
            eprintln!(
                "Post-handshake truncated packet returned error (non-fatal): {}",
                e
            );
        }
    }

    // Verify application data exchange still works
    client
        .send_application_data(b"hello")
        .expect("client send app data");
    client.handle_timeout(now).expect("client timeout");
    let client_out = drain_outputs(&mut client);
    deliver_packets(&client_out.packets, &mut server);

    server.handle_timeout(now).expect("server timeout");
    let server_out = drain_outputs(&mut server);
    assert!(
        server_out.app_data.iter().any(|d| d.as_slice() == b"hello"),
        "Server should receive application data after truncated bogus packets"
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls12_discards_unauthenticated_close_notify() {
    //! After a completed handshake (epoch 1), inject a plaintext close_notify
    //! alert at epoch 0. Since the connection is authenticated, the
    //! unauthenticated alert must be silently discarded and the connection
    //! must remain operational.

    let _ = env_logger::try_init();
    let mut now = Instant::now();
    let (mut client, mut server, now_hs) = setup_connected_12_pair(now);
    now = now_hs;

    // Craft a close_notify alert record at epoch 0 (plaintext alert).
    // Since DTLS 1.2 post-handshake records should be at epoch 1 and encrypted,
    // an epoch 0 plaintext alert should be silently discarded.
    let close_notify_epoch0 = vec![
        21, // content_type: alert
        0xFE, 0xFD, // version: DTLS 1.2
        0x00, 0x00, // epoch: 0 (plaintext — will be discarded post-handshake)
        0x00, 0x00, 0x00, 0x00, 0x00, 0x50, // sequence number
        0x00, 0x02, // length: 2
        0x01, // level: warning
        0x00, // description: close_notify
    ];

    // Epoch 0 alert post-handshake must be silently discarded (not an error).
    server
        .handle_packet(&close_notify_epoch0)
        .expect("epoch 0 alert must be silently discarded post-handshake");

    // Verify the server can still process data after the alert
    client
        .send_application_data(b"after-alert")
        .expect("client send after alert");
    now += Duration::from_millis(10);
    client.handle_timeout(now).expect("client timeout");
    let client_out = drain_outputs(&mut client);
    deliver_packets(&client_out.packets, &mut server);

    server.handle_timeout(now).expect("server timeout");
    let server_out = drain_outputs(&mut server);
    assert!(
        server_out
            .app_data
            .iter()
            .any(|d| d.as_slice() == b"after-alert"),
        "Server should still receive app data after close_notify alert at epoch 0"
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls12_rejects_renegotiation() {
    //! After a completed handshake, inject a ClientHello record to simulate
    //! a renegotiation attempt. Verify it is rejected (either silently dropped
    //! or returns `Error::RenegotiationAttempt`).

    let _ = env_logger::try_init();
    let now = Instant::now();
    let (_client, mut server, _now) = setup_connected_12_pair(now);

    // Craft a ClientHello record at epoch 0 to simulate a renegotiation attempt.
    // This is a plaintext handshake record with a minimal ClientHello.
    let renegotiation_hello = vec![
        22, // content_type: handshake
        0xFE, 0xFD, // version: DTLS 1.2
        0x00, 0x00, // epoch: 0
        0x00, 0x00, 0x00, 0x00, 0x01, 0x00, // sequence number
        0x00, 0x2F, // length: 47 bytes of handshake payload
        // Handshake header
        0x01, // msg_type: ClientHello
        0x00, 0x00, 0x23, // length: 35
        0x00, 0x01, // message_seq: 1
        0x00, 0x00, 0x00, // fragment_offset: 0
        0x00, 0x00, 0x23, // fragment_length: 35
        // ClientHello body
        0xFE, 0xFD, // client_version: DTLS 1.2
        // random (32 bytes)
        0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F,
        0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1A, 0x1B, 0x1C, 0x1D, 0x1E,
        0x1F, 0x20, 0x00, // session_id length: 0
        0x00, // cookie length: 0
        0x00, // cipher_suites length will make this invalid, but that's fine
    ];

    // The server should reject the renegotiation attempt.
    // It may return an error or silently discard it.
    let result = server.handle_packet(&renegotiation_hello);
    match result {
        Ok(()) => {
            // Silently discarded — acceptable (epoch 0 record post-handshake)
        }
        Err(e) => {
            // RenegotiationAttempt or other error — also acceptable
            eprintln!("Renegotiation attempt correctly rejected with error: {}", e);
        }
    }

    // Verify the connection still works after the renegotiation attempt — we need
    // a client to send data, so re-create using the existing pair's server.
    // Since _client was moved, just verify server can still queue data.
    let result = server.send_application_data(b"post-reneg");
    assert!(
        result.is_ok(),
        "Server should still accept sends after renegotiation attempt was rejected"
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls12_mixed_datagram_plaintext_first_then_valid() {
    //! Test that a UDP datagram with bogus plaintext ApplicationData FIRST
    //! followed by a valid encrypted record is handled correctly: the bogus
    //! record is silently discarded and the valid one is still processed.

    let _ = env_logger::try_init();
    let now = Instant::now();
    let (mut client, mut server, now) = setup_connected_12_pair(now);

    // Send valid application data from client and capture the encrypted packet.
    client
        .send_application_data(b"valid-data")
        .expect("send valid data");
    client.handle_timeout(now).expect("client timeout");
    let client_out = drain_outputs(&mut client);
    assert!(!client_out.packets.is_empty(), "Should have valid packet");
    let valid_packet = &client_out.packets[0];

    // Craft a plaintext ApplicationData record (epoch 0).
    let bogus_record = vec![
        0x17, // content_type: ApplicationData
        0xFE, 0xFD, // version: DTLS 1.2
        0x00, 0x00, // epoch: 0 (plaintext)
        0x00, 0x00, 0x00, 0x00, 0x00, 0x88, // sequence_number
        0x00, 0x06, // length: 6
        0x62, 0x6F, 0x67, 0x75, 0x73, 0x21, // "bogus!"
    ];

    // Construct a mixed datagram: bogus plaintext record FIRST, then valid record.
    let mut mixed_datagram = bogus_record;
    mixed_datagram.extend_from_slice(valid_packet);

    // Deliver the mixed datagram to server.
    server
        .handle_packet(&mixed_datagram)
        .expect("mixed datagram should not error");

    server.handle_timeout(now).expect("server timeout");
    let server_out = drain_outputs(&mut server);

    // The valid record should still be processed despite the bogus first record.
    assert!(
        server_out
            .app_data
            .iter()
            .any(|d| d.as_slice() == b"valid-data"),
        "Server should receive the valid encrypted ApplicationData even when bogus record comes first"
    );

    // The bogus plaintext record should NOT produce any output.
    assert_eq!(
        server_out.app_data.len(),
        1,
        "Should receive exactly 1 app data (the valid one), not the bogus plaintext"
    );
    assert!(
        !server_out
            .app_data
            .iter()
            .any(|d| d.as_slice() == b"bogus!"),
        "Bogus plaintext ApplicationData must not be delivered"
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls12_mixed_datagram_valid_first_then_bogus() {
    //! Test that a UDP datagram with a valid encrypted record FIRST followed
    //! by bogus plaintext ApplicationData is handled correctly: the valid
    //! record is processed and the trailing bogus record is discarded.

    let _ = env_logger::try_init();
    let now = Instant::now();
    let (mut client, mut server, now) = setup_connected_12_pair(now);

    // Send valid application data from client and capture the encrypted packet.
    client
        .send_application_data(b"valid-data")
        .expect("send valid data");
    client.handle_timeout(now).expect("client timeout");
    let client_out = drain_outputs(&mut client);
    assert!(!client_out.packets.is_empty(), "Should have valid packet");
    let valid_packet = &client_out.packets[0];

    // Craft a plaintext ApplicationData record (epoch 0).
    let bogus_record = vec![
        0x17, // content_type: ApplicationData
        0xFE, 0xFD, // version: DTLS 1.2
        0x00, 0x00, // epoch: 0 (plaintext)
        0x00, 0x00, 0x00, 0x00, 0x00, 0x99, // sequence_number
        0x00, 0x06, // length: 6
        0x62, 0x6F, 0x67, 0x75, 0x73, 0x21, // "bogus!"
    ];

    // Construct a mixed datagram: valid record FIRST, then bogus plaintext.
    let mut mixed_datagram = valid_packet.clone();
    mixed_datagram.extend_from_slice(&bogus_record);

    // Deliver the mixed datagram to server.
    server
        .handle_packet(&mixed_datagram)
        .expect("mixed datagram should not error");

    server.handle_timeout(now).expect("server timeout");
    let server_out = drain_outputs(&mut server);

    // The valid record should be processed.
    assert!(
        server_out
            .app_data
            .iter()
            .any(|d| d.as_slice() == b"valid-data"),
        "Server should receive the valid encrypted ApplicationData even when bogus record follows"
    );

    // The bogus trailing record should NOT produce any output.
    assert_eq!(
        server_out.app_data.len(),
        1,
        "Should receive exactly 1 app data (the valid one), not the bogus plaintext"
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls12_bad_encrypted_prefix_does_not_drop_valid_tail() {
    let _ = env_logger::try_init();
    let now = Instant::now();
    let (mut client, mut server, now) = setup_connected_12_pair(now);

    client
        .send_application_data(b"bad-prefix-clean-resend")
        .expect("send first application data");
    client.handle_timeout(now).expect("client timeout");
    let first_out = drain_outputs(&mut client);
    let first_packet = first_out
        .packets
        .first()
        .expect("first application data packet")
        .clone();

    client
        .send_application_data(b"valid-tail")
        .expect("send second application data");
    client.handle_timeout(now).expect("client timeout");
    let second_out = drain_outputs(&mut client);
    let second_packet = second_out
        .packets
        .first()
        .expect("second application data packet")
        .clone();

    let mut corrupted_first = first_packet.clone();
    *corrupted_first
        .last_mut()
        .expect("encrypted packet has ciphertext") ^= 0x55;

    let mut mixed_datagram = corrupted_first;
    mixed_datagram.extend_from_slice(&second_packet);

    server
        .handle_packet(&mixed_datagram)
        .expect("bad encrypted prefix should be discarded without dropping valid tail");
    let server_out = drain_outputs(&mut server);
    assert_eq!(
        server_out.app_data,
        vec![b"valid-tail".to_vec()],
        "valid tail record should be delivered despite bad encrypted prefix"
    );

    server
        .handle_packet(&first_packet)
        .expect("clean first packet should remain replay-acceptable");
    let server_out = drain_outputs(&mut server);
    assert_eq!(
        server_out.app_data,
        vec![b"bad-prefix-clean-resend".to_vec()]
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls12_relabelled_encrypted_handshake_failure_is_not_silently_discarded() {
    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");
    let config = dtls12_config();
    let mut now = Instant::now();

    let mut client = Dtls::new_12(Arc::clone(&config), client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_12(config, server_cert, now);
    server.set_active(false);

    client.handle_timeout(now).expect("client timeout start");
    client.handle_timeout(now).expect("client arm flight 1");
    let f1 = collect_packets(&mut client);
    for p in &f1 {
        server.handle_packet(p).expect("server recv ClientHello");
    }

    server.handle_timeout(now).expect("server arm flight 2");
    let f2 = collect_packets(&mut server);
    for p in &f2 {
        client
            .handle_packet(p)
            .expect("client recv HelloVerifyRequest");
    }

    client.handle_timeout(now).expect("client arm flight 3");
    let f3 = collect_packets(&mut client);
    for p in &f3 {
        server
            .handle_packet(p)
            .expect("server recv ClientHello with cookie");
    }

    server.handle_timeout(now).expect("server arm flight 4");
    let f4 = collect_packets(&mut server);
    for p in &f4 {
        client.handle_packet(p).expect("client recv server flight");
    }

    now += Duration::from_millis(10);
    client.handle_timeout(now).expect("client arm flight 5");
    let mut f5 = collect_packets(&mut client);
    assert!(!f5.is_empty(), "client should emit flight 5");

    let mut relabelled = false;
    let mut observed_error = None;
    for p in &mut f5 {
        let mut offset = 0;
        while offset + 13 <= p.len() {
            let epoch = u16::from_be_bytes([p[offset + 3], p[offset + 4]]);
            let len = u16::from_be_bytes([p[offset + 11], p[offset + 12]]) as usize;
            if p[offset] == 22 && epoch >= 1 {
                p[offset] = 23;
                relabelled = true;
                break;
            }
            offset += 13 + len;
        }

        match server.handle_packet(p) {
            Ok(()) => {}
            Err(e) => {
                observed_error = Some(e);
                break;
            }
        }
    }

    assert!(
        relabelled,
        "flight 5 should contain an encrypted Handshake record"
    );
    assert!(
        matches!(
            observed_error,
            Some(dimpl::Error::CryptoError(_) | dimpl::Error::SecurityError(_))
        ),
        "relabeled encrypted handshake must remain fatal, got {observed_error:?}"
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls12_app_data_after_close_notify_is_ignored() {
    //! Simulate UDP reordering: the client sends app data, then close_notify,
    //! but the close_notify datagram arrives at the server first. The app data
    //! datagram arriving afterwards must be silently discarded.

    let _ = env_logger::try_init();
    let mut now = Instant::now();
    let (mut client, mut server, now_hs) = setup_connected_12_pair(now);
    now = now_hs;

    // Step 1: Client sends app data — capture the packet but don't deliver yet.
    client
        .send_application_data(b"before-close")
        .expect("send app data");
    now += Duration::from_millis(10);
    client.handle_timeout(now).expect("client timeout");
    let app_data_out = drain_outputs(&mut client);
    let app_data_packets = app_data_out.packets.clone();
    assert!(!app_data_packets.is_empty(), "Should have app data packet");

    // Step 2: Client sends close_notify.
    client.close().unwrap();
    now += Duration::from_millis(10);
    client.handle_timeout(now).expect("client timeout");
    let close_out = drain_outputs(&mut client);
    assert!(
        !close_out.packets.is_empty(),
        "Should have close_notify packet"
    );

    // Step 3: Deliver close_notify FIRST (simulating UDP reordering).
    deliver_packets(&close_out.packets, &mut server);
    server.handle_timeout(now).expect("server timeout");
    let server_out = drain_outputs(&mut server);

    assert!(server_out.close_notify, "Server should emit CloseNotify");

    // Step 4: Now deliver the app data datagram that was sent BEFORE the alert
    // but arrived AFTER — it must be discarded.
    deliver_packets(&app_data_packets, &mut server);
    now += Duration::from_millis(10);
    server.handle_timeout(now).expect("server timeout");
    let server_out = drain_outputs(&mut server);

    assert!(
        server_out.app_data.is_empty(),
        "ApplicationData arriving after close_notify must be discarded"
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls12_close_during_handshake_emits_no_packets() {
    //! Call close() on the client while the handshake is in progress.
    //! Per `Dtls::close` API contract, close() during handshake silently
    //! discards state without sending any packets.

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    let config = dtls12_config();

    let now = Instant::now();

    let mut client = Dtls::new_12(Arc::clone(&config), client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_12(config, server_cert, now);
    server.set_active(false);

    // Start handshake — client sends ClientHello
    client.handle_timeout(now).expect("client timeout");
    let client_out = drain_outputs(&mut client);
    assert!(
        !client_out.packets.is_empty(),
        "Client should emit ClientHello"
    );

    // Deliver to server, server responds
    deliver_packets(&client_out.packets, &mut server);
    server.handle_timeout(now).expect("server timeout");
    let _server_out = drain_outputs(&mut server);

    // Now abort the client mid-handshake
    client.close().unwrap();

    // After close(), polling must not emit any more packets (library policy, not RFC mandate).
    let client_out = drain_outputs(&mut client);
    assert!(
        client_out.packets.is_empty(),
        "Client should not emit packets after close() during handshake"
    );

    // Even after a timeout, no packets should appear.
    let later = now + Duration::from_secs(5);
    // handle_timeout may error since state is Closed, which is fine
    let _ = client.handle_timeout(later);
    let client_out = drain_outputs(&mut client);
    assert!(
        client_out.packets.is_empty(),
        "Client should not emit packets after timeout post-close()"
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls12_reciprocal_close_notify_and_no_further_sends() {
    //! When the server receives a close_notify from the client, it must send
    //! a reciprocal close_notify back (RFC 5246 §7.2.1) and transition to
    //! Closed. DTLS 1.2 does not support half-close: subsequent
    //! send_application_data calls on both sides must fail.

    let _ = env_logger::try_init();
    let mut now = Instant::now();
    let (mut client, mut server, now_hs) = setup_connected_12_pair(now);
    now = now_hs;

    assert!(!client.is_closing());
    assert!(!client.is_closed());
    assert!(!server.is_closing());
    assert!(!server.is_closed());

    // Client sends close_notify
    client.close().unwrap();
    assert!(client.is_closing());
    assert!(!client.is_closed());

    now += Duration::from_millis(10);
    client.handle_timeout(now).expect("client timeout");
    let client_out = drain_outputs(&mut client);
    assert!(
        !client_out.packets.is_empty(),
        "Client should emit close_notify alert"
    );
    assert!(!client.is_closing());
    assert!(client.is_closed());

    // Deliver to server
    deliver_packets(&client_out.packets, &mut server);
    server.handle_timeout(now).expect("server timeout");
    assert!(server.is_closing());
    assert!(!server.is_closed());

    let server_out = drain_outputs(&mut server);

    // Server should emit CloseNotify event
    assert!(
        server_out.close_notify,
        "Server should emit Output::CloseNotify"
    );

    // Server should emit a reciprocal close_notify packet.
    assert!(
        !server_out.packets.is_empty(),
        "Server should emit a reciprocal close_notify packet"
    );
    assert!(!server.is_closing());
    assert!(server.is_closed());

    // Deliver reciprocal back to client and verify it sees CloseNotify.
    deliver_packets(&server_out.packets, &mut client);
    client
        .handle_timeout(now)
        .expect("client timeout after reciprocal");
    assert!(client.is_closing());
    assert!(!client.is_closed());

    let client_out2 = drain_outputs(&mut client);
    assert!(
        client_out2.close_notify,
        "Client should emit Output::CloseNotify after receiving reciprocal close_notify"
    );
    assert!(!client.is_closing());
    assert!(client.is_closed());

    // No half-close in DTLS 1.2: both sides must reject further sends.
    assert!(
        server.send_application_data(b"after-close").is_err(),
        "send_application_data must fail after close_notify in DTLS 1.2"
    );
    assert!(
        client.send_application_data(b"after-close").is_err(),
        "send_application_data must fail after close() in DTLS 1.2"
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls12_close_state_matrix() {
    let mut now = Instant::now();
    let (mut client, mut server, now_hs) = setup_connected_12_pair(now);
    now = now_hs;

    assert!(!client.is_closing());
    assert!(!client.is_closed());
    assert!(!server.is_closing());
    assert!(!server.is_closed());

    client.close().unwrap();
    assert!(client.is_closing(), "local close pending");
    assert!(!client.is_closed(), "local close not drained");

    now += Duration::from_millis(10);
    client.handle_timeout(now).expect("client timeout");
    let client_out = drain_outputs(&mut client);
    assert!(!client_out.packets.is_empty(), "local close_notify packet");
    assert!(!client.is_closing(), "local close drained");
    assert!(client.is_closed(), "local close terminal");

    deliver_packets(&client_out.packets, &mut server);
    server.handle_timeout(now).expect("server timeout");
    assert!(server.is_closing(), "remote close pending");
    assert!(!server.is_closed(), "remote close not drained");

    let server_out = drain_outputs(&mut server);
    assert!(server_out.close_notify, "remote CloseNotify event");
    assert!(
        !server_out.packets.is_empty(),
        "remote reciprocal close_notify packet"
    );
    assert!(!server.is_closing(), "remote close drained");
    assert!(server.is_closed(), "remote close terminal");
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls12_discard_pending_writes_on_close_notify() {
    //! Send application data from the server, then deliver a close_notify from
    //! the client before the server polls. The pending data must be discarded
    //! per RFC 5246 §7.2.1 — only the reciprocal close_notify is emitted.

    let _ = env_logger::try_init();
    let mut now = Instant::now();
    let (mut client, mut server, now_hs) = setup_connected_12_pair(now);
    now = now_hs;

    // Server queues some application data (not yet polled)
    server
        .send_application_data(b"pending-data")
        .expect("server send pending data");

    // Client sends close_notify
    client.close().unwrap();
    now += Duration::from_millis(10);
    client.handle_timeout(now).expect("client timeout");
    let client_out = drain_outputs(&mut client);

    // Deliver the close_notify to the server (before it polls its pending data)
    deliver_packets(&client_out.packets, &mut server);

    // Now poll the server — pending data should have been discarded
    server.handle_timeout(now).expect("server timeout");
    let server_out = drain_outputs(&mut server);

    assert!(server_out.close_notify, "Server should see CloseNotify");
    assert!(
        !server_out.packets.is_empty(),
        "Server should emit reciprocal close_notify"
    );

    // Deliver reciprocal to client — verify no app data leaked
    deliver_packets(&server_out.packets, &mut client);
    client
        .handle_timeout(now)
        .expect("client timeout after reciprocal");
    let client_out2 = drain_outputs(&mut client);

    assert!(
        client_out2.close_notify,
        "Client should emit Output::CloseNotify after receiving reciprocal close_notify"
    );
    assert!(
        client_out2.app_data.is_empty(),
        "Pending data must be discarded when close_notify is received"
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls12_fatal_alert_during_handshake() {
    //! During the handshake (peer_encryption_enabled == false), an epoch 0
    //! fatal alert (level=2) should be accepted and return a SecurityError.

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    let config = dtls12_config();

    let now = Instant::now();

    let mut client = Dtls::new_12(Arc::clone(&config), client_cert, now);
    client.set_active(true);

    let mut _server = Dtls::new_12(config, server_cert, now);

    // Start the handshake so the client is expecting a response
    client.handle_timeout(now).expect("client timeout");
    let _client_out = drain_outputs(&mut client);

    // Craft a fatal alert at epoch 0 (during handshake, this is legitimate)
    let fatal_alert = vec![
        21, // content_type: alert
        0xFE, 0xFD, // version: DTLS 1.2
        0x00, 0x00, // epoch: 0
        0x00, 0x00, 0x00, 0x00, 0x00, 0x01, // sequence number
        0x00, 0x02, // length: 2
        0x02, // level: fatal
        0x28, // description: handshake_failure (40)
    ];

    let result = client.handle_packet(&fatal_alert);
    assert!(
        result.is_err(),
        "Fatal alert during handshake should return an error"
    );
    let err = result.unwrap_err();
    assert!(matches!(
        err,
        dimpl::Error::SecurityError(dimpl::SecurityError::FatalAlert { description: 40 })
    ));
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls12_app_data_delivered_before_close_notify() {
    //! When app data and close_notify arrive together, the app data must be
    //! delivered before CloseNotify.

    let _ = env_logger::try_init();
    let mut now = Instant::now();
    let (mut client, mut server, now_hs) = setup_connected_12_pair(now);
    now = now_hs;

    // Send app data then immediately close
    client
        .send_application_data(b"before-close")
        .expect("send app data");
    client.close().unwrap();

    now += Duration::from_millis(10);
    client.handle_timeout(now).expect("client timeout");
    let client_out = drain_outputs(&mut client);

    deliver_packets(&client_out.packets, &mut server);

    // Poll server outputs and verify ordering: ApplicationData before CloseNotify
    server.handle_timeout(now).expect("server timeout");
    let mut saw_app_data = false;
    let mut saw_close_notify = false;
    let mut close_after_data = false;
    let mut buf = vec![0u8; 2048];
    loop {
        match server.poll_output(&mut buf) {
            Output::ApplicationData(data) => {
                assert!(
                    !saw_close_notify,
                    "ApplicationData must not appear after CloseNotify"
                );
                if data == b"before-close" {
                    saw_app_data = true;
                }
            }
            Output::CloseNotify => {
                saw_close_notify = true;
                if saw_app_data {
                    close_after_data = true;
                }
            }
            Output::Timeout(_) => break,
            _ => {}
        }
    }
    assert!(saw_app_data, "Server should receive the app data");
    assert!(saw_close_notify, "Server should see CloseNotify");
    assert!(
        close_after_data,
        "CloseNotify must come after ApplicationData"
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls12_close_notify_not_retransmitted() {
    //! After sending close_notify, the alert must not be retransmitted.
    //! RFC 6347 §4.2.7: "Alert messages are not retransmitted at all,
    //! even when they occur in the context of a handshake."

    let _ = env_logger::try_init();
    let mut now = Instant::now();
    let (mut client, _server, now_hs) = setup_connected_12_pair(now);
    now = now_hs;

    // Client sends close_notify
    client.close().unwrap();
    now += Duration::from_millis(10);
    client.handle_timeout(now).expect("client timeout");
    let client_out = drain_outputs(&mut client);
    assert!(
        !client_out.packets.is_empty(),
        "Client should emit close_notify alert"
    );

    // Advance time 5 times (5 seconds each) — no retransmissions should occur
    for _ in 0..5 {
        now += Duration::from_secs(5);
        let _ = client.handle_timeout(now);
        let out = drain_outputs(&mut client);
        assert!(
            out.packets.is_empty(),
            "close_notify must not be retransmitted (RFC 6347 §4.2.7)"
        );
    }
}
