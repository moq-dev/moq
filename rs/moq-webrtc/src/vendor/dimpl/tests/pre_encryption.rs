#![cfg(feature = "rcgen")]

use std::sync::Arc;
use std::time::{Duration, Instant};

use dimpl::certificate::generate_self_signed_certificate;
use dimpl::{Config, Dtls, Error, Output, UnexpectedMessageError};

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

fn deliver_packets(
    source: &mut Observed,
    destination: &mut Dtls,
    observed: &mut Observed,
    dtls13: bool,
) {
    for packet in std::mem::take(&mut source.packets) {
        if dtls13 {
            destination.handle_packet(&packet).expect("receive packet");
            drain(destination, observed);
        } else {
            // A peer may send CCS and Finished in separate UDP datagrams. Keep
            // that legal ordering so CCS enables encryption before Finished is
            // queued, rather than both arriving in one coalesced datagram.
            let mut remaining = packet.as_slice();
            while !remaining.is_empty() {
                let length = 13 + u16::from_be_bytes([remaining[11], remaining[12]]) as usize;
                destination
                    .handle_packet(&remaining[..length])
                    .expect("receive record");
                drain(destination, observed);
                remaining = &remaining[length..];
            }
        }
    }
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

fn assert_pre_encryption_record_is_not_delivered(dtls13: bool, epoch: u16) {
    let mut now = Instant::now();
    let config = Arc::new(Config::builder().build().expect("config"));
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

    let packet = pre_encryption_record(dtls13, epoch, 0x1234);
    let mut record_injected = false;
    if dtls13 {
        server.handle_packet(&packet).expect("receive before keys");
        drain(&mut server, &mut server_out);
        assert!(server_out.app_data.is_empty());
        record_injected = true;
    }

    for _ in 0..40 {
        client.handle_timeout(now).expect("client timeout");
        drain(&mut client, &mut client_out);
        server.handle_timeout(now).expect("server timeout");
        drain(&mut server, &mut server_out);
        deliver_packets(&mut client_out, &mut server, &mut server_out, dtls13);
        if !record_injected
            && server_out
                .packets
                .iter()
                .any(|packet| packet.first() == Some(&22) && packet.get(13) == Some(&2))
        {
            // Inject after ServerHello: DTLS 1.2's HelloVerifyRequest resets
            // the receive queue, so input before the cookie retry is discarded.
            server.handle_packet(&packet).expect("receive before CCS");
            drain(&mut server, &mut server_out);
            assert!(server_out.app_data.is_empty());
            record_injected = true;
        }
        deliver_packets(&mut server_out, &mut client, &mut client_out, dtls13);
        if client_out.connected && server_out.connected {
            break;
        }
        now += Duration::from_millis(10);
    }
    assert!(record_injected, "input must reach the pre-encryption queue");
    assert!(
        client_out.connected && server_out.connected,
        "handshake completes"
    );
    assert_eq!(
        server_out.app_data,
        Vec::<Vec<u8>>::new(),
        "pre-encryption epoch {epoch} bytes must be authenticated before delivery"
    );

    client
        .send_application_data(b"authenticated")
        .expect("send");
    drain(&mut client, &mut client_out);
    deliver_packets(&mut client_out, &mut server, &mut server_out, dtls13);
    assert_eq!(server_out.app_data, [b"authenticated".to_vec()]);
}

#[test]
fn dtls12_pre_encryption_unknown_epoch_is_not_application_data() {
    assert_pre_encryption_record_is_not_delivered(false, 2);
}

#[test]
fn dtls13_pre_encryption_epoch_bits_zero_is_not_application_data() {
    assert_pre_encryption_record_is_not_delivered(true, 0);
}

#[test]
fn dtls13_pre_encryption_epoch_bits_one_is_not_application_data() {
    assert_pre_encryption_record_is_not_delivered(true, 1);
}

fn split_records(packets: Vec<Vec<u8>>) -> Vec<Vec<u8>> {
    let mut records = Vec::new();
    for packet in packets {
        let mut remaining = packet.as_slice();
        while !remaining.is_empty() {
            let length = if remaining[0] & 0xe0 == 0x20 {
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

fn assert_handshake_completion_rejects_old_epoch(dtls13: bool, to_server: bool, key_update: bool) {
    let now = Instant::now();
    let config = Arc::new(
        Config::builder()
            .use_server_cookie(false)
            .build()
            .expect("config"),
    );
    let client_cert = generate_self_signed_certificate().expect("client certificate");
    let server_cert = generate_self_signed_certificate().expect("server certificate");
    let (mut client, mut server) = if dtls13 {
        (
            Dtls::new_13(Arc::clone(&config), client_cert, now),
            Dtls::new_13(config, server_cert, now),
        )
    } else {
        (
            Dtls::new_12(Arc::clone(&config), client_cert, now),
            Dtls::new_12(config, server_cert, now),
        )
    };
    client.set_active(true);
    server.set_active(false);
    let mut client_out = Observed::default();
    let mut server_out = Observed::default();
    server.handle_timeout(now).expect("start server");
    drain(&mut server, &mut server_out);
    client.handle_timeout(now).expect("start client");
    drain(&mut client, &mut client_out);
    if !to_server {
        deliver_packets(&mut client_out, &mut server, &mut server_out, dtls13);
    }
    let (source, destination, observed) = if to_server {
        (&mut client_out, &mut server, &mut server_out)
    } else {
        (&mut server_out, &mut client, &mut client_out)
    };
    let mut invalid = if key_update {
        let mut packet = vec![22, 0xfe, 0xfd, 0, 0];
        packet.extend_from_slice(&0xff00u64.to_be_bytes()[2..]);
        packet.extend_from_slice(&13u16.to_be_bytes());
        packet.extend_from_slice(&[24, 0, 0, 1]); // KeyUpdate, body length 1
        packet.extend_from_slice(&0xff00u16.to_be_bytes());
        packet.extend_from_slice(&[0, 0, 0, 0, 0, 1, 0]); // offset, length, request
        packet
    } else {
        let message_type = if to_server { 1 } else { 2 };
        let packet = source
            .packets
            .iter()
            .find(|packet| packet[0] == 22 && packet[13] == message_type)
            .expect("plaintext hello flight");
        let length = 13 + u16::from_be_bytes([packet[11], packet[12]]) as usize;
        packet[..length].to_vec()
    };
    // Buffer a future message. A high record sequence lets DTLS 1.2's CCS
    // sort ahead of it, so the genuine handshake can reach completion.
    invalid[5..11].copy_from_slice(&0xff00u64.to_be_bytes()[2..]);
    invalid[17..19].copy_from_slice(&0xff00u16.to_be_bytes());
    let result = destination.handle_packet(&invalid);
    drain(destination, observed);
    result.expect("buffer future plaintext while the handshake is in progress");

    // Deliver records separately so enabling encryption does not reparse and
    // discard the buffered plaintext alongside a coalesced protected flight.
    for _ in 0..10 {
        for to_server_now in [true, false] {
            let (source, destination, observed) = if to_server_now {
                (&mut client_out, &mut server, &mut server_out)
            } else {
                (&mut server_out, &mut client, &mut client_out)
            };
            for packet in split_records(std::mem::take(&mut source.packets)) {
                let result = destination.handle_packet(&packet);
                drain(destination, observed);
                if result.is_err() {
                    assert_eq!(to_server_now, to_server);
                    assert_eq!(
                        result,
                        Err(Error::UnexpectedMessage(
                            UnexpectedMessageError::UnexpectedEpoch
                        ))
                    );
                    assert!(!observed.connected, "must reject before emitting Connected");
                    assert!(
                        if dtls13 {
                            packet[0] & 0xe0 == 0x20
                        } else {
                            packet[3..5] == [0, 1]
                        },
                        "error must occur at completion, after receiving protected Finished"
                    );
                    return;
                }
            }
        }
    }
    panic!("old-epoch record survived the transition to application data");
}

#[test]
fn dtls12_client_errors_on_old_epoch_at_handshake_completion() {
    assert_handshake_completion_rejects_old_epoch(false, false, false);
}

#[test]
fn dtls12_server_errors_on_old_epoch_at_handshake_completion() {
    assert_handshake_completion_rejects_old_epoch(false, true, false);
}

#[test]
fn dtls13_client_errors_on_old_epoch_at_handshake_completion() {
    assert_handshake_completion_rejects_old_epoch(true, false, false);
}

#[test]
fn dtls13_server_errors_on_old_epoch_at_handshake_completion() {
    assert_handshake_completion_rejects_old_epoch(true, true, false);
}

#[test]
fn dtls13_client_errors_on_plaintext_key_update_at_handshake_completion() {
    assert_handshake_completion_rejects_old_epoch(true, false, true);
}

#[test]
fn dtls13_server_errors_on_plaintext_key_update_at_handshake_completion() {
    assert_handshake_completion_rejects_old_epoch(true, true, true);
}
