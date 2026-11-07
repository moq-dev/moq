#![cfg(feature = "rcgen")]

use std::sync::Arc;
use std::time::{Duration, Instant};

use dimpl::certificate::generate_self_signed_certificate;
use dimpl::{Config, Dtls, Output};

const UNAUTHENTICATED_INPUT: &[u8] = b"unauthenticated input";

#[derive(Default)]
struct Observed {
    packets: Vec<Vec<u8>>,
    connected: bool,
    app_data: Vec<Vec<u8>>,
}

fn drain(endpoint: &mut Dtls, observed: &mut Observed) {
    let mut buffer = [0; 2048];
    loop {
        match endpoint.poll_output(&mut buffer) {
            Output::Packet(packet) => observed.packets.push(packet.to_vec()),
            Output::Connected => observed.connected = true,
            Output::ApplicationData(data) => observed.app_data.push(data.to_vec()),
            Output::Timeout(_) => return,
            _ => {}
        }
    }
}

fn deliver_packets(source: &mut Observed, destination: &mut Dtls, observed: &mut Observed) {
    for packet in std::mem::take(&mut source.packets) {
        destination.handle_packet(&packet).expect("receive packet");
        drain(destination, observed);
    }
}

fn retry_handshake(
    mut now: Instant,
    client: &mut Dtls,
    server: &mut Dtls,
    client_out: &mut Observed,
    server_out: &mut Observed,
) {
    for _ in 0..10 {
        now += Duration::from_secs(1);
        client.handle_timeout(now).expect("client retry");
        drain(client, client_out);
        server.handle_timeout(now).expect("server retry");
        drain(server, server_out);
        deliver_packets(client_out, server, server_out);
        deliver_packets(server_out, client, client_out);
        if client_out.connected && server_out.connected {
            return;
        }
    }
    panic!("handshake must recover through retransmission");
}

fn split_records(packets: &[Vec<u8>], dtls13: bool) -> Vec<Vec<u8>> {
    let mut records = Vec::new();
    for packet in packets {
        let mut remaining = packet.as_slice();
        while !remaining.is_empty() {
            let length = if dtls13 && remaining[0] & 0xe0 == 0x20 {
                assert_eq!(remaining[0] & 0x1c, 0x0c, "two-byte sequence and length");
                5 + u16::from_be_bytes([remaining[3], remaining[4]]) as usize
            } else {
                13 + u16::from_be_bytes([remaining[11], remaining[12]]) as usize
            };
            records.push(remaining[..length].to_vec());
            remaining = &remaining[length..];
        }
    }
    records
}

fn pre_encryption_record(dtls13: bool, epoch: u16, sequence: u16) -> Vec<u8> {
    let mut packet = Vec::new();
    if dtls13 {
        packet.push(0x2c | (epoch as u8 & 3)); // unified header, S=1, L=1
        packet.extend_from_slice(&sequence.to_be_bytes());
    } else {
        packet.push(23); // ApplicationData
        packet.extend_from_slice(&[0xfe, 0xfd]);
        packet.extend_from_slice(&epoch.to_be_bytes());
        packet.extend_from_slice(&(sequence as u64).to_be_bytes()[2..]);
    }
    packet.extend_from_slice(&(UNAUTHENTICATED_INPUT.len() as u16).to_be_bytes());
    packet.extend_from_slice(UNAUTHENTICATED_INPUT);
    packet
}

fn assert_pre_encryption_records_need_no_queue_slot(dtls13: bool, capacity: usize) {
    let now = Instant::now();
    let config = Arc::new(
        Config::builder()
            .max_queue_rx(capacity)
            .build()
            .expect("config"),
    );
    let certificate = generate_self_signed_certificate().expect("certificate");
    let mut server = if dtls13 {
        Dtls::new_13(config, certificate, now)
    } else {
        Dtls::new_12(config, certificate, now)
    };
    server.set_active(false);
    let mut observed = Observed::default();
    let epoch = if dtls13 { 2 } else { 1 };
    let packet = pre_encryption_record(dtls13, epoch, 0x1234);
    server
        .handle_packet(&packet)
        .expect("discard premature record");
    drain(&mut server, &mut observed);

    // Both duplicate and distinct premature records are dropped. Neither can
    // consume a queue slot or cause the duplicate-epoch-1 panic.
    for sequence in [0x1234, 0x1235, 0x1234] {
        server
            .handle_packet(&pre_encryption_record(dtls13, epoch, sequence))
            .expect("discard premature record without filling the queue");
        drain(&mut server, &mut observed);
    }
    assert!(observed.packets.is_empty());
    assert!(observed.app_data.is_empty());
}

#[test]
fn dtls12_pre_encryption_duplicate_at_queue_capacity_is_dropped() {
    assert_pre_encryption_records_need_no_queue_slot(false, 1);
}

#[test]
fn dtls12_premature_duplicate_does_not_panic() {
    assert_pre_encryption_records_need_no_queue_slot(false, 8);
}

#[test]
fn dtls13_pre_encryption_duplicate_at_queue_capacity_is_dropped() {
    assert_pre_encryption_records_need_no_queue_slot(true, 1);
}

#[test]
fn dtls13_premature_corrupted_copy_does_not_block_retransmission() {
    let now = Instant::now();
    let config = Arc::new(
        Config::builder()
            .mtu(200)
            .use_server_cookie(false)
            .build()
            .expect("config"),
    );
    let client_cert = generate_self_signed_certificate().expect("client certificate");
    let server_cert = generate_self_signed_certificate().expect("server certificate");
    let mut client = Dtls::new_13(Arc::clone(&config), client_cert, now);
    client.set_active(true);
    let mut server = Dtls::new_13(config, server_cert, now);
    server.set_active(false);
    let mut client_out = Observed::default();
    let mut server_out = Observed::default();
    server.handle_timeout(now).expect("server timeout");
    drain(&mut server, &mut server_out);
    client.handle_timeout(now).expect("client timeout");
    drain(&mut client, &mut client_out);
    deliver_packets(&mut client_out, &mut server, &mut server_out);

    let (ciphertext, plaintext): (Vec<_>, Vec<_>) =
        split_records(&std::mem::take(&mut server_out.packets), true)
            .into_iter()
            .partition(|record| record[0] & 0xe0 == 0x20);
    assert!(!plaintext.is_empty(), "server emits ServerHello");
    assert!(!ciphertext.is_empty(), "server emits encrypted handshake");

    // A damaged copy arrives before the ServerHello needed to derive keys.
    // Clean ciphertext also arrives before ServerHello and must be retransmitted.
    let mut corrupted = ciphertext[0].clone();
    *corrupted.last_mut().expect("ciphertext has tag") ^= 0x55;
    client
        .handle_packet(&corrupted)
        .expect("discard premature damaged copy");
    drain(&mut client, &mut client_out);
    for record in ciphertext.iter().chain(&plaintext) {
        client.handle_packet(record).expect("receive clean flight");
        drain(&mut client, &mut client_out);
    }
    assert!(!client_out.connected, "early ciphertext was discarded");
    retry_handshake(
        now,
        &mut client,
        &mut server,
        &mut client_out,
        &mut server_out,
    );
}

#[test]
fn dtls12_premature_short_copy_does_not_block_retransmission() {
    let now = Instant::now();
    let config = Arc::new(
        Config::builder()
            .mtu(200)
            .use_server_cookie(false)
            .build()
            .expect("config"),
    );
    let client_cert = generate_self_signed_certificate().expect("client certificate");
    let server_cert = generate_self_signed_certificate().expect("server certificate");
    let mut client = Dtls::new_12(Arc::clone(&config), client_cert, now);
    client.set_active(true);
    let mut server = Dtls::new_12(config, server_cert, now);
    server.set_active(false);
    let mut client_out = Observed::default();
    let mut server_out = Observed::default();
    server.handle_timeout(now).expect("server timeout");
    drain(&mut server, &mut server_out);
    client.handle_timeout(now).expect("client timeout");
    drain(&mut client, &mut client_out);
    deliver_packets(&mut client_out, &mut server, &mut server_out);
    deliver_packets(&mut server_out, &mut client, &mut client_out);

    let flight = split_records(&std::mem::take(&mut client_out.packets), false);
    let finished = flight
        .iter()
        .find(|record| record[0] == 22 && record[3..5] == [0, 1])
        .expect("client emits epoch-1 Finished");
    // A too-short protected record is discarded without a fatal authentication
    // error. It cannot establish the identity of the genuine Finished before CCS.
    let mut short_copy = finished[..13].to_vec();
    short_copy[0] = 23;
    short_copy[11..13].copy_from_slice(&2u16.to_be_bytes());
    short_copy.extend_from_slice(&[0xaa, 0xbb]);
    server
        .handle_packet(&short_copy)
        .expect("discard premature short copy");
    drain(&mut server, &mut server_out);
    server
        .handle_packet(finished)
        .expect("discard premature clean Finished");
    drain(&mut server, &mut server_out);
    // CCS enables decryption, but the prematurely received Finished was dropped.
    for record in flight.iter().filter(|record| record[3..5] == [0, 0]) {
        server.handle_packet(record).expect("receive clean flight");
        drain(&mut server, &mut server_out);
    }
    assert!(!server_out.connected, "early Finished was discarded");
    retry_handshake(
        now,
        &mut client,
        &mut server,
        &mut client_out,
        &mut server_out,
    );
}

#[test]
fn dtls12_pre_encryption_handshake_ciphertext_cannot_advance_the_handshake() {
    let now = Instant::now();
    let config = Arc::new(
        Config::builder()
            .mtu(200)
            .use_server_cookie(false)
            .build()
            .expect("config"),
    );
    let client_cert = generate_self_signed_certificate().expect("client certificate");
    let server_cert = generate_self_signed_certificate().expect("server certificate");
    let mut client = Dtls::new_12(Arc::clone(&config), client_cert, now);
    client.set_active(true);
    let mut server = Dtls::new_12(config, server_cert, now);
    server.set_active(false);
    let mut client_out = Observed::default();
    let mut server_out = Observed::default();
    server.handle_timeout(now).expect("server timeout");
    drain(&mut server, &mut server_out);
    client.handle_timeout(now).expect("client timeout");
    drain(&mut client, &mut client_out);
    deliver_packets(&mut client_out, &mut server, &mut server_out);

    let flight = split_records(&std::mem::take(&mut server_out.packets), false);
    let hello_done = flight
        .iter()
        .find(|record| record[0] == 22 && record[13] == 14)
        .expect("server emits ServerHelloDone");
    let prefix = flight
        .iter()
        .rev()
        .find(|record| *record != hello_done)
        .expect("server emits a preceding plaintext record");
    for record in flight
        .iter()
        .filter(|record| *record != hello_done && *record != prefix)
    {
        client.handle_packet(record).expect("receive server flight");
        drain(&mut client, &mut client_out);
    }
    assert!(client_out.packets.is_empty(), "await ServerHelloDone");

    // An epoch-1 record is ciphertext even when its opaque payload happens to
    // have the shape of a plaintext handshake. This record is too short to
    // authenticate and must never substitute for the missing epoch-0 message.
    let mut opaque = hello_done.clone();
    opaque[3..5].copy_from_slice(&1u16.to_be_bytes());
    client
        .handle_packet(&opaque)
        .expect("discard premature future epoch");
    drain(&mut client, &mut client_out);
    assert!(
        client_out.packets.is_empty(),
        "unauthenticated epoch-1 bytes must not trigger the client's next flight"
    );

    // The same forged payload must remain opaque beside a valid plaintext
    // record, where the coalesced-flight path temporarily stages ciphertext.
    client
        .handle_packet(&[prefix.as_slice(), opaque.as_slice()].concat())
        .expect("receive plaintext followed by forged protected handshake");
    drain(&mut client, &mut client_out);
    assert!(
        client_out.packets.is_empty(),
        "await authenticated handshake"
    );

    client
        .handle_packet(hello_done)
        .expect("receive genuine ServerHelloDone");
    drain(&mut client, &mut client_out);
    assert!(
        !client_out.packets.is_empty(),
        "genuine handshake progresses"
    );
    deliver_packets(&mut client_out, &mut server, &mut server_out);
    deliver_packets(&mut server_out, &mut client, &mut client_out);
    assert!(client_out.connected && server_out.connected);
}

#[test]
fn dtls12_pre_encryption_ciphertext_cannot_substitute_for_plaintext_ccs() {
    let now = Instant::now();
    let config = Arc::new(
        Config::builder()
            .use_server_cookie(false)
            .build()
            .expect("config"),
    );
    let client_cert = generate_self_signed_certificate().expect("client certificate");
    let server_cert = generate_self_signed_certificate().expect("server certificate");
    let mut client = Dtls::new_12(Arc::clone(&config), client_cert, now);
    client.set_active(true);
    let mut server = Dtls::new_12(config, server_cert, now);
    server.set_active(false);
    let mut client_out = Observed::default();
    let mut server_out = Observed::default();
    server.handle_timeout(now).expect("server timeout");
    drain(&mut server, &mut server_out);
    client.handle_timeout(now).expect("client timeout");
    drain(&mut client, &mut client_out);
    deliver_packets(&mut client_out, &mut server, &mut server_out);
    deliver_packets(&mut server_out, &mut client, &mut client_out);
    deliver_packets(&mut client_out, &mut server, &mut server_out);
    let flight = split_records(&std::mem::take(&mut server_out.packets), false);
    let ccs = flight
        .iter()
        .find(|record| record[0] == 20)
        .expect("server emits CCS");
    let finished = flight
        .iter()
        .find(|record| record[0] == 22 && record[3..5] == [0, 1])
        .expect("server emits protected Finished");

    // This is an undecryptable future-epoch control record, not a legitimate
    // epoch-0 CCS. It must not make buffered Finished eligible for processing.
    let mut opaque = ccs.clone();
    opaque[3..5].copy_from_slice(&1u16.to_be_bytes());
    client
        .handle_packet(&opaque)
        .expect("discard premature future CCS");
    drain(&mut client, &mut client_out);
    client
        .handle_packet(finished)
        .expect("discard premature Finished");
    drain(&mut client, &mut client_out);
    assert!(
        !client_out.connected,
        "unauthenticated epoch-1 CCS must not enable peer encryption"
    );

    client.handle_packet(ccs).expect("receive genuine CCS");
    drain(&mut client, &mut client_out);
    assert!(
        !client_out.connected,
        "CCS cannot release a discarded Finished"
    );
    client
        .handle_packet(finished)
        .expect("receive retransmitted Finished");
    drain(&mut client, &mut client_out);
    assert!(
        client_out.connected,
        "authenticated Finished completes the handshake"
    );
}

fn assert_old_plaintext_hello_preserves_authenticated_data(dtls13: bool, plaintext_first: bool) {
    let now = Instant::now();
    let config = Arc::new(
        Config::builder()
            .max_queue_rx(8)
            .use_server_cookie(false)
            .build()
            .expect("config"),
    );
    let client_cert = generate_self_signed_certificate().expect("client certificate");
    let server_cert = generate_self_signed_certificate().expect("server certificate");
    let mut client = if dtls13 {
        Dtls::new_13(Arc::clone(&config), client_cert, now)
    } else {
        Dtls::new_12(Arc::clone(&config), client_cert, now)
    };
    client.set_active(true);
    let mut server = if dtls13 {
        Dtls::new_13(config, server_cert, now)
    } else {
        Dtls::new_12(config, server_cert, now)
    };
    server.set_active(false);
    let mut client_out = Observed::default();
    let mut server_out = Observed::default();
    server.handle_timeout(now).expect("server timeout");
    drain(&mut server, &mut server_out);
    client.handle_timeout(now).expect("client timeout");
    drain(&mut client, &mut client_out);
    let client_hello = split_records(&client_out.packets, dtls13)
        .into_iter()
        .find(|record| record[0] == 22 && record[13] == 1)
        .expect("client emits plaintext ClientHello");
    for _ in 0..4 {
        deliver_packets(&mut client_out, &mut server, &mut server_out);
        deliver_packets(&mut server_out, &mut client, &mut client_out);
    }
    assert!(client_out.connected && server_out.connected);

    client
        .send_application_data(b"authenticated beside an old hello")
        .expect("send application data");
    drain(&mut client, &mut client_out);
    assert_eq!(client_out.packets.len(), 1, "one protected data record");
    let protected = client_out.packets.pop().expect("protected data packet");
    let mixed = if plaintext_first {
        [client_hello, protected.clone()].concat()
    } else {
        [protected.clone(), client_hello].concat()
    };
    server
        .handle_packet(&mixed)
        .expect("receive mixed datagram");
    drain(&mut server, &mut server_out);

    // If the old hello discards its entire datagram after the protected record
    // updates the replay window, even a clean copy cannot recover the data.
    server
        .handle_packet(&protected)
        .expect("receive clean protected copy");
    drain(&mut server, &mut server_out);
    assert_eq!(
        server_out.app_data,
        [b"authenticated beside an old hello".to_vec()],
        "discarding a stale plaintext hello must preserve authenticated records beside it"
    );

    // A trailing stale hello must not retain its already consumed datagram and
    // pin every subsequent handled record until the bounded queue is full.
    for sequence in 0..16u8 {
        client
            .send_application_data(&[sequence])
            .expect("send subsequent authenticated data");
        drain(&mut client, &mut client_out);
        for packet in std::mem::take(&mut client_out.packets) {
            let result = server.handle_packet(&packet);
            drain(&mut server, &mut server_out);
            assert_eq!(
                result,
                Ok(()),
                "a stale hello must not fill the queue at data record {sequence}"
            );
        }
    }
    assert_eq!(server_out.app_data.len(), 17);
}

#[test]
fn dtls12_old_plaintext_hello_does_not_discard_authenticated_data() {
    assert_old_plaintext_hello_preserves_authenticated_data(false, true);
}

#[test]
fn dtls13_old_plaintext_hello_does_not_discard_authenticated_data() {
    assert_old_plaintext_hello_preserves_authenticated_data(true, true);
}

#[test]
fn dtls12_trailing_old_plaintext_hello_does_not_pin_receive_queue() {
    assert_old_plaintext_hello_preserves_authenticated_data(false, false);
}

#[test]
fn dtls13_trailing_old_plaintext_hello_does_not_pin_receive_queue() {
    assert_old_plaintext_hello_preserves_authenticated_data(true, false);
}
