//! DTLS 1.2 handshake tests (cookie retry, parallel handshakes, basic handshake).

use std::sync::Arc;
use std::time::{Duration, Instant};

use dimpl::{Config, Dtls, SrtpProfile};

use crate::common::*;

#[test]
#[cfg(feature = "rcgen")]
fn dtls12_client_certificate_not_required() {
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
    let mut client = Dtls::new_12(Arc::clone(&config), client_cert, now);
    client.set_active(true);
    let mut server = Dtls::new_12(config, server_cert, now);
    complete_dtls12_handshake(&mut client, &mut server, now);
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls12_empty_client_certificate_is_rejected_when_required() {
    use dimpl::certificate::generate_self_signed_certificate;
    use dimpl::{CertificateError, Error};

    let mut now = Instant::now();
    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");
    let config = Arc::new(Config::builder().build().expect("build config"));
    let mut client = Dtls::new_12(Arc::clone(&config), client_cert, now);
    client.set_active(true);
    let mut server = Dtls::new_12(config, server_cert, now);

    for step in 0..60 {
        client.handle_timeout(now).expect("client timeout");
        server.handle_timeout(now).expect("server timeout");
        let client_out = drain_outputs(&mut client);
        let server_out = drain_outputs(&mut server);
        for packet in &client_out.packets {
            if packet.len() >= 25 && packet[0] == 22 && packet[13] == CERTIFICATE {
                // Replace the client's plaintext Certificate record with an
                // empty certificate list, preserving record and handshake sequence numbers.
                let mut empty = packet[..25].to_vec();
                empty[11..13].copy_from_slice(&15u16.to_be_bytes());
                empty[14..17].copy_from_slice(&[0, 0, 3]);
                empty[22..25].copy_from_slice(&[0, 0, 3]);
                empty.extend_from_slice(&[0, 0, 0]);
                assert!(matches!(
                    server.handle_packet(&empty),
                    Err(Error::CertificateError(
                        CertificateError::NoClientCertificateReceived
                    ))
                ));
                let output = drain_outputs(&mut server);
                assert_eq!(output.packets.len(), 1, "one fatal alert packet");
                let alert = &output.packets[0];
                assert_eq!(alert[0], 21, "DTLS Alert record");
                assert_eq!(&alert[3..5], &[0, 0], "pre-key-exchange epoch");
                assert_eq!(&alert[13..], &[2, 40], "fatal handshake_failure");
                assert!(server.is_closed());
                return;
            }
            server.handle_packet(packet).expect("server packet");
        }
        deliver_packets(&server_out.packets, &mut client);
        if step % 5 == 4 {
            now += Duration::from_secs(2);
        } else {
            now += Duration::from_millis(50);
        }
    }
    panic!("client did not send Certificate");
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls12_cookie_retry_proceeds_to_server_hello() {
    //! Verify that after HelloVerifyRequest, the ClientHello with cookie
    //! is properly processed and the server sends ServerHello (not another HVR).

    use dimpl::certificate::generate_self_signed_certificate;

    let now = Instant::now();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    let config = Arc::new(Config::builder().build().expect("Failed to build config"));

    let mut client = Dtls::new_12(config.clone(), client_cert.clone(), now);
    client.set_active(true);

    let mut server = Dtls::new_12(config.clone(), server_cert.clone(), now);
    server.set_active(false);

    // FLIGHT 1: Client sends ClientHello (no cookie)
    client.handle_timeout(now).expect("client timeout start");
    client.handle_timeout(now).expect("client arm flight 1");
    let f1 = collect_packets(&mut client);
    assert!(!f1.is_empty(), "client should emit ClientHello");

    // Verify it's a ClientHello
    let f1_hs_types: Vec<u8> = f1.iter().flat_map(|p| parse_handshake_types(p)).collect();
    assert!(
        f1_hs_types.contains(&CLIENT_HELLO),
        "flight 1 should contain ClientHello, got {:?}",
        f1_hs_types
    );

    // Deliver to server
    for p in &f1 {
        server.handle_packet(p).expect("server recv f1");
    }

    // FLIGHT 2: Server sends HelloVerifyRequest
    server.handle_timeout(now).expect("server arm flight 2");
    let f2 = collect_packets(&mut server);
    assert!(!f2.is_empty(), "server should emit HelloVerifyRequest");

    // Verify it's a HelloVerifyRequest
    let f2_hs_types: Vec<u8> = f2.iter().flat_map(|p| parse_handshake_types(p)).collect();
    assert!(
        f2_hs_types.contains(&HELLO_VERIFY_REQUEST),
        "flight 2 should contain HelloVerifyRequest, got {:?}",
        f2_hs_types
    );

    // Deliver to client
    for p in &f2 {
        client.handle_packet(p).expect("client recv f2");
    }

    // FLIGHT 3: Client sends ClientHello WITH cookie (message_seq=1 per RFC 6347)
    client.handle_timeout(now).expect("client arm flight 3");
    let f3 = collect_packets(&mut client);
    assert!(!f3.is_empty(), "client should emit ClientHello with cookie");

    let f3_hs_types: Vec<u8> = f3.iter().flat_map(|p| parse_handshake_types(p)).collect();
    assert!(
        f3_hs_types.contains(&CLIENT_HELLO),
        "flight 3 should contain ClientHello (with cookie), got {:?}",
        f3_hs_types
    );

    // Deliver to server - THIS IS WHERE THE BUG MANIFESTS
    for p in &f3 {
        server.handle_packet(p).expect("server recv f3");
    }

    // FLIGHT 4: Server should send ServerHello, Certificate, etc. - NOT HelloVerifyRequest
    server.handle_timeout(now).expect("server arm flight 4");
    let f4 = collect_packets(&mut server);
    assert!(
        !f4.is_empty(),
        "server should emit flight 4 after ClientHello with cookie"
    );

    let f4_hs_types: Vec<u8> = f4.iter().flat_map(|p| parse_handshake_types(p)).collect();

    // THE KEY ASSERTION: Server should NOT send another HelloVerifyRequest
    assert!(
        !f4_hs_types.contains(&HELLO_VERIFY_REQUEST),
        "server should NOT send HelloVerifyRequest after valid cookie - BUG! got {:?}",
        f4_hs_types
    );

    // Server should send ServerHello
    assert!(
        f4_hs_types.contains(&SERVER_HELLO),
        "server should send ServerHello after valid cookie, got {:?}",
        f4_hs_types
    );

    // Should also contain Certificate and ServerHelloDone
    assert!(
        f4_hs_types.contains(&CERTIFICATE),
        "server should send Certificate, got {:?}",
        f4_hs_types
    );
    assert!(
        f4_hs_types.contains(&SERVER_HELLO_DONE),
        "server should send ServerHelloDone, got {:?}",
        f4_hs_types
    );

    println!(
        "SUCCESS: Server correctly processed ClientHello with cookie and sent ServerHello flight"
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls12_parallel_handshakes_with_cookies() {
    //! Test multiple parallel DTLS handshakes to ensure cookie handling
    //! works correctly under concurrent load (the original bug scenario).

    use dimpl::certificate::generate_self_signed_certificate;

    let now = Instant::now();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    let config = Arc::new(Config::builder().build().expect("Failed to build config"));

    // Create 5 parallel client-server pairs
    let mut pairs: Vec<(Dtls, Dtls)> = (0..5)
        .map(|_| {
            let mut client = Dtls::new_12(config.clone(), client_cert.clone(), now);
            client.set_active(true);
            let mut server = Dtls::new_12(config.clone(), server_cert.clone(), now);
            server.set_active(false);
            (client, server)
        })
        .collect();

    // Run all handshakes through the cookie exchange phase
    for (i, (client, server)) in pairs.iter_mut().enumerate() {
        // Flight 1: ClientHello
        client.handle_timeout(now).expect("client timeout");
        client.handle_timeout(now).expect("client arm f1");
        let f1 = collect_packets(client);
        for p in &f1 {
            server.handle_packet(p).expect("server recv f1");
        }

        // Flight 2: HelloVerifyRequest
        server.handle_timeout(now).expect("server arm f2");
        let f2 = collect_packets(server);
        for p in &f2 {
            client.handle_packet(p).expect("client recv f2");
        }

        // Flight 3: ClientHello with cookie
        client.handle_timeout(now).expect("client arm f3");
        let f3 = collect_packets(client);
        for p in &f3 {
            server.handle_packet(p).expect("server recv f3");
        }

        // Flight 4: Should be ServerHello, not HelloVerifyRequest
        server.handle_timeout(now).expect("server arm f4");
        let f4 = collect_packets(server);
        let f4_hs_types: Vec<u8> = f4.iter().flat_map(|p| parse_handshake_types(p)).collect();

        assert!(
            !f4_hs_types.contains(&HELLO_VERIFY_REQUEST),
            "pair {}: server sent HelloVerifyRequest instead of ServerHello - BUG!",
            i
        );
        assert!(
            f4_hs_types.contains(&SERVER_HELLO),
            "pair {}: server should send ServerHello, got {:?}",
            i,
            f4_hs_types
        );
    }

    println!(
        "SUCCESS: All {} parallel handshakes processed cookies correctly",
        pairs.len()
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls12_retransmit_no_cookie_after_cookie_sent() {
    //! Simulates the real Firefox bug scenario:
    //! 1. Client sends ClientHello (no cookie)
    //! 2. Server sends HelloVerifyRequest
    //! 3. Client sends ClientHello (with cookie)
    //! 4. Client's timer fires and it ALSO retransmits the original no-cookie ClientHello
    //! 5. Server should NOT get confused by this out-of-order retransmit

    use dimpl::certificate::generate_self_signed_certificate;

    let now = Instant::now();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    let config = Arc::new(Config::builder().build().expect("Failed to build config"));

    let mut client = Dtls::new_12(config.clone(), client_cert.clone(), now);
    client.set_active(true);

    let mut server = Dtls::new_12(config.clone(), server_cert.clone(), now);
    server.set_active(false);

    // Flight 1: ClientHello (no cookie)
    client.handle_timeout(now).expect("client timeout");
    client.handle_timeout(now).expect("client arm f1");
    let f1 = collect_packets(&mut client);
    assert!(!f1.is_empty());

    // Save a copy of the original no-cookie ClientHello for later retransmit
    let f1_copy = f1.clone();

    // Deliver to server
    for p in &f1 {
        server.handle_packet(p).expect("server recv f1");
    }

    // Flight 2: HelloVerifyRequest
    server.handle_timeout(now).expect("server arm f2");
    let f2 = collect_packets(&mut server);
    assert!(!f2.is_empty());

    // Deliver to client
    for p in &f2 {
        client.handle_packet(p).expect("client recv f2");
    }

    // Flight 3: ClientHello WITH cookie
    client.handle_timeout(now).expect("client arm f3");
    let f3 = collect_packets(&mut client);
    assert!(!f3.is_empty());

    // Deliver the cookie version to server
    for p in &f3 {
        server.handle_packet(p).expect("server recv f3 with cookie");
    }

    // NOW simulate Firefox's retransmit timer firing - send the ORIGINAL
    // no-cookie ClientHello again (this is what Firefox does in the real bug)
    for p in &f1_copy {
        // This should not cause the handshake to fail
        server
            .handle_packet(p)
            .expect("server recv retransmit of no-cookie CH");
    }

    // Server should still send ServerHello flight, not another HelloVerifyRequest
    server.handle_timeout(now).expect("server arm f4");
    let f4 = collect_packets(&mut server);
    assert!(!f4.is_empty(), "server should emit flight 4");

    let f4_hs_types: Vec<u8> = f4.iter().flat_map(|p| parse_handshake_types(p)).collect();

    // The key test: even after receiving the retransmitted no-cookie ClientHello,
    // the server should proceed with ServerHello (having already processed the cookie version)
    assert!(
        f4_hs_types.contains(&SERVER_HELLO),
        "server should send ServerHello even after retransmit of no-cookie CH, got {:?}",
        f4_hs_types
    );

    println!("SUCCESS: Server correctly handled out-of-order retransmit scenario");
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls12_retransmit_no_cookie_before_cookie_received() {
    //! Tests the specific bug scenario from webrtc deployments:
    //!
    //! 1. Client sends ClientHello (seq=0, no cookie)
    //! 2. Server sends HelloVerifyRequest, clears queue_rx
    //! 3. Client retransmits old ClientHello (seq=0, no cookie) - HVR was lost/delayed
    //! 4. Server resends HelloVerifyRequest (correct), but OLD ClientHello must NOT
    //!    be inserted into queue_rx, otherwise it blocks the new ClientHello
    //! 5. Client sends ClientHello (seq=1, with cookie)
    //! 6. Server should process it and send ServerHello

    use dimpl::certificate::generate_self_signed_certificate;

    let now = Instant::now();

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    let config = Arc::new(Config::builder().build().expect("Failed to build config"));

    let mut client = Dtls::new_12(config.clone(), client_cert.clone(), now);
    client.set_active(true);

    let mut server = Dtls::new_12(config.clone(), server_cert.clone(), now);
    server.set_active(false);

    // Flight 1: ClientHello (no cookie)
    client.handle_timeout(now).expect("client timeout");
    client.handle_timeout(now).expect("client arm f1");
    let f1 = collect_packets(&mut client);
    assert!(!f1.is_empty());

    // Save a copy of the original no-cookie ClientHello for retransmit simulation
    let f1_copy = f1.clone();

    // Deliver to server
    for p in &f1 {
        server.handle_packet(p).expect("server recv f1");
    }

    // Flight 2: HelloVerifyRequest
    server.handle_timeout(now).expect("server arm f2");
    let f2 = collect_packets(&mut server);
    assert!(!f2.is_empty());

    // Simulate: HVR is "lost" - don't deliver to client yet
    // Instead, client's retransmit timer fires and sends the old ClientHello again

    // THIS IS THE BUG TRIGGER: old ClientHello (seq=0) arrives at server
    // BEFORE the cookie-bearing ClientHello (seq=1)
    for p in &f1_copy {
        server
            .handle_packet(p)
            .expect("server recv retransmit of no-cookie CH");
    }

    // Server should resend HelloVerifyRequest (this is correct behavior)
    // The bug was that the old ClientHello got inserted into queue_rx
    let f2_resend = collect_packets(&mut server);
    let f2_resend_types: Vec<u8> = f2_resend
        .iter()
        .flat_map(|p| parse_handshake_types(p))
        .collect();
    assert!(
        f2_resend_types.contains(&HELLO_VERIFY_REQUEST),
        "server should resend HelloVerifyRequest after duplicate, got {:?}",
        f2_resend_types
    );

    // Now deliver the original HVR to client
    for p in &f2 {
        client.handle_packet(p).expect("client recv f2");
    }

    // Flight 3: ClientHello WITH cookie (seq=1)
    client.handle_timeout(now).expect("client arm f3");
    let f3 = collect_packets(&mut client);
    assert!(!f3.is_empty());

    // Deliver to server - THIS IS WHERE THE BUG MANIFESTED
    // Before fix: queue_rx had old ClientHello (seq=0), blocking this one
    for p in &f3 {
        server.handle_packet(p).expect("server recv f3 with cookie");
    }

    // Server should now send ServerHello flight
    server.handle_timeout(now).expect("server arm f4");
    let f4 = collect_packets(&mut server);
    assert!(!f4.is_empty(), "server should emit flight 4");

    let f4_hs_types: Vec<u8> = f4.iter().flat_map(|p| parse_handshake_types(p)).collect();

    // THE KEY ASSERTION: Server must NOT be stuck sending HelloVerifyRequest
    assert!(
        !f4_hs_types.contains(&HELLO_VERIFY_REQUEST),
        "server should NOT resend HelloVerifyRequest after valid cookie, got {:?}",
        f4_hs_types
    );

    // Server should send ServerHello
    assert!(
        f4_hs_types.contains(&SERVER_HELLO),
        "server should send ServerHello after cookie CH, got {:?}",
        f4_hs_types
    );

    println!("SUCCESS: Old duplicate ClientHello did not block new ClientHello with cookie");
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls12_basic_handshake() {
    //! Complete a full dimpl-to-dimpl DTLS 1.2 handshake and verify both
    //! client and server reach the connected state.

    use dimpl::certificate::generate_self_signed_certificate;

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    let config = dtls12_config();

    let mut now = Instant::now();

    let mut client = Dtls::new_12(Arc::clone(&config), client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_12(config, server_cert, now);
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

    assert!(client_connected, "Client should be connected");
    assert!(server_connected, "Server should be connected");
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls12_handshake_with_keying_material() {
    //! Complete a DTLS 1.2 handshake with SRTP profile configured and verify
    //! both sides derive identical SRTP keying material.

    use dimpl::certificate::generate_self_signed_certificate;

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    let config = dtls12_config();

    let mut now = Instant::now();

    let mut client = Dtls::new_12(Arc::clone(&config), client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_12(config, server_cert, now);
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

    assert_eq!(
        client_km.0, server_km.0,
        "Client and server keying material should match"
    );
    assert_eq!(
        client_km.1, server_km.1,
        "Client and server SRTP profile should match"
    );
    assert!(
        !client_km.0.is_empty(),
        "Keying material should not be empty"
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls12_peer_certificate_exchange() {
    //! Complete a DTLS 1.2 handshake and verify the client can access the
    //! server's certificate and the server can access the client's certificate.

    use dimpl::certificate::generate_self_signed_certificate;

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    let expected_client_cert = client_cert.certificate.clone();
    let expected_server_cert = server_cert.certificate.clone();

    let config = dtls12_config();

    let mut now = Instant::now();

    let mut client = Dtls::new_12(Arc::clone(&config), client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_12(config, server_cert, now);
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
fn dtls12_handshake_client_certificate_auth() {
    //! Configure the server to require a client certificate, complete the
    //! handshake, and verify the server received and validated the client
    //! certificate.

    use dimpl::certificate::generate_self_signed_certificate;

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    let expected_client_cert = client_cert.certificate.clone();

    let config = Arc::new(
        Config::builder()
            .require_client_certificate(true)
            .build()
            .expect("Failed to build config"),
    );

    let mut now = Instant::now();

    let mut client = Dtls::new_12(Arc::clone(&config), client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_12(config, server_cert, now);
    server.set_active(false);

    let mut server_connected = false;
    let mut server_peer_cert: Option<Vec<u8>> = None;

    for _ in 0..30 {
        client.handle_timeout(now).expect("client timeout");
        server.handle_timeout(now).expect("server timeout");

        let client_out = drain_outputs(&mut client);
        let server_out = drain_outputs(&mut server);

        if server_out.connected {
            server_connected = true;
        }
        if let Some(cert) = server_out.peer_cert {
            server_peer_cert = Some(cert);
        }

        deliver_packets(&client_out.packets, &mut server);
        deliver_packets(&server_out.packets, &mut client);

        if server_connected && server_peer_cert.is_some() {
            break;
        }

        now += Duration::from_millis(10);
    }

    assert!(
        server_connected,
        "Server should be connected after client certificate auth"
    );
    assert!(
        server_peer_cert.is_some(),
        "Server should receive client's certificate"
    );
    assert_eq!(
        server_peer_cert.unwrap(),
        expected_client_cert,
        "Server should receive the correct client certificate"
    );
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls12_handshake_secp384r1_key_exchange() {
    //! Configure to use only P-384 for key exchange, complete the handshake,
    //! and verify both sides reach connected.

    use dimpl::certificate::generate_self_signed_certificate;
    use dimpl::crypto::aws_lc_rs;

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    // Build a crypto provider with only the P-384 key exchange group.
    let mut provider = aws_lc_rs::default_provider();
    let p384_vec: Vec<&'static dyn dimpl::crypto::SupportedKxGroup> = provider
        .kx_groups
        .iter()
        .copied()
        .filter(|g| g.name() == dimpl::NamedGroup::Secp384r1)
        .collect();
    // leak: intentional leak to produce a &'static slice for the provider field
    let p384_only: &'static [&'static dyn dimpl::crypto::SupportedKxGroup] =
        Box::leak(p384_vec.into_boxed_slice());
    provider.kx_groups = p384_only;

    let config = Arc::new(
        Config::builder()
            .with_crypto_provider(provider)
            .build()
            .expect("Failed to build config with P-384 only"),
    );

    let mut now = Instant::now();

    let mut client = Dtls::new_12(Arc::clone(&config), client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_12(config, server_cert, now);
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

    assert!(client_connected, "Client should be connected with P-384");
    assert!(server_connected, "Server should be connected with P-384");
}

#[test]
#[cfg(feature = "rcgen")]
fn dtls12_handshake_timeout_expires() {
    //! Create client and server but never deliver packets. Trigger timeouts
    //! repeatedly and verify the handshake eventually times out/fails.

    use dimpl::certificate::generate_self_signed_certificate;

    let client_cert = generate_self_signed_certificate().expect("gen client cert");
    let server_cert = generate_self_signed_certificate().expect("gen server cert");

    // Use a short handshake timeout to speed up the test.
    let config = Arc::new(
        Config::builder()
            .handshake_timeout(Duration::from_secs(5))
            .flight_retries(2)
            .build()
            .expect("Failed to build config"),
    );

    let mut now = Instant::now();

    let mut client = Dtls::new_12(Arc::clone(&config), client_cert, now);
    client.set_active(true);

    let mut server = Dtls::new_12(config, server_cert, now);
    server.set_active(false);
    let mut client_timed_out = false;
    let mut server_timed_out = false;

    // Run for many iterations, advancing time each round, but never deliver
    // packets between client and server.
    for _ in 0..100 {
        if !client_timed_out {
            match client.handle_timeout(now) {
                Ok(()) => {
                    // Drain outputs but discard packets (never deliver them)
                    let _ = drain_outputs(&mut client);
                }
                Err(_) => {
                    client_timed_out = true;
                }
            }
        }

        if !server_timed_out {
            match server.handle_timeout(now) {
                Ok(()) => {
                    let _ = drain_outputs(&mut server);
                }
                Err(_) => {
                    server_timed_out = true;
                }
            }
        }

        if client_timed_out && server_timed_out {
            break;
        }

        now += Duration::from_secs(2);
    }

    assert!(
        client_timed_out,
        "Client handshake should eventually time out when no packets are delivered"
    );
}

#[test]
fn dtls12_handshake_p384_certificate() {
    //! DTLS 1.2 handshake where both sides use P-384 ECDSA certificates.
    //! Regression test: the server's ServerKeyExchange must sign with the
    //! same hash algorithm it advertises on the wire (SHA-384 for P-384).

    use crate::ossl_helper::{DtlsCertOptions, DtlsPKeyType, OsslDtlsCert};

    let client_cert = OsslDtlsCert::new(DtlsCertOptions {
        common_name: "WebRTC".into(),
        pkey_type: DtlsPKeyType::EcDsaP384,
    });
    let server_cert = OsslDtlsCert::new(DtlsCertOptions {
        common_name: "WebRTC".into(),
        pkey_type: DtlsPKeyType::EcDsaP384,
    });

    let config = dtls12_config();
    let mut now = Instant::now();

    let mut client = Dtls::new_12(
        Arc::clone(&config),
        dimpl::DtlsCertificate {
            certificate: client_cert.x509.to_der().expect("client cert der"),
            private_key: client_cert
                .pkey
                .private_key_to_der()
                .expect("client key der"),
        },
        now,
    );
    client.set_active(true);

    let mut server = Dtls::new_12(
        config,
        dimpl::DtlsCertificate {
            certificate: server_cert.x509.to_der().expect("server cert der"),
            private_key: server_cert
                .pkey
                .private_key_to_der()
                .expect("server key der"),
        },
        now,
    );
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

    assert!(client_connected, "Client should connect with P-384 cert");
    assert!(server_connected, "Server should connect with P-384 cert");
}
