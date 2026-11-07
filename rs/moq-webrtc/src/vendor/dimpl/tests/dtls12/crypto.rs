//! DTLS 1.2 cipher suite tests (all supported suites).

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Instant;

use dimpl::crypto::{Dtls12CipherSuite, SignatureAlgorithm};
use dimpl::{Config, Dtls, Output};

use crate::ossl_helper::{DtlsCertOptions, DtlsEvent, DtlsPKeyType, OsslDtlsCert};

const GROUPS_PREFER_X25519: &str = "X25519:P-256:P-384";
const NAMED_GROUP_X25519: u16 = 0x001D;

fn read_u24(bytes: &[u8]) -> usize {
    ((bytes[0] as usize) << 16) | ((bytes[1] as usize) << 8) | (bytes[2] as usize)
}

fn find_server_key_exchange_group(packet: &[u8]) -> Option<u16> {
    let mut rec_offset = 0usize;
    while rec_offset + 13 <= packet.len() {
        let content_type = packet[rec_offset];
        let rec_len =
            u16::from_be_bytes([packet[rec_offset + 11], packet[rec_offset + 12]]) as usize;
        let rec_start = rec_offset + 13;
        let rec_end = rec_start + rec_len;
        if rec_end > packet.len() {
            return None;
        }

        // Handshake records only (22)
        if content_type == 22 {
            let mut hs_offset = rec_start;
            while hs_offset + 12 <= rec_end {
                let hs_type = packet[hs_offset];
                let _hs_len = read_u24(&packet[hs_offset + 1..hs_offset + 4]);
                let frag_offset = read_u24(&packet[hs_offset + 6..hs_offset + 9]);
                let frag_len = read_u24(&packet[hs_offset + 9..hs_offset + 12]);
                let body_start = hs_offset + 12;
                let body_end = body_start + frag_len;
                if body_end > rec_end {
                    break;
                }

                // ServerKeyExchange (12), first fragment, ECDHE params:
                // curve_type(1) + named_group(2) + pubkey_len(1) + pubkey + signature...
                if hs_type == 12 && frag_offset == 0 && frag_len >= 4 && packet[body_start] == 3 {
                    let group =
                        u16::from_be_bytes([packet[body_start + 1], packet[body_start + 2]]);
                    return Some(group);
                }

                if frag_len == 0 {
                    break;
                }
                hs_offset = body_end;
            }
        }

        rec_offset = rec_end;
    }
    None
}

#[test]
fn dtls12_all_cipher_suites() {
    let _ = env_logger::try_init();

    // Loop over all supported cipher suites and ensure we can connect
    // Skip PSK suites — they require PSK config, not certificate-based interop
    for &suite in Dtls12CipherSuite::all().iter().filter(|s| !s.is_psk()) {
        eprintln!("Testing suite (dimpl client ↔️ ossl server): {:?}", suite);

        run_dimpl_client_vs_ossl_server_for_suite(suite);

        eprintln!("Testing suite (ossl client ↔️ dimpl server): {:?}", suite);
        run_ossl_client_vs_dimpl_server_for_suite(suite);
    }
}

fn config_for_suite(suite: Dtls12CipherSuite) -> Arc<Config> {
    let mut provider = Config::default().crypto_provider().clone();
    let selected = provider
        .cipher_suites
        .iter()
        .copied()
        .find(|cs| cs.suite() == suite)
        .unwrap_or_else(|| panic!("Suite {:?} not found in provider", suite));

    // Leak a tiny fixed-size slice so it can satisfy the provider's 'static requirement.
    let suites = Box::leak(Box::new([selected]));
    provider.cipher_suites = suites;

    Arc::new(
        Config::builder()
            .with_crypto_provider(provider)
            .build()
            .expect("build config for single suite"),
    )
}

fn run_dimpl_client_vs_ossl_server_for_suite(suite: Dtls12CipherSuite) {
    // Generate certificates for both client and server matching the suite's signature algorithm
    let pkey_type = match suite.signature_algorithm() {
        Some(SignatureAlgorithm::ECDSA) => DtlsPKeyType::EcDsaP256,
        Some(SignatureAlgorithm::RSA) => DtlsPKeyType::Rsa2048,
        _ => panic!("Unsupported signature algorithm in suite: {:?}", suite),
    };

    let client_cert = OsslDtlsCert::new(DtlsCertOptions {
        common_name: "WebRTC".into(),
        pkey_type: pkey_type.clone(),
    });

    let server_cert = OsslDtlsCert::new(DtlsCertOptions {
        common_name: "WebRTC".into(),
        pkey_type,
    });

    // Create OpenSSL server impl
    let mut server = server_cert
        .new_dtls_impl_with_groups(GROUPS_PREFER_X25519)
        .expect("Failed to create DTLS server");
    server.set_active(false);

    // Initialize dimpl client restricted to the single suite.
    let config = config_for_suite(suite);

    // DER encodings for our client
    let client_x509_der = client_cert.x509.to_der().expect("client cert der");
    let client_pkey_der = client_cert
        .pkey
        .private_key_to_der()
        .expect("client key der");

    let now = Instant::now();

    let mut client = Dtls::new_12(
        config,
        dimpl::DtlsCertificate {
            certificate: client_x509_der,
            private_key: client_pkey_der,
        },
        now,
    );
    client.set_active(true);

    let mut server_events = VecDeque::new();
    let mut client_connected = false;
    let mut server_connected = false;
    let mut server_kx_group = None;

    let mut out_buf = vec![0u8; 2048];
    for _ in 0..60 {
        client.handle_timeout(Instant::now()).unwrap();
        // Drain client outputs
        loop {
            match client.poll_output(&mut out_buf) {
                Output::Packet(data) => {
                    server
                        .handle_receive(data, &mut server_events)
                        .expect("Server failed to handle client packet");
                }
                Output::Connected => {
                    client_connected = true;
                }
                Output::Timeout(_) => break,
                _ => {}
            }
        }

        // Process server events
        while let Some(event) = server_events.pop_front() {
            if let DtlsEvent::Connected = event {
                server_connected = true;
            }
        }

        // Send server datagrams back to client
        while let Some(datagram) = server.poll_datagram() {
            if server_kx_group.is_none() {
                server_kx_group = find_server_key_exchange_group(&datagram);
            }
            client
                .handle_packet(&datagram)
                .expect("Failed to handle server packet");
        }

        if client_connected && server_connected {
            break;
        }
    }

    assert!(
        client_connected,
        "Client should connect for suite {:?}",
        suite
    );
    assert!(
        server_connected,
        "Server should connect for suite {:?}",
        suite
    );
    assert_eq!(
        server_kx_group,
        Some(NAMED_GROUP_X25519),
        "OpenSSL server should negotiate X25519 for suite {:?}",
        suite
    );
}

fn run_ossl_client_vs_dimpl_server_for_suite(suite: Dtls12CipherSuite) {
    // Generate certificates for both ends
    let pkey_type = match suite.signature_algorithm() {
        Some(SignatureAlgorithm::ECDSA) => DtlsPKeyType::EcDsaP256,
        Some(SignatureAlgorithm::RSA) => DtlsPKeyType::Rsa2048,
        _ => panic!("Unsupported signature algorithm in suite: {:?}", suite),
    };

    let server_cert = OsslDtlsCert::new(DtlsCertOptions {
        common_name: "WebRTC".into(),
        pkey_type: pkey_type.clone(),
    });
    let client_cert = OsslDtlsCert::new(DtlsCertOptions {
        common_name: "WebRTC".into(),
        pkey_type,
    });

    // OpenSSL DTLS client
    let mut ossl_client = client_cert
        .new_dtls_impl_with_groups(GROUPS_PREFER_X25519)
        .expect("Failed to create DTLS client");
    ossl_client.set_active(true);

    // dimpl server with single-suite config.
    let config = config_for_suite(suite);

    let server_x509_der = server_cert.x509.to_der().expect("server cert der");
    let server_pkey_der = server_cert
        .pkey
        .private_key_to_der()
        .expect("server key der");

    let now = Instant::now();

    let mut server = Dtls::new_12(
        config,
        dimpl::DtlsCertificate {
            certificate: server_x509_der,
            private_key: server_pkey_der,
        },
        now,
    );
    server.set_active(false);

    // Drive handshake until both sides report connected
    let mut client_events = VecDeque::new();
    let mut server_connected = false;
    let mut client_connected = false;
    let mut server_kx_group = None;

    let mut out_buf = vec![0u8; 2048];
    for _ in 0..60 {
        server.handle_timeout(Instant::now()).unwrap();
        ossl_client.handle_handshake(&mut client_events).unwrap();

        // 1) Drain client (OpenSSL) outgoing datagrams to the server
        while let Some(datagram) = ossl_client.poll_datagram() {
            server
                .handle_packet(&datagram)
                .expect("Server failed to handle client packet");
        }

        // 2) Poll server outputs and feed to client
        loop {
            match server.poll_output(&mut out_buf) {
                Output::Packet(data) => {
                    if server_kx_group.is_none() {
                        server_kx_group = find_server_key_exchange_group(data);
                    }
                    ossl_client
                        .handle_receive(data, &mut client_events)
                        .expect("Client failed to handle server packet");
                }
                Output::Connected => {
                    server_connected = true;
                }
                Output::Timeout(_) => break,
                _ => {}
            }
        }

        // 3) Process client (OpenSSL) events
        while let Some(event) = client_events.pop_front() {
            if let DtlsEvent::Connected = event {
                client_connected = true;
            }
        }

        // 4) Deliver any further client datagrams produced after events
        while let Some(datagram) = ossl_client.poll_datagram() {
            server
                .handle_packet(&datagram)
                .expect("Server failed to handle client packet");
        }

        if server_connected && client_connected {
            break;
        }
    }

    assert!(
        server_connected,
        "Server should connect for suite {:?}",
        suite
    );
    assert!(
        client_connected,
        "Client should connect for suite {:?}",
        suite
    );
    assert_eq!(
        server_kx_group,
        Some(NAMED_GROUP_X25519),
        "dimpl server should negotiate X25519 for suite {:?}",
        suite
    );
}
