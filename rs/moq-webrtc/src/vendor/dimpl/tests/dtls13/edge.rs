//! DTLS 1.3 edge case and error recovery tests.

use std::sync::Arc;
use std::time::{Duration, Instant};

#[cfg(feature = "rcgen")]
use dimpl::certificate::generate_self_signed_certificate;
use dimpl::{Config, Dtls, Output};

use crate::common::*;

fn dtls13_alert_record(seq: u64, level: u8, description: u8) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(21); // Alert
    out.extend_from_slice(&[0xFE, 0xFD]); // legacy DTLS record version
    out.extend_from_slice(&0u16.to_be_bytes()); // epoch 0 plaintext
    out.extend_from_slice(&seq.to_be_bytes()[2..]); // u48 sequence number
    out.extend_from_slice(&2u16.to_be_bytes()); // alert payload length
    out.extend_from_slice(&[level, description]); // legacy level, description
    out
}

fn dtls13_ack_record(seq: u64) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(26); // Ack
    out.extend_from_slice(&[0xFE, 0xFD]); // legacy DTLS record version
    out.extend_from_slice(&0u16.to_be_bytes()); // epoch 0 plaintext
    out.extend_from_slice(&seq.to_be_bytes()[2..]); // u48 sequence number
    out.extend_from_slice(&2u16.to_be_bytes()); // arbitrary payload length
    out.extend_from_slice(&[0xAA, 0xBB]);
    out
}

fn dtls13_ack_record_for_records(seq: u64, records: &[(u64, u64)]) -> Vec<u8> {
    let record_numbers_len = (records.len() * 16) as u16;
    let mut fragment = Vec::with_capacity(2 + records.len() * 16);
    fragment.extend_from_slice(&record_numbers_len.to_be_bytes());
    for &(epoch, record_seq) in records {
        fragment.extend_from_slice(&epoch.to_be_bytes());
        fragment.extend_from_slice(&record_seq.to_be_bytes());
    }

    let mut out = Vec::new();
    out.push(26); // Ack
    out.extend_from_slice(&[0xFE, 0xFD]); // legacy DTLS record version
    out.extend_from_slice(&0u16.to_be_bytes()); // epoch 0 plaintext
    out.extend_from_slice(&seq.to_be_bytes()[2..]); // u48 sequence number
    out.extend_from_slice(&(fragment.len() as u16).to_be_bytes());
    out.extend_from_slice(&fragment);
    out
}

#[test]
#[cfg(feature = "rcgen")]
fn oversized_application_data_reports_buffer_too_small() {
    let now = Instant::now();
    let (mut client, mut server, _now) = setup_connected_13_pair(now);
    let payload = vec![0xa5; 4000];

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

#[test]
#[cfg(feature = "rcgen")]
fn dtls13_malformed_datagram_is_discarded_without_processing_alerts() {
    let _ = env_logger::try_init();

    let server_cert = generate_self_signed_certificate().expect("gen server cert");
    let config = dtls13_config();
    let now = Instant::now();

    let mut server = Dtls::new_13(config, server_cert, now);
    server.set_active(false);

    let mut packet = dtls13_alert_record(1, 2, 40);
    packet.push(0xFF); // trailing truncated record header

    server
        .handle_packet(&packet)
        .expect("malformed datagram should be discarded");

    let mut buf = [0; 1500];
    assert!(!matches!(server.poll_output(&mut buf), Output::CloseNotify));
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls13_too_many_control_records_are_discarded() {
    let _ = env_logger::try_init();

    let server_cert = generate_self_signed_certificate().expect("gen server cert");
    let config = dtls13_config();
    let now = Instant::now();

    let mut server = Dtls::new_13(config, server_cert, now);
    server.set_active(false);

    let mut packet = Vec::new();
    for seq in 1..=17 {
        packet.extend_from_slice(&dtls13_ack_record(seq));
    }

    server
        .handle_packet(&packet)
        .expect("too many records should be discarded");
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls13_discards_too_short_ciphertext_record() {
    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    let config = dtls13_config();

    let mut now = Instant::now();

    let mut client = Dtls::new_13(Arc::clone(&config), client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_13(config, server_cert, now);
    server.set_active(false);

    now = complete_dtls13_handshake(&mut client, &mut server, now);

    // Every length in [0, AEAD overhead) must be rejected at the record boundary.
    // For the default DTLS 1.3 suites the AEAD overhead is the tag length (16).
    // Header: fixed bits 001, C=0, S=1 (16-bit seq), L=1 (length), epoch_bits=3
    // => 0b0010_1111 = 0x2F
    for len in 0..16u16 {
        let mut bogus = Vec::with_capacity(5 + len as usize);
        bogus.push(0x2F);
        bogus.extend_from_slice(&(0x0100 + len).to_be_bytes()); // encrypted seq bits
        bogus.extend_from_slice(&len.to_be_bytes());
        bogus.resize(5 + len as usize, 0);

        client
            .handle_packet(&bogus)
            .expect("short ciphertext record should be silently discarded");
    }

    // Verify we can still exchange application data.
    client.send_application_data(b"ping").expect("send app");
    client.handle_timeout(now).expect("client timeout");
    let client_out = drain_outputs(&mut client);
    deliver_packets(&client_out.packets, &mut server);

    server.handle_timeout(now).expect("server timeout");
    let server_out = drain_outputs(&mut server);
    assert!(
        server_out.app_data.iter().any(|d| d.as_slice() == b"ping"),
        "Server should receive application data after bogus packet"
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls13_discards_cid_bit_records() {
    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    let config = dtls13_config();

    let mut now = Instant::now();

    let mut client = Dtls::new_13(Arc::clone(&config), client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_13(config, server_cert, now);
    server.set_active(false);

    now = complete_dtls13_handshake(&mut client, &mut server, now);

    // Unified header with CID bit set: 001CSLEE with C=1, S=1, L=1, epoch_bits=3 => 0x3F.
    // We don't support CID, so this should be silently discarded.
    let bogus = vec![0x3F];

    client
        .handle_packet(&bogus)
        .expect("CID-bit record should be discarded");

    // Verify we can still exchange application data.
    client.send_application_data(b"ping").expect("send app");
    client.handle_timeout(now).expect("client timeout");
    let client_out = drain_outputs(&mut client);
    deliver_packets(&client_out.packets, &mut server);

    server.handle_timeout(now).expect("server timeout");
    let server_out = drain_outputs(&mut server);
    assert!(
        server_out.app_data.iter().any(|d| d.as_slice() == b"ping"),
        "Server should receive application data after CID-bit bogus packet"
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls13_discards_unauthenticated_ciphertext_without_length_field() {
    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    let config = dtls13_config();

    let mut now = Instant::now();

    let mut client = Dtls::new_13(Arc::clone(&config), client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_13(config, server_cert, now);
    server.set_active(false);

    now = complete_dtls13_handshake(&mut client, &mut server, now);

    // Craft a DTLS 1.3 ciphertext record with L=0 (no explicit length).
    // Header: 001CSLEE with C=0, S=1, L=0, epoch_bits=3 => 0x2B.
    // Provide 16+ bytes ciphertext so sequence-number mask can be computed.
    let mut bogus = Vec::new();
    bogus.push(0x2B);
    bogus.extend_from_slice(&0x0001u16.to_be_bytes()); // encrypted seq bits
    bogus.extend_from_slice(&[0u8; 16]); // unauthenticated ciphertext/tag bytes

    client
        .handle_packet(&bogus)
        .expect("Unauthenticated ciphertext should be discarded");

    // Verify we can still exchange application data.
    client.send_application_data(b"ping").expect("send app");
    client.handle_timeout(now).expect("client timeout");
    let client_out = drain_outputs(&mut client);
    deliver_packets(&client_out.packets, &mut server);

    server.handle_timeout(now).expect("server timeout");
    let server_out = drain_outputs(&mut server);
    assert!(
        server_out.app_data.iter().any(|d| d.as_slice() == b"ping"),
        "Server should receive application data after unauthenticated bogus packet"
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls13_recovers_from_corrupted_packet() {
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
    let mut corrupted_once = false;

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

        // Corrupt one packet
        for mut p in client_out.packets {
            if !corrupted_once && p.len() > 20 {
                // Corrupt some bytes in the middle (handshake length field)
                p[15] ^= 0xFF;
                p[16] ^= 0xFF;
                corrupted_once = true;
            }
            let _ = server.handle_packet(&p);
        }

        deliver_packets(&server_out.packets, &mut client);

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

    assert!(
        client_connected,
        "Client should connect despite corrupted packet"
    );
    assert!(
        server_connected,
        "Server should connect despite corrupted packet"
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls13_close_notify_graceful_shutdown() {
    let _ = env_logger::try_init();
    let mut now = Instant::now();
    let (mut client, mut server, now_hs) = setup_connected_13_pair(now);
    now = now_hs;

    // Client initiates graceful shutdown.
    client.close().expect("client close");
    now += Duration::from_millis(10);
    client.handle_timeout(now).expect("client timeout");
    let client_out = drain_outputs(&mut client);
    assert!(
        !client_out.packets.is_empty(),
        "Client should emit close_notify packet"
    );

    // Deliver the close_notify alert to the server.
    deliver_packets(&client_out.packets, &mut server);
    server.handle_timeout(now).expect("server timeout");
    let server_out = drain_outputs(&mut server);
    assert!(
        server_out.close_notify,
        "Server should observe CloseNotify from client"
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls13_warning_user_canceled_alert_is_ignored() {
    let _ = env_logger::try_init();

    let mut now = Instant::now();
    let (mut client, mut server, now_hs) = setup_connected_13_pair(now);
    now = now_hs;

    let warning_alert = dtls13_alert_record(100, 1, 90);
    server
        .handle_packet(&warning_alert)
        .expect("warning alert should be ignored");

    let server_out = drain_outputs(&mut server);
    assert!(
        !server_out.close_notify,
        "warning alert must not be reported as close_notify"
    );

    client
        .send_application_data(b"still-open")
        .expect("connection should remain open after warning alert");
    now += Duration::from_millis(10);
    client.handle_timeout(now).expect("client timeout");
    let client_out = drain_outputs(&mut client);
    assert!(
        !client_out.packets.is_empty(),
        "client should still emit application data after warning alert"
    );

    for packet in &client_out.packets {
        server
            .handle_packet(packet)
            .expect("server should still accept packets after warning alert");
    }
    now += Duration::from_millis(10);
    server.handle_timeout(now).expect("server timeout");
    let server_out = drain_outputs(&mut server);
    assert!(
        server_out
            .app_data
            .iter()
            .any(|data| data.as_slice() == b"still-open"),
        "application data should still be delivered after warning alert"
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls13_unknown_warning_level_alert_is_still_fatal() {
    let _ = env_logger::try_init();

    let server_cert = generate_self_signed_certificate().expect("gen server cert");
    let config = dtls13_config();
    let now = Instant::now();

    let mut server = Dtls::new_13(config, server_cert, now);
    server.set_active(false);

    // handshake_failure(40) with level=warning(1): TLS 1.3 ignores the level
    // byte, so this must still be treated as fatal.
    let packet = dtls13_alert_record(1, 1, 40);
    let err = server
        .handle_packet(&packet)
        .expect_err("non-whitelisted alert must be fatal regardless of level");
    assert!(matches!(
        err,
        dimpl::Error::SecurityError(dimpl::SecurityError::FatalAlert { description: 40 })
    ));
}

fn queue_ack_with_peer_key_update(sender: &mut Dtls, receiver: &mut Dtls, now: &mut Instant) {
    for i in 0..5 {
        sender
            .send_application_data(format!("msg{i}").as_bytes())
            .expect("send app data");
    }

    *now += Duration::from_millis(10);
    sender.handle_timeout(*now).expect("sender timeout");
    let sender_out = drain_outputs(sender);
    assert!(
        !sender_out.packets.is_empty(),
        "sender should emit app data and KeyUpdate"
    );

    for packet in &sender_out.packets {
        receiver
            .handle_packet(packet)
            .expect("receiver should accept KeyUpdate batch");
    }
}

fn drain_expected_app_data(endpoint: &mut Dtls, expected: usize) {
    let mut buf = vec![0u8; 2048];
    for i in 0..expected {
        match endpoint.poll_output(&mut buf) {
            Output::ApplicationData(data) => {
                assert_eq!(
                    data,
                    format!("msg{i}").as_bytes(),
                    "unexpected queued application data before close()"
                );
            }
            _ => panic!("expected queued application data"),
        }
    }
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls13_client_close_after_queued_ack_sends_close_notify() {
    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");
    let config = Arc::new(
        Config::builder()
            .aead_encryption_limit(5)
            .build()
            .expect("build config"),
    );

    let mut now = Instant::now();

    let mut client = Dtls::new_13(Arc::clone(&config), client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_13(config, server_cert, now);
    server.set_active(false);

    now = complete_dtls13_handshake(&mut client, &mut server, now);

    queue_ack_with_peer_key_update(&mut server, &mut client, &mut now);
    drain_expected_app_data(&mut client, 5);

    client
        .close()
        .expect("close should succeed with queued ACK pending");

    now += Duration::from_millis(10);
    client.handle_timeout(now).expect("client timeout");
    let mut buf = vec![0u8; 2048];
    let first_packet = match client.poll_output(&mut buf) {
        Output::Packet(packet) => packet.to_vec(),
        _ => panic!("expected first close output packet"),
    };

    server
        .handle_packet(&first_packet)
        .expect("server should accept the first client close packet");
    now += Duration::from_millis(10);
    server.handle_timeout(now).expect("server timeout");
    let server_out = drain_outputs(&mut server);
    assert!(
        server_out.close_notify,
        "server should observe close_notify from the first client close packet"
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls13_server_close_after_queued_ack_sends_close_notify() {
    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");
    let config = Arc::new(
        Config::builder()
            .aead_encryption_limit(5)
            .build()
            .expect("build config"),
    );

    let mut now = Instant::now();

    let mut client = Dtls::new_13(Arc::clone(&config), client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_13(config, server_cert, now);
    server.set_active(false);

    now = complete_dtls13_handshake(&mut client, &mut server, now);

    queue_ack_with_peer_key_update(&mut client, &mut server, &mut now);
    drain_expected_app_data(&mut server, 5);

    server
        .close()
        .expect("close should succeed with queued ACK pending");

    now += Duration::from_millis(10);
    server.handle_timeout(now).expect("server timeout");
    let mut buf = vec![0u8; 2048];
    let first_packet = match server.poll_output(&mut buf) {
        Output::Packet(packet) => packet.to_vec(),
        _ => panic!("expected first close output packet"),
    };

    client
        .handle_packet(&first_packet)
        .expect("client should accept the first server close packet");
    now += Duration::from_millis(10);
    client.handle_timeout(now).expect("client timeout");
    let client_out = drain_outputs(&mut client);
    assert!(
        client_out.close_notify,
        "client should observe close_notify from the first server close packet"
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls13_discards_unknown_epoch_record() {
    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    let config = dtls13_config();

    let mut now = Instant::now();

    let mut client = Dtls::new_13(Arc::clone(&config), client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_13(config, server_cert, now);
    server.set_active(false);

    now = complete_dtls13_handshake(&mut client, &mut server, now);

    // After handshake, application data uses epoch 3 (epoch_bits = 3 & 0x03 = 3).
    // Craft a ciphertext record with epoch_bits=1, which would map to epoch 1 if
    // no keys exist for it (or to an epoch whose low 2 bits are 01, e.g. epoch 5
    // which has never been negotiated).
    //
    // Unified header: 001CSLEE with C=0, S=1, L=1, EE=01 => 0b0010_1101 = 0x2D.
    // This targets epoch_bits=1 -- no keys installed for any epoch with low bits 01.
    let mut bogus = Vec::new();
    bogus.push(0x2D); // flags: S=1, L=1, epoch_bits=01
    bogus.extend_from_slice(&0x0000u16.to_be_bytes()); // encrypted seq bits
    bogus.extend_from_slice(&0x0020u16.to_be_bytes()); // length = 32
    bogus.extend_from_slice(&[0xAA; 32]); // fake ciphertext (will fail AEAD)

    // Should be silently discarded (decryption will fail since no keys for this epoch)
    client
        .handle_packet(&bogus)
        .expect("unknown-epoch record should be discarded");

    // Verify normal data exchange still works.
    client.send_application_data(b"ping").expect("send app");
    client.handle_timeout(now).expect("client timeout");
    let client_out = drain_outputs(&mut client);
    deliver_packets(&client_out.packets, &mut server);

    server.handle_timeout(now).expect("server timeout");
    let server_out = drain_outputs(&mut server);
    assert!(
        server_out.app_data.iter().any(|d| d.as_slice() == b"ping"),
        "Server should receive application data after unknown-epoch bogus packet"
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls13_discards_truncated_unified_header() {
    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    let config = dtls13_config();

    let mut now = Instant::now();

    let mut client = Dtls::new_13(Arc::clone(&config), client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_13(config, server_cert, now);
    server.set_active(false);

    now = complete_dtls13_handshake(&mut client, &mut server, now);

    // Deliver a 1-byte packet that looks like a unified header but is truncated.
    // 0x2F = 001CSLEE with C=0, S=1, L=1, EE=11 -- expects at least 5 header
    // bytes (flags + 2 seq + 2 length) but we only provide the flags byte.
    let bogus = vec![0x2F];

    // The parser requires at least 2 bytes for a ciphertext record. This should
    // result in a parse error, but handle_packet may surface it as Err. Either way,
    // the endpoint must remain operational.
    let _ = client.handle_packet(&bogus);

    // Verify normal operation continues.
    client.send_application_data(b"ping").expect("send app");
    client.handle_timeout(now).expect("client timeout");
    let client_out = drain_outputs(&mut client);
    deliver_packets(&client_out.packets, &mut server);

    server.handle_timeout(now).expect("server timeout");
    let server_out = drain_outputs(&mut server);
    assert!(
        server_out.app_data.iter().any(|d| d.as_slice() == b"ping"),
        "Server should receive application data after truncated header packet"
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls13_discards_plaintext_after_handshake() {
    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    let config = dtls13_config();

    let mut now = Instant::now();

    let mut client = Dtls::new_13(Arc::clone(&config), client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_13(config, server_cert, now);
    server.set_active(false);

    now = complete_dtls13_handshake(&mut client, &mut server, now);

    // Craft a DTLS 1.2-style plaintext record (13-byte header).
    // content_type=22 (Handshake), version=0xFEFD (DTLS 1.2), epoch=0, seq=0,
    // length=5, then 5 bytes of garbage body.
    let bogus = vec![
        0x16, // content_type: Handshake
        0xFE, 0xFD, // version: DTLS 1.2
        0x00, 0x00, // epoch: 0
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // sequence_number: 0
        0x00, 0x05, // length: 5
        0x01, 0x00, 0x00, 0x00, 0x00, // 5 bytes of fake handshake body
    ];

    // Delivering a plaintext handshake record after the handshake is complete should
    // be silently discarded per RFC 9147. The connection must remain operational.
    client
        .handle_packet(&bogus)
        .expect("silently discard should not return error");

    // Verify application data exchange still works.
    client
        .send_application_data(b"after-plaintext")
        .expect("send app");
    client.handle_timeout(now).expect("client timeout");
    let client_out = drain_outputs(&mut client);
    deliver_packets(&client_out.packets, &mut server);

    server.handle_timeout(now).expect("server timeout");
    let server_out = drain_outputs(&mut server);
    assert!(
        server_out
            .app_data
            .iter()
            .any(|d| d.as_slice() == b"after-plaintext"),
        "Server should receive application data after plaintext bogus packet"
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls13_post_encryption_plaintext_close_notify_is_ignored() {
    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    let config = dtls13_config();

    let mut now = Instant::now();

    let mut client = Dtls::new_13(Arc::clone(&config), client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_13(config, server_cert, now);
    server.set_active(false);

    now = complete_dtls13_handshake(&mut client, &mut server, now);

    server
        .handle_packet(&dtls13_alert_record(0x100, 1, 0))
        .expect("post-encryption plaintext close_notify should be ignored");

    let after_plaintext_alert = drain_outputs(&mut server);
    assert!(
        !after_plaintext_alert.close_notify,
        "plaintext close_notify after encryption must not close the connection"
    );

    client
        .send_application_data(b"after-plaintext-close-notify")
        .expect("send app data");
    client.handle_timeout(now).expect("client timeout");
    let client_out = drain_outputs(&mut client);
    deliver_packets(&client_out.packets, &mut server);

    server.handle_timeout(now).expect("server timeout");
    let server_out = drain_outputs(&mut server);
    assert!(
        server_out
            .app_data
            .iter()
            .any(|d| d.as_slice() == b"after-plaintext-close-notify"),
        "server should accept encrypted app data after plaintext close_notify"
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls13_post_encryption_plaintext_fatal_alert_is_ignored() {
    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    let config = dtls13_config();

    let mut now = Instant::now();

    let mut client = Dtls::new_13(Arc::clone(&config), client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_13(config, server_cert, now);
    server.set_active(false);

    now = complete_dtls13_handshake(&mut client, &mut server, now);

    server
        .handle_packet(&dtls13_alert_record(0x101, 2, 40))
        .expect("post-encryption plaintext fatal alert should be ignored");

    client
        .send_application_data(b"after-plaintext-fatal-alert")
        .expect("send app data");
    client.handle_timeout(now).expect("client timeout");
    let client_out = drain_outputs(&mut client);
    deliver_packets(&client_out.packets, &mut server);

    server.handle_timeout(now).expect("server timeout");
    let server_out = drain_outputs(&mut server);
    assert!(
        server_out
            .app_data
            .iter()
            .any(|d| d.as_slice() == b"after-plaintext-fatal-alert"),
        "server should accept encrypted app data after plaintext fatal alert"
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls13_post_encryption_plaintext_ack_does_not_stop_retransmit() {
    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    let config = Arc::new(
        Config::builder()
            .flight_start_rto(Duration::from_millis(100))
            .build()
            .expect("build config"),
    );

    let mut now = Instant::now();

    let mut client = Dtls::new_13(Arc::clone(&config), client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_13(config, server_cert, now);
    server.set_active(false);

    client.handle_timeout(now).expect("client timeout");
    let client_hello = collect_packets(&mut client);
    assert!(!client_hello.is_empty(), "client should emit ClientHello");

    deliver_packets(&client_hello, &mut server);
    server.handle_timeout(now).expect("server timeout");
    let first_server_flight = collect_packets(&mut server);
    assert!(
        !first_server_flight.is_empty(),
        "server should emit first flight"
    );

    // If this unauthenticated plaintext ACK reaches process_ack, it can mark
    // the saved epoch-2 handshake flight as fully acknowledged and disable
    // retransmission. The classify_record guard must discard it first.
    let acked_records: Vec<(u64, u64)> = (0..64).map(|seq| (2, seq)).collect();
    server
        .handle_packet(&dtls13_ack_record_for_records(0x200, &acked_records))
        .expect("post-encryption plaintext ACK should be ignored");

    // The flight timer jitter is absolute (+/-250 ms), so wait past the
    // maximum possible jitter for a 100 ms start RTO.
    now += Duration::from_millis(400);
    server.handle_timeout(now).expect("server timeout");
    let retransmit = collect_packets(&mut server);
    assert!(
        !retransmit.is_empty(),
        "forged plaintext ACK must not stop server flight retransmission"
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls13_duplicate_client_hello_still_triggers_retransmit_after_peer_encryption() {
    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    let config = dtls13_config();

    let now = Instant::now();

    let mut client = Dtls::new_13(Arc::clone(&config), client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_13(config, server_cert, now);
    server.set_active(false);

    client.handle_timeout(now).expect("client timeout");
    let client_hello = collect_packets(&mut client);
    assert!(!client_hello.is_empty(), "client should emit ClientHello");

    deliver_packets(&client_hello, &mut server);
    server.handle_timeout(now).expect("server timeout");
    let first_server_flight = collect_packets(&mut server);
    assert!(
        !first_server_flight.is_empty(),
        "server should emit ServerHello plus encrypted handshake flight"
    );

    deliver_packets(&client_hello, &mut server);
    let retransmit = collect_packets(&mut server);
    assert!(
        !retransmit.is_empty(),
        "duplicate plaintext ClientHello should still trigger server flight retransmission"
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls13_alert_bad_certificate() {
    // NOTE: dimpl does not perform certificate chain/trust validation. The library
    // surfaces the peer's leaf certificate via Output::PeerCert and delegates all
    // validation to the application layer. There is no configurable certificate
    // verifier or trust store that could cause the handshake to fail due to a
    // "bad certificate".
    //
    // This test documents the gap: dimpl should ideally support a pluggable
    // certificate verifier callback (e.g., via Config) so that applications can
    // reject untrusted certificates and trigger an appropriate alert.
    //
    // Since both endpoints use self-signed certificates and dimpl accepts them
    // unconditionally, we verify that the handshake completes and the peer
    // certificates are surfaced via Output::PeerCert. The application would
    // then inspect the certificate and decide whether to continue.

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    // Store the DER bytes so we can verify PeerCert output
    let client_cert_der = client_cert.certificate.clone();
    let server_cert_der = server_cert.certificate.clone();

    let config = dtls13_config();

    let mut now = Instant::now();

    let mut client = Dtls::new_13(Arc::clone(&config), client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_13(config, server_cert, now);
    server.set_active(false);

    let mut client_connected = false;
    let mut server_connected = false;
    let mut client_peer_cert: Option<Vec<u8>> = None;
    let mut server_peer_cert: Option<Vec<u8>> = None;

    for _ in 0..40 {
        client.handle_timeout(now).expect("client timeout");
        server.handle_timeout(now).expect("server timeout");

        let client_out = drain_outputs(&mut client);
        let server_out = drain_outputs(&mut server);

        client_connected |= client_out.connected;
        server_connected |= server_out.connected;

        if client_out.peer_cert.is_some() {
            client_peer_cert = client_out.peer_cert;
        }
        if server_out.peer_cert.is_some() {
            server_peer_cert = server_out.peer_cert;
        }

        deliver_packets(&client_out.packets, &mut server);
        deliver_packets(&server_out.packets, &mut client);

        if client_connected && server_connected {
            break;
        }
        now += Duration::from_millis(10);
    }

    assert!(client_connected, "Client should be connected");
    assert!(server_connected, "Server should be connected");

    // Verify that PeerCert was emitted so the application can inspect it.
    // The client should see the server's certificate and vice versa.
    let client_saw_cert = client_peer_cert.expect("Client should receive PeerCert");
    assert_eq!(
        client_saw_cert, server_cert_der,
        "Client's PeerCert should match the server's certificate"
    );

    let server_saw_cert = server_peer_cert.expect("Server should receive PeerCert");
    assert_eq!(
        server_saw_cert, client_cert_der,
        "Server's PeerCert should match the client's certificate"
    );

    // Gap: no way to reject a certificate and trigger a bad_certificate alert.
    // When a certificate verifier callback is added to Config, this test should
    // be updated to install a verifier that rejects the peer's self-signed cert
    // and assert the handshake fails with an appropriate error.
}

/// Regression test for bug #1: advertised signature schemes must all be functional.
///
/// Previously, Ed25519 (0x0807) and RSA_PSS_RSAE_SHA256 (0x0804) were advertised
/// in the ClientHello signature_algorithms extension but could not be verified,
/// causing handshake failures when a peer selected them.
#[test]
#[cfg(feature = "rcgen")]
fn dtls13_only_functional_signature_schemes_advertised() {
    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");

    let config = dtls13_config();

    let now = Instant::now();
    let mut client = Dtls::new_13(config, client_cert, now);
    client.set_active(true);

    // Trigger the first ClientHello
    client.handle_timeout(now).expect("client timeout");
    let out = drain_outputs(&mut client);
    assert!(!out.packets.is_empty(), "Client should send ClientHello");

    // The first packet is a plaintext record containing the ClientHello.
    // DTLSPlaintext header: content_type(1) + version(2) + epoch(2) + seq(6) + length(2) = 13 bytes.
    // Handshake header: msg_type(1) + length(3) + message_seq(2)
    //                   + frag_offset(3) + frag_length(3) = 12 bytes.
    // ClientHello body: version(2) + random(32) + session_id_len(1) + session_id + cookie_len(1) + cookie
    //                   + cipher_suites_len(2) + cipher_suites + comp_methods_len(1) + comp_methods
    //                   + extensions_len(2) + extensions...
    let ch_packet = &out.packets[0];

    // Find signature_algorithms extension (type 0x000D) in the packet.
    // Scan for the 2-byte extension type.
    let sig_alg_type: [u8; 2] = [0x00, 0x0D];
    let pos = ch_packet
        .windows(2)
        .position(|w| w == sig_alg_type)
        .expect("signature_algorithms extension should be present in ClientHello");

    // Extension format: type(2) + ext_len(2) + list_len(2) + schemes(2 each)
    let ext_len_offset = pos + 2;
    let list_len_offset = ext_len_offset + 2;
    let list_len =
        u16::from_be_bytes([ch_packet[list_len_offset], ch_packet[list_len_offset + 1]]) as usize;
    let schemes_start = list_len_offset + 2;

    // Extract all advertised scheme codes
    let mut advertised: Vec<u16> = Vec::new();
    let mut i = schemes_start;
    while i < schemes_start + list_len {
        let scheme = u16::from_be_bytes([ch_packet[i], ch_packet[i + 1]]);
        advertised.push(scheme);
        i += 2;
    }

    // Only ECDSA P-256 (0x0403) and P-384 (0x0503) should be advertised.
    let non_functional: Vec<u16> = advertised
        .iter()
        .copied()
        .filter(|s| *s != 0x0403 && *s != 0x0503)
        .collect();

    assert!(
        non_functional.is_empty(),
        "Non-functional signature schemes advertised: {:04X?}. \
         Only ECDSA_SECP256R1_SHA256 (0x0403) and ECDSA_SECP384R1_SHA384 (0x0503) \
         should be advertised.",
        non_functional,
    );

    assert_eq!(
        advertised,
        vec![0x0403, 0x0503],
        "Expected exactly ECDSA P-256 and P-384"
    );
}

/// Regression test for bug #2: a bad record in a multi-record datagram must not
/// kill subsequent valid records.
///
/// Previously, certain per-record errors (parse failure, content type recovery)
/// would propagate as Err and abort parsing of the entire datagram, causing valid
/// records after the bad one to be lost.
#[test]
#[cfg(feature = "rcgen")]
fn dtls13_bad_record_does_not_kill_datagram() {
    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    let config = dtls13_config();

    let mut now = Instant::now();

    let mut client = Dtls::new_13(Arc::clone(&config), client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_13(config, server_cert, now);
    server.set_active(false);

    now = complete_dtls13_handshake(&mut client, &mut server, now);

    // Send application data from server and capture the ciphertext packet.
    server
        .send_application_data(b"hello")
        .expect("send app data");
    server.handle_timeout(now).expect("server timeout");
    let server_out = drain_outputs(&mut server);
    assert!(
        !server_out.packets.is_empty(),
        "Server should produce a packet"
    );
    let good_record = &server_out.packets[0];

    // Construct a bogus ciphertext record with unknown epoch (epoch_bits=01).
    // Unified header: flags=0x2D (001 C=0 S=1 L=1 EE=01), seq=0x0000, length=32.
    let mut bogus_record = Vec::new();
    bogus_record.push(0x2D); // flags: S=1, L=1, epoch_bits=01
    bogus_record.extend_from_slice(&0x0000u16.to_be_bytes()); // encrypted seq
    bogus_record.extend_from_slice(&0x0020u16.to_be_bytes()); // length = 32
    bogus_record.extend_from_slice(&[0xAA; 32]); // fake ciphertext

    // Build a multi-record datagram: bogus record FIRST, then the valid record.
    // If the bad record aborts the datagram, the good record is lost.
    let mut combined = bogus_record;
    combined.extend_from_slice(good_record);

    // Deliver the combined datagram. Must not error.
    client
        .handle_packet(&combined)
        .expect("multi-record datagram with bad first record should not error");

    // The valid record should have been processed despite the bad first record.
    client.handle_timeout(now).expect("client timeout");
    let client_out = drain_outputs(&mut client);
    assert!(
        client_out.app_data.iter().any(|d| d.as_slice() == b"hello"),
        "Client should receive the valid app data record that followed the bogus record"
    );
}

/// Regression test for bug #3: single replay window across epochs.
///
/// Before the fix, a single `ReplayWindow` tracked all epochs. When a
/// new-epoch record arrived (after KeyUpdate), the window advanced and
/// permanently rejected all old-epoch records — even though old receive
/// keys were still retained. This test verifies that a delayed old-epoch
/// packet IS accepted and delivered as application data after a KeyUpdate.
#[test]
#[cfg(feature = "rcgen")]
fn dtls13_old_epoch_record_accepted_after_key_update() {
    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    // Low limit to trigger KeyUpdate quickly.
    let config = Arc::new(
        Config::builder()
            .aead_encryption_limit(5)
            .build()
            .expect("build config"),
    );

    let mut now = Instant::now();

    let mut client = Dtls::new_13(Arc::clone(&config), client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_13(config, server_cert, now);
    server.set_active(false);

    now = complete_dtls13_handshake(&mut client, &mut server, now);

    // Send one message and capture its packet WITHOUT delivering to server.
    // This packet is encrypted on the initial application epoch (epoch 3).
    client
        .send_application_data(b"old-epoch-data")
        .expect("send delayed msg");
    now += Duration::from_millis(10);
    client.handle_timeout(now).expect("client timeout");
    let client_out = drain_outputs(&mut client);
    let delayed_packets = client_out.packets.clone();

    // Process server output (don't deliver delayed to server).
    server.handle_timeout(now).expect("server timeout");
    let server_out = drain_outputs(&mut server);
    deliver_packets(&server_out.packets, &mut client);

    // Send enough messages to trigger at least one KeyUpdate (limit=5).
    let mut server_received = 0;
    for i in 0..12 {
        let msg = format!("msg-{}", i);
        client
            .send_application_data(msg.as_bytes())
            .expect("send app data");

        now += Duration::from_millis(10);

        for _ in 0..3 {
            client.handle_timeout(now).expect("client timeout");
            let client_out = drain_outputs(&mut client);
            deliver_packets(&client_out.packets, &mut server);

            server.handle_timeout(now).expect("server timeout");
            let server_out = drain_outputs(&mut server);
            deliver_packets(&server_out.packets, &mut client);

            server_received += server_out.app_data.len();
        }
    }

    assert_eq!(
        server_received, 12,
        "All regular messages should be received"
    );

    // NOW deliver the old-epoch packet. With per-epoch replay windows,
    // the epoch-3 window hasn't seen this sequence number, so it must be
    // accepted and decrypted using the retained old-epoch keys.
    deliver_packets(&delayed_packets, &mut server);
    now += Duration::from_millis(10);
    server.handle_timeout(now).expect("server timeout");
    let server_out = drain_outputs(&mut server);
    deliver_packets(&server_out.packets, &mut client);

    assert!(
        server_out
            .app_data
            .iter()
            .any(|d| d.as_slice() == b"old-epoch-data"),
        "Server must receive the delayed old-epoch packet (per-epoch replay windows)"
    );
}

/// Test that the ClientHello is padded to fill the configured MTU.
///
/// RFC 7685 defines the padding extension (type 0x0015) to increase
/// the ClientHello size, reducing the server-to-client amplification
/// factor in DTLS.
#[test]
#[cfg(feature = "rcgen")]
fn dtls13_client_hello_padded_to_mtu() {
    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let mtu = 1150; // default MTU

    let config = dtls13_config();

    let now = Instant::now();
    let mut client = Dtls::new_13(config, client_cert, now);
    client.set_active(true);

    client.handle_timeout(now).expect("client timeout");
    let out = drain_outputs(&mut client);
    assert!(!out.packets.is_empty(), "Client should send ClientHello");

    let ch_packet = &out.packets[0];

    // The ClientHello record should fill the MTU exactly.
    assert_eq!(
        ch_packet.len(),
        mtu,
        "ClientHello packet should be padded to MTU ({} bytes), got {} bytes",
        mtu,
        ch_packet.len()
    );

    // Verify the padding extension (type 0x0015) is present.
    let padding_type: [u8; 2] = [0x00, 0x15];
    let has_padding = ch_packet.windows(2).any(|w| w == padding_type);
    assert!(
        has_padding,
        "ClientHello should contain a padding extension (type 0x0015)"
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls13_mixed_datagram_during_handshake_bogus_first() {
    //! Test that during handshake, a mixed datagram with bogus plaintext
    //! ApplicationData first and valid handshake record second is handled
    //! correctly: bogus is discarded, valid handshake proceeds.

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    let config = dtls13_config();

    let now = Instant::now();

    let mut client = Dtls::new_13(Arc::clone(&config), client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_13(config, server_cert, now);
    server.set_active(false);

    // Client sends ClientHello.
    client.handle_timeout(now).expect("client timeout");
    let client_out = drain_outputs(&mut client);
    assert!(!client_out.packets.is_empty(), "Should have ClientHello");
    let client_hello = &client_out.packets[0];

    // Craft bogus plaintext ApplicationData.
    let bogus = vec![
        0x17, // content_type: ApplicationData
        0xFE, 0xFD, // version: DTLS 1.2
        0x00, 0x00, // epoch: 0 (plaintext)
        0x00, 0x00, 0x00, 0x00, 0x00, 0x80, // sequence_number
        0x00, 0x05, // length: 5
        0x62, 0x6F, 0x67, 0x75, 0x73, // "bogus"
    ];

    // Build mixed datagram: bogus first, then ClientHello.
    let mut mixed = bogus;
    mixed.extend_from_slice(client_hello);

    // Deliver mixed datagram to server.
    server
        .handle_packet(&mixed)
        .expect("mixed datagram should not error");

    // Server should process the ClientHello despite the bogus record.
    server.handle_timeout(now).expect("server timeout");
    let server_out = drain_outputs(&mut server);
    assert!(
        !server_out.packets.is_empty(),
        "Server should send ServerHello flight despite bogus record"
    );

    // Continue handshake normally.
    deliver_packets(&server_out.packets, &mut client);
    complete_dtls13_handshake(&mut client, &mut server, now);
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls13_mixed_datagram_plaintext_first_then_valid() {
    //! Post-handshake: a UDP datagram with bogus plaintext ApplicationData FIRST
    //! followed by a valid encrypted record is handled correctly: the bogus
    //! record is silently discarded and the valid one is still processed.

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    let config = dtls13_config();

    let mut now = Instant::now();

    let mut client = Dtls::new_13(Arc::clone(&config), client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_13(config, server_cert, now);
    server.set_active(false);

    now = complete_dtls13_handshake(&mut client, &mut server, now);

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
fn dtls13_mixed_datagram_valid_first_then_bogus() {
    //! Post-handshake: a UDP datagram with a valid encrypted record FIRST
    //! followed by bogus plaintext ApplicationData is handled correctly: the
    //! valid record is processed and the trailing bogus record is discarded.

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    let config = dtls13_config();

    let mut now = Instant::now();

    let mut client = Dtls::new_13(Arc::clone(&config), client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_13(config, server_cert, now);
    server.set_active(false);

    now = complete_dtls13_handshake(&mut client, &mut server, now);

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
fn dtls13_half_close_send_then_close() {
    //! After receiving close_notify, the write half remains open per RFC 8446 §6.1.
    //! The local side can send application data (half-close), and the data must
    //! be delivered to the peer. Then close() shuts down the write half.

    let _ = env_logger::try_init();
    let mut now = Instant::now();
    let (mut client, mut server, now_hs) = setup_connected_13_pair(now);
    now = now_hs;

    // Client sends close_notify
    client.close().unwrap();
    now += Duration::from_millis(10);
    client.handle_timeout(now).expect("client timeout");
    let client_out = drain_outputs(&mut client);
    deliver_packets(&client_out.packets, &mut server);

    server.handle_timeout(now).expect("server timeout");
    let server_out = drain_outputs(&mut server);
    assert!(server_out.close_notify, "Server should emit CloseNotify");

    // Half-close: server can still send after receiving close_notify
    server
        .send_application_data(b"half-close-data")
        .expect("send after close_notify should work");

    now += Duration::from_millis(10);
    server.handle_timeout(now).expect("server timeout");
    let server_out = drain_outputs(&mut server);
    deliver_packets(&server_out.packets, &mut client);

    // Client receives the data sent during half-close
    client.handle_timeout(now).expect("client timeout");
    let client_out = drain_outputs(&mut client);
    assert!(
        client_out
            .app_data
            .iter()
            .any(|d| d.as_slice() == b"half-close-data"),
        "Client should receive data sent during half-close"
    );

    // Server closes its write half
    server.close().unwrap();

    // After local close(), sends must fail
    assert!(
        server.send_application_data(b"after-own-close").is_err(),
        "Server should not accept sends after its own close()"
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls13_close_during_handshake_emits_no_packets() {
    //! Call close() on the client while the handshake is in progress.
    //! Per `Dtls::close` API contract, close() during handshake silently
    //! discards state without sending any packets.

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    let config = dtls13_config();

    let now = Instant::now();

    let mut client = Dtls::new_13(Arc::clone(&config), client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_13(config, server_cert, now);
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
    let _ = client.handle_timeout(later);
    let client_out = drain_outputs(&mut client);
    assert!(
        client_out.packets.is_empty(),
        "Client should not emit packets after timeout post-close()"
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls13_app_data_delivered_before_close_notify() {
    //! When app data and close_notify arrive in the same batch, the app data
    //! must be delivered before CloseNotify.

    let _ = env_logger::try_init();
    let mut now = Instant::now();
    let (mut client, mut server, now_hs) = setup_connected_13_pair(now);
    now = now_hs;

    // Send app data then immediately close (both queued)
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
fn dtls13_close_notify_out_of_order_app_data_accepted() {
    //! Out-of-order app data packets (sequence < close_notify sequence) that
    //! arrive after close_notify must still be accepted and delivered.

    let _ = env_logger::try_init();
    let mut now = Instant::now();
    let (mut client, mut server, now_hs) = setup_connected_13_pair(now);
    now = now_hs;

    // Server sends app data (seq N), then closes (close_notify at seq N+1)
    server
        .send_application_data(b"before-close-data")
        .expect("send app data");
    now += Duration::from_millis(10);
    server.handle_timeout(now).expect("server timeout");
    let app_data_packets = drain_outputs(&mut server).packets;

    server.close().unwrap();
    now += Duration::from_millis(10);
    server.handle_timeout(now).expect("server timeout");
    let close_packets = drain_outputs(&mut server).packets;

    // Deliver close_notify FIRST (out of order), then app data
    deliver_packets(&close_packets, &mut client);
    deliver_packets(&app_data_packets, &mut client);

    now += Duration::from_millis(10);
    client.handle_timeout(now).expect("client timeout");
    let client_out = drain_outputs(&mut client);

    // Client should still deliver the app data (its sequence < close_notify sequence)
    assert!(
        client_out
            .app_data
            .iter()
            .any(|d| d.as_slice() == b"before-close-data"),
        "Out-of-order app data with earlier sequence should be accepted"
    );

    // Client should also see CloseNotify
    assert!(client_out.close_notify, "Client should emit CloseNotify");
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls13_half_closed_local_no_retransmit() {
    //! After close(), in-flight retransmissions (e.g. a pending KeyUpdate
    //! awaiting ACK) must be cancelled. Advancing time past retransmit
    //! timeouts should produce no packets.

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    // Low AEAD limit so we can trigger a KeyUpdate after a few app-data records.
    let config = Arc::new(
        Config::builder()
            .aead_encryption_limit(3)
            .build()
            .expect("build config"),
    );

    let mut now = Instant::now();

    let mut client = Dtls::new_13(Arc::clone(&config), client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_13(config, server_cert, now);
    server.set_active(false);

    now = complete_dtls13_handshake(&mut client, &mut server, now);

    // Send enough app data from client to trigger needs_key_update.
    // aead_encryption_limit(3) → threshold is 3 (quarter=0, no jitter).
    for i in 0..3 {
        client
            .send_application_data(format!("msg{}", i).as_bytes())
            .expect("send app data");
    }

    // handle_timeout → make_progress → creates KeyUpdate, arms flight timer.
    // This puts KeyUpdate records into flight_saved_records for retransmission.
    now += Duration::from_millis(10);
    client.handle_timeout(now).expect("client timeout");
    let client_out = drain_outputs(&mut client);

    // Deliver app data to server but NOT the KeyUpdate ACK back to client,
    // so the client has an in-flight KeyUpdate awaiting acknowledgement.
    deliver_packets(&client_out.packets, &mut server);
    now += Duration::from_millis(10);
    server.handle_timeout(now).expect("server timeout");
    // Intentionally do NOT deliver server's ACK/response back to client.
    let _ = drain_outputs(&mut server);

    // Now close() — should cancel the in-flight KeyUpdate retransmission.
    client.close().unwrap();
    now += Duration::from_millis(10);
    client.handle_timeout(now).expect("client timeout");
    // Drain the close_notify packet
    let _ = drain_outputs(&mut client);

    // Advance time well past flight retransmit timeouts — should emit no packets.
    for _ in 0..5 {
        now += Duration::from_secs(5);
        client.handle_timeout(now).expect("client timeout");
        let client_out = drain_outputs(&mut client);
        assert!(
            client_out.packets.is_empty(),
            "No retransmission packets should be emitted after close()"
        );
    }

    // send_application_data must fail
    let result = client.send_application_data(b"should-fail");
    assert!(
        result.is_err(),
        "send_application_data should fail in HalfClosedLocal"
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls13_half_closed_local_transitions_to_closed() {
    //! After client calls close() (HalfClosedLocal), receiving the peer's
    //! close_notify should transition to Closed and emit CloseNotify.

    let _ = env_logger::try_init();
    let mut now = Instant::now();
    let (mut client, mut server, now_hs) = setup_connected_13_pair(now);
    now = now_hs;

    assert!(!client.is_closing());
    assert!(!client.is_closed());
    assert!(!server.is_closing());
    assert!(!server.is_closed());

    // Client calls close() → HalfClosedLocal
    client.close().unwrap();
    assert!(client.is_closing());
    assert!(!client.is_closed());

    now += Duration::from_millis(10);
    client.handle_timeout(now).expect("client timeout");
    let client_out = drain_outputs(&mut client);
    assert!(client.is_closing());
    assert!(!client.is_closed());

    // Deliver client's close_notify to server
    deliver_packets(&client_out.packets, &mut server);
    server.handle_timeout(now).expect("server timeout");
    assert!(server.is_closing());
    assert!(!server.is_closed());

    let server_out = drain_outputs(&mut server);
    assert!(server_out.close_notify, "Server should see CloseNotify");
    assert!(!server.is_closing());
    assert!(!server.is_closed());

    // Server calls close() → sends its own close_notify
    server.close().unwrap();
    now += Duration::from_millis(10);
    server.handle_timeout(now).expect("server timeout");
    assert!(server.is_closing());
    assert!(!server.is_closed());

    let server_out = drain_outputs(&mut server);
    assert!(!server.is_closing());
    assert!(server.is_closed());

    // Deliver server's close_notify to client
    deliver_packets(&server_out.packets, &mut client);
    now += Duration::from_millis(10);
    client.handle_timeout(now).expect("client timeout");
    assert!(client.is_closing());
    assert!(!client.is_closed());

    let client_out = drain_outputs(&mut client);

    // Client should emit CloseNotify (peer's close_notify received)
    assert!(
        client_out.close_notify,
        "Client should emit CloseNotify after receiving peer's close_notify"
    );
    assert!(!client.is_closing());
    assert!(client.is_closed());
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls13_close_state_matrix() {
    for client_closes_first in [true, false] {
        let (mut client, mut server, _) = setup_connected_13_pair(Instant::now());
        if !client_closes_first {
            std::mem::swap(&mut client, &mut server);
        }

        assert!(!client.is_closing());
        assert!(!client.is_closed());
        assert!(!server.is_closing());
        assert!(!server.is_closed());

        client.close().expect("initial close");
        assert!(client.is_closing(), "local close pending");
        assert!(!client.is_closed(), "local close not terminal");

        let client_out = drain_outputs(&mut client);
        assert!(!client_out.packets.is_empty(), "local close_notify packet");
        assert!(
            client.is_closing(),
            "local close remains pending until peer close_notify"
        );
        assert!(!client.is_closed(), "local half-close is not terminal");

        deliver_packets(&client_out.packets, &mut server);
        assert!(server.is_closing(), "remote close pending");
        assert!(!server.is_closed(), "remote close not drained");

        let server_out = drain_outputs(&mut server);
        assert!(server_out.close_notify, "remote CloseNotify event");
        assert!(!server.is_closing(), "remote close event drained");
        assert!(
            !server.is_closed(),
            "remote half-close leaves write side open"
        );

        server.close().expect("reciprocal close");
        assert!(server.is_closing(), "remote reciprocal close pending");
        assert!(!server.is_closed(), "remote reciprocal close not drained");

        let server_out = drain_outputs(&mut server);
        assert!(
            !server_out.packets.is_empty(),
            "remote reciprocal close_notify packet"
        );
        assert!(!server.is_closing(), "remote reciprocal close drained");
        assert!(server.is_closed(), "remote close terminal");

        deliver_packets(&server_out.packets, &mut client);
        assert!(client.is_closing(), "local close peer response pending");
        assert!(!client.is_closed(), "local close peer response not drained");

        let client_out = drain_outputs(&mut client);
        assert!(client_out.close_notify, "local CloseNotify event");
        assert!(!client.is_closing(), "local close drained");
        assert!(client.is_closed(), "local close terminal");
    }
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls13_close_prohibits_further_sends() {
    //! After close(), the sender enters HalfClosedLocal and
    //! send_application_data() must return an error.
    //!
    //! Note: the receiver-side sequence-threshold discard (RFC 9147 §5.10) is
    //! exercised by `dtls13_close_notify_out_of_order_app_data_accepted` (accept
    //! path). The discard path (seq > close_notify seq) cannot be tested at the
    //! integration level because DTLS 1.3 records are AEAD-encrypted and
    //! close_notify is always the highest-sequence record from a given sender.

    let _ = env_logger::try_init();
    let mut now = Instant::now();
    let (mut client, mut server, now_hs) = setup_connected_13_pair(now);
    now = now_hs;

    // Server sends close_notify
    server.close().unwrap();
    now += Duration::from_millis(10);
    server.handle_timeout(now).expect("server timeout");
    let close_packets = drain_outputs(&mut server).packets;

    // Deliver close_notify to client
    deliver_packets(&close_packets, &mut client);
    client.handle_timeout(now).expect("client timeout");
    let client_out = drain_outputs(&mut client);
    assert!(client_out.close_notify, "Client should see CloseNotify");

    // Now try to send application data from server (after close_notify)
    // This should fail because server is in HalfClosedLocal
    let result = server.send_application_data(b"after-close");
    assert!(
        result.is_err(),
        "send_application_data should fail after close()"
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls13_half_closed_local_no_ack() {
    //! Per RFC 9147 §5.10 / RFC 8446 §6.1, after sending close_notify, no
    //! further messages (including ACKs) should be sent. This test verifies
    //! that in HalfClosedLocal state, the implementation does not send ACKs.

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    // Use low AEAD limit to trigger automatic KeyUpdate
    let config = Arc::new(
        Config::builder()
            .aead_encryption_limit(5)
            .build()
            .expect("build config"),
    );

    let mut now = Instant::now();

    let mut client = Dtls::new_13(Arc::clone(&config), client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_13(config, server_cert, now);
    server.set_active(false);

    now = complete_dtls13_handshake(&mut client, &mut server, now);

    // Client calls close() → HalfClosedLocal
    client.close().unwrap();
    now += Duration::from_millis(10);
    client.handle_timeout(now).expect("client timeout");
    let close_packets = drain_outputs(&mut client).packets;

    // Send 5 messages to trigger needs_key_update (limit=5, threshold 4..=5).
    for i in 0..5 {
        server
            .send_application_data(format!("msg{}", i).as_bytes())
            .expect("send app data");
    }

    // handle_timeout → make_progress → creates KeyUpdate, rotates send keys
    // to a new epoch. The KeyUpdate handshake record is saved for retransmission.
    now += Duration::from_millis(10);
    server.handle_timeout(now).expect("server timeout");

    // Batch 1: 5 app-data records + KeyUpdate (all on old epoch).
    let batch1 = drain_outputs(&mut server).packets;

    // Send one more message on the NEW epoch (post-KeyUpdate).
    // The client must process the KeyUpdate to install recv keys for this epoch;
    // otherwise decryption fails and app_data count will be < 6.
    server
        .send_application_data(b"msg5")
        .expect("send app data on new epoch");
    now += Duration::from_millis(10);
    server.handle_timeout(now).expect("server timeout");

    // Batch 2: 1 app-data record on new epoch.
    let batch2 = drain_outputs(&mut server).packets;

    // Deliver close_notify to server
    deliver_packets(&close_packets, &mut server);
    now += Duration::from_millis(10);
    server.handle_timeout(now).expect("server timeout");
    let _ = drain_outputs(&mut server);

    // Deliver batch 1 (includes KeyUpdate) to client.
    // Client is in HalfClosedLocal — it should process the KeyUpdate
    // (install recv keys for the new epoch) but NOT send ACK.
    deliver_packets(&batch1, &mut client);
    now += Duration::from_millis(10);
    client.handle_timeout(now).expect("client timeout");
    let client_out1 = drain_outputs(&mut client);

    assert!(
        client_out1.packets.is_empty(),
        "Client in HalfClosedLocal should not send ACK for KeyUpdate"
    );

    // Deliver batch 2 (new-epoch app data) to client.
    // This will only succeed if KeyUpdate was actually processed above.
    deliver_packets(&batch2, &mut client);
    now += Duration::from_millis(10);
    client.handle_timeout(now).expect("client timeout");
    let client_out2 = drain_outputs(&mut client);

    let total = client_out1.app_data.len() + client_out2.app_data.len();
    assert_eq!(
        total, 6,
        "Client must receive all 6 messages (6th on new epoch proves KeyUpdate was processed)"
    );
    assert!(
        client_out2.packets.is_empty(),
        "Client in HalfClosedLocal should not send any packets"
    );
}
