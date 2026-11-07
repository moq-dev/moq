//! Auto-sense server fallback tests.
//!
//! Tests the `Dtls::new_auto()` server path where the server starts as
//! DTLS 1.3 and falls back to DTLS 1.2 when the client doesn't offer 1.3.
//! Also tests that DTLS 1.3 clients connect without fallback.

use std::sync::Arc;
use std::time::{Duration, Instant};

use dimpl::{Config, Dtls, Output, ProtocolVersion, PskResolver};

use crate::common::*;

/// Helper: run a handshake loop between client and server, return
/// (client_connected, server_connected, client_version, server_version).
fn run_handshake(
    client: &mut Dtls,
    server: &mut Dtls,
) -> (bool, bool, Option<ProtocolVersion>, Option<ProtocolVersion>) {
    let mut now = Instant::now();
    let mut client_connected = false;
    let mut server_connected = false;

    for _ in 0..80 {
        client.handle_timeout(now).expect("client timeout");
        server.handle_timeout(now).expect("server timeout");

        let client_out = drain_outputs(client);
        let server_out = drain_outputs(server);

        if client_out.connected {
            client_connected = true;
        }
        if server_out.connected {
            server_connected = true;
        }

        deliver_packets(&client_out.packets, server);
        deliver_packets(&server_out.packets, client);

        if client_connected && server_connected {
            break;
        }

        now += Duration::from_millis(10);
    }

    (
        client_connected,
        server_connected,
        client.protocol_version(),
        server.protocol_version(),
    )
}

// ============================================================================
// Auto server + explicit DTLS 1.3 client → DTLS 1.3 (no fallback)
// ============================================================================

#[test]
#[cfg(feature = "rcgen")]
fn auto_server_with_dtls13_client() {
    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().unwrap();
    let server_cert = generate_self_signed_certificate().unwrap();
    let config = default_config();

    let mut client = Dtls::new_13(Arc::clone(&config), client_cert, Instant::now());
    client.set_active(true);

    let mut server = Dtls::new_auto(config, server_cert, Instant::now());

    let (cc, sc, cv, sv) = run_handshake(&mut client, &mut server);

    assert!(cc, "Client should connect");
    assert!(sc, "Server should connect");
    assert_eq!(cv, Some(ProtocolVersion::DTLS1_3));
    assert_eq!(sv, Some(ProtocolVersion::DTLS1_3));
}

// ============================================================================
// Auto server + explicit DTLS 1.2 client → DTLS 1.2 (fallback)
// ============================================================================

#[test]
#[cfg(feature = "rcgen")]
fn auto_server_send_application_data_pending() {
    use dimpl::certificate::generate_self_signed_certificate;

    let client_cert = generate_self_signed_certificate().unwrap();
    let server_cert = generate_self_signed_certificate().unwrap();
    let config = default_config();

    let mut client = Dtls::new_12(Arc::clone(&config), client_cert, Instant::now());
    client.set_active(true);

    let mut server = Dtls::new_auto(config, server_cert, Instant::now());

    assert_eq!(
        server.send_application_data(b"early auto-server data"),
        Ok(0)
    );

    let (cc, sc, cv, sv) = run_handshake(&mut client, &mut server);

    assert!(cc, "Client should connect after pending send");
    assert!(sc, "Server should connect after pending send");
    assert_eq!(cv, Some(ProtocolVersion::DTLS1_2));
    assert_eq!(sv, Some(ProtocolVersion::DTLS1_2));
}

#[test]
#[cfg(feature = "rcgen")]
fn auto_server_protocol_version_pending() {
    use dimpl::certificate::generate_self_signed_certificate;

    let client_cert = generate_self_signed_certificate().unwrap();
    let server_cert = generate_self_signed_certificate().unwrap();
    let config = default_config();

    let mut client = Dtls::new_12(Arc::clone(&config), client_cert, Instant::now());
    client.set_active(true);

    let mut server = Dtls::new_auto(config, server_cert, Instant::now());

    assert_eq!(server.protocol_version(), None);

    let (cc, sc, cv, sv) = run_handshake(&mut client, &mut server);

    assert!(cc, "Client should connect after pending protocol check");
    assert!(sc, "Server should connect after pending protocol check");
    assert_eq!(cv, Some(ProtocolVersion::DTLS1_2));
    assert_eq!(sv, Some(ProtocolVersion::DTLS1_2));
}

#[test]
#[cfg(feature = "rcgen")]
fn auto_server_with_dtls12_client() {
    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().unwrap();
    let server_cert = generate_self_signed_certificate().unwrap();
    let config = default_config();

    let mut client = Dtls::new_12(Arc::clone(&config), client_cert, Instant::now());
    client.set_active(true);

    let mut server = Dtls::new_auto(config, server_cert, Instant::now());

    let (cc, sc, cv, sv) = run_handshake(&mut client, &mut server);

    assert!(cc, "Client should connect");
    assert!(sc, "Server should connect");
    assert_eq!(cv, Some(ProtocolVersion::DTLS1_2));
    assert_eq!(sv, Some(ProtocolVersion::DTLS1_2));
}

// ============================================================================
// Auto server + auto client → DTLS 1.3
// ============================================================================

#[test]
#[cfg(feature = "rcgen")]
fn auto_server_with_auto_client() {
    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().unwrap();
    let server_cert = generate_self_signed_certificate().unwrap();
    let config = default_config();

    let mut client = Dtls::new_auto(Arc::clone(&config), client_cert, Instant::now());
    client.set_active(true);

    let mut server = Dtls::new_auto(config, server_cert, Instant::now());

    let (cc, sc, cv, sv) = run_handshake(&mut client, &mut server);

    assert!(cc, "Client should connect");
    assert!(sc, "Server should connect");
    // Both auto: should negotiate DTLS 1.3
    assert_eq!(cv, Some(ProtocolVersion::DTLS1_3));
    assert_eq!(sv, Some(ProtocolVersion::DTLS1_3));
}

// ============================================================================
// Auto server + DTLS 1.2 client (no cookie) → fallback
// ============================================================================

#[test]
#[cfg(feature = "rcgen")]
fn auto_server_with_dtls12_client_no_cookie() {
    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().unwrap();
    let server_cert = generate_self_signed_certificate().unwrap();
    let client_config = default_config();
    let server_config = no_cookie_config();

    let mut client = Dtls::new_12(client_config, client_cert, Instant::now());
    client.set_active(true);

    let mut server = Dtls::new_auto(server_config, server_cert, Instant::now());

    let (cc, sc, cv, sv) = run_handshake(&mut client, &mut server);

    assert!(cc, "Client should connect");
    assert!(sc, "Server should connect");
    assert_eq!(cv, Some(ProtocolVersion::DTLS1_2));
    assert_eq!(sv, Some(ProtocolVersion::DTLS1_2));
}

// ============================================================================
// Auto server + DTLS 1.3 client (no cookie) → DTLS 1.3
// ============================================================================

#[test]
#[cfg(feature = "rcgen")]
fn auto_server_with_dtls13_client_no_cookie() {
    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().unwrap();
    let server_cert = generate_self_signed_certificate().unwrap();
    let client_config = default_config();
    let server_config = no_cookie_config();

    let mut client = Dtls::new_13(client_config, client_cert, Instant::now());
    client.set_active(true);

    let mut server = Dtls::new_auto(server_config, server_cert, Instant::now());

    let (cc, sc, cv, sv) = run_handshake(&mut client, &mut server);

    assert!(cc, "Client should connect");
    assert!(sc, "Server should connect");
    assert_eq!(cv, Some(ProtocolVersion::DTLS1_3));
    assert_eq!(sv, Some(ProtocolVersion::DTLS1_3));
}

// ============================================================================
// Auto server + DTLS 1.2 client → fallback, then exchange application data
// ============================================================================

#[test]
#[cfg(feature = "rcgen")]
fn auto_server_dtls12_fallback_application_data() {
    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().unwrap();
    let server_cert = generate_self_signed_certificate().unwrap();
    let config = default_config();

    let mut now = Instant::now();

    let mut client = Dtls::new_12(Arc::clone(&config), client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_auto(config, server_cert, now);

    // Complete handshake
    let (cc, sc, _, _) = run_handshake(&mut client, &mut server);
    assert!(cc && sc, "Handshake should complete");

    // Send data client → server
    let msg = b"hello from dtls12 client";
    client.send_application_data(msg).expect("client send");
    now = Instant::now() + Duration::from_millis(100);
    client.handle_timeout(now).expect("client timeout");
    let client_out = drain_outputs(&mut client);
    deliver_packets(&client_out.packets, &mut server);
    server.handle_timeout(now).expect("server timeout");
    let server_out = drain_outputs(&mut server);
    assert!(
        server_out.app_data.iter().any(|d| d == msg),
        "Server should receive client's application data"
    );

    // Send data server → client
    let reply = b"hello from auto server";
    server.send_application_data(reply).expect("server send");
    now += Duration::from_millis(10);
    server.handle_timeout(now).expect("server timeout");
    let server_out = drain_outputs(&mut server);
    deliver_packets(&server_out.packets, &mut client);
    client.handle_timeout(now).expect("client timeout");
    let client_out = drain_outputs(&mut client);
    assert!(
        client_out.app_data.iter().any(|d| d == reply),
        "Client should receive server's application data"
    );
}

// ============================================================================
// Auto server + DTLS 1.3 client → no fallback, exchange application data
// ============================================================================

#[test]
#[cfg(feature = "rcgen")]
fn auto_server_dtls13_application_data() {
    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().unwrap();
    let server_cert = generate_self_signed_certificate().unwrap();
    let config = default_config();

    let mut now = Instant::now();

    let mut client = Dtls::new_13(Arc::clone(&config), client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_auto(config, server_cert, now);

    // Complete handshake
    let (cc, sc, _, _) = run_handshake(&mut client, &mut server);
    assert!(cc && sc, "Handshake should complete");

    // Send data client → server
    let msg = b"hello from dtls13 client";
    client.send_application_data(msg).expect("client send");
    now = Instant::now() + Duration::from_millis(100);
    client.handle_timeout(now).expect("client timeout");
    let client_out = drain_outputs(&mut client);
    deliver_packets(&client_out.packets, &mut server);
    server.handle_timeout(now).expect("server timeout");
    let server_out = drain_outputs(&mut server);
    assert!(
        server_out.app_data.iter().any(|d| d == msg),
        "Server should receive client's application data"
    );

    // Send data server → client
    let reply = b"hello from auto server (1.3)";
    server.send_application_data(reply).expect("server send");
    now += Duration::from_millis(10);
    server.handle_timeout(now).expect("server timeout");
    let server_out = drain_outputs(&mut server);
    deliver_packets(&server_out.packets, &mut client);
    client.handle_timeout(now).expect("client timeout");
    let client_out = drain_outputs(&mut client);
    assert!(
        client_out.app_data.iter().any(|d| d == reply),
        "Client should receive server's application data"
    );
}

// ============================================================================
// Auto server + DTLS 1.2 client → keying material matches
// ============================================================================

#[test]
#[cfg(feature = "rcgen")]
fn auto_server_dtls12_fallback_keying_material() {
    use dimpl::SrtpProfile;
    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().unwrap();
    let server_cert = generate_self_signed_certificate().unwrap();
    let config = default_config();

    let mut now = Instant::now();

    let mut client = Dtls::new_12(Arc::clone(&config), client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_auto(config, server_cert, now);

    let mut client_km: Option<(Vec<u8>, SrtpProfile)> = None;
    let mut server_km: Option<(Vec<u8>, SrtpProfile)> = None;

    for _ in 0..80 {
        client.handle_timeout(now).expect("client timeout");
        server.handle_timeout(now).expect("server timeout");

        let client_out = drain_outputs(&mut client);
        let server_out = drain_outputs(&mut server);

        if let Some(km) = client_out.keying_material {
            client_km = Some(km);
        }
        if let Some(km) = server_out.keying_material {
            server_km = Some(km);
        }

        deliver_packets(&client_out.packets, &mut server);
        deliver_packets(&server_out.packets, &mut client);

        if client_km.is_some() && server_km.is_some() {
            break;
        }

        now += Duration::from_millis(10);
    }

    let client_km = client_km.expect("Client should have keying material");
    let server_km = server_km.expect("Server should have keying material");

    assert_eq!(client_km.0, server_km.0, "Keying material should match");
    assert_eq!(client_km.1, server_km.1, "SRTP profile should match");
    assert!(
        !client_km.0.is_empty(),
        "Keying material should not be empty"
    );
}

// ============================================================================
// Auto server + DTLS 1.3 client → keying material matches
// ============================================================================

#[test]
#[cfg(feature = "rcgen")]
fn auto_server_dtls13_keying_material() {
    use dimpl::SrtpProfile;
    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().unwrap();
    let server_cert = generate_self_signed_certificate().unwrap();
    let config = default_config();

    let mut now = Instant::now();

    let mut client = Dtls::new_13(Arc::clone(&config), client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_auto(config, server_cert, now);

    let mut client_km: Option<(Vec<u8>, SrtpProfile)> = None;
    let mut server_km: Option<(Vec<u8>, SrtpProfile)> = None;

    for _ in 0..80 {
        client.handle_timeout(now).expect("client timeout");
        server.handle_timeout(now).expect("server timeout");

        let client_out = drain_outputs(&mut client);
        let server_out = drain_outputs(&mut server);

        if let Some(km) = client_out.keying_material {
            client_km = Some(km);
        }
        if let Some(km) = server_out.keying_material {
            server_km = Some(km);
        }

        deliver_packets(&client_out.packets, &mut server);
        deliver_packets(&server_out.packets, &mut client);

        if client_km.is_some() && server_km.is_some() {
            break;
        }

        now += Duration::from_millis(10);
    }

    let client_km = client_km.expect("Client should have keying material");
    let server_km = server_km.expect("Server should have keying material");

    assert_eq!(client_km.0, server_km.0, "Keying material should match");
    assert_eq!(client_km.1, server_km.1, "SRTP profile should match");
    assert!(
        !client_km.0.is_empty(),
        "Keying material should not be empty"
    );
}

// ============================================================================
// Auto server set_active(true) creates ClientPending
// ============================================================================

#[test]
#[cfg(feature = "rcgen")]
fn auto_server_set_active_creates_client_pending() {
    use dimpl::certificate::generate_self_signed_certificate;

    let cert = generate_self_signed_certificate().unwrap();
    let config = default_config();

    let mut dtls = Dtls::new_auto(config, cert, Instant::now());
    assert!(!dtls.is_active());

    dtls.set_active(true);
    assert!(dtls.is_active());

    // Should be able to produce a hybrid ClientHello
    dtls.handle_timeout(Instant::now()).unwrap();
    let mut buf = vec![0u8; 2048];
    let output = dtls.poll_output(&mut buf);
    assert!(
        matches!(output, Output::Packet(_)),
        "Should send hybrid ClientHello"
    );
}

// ============================================================================
// Fragmented ClientHello tests — small MTU forces multi-fragment CH
// ============================================================================

/// DTLS 1.3 client with small MTU → fragmented ClientHello → auto server connects as 1.3.
#[test]
#[cfg(feature = "rcgen")]
fn auto_server_with_fragmented_dtls13_client_hello() {
    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().unwrap();
    let server_cert = generate_self_signed_certificate().unwrap();

    // MTU=200 forces the ClientHello to be fragmented across multiple records
    let client_config = small_mtu_config(200);
    let server_config = default_config();

    let mut client = Dtls::new_13(client_config, client_cert, Instant::now());
    client.set_active(true);

    let mut server = Dtls::new_auto(server_config, server_cert, Instant::now());

    let (cc, sc, cv, sv) = run_handshake(&mut client, &mut server);

    assert!(cc, "Client should connect with fragmented CH");
    assert!(sc, "Server should connect with fragmented CH");
    assert_eq!(cv, Some(ProtocolVersion::DTLS1_3));
    assert_eq!(sv, Some(ProtocolVersion::DTLS1_3));
}

/// DTLS 1.3 client with very small MTU (100 bytes) → heavily fragmented CH → auto server 1.3.
#[test]
#[cfg(feature = "rcgen")]
fn auto_server_with_heavily_fragmented_dtls13_client_hello() {
    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().unwrap();
    let server_cert = generate_self_signed_certificate().unwrap();

    // MTU=100 — heavy fragmentation (CH will be ~5-6 fragments)
    let client_config = small_mtu_config(100);
    let server_config = default_config();

    let mut client = Dtls::new_13(client_config, client_cert, Instant::now());
    client.set_active(true);

    let mut server = Dtls::new_auto(server_config, server_cert, Instant::now());

    let (cc, sc, _cv, sv) = run_handshake(&mut client, &mut server);

    assert!(cc, "Client should connect with heavily fragmented CH");
    assert!(sc, "Server should connect with heavily fragmented CH");
    assert_eq!(sv, Some(ProtocolVersion::DTLS1_3));
}

/// Both client and server with small MTU → fragmented CH → auto server 1.3 + data exchange.
#[test]
#[cfg(feature = "rcgen")]
fn auto_server_fragmented_ch_application_data() {
    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().unwrap();
    let server_cert = generate_self_signed_certificate().unwrap();

    let client_config = small_mtu_config(200);
    let server_config = default_config();

    let mut now = Instant::now();
    let mut client = Dtls::new_13(client_config, client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_auto(server_config, server_cert, now);

    // Complete handshake
    let (cc, sc, _, _) = run_handshake(&mut client, &mut server);
    assert!(cc && sc, "Handshake should complete with fragmented CH");

    // Send data client → server
    let msg = b"data after fragmented handshake";
    client.send_application_data(msg).expect("client send");
    now = Instant::now() + Duration::from_millis(100);
    client.handle_timeout(now).expect("client timeout");
    let client_out = drain_outputs(&mut client);
    deliver_packets(&client_out.packets, &mut server);
    server.handle_timeout(now).expect("server timeout");
    let server_out = drain_outputs(&mut server);
    assert!(
        server_out.app_data.iter().any(|d| d == msg),
        "Server should receive data after fragmented CH handshake"
    );
}

/// Fragmented DTLS 1.3 ClientHello with no-cookie config → auto server 1.3.
#[test]
#[cfg(feature = "rcgen")]
fn auto_server_fragmented_ch_no_cookie() {
    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().unwrap();
    let server_cert = generate_self_signed_certificate().unwrap();

    let client_config = small_mtu_config(200);
    let server_config = no_cookie_config();

    let mut client = Dtls::new_13(client_config, client_cert, Instant::now());
    client.set_active(true);

    let mut server = Dtls::new_auto(server_config, server_cert, Instant::now());

    let (cc, sc, _cv, sv) = run_handshake(&mut client, &mut server);

    assert!(cc, "Client should connect with fragmented CH, no cookie");
    assert!(sc, "Server should connect with fragmented CH, no cookie");
    assert_eq!(sv, Some(ProtocolVersion::DTLS1_3));
}

// ============================================================================
// Auto server + DTLS 1.2 PSK client → fallback picks PSK-mode Server12
// ============================================================================

/// Regression for https://github.com/algesten/dimpl/issues/100 — a
/// `Dtls::new_auto` server configured with `with_psk_server` must accept a
/// DTLS 1.2 PSK client. Before the fix the fallback always constructed a
/// certificate-auth Server12 and failed with "No mutually acceptable cipher
/// suite".
#[test]
#[cfg(feature = "rcgen")]
fn auto_server_psk_fallback_with_dtls12_psk_client() {
    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    struct FixedPsk;
    impl PskResolver for FixedPsk {
        fn resolve(&self, _identity: &[u8]) -> Option<Vec<u8>> {
            Some(b"0123456789abcdef".to_vec())
        }
    }

    let server_cert = generate_self_signed_certificate().unwrap();

    let client_config = Arc::new(
        Config::builder()
            .with_psk_client(b"test-device".to_vec(), Arc::new(FixedPsk))
            .build()
            .expect("build PSK client config"),
    );
    let server_config = Arc::new(
        Config::builder()
            .with_psk_server(Some(b"hint".to_vec()), Arc::new(FixedPsk))
            .build()
            .expect("build PSK server config"),
    );

    let mut client = Dtls::new_12_psk(client_config, Instant::now());
    client.set_active(true);

    let mut server = Dtls::new_auto(server_config, server_cert, Instant::now());

    let (cc, sc, cv, sv) = run_handshake(&mut client, &mut server);

    assert!(cc, "PSK client should connect after auto-server fallback");
    assert!(sc, "Auto server should connect to DTLS 1.2 PSK client");
    assert_eq!(cv, Some(ProtocolVersion::DTLS1_2));
    assert_eq!(sv, Some(ProtocolVersion::DTLS1_2));
}

/// Fragmented DTLS 1.3 ClientHello → keying material matches between client and auto server.
#[test]
#[cfg(feature = "rcgen")]
fn auto_server_fragmented_ch_keying_material() {
    use dimpl::SrtpProfile;
    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().unwrap();
    let server_cert = generate_self_signed_certificate().unwrap();

    let client_config = small_mtu_config(200);
    let server_config = default_config();

    let mut now = Instant::now();
    let mut client = Dtls::new_13(client_config, client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_auto(server_config, server_cert, now);

    let mut client_km: Option<(Vec<u8>, SrtpProfile)> = None;
    let mut server_km: Option<(Vec<u8>, SrtpProfile)> = None;

    for _ in 0..80 {
        client.handle_timeout(now).expect("client timeout");
        server.handle_timeout(now).expect("server timeout");

        let client_out = drain_outputs(&mut client);
        let server_out = drain_outputs(&mut server);

        if let Some(km) = client_out.keying_material {
            client_km = Some(km);
        }
        if let Some(km) = server_out.keying_material {
            server_km = Some(km);
        }

        deliver_packets(&client_out.packets, &mut server);
        deliver_packets(&server_out.packets, &mut client);

        if client_km.is_some() && server_km.is_some() {
            break;
        }

        now += Duration::from_millis(10);
    }

    let client_km = client_km.expect("Client should have keying material");
    let server_km = server_km.expect("Server should have keying material");

    assert_eq!(client_km.0, server_km.0, "Keying material should match");
    assert_eq!(client_km.1, server_km.1, "SRTP profile should match");
    assert!(
        !client_km.0.is_empty(),
        "Keying material should not be empty"
    );
}
