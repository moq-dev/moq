//! Shared helpers for DTLS 1.2 integration tests.

#![allow(unused)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use dimpl::{Config, Dtls, Output, SrtpProfile};

/// Parsed DTLS 1.2 record header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecHdr {
    pub ctype: u8,
    pub epoch: u16,
    pub seq: u64,
}

/// Handshake message types (RFC 5246 / 6347).
pub const CLIENT_HELLO: u8 = 1;
pub const SERVER_HELLO: u8 = 2;
pub const HELLO_VERIFY_REQUEST: u8 = 3;
pub const CERTIFICATE: u8 = 11;
pub const SERVER_HELLO_DONE: u8 = 14;

/// Parse DTLS 1.2 record headers from a datagram.
pub fn parse_records(datagram: &[u8]) -> Vec<RecHdr> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i + 13 <= datagram.len() {
        let ctype = datagram[i];
        let epoch = u16::from_be_bytes([datagram[i + 3], datagram[i + 4]]);
        let seq_bytes = [
            0u8,
            0u8,
            datagram[i + 5],
            datagram[i + 6],
            datagram[i + 7],
            datagram[i + 8],
            datagram[i + 9],
            datagram[i + 10],
        ];
        let seq = u64::from_be_bytes(seq_bytes);
        let len = u16::from_be_bytes([datagram[i + 11], datagram[i + 12]]) as usize;
        out.push(RecHdr { ctype, epoch, seq });
        i += 13 + len;
    }
    out
}

/// Collect record headers from a slice of datagrams.
pub fn collect_headers(datagrams: &[Vec<u8>]) -> Vec<RecHdr> {
    datagrams.iter().flat_map(|d| parse_records(d)).collect()
}

/// Parse handshake message types from a datagram (content_type=22).
pub fn parse_handshake_types(datagram: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i + 13 <= datagram.len() {
        let ctype = datagram[i];
        let len = u16::from_be_bytes([datagram[i + 11], datagram[i + 12]]) as usize;

        // Only parse handshake records (content_type=22)
        if ctype == 22 && i + 14 <= datagram.len() {
            // Handshake message type is first byte of payload
            let hs_type = datagram[i + 13];
            out.push(hs_type);
        }
        i += 13 + len;
    }
    out
}

/// Assert that retransmitted records have the same epochs but increased sequence numbers.
pub fn assert_epochs_and_seq_increased(init: &[RecHdr], resend: &[RecHdr]) {
    assert_eq!(
        init.len(),
        resend.len(),
        "record count must match between initial and resend"
    );
    for (a, b) in init.iter().zip(resend.iter()) {
        assert_eq!(
            a.epoch, b.epoch,
            "epoch must match for the same record on resend"
        );
        assert!(
            b.seq > a.seq,
            "sequence must increase on resend: {:?} -> {:?}",
            a,
            b
        );
    }
}

/// Poll until `Timeout`, collecting only packets.
pub fn collect_packets(endpoint: &mut Dtls) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut buf = vec![0u8; 2048];
    loop {
        match endpoint.poll_output(&mut buf) {
            Output::Packet(p) => out.push(p.to_vec()),
            Output::Timeout(_) => break,
            _ => {}
        }
    }
    out
}

/// Collected outputs from polling an endpoint to `Timeout`.
#[derive(Default, Debug)]
pub struct DrainedOutputs {
    pub packets: Vec<Vec<u8>>,
    pub connected: bool,
    pub peer_cert: Option<Vec<u8>>,
    pub keying_material: Option<(Vec<u8>, SrtpProfile)>,
    pub app_data: Vec<Vec<u8>>,
    pub timeout: Option<Instant>,
    pub close_notify: bool,
}

/// Poll until `Timeout`, collecting everything.
pub fn drain_outputs(endpoint: &mut Dtls) -> DrainedOutputs {
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
            Output::CloseNotify => result.close_notify = true,
            Output::Timeout(t) => {
                result.timeout = Some(t);
                break;
            }
            _ => {}
        }
    }
    result
}

/// Deliver a slice of packets to a destination endpoint.
pub fn deliver_packets(packets: &[Vec<u8>], dest: &mut Dtls) {
    for p in packets {
        // Ignore errors - they may be expected for duplicates/replays
        let _ = dest.handle_packet(p);
    }
}

/// Trigger a timeout by advancing time 2 seconds.
pub fn trigger_timeout(ep: &mut Dtls, now: &mut Instant) {
    *now += Duration::from_secs(2);
    ep.handle_timeout(*now).expect("handle_timeout");
}

/// Create a DTLS 1.2 config with default settings.
pub fn dtls12_config() -> Arc<Config> {
    Arc::new(Config::default())
}

/// Create a DTLS 1.2 config with custom MTU.
pub fn dtls12_config_with_mtu(mtu: usize) -> Arc<Config> {
    Arc::new(
        Config::builder()
            .mtu(mtu)
            .build()
            .expect("Failed to build config"),
    )
}

/// Complete a full DTLS 1.2 handshake between client and server.
///
/// Returns the final `Instant` (time advanced during the handshake).
/// Panics if the handshake does not complete within the iteration limit.
pub fn complete_dtls12_handshake(
    client: &mut Dtls,
    server: &mut Dtls,
    mut now: Instant,
) -> Instant {
    let mut client_connected = false;
    let mut server_connected = false;

    for i in 0..60 {
        client.handle_timeout(now).expect("client timeout");
        server.handle_timeout(now).expect("server timeout");

        let client_out = drain_outputs(client);
        let server_out = drain_outputs(server);

        client_connected |= client_out.connected;
        server_connected |= server_out.connected;

        deliver_packets(&client_out.packets, server);
        deliver_packets(&server_out.packets, client);

        if client_connected && server_connected {
            return now;
        }

        // Trigger retransmissions periodically
        if i % 5 == 4 {
            now += Duration::from_secs(2);
        } else {
            now += Duration::from_millis(50);
        }
    }

    panic!("DTLS 1.2 handshake did not complete within iteration limit");
}

/// Create a connected DTLS 1.2 client/server pair with self-signed certificates.
///
/// Returns `(client, server, now)` with the handshake already completed.
#[cfg(feature = "rcgen")]
pub fn setup_connected_12_pair(now: Instant) -> (Dtls, Dtls, Instant) {
    use dimpl::certificate::generate_self_signed_certificate;

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");
    let config = dtls12_config();

    let mut client = Dtls::new_12(Arc::clone(&config), client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_12(config, server_cert, now);
    server.set_active(false);

    let now = complete_dtls12_handshake(&mut client, &mut server, now);
    (client, server, now)
}
