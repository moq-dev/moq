//! Cross-version matrix tests for DTLS auto-sense.
//!
//! Tests every combination of (client_version × server_version × mtu) to
//! verify that auto-sense, explicit 1.2, and explicit 1.3 all interoperate
//! correctly, including with fragmented ClientHellos.
//!
//! Matrix (expected outcome):
//!
//! | Client \ Server |  auto  |  1.2   |  1.3   |
//! |-----------------|--------|--------|--------|
//! | auto            |  1.3   |  1.2   |  1.3   |
//! | 1.2             |  1.2   |  1.2   |  FAIL  |
//! | 1.3             |  1.3   |  FAIL  |  1.3   |
//!
//! Each passing combination is tested at normal MTU and at small MTU (200)
//! to exercise fragmented handshake messages.

#![cfg(feature = "rcgen")]

use std::sync::Arc;
use std::time::{Duration, Instant};

use dimpl::certificate::generate_self_signed_certificate;
use dimpl::{Config, Dtls, ProtocolVersion, SrtpProfile};

use crate::common::*;

// ── Helpers ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
enum Ver {
    Auto,
    V12,
    V13,
}

fn make_endpoint(ver: Ver, config: Arc<Config>, active: bool) -> Dtls {
    let cert = generate_self_signed_certificate().unwrap();
    let now = Instant::now();
    let mut d = match ver {
        Ver::Auto => Dtls::new_auto(config, cert, now),
        Ver::V12 => Dtls::new_12(config, cert, now),
        Ver::V13 => Dtls::new_13(config, cert, now),
    };
    if active {
        d.set_active(true);
    }
    d
}

fn cfg(mtu: usize) -> Arc<Config> {
    Arc::new(Config::builder().mtu(mtu).build().unwrap())
}

/// Drive a handshake to completion. Returns `None` if it fails to connect
/// within the iteration limit (expected for incompatible combinations).
fn try_handshake(
    client: &mut Dtls,
    server: &mut Dtls,
) -> Option<(ProtocolVersion, ProtocolVersion)> {
    let mut now = Instant::now();
    let mut cc = false;
    let mut sc = false;

    for _ in 0..80 {
        if client.handle_timeout(now).is_err() {
            return None;
        }
        if server.handle_timeout(now).is_err() {
            return None;
        }

        let co = drain_outputs(client);
        let so = drain_outputs(server);

        if co.connected {
            cc = true;
        }
        if so.connected {
            sc = true;
        }

        // Deliver packets, ignoring errors (incompatible version → errors are expected)
        for p in &co.packets {
            let _ = server.handle_packet(p);
        }
        for p in &so.packets {
            let _ = client.handle_packet(p);
        }

        if cc && sc {
            return Some((
                client.protocol_version().unwrap(),
                server.protocol_version().unwrap(),
            ));
        }

        now += Duration::from_millis(10);
    }

    None
}

/// Helper: successful handshake + verify versions + data exchange.
fn assert_connects(client_ver: Ver, server_ver: Ver, mtu: usize, expected_proto: ProtocolVersion) {
    let _ = env_logger::try_init();

    let client_cfg = cfg(mtu);
    let server_cfg = cfg(mtu);

    let mut client = make_endpoint(client_ver, client_cfg, true);
    let mut server = make_endpoint(server_ver, server_cfg, false);

    let result = try_handshake(&mut client, &mut server);
    let (cv, sv) = result.unwrap_or_else(|| {
        panic!(
            "{:?} client (mtu={}) → {:?} server should connect as {:?}",
            client_ver, mtu, server_ver, expected_proto
        )
    });

    assert_eq!(
        cv, expected_proto,
        "{:?} client version mismatch (mtu={})",
        client_ver, mtu
    );
    assert_eq!(
        sv, expected_proto,
        "{:?} server version mismatch (mtu={})",
        server_ver, mtu
    );

    // Verify bidirectional data exchange.
    let msg_c = b"from client";
    let msg_s = b"from server";
    let mut now = Instant::now() + Duration::from_millis(500);

    client.send_application_data(msg_c).unwrap();
    server.send_application_data(msg_s).unwrap();

    for _ in 0..20 {
        client.handle_timeout(now).unwrap();
        server.handle_timeout(now).unwrap();

        let co = drain_outputs(&mut client);
        let so = drain_outputs(&mut server);

        deliver_packets(&co.packets, &mut server);
        deliver_packets(&so.packets, &mut client);

        let co2 = drain_outputs(&mut client);
        let so2 = drain_outputs(&mut server);

        if so2.app_data.iter().any(|d| d == msg_c) && co2.app_data.iter().any(|d| d == msg_s) {
            return; // success
        }

        now += Duration::from_millis(10);
    }

    panic!(
        "{:?} client → {:?} server (mtu={}): data exchange failed",
        client_ver, server_ver, mtu
    );
}

/// Helper: verify that an incompatible combination does NOT connect.
fn assert_fails(client_ver: Ver, server_ver: Ver, mtu: usize) {
    let _ = env_logger::try_init();

    let client_cfg = cfg(mtu);
    let server_cfg = cfg(mtu);

    let mut client = make_endpoint(client_ver, client_cfg, true);
    let mut server = make_endpoint(server_ver, server_cfg, false);

    let result = try_handshake(&mut client, &mut server);
    assert!(
        result.is_none(),
        "{:?} client (mtu={}) → {:?} server should NOT connect, but got {:?}",
        client_ver,
        mtu,
        server_ver,
        result
    );
}

// ── Normal MTU (1150) ──────────────────────────────────────────────────

const NORMAL: usize = 1150;

// auto × auto → 1.3

#[test]
fn cross_auto_auto_normal() {
    assert_connects(Ver::Auto, Ver::Auto, NORMAL, ProtocolVersion::DTLS1_3);
}

// auto × 1.2 → 1.2

#[test]
fn cross_auto_v12_normal() {
    assert_connects(Ver::Auto, Ver::V12, NORMAL, ProtocolVersion::DTLS1_2);
}

// auto × 1.3 → 1.3

#[test]
fn cross_auto_v13_normal() {
    assert_connects(Ver::Auto, Ver::V13, NORMAL, ProtocolVersion::DTLS1_3);
}

// 1.2 × auto → 1.2

#[test]
fn cross_v12_auto_normal() {
    assert_connects(Ver::V12, Ver::Auto, NORMAL, ProtocolVersion::DTLS1_2);
}

// 1.2 × 1.2 → 1.2

#[test]
fn cross_v12_v12_normal() {
    assert_connects(Ver::V12, Ver::V12, NORMAL, ProtocolVersion::DTLS1_2);
}

// 1.2 × 1.3 → FAIL

#[test]
fn cross_v12_v13_normal() {
    assert_fails(Ver::V12, Ver::V13, NORMAL);
}

// 1.3 × auto → 1.3

#[test]
fn cross_v13_auto_normal() {
    assert_connects(Ver::V13, Ver::Auto, NORMAL, ProtocolVersion::DTLS1_3);
}

// 1.3 × 1.2 → FAIL

#[test]
fn cross_v13_v12_normal() {
    assert_fails(Ver::V13, Ver::V12, NORMAL);
}

// 1.3 × 1.3 → 1.3

#[test]
fn cross_v13_v13_normal() {
    assert_connects(Ver::V13, Ver::V13, NORMAL, ProtocolVersion::DTLS1_3);
}

// ── Small MTU (200) — fragmented ClientHello ───────────────────────────

const FRAG: usize = 200;

// auto × auto → 1.3

#[test]
fn cross_auto_auto_frag() {
    assert_connects(Ver::Auto, Ver::Auto, FRAG, ProtocolVersion::DTLS1_3);
}

// auto × 1.2 → 1.2

#[test]
fn cross_auto_v12_frag() {
    assert_connects(Ver::Auto, Ver::V12, FRAG, ProtocolVersion::DTLS1_2);
}

// auto × 1.3 → 1.3

#[test]
fn cross_auto_v13_frag() {
    assert_connects(Ver::Auto, Ver::V13, FRAG, ProtocolVersion::DTLS1_3);
}

// 1.2 × auto → 1.2

#[test]
fn cross_v12_auto_frag() {
    assert_connects(Ver::V12, Ver::Auto, FRAG, ProtocolVersion::DTLS1_2);
}

// 1.2 × 1.2 → 1.2

#[test]
fn cross_v12_v12_frag() {
    assert_connects(Ver::V12, Ver::V12, FRAG, ProtocolVersion::DTLS1_2);
}

// 1.2 × 1.3 → FAIL

#[test]
fn cross_v12_v13_frag() {
    assert_fails(Ver::V12, Ver::V13, FRAG);
}

// 1.3 × auto → 1.3

#[test]
fn cross_v13_auto_frag() {
    assert_connects(Ver::V13, Ver::Auto, FRAG, ProtocolVersion::DTLS1_3);
}

// 1.3 × 1.2 → FAIL

#[test]
fn cross_v13_v12_frag() {
    assert_fails(Ver::V13, Ver::V12, FRAG);
}

// 1.3 × 1.3 → 1.3

#[test]
fn cross_v13_v13_frag() {
    assert_connects(Ver::V13, Ver::V13, FRAG, ProtocolVersion::DTLS1_3);
}

// ── Very small MTU (100) — heavy fragmentation ────────────────────────

const HEAVY: usize = 150;

#[test]
fn cross_auto_auto_heavy() {
    assert_connects(Ver::Auto, Ver::Auto, HEAVY, ProtocolVersion::DTLS1_3);
}

#[test]
fn cross_v13_auto_heavy() {
    assert_connects(Ver::V13, Ver::Auto, HEAVY, ProtocolVersion::DTLS1_3);
}

#[test]
fn cross_v13_v13_heavy() {
    assert_connects(Ver::V13, Ver::V13, HEAVY, ProtocolVersion::DTLS1_3);
}

#[test]
fn cross_v12_auto_heavy() {
    assert_connects(Ver::V12, Ver::Auto, HEAVY, ProtocolVersion::DTLS1_2);
}

#[test]
fn cross_v12_v12_heavy() {
    assert_connects(Ver::V12, Ver::V12, HEAVY, ProtocolVersion::DTLS1_2);
}

#[test]
fn cross_auto_v13_heavy() {
    assert_connects(Ver::Auto, Ver::V13, HEAVY, ProtocolVersion::DTLS1_3);
}

#[test]
fn cross_auto_v12_heavy() {
    assert_connects(Ver::Auto, Ver::V12, HEAVY, ProtocolVersion::DTLS1_2);
}

// ── Keying material tests (fragmented) ─────────────────────────────────

fn assert_keying_material(client_ver: Ver, server_ver: Ver, mtu: usize) {
    let _ = env_logger::try_init();

    let client_cfg = cfg(mtu);
    let server_cfg = cfg(mtu);

    let mut client = make_endpoint(client_ver, client_cfg, true);
    let mut server = make_endpoint(server_ver, server_cfg, false);

    let mut now = Instant::now();
    let mut client_km: Option<(Vec<u8>, SrtpProfile)> = None;
    let mut server_km: Option<(Vec<u8>, SrtpProfile)> = None;

    for _ in 0..80 {
        client.handle_timeout(now).unwrap();
        server.handle_timeout(now).unwrap();

        let co = drain_outputs(&mut client);
        let so = drain_outputs(&mut server);

        if let Some(km) = co.keying_material {
            client_km = Some(km);
        }
        if let Some(km) = so.keying_material {
            server_km = Some(km);
        }

        deliver_packets(&co.packets, &mut server);
        deliver_packets(&so.packets, &mut client);

        if client_km.is_some() && server_km.is_some() {
            break;
        }

        now += Duration::from_millis(10);
    }

    let ckm = client_km.expect("Client should have keying material");
    let skm = server_km.expect("Server should have keying material");

    assert_eq!(ckm.0, skm.0, "Keying material should match");
    assert_eq!(ckm.1, skm.1, "SRTP profile should match");
    assert!(!ckm.0.is_empty());
}

#[test]
fn keying_auto_auto_frag() {
    assert_keying_material(Ver::Auto, Ver::Auto, FRAG);
}

#[test]
fn keying_v13_auto_frag() {
    assert_keying_material(Ver::V13, Ver::Auto, FRAG);
}

#[test]
fn keying_v12_auto_frag() {
    assert_keying_material(Ver::V12, Ver::Auto, FRAG);
}

#[test]
fn keying_auto_v13_frag() {
    assert_keying_material(Ver::Auto, Ver::V13, FRAG);
}

#[test]
fn keying_auto_v12_frag() {
    assert_keying_material(Ver::Auto, Ver::V12, FRAG);
}

#[test]
fn keying_v13_v13_frag() {
    assert_keying_material(Ver::V13, Ver::V13, FRAG);
}

#[test]
fn keying_v12_v12_frag() {
    assert_keying_material(Ver::V12, Ver::V12, FRAG);
}
