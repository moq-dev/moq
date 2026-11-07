//! DTLS can change its timeout even when processing a packet returns an error.
#![cfg(any(
    feature = "aws-lc-rs",
    feature = "rust-crypto",
    feature = "openssl-dimpl",
    feature = "wincrypto",
    feature = "apple-crypto",
))]

use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use str0m::config::DtlsVersion;
use str0m::net::{Protocol, Receive};
use str0m::{Input, Output, Reason, Rtc, RtcError};

mod common;
use common::init_crypto_default;

#[test]
fn rejected_client_hello_still_exposes_new_handshake_deadline() {
    init_crypto_default();
    let now = Instant::now();
    let mut rtc = Rtc::builder()
        .set_dtls_version(DtlsVersion::Dtls12)
        .build(now);
    rtc.direct_api().start_dtls(false).unwrap();
    match rtc.poll_output().unwrap() {
        Output::Timeout(deadline) => assert!(deadline > now + Duration::from_secs(60)),
        output => panic!("expected an idle timeout, got {output:?}"),
    }

    // A structurally valid ClientHello starts dimpl's handshake deadline before
    // its unsupported DTLS 1.0 client version is rejected by body validation.
    let mut body = vec![0xfe, 0xff];
    body.extend_from_slice(&[0; 32]); // random
    body.extend_from_slice(&[0, 0]); // empty session ID and cookie
    body.extend_from_slice(&[0, 2, 0xc0, 0x2b]); // one cipher suite
    body.extend_from_slice(&[1, 0]); // null compression
    body.extend_from_slice(&[0, 0]); // no extensions
    let body_len = body.len() as u8;
    let mut handshake = vec![1, 0, 0, body_len, 0, 0, 0, 0, 0, 0, 0, body_len];
    handshake.extend_from_slice(&body);
    let mut packet = vec![22, 0xfe, 0xfd, 0, 0, 0, 0, 0, 0, 0, 0];
    packet.extend_from_slice(&(handshake.len() as u16).to_be_bytes());
    packet.extend_from_slice(&handshake);

    let input = Input::Receive(
        now,
        Receive {
            proto: Protocol::Udp,
            source: (Ipv4Addr::LOCALHOST, 1234).into(),
            destination: (Ipv4Addr::LOCALHOST, 5678).into(),
            contents: packet.as_slice().try_into().unwrap(),
        },
    );
    let error = rtc.handle_input(input).unwrap_err();
    assert!(matches!(error, RtcError::Dtls(_)));
    assert!(error.to_string().contains("unsupported DTLS version"));

    match rtc.poll_output().unwrap() {
        Output::Timeout(deadline) => assert!(
            deadline <= now + Duration::from_secs(60),
            "the changed DTLS deadline must replace the previously cached idle timeout"
        ),
        output => panic!("expected the DTLS handshake timeout, got {output:?}"),
    }
    assert_eq!(rtc.last_timeout_reason(), Reason::DTLS);
}
