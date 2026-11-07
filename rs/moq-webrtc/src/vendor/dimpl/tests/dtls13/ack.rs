//! ACKs must stop retransmission even when they name an earlier transmission.

#![cfg(feature = "rcgen")]

use std::sync::Arc;
use std::time::{Duration, Instant};

use dimpl::certificate::generate_self_signed_certificate;
use dimpl::{Config, Dtls, ProtocolVersion};

use crate::common::drain_outputs;

fn handshake_with_held_final_ack(auto: bool) -> (Dtls, Dtls, Instant, Vec<Vec<u8>>) {
    let now = Instant::now();
    // Keep the separate overall-handshake deadline beyond the observation window.
    let config = Arc::new(
        Config::builder()
            .handshake_timeout(Duration::from_secs(300))
            .dangerously_set_rng_seed(172)
            .build()
            .expect("build config"),
    );
    let new = if auto { Dtls::new_auto } else { Dtls::new_13 };
    let mut client = new(
        config.clone(),
        generate_self_signed_certificate().expect("client certificate"),
        now,
    );
    let mut server = new(
        config,
        generate_self_signed_certificate().expect("server certificate"),
        now,
    );
    client.set_active(true);
    server.handle_timeout(now).expect("start server");
    assert!(drain_outputs(&mut server).packets.is_empty());
    client.handle_timeout(now).expect("start client");
    let mut to_server = drain_outputs(&mut client).packets;
    let mut client_connected = false;

    for _ in 0..10 {
        let mut to_client = Vec::new();
        for packet in std::mem::take(&mut to_server) {
            server.handle_packet(&packet).expect("server packet");
            let output = drain_outputs(&mut server);
            if output.connected {
                assert!(client_connected, "client must already be connected");
                assert_eq!(client.protocol_version(), Some(ProtocolVersion::DTLS1_3));
                assert_eq!(server.protocol_version(), Some(ProtocolVersion::DTLS1_3));
                assert!(!output.packets.is_empty(), "server must send its final ACK");
                client.handle_timeout(now).expect("arm final-flight timer");
                assert!(drain_outputs(&mut client).packets.is_empty());
                return (client, server, now, output.packets);
            }
            to_client.extend(output.packets);
        }
        for packet in to_client {
            client.handle_packet(&packet).expect("client packet");
            let output = drain_outputs(&mut client);
            client_connected |= output.connected;
            to_server.extend(output.packets);
        }
    }
    panic!("handshake did not complete");
}

fn accept_packets(endpoint: &mut Dtls, packets: Vec<Vec<u8>>) -> Vec<Vec<u8>> {
    let mut replies = Vec::new();
    for packet in packets {
        endpoint.handle_packet(&packet).expect("accept packet");
        replies.extend(drain_outputs(endpoint).packets);
    }
    replies
}

fn assert_connection_survives(client: &mut Dtls, server: &mut Dtls, base: Instant) {
    for second in 8..=120 {
        let now = base + Duration::from_secs(second);
        client
            .handle_timeout(now)
            .expect("client must stay connected");
        assert!(
            drain_outputs(client).packets.is_empty(),
            "ACKed final flight must not be retransmitted at {second}s"
        );
        server
            .handle_timeout(now)
            .expect("server must stay connected");
        assert!(drain_outputs(server).packets.is_empty());
    }

    assert_application_data(client, server);
    assert_application_data(server, client);
}

fn retransmit_final_flight(client: &mut Dtls, now: Instant) -> Vec<Vec<u8>> {
    let mut retransmission = Vec::new();
    for second in [2, 7] {
        client
            .handle_timeout(now + Duration::from_secs(second))
            .expect("retransmit final flight");
        retransmission = drain_outputs(client).packets;
        assert!(
            !retransmission.is_empty(),
            "final flight must be retransmitted"
        );
    }
    retransmission
}

fn assert_application_data(sender: &mut Dtls, receiver: &mut Dtls) {
    sender
        .send_application_data(b"after ACK")
        .expect("send data");
    let packets = drain_outputs(sender).packets;
    let mut received = Vec::new();
    for packet in packets {
        receiver.handle_packet(&packet).expect("receive data");
        received.extend(drain_outputs(receiver).app_data);
    }
    assert_eq!(received, [b"after ACK".to_vec()]);
}

#[test]
fn final_flight_accepts_late_ack_for_original_transmission() {
    for auto in [false, true] {
        let (mut client, mut server, now, ack) = handshake_with_held_final_ack(auto);
        let _lost_retransmissions = retransmit_final_flight(&mut client, now);

        assert!(accept_packets(&mut client, ack).is_empty());
        // The original ACK is sufficient even if the retransmission is lost.
        assert_connection_survives(&mut client, &mut server, now);
    }
}

#[test]
fn final_flight_recovers_after_lost_ack() {
    for auto in [false, true] {
        let (mut client, mut server, now, _lost_ack) = handshake_with_held_final_ack(auto);
        let retransmission = retransmit_final_flight(&mut client, now);

        let ack = accept_packets(&mut server, retransmission);
        assert!(!ack.is_empty(), "server must retransmit its final ACK");
        assert!(accept_packets(&mut client, ack).is_empty());
        assert_connection_survives(&mut client, &mut server, now);
    }
}
