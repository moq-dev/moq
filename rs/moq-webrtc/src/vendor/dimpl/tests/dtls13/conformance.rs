//! DTLS 1.3 RFC 9147 conformance regression tests.

use std::sync::Arc;
use std::time::{Duration, Instant};

#[cfg(feature = "rcgen")]
use dimpl::certificate::generate_self_signed_certificate;
use dimpl::{Config, Dtls};

use crate::common::*;

const RECORD_HEADER_LEN: usize = 13;
const HANDSHAKE_HEADER_LEN: usize = 12;
const HANDSHAKE_OFFSET: usize = RECORD_HEADER_LEN;
const BODY_OFFSET: usize = RECORD_HEADER_LEN + HANDSHAKE_HEADER_LEN;

fn set_u24(buf: &mut [u8], offset: usize, value: usize) {
    let value = value as u32;
    buf[offset] = (value >> 16) as u8;
    buf[offset + 1] = (value >> 8) as u8;
    buf[offset + 2] = value as u8;
}

fn grow_plaintext_handshake(packet: &mut [u8], added: usize) {
    let record_len = u16::from_be_bytes([packet[11], packet[12]]) as usize + added;
    packet[11..13].copy_from_slice(&(record_len as u16).to_be_bytes());

    let body_len =
        ((packet[14] as usize) << 16) | ((packet[15] as usize) << 8) | packet[16] as usize;
    set_u24(packet, 14, body_len + added);

    let fragment_len =
        ((packet[22] as usize) << 16) | ((packet[23] as usize) << 8) | packet[24] as usize;
    set_u24(packet, 22, fragment_len + added);
}

fn insert_legacy_session_id(packet: &[u8], session_id: &[u8]) -> Vec<u8> {
    assert_eq!(packet[0], 22, "expected plaintext handshake record");
    assert_eq!(packet[HANDSHAKE_OFFSET], 1, "expected ClientHello");

    let sid_len_offset = BODY_OFFSET + 2 + 32;
    assert_eq!(
        packet[sid_len_offset], 0,
        "dimpl ClientHello starts with empty session id"
    );

    let mut out = packet.to_vec();
    out[sid_len_offset] = session_id.len() as u8;
    out.splice(
        sid_len_offset + 1..sid_len_offset + 1,
        session_id.iter().copied(),
    );
    grow_plaintext_handshake(&mut out, session_id.len());
    out
}

fn insert_legacy_cookie(packet: &[u8], cookie: &[u8]) -> Vec<u8> {
    assert_eq!(packet[0], 22, "expected plaintext handshake record");
    assert_eq!(packet[HANDSHAKE_OFFSET], 1, "expected ClientHello");

    let sid_len_offset = BODY_OFFSET + 2 + 32;
    let cookie_len_offset = sid_len_offset + 1 + packet[sid_len_offset] as usize;
    assert_eq!(
        packet[cookie_len_offset], 0,
        "dimpl ClientHello starts with empty legacy cookie"
    );

    let mut out = packet.to_vec();
    out[cookie_len_offset] = cookie.len() as u8;
    out.splice(
        cookie_len_offset + 1..cookie_len_offset + 1,
        cookie.iter().copied(),
    );
    grow_plaintext_handshake(&mut out, cookie.len());
    out
}

fn plaintext_server_hello_legacy_session_id_len(packet: &[u8]) -> u8 {
    assert_eq!(packet[0], 22, "expected plaintext handshake record");
    assert_eq!(packet[HANDSHAKE_OFFSET], 2, "expected ServerHello/HRR");
    packet[BODY_OFFSET + 2 + 32]
}

fn ciphertext_record_count(packets: &[Vec<u8>]) -> usize {
    let mut count = 0;
    for packet in packets {
        let mut input = packet.as_slice();
        while !input.is_empty() {
            assert!(
                input[0] & 0b1110_0000 == 0b0010_0000,
                "expected ciphertext record, got first byte {:#04x}",
                input[0]
            );
            assert!(
                input[0] & 0b0000_1000 != 0,
                "test expects 2-byte sequence numbers"
            );
            assert!(
                input[0] & 0b0000_0100 != 0,
                "test expects explicit ciphertext length"
            );
            let len = u16::from_be_bytes([input[3], input[4]]) as usize;
            count += 1;
            input = &input[5 + len..];
        }
    }
    count
}

fn has_ciphertext_record_len(packets: &[Vec<u8>], wanted: usize) -> bool {
    for packet in packets {
        let mut input = packet.as_slice();
        while !input.is_empty() {
            let len = u16::from_be_bytes([input[3], input[4]]) as usize;
            if len == wanted {
                return true;
            }
            input = &input[5 + len..];
        }
    }
    false
}

#[cfg(feature = "rcgen")]
fn connected_pair(config: Arc<Config>) -> (Dtls, Dtls, Instant) {
    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

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
        client_connected |= client_out.connected;
        server_connected |= server_out.connected;

        deliver_packets(&client_out.packets, &mut server);
        deliver_packets(&server_out.packets, &mut client);

        if client_connected && server_connected {
            return (client, server, now);
        }
        now += Duration::from_millis(10);
    }

    panic!("DTLS 1.3 handshake did not complete");
}

fn quiesce_pair(client: &mut Dtls, server: &mut Dtls, now: Instant) {
    for _ in 0..8 {
        client.handle_timeout(now).expect("client timeout");
        server.handle_timeout(now).expect("server timeout");

        let client_out = drain_outputs(client);
        let server_out = drain_outputs(server);
        let quiet = client_out.packets.is_empty() && server_out.packets.is_empty();

        deliver_packets(&client_out.packets, server);
        deliver_packets(&server_out.packets, client);

        if quiet {
            break;
        }
    }
}

#[test]
#[cfg(feature = "rcgen")]
fn server_does_not_echo_client_legacy_session_id() {
    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    let config = Arc::new(
        Config::builder()
            .use_server_cookie(false)
            .build()
            .expect("build config"),
    );
    let now = Instant::now();

    let mut client = Dtls::new_13(Arc::clone(&config), client_cert, now);
    client.set_active(true);
    let mut server = Dtls::new_13(config, server_cert, now);
    server.set_active(false);
    server.handle_timeout(now).expect("server timeout");

    client.handle_timeout(now).expect("client timeout");
    let client_hello = drain_outputs(&mut client)
        .packets
        .into_iter()
        .next()
        .expect("client should emit ClientHello");
    let client_hello = insert_legacy_session_id(&client_hello, b"non-empty-session");

    server
        .handle_packet(&client_hello)
        .expect("server should parse ClientHello");
    server.handle_timeout(now).expect("server timeout");
    let server_hello = drain_outputs(&mut server)
        .packets
        .into_iter()
        .next()
        .expect("server should emit ServerHello");

    assert_eq!(
        plaintext_server_hello_legacy_session_id_len(&server_hello),
        0,
        "DTLS 1.3 servers MUST NOT echo legacy_session_id"
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn server_rejects_non_empty_client_legacy_cookie() {
    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    let config = Arc::new(
        Config::builder()
            .use_server_cookie(false)
            .build()
            .expect("build config"),
    );
    let now = Instant::now();

    let mut client = Dtls::new_13(Arc::clone(&config), client_cert, now);
    client.set_active(true);
    let mut server = Dtls::new_13(config, server_cert, now);
    server.set_active(false);
    server.handle_timeout(now).expect("server timeout");

    client.handle_timeout(now).expect("client timeout");
    let client_hello = drain_outputs(&mut client)
        .packets
        .into_iter()
        .next()
        .expect("client should emit ClientHello");
    let client_hello = insert_legacy_cookie(&client_hello, b"legacy-cookie");

    assert!(
        server.handle_packet(&client_hello).is_err(),
        "server must abort ClientHello messages with non-empty legacy_cookie"
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn client_aborts_on_distinct_second_hello_retry_request() {
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
    let client_hello = drain_outputs(&mut client).packets;
    deliver_packets(&client_hello, &mut server);

    server.handle_timeout(now).expect("server timeout");
    let hrr = drain_outputs(&mut server)
        .packets
        .into_iter()
        .next()
        .expect("server should emit HRR");

    client
        .handle_packet(&hrr)
        .expect("client accepts first HRR");
    client.handle_timeout(now).expect("client sends CH2");
    let _ = drain_outputs(&mut client);
    let mut second_hrr = hrr.clone();
    second_hrr[17..19].copy_from_slice(&1u16.to_be_bytes());

    assert!(
        client.handle_packet(&second_hrr).is_err(),
        "client must abort on a second HRR"
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn application_data_waits_for_key_update_ack() {
    let _ = env_logger::try_init();

    let config = Arc::new(
        Config::builder()
            .aead_encryption_limit(3)
            .build()
            .expect("build config"),
    );
    let (mut client, mut server, mut now) = connected_pair(config);
    quiesce_pair(&mut client, &mut server, now);

    let mut key_update_out = None;
    for i in 0..10 {
        let msg = format!("key-update-prime-application-payload-{i}").into_bytes();
        client.send_application_data(&msg).expect("queue app data");
        let out = drain_outputs(&mut client).packets;
        assert_eq!(ciphertext_record_count(&out), 1);
        deliver_packets(&out, &mut server);
        server.handle_timeout(now).expect("server timeout");
        let server_out = drain_outputs(&mut server);
        assert_eq!(server_out.app_data, vec![msg]);
        now += Duration::from_millis(10);

        client.handle_timeout(now).expect("client timeout");
        let out = drain_outputs(&mut client).packets;
        if has_ciphertext_record_len(&out, 30) {
            key_update_out = Some(out);
            break;
        }
    }

    let out = key_update_out.expect("client should initiate KeyUpdate");
    assert!(!out.is_empty(), "client should send a KeyUpdate");
    let old_epoch_msg = b"sent-on-old-epoch-before-key-update-ack".to_vec();
    client
        .send_application_data(&old_epoch_msg)
        .expect("send app data while KeyUpdate is in flight");
    let old_epoch_data = drain_outputs(&mut client).packets;
    assert!(
        !old_epoch_data.is_empty(),
        "application data should continue on the old epoch while KeyUpdate is in flight"
    );
    deliver_packets(&old_epoch_data, &mut server);
    server
        .handle_timeout(now)
        .expect("server receives old-epoch data");
    let server_out = drain_outputs(&mut server);
    assert_eq!(server_out.app_data, vec![old_epoch_msg]);

    deliver_packets(&out, &mut server);
    server
        .handle_timeout(now)
        .expect("server processes KeyUpdate");
    let server_out = drain_outputs(&mut server);
    assert!(
        server_out.app_data.is_empty(),
        "processing the KeyUpdate should not produce duplicate application data"
    );

    deliver_packets(&server_out.packets, &mut client);
    now += Duration::from_millis(10);
    client
        .handle_timeout(now)
        .expect("client handles KeyUpdate ACK");
    let _ = drain_outputs(&mut client);
}

#[test]
#[cfg(feature = "rcgen")]
fn update_requested_is_ignored_while_key_update_in_flight() {
    let _ = env_logger::try_init();

    let config = Arc::new(
        Config::builder()
            .aead_encryption_limit(3)
            .build()
            .expect("build config"),
    );
    let (mut client, mut server, mut now) = connected_pair(config);
    quiesce_pair(&mut client, &mut server, now);

    let mut client_key_update = None;
    for i in 0..10 {
        let msg = format!("client-prime-application-payload-{i}").into_bytes();
        client.send_application_data(&msg).expect("client send");
        let out = drain_outputs(&mut client).packets;
        deliver_packets(&out, &mut server);
        server.handle_timeout(now).expect("server timeout");
        let _ = drain_outputs(&mut server);
        now += Duration::from_millis(10);

        client.handle_timeout(now).expect("client timeout");
        let out = drain_outputs(&mut client).packets;
        if has_ciphertext_record_len(&out, 30) {
            client_key_update = Some(out);
            break;
        }
    }

    let mut server_key_update = None;
    for i in 0..10 {
        let msg = format!("server-prime-application-payload-{i}").into_bytes();
        server.send_application_data(&msg).expect("server send");
        let out = drain_outputs(&mut server).packets;
        deliver_packets(&out, &mut client);
        let _ = drain_outputs(&mut client);
        now += Duration::from_millis(10);

        server.handle_timeout(now).expect("server timeout");
        let out = drain_outputs(&mut server).packets;
        if has_ciphertext_record_len(&out, 30) {
            server_key_update = Some(out);
            break;
        }
    }

    let client_key_update = client_key_update.expect("client should initiate KeyUpdate");
    assert!(
        !client_key_update.is_empty(),
        "client should send its own KeyUpdate"
    );

    let server_key_update = server_key_update.expect("server should initiate KeyUpdate");
    assert!(
        !server_key_update.is_empty(),
        "server should send its own KeyUpdate"
    );
    deliver_packets(&server_key_update, &mut client);

    client
        .handle_timeout(now)
        .expect("client processes peer KeyUpdate");
    let ack_only = drain_outputs(&mut client).packets;
    assert_eq!(
        ciphertext_record_count(&ack_only),
        1,
        "client should ACK peer KeyUpdate"
    );

    now += Duration::from_millis(10);
    client.handle_timeout(now).expect("client timeout");
    let response = drain_outputs(&mut client).packets;

    assert!(
        response.is_empty(),
        "client must not send a second KeyUpdate while the first is unacknowledged"
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn server_retransmits_final_ack_for_retransmitted_client_final_flight() {
    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");
    let config = Arc::new(
        Config::builder()
            .flight_retries(8)
            .build()
            .expect("build config"),
    );
    let mut now = Instant::now();

    let mut client = Dtls::new_13(Arc::clone(&config), client_cert, now);
    client.set_active(true);
    let mut server = Dtls::new_13(config, server_cert, now);
    server.set_active(false);

    let mut client_final_flight_seen = false;
    let mut ack_dropped = false;
    for round in 0..100 {
        client.handle_timeout(now).expect("client timeout");
        server.handle_timeout(now).expect("server timeout");

        let client_out = drain_outputs(&mut client);
        let server_out = drain_outputs(&mut server);

        deliver_packets(&client_out.packets, &mut server);
        if client_final_flight_seen && !ack_dropped && !server_out.packets.is_empty() {
            ack_dropped = true;
        } else {
            deliver_packets(&server_out.packets, &mut client);
        }

        if !client_final_flight_seen && !client_out.packets.is_empty() && round > 2 {
            client_final_flight_seen = true;
        }

        if ack_dropped {
            break;
        }

        now += if round % 5 == 4 {
            Duration::from_secs(2)
        } else {
            Duration::from_millis(10)
        };
    }

    assert!(ack_dropped, "test should drop the server completion ACK");

    let mut retransmitted_final_flight = Vec::new();
    for _ in 0..10 {
        now += Duration::from_secs(2);
        client
            .handle_timeout(now)
            .expect("client retransmit timeout");
        retransmitted_final_flight = drain_outputs(&mut client).packets;
        if !retransmitted_final_flight.is_empty() {
            break;
        }
    }
    assert!(
        !retransmitted_final_flight.is_empty(),
        "client should retransmit its final flight"
    );

    deliver_packets(&retransmitted_final_flight, &mut server);
    server.handle_timeout(now).expect("server timeout");
    let server_response = drain_outputs(&mut server).packets;

    assert!(
        !server_response.is_empty(),
        "server must retransmit its final ACK when the client final flight is retransmitted"
    );
}
