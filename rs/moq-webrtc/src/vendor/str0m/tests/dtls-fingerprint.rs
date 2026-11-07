//! The remote certificate fingerprint is verified with the hash function the
//! remote names (RFC 8122 §5): sha-256 or sha-1.
#![cfg(any(
    feature = "aws-lc-rs",
    feature = "rust-crypto",
    feature = "openssl",
    feature = "openssl-dimpl",
    feature = "wincrypto",
    feature = "apple-crypto",
))]

use std::net::Ipv4Addr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use str0m::crypto::{CryptoProvider, Fingerprint, Sha1Provider};
use str0m::{Candidate, RtcConfig, RtcError};
use tracing::info_span;

mod common;
use common::{TestRtc, init_crypto_default, init_log, progress};

/// Connects L to R, where L expects R's certificate to have the fingerprint
/// `expected(R's DER certificate)`. Returns L's error, if any.
fn connect(expected: impl Fn(&[u8]) -> Fingerprint) -> Result<(), RtcError> {
    connect_with_provider(expected, str0m::crypto::from_feature_flags())
}

fn connect_with_provider(
    expected: impl Fn(&[u8]) -> Fingerprint,
    provider: CryptoProvider,
) -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let cert = provider
        .dtls_provider
        .generate_certificate()
        .expect("the provider generates certificates");
    let finger_r = expected(&cert.certificate);

    let now = Instant::now();
    let provider = Arc::new(provider);
    let mut l = TestRtc::new_with_rtc(
        info_span!("L"),
        RtcConfig::new()
            .set_crypto_provider(provider.clone())
            .build(now),
    );
    let mut r = TestRtc::new_with_rtc(
        info_span!("R"),
        RtcConfig::new()
            .set_crypto_provider(provider)
            .set_dtls_cert(cert)
            .build(now),
    );

    let host_l = Candidate::host((Ipv4Addr::new(1, 1, 1, 1), 1000).into(), "udp").unwrap();
    let host_r = Candidate::host((Ipv4Addr::new(2, 2, 2, 2), 2000).into(), "udp").unwrap();
    l.add_local_candidate(host_l.clone()).unwrap();
    l.add_remote_candidate(host_r.clone());
    r.add_local_candidate(host_r).unwrap();
    r.add_remote_candidate(host_l);

    let finger_l = l.direct_api().local_dtls_fingerprint().clone();
    l.direct_api().set_remote_fingerprint(finger_r);
    r.direct_api().set_remote_fingerprint(finger_l);

    let creds_l = l.direct_api().local_ice_credentials();
    let creds_r = r.direct_api().local_ice_credentials();
    l.direct_api().set_remote_ice_credentials(creds_r);
    r.direct_api().set_remote_ice_credentials(creds_l);
    l.direct_api().set_ice_controlling(true);
    r.direct_api().set_ice_controlling(false);

    // L is the DTLS client, as towards a media gateway answering `a=setup:passive`.
    l.direct_api().start_dtls(true).unwrap();
    r.direct_api().start_dtls(false).unwrap();

    while !(l.is_connected() && r.is_connected()) {
        if l.duration() > Duration::from_secs(5) {
            panic!("DTLS did not connect");
        }
        progress(&mut l, &mut r)?;
    }
    Ok(())
}

fn reference_sha1(data: &[u8]) -> [u8; 20] {
    str0m_rust_crypto::default_provider()
        .sha1_provider
        .sha1(data)
}

fn sha1_fingerprint(hash_func: &str, der: &[u8]) -> Fingerprint {
    Fingerprint {
        hash_func: hash_func.into(),
        bytes: reference_sha1(der).to_vec(),
    }
}

#[test]
fn sha1_fingerprint_is_verified() -> Result<(), RtcError> {
    connect(|der| sha1_fingerprint("sha-1", der))
}

#[test]
fn sha1_fingerprint_uses_configured_provider() -> Result<(), RtcError> {
    #[derive(Debug)]
    struct TrackingSha1(AtomicUsize);

    impl Sha1Provider for TrackingSha1 {
        fn sha1(&self, data: &[u8]) -> [u8; 20] {
            self.0.fetch_add(1, Ordering::SeqCst);
            reference_sha1(data)
        }
    }

    static SHA1: TrackingSha1 = TrackingSha1(AtomicUsize::new(0));
    let mut provider = str0m::crypto::from_feature_flags();
    provider.sha1_provider = &SHA1;
    connect_with_provider(|der| sha1_fingerprint("sha-1", der), provider)?;
    assert_eq!(SHA1.0.load(Ordering::SeqCst), 1);
    Ok(())
}

#[test]
fn hash_function_names_are_case_insensitive() -> Result<(), RtcError> {
    connect(|der| sha1_fingerprint("SHA-1", der))
}

#[test]
fn wrong_sha1_fingerprint_is_rejected() {
    let error = connect(|der| {
        let mut wrong = sha1_fingerprint("sha-1", der);
        wrong.bytes[0] ^= 0xff;
        wrong
    })
    .unwrap_err();
    assert!(
        error.to_string().contains("remote fingerprint no match"),
        "{error}"
    );
}

#[test]
fn unsupported_hash_function_is_rejected() {
    let error = connect(|_| Fingerprint {
        hash_func: "sha-512".into(),
        bytes: vec![0; 64],
    })
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("unsupported fingerprint hash function: sha-512"),
        "{error}"
    );
}

#[test]
fn stronger_fingerprint_mismatch_must_be_rejected() {
    init_log();
    init_crypto_default();
    let provider = str0m::crypto::from_feature_flags();
    let cert = provider.dtls_provider.generate_certificate().unwrap();
    let weak = Fingerprint {
        hash_func: "sha-1".into(),
        bytes: reference_sha1(&cert.certificate).to_vec(),
    };
    let strong = Fingerprint {
        hash_func: "sha-256".into(),
        bytes: vec![0; 32],
    };
    let now = Instant::now();
    let mut l = TestRtc::new_with_rtc(info_span!("L"), RtcConfig::new().build(now));
    let mut r = TestRtc::new_with_rtc(
        info_span!("R"),
        RtcConfig::new().set_dtls_cert(cert).build(now),
    );
    l.add_local_candidate(
        Candidate::host((Ipv4Addr::new(1, 1, 1, 1), 1000).into(), "udp").unwrap(),
    )
    .unwrap();
    r.add_local_candidate(
        Candidate::host((Ipv4Addr::new(2, 2, 2, 2), 2000).into(), "udp").unwrap(),
    )
    .unwrap();
    let mut change = l.sdp_api();
    change.add_channel("test".into());
    let (offer, pending) = change.apply().unwrap();
    let answer = r.sdp_api().accept_offer(offer).unwrap();
    let sdp = answer.to_sdp_string();
    let rewritten = sdp
        .lines()
        .map(|line| {
            if line.starts_with("a=fingerprint:") {
                format!("a=fingerprint:{weak}\r\na=fingerprint:{strong}\r\n")
            } else {
                format!("{line}\r\n")
            }
        })
        .collect::<String>();
    l.sdp_api()
        .accept_answer(
            pending,
            str0m::change::SdpAnswer::from_sdp_string(&rewritten).unwrap(),
        )
        .unwrap();
    while !(l.is_connected() && r.is_connected()) {
        assert!(l.duration() < Duration::from_secs(5));
        if let Err(error) = progress(&mut l, &mut r) {
            assert!(error.to_string().contains("remote fingerprint no match"));
            return;
        }
    }
    panic!("connected despite a mismatched offered SHA-256 fingerprint");
}
