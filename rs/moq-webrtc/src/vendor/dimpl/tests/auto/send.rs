//! Accepted byte counts and caller retries across version selection.

#![cfg(feature = "rcgen")]

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use dimpl::certificate::generate_self_signed_certificate;
use dimpl::{Dtls, Error};

use crate::common::{default_config, drain_outputs};

#[test]
fn accepted_data_is_delivered_once_in_both_directions() {
    for version in [12, 13, 0] {
        let mut now = Instant::now();
        let certificate = generate_self_signed_certificate().expect("certificate");
        let new = match version {
            12 => Dtls::new_12,
            13 => Dtls::new_13,
            _ => Dtls::new_auto,
        };
        let mut peers = [
            new(default_config(), certificate.clone(), now),
            new(default_config(), certificate, now),
        ];
        peers[0].set_active(true);
        let messages: [&[u8]; 2] = [b"client payload", b"server payload"];
        let mut accepted = [false; 2];
        let mut received = [Vec::new(), Vec::new()];
        let mut connected = [false; 2];
        let mut packets = VecDeque::new();

        for i in 0..2 {
            let count = peers[i]
                .send_application_data(messages[i])
                .expect("early send");
            assert_eq!(count, if version == 0 { 0 } else { messages[i].len() });
            accepted[i] = count > 0;
        }
        for _ in 0..40 {
            for i in 0..2 {
                peers[i].handle_timeout(now).expect("advance time");
                let output = drain_outputs(&mut peers[i]);
                connected[i] |= output.connected;
                received[i].extend(output.app_data);
                packets.extend(output.packets.into_iter().map(|p| (1 - i, p)));
            }
            while let Some((i, packet)) = packets.pop_front() {
                peers[i].handle_packet(&packet).expect("receive packet");
                let output = drain_outputs(&mut peers[i]);
                connected[i] |= output.connected;
                received[i].extend(output.app_data);
                packets.extend(output.packets.into_iter().map(|p| (1 - i, p)));
            }
            for i in 0..2 {
                if !accepted[i] && connected[i] {
                    assert_eq!(
                        peers[i].send_application_data(messages[i]),
                        Ok(messages[i].len())
                    );
                    accepted[i] = true;
                    let output = drain_outputs(&mut peers[i]);
                    packets.extend(output.packets.into_iter().map(|p| (1 - i, p)));
                }
            }
            if received.iter().all(|data| !data.is_empty()) && packets.is_empty() {
                break;
            }
            now += Duration::from_millis(10);
        }
        assert_eq!(connected, [true; 2]);
        for i in 0..2 {
            assert_eq!(received[i], vec![messages[1 - i].to_vec()]);
            assert_eq!(peers[i].send_application_data(b""), Ok(0));
            drain_outputs(&mut peers[i]);
            peers[i].close().expect("close connected peer");
            drain_outputs(&mut peers[i]);
            assert_eq!(
                peers[i].send_application_data(messages[i]),
                Err(Error::ConnectionClosed)
            );
        }
    }
}

#[test]
fn closing_pending_auto_aborts_without_an_alert() {
    for active in [false, true] {
        for started in [false, true] {
            let now = Instant::now();
            let certificate = generate_self_signed_certificate().expect("certificate");
            let mut endpoint = Dtls::new_auto(default_config(), certificate, now);
            endpoint.set_active(active);
            if started {
                endpoint.handle_timeout(now).expect("start handshake");
                drain_outputs(&mut endpoint);
            }
            endpoint.close().expect("close pending endpoint");
            assert!(endpoint.is_closed());
            assert!(!endpoint.is_closing());
            assert_eq!(endpoint.is_active(), active);
            assert_eq!(endpoint.protocol_version(), None);
            assert_eq!(
                endpoint.send_application_data(b"data"),
                Err(Error::ConnectionClosed)
            );
            endpoint.close().expect("idempotent close");
            assert!(drain_outputs(&mut endpoint).packets.is_empty());

            let later = now + Duration::from_secs(120);
            endpoint
                .handle_timeout(later)
                .expect("closed endpoint has no retry timer");
            let output = drain_outputs(&mut endpoint);
            assert!(output.packets.is_empty());
            assert!(output.timeout.expect("idle timeout") > later);
            endpoint
                .handle_packet(b"late packet")
                .expect("ignore input after close");
            assert!(drain_outputs(&mut endpoint).packets.is_empty());
        }
    }
}

#[test]
fn closed_auto_server_ignores_valid_late_client_hellos() {
    for new_client in [Dtls::new_12, Dtls::new_13, Dtls::new_auto] {
        let now = Instant::now();
        let certificate = generate_self_signed_certificate().expect("certificate");
        let mut client = new_client(default_config(), certificate.clone(), now);
        client.set_active(true);
        client.handle_timeout(now).expect("start client");
        let hello = drain_outputs(&mut client).packets;
        assert!(!hello.is_empty());

        let mut server = Dtls::new_auto(default_config(), certificate, now);
        server.close().expect("close unresolved server");
        drain_outputs(&mut server);
        for packet in &hello {
            server
                .handle_packet(packet)
                .expect("ignore late ClientHello");
            let output = drain_outputs(&mut server);
            assert!(output.packets.is_empty());
            assert!(!output.connected);
            assert!(server.is_closed());
            assert_eq!(server.protocol_version(), None);
        }
    }
}
