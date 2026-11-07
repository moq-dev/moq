//! Tests for DTLS handshake edge cases and security.

use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use str0m::Rtc;
use str0m::RtcError;

mod common;
use common::{Peer, TestRtc, init_crypto_default, init_log, progress};

/// Test certificate fingerprint format and uniqueness.
#[test]
fn dtls_certificate_fingerprint_format() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    // Create two instances and verify they have different certificates
    let mut l = TestRtc::new(Peer::Left);
    let mut r = TestRtc::new(Peer::Right);

    l.add_host_candidate((Ipv4Addr::new(1, 1, 1, 1), 1000).into());
    r.add_host_candidate((Ipv4Addr::new(2, 2, 2, 2), 2000).into());

    // Get fingerprints
    let finger_l = l.direct_api().local_dtls_fingerprint().clone();
    let finger_r = r.direct_api().local_dtls_fingerprint().clone();

    // Fingerprints should be different for different instances
    assert_ne!(
        finger_l, finger_r,
        "Different Rtc instances should have different certificates"
    );

    // Fingerprint should be properly formatted (SHA-256 format)
    let finger_str = format!("{}", finger_l);
    assert!(
        finger_str.starts_with("sha-256 ") || finger_str.contains(':'),
        "Fingerprint should be in SHA-256 format: {}",
        finger_str
    );

    Ok(())
}

/// Test that new Rtc instances get new certificates.
#[test]
fn dtls_certificate_rotation() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    // Create first connection using L/R peers (respects L_CRYPTO/R_CRYPTO env vars)
    let mut l1 = TestRtc::new(Peer::Left);
    let mut r1 = TestRtc::new(Peer::Right);

    let finger1_l = l1.direct_api().local_dtls_fingerprint().clone();
    let finger1_r = r1.direct_api().local_dtls_fingerprint().clone();

    // Create second connection with new certificates
    let mut l2 = TestRtc::new(Peer::Left);
    let mut r2 = TestRtc::new(Peer::Right);

    let finger2_l = l2.direct_api().local_dtls_fingerprint().clone();
    let finger2_r = r2.direct_api().local_dtls_fingerprint().clone();

    // All fingerprints should be unique
    assert_ne!(finger1_l, finger1_r, "L1 and R1 should differ");
    assert_ne!(finger2_l, finger2_r, "L2 and R2 should differ");
    assert_ne!(finger1_l, finger2_l, "L1 and L2 should differ");
    assert_ne!(finger1_r, finger2_r, "R1 and R2 should differ");

    Ok(())
}

/// Test DTLS connection with ice-lite mode (passive role).
#[test]
fn dtls_with_ice_lite() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let mut l = TestRtc::new(Peer::Left);

    // R is ice-lite (typically server-side), but still use Peer::Right crypto provider
    let mut rtc_r_builder = Rtc::builder().set_ice_lite(true);
    if let Some(crypto) = Peer::Right.crypto_provider() {
        rtc_r_builder = rtc_r_builder.set_crypto_provider(crypto);
    }
    let mut r = TestRtc::new_with_rtc(Peer::Right.span(), rtc_r_builder.build(Instant::now()));

    l.add_host_candidate((Ipv4Addr::new(1, 1, 1, 1), 1000).into());
    r.add_host_candidate((Ipv4Addr::new(2, 2, 2, 2), 2000).into());

    let (offer, pending) = l.span.in_scope(|| {
        let mut change = l.rtc.sdp_api();
        let _ = change.add_channel("test".into());
        change.apply().unwrap()
    });

    let answer = r.span.in_scope(|| r.rtc.sdp_api().accept_offer(offer))?;
    l.span
        .in_scope(|| l.rtc.sdp_api().accept_answer(pending, answer))?;

    loop {
        if l.is_connected() && r.is_connected() {
            break;
        }
        if l.duration() > Duration::from_secs(5) {
            panic!("Failed to connect with ice-lite");
        }
        progress(&mut l, &mut r)?;
    }

    Ok(())
}

/// Test pregenerated DTLS certificate.
#[test]
fn dtls_pregenerated_certificate() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    // Generate a certificate using Peer::Left's crypto provider if set
    let provider = Peer::Left
        .crypto_provider()
        .unwrap_or_else(|| std::sync::Arc::new(str0m::crypto::from_feature_flags()));
    let cert = provider.dtls_provider.generate_certificate().unwrap();

    // Use the pregenerated certificate with the same crypto provider
    let mut rtc_l_builder = Rtc::builder().set_dtls_cert(cert);
    if let Some(crypto) = Peer::Left.crypto_provider() {
        rtc_l_builder = rtc_l_builder.set_crypto_provider(crypto);
    }

    let mut l = TestRtc::new_with_rtc(Peer::Left.span(), rtc_l_builder.build(Instant::now()));
    let mut r = TestRtc::new(Peer::Right);

    l.add_host_candidate((Ipv4Addr::new(1, 1, 1, 1), 1000).into());
    r.add_host_candidate((Ipv4Addr::new(2, 2, 2, 2), 2000).into());

    let (offer, pending) = l.span.in_scope(|| {
        let mut change = l.rtc.sdp_api();
        let _ = change.add_channel("test".into());
        change.apply().unwrap()
    });

    let answer = r.span.in_scope(|| r.rtc.sdp_api().accept_offer(offer))?;
    l.span
        .in_scope(|| l.rtc.sdp_api().accept_answer(pending, answer))?;

    loop {
        if l.is_connected() && r.is_connected() {
            break;
        }
        if l.duration() > Duration::from_secs(5) {
            panic!("Failed to connect with pregenerated certificate");
        }
        progress(&mut l, &mut r)?;
    }

    Ok(())
}

/// Test that same pregenerated certificate produces same fingerprint.
#[test]
fn dtls_pregenerated_certificate_same_fingerprint() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    // Generate a certificate using Peer::Left's crypto provider if set
    let provider = Peer::Left
        .crypto_provider()
        .unwrap_or_else(|| std::sync::Arc::new(str0m::crypto::from_feature_flags()));
    let cert = provider.dtls_provider.generate_certificate().unwrap();
    let cert_clone = cert.clone();

    // Create two instances with the same certificate
    let mut rtc1_builder = Rtc::builder().set_dtls_cert(cert);
    let mut rtc2_builder = Rtc::builder().set_dtls_cert(cert_clone);
    if let Some(crypto) = Peer::Left.crypto_provider() {
        rtc1_builder = rtc1_builder.set_crypto_provider(crypto.clone());
        rtc2_builder = rtc2_builder.set_crypto_provider(crypto);
    }

    let mut l1 = TestRtc::new_with_rtc(Peer::Left.span(), rtc1_builder.build(Instant::now()));
    let mut l2 = TestRtc::new_with_rtc(Peer::Left.span(), rtc2_builder.build(Instant::now()));

    let finger1 = l1.direct_api().local_dtls_fingerprint().clone();
    let finger2 = l2.direct_api().local_dtls_fingerprint().clone();

    // Same certificate should produce same fingerprint
    assert_eq!(
        finger1, finger2,
        "Same certificate should produce same fingerprint"
    );

    Ok(())
}
