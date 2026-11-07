#![cfg(any(feature = "aws-lc-rs", feature = "rust-crypto"))]

#[path = "dtls13/common.rs"]
mod common;

#[path = "ossl/mod.rs"]
mod ossl_helper;

#[cfg(not(windows))]
#[path = "wolfssl/mod.rs"]
mod wolfssl_helper;

use std::sync::Arc;
use std::time::Instant;

use dimpl::{Dtls, DtlsCertificate};

use crate::common::{drain_outputs, dtls13_config};
use crate::ossl_helper::{DtlsCertOptions, OsslDtlsCert};

fn certificate() -> DtlsCertificate {
    let cert = OsslDtlsCert::new(DtlsCertOptions::default());
    DtlsCertificate {
        certificate: cert.x509.to_der().expect("certificate DER"),
        private_key: cert.pkey.private_key_to_pkcs8().expect("private key DER"),
    }
}

fn cookie_extensions_start(body: &[u8], msg_type: u8) -> Option<usize> {
    let mut pos = 0;
    match msg_type {
        0x01 => {
            pos += 2 + 32;
            let sid_len = *body.get(pos)? as usize;
            pos += 1 + sid_len;
            let cookie_len = *body.get(pos)? as usize;
            pos += 1 + cookie_len;
            let suites_len = u16::from_be_bytes([*body.get(pos)?, *body.get(pos + 1)?]) as usize;
            pos += 2 + suites_len;
            let compression_len = *body.get(pos)? as usize;
            pos += 1 + compression_len;
        }
        0x02 => {
            pos += 2 + 32;
            let sid_len = *body.get(pos)? as usize;
            pos += 1 + sid_len + 2 + 1;
        }
        _ => return None,
    }

    Some(pos)
}

fn shrink_dtls13_cookie_extension_inner_len(packet: &mut [u8]) -> bool {
    const RECORD_HEADER_LEN: usize = 13;
    const HANDSHAKE_HEADER_LEN: usize = 12;
    const COOKIE_EXTENSION: u16 = 0x002C;

    if packet.len() < RECORD_HEADER_LEN + HANDSHAKE_HEADER_LEN || packet[0] != 22 {
        return false;
    }

    let handshake = &mut packet[RECORD_HEADER_LEN..];
    let msg_type = handshake[0];
    let body_len =
        ((handshake[1] as usize) << 16) | ((handshake[2] as usize) << 8) | handshake[3] as usize;
    if handshake.len() < HANDSHAKE_HEADER_LEN + body_len {
        return false;
    }

    let body = &mut handshake[HANDSHAKE_HEADER_LEN..HANDSHAKE_HEADER_LEN + body_len];
    let mut pos = match cookie_extensions_start(body, msg_type) {
        Some(pos) => pos,
        None => return false,
    };
    if body.len() < pos + 2 {
        return false;
    }

    let extensions_len = u16::from_be_bytes([body[pos], body[pos + 1]]) as usize;
    pos += 2;
    let extensions_end = pos + extensions_len;
    if body.len() < extensions_end {
        return false;
    }

    while pos + 4 <= extensions_end {
        let extension_type = u16::from_be_bytes([body[pos], body[pos + 1]]);
        let extension_len = u16::from_be_bytes([body[pos + 2], body[pos + 3]]) as usize;
        let extension_body = pos + 4;
        let next = extension_body + extension_len;
        if next > extensions_end {
            return false;
        }

        if extension_type == COOKIE_EXTENSION && extension_len > 2 {
            let cookie_len = u16::from_be_bytes([body[extension_body], body[extension_body + 1]]);
            if cookie_len == 0 {
                return false;
            }
            body[extension_body..extension_body + 2]
                .copy_from_slice(&(cookie_len - 1).to_be_bytes());
            return true;
        }

        pos = next;
    }

    false
}

#[test]
fn dtls13_client_rejects_hrr_cookie_extension_trailing_bytes() {
    let _ = env_logger::try_init();

    let client_cert = certificate();
    let server_cert = certificate();
    let config = dtls13_config();
    let now = Instant::now();

    let mut client = Dtls::new_13(Arc::clone(&config), client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_13(config, server_cert, now);
    server.set_active(false);

    client.handle_timeout(now).expect("client timeout");
    let client_out = drain_outputs(&mut client);
    assert!(!client_out.packets.is_empty(), "client should send CH1");
    for packet in &client_out.packets {
        server.handle_packet(packet).expect("server receives CH1");
    }

    server.handle_timeout(now).expect("server timeout");
    let server_out = drain_outputs(&mut server);
    let mut hrr = server_out
        .packets
        .into_iter()
        .next()
        .expect("server should emit HRR");
    assert!(
        shrink_dtls13_cookie_extension_inner_len(&mut hrr),
        "fixture should contain a Cookie extension"
    );

    client
        .handle_packet(&hrr)
        .expect("malformed HRR Cookie extension should be discarded");

    client
        .handle_timeout(now)
        .expect("client timeout after error");
    let client_out = drain_outputs(&mut client);
    assert!(
        client_out.packets.is_empty(),
        "client must not send CH2 after malformed HRR Cookie"
    );
}

#[test]
fn dtls13_server_rejects_clienthello_cookie_extension_trailing_bytes() {
    let _ = env_logger::try_init();

    let client_cert = certificate();
    let server_cert = certificate();
    let config = dtls13_config();
    let now = Instant::now();

    let mut client = Dtls::new_13(Arc::clone(&config), client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_13(config, server_cert, now);
    server.set_active(false);

    client.handle_timeout(now).expect("client timeout");
    let client_out = drain_outputs(&mut client);
    for packet in &client_out.packets {
        server.handle_packet(packet).expect("server receives CH1");
    }

    server.handle_timeout(now).expect("server timeout");
    let server_out = drain_outputs(&mut server);
    let hrr = server_out
        .packets
        .first()
        .expect("server should emit HRR")
        .clone();
    client
        .handle_packet(&hrr)
        .expect("client receives valid HRR");

    client
        .handle_timeout(now)
        .expect("client timeout after HRR");
    let client_out = drain_outputs(&mut client);
    let mut ch2 = client_out
        .packets
        .into_iter()
        .next()
        .expect("client should emit CH2 with cookie");
    assert!(
        shrink_dtls13_cookie_extension_inner_len(&mut ch2),
        "fixture should contain a Cookie extension"
    );

    server
        .handle_packet(&ch2)
        .expect("malformed ClientHello Cookie extension should be discarded");
}

#[test]
fn dtls13_cookie_only_retry_preserves_key_share() {
    const COOKIE: u16 = 44;
    const KEY_SHARE: u16 = 51;

    let config = dtls13_config();
    let now = Instant::now();
    let mut client = Dtls::new_13(Arc::clone(&config), certificate(), now);
    client.set_active(true);
    let mut server = Dtls::new_13(config, certificate(), now);
    server.set_active(false);
    server.handle_timeout(now).expect("start server");
    assert!(drain_outputs(&mut server).packets.is_empty());

    client.handle_timeout(now).expect("send CH1");
    let ch1 = drain_outputs(&mut client).packets;
    assert_eq!(ch1.len(), 1);
    let key_share = hello_extension(&ch1[0], 1, KEY_SHARE).expect("CH1 key_share");

    server.handle_packet(&ch1[0]).expect("receive CH1");
    let hrr = drain_outputs(&mut server).packets;
    assert_eq!(hrr.len(), 1);
    let cookie = hello_extension(&hrr[0], 2, COOKIE).expect("HRR cookie");
    assert!(
        hello_extension(&hrr[0], 2, KEY_SHARE).is_none(),
        "cookie-only HRR"
    );

    client.handle_packet(&hrr[0]).expect("receive HRR");
    let ch2 = drain_outputs(&mut client).packets;
    assert_eq!(ch2.len(), 1);
    assert_eq!(hello_extension(&ch2[0], 1, COOKIE), Some(cookie));
    assert_eq!(hello_extension(&ch2[0], 1, KEY_SHARE), Some(key_share));
}

fn hello_extension(packet: &[u8], message_type: u8, extension_type: u16) -> Option<&[u8]> {
    assert_eq!(packet[0], 22);
    assert_eq!(&packet[3..5], &[0, 0]);
    assert_eq!(packet[13], message_type);
    assert_eq!(&packet[19..22], &[0, 0, 0]);
    assert_eq!(&packet[14..17], &packet[22..25]);
    let body = &packet[25..];
    let start = cookie_extensions_start(body, message_type).expect("hello extensions");
    let length = u16::from_be_bytes([body[start], body[start + 1]]) as usize;
    let mut extensions = &body[start + 2..start + 2 + length];
    while !extensions.is_empty() {
        let kind = u16::from_be_bytes([extensions[0], extensions[1]]);
        let length = u16::from_be_bytes([extensions[2], extensions[3]]) as usize;
        if kind == extension_type {
            return Some(&extensions[4..4 + length]);
        }
        extensions = &extensions[4 + length..];
    }
    None
}

#[test]
#[cfg(not(windows))]
fn dtls13_wolfssl_cookie_only_retry_preserves_key_share() {
    use std::collections::VecDeque;
    use std::time::Duration;

    use crate::wolfssl_helper::WolfDtlsCert;

    const COOKIE: u16 = 44;
    const KEY_SHARE: u16 = 51;

    let server_cert = certificate();
    let wolf_cert = WolfDtlsCert::new(server_cert.certificate, server_cert.private_key);
    let mut server = wolf_cert.new_dtls13_impl(true).expect("wolfSSL server");
    let mut events = VecDeque::new();
    let mut now = Instant::now();
    let mut client = Dtls::new_13(dtls13_config(), certificate(), now);
    client.set_active(true);

    client.handle_timeout(now).expect("send CH1");
    let ch1 = drain_outputs(&mut client).packets;
    assert_eq!(ch1.len(), 1);
    let key_share = hello_extension(&ch1[0], 1, KEY_SHARE).expect("CH1 key_share");
    server
        .handle_receive(&ch1[0], &mut events)
        .expect("receive CH1");

    let hrr = server.poll_datagram().expect("wolfSSL HRR");
    let cookie = hello_extension(&hrr, 2, COOKIE).expect("HRR cookie");
    assert!(
        hello_extension(&hrr, 2, KEY_SHARE).is_none(),
        "cookie-only HRR"
    );
    client.handle_packet(&hrr).expect("receive HRR");
    let ch2 = drain_outputs(&mut client).packets;
    assert_eq!(ch2.len(), 1);
    assert_eq!(hello_extension(&ch2[0], 1, COOKIE), Some(cookie));
    assert_eq!(hello_extension(&ch2[0], 1, KEY_SHARE), Some(key_share));
    server
        .handle_receive(&ch2[0], &mut events)
        .expect("receive CH2");

    let mut connected = false;
    for _ in 0..50 {
        while let Some(packet) = server.poll_datagram() {
            client
                .handle_packet(&packet)
                .expect("receive wolfSSL flight");
            let out = drain_outputs(&mut client);
            connected |= out.connected;
            for packet in out.packets {
                server
                    .handle_receive(&packet, &mut events)
                    .expect("receive client flight");
            }
        }
        if connected && server.is_connected() {
            break;
        }
        now += Duration::from_millis(10);
        client.handle_timeout(now).expect("client timeout");
        let out = drain_outputs(&mut client);
        connected |= out.connected;
        for packet in out.packets {
            server
                .handle_receive(&packet, &mut events)
                .expect("receive client retry");
        }
    }
    assert!(connected, "dimpl client connected");
    assert!(server.is_connected(), "wolfSSL server connected");

    let payload = b"cookie retry interop";
    server.write(payload).expect("wolfSSL write");
    let mut received = Vec::new();
    while let Some(packet) = server.poll_datagram() {
        client
            .handle_packet(&packet)
            .expect("receive encrypted data");
        let out = drain_outputs(&mut client);
        for data in out.app_data {
            received.extend_from_slice(&data);
        }
        for packet in out.packets {
            server
                .handle_receive(&packet, &mut events)
                .expect("receive client ACK");
        }
    }
    assert_eq!(received, payload);
}
