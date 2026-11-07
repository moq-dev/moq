//! Regression test for the DTLS retransmit bug fixed in PR #943 (refs #932).
//!
//! When a DTLS flight timeout elapses, dimpl's `handle_timeout` re-arms the
//! flight timer and queues the retransmit packet. Previously str0m ran
//! `handle_timeout` at the end of `do_poll_output`, after both the DTLS
//! `poll_output` loop and `dtls.poll_packet()` had already run — so the
//! freshly queued retransmit was left sitting in dimpl's tx queue until
//! some unrelated subsystem (typically an ICE consent check) woke the
//! caller for another poll pass.
//!
//! This test drives two peers to the mid-handshake state (L has sent
//! ClientHello, R ignores DTLS so never replies), advances L's clock past
//! the flight deadline with a single `handle_input(Timeout)`, then asserts
//! that a single `poll_output` pass emits the retransmit as a `Transmit`.
//! Without the fix, the pass instead returns `Timeout` and the retransmit
//! is stranded.
//!
//! The scenario asserted here is specific to dimpl's tx-queue semantics,
//! so the test is compiled only under dimpl-backed crypto providers.
#![cfg(any(
    feature = "aws-lc-rs",
    feature = "rust-crypto",
    feature = "openssl-dimpl",
    feature = "wincrypto",
    feature = "apple-crypto",
))]
use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use netem::{NetemConfig, Probability, RandomLoss};
use str0m::RtcError;
use str0m::config::DtlsVersion;
use str0m::{Candidate, Input, Output, Reason, Rtc};
use tracing::info_span;

mod common;
use common::{TestRtc, init_crypto_default, progress};

#[test]
fn dtls_retransmit_emitted_in_same_poll_pass() {
    init_crypto_default();

    let now = Instant::now();
    let mut l = TestRtc::new_with_rtc(info_span!("L"), Rtc::new(now));
    let mut r = TestRtc::new_with_rtc(info_span!("R"), Rtc::new(now));

    let host_l = Candidate::host((Ipv4Addr::new(1, 1, 1, 1), 1000).into(), "udp").unwrap();
    let host_r = Candidate::host((Ipv4Addr::new(2, 2, 2, 2), 2000).into(), "udp").unwrap();
    l.add_local_candidate(host_l.clone());
    l.add_remote_candidate(host_r.clone());
    r.add_local_candidate(host_r);
    r.add_remote_candidate(host_l);

    let finger_l = l.direct_api().local_dtls_fingerprint().clone();
    let finger_r = r.direct_api().local_dtls_fingerprint().clone();
    l.direct_api().set_remote_fingerprint(finger_r);
    r.direct_api().set_remote_fingerprint(finger_l);

    let creds_l = l.direct_api().local_ice_credentials();
    let creds_r = r.direct_api().local_ice_credentials();
    l.direct_api().set_remote_ice_credentials(creds_r);
    r.direct_api().set_remote_ice_credentials(creds_l);

    l.direct_api().set_ice_controlling(true);
    r.direct_api().set_ice_controlling(false);

    // Only L starts DTLS. R receives L's ClientHello but ignores it
    // (active_state is None in dtls::handle_receive), so L never gets a
    // ServerHello and must retransmit.
    l.direct_api().start_dtls(true).unwrap();

    // Drive both sides until L is awaiting the DTLS flight deadline.
    let mut steps = 0;
    while l.rtc.last_timeout_reason() != Reason::DTLS {
        progress(&mut l, &mut r).unwrap();
        steps += 1;
        assert!(
            steps < 200,
            "failed to reach DTLS-awaiting state (last reason: {:?})",
            l.rtc.last_timeout_reason()
        );
    }

    // Advance past the flight deadline in one step with no network input.
    // The fix causes the following poll_output pass to emit the queued
    // retransmit. Without the fix, it returns Output::Timeout and the
    // retransmit stays stranded in dimpl.
    let deadline = l.last + Duration::from_millis(1500);
    l.rtc.handle_input(Input::Timeout(deadline)).unwrap();

    let mut got_transmit = false;
    loop {
        match l.rtc.poll_output().unwrap() {
            Output::Timeout(_) => break,
            Output::Transmit(_) => {
                got_transmit = true;
                break;
            }
            Output::Event(_) => continue,
        }
    }

    assert!(
        got_transmit,
        "poll_output should emit the DTLS retransmit in the same pass that runs handle_timeout"
    );
}

/// A terminal dimpl handshake timeout must be returned and must leave the
/// `Rtc` inert rather than continually returning the expired DTLS deadline.
///
/// This runs for every dimpl-backed crypto provider. It drives the scheduler
/// with the exact timeout returned by `Rtc`, which is the behavior that
/// previously caused an immediate-wake loop after dimpl had exhausted its
/// retry budget.
#[test]
fn terminal_dtls_12_to_auto_timeout_does_not_rearm_expired_deadline() {
    terminal_dtls_timeout_does_not_rearm_expired_deadline_for(
        DtlsVersion::Dtls12,
        DtlsVersion::Auto,
    );
}

#[test]
fn terminal_dtls_13_to_auto_timeout_does_not_rearm_expired_deadline() {
    terminal_dtls_timeout_does_not_rearm_expired_deadline_for(
        DtlsVersion::Dtls13,
        DtlsVersion::Auto,
    );
}

#[test]
fn terminal_dtls_auto_to_auto_timeout_does_not_rearm_expired_deadline() {
    terminal_dtls_timeout_does_not_rearm_expired_deadline_for(DtlsVersion::Auto, DtlsVersion::Auto);
}

#[test]
fn terminal_dtls_12_to_12_timeout_does_not_rearm_expired_deadline() {
    terminal_dtls_timeout_does_not_rearm_expired_deadline_for(
        DtlsVersion::Dtls12,
        DtlsVersion::Dtls12,
    );
}

#[test]
fn terminal_dtls_13_to_13_timeout_does_not_rearm_expired_deadline() {
    terminal_dtls_timeout_does_not_rearm_expired_deadline_for(
        DtlsVersion::Dtls13,
        DtlsVersion::Dtls13,
    );
}

fn terminal_dtls_timeout_does_not_rearm_expired_deadline_for(
    client_dtls: DtlsVersion,
    server_dtls: DtlsVersion,
) {
    init_crypto_default();

    let now = Instant::now();
    let left_rtc = Rtc::builder().set_dtls_version(client_dtls).build(now);
    let right_rtc = Rtc::builder().set_dtls_version(server_dtls).build(now);
    let mut left = TestRtc::new_with_rtc(info_span!("L"), left_rtc);
    let mut right = TestRtc::new_with_rtc(info_span!("R"), right_rtc);

    // Responses from the passive peer are discarded after it receives the
    // client's first flight, so the active peer retries until dimpl times out.
    left.set_netem(
        NetemConfig::new()
            .loss(RandomLoss::new(Probability::new(1.0)))
            .seed(1),
    );

    let host_left = Candidate::host((Ipv4Addr::new(1, 1, 1, 1), 1000).into(), "udp").unwrap();
    let host_right = Candidate::host((Ipv4Addr::new(2, 2, 2, 2), 2000).into(), "udp").unwrap();
    left.add_local_candidate(host_left.clone());
    left.add_remote_candidate(host_right.clone());
    right.add_local_candidate(host_right);
    right.add_remote_candidate(host_left);

    let fingerprint_left = left.direct_api().local_dtls_fingerprint().clone();
    let fingerprint_right = right.direct_api().local_dtls_fingerprint().clone();
    left.direct_api().set_remote_fingerprint(fingerprint_right);
    right.direct_api().set_remote_fingerprint(fingerprint_left);

    let credentials_left = left.direct_api().local_ice_credentials();
    let credentials_right = right.direct_api().local_ice_credentials();
    left.direct_api()
        .set_remote_ice_credentials(credentials_right);
    right
        .direct_api()
        .set_remote_ice_credentials(credentials_left);

    left.direct_api().set_ice_controlling(true);
    right.direct_api().set_ice_controlling(false);
    left.direct_api().start_dtls(true).unwrap();
    right.direct_api().start_dtls(false).unwrap();

    // Let the first flight reach the passive peer and its response be dropped.
    let mut setup_steps = 0;
    while left.rtc.last_timeout_reason() != Reason::DTLS {
        progress(&mut left, &mut right).unwrap();
        setup_steps += 1;
        assert!(
            setup_steps < 200,
            "failed to reach a DTLS flight deadline (last reason: {:?})",
            left.rtc.last_timeout_reason()
        );
    }

    // Unlike TestRtc::progress, preserve an immediate timeout exactly. This
    // models a scheduler that runs work scheduled for now without adding a
    // retry delay of its own.
    let mut last_now = left.last;
    let mut terminal_error = None;
    for _ in 0..1_000 {
        left.rtc.handle_input(Input::Timeout(last_now)).unwrap();

        loop {
            match left.rtc.poll_output() {
                Err(RtcError::Dtls(error)) => {
                    terminal_error = Some(error);
                    break;
                }
                Err(error) => panic!("unexpected RTC error: {error}"),
                Ok(Output::Timeout(timeout)) => {
                    last_now = timeout;
                    break;
                }
                Ok(Output::Transmit(_)) => {
                    // Drop each retransmitted DTLS flight.
                }
                Ok(Output::Event(_)) => {
                    // ICE may report its disconnected state before DTLS gives up.
                }
            }
        }

        if terminal_error.is_some() {
            break;
        }
    }

    assert!(
        terminal_error.is_some(),
        "terminal dimpl timeout was not returned within the bounded scheduler run"
    );
    assert!(
        !left.rtc.is_alive(),
        "terminal DTLS error must close the RTC"
    );

    // After the error, callers that continue polling and feeding timeout input
    // must not receive another due timeout, packet, or event.
    let terminal_last_now = last_now;
    for _ in 0..16 {
        left.rtc
            .handle_input(Input::Timeout(terminal_last_now))
            .unwrap();

        match left.rtc.poll_output().unwrap() {
            Output::Timeout(timeout) => {
                assert!(
                    timeout > terminal_last_now,
                    "closed RTC returned a timeout at or before its last input"
                );
            }
            Output::Transmit(_) => panic!("closed RTC produced a DTLS transmission"),
            Output::Event(event) => panic!("closed RTC produced an event: {event:?}"),
        }
    }
}

/// End-to-end: a DTLS handshake between two peers must complete even
/// when the link drops packets. Before the fix, dropped handshake
/// packets stalled for seconds at a time because each lost flight's
/// retransmit was deferred to the next unrelated wake-up — on a lossy
/// link the handshake could miss its connect deadline entirely.
#[test]
fn dtls_handshake_completes_under_packet_loss() {
    init_crypto_default();

    let now = Instant::now();
    let mut l = TestRtc::new_with_rtc(info_span!("L"), Rtc::new(now));
    let mut r = TestRtc::new_with_rtc(info_span!("R"), Rtc::new(now));

    // 30% random loss in both directions from the very first packet.
    let lossy = NetemConfig::new()
        .loss(RandomLoss::new(Probability::new(0.3)))
        .seed(1);
    l.set_netem(lossy);
    r.set_netem(lossy);

    let host_l = Candidate::host((Ipv4Addr::new(1, 1, 1, 1), 1000).into(), "udp").unwrap();
    let host_r = Candidate::host((Ipv4Addr::new(2, 2, 2, 2), 2000).into(), "udp").unwrap();
    l.add_local_candidate(host_l.clone());
    l.add_remote_candidate(host_r.clone());
    r.add_local_candidate(host_r);
    r.add_remote_candidate(host_l);

    let finger_l = l.direct_api().local_dtls_fingerprint().clone();
    let finger_r = r.direct_api().local_dtls_fingerprint().clone();
    l.direct_api().set_remote_fingerprint(finger_r);
    r.direct_api().set_remote_fingerprint(finger_l);

    let creds_l = l.direct_api().local_ice_credentials();
    let creds_r = r.direct_api().local_ice_credentials();
    l.direct_api().set_remote_ice_credentials(creds_r);
    r.direct_api().set_remote_ice_credentials(creds_l);

    l.direct_api().set_ice_controlling(true);
    r.direct_api().set_ice_controlling(false);

    l.direct_api().start_dtls(true).unwrap();
    r.direct_api().start_dtls(false).unwrap();
    l.direct_api().start_sctp(true);
    r.direct_api().start_sctp(false);

    // Drive both sides forward. The DTLS default connect timeout is 10s,
    // so a bounded iteration cap that allows well past that is enough to
    // tell whether the handshake is making progress at all.
    let mut iterations = 0;
    while !(l.is_connected() && r.is_connected()) {
        progress(&mut l, &mut r).unwrap();
        iterations += 1;
        assert!(
            iterations < 5000,
            "DTLS handshake did not complete under 30% packet loss \
             (l_connected={}, r_connected={})",
            l.is_connected(),
            r.is_connected()
        );
    }
}
