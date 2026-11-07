#![cfg(feature = "rcgen")]

use std::sync::Arc;
use std::time::Instant;

use dimpl::{Config, Dtls, DtlsCertificate, Output, certificate};

const COOKIE: [u8; 20] = [0xC0; 20];
const HRR_RANDOM: [u8; 32] = [
    0xCF, 0x21, 0xAD, 0x74, 0xE5, 0x9A, 0x61, 0x11, 0xBE, 0x1D, 0x8C, 0x02, 0x1E, 0x65, 0xB8, 0x91,
    0xC2, 0xA2, 0x11, 0x16, 0x7A, 0xBB, 0x8C, 0x5E, 0x07, 0x9E, 0x09, 0xE2, 0xC8, 0xA8, 0x33, 0x9C,
];

struct Hello {
    parameters: Vec<u8>,
    cookie: Vec<u8>,
    extensions: Vec<(u16, Vec<u8>)>,
}

fn next_hello(client: &mut Dtls) -> Hello {
    let mut packet = None;
    let mut buf = [0; 2048];
    loop {
        match client.poll_output(&mut buf) {
            Output::Packet(p) => {
                assert!(packet.is_none(), "one ClientHello datagram");
                packet = Some(p.to_vec());
            }
            Output::Timeout(_) => break,
            _ => {}
        }
    }
    let packet = packet.expect("ClientHello");
    assert_eq!(packet[13], 1);
    let body = &packet[25..];
    assert_eq!(body[34], 0, "empty session ID");
    let suites = 36 + body[35] as usize;
    let compression = suites + 2 + u16::from_be_bytes([body[suites], body[suites + 1]]) as usize;
    let extensions = compression + 1 + body[compression] as usize;
    let mut hello = Hello {
        parameters: body[..35].to_vec(),
        cookie: body[36..suites].to_vec(),
        extensions: Vec::new(),
    };
    hello
        .parameters
        .extend_from_slice(&body[suites..extensions]);
    let mut remaining = &body[extensions + 2..];
    while !remaining.is_empty() {
        let kind = u16::from_be_bytes([remaining[0], remaining[1]]);
        let len = u16::from_be_bytes([remaining[2], remaining[3]]) as usize;
        hello
            .extensions
            .push((kind, remaining[4..4 + len].to_vec()));
        remaining = &remaining[4 + len..];
    }
    hello
}

fn challenge(dtls13: bool, group: Option<u16>) -> Vec<u8> {
    let (kind, body) = if dtls13 {
        let mut body = vec![0xFE, 0xFD];
        body.extend_from_slice(&HRR_RANDOM);
        body.extend_from_slice(&[0, 0x13, 1, 0]); // session ID, cipher suite, compression
        let mut extensions = vec![0, 43, 0, 2, 0xFE, 0xFC, 0, 44, 0, 22, 0, 20];
        extensions.extend_from_slice(&COOKIE);
        if let Some(group) = group {
            extensions.extend_from_slice(&[0, 51, 0, 2]);
            extensions.extend_from_slice(&group.to_be_bytes());
        }
        body.extend_from_slice(&(extensions.len() as u16).to_be_bytes());
        body.extend_from_slice(&extensions);
        (2, body)
    } else {
        let mut body = vec![0xFE, 0xFF, COOKIE.len() as u8];
        body.extend_from_slice(&COOKIE);
        (3, body)
    };
    let mut packet = vec![22, 0xFE, 0xFD, 0, 0, 0, 0, 0, 0, 0, 0];
    packet.extend_from_slice(&((body.len() + 12) as u16).to_be_bytes());
    packet.push(kind);
    packet.extend_from_slice(&(body.len() as u32).to_be_bytes()[1..]);
    packet.extend_from_slice(&[0; 5]); // message_seq and fragment_offset
    packet.extend_from_slice(&(body.len() as u32).to_be_bytes()[1..]);
    packet.extend_from_slice(&body);
    packet
}

fn check_retry(
    new: fn(Arc<Config>, DtlsCertificate, Instant) -> Dtls,
    dtls13: bool,
    group: Option<u16>,
) {
    let now = Instant::now();
    let config = Arc::new(Config::builder().build().expect("config"));
    let cert = certificate::generate_self_signed_certificate().expect("certificate");
    let mut client = new(config, cert, now);
    client.set_active(true);
    client.handle_timeout(now).expect("timeout");
    let mut first = next_hello(&mut client);
    client
        .handle_packet(&challenge(dtls13, group))
        .expect("challenge");
    let mut retry = next_hello(&mut client);
    assert_eq!(
        first.parameters, retry.parameters,
        "original offer parameters"
    );
    if dtls13 {
        assert_eq!(first.cookie, retry.cookie);
        let cookie = retry
            .extensions
            .iter()
            .find(|(kind, _)| *kind == 44)
            .expect("cookie");
        assert_eq!(&cookie.1[2..], COOKIE);
        if let Some(group) = group {
            let key = retry
                .extensions
                .iter()
                .find(|(kind, _)| *kind == 51)
                .expect("key share");
            assert_eq!(&key.1[2..4], group.to_be_bytes());
            assert_ne!(
                first.extensions.iter().find(|(kind, _)| *kind == 51),
                Some(key)
            );
        }
        // HRR may change cookie, padding, and the requested key share only.
        let unchanged = |(kind, _): &(u16, Vec<u8>)| {
            *kind != 44 && *kind != 21 && !(group.is_some() && *kind == 51)
        };
        first.extensions.retain(unchanged);
        retry.extensions.retain(unchanged);
        assert_eq!(first.extensions, retry.extensions);
    } else {
        assert_eq!(retry.cookie, COOKIE);
        assert_eq!(
            first.extensions.iter().find(|(kind, _)| *kind == 14),
            retry.extensions.iter().find(|(kind, _)| *kind == 14)
        );
        assert!(
            !retry
                .extensions
                .iter()
                .any(|(kind, _)| *kind == 43 || *kind == 51)
        );
    }
}

#[test]
fn hello_verify_request_preserves_offer() {
    check_retry(Dtls::new_auto, false, None);
    check_retry(Dtls::new_12, false, None);
}

#[test]
fn hello_retry_request_preserves_offer() {
    check_retry(Dtls::new_auto, true, None);
    check_retry(Dtls::new_13, true, None);
    check_retry(Dtls::new_auto, true, Some(23)); // secp256r1 replaces X25519
}
