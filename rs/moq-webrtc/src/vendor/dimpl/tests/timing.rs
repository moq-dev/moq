#![cfg(feature = "rcgen")]

use std::mem;
use std::ops::Range;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dimpl::certificate::generate_self_signed_certificate;
use dimpl::{Config, Dtls, DtlsCertificate, Error, Output, ProtocolVersion, TimeoutError};

#[path = "dtls13/common.rs"]
mod common;

use common::{DrainedOutputs, drain_outputs};

const VERSIONS: &[Version] = &[Version::Dtls12, Version::Dtls13, Version::Auto];
const BUDGET: Duration = Duration::from_millis(100);
const LONG_RTO: Duration = Duration::from_secs(1);
const PAIRS: &[(Version, Version)] = &[
    (Version::Dtls12, Version::Dtls12),
    (Version::Dtls13, Version::Dtls13),
    (Version::Auto, Version::Dtls12),
    (Version::Auto, Version::Dtls13),
    (Version::Dtls12, Version::Auto),
    (Version::Dtls13, Version::Auto),
    (Version::Auto, Version::Auto),
];

// Offsets in a datagram holding one plaintext record (RFC 6347 §4.1, §4.2.2).
const CONTENT_TYPE: usize = 0;
const RECORD_EPOCH: Range<usize> = 3..5;
const RECORD_SEQUENCE: Range<usize> = 5..11;
const RECORD_LENGTH: Range<usize> = 11..13;
const RECORD_HEADER_LEN: usize = 13;
const MESSAGE_LENGTH: Range<usize> = 14..17;
const MESSAGE_SEQ: Range<usize> = 17..19;
const FRAGMENT_OFFSET: Range<usize> = 19..22;
const FRAGMENT_LENGTH: Range<usize> = 22..25;
const HANDSHAKE_HEADER_LEN: usize = 12;
const HANDSHAKE_BODY: usize = RECORD_HEADER_LEN + HANDSHAKE_HEADER_LEN;
// ClientHello legacy_version (2) and random (32) precede the session id.
const SESSION_ID_LENGTH: usize = HANDSHAKE_BODY + 34;

#[derive(Clone, Copy, Debug)]
enum Version {
    Dtls12,
    Dtls13,
    Auto,
}

impl Version {
    fn endpoint(self, config: Arc<Config>, certificate: DtlsCertificate, now: Instant) -> Dtls {
        match self {
            Self::Dtls12 => Dtls::new_12(config, certificate, now),
            Self::Dtls13 => Dtls::new_13(config, certificate, now),
            Self::Auto => Dtls::new_auto(config, certificate, now),
        }
    }
}

fn config() -> Arc<Config> {
    Arc::new(
        Config::builder()
            .dangerously_set_rng_seed(42)
            .handshake_timeout(BUDGET)
            .flight_start_rto(LONG_RTO)
            .flight_retries(0)
            .build()
            .expect("valid timing config"),
    )
}

fn drain(endpoint: &mut Dtls) -> (Vec<Vec<u8>>, Instant) {
    let mut packets = Vec::new();
    let mut buffer = vec![0; 65536];
    for _ in 0..100 {
        match endpoint.poll_output(&mut buffer) {
            Output::Packet(packet) => packets.push(packet.to_vec()),
            Output::Timeout(deadline) => return (packets, deadline),
            Output::BufferTooSmall { .. } => panic!("unexpected BufferTooSmall output"),
            _ => {}
        }
    }
    panic!("poll cycle did not reach Timeout");
}

fn start_endpoint(
    version: Version,
    active: bool,
    config: Arc<Config>,
    certificate: &DtlsCertificate,
    now: Instant,
) -> (Dtls, Vec<Vec<u8>>, Instant) {
    let mut endpoint = version.endpoint(config.clone(), certificate.clone(), now);
    endpoint.set_active(active);
    endpoint.handle_timeout(now).expect("initialize clock");
    let (mut packets, mut deadline) = drain(&mut endpoint);
    if !active {
        let mut peer = version.endpoint(config, certificate.clone(), now);
        peer.set_active(true);
        peer.handle_timeout(now).expect("queue peer ClientHello");
        for packet in drain(&mut peer).0 {
            endpoint.handle_packet(&packet).expect("accept ClientHello");
            let output = drain(&mut endpoint);
            packets.extend(output.0);
            deadline = output.1;
        }
    }
    assert!(!packets.is_empty(), "{version:?}, active={active}");
    (endpoint, packets, deadline)
}

fn merge_output(received: &mut DrainedOutputs, output: DrainedOutputs) {
    received.connected |= output.connected;
    received.app_data.extend(output.app_data);
    received.packets.extend(output.packets);
    received.timeout = output.timeout;
}

fn deliver_queued(source: &mut DrainedOutputs, target: &mut Dtls, received: &mut DrainedOutputs) {
    for packet in mem::take(&mut source.packets) {
        target.handle_packet(&packet).expect("deliver packet");
        merge_output(received, drain_outputs(target));
    }
}

fn exchange(
    client: &mut Dtls,
    server: &mut Dtls,
    now: Instant,
) -> (DrainedOutputs, DrainedOutputs) {
    let mut client_output = drain_outputs(client);
    let mut server_output = drain_outputs(server);
    for _ in 0..100 {
        deliver_queued(&mut client_output, server, &mut server_output);
        deliver_queued(&mut server_output, client, &mut client_output);
        client.handle_timeout(now).expect("client pending progress");
        merge_output(&mut client_output, drain_outputs(client));
        server.handle_timeout(now).expect("server pending progress");
        merge_output(&mut server_output, drain_outputs(server));
        if client_output.packets.is_empty() && server_output.packets.is_empty() {
            return (client_output, server_output);
        }
    }
    panic!("packet exchange did not settle");
}

fn connected_pair(
    versions: (Version, Version),
    config: Arc<Config>,
    certificate: &DtlsCertificate,
    now: Instant,
) -> (Dtls, Dtls) {
    let mut client = versions
        .0
        .endpoint(config.clone(), certificate.clone(), now);
    let mut server = versions.1.endpoint(config, certificate.clone(), now);
    client.set_active(true);
    client.handle_timeout(now).expect("start client");
    server.handle_timeout(now).expect("server clock");
    let output = exchange(&mut client, &mut server, now);
    assert!(output.0.connected, "client {versions:?}");
    assert!(output.1.connected, "server {versions:?}");
    assert_eq!(client.protocol_version(), server.protocol_version());
    (client, server)
}

fn assert_expires_at(endpoint: &mut Dtls, expected: Instant) {
    for _ in 0..20 {
        let deadline = drain(endpoint).1;
        if deadline == expected {
            assert_eq!(
                endpoint.handle_timeout(deadline),
                Err(Error::Timeout(TimeoutError::Connect))
            );
            return;
        }
        assert!(deadline < expected, "overall deadline moved later");
        endpoint
            .handle_timeout(deadline)
            .expect("retry budget remains");
    }
    panic!("overall deadline was not advertised");
}

fn corrupt_client_hello_extension(hello: &mut [u8], extension_type: u16) {
    let mut cursor = SESSION_ID_LENGTH;
    cursor += 1 + hello[cursor] as usize; // session id
    cursor += 1 + hello[cursor] as usize; // cookie
    cursor += 2 + u16::from_be_bytes([hello[cursor], hello[cursor + 1]]) as usize; // cipher suites
    cursor += 1 + hello[cursor] as usize; // compression methods
    cursor += 2; // extensions length
    while cursor + 4 <= hello.len() {
        let kind = u16::from_be_bytes([hello[cursor], hello[cursor + 1]]);
        let length = u16::from_be_bytes([hello[cursor + 2], hello[cursor + 3]]) as usize;
        if kind == extension_type {
            assert!(length >= 2);
            hello[cursor + 4..cursor + 6].copy_from_slice(&u16::MAX.to_be_bytes());
            return;
        }
        cursor += 4 + length;
    }
    panic!("missing ClientHello extension {extension_type}");
}

#[test]
fn rejected_extensions_and_corrected_hello_share_original_deadline() {
    let base = Instant::now();
    let certificate = generate_self_signed_certificate().expect("certificate");
    let config = Arc::new(
        Config::builder()
            .dangerously_set_rng_seed(42)
            .use_server_cookie(false)
            .handshake_timeout(BUDGET)
            .flight_start_rto(LONG_RTO)
            .build()
            .expect("config"),
    );
    for &(client_version, server_version) in PAIRS {
        for extension_type in [10, 14] {
            let mut client = client_version.endpoint(config.clone(), certificate.clone(), base);
            client.set_active(true);
            client.handle_timeout(base).expect("client clock");
            let (hello, _) = drain(&mut client);
            assert_eq!(hello.len(), 1);
            let mut malformed = hello[0].clone();
            corrupt_client_hello_extension(&mut malformed, extension_type);

            let mut server = server_version.endpoint(config.clone(), certificate.clone(), base);
            server.handle_timeout(base).expect("server clock");
            drain(&mut server);
            server
                .handle_packet(&malformed)
                .expect("discard malformed extension");
            let rejected = drain(&mut server);
            assert!(rejected.0.is_empty());
            assert!(
                rejected.1 == base + BUDGET,
                "{client_version:?} -> {server_version:?}"
            );

            let reception = base + Duration::from_millis(50);
            server
                .handle_timeout(reception)
                .expect("original budget remains");
            drain(&mut server);
            server
                .handle_packet(&malformed)
                .expect("repeated rejection");
            assert_eq!(drain(&mut server).1, base + BUDGET);
            server
                .handle_packet(&hello[0])
                .expect("valid ClientHello after rejected input");
            let accepted = drain(&mut server);
            assert!(
                !accepted.0.is_empty(),
                "{client_version:?} -> {server_version:?}, extension {extension_type}: {server:?}"
            );
            assert_eq!(accepted.1, base + BUDGET);
            assert_expires_at(&mut server, base + BUDGET);
        }
    }
}

#[test]
fn initial_poll_schedules_client_progress_without_spinning_idle_servers() {
    let base = Instant::now();
    let certificate = generate_self_signed_certificate().expect("certificate");
    for &version in VERSIONS {
        let mut endpoint = version.endpoint(config(), certificate.clone(), base);
        let idle = drain(&mut endpoint);
        assert!(idle.0.is_empty());
        assert!(idle.1 > base);
        endpoint.set_active(true);
        if !matches!(version, Version::Auto) {
            let initial = drain(&mut endpoint);
            assert!(initial.0.is_empty());
            assert_eq!(initial.1, base);
            endpoint
                .handle_timeout(initial.1)
                .expect("initial client progress");
        }
        let initial = drain(&mut endpoint);
        assert!(!initial.0.is_empty());
        assert_eq!(initial.1, base + BUDGET);
        endpoint.handle_timeout(base).expect("same logical instant");
        let after = drain(&mut endpoint);
        assert!(after.0.is_empty());
        assert_eq!(after.1, initial.1);
    }
}

#[test]
fn handshake_completes_after_rejected_client_hello() {
    let base = Instant::now();
    let certificate = generate_self_signed_certificate().expect("certificate");
    let config = Arc::new(
        Config::builder()
            .dangerously_set_rng_seed(42)
            .use_server_cookie(false)
            .handshake_timeout(BUDGET)
            .build()
            .expect("config"),
    );
    for &(client_version, server_version) in PAIRS {
        for extension_type in [10, 14] {
            let mut client = client_version.endpoint(config.clone(), certificate.clone(), base);
            let mut server = server_version.endpoint(config.clone(), certificate.clone(), base);
            client.set_active(true);
            client.handle_timeout(base).expect("client clock");
            let (hello, _) = drain(&mut client);
            assert_eq!(hello.len(), 1);
            server.handle_timeout(base).expect("server clock");
            drain(&mut server);
            let mut malformed = hello[0].clone();
            corrupt_client_hello_extension(&mut malformed, extension_type);
            server
                .handle_packet(&malformed)
                .expect("rejected ClientHello");
            assert!(drain(&mut server).0.is_empty());
            server
                .handle_packet(&hello[0])
                .expect("valid ClientHello retry");
            let output = exchange(&mut client, &mut server, base);
            assert!(
                output.0.connected,
                "{client_version:?} -> {server_version:?}"
            );
            assert!(
                output.1.connected,
                "{client_version:?} -> {server_version:?}"
            );
            let later = base + Duration::from_secs(1);
            client.handle_timeout(later).expect("completed client");
            server.handle_timeout(later).expect("completed server");
            exchange(&mut client, &mut server, later);
        }
    }
}

#[test]
fn rejected_later_fragments_keep_first_reception_deadline() {
    let base = Instant::now();
    let certificate = generate_self_signed_certificate().expect("certificate");
    let config = Arc::new(
        Config::builder()
            .dangerously_set_rng_seed(42)
            .use_server_cookie(false)
            .max_queue_rx(50)
            .handshake_timeout(BUDGET)
            .flight_start_rto(LONG_RTO)
            .build()
            .expect("config"),
    );
    for &(client_version, server_version) in PAIRS {
        let mut client = client_version.endpoint(config.clone(), certificate.clone(), base);
        client.set_active(true);
        client.handle_timeout(base).expect("client clock");
        let (hello, _) = drain(&mut client);
        assert_eq!(hello.len(), 1);
        let mut malformed = hello[0].clone();
        corrupt_client_hello_extension(&mut malformed, 14);
        let mut server = server_version.endpoint(config.clone(), certificate.clone(), base);
        server.handle_timeout(base).expect("server clock");
        drain(&mut server);
        let chunk = 32;
        assert!(malformed.len() > HANDSHAKE_BODY + chunk);
        for (index, body) in malformed[HANDSHAKE_BODY..].chunks(chunk).enumerate() {
            if index == 1 {
                server
                    .handle_timeout(base + Duration::from_millis(70))
                    .expect("partial budget");
                drain(&mut server);
            }
            let mut fragment = malformed[..HANDSHAKE_BODY].to_vec();
            fragment.extend_from_slice(body);
            fragment[RECORD_SEQUENCE].copy_from_slice(&(index as u64).to_be_bytes()[2..]);
            fragment[RECORD_LENGTH]
                .copy_from_slice(&((HANDSHAKE_HEADER_LEN + body.len()) as u16).to_be_bytes());
            fragment[FRAGMENT_OFFSET].copy_from_slice(&((index * chunk) as u32).to_be_bytes()[1..]);
            fragment[FRAGMENT_LENGTH].copy_from_slice(&(body.len() as u32).to_be_bytes()[1..]);
            server
                .handle_packet(&fragment)
                .expect("accept fragment or reject completed extension");
            let output = drain(&mut server);
            assert!(output.0.is_empty());
            assert_eq!(
                output.1,
                base + BUDGET,
                "{client_version:?} -> {server_version:?}, fragment {index}"
            );
        }
        assert_expires_at(&mut server, base + BUDGET);
    }
}

#[test]
fn later_message_rejection_does_not_undo_accepted_client_hello() {
    let base = Instant::now();
    let certificate = generate_self_signed_certificate().expect("certificate");
    let config = Arc::new(
        Config::builder()
            .dangerously_set_rng_seed(42)
            .use_server_cookie(false)
            .handshake_timeout(BUDGET)
            .flight_start_rto(LONG_RTO)
            .build()
            .expect("config"),
    );
    for &(client_version, server_version) in PAIRS {
        let mut client = client_version.endpoint(config.clone(), certificate.clone(), base);
        client.set_active(true);
        client.handle_timeout(base).expect("client clock");
        let (hello, _) = drain(&mut client);
        assert_eq!(hello.len(), 1);
        let mut packet = hello[0].clone();
        // A second record carrying a one-byte Certificate (type 11) message.
        let certificate_message = [11, 0, 0, 1, 0, 1, 0, 0, 0, 0, 0, 1, 0];
        let mut malformed_certificate = packet[..RECORD_HEADER_LEN].to_vec();
        malformed_certificate[RECORD_SEQUENCE].copy_from_slice(&1u64.to_be_bytes()[2..]);
        malformed_certificate[RECORD_LENGTH]
            .copy_from_slice(&(certificate_message.len() as u16).to_be_bytes());
        malformed_certificate.extend_from_slice(&certificate_message);
        packet.extend_from_slice(&malformed_certificate);
        let mut server = server_version.endpoint(config.clone(), certificate.clone(), base);
        server.handle_timeout(base).expect("server clock");
        drain(&mut server);
        server
            .handle_packet(&packet)
            .expect("accept ClientHello and discard malformed certificate");
        let output = drain(&mut server);
        assert!(!output.0.is_empty());
        assert_eq!(
            output.1,
            base + BUDGET,
            "{client_version:?} -> {server_version:?}"
        );
        assert_expires_at(&mut server, base + BUDGET);
    }
}

#[test]
fn clients_start_at_emission_not_construction_or_timeout() {
    let base = Instant::now();
    let certificate = generate_self_signed_certificate().expect("certificate");
    for &version in VERSIONS {
        let mut client = version.endpoint(config(), certificate.clone(), base);
        let late = base + Duration::from_secs(60);
        client
            .handle_timeout(late)
            .expect("idle server has no deadline");
        assert!(drain(&mut client).1 > late);
        client.set_active(true);
        client
            .handle_timeout(late)
            .expect("queue initial ClientHello");
        // Test-only polling exception (#161): keep ClientHello queued across a
        // clock update to prove its timers start at emission, not construction.
        let emission = late + Duration::from_secs(60);
        client
            .handle_timeout(emission)
            .expect("unsent flight has no timer");
        let (packets, deadline) = drain(&mut client);
        assert!(!packets.is_empty(), "{version:?}");
        assert_eq!(deadline, emission + BUDGET, "{version:?}");
        client
            .handle_timeout(deadline - Duration::from_nanos(1))
            .expect("full budget remains");
        assert_eq!(drain(&mut client).1, deadline);
        assert_eq!(
            client.handle_timeout(deadline),
            Err(Error::Timeout(TimeoutError::Connect))
        );
    }
}

#[test]
fn too_small_output_does_not_start_or_exhaust_client_timers() {
    let base = Instant::now();
    let certificate = generate_self_signed_certificate().expect("certificate");
    for &version in VERSIONS {
        let mut client = version.endpoint(config(), certificate.clone(), base);
        client.set_active(true);
        client.handle_timeout(base).expect("queue ClientHello");
        // Test-only polling exception (#161): retain the packet after each
        // BufferTooSmall to verify neither timer runs before successful output.
        for seconds in [1, 10, 100] {
            client
                .handle_timeout(base + Duration::from_secs(seconds))
                .expect("not emitted");
            assert!(matches!(
                client.poll_output(&mut []),
                Output::BufferTooSmall { .. }
            ));
        }
        let emission = base + Duration::from_secs(100);
        let (packets, deadline) = drain(&mut client);
        assert_eq!(packets.len(), 1, "{version:?}");
        assert_eq!(deadline, emission + BUDGET, "{version:?}");
        assert_eq!(
            client.handle_timeout(deadline),
            Err(Error::Timeout(TimeoutError::Connect))
        );
    }
}

#[test]
fn too_small_retry_output_does_not_start_or_exhaust_flight_timers() {
    let base = Instant::now();
    let certificate = generate_self_signed_certificate().expect("certificate");
    let initial_rto = Duration::from_millis(20);
    let config = Arc::new(
        Config::builder()
            .dangerously_set_rng_seed(42)
            .handshake_timeout(Duration::from_secs(10))
            .flight_start_rto(initial_rto)
            .flight_retries(1)
            .build()
            .expect("retry configuration"),
    );
    for &version in VERSIONS {
        for active in [false, true] {
            let (mut endpoint, original, retry_at) =
                start_endpoint(version, active, config.clone(), &certificate, base);
            endpoint.handle_timeout(retry_at).expect("queue retry");
            assert!(matches!(
                endpoint.poll_output(&mut []),
                Output::BufferTooSmall { .. }
            ));

            let emission = retry_at + Duration::from_secs(1);
            endpoint
                .handle_timeout(emission)
                .expect("unemitted retry has no running timer");
            assert!(matches!(
                endpoint.poll_output(&mut []),
                Output::BufferTooSmall { .. }
            ));

            let (retried, deadline) = drain(&mut endpoint);
            assert_eq!(
                retried.len(),
                original.len(),
                "{version:?}, active={active}"
            );
            let nominal = initial_rto * 2;
            assert!(deadline >= emission + nominal.mul_f64(0.75));
            assert!(deadline <= emission + nominal.mul_f64(1.25));
        }
    }
}

#[test]
fn servers_start_at_late_client_hello_reception() {
    let base = Instant::now();
    let certificate = generate_self_signed_certificate().expect("certificate");
    for &version in VERSIONS {
        let mut server = version.endpoint(config(), certificate.clone(), base);
        for seconds in [1, 10, 100] {
            let now = base + Duration::from_secs(seconds);
            server.handle_timeout(now).expect("passive server");
            assert!(drain(&mut server).1 > now);
            server
                .handle_packet(&[0, 1, 2])
                .expect("malformed input is discarded");
            assert!(drain(&mut server).1 > now);
        }
        let reception = base + Duration::from_secs(100);
        let mut client = version.endpoint(config(), certificate.clone(), reception);
        client.set_active(true);
        client.handle_timeout(reception).expect("queue ClientHello");
        for packet in drain(&mut client).0 {
            server.handle_packet(&packet).expect("accept ClientHello");
            drain(&mut server);
        }
        let deadline = drain(&mut server).1;
        assert_eq!(deadline, reception + BUDGET, "{version:?}");
        assert_eq!(
            server.handle_timeout(deadline),
            Err(Error::Timeout(TimeoutError::Connect))
        );
    }
}

#[test]
fn configured_rto_jitter_and_exact_retry_counts_apply_to_every_role() {
    let base = Instant::now();
    let certificate = generate_self_signed_certificate().expect("certificate");
    for initial_rto in [
        Duration::from_nanos(1),
        Duration::from_micros(80),
        Duration::from_millis(20),
    ] {
        for retries in [0, 1, 3] {
            let config = Arc::new(
                Config::builder()
                    .dangerously_set_rng_seed(42)
                    .handshake_timeout(Duration::from_secs(3600))
                    .flight_start_rto(initial_rto)
                    .flight_retries(retries)
                    .build()
                    .expect("valid retry configuration"),
            );
            for &version in VERSIONS {
                for active in [false, true] {
                    let (mut endpoint, original, mut deadline) =
                        start_endpoint(version, active, config.clone(), &certificate, base);
                    let mut now = base;
                    for attempt in 0..=retries {
                        let nominal = initial_rto * (1 << attempt);
                        let interval = deadline.duration_since(now);
                        assert!(interval >= nominal.mul_f64(0.75).max(Duration::from_nanos(1)));
                        assert!(interval <= nominal.mul_f64(1.25).max(Duration::from_nanos(1)));
                        endpoint
                            .handle_timeout(deadline - Duration::from_nanos(1))
                            .expect("not yet due");
                        let before = drain(&mut endpoint);
                        assert!(before.0.is_empty());
                        assert_eq!(before.1, deadline);
                        if attempt == retries {
                            let reason = if active && matches!(version, Version::Auto) {
                                TimeoutError::HybridClientHello
                            } else {
                                TimeoutError::Handshake
                            };
                            assert_eq!(
                                endpoint.handle_timeout(deadline),
                                Err(Error::Timeout(reason))
                            );
                        } else {
                            endpoint.handle_timeout(deadline).expect("retry available");
                            now = deadline;
                            let output = drain(&mut endpoint);
                            assert_eq!(
                                output.0.len(),
                                original.len(),
                                "{version:?}, active={active}"
                            );
                            deadline = output.1;
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn overall_deadline_precedes_long_rto_with_generous_retries() {
    let base = Instant::now();
    let certificate = generate_self_signed_certificate().expect("certificate");
    let config = Arc::new(
        Config::builder()
            .dangerously_set_rng_seed(42)
            .handshake_timeout(BUDGET)
            .flight_start_rto(LONG_RTO)
            .flight_retries(100)
            .build()
            .expect("valid timing config"),
    );
    for &version in VERSIONS {
        for active in [false, true] {
            let (mut endpoint, _, deadline) =
                start_endpoint(version, active, config.clone(), &certificate, base);
            assert_eq!(deadline, base + BUDGET, "{version:?}, active={active}");
            assert_eq!(
                endpoint.handle_timeout(deadline),
                Err(Error::Timeout(TimeoutError::Connect))
            );
        }
    }
}

#[test]
fn duplicate_client_hellos_share_the_flight_retry_budget() {
    let base = Instant::now();
    let certificate = generate_self_signed_certificate().expect("certificate");
    let initial_rto = Duration::from_millis(20);
    for use_cookie in [false, true] {
        for (retries, timed_retries) in [(0, 0), (1, 0), (2, 0), (2, 1)] {
            let config = Arc::new(
                Config::builder()
                    .dangerously_set_rng_seed(42)
                    .use_server_cookie(use_cookie)
                    .flight_start_rto(initial_rto)
                    .flight_retries(retries)
                    .handshake_timeout(Duration::from_secs(1))
                    .build()
                    .expect("retry config"),
            );
            for &(client_version, server_version) in PAIRS {
                let mut client = client_version.endpoint(config.clone(), certificate.clone(), base);
                client.set_active(true);
                client.handle_timeout(base).expect("client clock");
                let (hello, _) = drain(&mut client);
                assert_eq!(hello.len(), 1);
                let mut server = server_version.endpoint(config.clone(), certificate.clone(), base);
                server.handle_timeout(base).expect("server clock");
                drain(&mut server);
                server
                    .handle_packet(&hello[0])
                    .expect("initial ClientHello");
                let (original, mut retry_at) = drain(&mut server);
                assert!(!original.is_empty());
                let suppress_duplicate_bursts =
                    server.protocol_version() == Some(ProtocolVersion::DTLS1_2);
                let mut now = base;
                for _ in 0..timed_retries {
                    server.handle_timeout(retry_at).expect("timer retry");
                    now = retry_at;
                    let output = drain(&mut server);
                    assert_eq!(output.0.len(), original.len());
                    retry_at = output.1;
                }
                let remaining = retries - timed_retries;
                let mut resends = timed_retries;
                for duplicate in 0..remaining + 2 {
                    now += Duration::from_millis(1);
                    server.handle_timeout(now).expect("before retry timer");
                    assert!(drain(&mut server).0.is_empty());
                    server
                        .handle_packet(&hello[0])
                        .expect("duplicate ClientHello");
                    let output = drain(&mut server);
                    let expected_packets =
                        if resends < retries && (!suppress_duplicate_bursts || resends == 0) {
                            original.len()
                        } else {
                            0
                        };
                    assert_eq!(
                        output.0.len(),
                        expected_packets,
                        "{client_version:?} -> {server_version:?}, retries={retries}, duplicate={duplicate}"
                    );
                    if expected_packets > 0 {
                        resends += 1;
                        let nominal = initial_rto * (1 << resends);
                        assert!(output.1 >= now + nominal.mul_f64(0.75));
                        assert!(output.1 <= now + nominal.mul_f64(1.25));
                        retry_at = output.1;
                    } else {
                        assert_eq!(output.1, retry_at);
                    }
                }
                for attempt in resends..retries {
                    now = retry_at;
                    server.handle_timeout(now).expect("remaining timer retry");
                    let output = drain(&mut server);
                    assert_eq!(output.0.len(), original.len());
                    let nominal = initial_rto * (1 << (attempt + 1));
                    assert!(output.1 >= now + nominal.mul_f64(0.75));
                    assert!(output.1 <= now + nominal.mul_f64(1.25));
                    retry_at = output.1;
                }
                assert_eq!(
                    server.handle_timeout(retry_at),
                    Err(Error::Timeout(TimeoutError::Handshake))
                );
            }
        }
    }
}

#[test]
fn zero_retry_servers_answer_every_retransmitted_final_flight() {
    let base = Instant::now();
    let certificate = generate_self_signed_certificate().expect("certificate");
    let config = |retries| {
        Arc::new(
            Config::builder()
                .dangerously_set_rng_seed(42)
                .handshake_timeout(Duration::from_secs(60))
                .flight_start_rto(Duration::from_millis(20))
                .flight_retries(retries)
                .build()
                .expect("retry configuration"),
        )
    };
    for &(client_version, server_version) in PAIRS {
        let mut client = client_version.endpoint(config(3), certificate.clone(), base);
        let mut server = server_version.endpoint(config(0), certificate.clone(), base);
        client.set_active(true);
        client.handle_timeout(base).expect("client clock");
        server.handle_timeout(base).expect("server clock");
        drain(&mut server);
        let mut client_connected = false;
        let mut to_server = drain(&mut client).0;
        let mut lost_final_flight = Vec::new();
        'handshake: for _ in 0..20 {
            let mut to_client = Vec::new();
            for packet in mem::take(&mut to_server) {
                server.handle_packet(&packet).expect("server receives");
                let output = drain_outputs(&mut server);
                if output.connected {
                    lost_final_flight = output.packets;
                    break 'handshake;
                }
                to_client.extend(output.packets);
            }
            for packet in to_client {
                client.handle_packet(&packet).expect("client receives");
                let output = drain_outputs(&mut client);
                client_connected |= output.connected;
                to_server.extend(output.packets);
            }
        }
        assert!(!lost_final_flight.is_empty(), "server sends a final flight");

        let mut answer = Vec::new();
        for round in 0..2 {
            let retry_at = drain(&mut client).1;
            client
                .handle_timeout(retry_at)
                .expect("client retransmits its final flight");
            server.handle_timeout(retry_at).expect("completed server");
            assert!(drain(&mut server).0.is_empty(), "no timer-driven resend");
            answer.clear();
            for packet in drain(&mut client).0 {
                server
                    .handle_packet(&packet)
                    .expect("retransmitted final flight");
                answer.extend(drain(&mut server).0);
            }
            assert!(
                !answer.is_empty(),
                "{client_version:?} -> {server_version:?}, round {round}"
            );
        }
        for packet in &answer {
            client.handle_packet(packet).expect("final flight answer");
            client_connected |= drain_outputs(&mut client).connected;
        }
        assert!(client_connected, "{client_version:?} -> {server_version:?}");
        let later = base + Duration::from_secs(30);
        client
            .handle_timeout(later)
            .expect("client handshake settled");
        assert!(
            drain(&mut client).0.is_empty(),
            "{client_version:?} -> {server_version:?}: client still retransmits"
        );
    }
}

#[test]
fn delayed_auto_client_selection_keeps_only_the_original_remaining_budget() {
    let base = Instant::now();
    let certificate = generate_self_signed_certificate().expect("certificate");
    for peer_version in [Version::Dtls12, Version::Dtls13] {
        let mut client = Version::Auto.endpoint(config(), certificate.clone(), base);
        client.set_active(true);
        client.handle_timeout(base).expect("initial clock");
        let (hello, original_deadline) = drain(&mut client);
        assert_eq!(client.protocol_version(), None);
        let selection = base + Duration::from_millis(70);
        client
            .handle_timeout(selection)
            .expect("remaining initial budget");
        assert_eq!(drain(&mut client).1, original_deadline);
        let mut server = peer_version.endpoint(config(), certificate.clone(), selection);
        server.handle_timeout(selection).expect("server clock");
        drain(&mut server);
        let mut responses = Vec::new();
        for packet in hello {
            server
                .handle_packet(&packet)
                .expect("peer accepts hybrid ClientHello");
            responses.extend(drain(&mut server).0);
        }
        assert!(!responses.is_empty());
        let mut second_hello = Vec::new();
        for packet in responses {
            client.handle_packet(&packet).expect("version handoff");
            let output = drain(&mut client);
            second_hello.extend(output.0);
            assert_eq!(output.1, original_deadline);
        }
        assert!(!second_hello.is_empty());
        let expected = match peer_version {
            Version::Dtls12 => ProtocolVersion::DTLS1_2,
            Version::Dtls13 => ProtocolVersion::DTLS1_3,
            Version::Auto => unreachable!(),
        };
        assert_eq!(client.protocol_version(), Some(expected));
        assert_eq!(
            client.handle_timeout(original_deadline),
            Err(Error::Timeout(TimeoutError::Connect))
        );
    }
}

#[test]
fn delayed_auto_client_without_cookie_keeps_deadline_while_waiting_for_server_flight() {
    let base = Instant::now();
    let certificate = generate_self_signed_certificate().expect("certificate");
    for (peer_version, expected_version) in [
        (Version::Dtls12, ProtocolVersion::DTLS1_2),
        (Version::Dtls13, ProtocolVersion::DTLS1_3),
    ] {
        let mut client = Version::Auto.endpoint(config(), certificate.clone(), base);
        client.set_active(true);
        client.handle_timeout(base).expect("client clock");
        let (hello, original_deadline) = drain(&mut client);
        let selection = base + Duration::from_millis(70);
        client
            .handle_timeout(selection)
            .expect("remaining initial budget");
        assert_eq!(drain(&mut client).1, original_deadline);

        let server_config = Arc::new(
            Config::builder()
                .dangerously_set_rng_seed(42)
                .use_server_cookie(false)
                .mtu(128)
                .max_queue_tx(30)
                .build()
                .expect("fragmented server flight"),
        );
        let mut server = peer_version.endpoint(server_config, certificate.clone(), selection);
        server.handle_timeout(selection).expect("server clock");
        drain(&mut server);
        let mut responses = Vec::new();
        for packet in hello {
            server.handle_packet(&packet).expect("hybrid ClientHello");
            responses.extend(drain(&mut server).0);
        }
        assert!(responses.len() > 1, "withhold part of the server flight");
        client
            .handle_packet(&responses[0])
            .expect("version selection without a cookie");
        let output = drain_outputs(&mut client);
        assert!(!output.connected);
        assert!(output.packets.is_empty());
        assert_eq!(client.protocol_version(), Some(expected_version));
        assert_eq!(output.timeout, Some(original_deadline));
        assert_expires_at(&mut client, original_deadline);
    }
}

#[test]
fn auto_handoff_preserves_outstanding_flight_retries() {
    let base = Instant::now();
    let certificate = generate_self_signed_certificate().expect("certificate");
    let initial_rto = Duration::from_millis(20);
    for peer_version in [Version::Dtls12, Version::Dtls13] {
        for (retries, used) in [(0, 0), (2, 0), (2, 1)] {
            let config = Arc::new(
                Config::builder()
                    .dangerously_set_rng_seed(42)
                    .flight_start_rto(initial_rto)
                    .flight_retries(retries)
                    .handshake_timeout(Duration::from_secs(1))
                    .build()
                    .expect("client config"),
            );
            let mut client = Version::Auto.endpoint(config, certificate.clone(), base);
            client.set_active(true);
            client.handle_timeout(base).expect("client clock");
            let (mut hello, mut retry_at) = drain(&mut client);
            let mut now = base;
            for _ in 0..used {
                client.handle_timeout(retry_at).expect("pending Auto retry");
                now = retry_at;
                (hello, retry_at) = drain(&mut client);
            }
            assert_eq!(hello.len(), 1);
            let selection = now + Duration::from_millis(1);
            client
                .handle_timeout(selection)
                .expect("before outstanding retry");
            assert_eq!(drain(&mut client).1, retry_at);
            let server_config = Arc::new(
                Config::builder()
                    .use_server_cookie(false)
                    .mtu(128)
                    .max_queue_tx(30)
                    .build()
                    .expect("fragmented server config"),
            );
            let mut server = peer_version.endpoint(server_config, certificate.clone(), selection);
            server.handle_timeout(selection).expect("server clock");
            drain(&mut server);
            server.handle_packet(&hello[0]).expect("hybrid ClientHello");
            let (responses, _) = drain(&mut server);
            assert!(responses.len() > 1);
            client
                .handle_packet(&responses[0])
                .expect("partial server flight handoff");
            assert_eq!(
                drain(&mut client).1,
                retry_at,
                "{peer_version:?}, retries={retries}, used={used}"
            );
            for attempt in used..retries {
                client
                    .handle_timeout(retry_at)
                    .expect("remaining retry after handoff");
                let sent_at = retry_at;
                let output = drain(&mut client);
                assert_eq!(output.0.len(), 1);
                assert_eq!(
                    &output.0[0][RECORD_HEADER_LEN..],
                    &hello[0][RECORD_HEADER_LEN..]
                );
                let nominal = initial_rto * (1 << (attempt + 1));
                assert!(output.1 >= sent_at + nominal.mul_f64(0.75));
                assert!(output.1 <= sent_at + nominal.mul_f64(1.25));
                retry_at = output.1;
            }
            assert_eq!(
                client.handle_timeout(retry_at),
                Err(Error::Timeout(TimeoutError::Handshake))
            );
        }
    }
}

#[test]
fn first_client_fragment_starts_deadline_and_later_emissions_do_not_refresh_it() {
    let base = Instant::now();
    let certificate = generate_self_signed_certificate().expect("certificate");
    let config = Arc::new(
        Config::builder()
            .dangerously_set_rng_seed(42)
            .mtu(64)
            .handshake_timeout(BUDGET)
            .flight_start_rto(LONG_RTO)
            .flight_retries(0)
            .build()
            .expect("fragmented configuration"),
    );
    for version in [Version::Dtls12, Version::Dtls13] {
        let mut client = version.endpoint(config.clone(), certificate.clone(), base);
        client.set_active(true);
        client.handle_timeout(base).expect("queue fragments");
        // Test-only polling exception (#161): leave the first fragment pending
        // after BufferTooSmall while advancing the logical clock.
        assert!(matches!(
            client.poll_output(&mut []),
            Output::BufferTooSmall { .. }
        ));
        let emission = base + Duration::from_secs(60);
        client
            .handle_timeout(emission)
            .expect("no emitted fragments yet");
        let mut buffer = [0; 64];
        assert!(matches!(client.poll_output(&mut buffer), Output::Packet(_)));
        // Test-only polling exception (#161): pause after the first fragment
        // to verify later fragments cannot refresh the original deadline.
        client
            .handle_timeout(emission + Duration::from_millis(70))
            .expect("first fragment has full budget");
        let (remaining, deadline) = drain(&mut client);
        assert!(!remaining.is_empty());
        assert_eq!(deadline, emission + BUDGET);
        assert_eq!(
            client.handle_timeout(deadline),
            Err(Error::Timeout(TimeoutError::Connect))
        );
    }
}

#[test]
fn server_fragments_duplicates_and_auto_fallback_preserve_first_reception() {
    let base = Instant::now();
    let certificate = generate_self_signed_certificate().expect("certificate");
    let config = Arc::new(
        Config::builder()
            .dangerously_set_rng_seed(42)
            .mtu(64)
            .handshake_timeout(BUDGET)
            .flight_start_rto(LONG_RTO)
            .flight_retries(0)
            .build()
            .expect("fragmented configuration"),
    );
    for (client_version, server_version) in [
        (Version::Dtls12, Version::Dtls12),
        (Version::Dtls13, Version::Dtls13),
        (Version::Dtls12, Version::Auto),
        (Version::Dtls13, Version::Auto),
    ] {
        for reverse in [false, true] {
            for complete in [false, true] {
                let reception = base + Duration::from_secs(60);
                let mut client =
                    client_version.endpoint(config.clone(), certificate.clone(), reception);
                client.set_active(true);
                client
                    .handle_timeout(reception)
                    .expect("queue fragmented ClientHello");
                let mut fragments = drain(&mut client).0;
                assert!(fragments.len() > 1);
                if reverse {
                    fragments.reverse();
                }
                let mut server = server_version.endpoint(config.clone(), certificate.clone(), base);
                server
                    .handle_timeout(reception)
                    .expect("late passive server");
                assert!(drain(&mut server).1 > reception + BUDGET);
                server
                    .handle_packet(&fragments[0])
                    .expect("accept first arriving fragment");
                let first_output = drain(&mut server);
                assert!(first_output.0.is_empty());
                assert_eq!(first_output.1, reception + BUDGET);
                server
                    .handle_timeout(reception + Duration::from_millis(20))
                    .expect("incomplete assembly");
                drain(&mut server);
                server
                    .handle_packet(&fragments[0])
                    .expect("duplicate fragment");
                assert_eq!(drain(&mut server).1, first_output.1);
                if complete {
                    server
                        .handle_timeout(reception + Duration::from_millis(70))
                        .expect("remaining assembly budget");
                    drain(&mut server);
                    let mut responses = Vec::new();
                    for fragment in &fragments[1..] {
                        server
                            .handle_packet(fragment)
                            .expect("complete ClientHello");
                        let output = drain(&mut server);
                        responses.extend(output.0);
                        assert_eq!(output.1, first_output.1);
                    }
                    assert!(!responses.is_empty());
                    if matches!(server_version, Version::Auto)
                        && matches!(client_version, Version::Dtls13)
                    {
                        assert_eq!(server.protocol_version(), None);
                    } else {
                        let expected = if matches!(client_version, Version::Dtls12) {
                            ProtocolVersion::DTLS1_2
                        } else {
                            ProtocolVersion::DTLS1_3
                        };
                        assert_eq!(server.protocol_version(), Some(expected));
                    }
                }
                assert_eq!(
                    server.handle_timeout(first_output.1),
                    Err(Error::Timeout(TimeoutError::Connect))
                );
            }
        }
    }
}

#[test]
fn server_clock_start_depends_on_fragment_structure_not_body_validity() {
    let base = Instant::now();
    let certificate = generate_self_signed_certificate().expect("certificate");
    for &version in VERSIONS {
        let (_, hello, _) = start_endpoint(version, true, config(), &certificate, base);
        assert_eq!(hello.len(), 1);
        let hello = &hello[0];
        let mut inputs = vec![("unrelated bytes", vec![0, 1, 2])];

        let mut empty = hello[..HANDSHAKE_BODY].to_vec();
        empty[RECORD_LENGTH].copy_from_slice(&(HANDSHAKE_HEADER_LEN as u16).to_be_bytes());
        empty[MESSAGE_LENGTH].fill(0);
        empty[FRAGMENT_LENGTH].fill(0);
        inputs.push(("empty ClientHello", empty));

        let mut zero_fragment = hello[..HANDSHAKE_BODY].to_vec();
        zero_fragment[RECORD_LENGTH].copy_from_slice(&(HANDSHAKE_HEADER_LEN as u16).to_be_bytes());
        zero_fragment[FRAGMENT_LENGTH].fill(0);
        inputs.push(("zero-length fragment", zero_fragment));

        let mut outside_message = hello.clone();
        outside_message[FRAGMENT_OFFSET].fill(0xff);
        inputs.push(("fragment outside message", outside_message));

        let mut malformed_body = hello.clone();
        // An empty session id and cookie leave the cipher suites length next.
        let cipher_suites_length = SESSION_ID_LENGTH + 2;
        assert_eq!(
            &malformed_body[SESSION_ID_LENGTH..cipher_suites_length],
            &[0, 0]
        );
        malformed_body[cipher_suites_length..cipher_suites_length + 2].fill(0xff);
        inputs.push(("malformed complete body", malformed_body));

        // A well-framed but too-short body still starts the peer's attempt.
        // Its rejection belongs to the body parser, not the timer.
        let mut short_body = hello[..HANDSHAKE_BODY + 1].to_vec();
        short_body[RECORD_LENGTH]
            .copy_from_slice(&((HANDSHAKE_HEADER_LEN + 1) as u16).to_be_bytes());
        short_body[MESSAGE_LENGTH].copy_from_slice(&1u32.to_be_bytes()[1..]);
        short_body[FRAGMENT_LENGTH].copy_from_slice(&1u32.to_be_bytes()[1..]);
        inputs.push(("short complete body", short_body));

        let mut future_message = hello.clone();
        future_message[MESSAGE_SEQ].copy_from_slice(&1u16.to_be_bytes());
        inputs.push(("unexpected handshake sequence", future_message));

        let mut encrypted = hello.clone();
        encrypted[CONTENT_TYPE] = 23;
        encrypted[RECORD_EPOCH].copy_from_slice(&1u16.to_be_bytes());
        inputs.push(("unrelated encrypted application data", encrypted));

        let mut ccs = hello[..RECORD_HEADER_LEN].to_vec();
        ccs[CONTENT_TYPE] = 20;
        ccs[RECORD_LENGTH].copy_from_slice(&1u16.to_be_bytes());
        ccs.push(1);
        inputs.push(("unrelated ChangeCipherSpec", ccs));

        for (label, input) in inputs {
            let mut server = version.endpoint(config(), certificate.clone(), base);
            server.handle_timeout(base).expect("passive server");
            drain(&mut server);
            server
                .handle_packet(&input)
                .expect("discard or defer unrelated input");
            let output = drain(&mut server);
            assert!(output.0.is_empty(), "{version:?}: {label}");
            if matches!(label, "malformed complete body" | "short complete body") {
                assert_eq!(output.1, base + BUDGET, "{version:?}: {label}");
                assert_expires_at(&mut server, base + BUDGET);
                continue;
            }
            assert!(
                output.1 > base + BUDGET,
                "{version:?}: {label} started the clock"
            );
            let reception = base + Duration::from_secs(60);
            server
                .handle_timeout(reception)
                .expect("rejected input did not arm timers");
            drain(&mut server);
        }
    }
}

#[test]
fn cookie_exchanges_and_fresh_flights_reset_only_retry_state() {
    let base = Instant::now();
    let certificate = generate_self_signed_certificate().expect("certificate");
    let rto = Duration::from_millis(20);
    let config = Arc::new(
        Config::builder()
            .dangerously_set_rng_seed(42)
            .handshake_timeout(BUDGET)
            .flight_start_rto(rto)
            .flight_retries(8)
            .build()
            .expect("valid cookie configuration"),
    );
    for &(client_version, server_version) in PAIRS {
        let mut client = client_version.endpoint(config.clone(), certificate.clone(), base);
        let mut server = server_version.endpoint(config.clone(), certificate.clone(), base);
        client.set_active(true);
        client.handle_timeout(base).expect("start client");
        server.handle_timeout(base).expect("server clock");
        drain(&mut server);
        let (hello, client_retry) = drain(&mut client);
        let mut cookie_packets = Vec::new();
        let mut server_retry = base;
        for packet in hello {
            server.handle_packet(&packet).expect("initial ClientHello");
            let output = drain(&mut server);
            cookie_packets.extend(output.0);
            server_retry = output.1;
        }
        assert!(!cookie_packets.is_empty());
        client
            .handle_timeout(client_retry)
            .expect("retry ClientHello");
        assert!(!drain(&mut client).0.is_empty());
        server
            .handle_timeout(server_retry)
            .expect("retry cookie challenge");
        assert!(!drain(&mut server).0.is_empty());
        let now = client_retry.max(server_retry) + Duration::from_millis(1);
        client
            .handle_timeout(now)
            .expect("client clock before challenge");
        server
            .handle_timeout(now)
            .expect("server clock before challenge response");
        drain(&mut client);
        drain(&mut server);

        let mut cookie_hello = Vec::new();
        let mut next_client = now;
        for packet in &cookie_packets {
            client.handle_packet(packet).expect("cookie challenge");
            let output = drain(&mut client);
            cookie_hello.extend(output.0);
            next_client = output.1;
        }
        assert!(!cookie_hello.is_empty());
        assert!(next_client >= now + rto.mul_f64(0.75));
        assert!(next_client <= now + rto.mul_f64(1.25));
        for packet in cookie_packets {
            client
                .handle_packet(&packet)
                .expect("duplicate cookie challenge");
            let output = drain(&mut client);
            if client.protocol_version() == Some(ProtocolVersion::DTLS1_2) {
                assert!(!output.0.is_empty());
                assert!(output.1 >= now + (rto * 2).mul_f64(0.75));
                assert!(output.1 <= now + (rto * 2).mul_f64(1.25));
            } else {
                assert!(output.0.is_empty());
                assert_eq!(output.1, next_client);
            }
        }

        let mut server_flight = Vec::new();
        let mut next_server = now;
        for packet in cookie_hello {
            server
                .handle_packet(&packet)
                .expect("ClientHello with cookie");
            let output = drain(&mut server);
            server_flight.extend(output.0);
            next_server = output.1;
        }
        assert!(!server_flight.is_empty());
        assert!(next_server >= now + rto.mul_f64(0.75));
        assert!(next_server <= now + rto.mul_f64(1.25));
        assert_expires_at(&mut client, base + BUDGET);
        assert_expires_at(&mut server, base + BUDGET);
    }
}

#[test]
fn completed_handshakes_stay_connected_after_deadline_and_key_updates() {
    let base = Instant::now();
    let certificate = generate_self_signed_certificate().expect("certificate");
    for use_cookie in [false, true] {
        let config = Arc::new(
            Config::builder()
                .dangerously_set_rng_seed(42)
                .use_server_cookie(use_cookie)
                .handshake_timeout(BUDGET)
                .flight_start_rto(Duration::from_millis(20))
                .flight_retries(3)
                .aead_encryption_limit(3)
                .build()
                .expect("small key-update threshold"),
        );
        for &versions in PAIRS {
            let (mut client, mut server) =
                connected_pair(versions, config.clone(), &certificate, base);
            for round in 0..6 {
                let now = base + Duration::from_secs(60 + round);
                client
                    .handle_timeout(now)
                    .expect("completed client deadline disabled");
                server
                    .handle_timeout(now)
                    .expect("completed server deadline disabled");
                let idle = exchange(&mut client, &mut server, now);
                assert!(idle.0.timeout.expect("client timeout") > now);
                assert!(idle.1.timeout.expect("server timeout") > now);

                client
                    .send_application_data(b"client traffic")
                    .expect("client send");
                let received = exchange(&mut client, &mut server, now);
                assert_eq!(received.1.app_data, [b"client traffic".to_vec()]);
                server
                    .send_application_data(b"server traffic")
                    .expect("server send");
                let received = exchange(&mut client, &mut server, now);
                assert_eq!(received.0.app_data, [b"server traffic".to_vec()]);
                assert!(received.0.timeout.expect("client timeout") > now);
                assert!(received.1.timeout.expect("server timeout") > now);
            }
        }
    }
}

#[test]
fn key_update_uses_configured_retries_without_restarting_handshake_budget() {
    let base = Instant::now();
    let certificate = generate_self_signed_certificate().expect("certificate");
    let rto = Duration::from_millis(20);
    for retries in [0, 2] {
        let config = Arc::new(
            Config::builder()
                .dangerously_set_rng_seed(42)
                .handshake_timeout(Duration::from_millis(5))
                .flight_start_rto(rto)
                .flight_retries(retries)
                .aead_encryption_limit(3)
                .build()
                .expect("small handshake budget and key-update threshold"),
        );
        for versions in [
            (Version::Dtls13, Version::Dtls13),
            (Version::Auto, Version::Dtls13),
            (Version::Dtls13, Version::Auto),
            (Version::Auto, Version::Auto),
        ] {
            for client_sends in [false, true] {
                let (mut client, mut server) =
                    connected_pair(versions, config.clone(), &certificate, base);
                let now = base + Duration::from_secs(60);
                client
                    .handle_timeout(now)
                    .expect("client idle after completion");
                server
                    .handle_timeout(now)
                    .expect("server idle after completion");
                exchange(&mut client, &mut server, now);
                let sender = if client_sends {
                    &mut client
                } else {
                    &mut server
                };
                let mut next_retry = None;
                for _ in 0..3 {
                    sender
                        .send_application_data(b"trigger key update")
                        .expect("application data");
                    assert!(!drain(sender).0.is_empty());
                    sender.handle_timeout(now).expect("trigger KeyUpdate");
                    let output = drain(sender);
                    if output.1 < now + Duration::from_secs(1) {
                        assert!(!output.0.is_empty());
                        next_retry = Some(output.1);
                        break;
                    }
                }
                let mut deadline =
                    next_retry.expect("AEAD threshold must trigger a KeyUpdate flight");
                let mut sent_at = now;
                for attempt in 0..=retries {
                    let nominal = rto * (1 << attempt);
                    assert!(deadline >= sent_at + nominal.mul_f64(0.75));
                    assert!(deadline <= sent_at + nominal.mul_f64(1.25));
                    if attempt == retries {
                        assert_eq!(
                            sender.handle_timeout(deadline),
                            Err(Error::Timeout(TimeoutError::Handshake))
                        );
                    } else {
                        sender.handle_timeout(deadline).expect("KeyUpdate retry");
                        sent_at = deadline;
                        let output = drain(sender);
                        assert!(!output.0.is_empty());
                        deadline = output.1;
                    }
                }
            }
        }
    }
}

#[test]
fn server_deadline_survives_client_certificate_until_finished() {
    let base = Instant::now();
    let certificate = generate_self_signed_certificate().expect("certificate");
    for use_cookie in [false, true] {
        let config = Arc::new(
            Config::builder()
                .dangerously_set_rng_seed(42)
                .use_server_cookie(use_cookie)
                .mtu(128)
                .max_queue_tx(30)
                .handshake_timeout(BUDGET)
                .flight_start_rto(LONG_RTO)
                .flight_retries(100)
                .build()
                .expect("fragmented certificate configuration"),
        );
        for (client_version, server_version) in [
            (Version::Dtls13, Version::Dtls13),
            (Version::Auto, Version::Dtls13),
            (Version::Dtls13, Version::Auto),
            (Version::Auto, Version::Auto),
        ] {
            let mut client = client_version.endpoint(config.clone(), certificate.clone(), base);
            let mut server = server_version.endpoint(config.clone(), certificate.clone(), base);
            client.set_active(true);
            client.handle_timeout(base).expect("start client");
            server.handle_timeout(base).expect("server clock");
            let mut client_output = drain_outputs(&mut client);
            let mut server_output = drain_outputs(&mut server);
            let mut saw_certificate = false;
            'handshake: for _ in 0..20 {
                for packet in mem::take(&mut client_output.packets) {
                    server
                        .handle_packet(&packet)
                        .expect("receive client flight");
                    let output = drain_outputs(&mut server);
                    if output.peer_cert.is_some() {
                        assert!(!output.connected, "Finished must still be withheld");
                        assert_eq!(output.timeout, Some(base + BUDGET));
                        saw_certificate = true;
                        break 'handshake;
                    }
                    merge_output(&mut server_output, output);
                }
                deliver_queued(&mut server_output, &mut client, &mut client_output);
            }
            assert!(
                saw_certificate,
                "must stop between Certificate and Finished"
            );
            server
                .handle_timeout(base + Duration::from_millis(70))
                .expect("remaining handshake budget");
            assert_eq!(drain(&mut server).1, base + BUDGET);
            assert_expires_at(&mut server, base + BUDGET);
        }
    }
}
