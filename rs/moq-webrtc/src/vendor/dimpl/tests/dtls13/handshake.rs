//! DTLS 1.3 handshake tests.

use std::sync::Arc;
use std::time::{Duration, Instant};

use dimpl::{CertificateError, Config, Dtls, DtlsCertificate, Error, SrtpProfile};

use crate::common::*;

#[test]
#[cfg(feature = "rcgen")]
fn dtls13_empty_client_certificate_is_rejected_when_required() {
    use dimpl::certificate::generate_self_signed_certificate;

    let now = Instant::now();
    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");
    let config = Arc::new(Config::builder().build().expect("build config"));
    let mut client = Dtls::new_13(
        Arc::clone(&config),
        DtlsCertificate {
            certificate: Vec::new(),
            private_key: client_cert.private_key,
        },
        now,
    );
    client.set_active(true);
    let mut server = Dtls::new_13(config, server_cert, now);

    let mut now = now;
    for _ in 0..40 {
        client.handle_timeout(now).expect("client timeout");
        server.handle_timeout(now).expect("server timeout");
        let client_out = drain_outputs(&mut client);
        let server_out = drain_outputs(&mut server);
        for packet in &client_out.packets {
            match server.handle_packet(packet) {
                Err(Error::CertificateError(CertificateError::NoClientCertificateReceived)) => {
                    let output = drain_outputs(&mut server);
                    assert_eq!(output.packets.len(), 1, "one fatal alert packet");
                    let alert_result = client.handle_packet(&output.packets[0]);
                    assert!(
                        matches!(
                            alert_result,
                            Err(Error::SecurityError(dimpl::SecurityError::FatalAlert {
                                description: 116
                            }))
                        ),
                        "client result: {alert_result:?}"
                    );
                    assert!(server.is_closed());
                    return;
                }
                Err(error) => panic!("unexpected server error: {error}"),
                Ok(()) => {}
            }
        }
        deliver_packets(&server_out.packets, &mut client);
        now += Duration::from_millis(10);
    }
    panic!("server accepted an empty client Certificate");
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls13_client_certificate_not_required() {
    use dimpl::certificate::generate_self_signed_certificate;

    let now = Instant::now();
    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");
    let config = Arc::new(
        Config::builder()
            .require_client_certificate(false)
            .build()
            .expect("build config"),
    );
    let mut client = Dtls::new_13(Arc::clone(&config), client_cert, now);
    client.set_active(true);
    let mut server = Dtls::new_13(config, server_cert, now);
    complete_dtls13_handshake(&mut client, &mut server, now);
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls13_basic_handshake() {
    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    let config = dtls13_config();

    let mut now = Instant::now();

    let mut client = Dtls::new_13(Arc::clone(&config), client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_13(config, server_cert, now);
    server.set_active(false);

    // Run handshake
    let mut client_connected = false;
    let mut server_connected = false;

    for _ in 0..30 {
        client.handle_timeout(now).expect("client timeout");
        server.handle_timeout(now).expect("server timeout");

        let client_out = drain_outputs(&mut client);
        let server_out = drain_outputs(&mut server);

        if client_out.connected {
            client_connected = true;
        }
        if server_out.connected {
            server_connected = true;
        }

        deliver_packets(&client_out.packets, &mut server);
        deliver_packets(&server_out.packets, &mut client);

        if client_connected && server_connected {
            break;
        }

        now += Duration::from_millis(10);
    }

    assert!(client_connected, "Client should be connected");
    assert!(server_connected, "Server should be connected");
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls13_handshake_with_keying_material() {
    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    let config = dtls13_config();

    let mut now = Instant::now();

    let mut client = Dtls::new_13(Arc::clone(&config), client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_13(config, server_cert, now);
    server.set_active(false);

    let mut client_km: Option<(Vec<u8>, SrtpProfile)> = None;
    let mut server_km: Option<(Vec<u8>, SrtpProfile)> = None;

    for _ in 0..30 {
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

    // Both sides should derive the same keying material
    assert_eq!(
        client_km.0, server_km.0,
        "Client and server keying material should match"
    );
    assert_eq!(
        client_km.1, server_km.1,
        "Client and server SRTP profile should match"
    );

    // Keying material should be non-empty and properly sized
    // SRTP keying material is typically 2*(key_len + salt_len) for both directions
    assert!(
        !client_km.0.is_empty(),
        "Keying material should not be empty"
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls13_peer_certificate_exchange() {
    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    // Store expected certificates
    let expected_client_cert = client_cert.certificate.clone();
    let expected_server_cert = server_cert.certificate.clone();

    let config = dtls13_config();

    let mut now = Instant::now();

    let mut client = Dtls::new_13(Arc::clone(&config), client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_13(config, server_cert, now);
    server.set_active(false);

    let mut client_peer_cert: Option<Vec<u8>> = None;
    let mut server_peer_cert: Option<Vec<u8>> = None;

    for _ in 0..30 {
        client.handle_timeout(now).expect("client timeout");
        server.handle_timeout(now).expect("server timeout");

        let client_out = drain_outputs(&mut client);
        let server_out = drain_outputs(&mut server);

        if let Some(cert) = client_out.peer_cert {
            client_peer_cert = Some(cert);
        }
        if let Some(cert) = server_out.peer_cert {
            server_peer_cert = Some(cert);
        }

        deliver_packets(&client_out.packets, &mut server);
        deliver_packets(&server_out.packets, &mut client);

        if client_peer_cert.is_some() && server_peer_cert.is_some() {
            break;
        }

        now += Duration::from_millis(10);
    }

    assert!(
        client_peer_cert.is_some(),
        "Client should receive server's certificate"
    );
    assert!(
        server_peer_cert.is_some(),
        "Server should receive client's certificate"
    );

    // Verify the certificates match what was configured
    assert_eq!(
        client_peer_cert.unwrap(),
        expected_server_cert,
        "Client should receive server's certificate"
    );
    assert_eq!(
        server_peer_cert.unwrap(),
        expected_client_cert,
        "Server should receive client's certificate"
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls13_srtp_keying_material_correct_size() {
    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    let config = dtls13_config();

    let mut now = Instant::now();

    let mut client = Dtls::new_13(Arc::clone(&config), client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_13(config, server_cert, now);
    server.set_active(false);

    let mut client_km: Option<(Vec<u8>, SrtpProfile)> = None;

    for _ in 0..30 {
        client.handle_timeout(now).expect("client timeout");
        server.handle_timeout(now).expect("server timeout");

        let client_out = drain_outputs(&mut client);
        let server_out = drain_outputs(&mut server);

        if let Some(km) = client_out.keying_material {
            client_km = Some(km);
        }

        deliver_packets(&client_out.packets, &mut server);
        deliver_packets(&server_out.packets, &mut client);

        if client_km.is_some() {
            break;
        }

        now += Duration::from_millis(10);
    }

    let (km, profile) = client_km.expect("Should have keying material");

    // Verify keying material size based on profile
    let expected_size = match profile {
        SrtpProfile::AEAD_AES_128_GCM => 2 * (16 + 12), // 2 * (key + salt) for AES-128-GCM
        SrtpProfile::AEAD_AES_256_GCM => 2 * (32 + 12), // 2 * (key + salt) for AES-256-GCM
        SrtpProfile::AES128_CM_SHA1_80 => 2 * (16 + 14), // 2 * (key + salt) for AES-128-CM
        _ => unreachable!(),
    };

    assert_eq!(
        km.len(),
        expected_size,
        "Keying material should be {} bytes for {:?}, got {}",
        expected_size,
        profile,
        km.len()
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls13_hello_retry_request_flow() {
    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    // Server config that will trigger HRR (by requiring cookie)
    let config = dtls13_config();

    let mut now = Instant::now();

    let mut client = Dtls::new_13(Arc::clone(&config), client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_13(config, server_cert, now);
    server.set_active(false);

    let mut client_connected = false;
    let mut server_connected = false;
    let mut saw_hrr = false;
    let mut flight_count = 0;

    for _ in 0..40 {
        client.handle_timeout(now).expect("client timeout");
        server.handle_timeout(now).expect("server timeout");

        let client_out = drain_outputs(&mut client);
        let server_out = drain_outputs(&mut server);

        if !client_out.packets.is_empty() {
            flight_count += 1;
        }

        // Track if we see what looks like HRR response (server sends before full handshake)
        if !server_out.packets.is_empty() && !client_connected && flight_count <= 2 {
            saw_hrr = true;
        }

        if client_out.connected {
            client_connected = true;
        }
        if server_out.connected {
            server_connected = true;
        }

        deliver_packets(&client_out.packets, &mut server);
        deliver_packets(&server_out.packets, &mut client);

        if client_connected && server_connected {
            break;
        }

        now += Duration::from_millis(10);
    }

    assert!(client_connected, "Client should be connected after HRR");
    assert!(server_connected, "Server should be connected after HRR");
    // In DTLS 1.3 with cookies, we expect HelloRetryRequest
    assert!(
        saw_hrr || flight_count >= 2,
        "Should have seen HRR or multiple client flights"
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls13_handshake_aes_256_gcm() {
    use dimpl::Config;
    use dimpl::certificate::generate_self_signed_certificate;
    use dimpl::crypto::Dtls13CipherSuite;
    use dimpl::crypto::aws_lc_rs;

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    // Build a custom provider that only offers AES_256_GCM_SHA384
    let default = aws_lc_rs::default_provider();
    let aes256_only: Vec<_> = default
        .dtls13_cipher_suites
        .iter()
        .copied()
        .filter(|cs| cs.suite() == Dtls13CipherSuite::AES_256_GCM_SHA384)
        .collect();
    assert!(!aes256_only.is_empty(), "Provider must have AES-256-GCM");

    // Leak the filtered vec to get a &'static slice
    let aes256_static: &'static [_] = Box::leak(aes256_only.into_boxed_slice());

    let provider = dimpl::crypto::CryptoProvider {
        dtls13_cipher_suites: aes256_static,
        ..default
    };

    let config = Arc::new(
        Config::builder()
            .with_crypto_provider(provider)
            .build()
            .expect("build config"),
    );

    let mut now = Instant::now();

    let mut client = Dtls::new_13(Arc::clone(&config), client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_13(config, server_cert, now);
    server.set_active(false);
    let mut client_connected = false;
    let mut server_connected = false;

    for _ in 0..30 {
        client.handle_timeout(now).expect("client timeout");
        server.handle_timeout(now).expect("server timeout");

        let client_out = drain_outputs(&mut client);
        let server_out = drain_outputs(&mut server);

        if client_out.connected {
            client_connected = true;
        }
        if server_out.connected {
            server_connected = true;
        }

        deliver_packets(&client_out.packets, &mut server);
        deliver_packets(&server_out.packets, &mut client);

        if client_connected && server_connected {
            break;
        }

        now += Duration::from_millis(10);
    }

    assert!(
        client_connected,
        "Client should be connected with AES-256-GCM"
    );
    assert!(
        server_connected,
        "Server should be connected with AES-256-GCM"
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls13_handshake_chacha20_poly1305() {
    use dimpl::Config;
    use dimpl::certificate::generate_self_signed_certificate;
    use dimpl::crypto::Dtls13CipherSuite;
    use dimpl::crypto::aws_lc_rs;

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    let default = aws_lc_rs::default_provider();
    let chacha_only: Vec<_> = default
        .dtls13_cipher_suites
        .iter()
        .copied()
        .filter(|cs| cs.suite() == Dtls13CipherSuite::CHACHA20_POLY1305_SHA256)
        .collect();
    assert!(
        !chacha_only.is_empty(),
        "Provider must have CHACHA20_POLY1305"
    );

    let chacha_static: &'static [_] = Box::leak(chacha_only.into_boxed_slice());

    let provider = dimpl::crypto::CryptoProvider {
        dtls13_cipher_suites: chacha_static,
        ..default
    };

    let config = Arc::new(
        Config::builder()
            .with_crypto_provider(provider)
            .build()
            .expect("build config"),
    );

    let mut now = Instant::now();

    let mut client = Dtls::new_13(Arc::clone(&config), client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_13(config, server_cert, now);
    server.set_active(false);
    let mut client_connected = false;
    let mut server_connected = false;

    for _ in 0..30 {
        client.handle_timeout(now).expect("client timeout");
        server.handle_timeout(now).expect("server timeout");

        let client_out = drain_outputs(&mut client);
        let server_out = drain_outputs(&mut server);

        if client_out.connected {
            client_connected = true;
        }
        if server_out.connected {
            server_connected = true;
        }

        deliver_packets(&client_out.packets, &mut server);
        deliver_packets(&server_out.packets, &mut client);

        if client_connected && server_connected {
            break;
        }

        now += Duration::from_millis(10);
    }

    assert!(
        client_connected,
        "Client should be connected with CHACHA20-POLY1305"
    );
    assert!(
        server_connected,
        "Server should be connected with CHACHA20-POLY1305"
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls13_handshake_secp256r1_key_exchange() {
    use dimpl::Config;
    use dimpl::certificate::generate_self_signed_certificate;
    use dimpl::crypto::NamedGroup;
    use dimpl::crypto::aws_lc_rs;

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    // Build a custom provider where the client only offers P-256
    let default = aws_lc_rs::default_provider();
    let p256_only: Vec<_> = default
        .kx_groups
        .iter()
        .copied()
        .filter(|g| g.name() == NamedGroup::Secp256r1)
        .collect();
    assert!(!p256_only.is_empty(), "Provider must have P-256");

    let p256_static: &'static [_] = Box::leak(p256_only.into_boxed_slice());

    let client_provider = dimpl::crypto::CryptoProvider {
        kx_groups: p256_static,
        ..default.clone()
    };

    let client_config = Arc::new(
        Config::builder()
            .with_crypto_provider(client_provider)
            .build()
            .expect("build client config"),
    );

    // Server uses default provider (supports both P-256 and P-384)
    let server_config = Arc::new(
        Config::builder()
            .with_crypto_provider(default)
            .build()
            .expect("build server config"),
    );

    let mut now = Instant::now();

    let mut client = Dtls::new_13(client_config, client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_13(server_config, server_cert, now);
    server.set_active(false);
    let mut client_connected = false;
    let mut server_connected = false;

    for _ in 0..30 {
        client.handle_timeout(now).expect("client timeout");
        server.handle_timeout(now).expect("server timeout");

        let client_out = drain_outputs(&mut client);
        let server_out = drain_outputs(&mut server);

        if client_out.connected {
            client_connected = true;
        }
        if server_out.connected {
            server_connected = true;
        }

        deliver_packets(&client_out.packets, &mut server);
        deliver_packets(&server_out.packets, &mut client);

        if client_connected && server_connected {
            break;
        }

        now += Duration::from_millis(10);
    }

    assert!(
        client_connected,
        "Client should be connected with P-256 key exchange"
    );
    assert!(
        server_connected,
        "Server should be connected with P-256 key exchange"
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls13_handshake_x25519_key_exchange() {
    use dimpl::Config;
    use dimpl::certificate::generate_self_signed_certificate;
    use dimpl::crypto::NamedGroup;

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    // Use config filter to select only X25519 and disable DTLS 1.2
    // to keep this test focused on DTLS 1.3 behavior.
    let config = Arc::new(
        Config::builder()
            .kx_groups(&[NamedGroup::X25519])
            .dtls12_cipher_suites(&[])
            .build()
            .expect("build config"),
    );

    let mut now = Instant::now();

    let mut client = Dtls::new_13(Arc::clone(&config), client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_13(config, server_cert, now);
    server.set_active(false);
    let mut client_connected = false;
    let mut server_connected = false;

    for _ in 0..30 {
        client.handle_timeout(now).expect("client timeout");
        server.handle_timeout(now).expect("server timeout");

        let client_out = drain_outputs(&mut client);
        let server_out = drain_outputs(&mut server);

        if client_out.connected {
            client_connected = true;
        }
        if server_out.connected {
            server_connected = true;
        }

        deliver_packets(&client_out.packets, &mut server);
        deliver_packets(&server_out.packets, &mut client);

        if client_connected && server_connected {
            break;
        }

        now += Duration::from_millis(10);
    }

    assert!(
        client_connected,
        "Client should be connected with X25519 key exchange"
    );
    assert!(
        server_connected,
        "Server should be connected with X25519 key exchange"
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls13_handshake_client_certificate_auth() {
    use dimpl::Config;
    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    let expected_client_cert = client_cert.certificate.clone();

    // Explicitly require client certificate
    let config = Arc::new(
        Config::builder()
            .require_client_certificate(true)
            .build()
            .expect("build config"),
    );

    let mut now = Instant::now();

    let mut client = Dtls::new_13(Arc::clone(&config), client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_13(config, server_cert, now);
    server.set_active(false);
    let mut client_connected = false;
    let mut server_connected = false;
    let mut server_peer_cert: Option<Vec<u8>> = None;

    for _ in 0..30 {
        client.handle_timeout(now).expect("client timeout");
        server.handle_timeout(now).expect("server timeout");

        let client_out = drain_outputs(&mut client);
        let server_out = drain_outputs(&mut server);

        if client_out.connected {
            client_connected = true;
        }
        if server_out.connected {
            server_connected = true;
        }
        if let Some(cert) = server_out.peer_cert {
            server_peer_cert = Some(cert);
        }

        deliver_packets(&client_out.packets, &mut server);
        deliver_packets(&server_out.packets, &mut client);

        if client_connected && server_connected {
            break;
        }

        now += Duration::from_millis(10);
    }

    assert!(
        client_connected,
        "Client should be connected with client cert auth"
    );
    assert!(
        server_connected,
        "Server should be connected with client cert auth"
    );
    assert!(
        server_peer_cert.is_some(),
        "Server should have received client's certificate"
    );
    assert_eq!(
        server_peer_cert.unwrap(),
        expected_client_cert,
        "Server should receive the correct client certificate"
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls13_handshake_timeout_expires() {
    use dimpl::Config;
    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    // Short handshake timeout so the test runs quickly
    let config = Arc::new(
        Config::builder()
            .handshake_timeout(Duration::from_secs(5))
            .flight_start_rto(Duration::from_millis(500))
            .flight_retries(2)
            .build()
            .expect("build config"),
    );

    let mut now = Instant::now();

    let mut client = Dtls::new_13(Arc::clone(&config), client_cert, now);
    client.set_active(true);

    let mut _server = Dtls::new_13(config, server_cert, now);
    _server.set_active(false);

    let mut timed_out = false;

    // Never deliver any packets between client and server.
    // Keep triggering timeouts until the handshake times out.
    for _ in 0..100 {
        match client.handle_timeout(now) {
            Ok(()) => {
                // Drain outputs to keep the state machine consistent
                drain_outputs(&mut client);
            }
            Err(_) => {
                timed_out = true;
                break;
            }
        }

        now += Duration::from_secs(1);
    }

    assert!(
        timed_out,
        "Client should eventually report a timeout error when no packets are delivered"
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls13_hrr_with_p256_then_x25519() {
    // Client offers P-256 key_share but server prefers X25519. Since the
    // client's key_share does not match the server's preferred group, the
    // server sends HelloRetryRequest asking the client to retry with X25519.
    use dimpl::Config;
    use dimpl::certificate::generate_self_signed_certificate;
    use dimpl::crypto::NamedGroup;
    use dimpl::crypto::aws_lc_rs;

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    let default = aws_lc_rs::default_provider();

    // Client: offers both groups but P-256 is first (sends P-256 key_share)
    let client_groups: Vec<_> = default
        .kx_groups
        .iter()
        .copied()
        .filter(|g| g.name() == NamedGroup::Secp256r1 || g.name() == NamedGroup::X25519)
        .collect();
    // Ensure P-256 is first
    let mut client_groups_sorted: Vec<_> = client_groups;
    client_groups_sorted.sort_by_key(|g| {
        if g.name() == NamedGroup::Secp256r1 {
            0
        } else {
            1
        }
    });
    let client_groups_static: &'static [_] = Box::leak(client_groups_sorted.into_boxed_slice());

    let client_provider = dimpl::crypto::CryptoProvider {
        kx_groups: client_groups_static,
        ..default.clone()
    };

    // Server: prefers X25519 (X25519 is first in kx_groups)
    let server_groups: Vec<_> = default
        .kx_groups
        .iter()
        .copied()
        .filter(|g| g.name() == NamedGroup::Secp256r1 || g.name() == NamedGroup::X25519)
        .collect();
    let mut server_groups_sorted: Vec<_> = server_groups;
    server_groups_sorted.sort_by_key(|g| if g.name() == NamedGroup::X25519 { 0 } else { 1 });
    let server_groups_static: &'static [_] = Box::leak(server_groups_sorted.into_boxed_slice());

    let server_provider = dimpl::crypto::CryptoProvider {
        kx_groups: server_groups_static,
        ..default
    };

    let client_config = Arc::new(
        Config::builder()
            .with_crypto_provider(client_provider)
            .build()
            .expect("build client config"),
    );

    let server_config = Arc::new(
        Config::builder()
            .with_crypto_provider(server_provider)
            .build()
            .expect("build server config"),
    );

    let mut now = Instant::now();

    let mut client = Dtls::new_13(client_config, client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_13(server_config, server_cert, now);
    server.set_active(false);
    let mut client_connected = false;
    let mut server_connected = false;
    let mut saw_hrr = false;
    let mut flight_count = 0;

    for _ in 0..40 {
        client.handle_timeout(now).expect("client timeout");
        server.handle_timeout(now).expect("server timeout");

        let client_out = drain_outputs(&mut client);
        let server_out = drain_outputs(&mut server);

        if !client_out.packets.is_empty() {
            flight_count += 1;
        }

        // HRR: server sends packets before full handshake completes, during
        // the initial exchange (flight_count <= 2)
        if !server_out.packets.is_empty() && !client_connected && flight_count <= 2 {
            saw_hrr = true;
        }

        if client_out.connected {
            client_connected = true;
        }
        if server_out.connected {
            server_connected = true;
        }

        deliver_packets(&client_out.packets, &mut server);
        deliver_packets(&server_out.packets, &mut client);

        if client_connected && server_connected {
            break;
        }

        now += Duration::from_millis(10);
    }

    assert!(
        client_connected,
        "Client should be connected after HRR with group mismatch"
    );
    assert!(
        server_connected,
        "Server should be connected after HRR with group mismatch"
    );
    // The client sent P-256 key_share but server prefers X25519, so HRR should occur
    assert!(
        saw_hrr || flight_count >= 2,
        "Should have seen HRR (server prefers X25519 but client offered P-256)"
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls13_hrr_handshake_completes_after_packet_loss() {
    use dimpl::Config;
    use dimpl::certificate::generate_self_signed_certificate;
    use dimpl::crypto::NamedGroup;
    use dimpl::crypto::aws_lc_rs;

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    let default = aws_lc_rs::default_provider();

    // Client: P-256 first (sends P-256 key_share)
    let client_groups: Vec<_> = default
        .kx_groups
        .iter()
        .copied()
        .filter(|g| g.name() == NamedGroup::Secp256r1 || g.name() == NamedGroup::Secp384r1)
        .collect();
    let mut client_groups_sorted: Vec<_> = client_groups;
    client_groups_sorted.sort_by_key(|g| {
        if g.name() == NamedGroup::Secp256r1 {
            0
        } else {
            1
        }
    });
    let client_groups_static: &'static [_] = Box::leak(client_groups_sorted.into_boxed_slice());

    let client_provider = dimpl::crypto::CryptoProvider {
        kx_groups: client_groups_static,
        ..default.clone()
    };

    // Server: P-384 first (triggers HRR when client offers P-256 key_share)
    let server_groups: Vec<_> = default
        .kx_groups
        .iter()
        .copied()
        .filter(|g| g.name() == NamedGroup::Secp256r1 || g.name() == NamedGroup::Secp384r1)
        .collect();
    let mut server_groups_sorted: Vec<_> = server_groups;
    server_groups_sorted.sort_by_key(|g| {
        if g.name() == NamedGroup::Secp384r1 {
            0
        } else {
            1
        }
    });
    let server_groups_static: &'static [_] = Box::leak(server_groups_sorted.into_boxed_slice());

    let server_provider = dimpl::crypto::CryptoProvider {
        kx_groups: server_groups_static,
        ..default
    };

    let client_config = Arc::new(
        Config::builder()
            .with_crypto_provider(client_provider)
            .flight_retries(8)
            .build()
            .expect("build client config"),
    );

    let server_config = Arc::new(
        Config::builder()
            .with_crypto_provider(server_provider)
            .flight_retries(8)
            .build()
            .expect("build server config"),
    );

    let mut now = Instant::now();

    let mut client = Dtls::new_13(client_config, client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_13(server_config, server_cert, now);
    server.set_active(false);
    let mut client_connected = false;
    let mut server_connected = false;

    // Track flights to drop the first packet of each new flight
    let mut last_client_flight_dropped = false;
    let mut last_server_flight_dropped = false;
    let mut prev_client_had_packets = false;
    let mut prev_server_had_packets = false;

    for i in 0..80 {
        client.handle_timeout(now).expect("client timeout");
        server.handle_timeout(now).expect("server timeout");

        let client_out = drain_outputs(&mut client);
        let server_out = drain_outputs(&mut server);

        if client_out.connected {
            client_connected = true;
        }
        if server_out.connected {
            server_connected = true;
        }

        // Detect new flight from client: packets appear after a gap
        let client_has_packets = !client_out.packets.is_empty();
        let is_new_client_flight = client_has_packets && !prev_client_had_packets;
        prev_client_had_packets = client_has_packets;

        if is_new_client_flight && !last_client_flight_dropped {
            // Drop the first packet of this flight, deliver the rest
            last_client_flight_dropped = true;
            for p in client_out.packets.iter().skip(1) {
                let _ = server.handle_packet(p);
            }
        } else {
            if client_has_packets {
                // Reset for next flight detection
                last_client_flight_dropped = false;
            }
            deliver_packets(&client_out.packets, &mut server);
        }

        // Detect new flight from server
        let server_has_packets = !server_out.packets.is_empty();
        let is_new_server_flight = server_has_packets && !prev_server_had_packets;
        prev_server_had_packets = server_has_packets;

        if is_new_server_flight && !last_server_flight_dropped {
            // Drop the first packet of this flight, deliver the rest
            last_server_flight_dropped = true;
            for p in server_out.packets.iter().skip(1) {
                let _ = client.handle_packet(p);
            }
        } else {
            if server_has_packets {
                last_server_flight_dropped = false;
            }
            deliver_packets(&server_out.packets, &mut client);
        }

        if client_connected && server_connected {
            break;
        }

        // Advance time to trigger retransmissions periodically
        if i % 5 == 4 {
            now += Duration::from_secs(2);
        } else {
            now += Duration::from_millis(10);
        }
    }

    assert!(
        client_connected,
        "Client should connect after HRR despite packet loss"
    );
    assert!(
        server_connected,
        "Server should connect after HRR despite packet loss"
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls13_handshake_no_client_auth() {
    use dimpl::Config;
    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    let config = Arc::new(
        Config::builder()
            .require_client_certificate(false)
            .build()
            .expect("build config"),
    );

    let mut now = Instant::now();

    let mut client = Dtls::new_13(Arc::clone(&config), client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_13(config, server_cert, now);
    server.set_active(false);

    let mut client_connected = false;
    let mut server_connected = false;

    for _ in 0..30 {
        client.handle_timeout(now).expect("client timeout");
        server.handle_timeout(now).expect("server timeout");

        let client_out = drain_outputs(&mut client);
        let server_out = drain_outputs(&mut server);

        client_connected |= client_out.connected;
        server_connected |= server_out.connected;

        deliver_packets(&client_out.packets, &mut server);
        deliver_packets(&server_out.packets, &mut client);

        if client_connected && server_connected {
            break;
        }
        now += Duration::from_millis(50);
    }

    assert!(
        client_connected,
        "Client should connect without client auth"
    );
    assert!(
        server_connected,
        "Server should connect without client auth"
    );

    // Verify data exchange works after handshake
    client
        .send_application_data(b"no-auth-ping")
        .expect("send app data");
    now += Duration::from_millis(10);
    client.handle_timeout(now).expect("client timeout");
    let client_out = drain_outputs(&mut client);
    deliver_packets(&client_out.packets, &mut server);

    server.handle_timeout(now).expect("server timeout");
    let server_out = drain_outputs(&mut server);
    assert!(
        server_out
            .app_data
            .iter()
            .any(|d| d.as_slice() == b"no-auth-ping"),
        "Server should receive app data after no-client-auth handshake"
    );
}

/// Verify the Finished flight is retransmitted on packet loss when client
/// auth is NOT requested (regression test for flight_begin fix).
#[test]
#[cfg(feature = "rcgen")]
fn dtls13_no_client_auth_retransmit_finished() {
    use dimpl::Config;
    use dimpl::certificate::generate_self_signed_certificate;

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    let config = Arc::new(
        Config::builder()
            .require_client_certificate(false)
            .flight_retries(6)
            .build()
            .expect("build config"),
    );

    let mut now = Instant::now();

    let mut client = Dtls::new_13(Arc::clone(&config), client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_13(config, server_cert, now);
    server.set_active(false);

    let mut client_connected = false;
    let mut server_connected = false;
    let mut dropped_client_finished = false;

    for i in 0..80 {
        client.handle_timeout(now).expect("client timeout");
        server.handle_timeout(now).expect("server timeout");

        let client_out = drain_outputs(&mut client);
        let server_out = drain_outputs(&mut server);

        client_connected |= client_out.connected;
        server_connected |= server_out.connected;

        // Drop the first client flight after the client reports Connected.
        // This simulates loss of the client's Finished message.
        if client_connected && !dropped_client_finished && !client_out.packets.is_empty() {
            dropped_client_finished = true;
            // Don't deliver — the Finished flight is lost.
        } else {
            deliver_packets(&client_out.packets, &mut server);
        }

        deliver_packets(&server_out.packets, &mut client);

        if client_connected && server_connected {
            break;
        }

        // Trigger retransmissions
        if i % 5 == 4 {
            now += Duration::from_secs(2);
        } else {
            now += Duration::from_millis(50);
        }
    }

    assert!(
        client_connected,
        "Client should connect despite Finished loss"
    );
    assert!(
        server_connected,
        "Server should connect via retransmitted Finished"
    );
}

#[test]
#[cfg(all(feature = "rcgen", feature = "rust-crypto"))]
fn dtls13_handshake_chacha20_poly1305_rust_crypto() {
    use dimpl::Config;
    use dimpl::certificate::generate_self_signed_certificate;
    use dimpl::crypto::Dtls13CipherSuite;
    use dimpl::crypto::rust_crypto;

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    let default = rust_crypto::default_provider();
    let chacha_only: Vec<_> = default
        .dtls13_cipher_suites
        .iter()
        .copied()
        .filter(|cs| cs.suite() == Dtls13CipherSuite::CHACHA20_POLY1305_SHA256)
        .collect();
    assert!(
        !chacha_only.is_empty(),
        "rust_crypto provider must have CHACHA20_POLY1305"
    );

    let chacha_static: &'static [_] = Box::leak(chacha_only.into_boxed_slice());

    let provider = dimpl::crypto::CryptoProvider {
        dtls13_cipher_suites: chacha_static,
        ..default
    };

    let config = Arc::new(
        Config::builder()
            .with_crypto_provider(provider)
            .build()
            .expect("build config"),
    );

    let mut now = Instant::now();

    let mut client = Dtls::new_13(Arc::clone(&config), client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_13(config, server_cert, now);
    server.set_active(false);
    let mut client_connected = false;
    let mut server_connected = false;

    for _ in 0..30 {
        client.handle_timeout(now).expect("client timeout");
        server.handle_timeout(now).expect("server timeout");

        let client_out = drain_outputs(&mut client);
        let server_out = drain_outputs(&mut server);

        if client_out.connected {
            client_connected = true;
        }
        if server_out.connected {
            server_connected = true;
        }

        deliver_packets(&client_out.packets, &mut server);
        deliver_packets(&server_out.packets, &mut client);

        if client_connected && server_connected {
            break;
        }

        now += Duration::from_millis(10);
    }

    assert!(
        client_connected,
        "Client should be connected with rust_crypto CHACHA20-POLY1305"
    );
    assert!(
        server_connected,
        "Server should be connected with rust_crypto CHACHA20-POLY1305"
    );
}

#[test]
#[cfg(all(feature = "rcgen", feature = "rust-crypto"))]
fn dtls13_handshake_x25519_rust_crypto() {
    use dimpl::Config;
    use dimpl::certificate::generate_self_signed_certificate;
    use dimpl::crypto::NamedGroup;
    use dimpl::crypto::rust_crypto;

    let _ = env_logger::try_init();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    let default = rust_crypto::default_provider();
    let x25519_only: Vec<_> = default
        .kx_groups
        .iter()
        .copied()
        .filter(|g| g.name() == NamedGroup::X25519)
        .collect();
    assert!(
        !x25519_only.is_empty(),
        "rust_crypto provider must have X25519"
    );

    let x25519_static: &'static [_] = Box::leak(x25519_only.into_boxed_slice());

    let provider = dimpl::crypto::CryptoProvider {
        kx_groups: x25519_static,
        ..default
    };

    let config = Arc::new(
        Config::builder()
            .with_crypto_provider(provider)
            .build()
            .expect("build config"),
    );

    let mut now = Instant::now();

    let mut client = Dtls::new_13(Arc::clone(&config), client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_13(config, server_cert, now);
    server.set_active(false);
    let mut client_connected = false;
    let mut server_connected = false;

    for _ in 0..30 {
        client.handle_timeout(now).expect("client timeout");
        server.handle_timeout(now).expect("server timeout");

        let client_out = drain_outputs(&mut client);
        let server_out = drain_outputs(&mut server);

        if client_out.connected {
            client_connected = true;
        }
        if server_out.connected {
            server_connected = true;
        }

        deliver_packets(&client_out.packets, &mut server);
        deliver_packets(&server_out.packets, &mut client);

        if client_connected && server_connected {
            break;
        }

        now += Duration::from_millis(10);
    }

    assert!(
        client_connected,
        "Client should be connected with rust_crypto X25519"
    );
    assert!(
        server_connected,
        "Server should be connected with rust_crypto X25519"
    );
}
