use crate::cid_generator::HashedConnectionIdGenerator;
use std::{
	any::Any,
	collections::{BTreeMap, BTreeSet},
	convert::TryInto,
	iter, mem,
	net::{Ipv4Addr, Ipv6Addr, SocketAddr},
	num::NonZeroU64,
	sync::{
		Arc, Mutex,
		atomic::{AtomicU64, Ordering},
	},
};

use assert_matches::assert_matches;
use bytes::{Buf, Bytes, BytesMut};
use hex_literal::hex;
use rand::Rng;
#[cfg(all(feature = "aws-lc-rs", not(feature = "ring")))]
use rustls::crypto::aws_lc_rs::default_provider;
#[cfg(feature = "ring")]
use rustls::crypto::ring::default_provider;
use rustls::{
	AlertDescription, RootCertStore,
	pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
	server::WebPkiClientVerifier,
};
use tracing::info;

use super::*;
use crate::{
	Duration, Instant,
	cid_generator::{ConnectionIdGenerator, RandomConnectionIdGenerator},
	congestion::{self, Controller, ControllerFactory, ControllerMetrics},
	connection::{Connection, Error as ConnectionError},
	crypto::rustls::QuicServerConfig,
	endpoint::{Accepting, ConnectionHandle, DatagramEvent, Dial, Endpoint, Incoming, Received},
	frame::FrameStruct,
	packet::{Header, InitialHeader, PacketNumber},
	transport_parameters::TransportParameters,
};
mod util;
use util::*;

mod acked;
mod characterize;
mod probe;
mod reset_stream_at;
mod sim;
mod token;
mod waits;

#[cfg(all(target_family = "wasm", target_os = "unknown"))]
use wasm_bindgen_test::wasm_bindgen_test as test;

// Enable this if you want to run these tests in the browser.
// Unfortunately it's either-or: Enable this and you can run in the browser, disable to run in nodejs.
// #[cfg(all(target_family = "wasm", target_os = "unknown"))]
// wasm_bindgen_test::wasm_bindgen_test_configure!(run_in_browser);

#[test]
fn pending_incoming_survives_server_config_change() {
	let _guard = subscribe();
	let replacement = server::Config::new(Arc::new(server_crypto_with_alpn(vec!["replacement-only".into()])));
	for replacement in [None, Some(replacement)] {
		let mut pair = Pair::default();
		let client_ch = pair.begin_connect(client_config());
		pair.drive_client();
		let (received, ecn, data) = pair.server.inbound.pop_front().unwrap();
		let mut response = Vec::new();
		let Some(DatagramEvent::NewConnection(incoming)) = pair.server.handle(
			received,
			Received {
				remote: pair.client.addr,
				local_ip: None,
				ecn,
				data: data.clone(),
			},
			&mut response,
		) else {
			panic!("expected an incoming connection");
		};

		pair.server.set_server_config(replacement);
		// Retransmitted Initials must still be buffered after the configuration changes.
		assert!(
			pair.server
				.handle(
					received,
					Received {
						remote: pair.client.addr,
						local_ip: None,
						ecn,
						data
					},
					&mut response
				)
				.is_none()
		);
		let server_ch = pair.server.try_accept(incoming, pair.time).unwrap();
		pair.drive();
		for (endpoint, ch) in [(&mut pair.client, client_ch), (&mut pair.server, server_ch)] {
			let conn = endpoint.connections.get_mut(&ch).unwrap();
			assert!(iter::from_fn(|| conn.poll()).any(|event| matches!(event, Event::Connected)));
		}
	}
}

#[test]
fn pending_incoming_can_retry_after_disabling_server() {
	let _guard = subscribe();
	let config = server_config();
	let mut pair = Pair::new(Default::default(), config.clone());
	pair.server.handle_incoming = Box::new(|_| IncomingConnectionBehavior::Wait);
	let client_ch = pair.begin_connect(client_config());
	pair.drive_client();
	pair.server.drive_incoming(pair.time, pair.client.addr);
	let incoming = pair.server.waiting_incoming.pop().unwrap();

	pair.server.set_server_config(None);
	pair.server.retry(incoming);
	pair.server.set_server_config(Some(config));
	pair.server.handle_incoming = Box::new(|incoming| {
		assert!(incoming.remote_address_validated());
		IncomingConnectionBehavior::Accept
	});
	pair.drive();
	pair.server.assert_accept();
	assert!(iter::from_fn(|| pair.client_conn_mut(client_ch).poll()).any(|event| matches!(event, Event::Connected)));
}

#[test]
fn version_negotiate_server() {
	let _guard = subscribe();
	let client_addr = "[::2]:7890".parse().unwrap();
	let mut server = Endpoint::new(
		endpoint::Config {
			server: Some(server_config()),
			..endpoint::Config::default()
		},
		true,
	)
	.unwrap();
	let now = Instant::now();
	let mut buf = Vec::with_capacity(server.max_udp_payload_size() as usize);
	// Long-header packet with reserved version number
	let header = hex!("80 0a1a2a3a 04 00000000 04 00000000 00");

	// RFC 9000 §5.2.2: packets too small to initiate a connection are dropped
	let event = server.handle(
		now,
		Received {
			remote: client_addr,
			local_ip: None,
			ecn: None,
			data: header[..].into(),
		},
		&mut buf,
	);
	assert!(event.is_none());
	assert!(buf.is_empty());

	let mut packet = header.to_vec();
	packet.resize(MIN_INITIAL_SIZE as usize, 0);
	let event = server.handle(
		now,
		Received {
			remote: client_addr,
			local_ip: None,
			ecn: None,
			data: packet[..].into(),
		},
		&mut buf,
	);
	let Some(DatagramEvent::Response(Transmit { .. })) = event else {
		panic!("expected a response");
	};

	assert_ne!(buf[0] & 0x80, 0);
	assert_eq!(&buf[1..15], hex!("00000000 04 00000000 04 00000000"));
	assert!(
		buf[15..]
			.chunks(4)
			.any(|x| { Version::from_wire(u32::from_be_bytes(x.try_into().unwrap())).is_some() })
	);
}

#[test]
fn version_negotiate_client() {
	let _guard = subscribe();
	let server_addr = "[::2]:7890".parse().unwrap();
	// Configure client to use empty CIDs so we can easily hardcode a server version negotiation
	// packet
	let cid_generator_factory: fn() -> Box<dyn ConnectionIdGenerator> =
		|| Box::new(RandomConnectionIdGenerator::new(0));
	let mut client = Endpoint::new(
		endpoint::Config {
			cid_generator: Some(Arc::new(cid_generator_factory)),
			..Default::default()
		},
		true,
	)
	.unwrap();
	let (_, mut client_ch) = client
		.connect(
			Instant::now(),
			Dial {
				config: &client_config(),
				remote: server_addr,
				server_name: "localhost",
			},
		)
		.unwrap();
	let now = Instant::now();
	let mut buf = Vec::with_capacity(client.max_udp_payload_size() as usize);
	let opt_event = client.handle(
		now,
		Received {
			remote: server_addr,
			local_ip: None,
			ecn: None,
			// Version negotiation packet for reserved version, with empty DCID
			data: hex!(
				"80 00000000 00 04 00000000
                 0a1a2a3a"
			)[..]
				.into(),
		},
		&mut buf,
	);
	if let Some(DatagramEvent::ConnectionEvent(_, event)) = opt_event {
		client_ch.handle_event(event);
	}
	assert_matches!(
		client_ch.poll(),
		Some(Event::ConnectionLost {
			reason: ConnectionError::VersionMismatch,
		})
	);
}

#[test]
fn lifecycle() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	let (client_ch, server_ch) = pair.connect();
	assert_matches!(pair.client_conn_mut(client_ch).poll(), None);
	assert!(pair.client_conn_mut(client_ch).using_ecn());
	assert!(pair.server_conn_mut(server_ch).using_ecn());

	const REASON: &str = "whee";
	info!("closing");
	pair.client
		.connections
		.get_mut(&client_ch)
		.unwrap()
		.close(pair.time, VarInt(42), REASON.into());
	pair.drive();
	assert_matches!(pair.server_conn_mut(server_ch).poll(),
                    Some(Event::ConnectionLost { reason: ConnectionError::ApplicationClosed(
                        ApplicationClose { error_code: VarInt(42), reason }
                    )}) if reason == REASON);
	assert_matches!(pair.client_conn_mut(client_ch).poll(),
                    Some(Event::ConnectionLost { reason: ConnectionError::LocallyClosed {
                        code: VarInt(42), reason }
                    }) if reason == REASON);
	assert_matches!(pair.client_conn_mut(client_ch).poll(), None);
	assert_eq!(pair.client.known_connections(), 0);
	assert_eq!(pair.client.known_cids(), 0);
	assert_eq!(pair.server.known_connections(), 0);
	assert_eq!(pair.server.known_cids(), 0);
}

/// Which client datagram a `close_before_the_server_has_the_finished` run loses.
#[derive(Clone, Copy, PartialEq, Eq)]
enum LostFlight {
	None,
	/// The one carrying the Finished, sent before the close.
	Finished,
	/// The first close, which the client resends for the server's retransmitted handshake.
	Close,
}

/// A client that closes as soon as it is `Connected`, before its Finished has left, still lets
/// the server complete the handshake and read the application close.
#[test]
fn close_before_the_client_finished_leaves() {
	close_before_the_server_has_the_finished(LostFlight::None);
}

#[test]
fn close_after_the_client_finished_is_lost() {
	close_before_the_server_has_the_finished(LostFlight::Finished);
}

#[test]
fn close_before_the_client_finished_leaves_survives_a_lost_flight() {
	close_before_the_server_has_the_finished(LostFlight::Close);
}

/// Hands the client the server's flight without letting it transmit, as a pacer would.
fn connect_before_the_finished_leaves(pair: &mut Pair, config: client::Config) -> ConnectionHandle {
	let client_ch = pair.begin_connect(config);
	pair.drive_client();
	pair.drive_server();

	let mut buf = Vec::new();
	while let Some((recv_time, ecn, packet)) = pair.client.inbound.pop_front() {
		let server = pair.server.addr;
		let Some(DatagramEvent::ConnectionEvent(_, event)) = pair.client.endpoint.handle(
			recv_time,
			Received {
				remote: server,
				local_ip: None,
				ecn,
				data: packet,
			},
			&mut buf,
		) else {
			panic!("expected a connection event");
		};
		pair.client_conn_mut(client_ch).handle_event(event);
	}

	let client = pair.client_conn_mut(client_ch);
	assert_matches!(client.poll(), Some(Event::HandshakeDataReady));
	assert_matches!(client.poll(), Some(Event::Connected));
	client_ch
}

/// A client certificate flight that fits the close packet but crowds out the coalesced 1-RTT close
/// falls back to the plain close, instead of resending the flight on every poll of a one-datagram
/// budget and never reaching the close.
#[test]
fn close_before_a_client_certificate_leaves_ends() {
	let _guard = subscribe();
	let provider = Arc::new(default_provider());
	let mut roots = RootCertStore::empty();
	roots.add(CERTIFIED_KEY.cert.der().clone()).unwrap();

	let (mut carried, mut fell_back) = (0, 0);
	// Grow the client certificate by names, sweeping its flight past the room in the close packet.
	for len in (0..1400).step_by(16) {
		let mut names = vec!["localhost".to_string()];
		names.extend((0..len / 50).map(|i| format!("{i:03}{}.test", "a".repeat(42))));
		names.push(format!("{}.test", "b".repeat(len % 50 + 1)));
		let identity = rcgen::generate_simple_self_signed(names).unwrap();

		let mut store = RootCertStore::empty();
		store.add(identity.cert.der().clone()).unwrap();
		let server = rustls::ServerConfig::builder_with_provider(provider.clone())
			.with_protocol_versions(&[&rustls::version::TLS13])
			.unwrap()
			.with_client_cert_verifier(
				WebPkiClientVerifier::builder_with_provider(Arc::new(store), provider.clone())
					.build()
					.unwrap(),
			)
			.with_single_cert(
				vec![CERTIFIED_KEY.cert.der().clone()],
				PrivatePkcs8KeyDer::from(CERTIFIED_KEY.signing_key.serialize_der()).into(),
			)
			.unwrap();
		let client = rustls::ClientConfig::builder_with_provider(provider.clone())
			.with_protocol_versions(&[&rustls::version::TLS13])
			.unwrap()
			.with_root_certificates(roots.clone())
			.with_client_auth_cert(
				vec![identity.cert.der().clone()],
				PrivatePkcs8KeyDer::from(identity.signing_key.serialize_der()).into(),
			)
			.unwrap();
		let mut client = client::Config::new(Arc::new(crypto::rustls::QuicClientConfig::try_from(client).unwrap()));
		client.transport = cubic_transport();

		let mut pair = Pair::new(
			Default::default(),
			server::Config::new(Arc::new(QuicServerConfig::try_from(server).unwrap())),
		);
		let client_ch = connect_before_the_finished_leaves(&mut pair, client);

		let now = pair.time;
		let client = pair.client_conn_mut(client_ch);
		client.close(now, VarInt(42), String::new());
		let mut transmits = Vec::new();
		let mut buf = Vec::new();
		while transmits.len() <= 2
			&& let Some(transmit) = client.poll_transmit(now, 1, &mut buf)
		{
			let datagram = Bytes::copy_from_slice(&buf[..transmit.size]);
			transmits.push((transmit, datagram));
			buf.clear();
		}
		assert_eq!(transmits.len(), 1, "{len} bytes of names");
		pair.client.outbound.extend(transmits);
		pair.drive();

		let server_ch = pair.server.assert_accept();
		let server = pair.server_conn_mut(server_ch);
		let mut connected = false;
		while let Some(event) = server.poll() {
			match event {
				Event::Connected => connected = true,
				Event::ConnectionLost { reason } if connected => {
					assert_matches!(
						reason,
						ConnectionError::ApplicationClosed(ApplicationClose {
							error_code: VarInt(42),
							..
						})
					);
				}
				_ => {}
			}
		}
		if connected {
			carried += 1;
		} else {
			fell_back += 1;
		}
	}
	// The sweep covers a flight carried with the close and one too big for it.
	assert!(carried > 0 && fell_back > 0, "{carried} carried, {fell_back} fell back");
}

fn close_before_the_server_has_the_finished(lost: LostFlight) {
	let _guard = subscribe();
	let mut pair = Pair::default();
	let client_ch = connect_before_the_finished_leaves(&mut pair, client_config());

	const REASON: &str = "whee";
	let now = pair.time;
	let server = pair.server.addr;
	if lost == LostFlight::Finished {
		pair.client.drive(now, server);
		assert!(!pair.client.outbound.is_empty());
		pair.client.outbound.clear();
	}
	pair.client_conn_mut(client_ch).close(now, VarInt(42), REASON.into());
	if lost == LostFlight::Close {
		pair.client.drive(now, server);
		assert!(!pair.client.outbound.is_empty());
		pair.client.outbound.clear();
	}
	pair.drive();

	let server_ch = pair.server.assert_accept();
	let server = pair.server_conn_mut(server_ch);
	assert_matches!(server.poll(), Some(Event::HandshakeDataReady));
	assert_matches!(server.poll(), Some(Event::HandshakeConfirmed));
	assert_matches!(server.poll(), Some(Event::Connected));
	assert_matches!(server.poll(),
                    Some(Event::ConnectionLost { reason: ConnectionError::ApplicationClosed(
                        ApplicationClose { error_code: VarInt(42), reason }
                    )}) if reason == REASON);
}

#[test]
fn stats_include_congestion_controller_bandwidth_estimate() {
	const WINDOW: u64 = 12_000;
	const BANDWIDTH_ESTIMATE: u64 = 4_000_000;

	#[derive(Clone)]
	struct TestController;

	impl Controller for TestController {
		fn on_sent(&mut self, _sent: congestion::Sent) -> congestion::Packet {
			Default::default()
		}

		fn on_ack(&mut self, _ack: congestion::Ack<'_>) {}

		fn on_loss(&mut self, _loss: congestion::Loss<'_>) {}

		fn on_ce(&mut self, _ce: congestion::Ce) {}

		fn on_mtu_update(&mut self, _new_mtu: u16) {}

		fn window(&self) -> u64 {
			WINDOW
		}

		fn pace(&mut self, _now: Instant, _bytes: u64) -> Option<Instant> {
			None
		}

		fn metrics(&self) -> ControllerMetrics {
			ControllerMetrics {
				congestion_window: WINDOW,
				bandwidth_estimate: Some(BANDWIDTH_ESTIMATE),
				..Default::default()
			}
		}

		fn clone_box(&self) -> Box<dyn Controller> {
			Box::new(self.clone())
		}

		fn into_any(self: Box<Self>) -> Box<dyn Any> {
			self
		}
	}

	struct TestControllerFactory;

	impl ControllerFactory for TestControllerFactory {
		fn build(self: Arc<Self>, _init: congestion::Init) -> Box<dyn Controller> {
			Box::new(TestController)
		}
	}

	let transport = transport::Config {
		congestion: Arc::new(TestControllerFactory),
		..transport::Config::default()
	};
	let mut config = client_config();
	config.transport = transport;

	let mut pair = Pair::default();
	let (client_ch, _) = pair.connect_with(config);
	let stats = pair.client_conn_mut(client_ch).stats();

	assert_eq!(stats.path.cwnd, WINDOW);
	assert_eq!(stats.path.bandwidth_estimate, Some(BANDWIDTH_ESTIMATE));
}

#[test]
fn server_stateless_reset() {
	let _guard = subscribe();
	let mut secret = [0; 32];
	rand::rng().fill_bytes(&mut secret);
	let endpoint_config = endpoint::Config {
		reset_secret: Some(secret),
		cid_generator: Some(Arc::new(move || Box::new(HashedConnectionIdGenerator::from_key(0)))),
		..endpoint::Config::default()
	};

	let mut pair = Pair::new(endpoint_config.clone(), server_config());
	let (client_ch, _) = pair.connect();
	pair.drive(); // Flush any post-handshake frames
	pair.server.endpoint = Endpoint::new(
		endpoint::Config {
			server: Some(server_config()),
			..endpoint_config
		},
		true,
	)
	.unwrap();
	// Force the server to generate the smallest possible stateless reset
	pair.client.connections.get_mut(&client_ch).unwrap().ping();
	info!("resetting");
	pair.drive();
	assert_matches!(
		pair.client_conn_mut(client_ch).poll(),
		Some(Event::ConnectionLost {
			reason: ConnectionError::Reset
		})
	);
}

/// A reset answering a locally closed connection drains it without reporting a loss beyond the
/// local close's own
#[test]
fn client_stateless_reset() {
	let _guard = subscribe();
	let mut secret = [0; 32];
	rand::rng().fill_bytes(&mut secret);
	let endpoint_config = endpoint::Config {
		reset_secret: Some(secret),
		cid_generator: Some(Arc::new(move || Box::new(HashedConnectionIdGenerator::from_key(0)))),
		..endpoint::Config::default()
	};

	let mut pair = Pair::new(endpoint_config.clone(), server_config());
	let (_, server_ch) = pair.connect();
	pair.client.endpoint = Endpoint::new(
		endpoint::Config {
			server: Some(server_config()),
			..endpoint_config
		},
		true,
	)
	.unwrap();
	// Send something big enough to allow room for a smaller stateless reset.
	pair.server
		.connections
		.get_mut(&server_ch)
		.unwrap()
		.close(pair.time, VarInt(42), "x".repeat(128));
	info!("resetting");
	// Step without advancing time, so the reset rather than the close timer drains the server
	pair.drive_server();
	pair.drive_client();
	pair.drive_server();
	let conn = pair.server_conn_mut(server_ch);
	assert!(conn.is_drained());
	assert_matches!(
		conn.poll(),
		Some(Event::ConnectionLost {
			reason: ConnectionError::LocallyClosed { code: VarInt(42), .. }
		})
	);
	assert_matches!(conn.poll(), None);
}

/// Restart the server from a fresh config built from `restart`, and report whether the client's
/// next packet for the old connection drew a stateless reset
fn restarted_server_resets(restart: &[u8; 32]) -> bool {
	let _guard = subscribe();
	let config = |secret: &[u8; 32]| endpoint::Config {
		reset_secret: Some(*secret),
		..endpoint::Config::default()
	};

	let mut pair = Pair::new(config(&[1; 32]), server_config());
	let (client_ch, _) = pair.connect();
	pair.drive(); // Flush any post-handshake frames
	pair.server.endpoint = Endpoint::new(
		endpoint::Config {
			server: Some(server_config()),
			..config(restart)
		},
		true,
	)
	.unwrap();
	pair.client.connections.get_mut(&client_ch).unwrap().ping();
	pair.drive();
	matches!(
		pair.client_conn_mut(client_ch).poll(),
		Some(Event::ConnectionLost {
			reason: ConnectionError::Reset
		})
	)
}

#[test]
fn reset_secret_survives_a_restart() {
	assert!(restarted_server_resets(&[1; 32]));
}

#[test]
fn reset_secret_must_match() {
	assert!(!restarted_server_resets(&[2; 32]));
}

/// After a restart every peer's next packet draws a reset at once, so a reset secret answers
/// 1000 connections a second, still dropping a packet that arrives within 1ms of the last reset
#[test]
fn reset_secret_resets_connections_each_millisecond() {
	let _guard = subscribe();
	let config = endpoint::Config {
		reset_secret: Some([1; 32]),
		..endpoint::Config::default()
	};
	let (_, mut cids) = config.resolve().unwrap();
	let mut endpoint = Endpoint::new(
		endpoint::Config {
			server: Some(server_config()),
			..config
		},
		true,
	)
	.unwrap();

	let interval = Duration::from_millis(1);
	let start = Instant::now();
	let mut buf = Vec::new();
	for i in 0..2_000u16 {
		let remote = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 1024 + i);
		let mut packet = vec![0x40];
		packet.extend_from_slice(&cids.generate_cid());
		packet.resize(100, 0);

		let now = start + interval * u32::from(i);
		let event = endpoint.handle(
			now,
			Received {
				remote,
				local_ip: None,
				ecn: None,
				data: packet[..].into(),
			},
			&mut buf,
		);
		assert!(
			matches!(event, Some(DatagramEvent::Response(_))),
			"connection {i} was not reset"
		);
		let event = endpoint.handle(
			now + interval / 2,
			Received {
				remote,
				local_ip: None,
				ecn: None,
				data: packet[..].into(),
			},
			&mut buf,
		);
		assert!(event.is_none(), "connection {i} was reset twice within 1ms");
	}
}

/// The client reads CONNECTION_CLOSE after the server forgets the connection.
/// The client's reply triggers a reset before the application polls.
#[test]
fn stateless_reset_while_draining_keeps_close_reason() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	let (client_ch, server_ch) = pair.connect();

	const REASON: &str = "not accepted";
	server_closes_and_forgets(&mut pair, server_ch, REASON);

	pair.drive_client();
	assert!(!pair.server.inbound.is_empty(), "client should have answered the close");
	pair.drive_server();
	assert!(
		!pair.client.inbound.is_empty(),
		"server should have sent a stateless reset"
	);
	pair.drive_client();

	assert!(pair.client_conn_mut(client_ch).is_drained());
	assert_eq!(pair.client_conn_mut(client_ch).poll_timeout(), None);
	assert_eq!(pair.client.known_connections(), 0);
	assert_matches!(pair.client_conn_mut(client_ch).poll(),
                    Some(Event::ConnectionLost { reason: ConnectionError::ApplicationClosed(
                        ApplicationClose { error_code: VarInt(42), reason }
                    )}) if reason == REASON);
	assert_matches!(pair.client_conn_mut(client_ch).poll(), None);
}

/// The application consumes the peer's close reason before the reset is processed.
#[test]
fn stateless_reset_while_draining_is_not_a_second_loss() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	let (client_ch, server_ch) = pair.connect();

	const REASON: &str = "not accepted";
	server_closes_and_forgets(&mut pair, server_ch, REASON);

	pair.drive_client();
	assert_matches!(pair.client_conn_mut(client_ch).poll(),
                    Some(Event::ConnectionLost { reason: ConnectionError::ApplicationClosed(
                        ApplicationClose { error_code: VarInt(42), reason }
                    )}) if reason == REASON);

	pair.drive_server();
	assert!(
		!pair.client.inbound.is_empty(),
		"server should have sent a stateless reset"
	);
	pair.drive_client();

	assert!(pair.client_conn_mut(client_ch).is_drained());
	assert_eq!(pair.client_conn_mut(client_ch).poll_timeout(), None);
	assert_eq!(pair.client.known_connections(), 0);
	assert_matches!(pair.client_conn_mut(client_ch).poll(), None);
}

/// Process CONNECTION_CLOSE followed by two resets in one batch. The first reset ends draining;
/// the second reaches the Drained state.
#[test]
fn stateless_reset_after_draining_keeps_close_reason() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	let (client_ch, server_ch) = pair.connect();

	const REASON: &str = "not accepted";
	server_closes_and_forgets(&mut pair, server_ch, REASON);
	let mut unread = mem::take(&mut pair.client.inbound);
	assert_eq!(unread.len(), 1, "the close should be a single datagram");

	for _ in 0..2 {
		pair.client_conn_mut(client_ch).ping();
		pair.drive_client();
		pair.drive_server();
		unread.append(&mut pair.client.inbound);
		// The server sends at most one stateless reset per interval.
		pair.time += pair.server.min_reset_interval();
	}
	assert_eq!(unread.len(), 3, "server should have sent two stateless resets");

	// Model the client reading all three datagrams at the current time.
	for (_, ecn, packet) in unread {
		pair.client.inbound.push_back((pair.time, ecn, packet));
	}
	pair.drive_client();

	assert!(pair.client_conn_mut(client_ch).is_drained());
	assert_eq!(pair.client.known_connections(), 0);
	assert_matches!(pair.client_conn_mut(client_ch).poll(),
                    Some(Event::ConnectionLost { reason: ConnectionError::ApplicationClosed(
                        ApplicationClose { error_code: VarInt(42), reason }
                    )}) if reason == REASON);
	assert_matches!(pair.client_conn_mut(client_ch).poll(), None);
}

/// Leave the server's CONNECTION_CLOSE unread until the server discards its connection state.
fn server_closes_and_forgets(pair: &mut Pair, server_ch: ConnectionHandle, reason: &'static str) {
	pair.drive(); // Flush post-handshake traffic so the close is the only unread datagram.
	let now = pair.time;
	pair.server_conn_mut(server_ch).close(now, VarInt(42), reason.into());
	pair.drive_server();
	assert!(
		!pair.client.inbound.is_empty(),
		"server should have sent its CONNECTION_CLOSE"
	);

	while pair.server.known_connections() > 0 {
		pair.time = pair
			.server
			.next_wakeup()
			.expect("server should have its close timer armed");
		pair.drive_server();
	}

	// Use the client's read time so the reset reaches Draining. With the original timestamp,
	// the draining deadline would expire before the reset is processed.
	for (recv_time, _, _) in &mut pair.client.inbound {
		*recv_time = pair.time;
	}
}

/// Verify that stateless resets are rate-limited
#[test]
fn stateless_reset_limit() {
	let _guard = subscribe();
	let remote = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 42);
	let endpoint_config = endpoint::Config {
		cid_generator: Some(Arc::new(move || Box::new(RandomConnectionIdGenerator::new(8)))),
		..endpoint::Config::default()
	};
	let mut endpoint = Endpoint::new(
		endpoint::Config {
			server: Some(server_config()),
			..endpoint_config.clone()
		},
		true,
	)
	.unwrap();
	let time = Instant::now();
	let mut buf = Vec::new();
	let event = endpoint.handle(
		time,
		Received {
			remote,
			local_ip: None,
			ecn: None,
			data: [0u8; 1024][..].into(),
		},
		&mut buf,
	);
	assert!(matches!(event, Some(DatagramEvent::Response(_))));
	let event = endpoint.handle(
		time,
		Received {
			remote,
			local_ip: None,
			ecn: None,
			data: [0u8; 1024][..].into(),
		},
		&mut buf,
	);
	assert!(event.is_none());
	let event = endpoint.handle(
		time + endpoint.min_reset_interval() - Duration::from_nanos(1),
		Received {
			remote,
			local_ip: None,
			ecn: None,
			data: [0u8; 1024][..].into(),
		},
		&mut buf,
	);
	assert!(event.is_none());
	let event = endpoint.handle(
		time + endpoint.min_reset_interval(),
		Received {
			remote,
			local_ip: None,
			ecn: None,
			data: [0u8; 1024][..].into(),
		},
		&mut buf,
	);
	assert!(matches!(event, Some(DatagramEvent::Response(_))));
}

#[test]
fn export_keying_material() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	let (client_ch, server_ch) = pair.connect();

	const LABEL: &[u8] = b"test_label";
	const CONTEXT: &[u8] = b"test_context";

	// client keying material
	let mut client_buf = [0u8; 64];
	pair.client_conn_mut(client_ch)
		.crypto_session()
		.export_keying_material(&mut client_buf, LABEL, CONTEXT)
		.unwrap();

	// server keying material
	let mut server_buf = [0u8; 64];
	pair.server_conn_mut(server_ch)
		.crypto_session()
		.export_keying_material(&mut server_buf, LABEL, CONTEXT)
		.unwrap();

	assert_eq!(&client_buf[..], &server_buf[..]);
}

#[test]
fn finish_stream_simple() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	let (client_ch, server_ch) = pair.connect();

	let s = pair
		.client_streams(client_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();

	const MSG: &[u8] = b"hello";
	pair.client_send(client_ch, s).write(&mut &MSG[..]).unwrap();
	assert_eq!(pair.client_streams(client_ch).send_streams(), 1);
	pair.client_send(client_ch, s).finish().unwrap();
	pair.drive();

	assert_matches!(
		pair.client_conn_mut(client_ch).poll(),
		Some(Event::Stream(StreamEvent::Finished { id })) if id == s
	);
	assert_matches!(pair.client_conn_mut(client_ch).poll(), None);
	assert_eq!(pair.client_streams(client_ch).send_streams(), 0);
	assert_eq!(pair.server_conn_mut(client_ch).streams().send_streams(), 0);
	assert_matches!(
		pair.server_conn_mut(server_ch).poll(),
		Some(Event::Stream(StreamEvent::Opened { dir: Dir::Uni }))
	);
	assert_matches!(
		pair.server_conn_mut(server_ch).poll(),
		Some(Event::Stream(StreamEvent::Readable { id })) if id == s
	);
	// Receive-only streams do not get `StreamFinished` events
	assert_eq!(pair.server_conn_mut(client_ch).streams().send_streams(), 0);
	assert_matches!(pair.server_streams(server_ch).accept(Dir::Uni), Some(stream) if stream == s);
	assert_matches!(pair.server_conn_mut(server_ch).poll(), None);

	let mut recv = pair.server_recv(server_ch, s);
	let mut chunks = recv.read().unwrap();
	assert_matches!(
		chunks.next(usize::MAX),
		Ok(Some(chunk)) if chunk.bytes == MSG
	);
	assert_matches!(chunks.next(usize::MAX), Ok(None));
	let _ = chunks.finalize();
}

#[test]
fn reset_stream() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	let (client_ch, server_ch) = pair.connect();

	let s = pair
		.client_streams(client_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();

	const MSG: &[u8] = b"hello";
	pair.client_send(client_ch, s).write(&mut &MSG[..]).unwrap();
	pair.drive();

	info!("resetting stream");
	const ERROR: VarInt = VarInt(42);
	pair.client_send(client_ch, s).reset(ERROR).unwrap();
	pair.drive();

	assert_matches!(
		pair.server_conn_mut(server_ch).poll(),
		Some(Event::Stream(StreamEvent::Opened { dir: Dir::Uni }))
	);
	assert_matches!(pair.server_streams(server_ch).accept(Dir::Uni), Some(stream) if stream == s);
	let mut recv = pair.server_recv(server_ch, s);
	let mut chunks = recv.read().unwrap();
	assert_matches!(chunks.next(usize::MAX), Err(recv::Error::Reset(ERROR)));
	let _ = chunks.finalize();
	assert_matches!(pair.client_conn_mut(client_ch).poll(), None);
}

#[test]
fn stop_stream() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	let (client_ch, server_ch) = pair.connect();

	let s = pair
		.client_streams(client_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();
	const MSG: &[u8] = b"hello";
	pair.client_send(client_ch, s).write(&mut &MSG[..]).unwrap();
	pair.drive();

	info!("stopping stream");
	const ERROR: VarInt = VarInt(42);
	pair.server_recv(server_ch, s).stop(ERROR).unwrap();
	pair.drive();

	assert_matches!(
		pair.server_conn_mut(server_ch).poll(),
		Some(Event::Stream(StreamEvent::Opened { dir: Dir::Uni }))
	);
	assert_matches!(pair.server_streams(server_ch).accept(Dir::Uni), Some(stream) if stream == s);

	assert_matches!(
		pair.client_send(client_ch, s).write(&mut &b"foo"[..]),
		Err(send::Error::Stopped(ERROR))
	);
	assert_matches!(
		pair.client_send(client_ch, s).finish(),
		Err(send::Error::Stopped(ERROR))
	);
}

#[test]
fn reject_self_signed_server_cert() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	info!("connecting");

	// Create a self-signed certificate with a different distinguished name than the default one,
	// such that path building cannot confuse the default root the server is using and the one
	// the client is trusting (in which case we'd get a different error).
	let mut cert = rcgen::CertificateParams::new(["localhost".into()]).unwrap();
	let mut issuer = rcgen::DistinguishedName::new();
	issuer.push(rcgen::DnType::OrganizationName, "Crazy Quinn's House of Certificates");
	cert.distinguished_name = issuer;
	let cert = cert.self_signed(&rcgen::KeyPair::generate().unwrap()).unwrap();
	let client_ch = pair.begin_connect(client_config_with_certs(vec![cert.into()]));

	pair.drive();

	assert_matches!(pair.client_conn_mut(client_ch).poll(),
                    Some(Event::ConnectionLost { reason: ConnectionError::TransportError(error)})
                    if error.code == TransportErrorCode::crypto(AlertDescription::UnknownCA.into()));
}

#[test]
fn reject_missing_client_cert() {
	let _guard = subscribe();

	let mut store = RootCertStore::empty();
	// `WebPkiClientVerifier` requires a non-empty store, so we stick our own certificate into it
	// because it's convenient.
	store.add(CERTIFIED_KEY.cert.der().clone()).unwrap();

	let key = PrivatePkcs8KeyDer::from(CERTIFIED_KEY.signing_key.serialize_der());
	let cert = CERTIFIED_KEY.cert.der().clone();

	let provider = Arc::new(default_provider());
	let config = rustls::ServerConfig::builder_with_provider(provider.clone())
		.with_protocol_versions(&[&rustls::version::TLS13])
		.unwrap()
		.with_client_cert_verifier(
			WebPkiClientVerifier::builder_with_provider(Arc::new(store), provider)
				.build()
				.unwrap(),
		)
		.with_single_cert(vec![cert], PrivateKeyDer::from(key))
		.unwrap();
	let config = QuicServerConfig::try_from(config).unwrap();

	let mut pair = Pair::new(Default::default(), server::Config::new(Arc::new(config)));

	info!("connecting");
	let client_ch = pair.begin_connect(client_config());
	pair.drive();

	// The client completes the connection, but finds it immediately closed
	assert_matches!(pair.client_conn_mut(client_ch).poll(), Some(Event::HandshakeDataReady));
	assert_matches!(pair.client_conn_mut(client_ch).poll(), Some(Event::Connected));
	assert_matches!(pair.client_conn_mut(client_ch).poll(),
                    Some(Event::ConnectionLost { reason: ConnectionError::ConnectionClosed(close)})
                    if close.error_code == TransportErrorCode::crypto(AlertDescription::CertificateRequired.into()));

	// The server never completes the connection
	let server_ch = pair.server.assert_accept();
	assert_matches!(pair.server_conn_mut(server_ch).poll(), Some(Event::HandshakeDataReady));
	assert_matches!(pair.server_conn_mut(server_ch).poll(),
                    Some(Event::ConnectionLost { reason: ConnectionError::TransportError(error)})
                    if error.code == TransportErrorCode::crypto(AlertDescription::CertificateRequired.into()));
}

#[test]
fn congestion() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	let (client_ch, _) = pair.connect();

	const TARGET: u64 = 2048;
	assert!(pair.client_conn_mut(client_ch).congestion_window() > TARGET);
	let s = pair
		.client_streams(client_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();
	// Send data without receiving ACKs until the congestion state falls below target
	while pair.client_conn_mut(client_ch).congestion_window() > TARGET {
		let n = pair.client_send(client_ch, s).write(&mut &[42; 1024][..]).unwrap();
		assert_eq!(n, 1024);
		pair.drive_client();
	}
	// Ensure that the congestion state recovers after receiving the ACKs
	pair.drive();
	assert!(pair.client_conn_mut(client_ch).congestion_window() >= TARGET);
	pair.client_send(client_ch, s).write(&mut &[42; 1024][..]).unwrap();
}

#[test]
fn full_initial_window() {
	let _guard = subscribe();

	// Keep `current_mtu` pinned to `INITIAL_MTU`, which Cubic's initial window of 12000 bytes
	// is an exact multiple of, so that the window can be filled precisely.
	let mut transport = cubic_transport();
	transport.mtu = None;
	let mut config = client_config();
	config.transport = transport.clone();
	// A BBR3 server paces its handshake, so the client measures a nonzero RTT that its pacer
	// then spreads the window over.
	let mut server_config = server_config();
	server_config.transport = transport;

	let mut pair = Pair::new(Default::default(), server_config);
	let (client_ch, _) = pair.connect_with(config);
	assert_eq!(pair.client_conn_mut(client_ch).bytes_in_flight(), 0);
	let window = pair.client_conn_mut(client_ch).congestion_window();
	let mtu = u64::from(INITIAL_MTU);
	assert_eq!(window % mtu, 0, "window must be exactly fillable");

	let s = pair
		.client_streams(client_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();
	let data = vec![42; 2 * window as usize];
	assert_eq!(
		pair.client_send(client_ch, s).write(&mut &data[..]),
		Ok(data.len()),
		"the test must be limited by congestion control, not by flow control"
	);

	let span = tracing::info_span!("client");
	let _guard = span.enter();
	pair.client.drive(pair.time, pair.server.addr);
	assert_eq!(pair.client_conn_mut(client_ch).bytes_in_flight(), window);
	assert_eq!(pair.client.outbound.len() as u64, window / mtu);
}

#[test]
fn high_latency_handshake() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	pair.latency = Duration::from_micros(200 * 1000);
	let (client_ch, server_ch) = pair.connect();
	assert_eq!(pair.client_conn_mut(client_ch).bytes_in_flight(), 0);
	assert_eq!(pair.server_conn_mut(server_ch).bytes_in_flight(), 0);
	assert!(pair.client_conn_mut(client_ch).using_ecn());
	assert!(pair.server_conn_mut(server_ch).using_ecn());
}

/// A server that defers accepting an Initial has the handshake idle timeout to do so, not the
/// shorter idle timeout.
#[test]
fn deferred_accept_uses_handshake_idle_timeout() {
	let _guard = subscribe();
	let transport = transport::Config {
		idle_timeout: Some(Duration::from_secs(2)),
		..transport::Config::default()
	};
	let mut server = server_config();
	server.transport = transport;
	let mut pair = Pair::new(Default::default(), server);
	pair.server.handle_incoming = Box::new(|_| IncomingConnectionBehavior::Wait);

	pair.begin_connect(client_config());
	pair.begin_connect(client_config());
	pair.drive_client();
	pair.drive_server();
	let start = pair.time;
	let late = pair.server.waiting_incoming.pop().unwrap();
	let delayed = pair.server.waiting_incoming.pop().unwrap();

	// Past the idle timeout, but within the handshake idle timeout.
	pair.time = start + Duration::from_secs(3);
	assert!(pair.server.try_accept(delayed, pair.time).is_ok());

	pair.time = start + Duration::from_secs(10);
	assert_matches!(pair.server.try_accept(late, pair.time), Err(ConnectionError::TimedOut));
}

/// Verify that datagrams arriving while a connection is in the `Accepting` state (between
/// `start_accept` and `finish_accept`) are buffered in `incoming_buffers` and replayed into the
/// connection after `finish_accept`. Drives through the full handshake and clean shutdown to
/// confirm no endpoint state is leaked.
#[test]
fn accepting_state_buffers_retransmitted_initials() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	pair.server.handle_incoming = Box::new(|_| IncomingConnectionBehavior::Wait);

	let client_ch = pair.begin_connect(client_config());
	pair.drive_client();
	pair.drive_server();

	let incoming = pair.server.pop_waiting_incoming();

	let accepting = pair.server.start_split_accept(incoming, pair.time);
	assert_eq!(pair.server.incoming_buffer_bytes(), 0);
	assert_eq!(pair.server.open_connections(), 0);

	// With no server response, the client's next wakeup is its loss timer. Advancing to it and
	// driving the client emits a retransmitted Initial for the same connection attempt.
	pair.time = pair.client.next_wakeup().unwrap();
	pair.drive_client();
	assert!(!pair.server.inbound.is_empty());
	let buffered_datagrams = pair.server.inbound.len() as u64;
	pair.drive_server();

	assert!(pair.server.waiting_incoming.is_empty());
	assert!(pair.server.incoming_buffer_bytes() > 0);

	let server_ch = pair.server.finish_split_accept(accepting);
	assert_eq!(pair.server.incoming_buffer_bytes(), 0);
	assert_eq!(pair.server.open_connections(), 1);
	// Check delivery before driving either endpoint again: completing the handshake alone
	// would also succeed if finish_accept silently discarded the retransmissions.
	assert_eq!(
		pair.server_conn_mut(server_ch).stats().udp_rx.datagrams,
		buffered_datagrams
	);

	pair.drive();
	pair.finish_connect(client_ch, server_ch);

	pair.client
		.connections
		.get_mut(&client_ch)
		.unwrap()
		.close(pair.time, VarInt(42), String::new());
	pair.drive();
	assert_eq!(pair.client.known_connections(), 0);
	assert_eq!(pair.client.known_cids(), 0);
	assert_eq!(pair.server.known_connections(), 0);
	assert_eq!(pair.server.known_cids(), 0);
}

/// Verify that attempts in the `Accepting` state count toward `max_incoming`, so a second
/// connection attempt is refused while the first attempt is still between `start_accept`
/// and `finish_accept`.
#[test]
fn max_incoming_counts_accepts_in_progress() {
	let _guard = subscribe();
	let mut server_config = server_config();
	server_config.max_incoming = 1;
	let mut pair = Pair::new(endpoint::Config::default(), server_config);
	pair.server.handle_incoming = Box::new(|_| IncomingConnectionBehavior::Wait);

	let _client_ch = pair.begin_connect(client_config());
	pair.drive_client();
	pair.drive_server();

	let incoming = pair.server.pop_waiting_incoming();

	let accepting = pair.server.start_split_accept(incoming, pair.time);
	assert_eq!(pair.server.open_connections(), 0);

	let _refused_ch = pair.begin_connect(client_config());
	pair.drive_client();
	pair.drive_server();
	assert!(pair.server.waiting_incoming.is_empty());
	assert_eq!(pair.server.open_connections(), 0);

	pair.server.finish_split_accept(accepting);
	assert_eq!(pair.server.open_connections(), 1);
}

/// Verify that when the handshake fails (here via ALPN mismatch) after `start_accept` has
/// reserved endpoint state, `finish_accept` releases the reserved CIDs and buffer, leaving no
/// endpoint state behind.
#[test]
fn accepting_state_cleaned_up_on_handshake_failure() {
	let _guard = subscribe();
	let mut server_config = server::Config::new(Arc::new(server_crypto_with_alpn(vec!["foo".into()])));
	server_config.max_incoming = 1;
	let mut pair = Pair::new(endpoint::Config::default(), server_config);
	pair.server.handle_incoming = Box::new(|_| IncomingConnectionBehavior::Wait);

	let _client_ch = pair.begin_connect(client::Config::new(Arc::new(client_crypto_with_alpn(vec![
		"bar".into(),
	]))));
	pair.drive_client();
	pair.drive_server();

	let incoming = pair.server.pop_waiting_incoming();
	let accepting = pair.server.start_split_accept(incoming, pair.time);

	pair.time = pair.client.next_wakeup().unwrap();
	pair.drive_client();
	pair.drive_server();
	assert!(pair.server.incoming_buffer_bytes() > 0);

	// The TLS handshake runs in Accepting::accept and fails on the ALPN mismatch.
	let cause = pair.server.finish_split_accept_error(accepting);
	assert_matches!(
		cause,
		ConnectionError::TransportError(e) if e.code == TransportErrorCode::crypto(0x78)
	);

	// The failed accept must leave no reserved endpoint state behind.
	assert_eq!(pair.server.open_connections(), 0);
	assert_eq!(pair.server.incoming_buffer_bytes(), 0);
	assert_eq!(pair.server.known_connections(), 0);
	assert_eq!(pair.server.known_cids(), 0);

	// Releasing the byte accounting is not sufficient: the incoming buffer slot must also
	// be freed so the next attempt can be admitted under max_incoming(1).
	pair.begin_connect(client_config());
	pair.drive_client();
	pair.drive_server();
	let incoming = pair.server.pop_waiting_incoming();
	pair.server.ignore(incoming);
}

#[test]
fn alpn_success() {
	let _guard = subscribe();
	let server_config = server::Config::new(Arc::new(server_crypto_with_alpn(vec![
		"foo".into(),
		"bar".into(),
		"baz".into(),
	])));

	let mut pair = Pair::new(endpoint::Config::default(), server_config);
	let client_config = client::Config::new(Arc::new(client_crypto_with_alpn(vec![
		"bar".into(),
		"quux".into(),
		"corge".into(),
	])));

	// Establish normal connection
	let client_ch = pair.begin_connect(client_config);
	pair.drive();
	let server_ch = pair.server.assert_accept();
	assert_matches!(pair.server_conn_mut(server_ch).poll(), Some(Event::HandshakeDataReady));
	assert_matches!(pair.server_conn_mut(server_ch).poll(), Some(Event::HandshakeConfirmed));
	assert_matches!(pair.server_conn_mut(server_ch).poll(), Some(Event::Connected));

	let hd = pair
		.client_conn_mut(client_ch)
		.crypto_session()
		.handshake_data()
		.unwrap()
		.downcast::<crypto::rustls::HandshakeData>()
		.unwrap();
	assert_eq!(hd.protocol.unwrap(), &b"bar"[..]);
	assert_eq!(
		hd.protocol_version.unwrap().downcast_ref::<rustls::ProtocolVersion>(),
		Some(&rustls::ProtocolVersion::TLSv1_3)
	);
	assert!(hd.cipher_suite.unwrap().downcast_ref::<rustls::CipherSuite>().is_some());
}

#[test]
fn server_alpn_unset() {
	let _guard = subscribe();
	let mut pair = Pair::new(endpoint::Config::default(), server_config());
	let client_config = client::Config::new(Arc::new(client_crypto_with_alpn(vec!["foo".into()])));

	let client_ch = pair.begin_connect(client_config);
	pair.drive();
	assert_matches!(
		pair.client_conn_mut(client_ch).poll(),
		Some(Event::ConnectionLost { reason: ConnectionError::ConnectionClosed(err) }) if err.error_code == TransportErrorCode::crypto(0x78)
	);
}

#[test]
fn client_alpn_unset() {
	let _guard = subscribe();
	let server_config = server::Config::new(Arc::new(server_crypto_with_alpn(vec![
		"foo".into(),
		"bar".into(),
		"baz".into(),
	])));

	let mut pair = Pair::new(endpoint::Config::default(), server_config);
	let client_ch = pair.begin_connect(client_config());
	pair.drive();
	assert_matches!(
		pair.client_conn_mut(client_ch).poll(),
		Some(Event::ConnectionLost { reason: ConnectionError::ConnectionClosed(err) }) if err.error_code == TransportErrorCode::crypto(0x78)
	);
}

#[test]
fn alpn_mismatch() {
	let _guard = subscribe();
	let server_config = server::Config::new(Arc::new(server_crypto_with_alpn(vec![
		"foo".into(),
		"bar".into(),
		"baz".into(),
	])));

	let mut pair = Pair::new(endpoint::Config::default(), server_config);
	let client_ch = pair.begin_connect(client::Config::new(Arc::new(client_crypto_with_alpn(vec![
		"quux".into(),
		"corge".into(),
	]))));

	pair.drive();
	assert_matches!(
		pair.client_conn_mut(client_ch).poll(),
		Some(Event::ConnectionLost { reason: ConnectionError::ConnectionClosed(err) }) if err.error_code == TransportErrorCode::crypto(0x78)
	);
}

/// A server accepted before the rest of a multi-packet ClientHello arrives learns the client's
/// stream limits later, and must wake a core caller whose earlier open found none.
#[test]
fn late_peer_params_make_streams_available() {
	let _guard = subscribe();
	// Long ALPN names push the ClientHello into a second Initial, as a post-quantum key share does.
	let alpn: Vec<Vec<u8>> = (0..4).map(|i| vec![b'a' + i; 255]).collect();
	let server_config = server::Config::new(Arc::new(server_crypto_with_alpn(alpn.clone())));
	let mut pair = Pair::new(Default::default(), server_config);
	pair.server.handle_incoming = Box::new(|_| IncomingConnectionBehavior::Wait);
	pair.begin_connect(client::Config::new(Arc::new(client_crypto_with_alpn(alpn))));
	pair.drive_client();
	pair.server.drive_incoming(pair.time, pair.client.addr);
	let incoming = pair.server.pop_waiting_incoming();
	let server_ch = pair.server.try_accept(incoming, pair.time).unwrap();
	assert_eq!(pair.server_streams(server_ch).open(Dir::Uni, Default::default()), None);

	pair.drive();
	assert!(
		iter::from_fn(|| pair.server_conn_mut(server_ch).poll())
			.any(|event| matches!(event, Event::Stream(StreamEvent::Available { dir: Dir::Uni })))
	);
	assert!(
		pair.server_streams(server_ch)
			.open(Dir::Uni, Default::default())
			.is_some()
	);
}

#[test]
fn stream_id_limit() {
	let _guard = subscribe();
	let server = server::Config {
		transport: transport::Config {
			limits: Limits {
				max_streams: NonZeroU64::new(1),
				..Limits::default()
			},
			..transport::Config::default()
		},
		..server_config()
	};
	let mut pair = Pair::new(Default::default(), server);
	let (client_ch, server_ch) = pair.connect();

	let s = pair
		.client
		.connections
		.get_mut(&client_ch)
		.unwrap()
		.streams()
		.open(Dir::Uni, Default::default())
		.expect("couldn't open first stream");
	assert_eq!(
		pair.client_streams(client_ch).open(Dir::Uni, Default::default()),
		None,
		"only one stream is permitted at a time"
	);
	// Generate some activity to allow the server to see the stream
	const MSG: &[u8] = b"hello";
	pair.client_send(client_ch, s).write(&mut &MSG[..]).unwrap();
	pair.client_send(client_ch, s).finish().unwrap();
	pair.drive();
	assert_matches!(
		pair.client_conn_mut(client_ch).poll(),
		Some(Event::Stream(StreamEvent::Finished { id })) if id == s
	);
	assert_eq!(
		pair.client_streams(client_ch).open(Dir::Uni, Default::default()),
		None,
		"server does not immediately grant additional credit"
	);
	assert_matches!(
		pair.server_conn_mut(server_ch).poll(),
		Some(Event::Stream(StreamEvent::Opened { dir: Dir::Uni }))
	);
	assert_matches!(
		pair.server_conn_mut(server_ch).poll(),
		Some(Event::Stream(StreamEvent::Readable { id })) if id == s
	);
	assert_matches!(pair.server_streams(server_ch).accept(Dir::Uni), Some(stream) if stream == s);

	let mut recv = pair.server_recv(server_ch, s);
	let mut chunks = recv.read().unwrap();
	assert_matches!(
		chunks.next(usize::MAX),
		Ok(Some(chunk)) if chunk.bytes == MSG
	);
	assert_eq!(chunks.next(usize::MAX), Ok(None));
	let _ = chunks.finalize();

	// Lose the stream-credit grant itself, after proving the sender exhausted its budget.
	let grants = |pair: &mut Pair| pair.server_conn_mut(server_ch).stats().frame_tx.max_streams_uni;
	let before = grants(&mut pair);
	pair.to_client.loss = 1.0;
	drive_until(&mut pair, |pair| grants(pair) > before);
	let lost = grants(&mut pair);
	assert_eq!(
		pair.client_streams(client_ch).open(Dir::Uni, Default::default()),
		None,
		"the dropped grant did not reach the sender"
	);
	pair.to_client.loss = 0.0;
	pair.drive();
	assert!(grants(&mut pair) > lost, "stream credit was retransmitted");
	assert_matches!(
		pair.client_conn_mut(client_ch).poll(),
		Some(Event::Stream(StreamEvent::Available { dir: Dir::Uni }))
	);
	assert_matches!(pair.client_conn_mut(client_ch).poll(), None);

	// Try opening the second stream again, now that we've made room
	let s = pair
		.client
		.connections
		.get_mut(&client_ch)
		.unwrap()
		.streams()
		.open(Dir::Uni, Default::default())
		.expect("didn't get stream id budget");
	pair.client_send(client_ch, s).finish().unwrap();
	pair.drive();
	// Make sure the server actually processes data on the newly-available stream
	assert_matches!(
		pair.server_conn_mut(server_ch).poll(),
		Some(Event::Stream(StreamEvent::Opened { dir: Dir::Uni }))
	);
	assert_matches!(
		pair.server_conn_mut(server_ch).poll(),
		Some(Event::Stream(StreamEvent::Readable { id })) if id == s
	);
	assert_matches!(pair.server_streams(server_ch).accept(Dir::Uni), Some(stream) if stream == s);
	assert_matches!(pair.server_conn_mut(server_ch).poll(), None);

	let mut recv = pair.server_recv(server_ch, s);
	let mut chunks = recv.read().unwrap();
	assert_matches!(chunks.next(usize::MAX), Ok(None));
	let _ = chunks.finalize();
}

#[test]
fn streams_blocked() {
	let _guard = subscribe();
	let server = server::Config {
		transport: transport::Config {
			limits: Limits {
				max_streams: NonZeroU64::new(1),
				..Limits::default()
			},
			..transport::Config::default()
		},
		..server_config()
	};
	let mut pair = Pair::new(Default::default(), server);
	let (client_ch, server_ch) = pair.connect();

	// Use up the only stream slot, then try to open another
	let s = pair
		.client_streams(client_ch)
		.open(Dir::Uni, Default::default())
		.expect("first uni stream");
	assert_eq!(pair.client_streams(client_ch).open(Dir::Uni, Default::default()), None);

	// Send data so the STREAMS_BLOCKED piggybacks on an outgoing packet
	pair.client_send(client_ch, s).write(&mut &b"hi"[..]).unwrap();
	pair.drive();

	assert_eq!(pair.client_conn_mut(client_ch).stats().frame_tx.streams_blocked_uni, 1);
	assert_eq!(pair.server_conn_mut(server_ch).stats().frame_rx.streams_blocked_uni, 1);
}

#[test]
fn streams_blocked_not_sent_under_limit() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	let (client_ch, _server_ch) = pair.connect();

	// Default config allows many streams; opening one should not trigger STREAMS_BLOCKED
	let s = pair
		.client_streams(client_ch)
		.open(Dir::Uni, Default::default())
		.expect("open stream");
	pair.client_send(client_ch, s).write(&mut &b"hi"[..]).unwrap();
	pair.drive();

	assert_eq!(pair.client_conn_mut(client_ch).stats().frame_tx.streams_blocked_uni, 0);
}

#[test]
fn data_blocked() {
	let _guard = subscribe();
	let server = server::Config {
		transport: transport::Config {
			limits: Limits {
				receive_window: NonZeroU64::new(10),
				..Limits::default()
			},
			..transport::Config::default()
		},
		..server_config()
	};
	let mut pair = Pair::new(Default::default(), server);
	let (client_ch, server_ch) = pair.connect();

	// Fill the connection-level window, then run into the limit
	let s = pair
		.client_streams(client_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();
	assert_eq!(
		pair.client_send(client_ch, s).write(&mut &b"0123456789ab"[..]).unwrap(),
		10
	);
	assert_matches!(
		pair.client_send(client_ch, s).write(&mut &b"c"[..]),
		Err(send::Error::Blocked)
	);
	pair.drive();
	assert_eq!(pair.client_conn_mut(client_ch).stats().frame_tx.data_blocked, 1);
	assert_eq!(pair.server_conn_mut(server_ch).stats().frame_rx.data_blocked, 1);

	// Being refused again at the same limit does not repeat the frame
	assert_matches!(
		pair.client_send(client_ch, s).write(&mut &b"c"[..]),
		Err(send::Error::Blocked)
	);
	pair.drive();
	assert_eq!(pair.client_conn_mut(client_ch).stats().frame_tx.data_blocked, 1);

	// Running into the raised limit is reported again
	assert_matches!(pair.server_streams(server_ch).accept(Dir::Uni), Some(stream) if stream == s);
	let mut recv = pair.server_recv(server_ch, s);
	let mut chunks = recv.read().unwrap();
	assert_matches!(chunks.next(usize::MAX), Ok(Some(chunk)) if chunk.bytes.len() == 10);
	let _ = chunks.finalize();
	pair.drive();
	assert_eq!(
		pair.client_send(client_ch, s).write(&mut &b"0123456789ab"[..]).unwrap(),
		10
	);
	assert_matches!(
		pair.client_send(client_ch, s).write(&mut &b"c"[..]),
		Err(send::Error::Blocked)
	);
	pair.drive();
	let stats = pair.client_conn_mut(client_ch).stats();
	assert_eq!(stats.frame_tx.data_blocked, 2);
	assert_eq!(stats.frame_tx.stream_data_blocked, 0);
}

#[test]
fn stream_data_blocked() {
	let _guard = subscribe();
	let server = server::Config {
		transport: transport::Config {
			limits: Limits {
				stream_receive_window: NonZeroU64::new(10),
				..Limits::default()
			},
			..transport::Config::default()
		},
		..server_config()
	};
	let mut pair = Pair::new(Default::default(), server);
	let (client_ch, server_ch) = pair.connect();

	// Fill the stream-level window, then run into the limit
	let s = pair
		.client_streams(client_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();
	assert_eq!(
		pair.client_send(client_ch, s).write(&mut &b"0123456789ab"[..]).unwrap(),
		10
	);
	assert_matches!(
		pair.client_send(client_ch, s).write(&mut &b"c"[..]),
		Err(send::Error::Blocked)
	);
	pair.drive();
	assert_eq!(pair.client_conn_mut(client_ch).stats().frame_tx.stream_data_blocked, 1);
	assert_eq!(pair.server_conn_mut(server_ch).stats().frame_rx.stream_data_blocked, 1);

	// Being refused again at the same limit does not repeat the frame
	assert_matches!(
		pair.client_send(client_ch, s).write(&mut &b"c"[..]),
		Err(send::Error::Blocked)
	);
	pair.drive();
	assert_eq!(pair.client_conn_mut(client_ch).stats().frame_tx.stream_data_blocked, 1);

	// Running into the raised limit is reported again
	assert_matches!(pair.server_streams(server_ch).accept(Dir::Uni), Some(stream) if stream == s);
	let mut recv = pair.server_recv(server_ch, s);
	let mut chunks = recv.read().unwrap();
	assert_matches!(chunks.next(usize::MAX), Ok(Some(chunk)) if chunk.bytes.len() == 10);
	let _ = chunks.finalize();
	pair.drive();
	assert_eq!(
		pair.client_send(client_ch, s).write(&mut &b"0123456789ab"[..]).unwrap(),
		10
	);
	assert_matches!(
		pair.client_send(client_ch, s).write(&mut &b"c"[..]),
		Err(send::Error::Blocked)
	);
	pair.drive();
	let stats = pair.client_conn_mut(client_ch).stats();
	assert_eq!(stats.frame_tx.stream_data_blocked, 2);
	assert_eq!(stats.frame_tx.data_blocked, 0);
}

#[test]
fn data_blocked_not_sent_under_limit() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	let (client_ch, _server_ch) = pair.connect();

	// Default windows are far larger than this write, so nothing is blocked
	let s = pair
		.client_streams(client_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();
	pair.client_send(client_ch, s).write(&mut &b"hi"[..]).unwrap();
	pair.drive();

	let stats = pair.client_conn_mut(client_ch).stats();
	assert_eq!(stats.frame_tx.data_blocked, 0);
	assert_eq!(stats.frame_tx.stream_data_blocked, 0);
}

#[test]
fn data_blocked_not_sent_for_local_send_window() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	let (client_ch, _server_ch) = pair.connect();

	// Running out of our own send window is not the peer's flow control limit
	pair.client_conn_mut(client_ch).set_send_window(5);
	let s = pair
		.client_streams(client_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();
	assert_eq!(
		pair.client_send(client_ch, s).write(&mut &b"0123456789"[..]).unwrap(),
		5
	);
	assert_matches!(
		pair.client_send(client_ch, s).write(&mut &b"x"[..]),
		Err(send::Error::Blocked)
	);
	pair.drive();

	assert_eq!(pair.client_conn_mut(client_ch).stats().frame_tx.data_blocked, 0);
}

#[test]
fn data_blocked_dropped_when_limit_raised() {
	let _guard = subscribe();
	let server = server::Config {
		transport: transport::Config {
			limits: Limits {
				receive_window: NonZeroU64::new(10),
				..Limits::default()
			},
			..transport::Config::default()
		},
		..server_config()
	};
	let mut pair = Pair::new(Default::default(), server);
	let (client_ch, server_ch) = pair.connect();

	let s = pair
		.client_streams(client_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();
	assert_eq!(
		pair.client_send(client_ch, s).write(&mut &b"0123456789ab"[..]).unwrap(),
		10
	);
	assert_matches!(
		pair.client_send(client_ch, s).write(&mut &b"c"[..]),
		Err(send::Error::Blocked)
	);

	// The peer raises the limit before the queued frame gets transmitted
	pair.server_conn_mut(server_ch).set_receive_window(100u32.into());
	pair.drive_server();
	pair.drive();

	assert_eq!(pair.client_conn_mut(client_ch).stats().frame_tx.data_blocked, 0);
}

#[test]
fn stream_data_blocked_not_sent_after_reset() {
	let _guard = subscribe();
	let server = server::Config {
		transport: transport::Config {
			limits: Limits {
				stream_receive_window: NonZeroU64::new(10),
				..Limits::default()
			},
			..transport::Config::default()
		},
		..server_config()
	};
	let mut pair = Pair::new(Default::default(), server);
	let (client_ch, _server_ch) = pair.connect();

	let s = pair
		.client_streams(client_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();
	assert_eq!(
		pair.client_send(client_ch, s).write(&mut &b"0123456789ab"[..]).unwrap(),
		10
	);
	assert_matches!(
		pair.client_send(client_ch, s).write(&mut &b"c"[..]),
		Err(send::Error::Blocked)
	);

	// Resetting the stream before the queued frame gets transmitted discards it
	pair.client_send(client_ch, s).reset(0u32.into()).unwrap();
	pair.drive();

	let stats = pair.client_conn_mut(client_ch).stats();
	assert_eq!(stats.frame_tx.stream_data_blocked, 0);
	assert_eq!(stats.frame_tx.reset_stream, 1);
}

#[test]
fn data_blocked_retransmit() {
	let _guard = subscribe();
	let server = server::Config {
		transport: transport::Config {
			limits: Limits {
				receive_window: NonZeroU64::new(10),
				..Limits::default()
			},
			..transport::Config::default()
		},
		..server_config()
	};
	let mut pair = Pair::new(Default::default(), server);
	let (client_ch, server_ch) = pair.connect();

	let s = pair
		.client_streams(client_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();
	assert_eq!(
		pair.client_send(client_ch, s).write(&mut &b"0123456789ab"[..]).unwrap(),
		10
	);
	assert_matches!(
		pair.client_send(client_ch, s).write(&mut &b"c"[..]),
		Err(send::Error::Blocked)
	);
	pair.drive_client();
	pair.server.inbound.clear(); // Lose the packet carrying the frame
	pair.drive();

	// Still blocked at the same limit, so the frame is sent again. A PTO may send several probes,
	// each carrying the frame, so only check that it was repeated and got through.
	assert!(pair.client_conn_mut(client_ch).stats().frame_tx.data_blocked > 1);
	assert!(pair.server_conn_mut(server_ch).stats().frame_rx.data_blocked > 0);
}

#[test]
fn key_update_simple() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	let (client_ch, server_ch) = pair.connect();
	let s = pair
		.client
		.connections
		.get_mut(&client_ch)
		.unwrap()
		.streams()
		.open(Dir::Bi, Default::default())
		.expect("couldn't open first stream");

	const MSG1: &[u8] = b"hello1";
	pair.client_send(client_ch, s).write(&mut &MSG1[..]).unwrap();
	pair.drive();

	assert_matches!(
		pair.server_conn_mut(server_ch).poll(),
		Some(Event::Stream(StreamEvent::Opened { dir: Dir::Bi }))
	);
	assert_matches!(
		pair.server_conn_mut(server_ch).poll(),
		Some(Event::Stream(StreamEvent::Readable { id })) if id == s
	);
	assert_matches!(pair.server_streams(server_ch).accept(Dir::Bi), Some(stream) if stream == s);
	assert_matches!(pair.server_conn_mut(server_ch).poll(), None);
	let mut recv = pair.server_recv(server_ch, s);
	let mut chunks = recv.read().unwrap();
	assert_matches!(
		chunks.next(usize::MAX),
		Ok(Some(chunk)) if chunk.bytes == MSG1
	);
	let _ = chunks.finalize();

	info!("initiating key update");
	pair.client_conn_mut(client_ch).force_key_update();

	const MSG2: &[u8] = b"hello2";
	pair.client_send(client_ch, s).write(&mut &MSG2[..]).unwrap();
	pair.drive();

	assert_matches!(pair.server_conn_mut(server_ch).poll(), Some(Event::Stream(StreamEvent::Readable { id })) if id == s);
	assert_matches!(pair.server_conn_mut(server_ch).poll(), None);
	let mut recv = pair.server_recv(server_ch, s);
	let mut chunks = recv.read().unwrap();
	assert_matches!(
		chunks.next(usize::MAX),
		Ok(Some(chunk)) if chunk.bytes == MSG2
	);
	let _ = chunks.finalize();

	assert_eq!(pair.client_conn_mut(client_ch).stats().path.lost_packets, 0);
	assert_eq!(pair.server_conn_mut(server_ch).stats().path.lost_packets, 0);
}

#[test]
fn key_update_reordered() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	let (client_ch, server_ch) = pair.connect();
	let s = pair
		.client
		.connections
		.get_mut(&client_ch)
		.unwrap()
		.streams()
		.open(Dir::Bi, Default::default())
		.expect("couldn't open first stream");

	const MSG1: &[u8] = b"1";
	pair.client_send(client_ch, s).write(&mut &MSG1[..]).unwrap();
	pair.client.drive(pair.time, pair.server.addr);
	assert!(!pair.client.outbound.is_empty());
	pair.client.delay_outbound();

	pair.client_conn_mut(client_ch).force_key_update();
	info!("updated keys");

	const MSG2: &[u8] = b"two";
	pair.client_send(client_ch, s).write(&mut &MSG2[..]).unwrap();
	pair.client.drive(pair.time, pair.server.addr);
	pair.client.finish_delay();
	pair.drive();

	assert_eq!(pair.client_conn_mut(client_ch).stats().path.lost_packets, 0);
	assert_matches!(
		pair.server_conn_mut(server_ch).poll(),
		Some(Event::Stream(StreamEvent::Opened { dir: Dir::Bi }))
	);
	assert_matches!(pair.server_streams(server_ch).accept(Dir::Bi), Some(stream) if stream == s);

	let mut recv = pair.server_recv(server_ch, s);
	let mut chunks = recv.read().unwrap();
	let buf1 = chunks.next(usize::MAX).unwrap().unwrap();
	assert_matches!(&*buf1.bytes, MSG1);
	let buf2 = chunks.next(usize::MAX).unwrap().unwrap();
	assert_eq!(buf2.bytes, MSG2);
	let _ = chunks.finalize();

	assert_eq!(pair.client_conn_mut(client_ch).stats().path.lost_packets, 0);
	assert_eq!(pair.server_conn_mut(server_ch).stats().path.lost_packets, 0);
}

#[test]
fn initial_retransmit() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	let client_ch = pair.begin_connect(client_config());
	pair.client.drive(pair.time, pair.server.addr);
	pair.client.outbound.clear(); // Drop initial
	pair.drive();
	assert_matches!(pair.client_conn_mut(client_ch).poll(), Some(Event::HandshakeDataReady));
	assert_matches!(pair.client_conn_mut(client_ch).poll(), Some(Event::Connected));
}

/// Each side tells its endpoint once that the connection stopped being active, before it drains,
/// so a shutdown waiting on every connection can stop at the first of these
#[test]
fn draining_is_reported_once_before_drained() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	let (client_ch, server_ch) = pair.connect();
	let now = pair.time;
	pair.client_conn_mut(client_ch).close(now, VarInt(0), String::new());
	pair.drive();

	let expected = [Lifecycle::Draining, Lifecycle::Drained];
	let client: Vec<_> = pair
		.client
		.lifecycle
		.iter()
		.filter(|(ch, _)| *ch == client_ch)
		.map(|(_, l)| *l)
		.collect();
	let server: Vec<_> = pair
		.server
		.lifecycle
		.iter()
		.filter(|(ch, _)| *ch == server_ch)
		.map(|(_, l)| *l)
		.collect();
	assert_eq!(client, expected);
	assert_eq!(server, expected);
}

/// A handshake the peer never answered stops being active as soon as it closes, so a shutdown
/// need not sit out the closing period for a close nobody will answer
#[test]
fn unanswered_handshake_drains_at_close() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	let client_ch = pair.begin_connect(client_config());
	pair.client.drive(pair.time, pair.server.addr);
	pair.client.outbound.clear(); // The server never hears the Initial

	let now = pair.time;
	pair.client_conn_mut(client_ch).close(now, VarInt(0), String::new());
	pair.client.drive(now, pair.server.addr);

	assert!(!pair.client.outbound.is_empty(), "the close is still sent");
	// Drained waits for the close timer, which still answers a late reply
	assert_eq!(pair.client.lifecycle, [(client_ch, Lifecycle::Draining)]);
}

/// Once the peer has answered, a close stays active until the peer's close comes back
#[test]
fn answered_handshake_waits_for_peer_close() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	let client_ch = pair.begin_connect(client_config());
	pair.drive_client();
	pair.drive_server();
	pair.client.drive(pair.time, pair.server.addr);

	let now = pair.time;
	pair.client_conn_mut(client_ch).close(now, VarInt(0), String::new());
	pair.client.drive(now, pair.server.addr);
	assert!(pair.client.lifecycle.is_empty());

	pair.drive();
	assert_eq!(pair.client.lifecycle[0], (client_ch, Lifecycle::Draining));
}

#[test]
fn instant_close_1() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	info!("connecting");
	let client_ch = pair.begin_connect(client_config());
	pair.client
		.connections
		.get_mut(&client_ch)
		.unwrap()
		.close(pair.time, VarInt(0), String::new());
	pair.drive();
	let server_ch = pair.server.assert_accept();
	assert_matches!(
		pair.client_conn_mut(client_ch).poll(),
		Some(Event::ConnectionLost {
			reason: ConnectionError::LocallyClosed { code: VarInt(0), .. }
		})
	);
	assert_matches!(
		pair.server_conn_mut(server_ch).poll(),
		Some(Event::ConnectionLost {
			reason: ConnectionError::ConnectionClosed(ConnectionClose {
				error_code: TransportErrorCode::APPLICATION_ERROR,
				..
			}),
		})
	);
}

#[test]
fn instant_close_2() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	info!("connecting");
	let client_ch = pair.begin_connect(client_config());
	// Unlike `instant_close`, the server sees a valid Initial packet first.
	pair.drive_client();
	pair.client
		.connections
		.get_mut(&client_ch)
		.unwrap()
		.close(pair.time, VarInt(42), String::new());
	pair.drive();
	assert_matches!(
		pair.client_conn_mut(client_ch).poll(),
		Some(Event::ConnectionLost {
			reason: ConnectionError::LocallyClosed { code: VarInt(42), .. }
		})
	);
	let server_ch = pair.server.assert_accept();
	assert_matches!(pair.server_conn_mut(server_ch).poll(), Some(Event::HandshakeDataReady));
	assert_matches!(
		pair.server_conn_mut(server_ch).poll(),
		Some(Event::ConnectionLost {
			reason: ConnectionError::ConnectionClosed(ConnectionClose {
				error_code: TransportErrorCode::APPLICATION_ERROR,
				..
			}),
		})
	);
}

#[test]
fn instant_server_close() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	info!("connecting");
	pair.begin_connect(client_config());
	pair.drive_client();
	pair.server.drive_incoming(pair.time, pair.client.addr);
	let server_ch = pair.server.assert_accept();
	info!("closing");
	pair.server
		.connections
		.get_mut(&server_ch)
		.unwrap()
		.close(pair.time, VarInt(42), String::new());
	pair.drive();
	assert_matches!(
		pair.client_conn_mut(server_ch).poll(),
		Some(Event::ConnectionLost {
			reason: ConnectionError::ConnectionClosed(ConnectionClose {
				error_code: TransportErrorCode::APPLICATION_ERROR,
				..
			}),
		})
	);
}

#[test]
fn idle_timeout() {
	let _guard = subscribe();
	const IDLE_TIMEOUT: u64 = 100;
	let server = server::Config {
		transport: transport::Config {
			idle_timeout: Some(Duration::from_millis(IDLE_TIMEOUT)),
			..transport::Config::default()
		},
		..server_config()
	};
	let mut pair = Pair::new(Default::default(), server);
	let (client_ch, server_ch) = pair.connect();
	pair.client_conn_mut(client_ch).ping();
	let start = pair.time;

	while !pair.client_conn_mut(client_ch).is_closed() || !pair.server_conn_mut(server_ch).is_closed() {
		if !pair.step()
			&& let Some(t) = min_opt(pair.client.next_wakeup(), pair.server.next_wakeup())
		{
			pair.time = t;
		}
		pair.client.inbound.clear(); // Simulate total S->C packet loss
	}

	assert!(pair.time - start < Duration::from_millis(2 * IDLE_TIMEOUT));
	assert_matches!(
		pair.client_conn_mut(client_ch).poll(),
		Some(Event::ConnectionLost {
			reason: ConnectionError::TimedOut,
		})
	);
	assert_matches!(
		pair.server_conn_mut(server_ch).poll(),
		Some(Event::ConnectionLost {
			reason: ConnectionError::TimedOut,
		})
	);
}

#[test]
fn unreachable_handshake_fails_with_no_viable_path() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	let client_ch = pair.begin_connect(client_config());

	pair.client_conn_mut(client_ch).handle_unreachable();

	let conn = pair.client_conn_mut(client_ch);
	assert!(conn.is_drained());
	assert_matches!(
		conn.poll(),
		Some(Event::ConnectionLost {
			reason: ConnectionError::TransportError(TransportError {
				code: TransportErrorCode::NO_VIABLE_PATH,
				..
			}),
		})
	);
	// Not even a CONNECTION_CLOSE goes out.
	pair.drive();
	pair.server.assert_no_accept();
}

#[test]
fn unreachable_is_loss_once_established() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	let (client_ch, server_ch) = pair.connect();

	pair.client_conn_mut(client_ch).handle_unreachable();

	assert!(!pair.client_conn_mut(client_ch).is_closed());
	assert_matches!(pair.client_conn_mut(client_ch).poll(), None);
	let s = pair
		.client_streams(client_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();
	pair.client_send(client_ch, s).write(&mut &b"hello"[..]).unwrap();
	pair.drive();
	assert_matches!(
		pair.server_conn_mut(server_ch).poll(),
		Some(Event::Stream(StreamEvent::Opened { dir: Dir::Uni }))
	);
}

#[test]
fn connection_close_sends_acks() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	let (client_ch, _server_ch) = pair.connect();

	let client_acks = pair.client_conn_mut(client_ch).stats().frame_rx.acks;

	pair.client_conn_mut(client_ch).ping();
	pair.drive_client();

	let time = pair.time;
	pair.server_conn_mut(client_ch).close(time, VarInt(42), String::new());

	pair.drive();

	let client_acks_2 = pair.client_conn_mut(client_ch).stats().frame_rx.acks;
	assert!(client_acks_2 > client_acks, "Connection close should send pending ACKs");
}

/// A connection closed while its congestion window is saturated must still deliver
/// CONNECTION_CLOSE to the peer promptly, rather than leaving the peer to discover the close via
/// its idle timeout (see https://github.com/quinn-rs/quinn/issues/2785)
#[test]
fn connection_close_while_congestion_blocked() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	let (client_ch, server_ch) = pair.connect();

	// Saturate the congestion window with unacknowledged stream data by transmitting from the
	// client without driving the server, so no ACKs come back and in-flight bytes stay pinned at
	// the window
	let s = pair
		.client_streams(client_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();
	pair.client_send(client_ch, s)
		.write(&mut &[42; 1024 * 1024][..])
		.unwrap();
	pair.drive_client();

	// Close while the window is full and stream data is still pending
	const REASON: &str = "whee";
	let close_time = pair.time;
	pair.client
		.connections
		.get_mut(&client_ch)
		.unwrap()
		.close(pair.time, VarInt(42), REASON.into());

	// Step the simulation by hand so we can catch the exact moment the server hears about the
	// close: check for the event after each packet exchange, before the clock jumps ahead
	let mut result = None;
	loop {
		pair.drive_client();
		pair.drive_server();
		while let Some(event) = pair.server_conn_mut(server_ch).poll() {
			if let Event::ConnectionLost { reason } = event {
				result = Some((reason, pair.time));
			}
		}
		if result.is_some() || !pair.step() {
			break;
		}
	}
	let (reason, delivered_at) = result.expect("server never learned of the close");
	assert_matches!(reason, ConnectionError::ApplicationClosed(
        ApplicationClose { error_code: VarInt(42), reason }
    ) if reason == REASON);
	// Close packets aren't congestion controlled and the test link has no latency, so the close
	// should arrive the moment it was issued — any delay means a timer had to rescue it
	assert_eq!(delivered_at, close_time);
}

#[test]
fn server_hs_retransmit() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	let client_ch = pair.begin_connect(client_config());
	pair.step();
	assert!(!pair.client.inbound.is_empty()); // Initial + Handshakes
	pair.client.inbound.clear();
	pair.drive();
	assert_matches!(pair.client_conn_mut(client_ch).poll(), Some(Event::HandshakeDataReady));
	assert_matches!(pair.client_conn_mut(client_ch).poll(), Some(Event::Connected));
}

#[test]
fn migration() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	let (client_ch, server_ch) = pair.connect();
	pair.drive();

	let client_stats_after_connect = pair.client_conn_mut(client_ch).stats();

	pair.client.addr = SocketAddr::new(Ipv4Addr::new(127, 0, 0, 1).into(), pair.next_port());
	pair.client_conn_mut(client_ch).ping();

	// Assert that just receiving the ping message is accounted into the servers
	// anti-amplification budget
	pair.drive_client();
	pair.drive_server();
	assert_ne!(pair.server_conn_mut(server_ch).total_recvd(), 0);

	pair.drive();
	assert_matches!(pair.client_conn_mut(client_ch).poll(), None);
	assert_eq!(pair.server_conn_mut(server_ch).remote_address(), pair.client.addr);

	// Assert that the client's response to the PATH_CHALLENGE was an IMMEDIATE_ACK, instead of a
	// second ping
	let client_stats_after_migrate = pair.client_conn_mut(client_ch).stats();
	assert_eq!(
		client_stats_after_migrate.frame_tx.ping - client_stats_after_connect.frame_tx.ping,
		1
	);
	assert_eq!(
		client_stats_after_migrate.frame_tx.immediate_ack - client_stats_after_connect.frame_tx.immediate_ack,
		1
	);
}

fn test_flow_control(config: transport::Config, window_size: usize) {
	let _guard = subscribe();
	let stream_limited = config.stream_receive_window().into_inner() == window_size as u64;
	let mut pair = Pair::new(
		Default::default(),
		server::Config {
			transport: config,
			..server_config()
		},
	);
	let (client_ch, server_ch) = pair.connect();
	let msg = vec![0xAB; window_size + 10];

	// Stream reset before read
	let s = pair
		.client_streams(client_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();
	info!("writing");
	assert_eq!(pair.client_send(client_ch, s).write(&mut &msg[..]), Ok(window_size));
	assert_eq!(
		pair.client_send(client_ch, s).write(&mut &msg[window_size..]),
		Err(send::Error::Blocked)
	);
	pair.drive();
	info!("resetting");
	pair.client_send(client_ch, s).reset(VarInt(42)).unwrap();
	pair.drive();

	let mut recv = pair.server_recv(server_ch, s);
	let mut chunks = recv.read().unwrap();
	assert_eq!(chunks.next(usize::MAX).err(), Some(recv::Error::Reset(VarInt(42))));
	let _ = chunks.finalize();

	// Happy path
	info!("writing");
	let s = pair
		.client_streams(client_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();
	assert_eq!(pair.client_send(client_ch, s).write(&mut &msg[..]), Ok(window_size));
	assert_eq!(
		pair.client_send(client_ch, s).write(&mut &msg[window_size..]),
		Err(send::Error::Blocked)
	);

	pair.drive();
	let mut cursor = 0;
	let mut recv = pair.server_recv(server_ch, s);
	let mut chunks = recv.read().unwrap();
	loop {
		match chunks.next(usize::MAX) {
			Ok(Some(chunk)) => {
				cursor += chunk.bytes.len();
			}
			Ok(None) => {
				panic!("end of stream");
			}
			Err(recv::Error::Blocked) => {
				break;
			}
			Err(e) => {
				panic!("{}", e);
			}
		}
	}
	let _ = chunks.finalize();

	info!("finished reading");
	assert_eq!(cursor, window_size);
	// Lose the grant that releases the exhausted window. Count the frame, so a data loss or
	// an unrelated ACK cannot satisfy the activation proof.
	let grants = |pair: &mut Pair| {
		let stats = pair.server_conn_mut(server_ch).stats().frame_tx;
		if stream_limited {
			stats.max_stream_data
		} else {
			stats.max_data
		}
	};
	let before = grants(&mut pair);
	pair.to_client.loss = 1.0;
	drive_until(&mut pair, |pair| grants(pair) > before);
	let lost = grants(&mut pair);
	assert_eq!(
		pair.client_send(client_ch, s).write(&mut &msg[..]),
		Err(send::Error::Blocked),
		"the dropped grant did not reach the sender"
	);
	pair.to_client.loss = 0.0;
	pair.drive();
	assert!(grants(&mut pair) > lost, "flow-control credit was retransmitted");
	info!("writing");
	assert_eq!(pair.client_send(client_ch, s).write(&mut &msg[..]), Ok(window_size));
	assert_eq!(
		pair.client_send(client_ch, s).write(&mut &msg[window_size..]),
		Err(send::Error::Blocked)
	);

	pair.drive();
	let mut cursor = 0;
	let mut recv = pair.server_recv(server_ch, s);
	let mut chunks = recv.read().unwrap();
	loop {
		match chunks.next(usize::MAX) {
			Ok(Some(chunk)) => {
				cursor += chunk.bytes.len();
			}
			Ok(None) => {
				panic!("end of stream");
			}
			Err(recv::Error::Blocked) => {
				break;
			}
			Err(e) => {
				panic!("{}", e);
			}
		}
	}
	assert_eq!(cursor, window_size);
	let _ = chunks.finalize();
	info!("finished reading");
}

#[test]
fn stream_flow_control() {
	test_flow_control(
		transport::Config {
			limits: Limits {
				stream_receive_window: NonZeroU64::new(2000),
				..Limits::default()
			},
			..transport::Config::default()
		},
		2000,
	);
}

#[test]
fn conn_flow_control() {
	test_flow_control(
		transport::Config {
			limits: Limits {
				receive_window: NonZeroU64::new(2000),
				..Limits::default()
			},
			..transport::Config::default()
		},
		2000,
	);
}

#[test]
fn stop_opens_bidi() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	let (client_ch, server_ch) = pair.connect();
	assert_eq!(pair.client_streams(client_ch).send_streams(), 0);
	let s = pair
		.client_streams(client_ch)
		.open(Dir::Bi, Default::default())
		.unwrap();
	assert_eq!(pair.client_streams(client_ch).send_streams(), 1);
	const ERROR: VarInt = VarInt(42);
	pair.client
		.connections
		.get_mut(&server_ch)
		.unwrap()
		.recv_stream(s)
		.stop(ERROR)
		.unwrap();
	pair.drive();

	assert_matches!(
		pair.server_conn_mut(server_ch).poll(),
		Some(Event::Stream(StreamEvent::Opened { dir: Dir::Bi }))
	);
	assert_eq!(pair.server_conn_mut(client_ch).streams().send_streams(), 0);
	assert_matches!(pair.server_streams(server_ch).accept(Dir::Bi), Some(stream) if stream == s);
	assert_eq!(pair.server_conn_mut(client_ch).streams().send_streams(), 1);

	let mut recv = pair.server_recv(server_ch, s);
	let mut chunks = recv.read().unwrap();
	assert_matches!(chunks.next(usize::MAX), Err(recv::Error::Blocked));
	let _ = chunks.finalize();

	assert_matches!(
		pair.server_send(server_ch, s).write(&mut &b"foo"[..]),
		Err(send::Error::Stopped(ERROR))
	);
	assert_matches!(
		pair.server_conn_mut(server_ch).poll(),
		Some(Event::Stream(StreamEvent::Stopped {
			id: _,
			error_code: ERROR
		}))
	);
	assert_matches!(pair.server_conn_mut(server_ch).poll(), None);
}

#[test]
fn implicit_open() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	let (client_ch, server_ch) = pair.connect();
	let s1 = pair
		.client_streams(client_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();
	let s2 = pair
		.client_streams(client_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();
	pair.client_send(client_ch, s2).write(&mut &b"hello"[..]).unwrap();
	pair.drive();
	assert_matches!(
		pair.server_conn_mut(server_ch).poll(),
		Some(Event::Stream(StreamEvent::Opened { dir: Dir::Uni }))
	);
	assert_eq!(pair.server_streams(server_ch).accept(Dir::Uni), Some(s1));
	assert_eq!(pair.server_streams(server_ch).accept(Dir::Uni), Some(s2));
	assert_eq!(pair.server_streams(server_ch).accept(Dir::Uni), None);
}

#[test]
fn zero_length_cid() {
	let _guard = subscribe();
	let cid_generator_factory: fn() -> Box<dyn ConnectionIdGenerator> =
		|| Box::new(RandomConnectionIdGenerator::new(0));
	let mut pair = Pair::new(
		endpoint::Config {
			cid_generator: Some(Arc::new(cid_generator_factory)),
			..endpoint::Config::default()
		},
		server_config(),
	);
	let (client_ch, server_ch) = pair.connect();
	// Ensure we can reconnect after a previous connection is cleaned up
	info!("closing");
	pair.client
		.connections
		.get_mut(&client_ch)
		.unwrap()
		.close(pair.time, VarInt(42), String::new());
	pair.drive();
	pair.server
		.connections
		.get_mut(&server_ch)
		.unwrap()
		.close(pair.time, VarInt(42), String::new());
	pair.connect();
}

#[test]
fn keep_alive() {
	let _guard = subscribe();
	const IDLE_TIMEOUT: u64 = 10;
	let server = server::Config {
		transport: transport::Config {
			keep_alive: Some(Duration::from_millis(IDLE_TIMEOUT / 2)),
			idle_timeout: Some(Duration::from_millis(IDLE_TIMEOUT)),
			..transport::Config::default()
		},
		..server_config()
	};
	let mut pair = Pair::new(Default::default(), server);
	let (client_ch, server_ch) = pair.connect();
	// Run a good while longer than the idle timeout
	let end = pair.time + Duration::from_millis(20 * IDLE_TIMEOUT);
	while pair.time < end {
		if !pair.step()
			&& let Some(time) = min_opt(pair.client.next_wakeup(), pair.server.next_wakeup())
		{
			pair.time = time;
		}
		assert!(!pair.client_conn_mut(client_ch).is_closed());
		assert!(!pair.server_conn_mut(server_ch).is_closed());
	}
}

/// Runs `pair` idle well past `idle_timeout`, asserting neither side closes, and returns the
/// gaps between the client's keep-alive packets.
fn idle_keep_alive_gaps(
	pair: &mut Pair,
	client_ch: ConnectionHandle,
	server_ch: ConnectionHandle,
	idle_timeout: Duration,
) -> Vec<Duration> {
	let end = pair.time + 20 * idle_timeout;
	let mut sent = pair.client_conn_mut(client_ch).stats().path.sent_packets;
	let mut last = pair.time;
	let mut gaps = Vec::new();
	while pair.time < end {
		if !pair.step()
			&& let Some(time) = min_opt(pair.client.next_wakeup(), pair.server.next_wakeup())
		{
			pair.time = time;
		}
		assert!(!pair.client_conn_mut(client_ch).is_closed());
		assert!(!pair.server_conn_mut(server_ch).is_closed());
		let now = pair.client_conn_mut(client_ch).stats().path.sent_packets;
		if now != sent {
			sent = now;
			gaps.push(pair.time - last);
			last = pair.time;
		}
	}
	gaps
}

/// A client that sets no keep-alive and no per-connection config still pings a server that
/// advertises a shorter idle timeout, below the negotiated timeout rather than its own.
#[test]
fn keep_alive_derives_from_a_shorter_peer_idle_timeout() {
	let _guard = subscribe();
	const PEER: Duration = Duration::from_secs(2);
	let mut server = server_config();
	server.transport.idle_timeout = Some(PEER);
	server.transport.keep_alive = Some(Duration::ZERO);
	let mut client = client_config();
	client.transport.idle_timeout = Some(Duration::from_secs(10));
	let mut pair = Pair::new(Default::default(), server);
	let (client_ch, server_ch) = pair.connect_with(client);
	let gaps = idle_keep_alive_gaps(&mut pair, client_ch, server_ch, PEER);
	assert!(gaps.len() >= 10, "{gaps:?}");
	assert!(gaps.iter().all(|&gap| gap * 3 < PEER), "{gaps:?}");
}

/// The derived interval follows the local idle timeout whenever it is the smaller one.
#[test]
fn keep_alive_follows_the_local_idle_timeout() {
	let _guard = subscribe();
	let mut last = None;
	for local in [Duration::from_secs(10), Duration::from_secs(5)] {
		let mut server = server_config();
		server.transport.idle_timeout = Some(Duration::from_secs(30));
		server.transport.keep_alive = Some(Duration::ZERO);
		let mut client = client_config();
		client.transport.idle_timeout = Some(local);
		let mut pair = Pair::new(Default::default(), server);
		let (client_ch, server_ch) = pair.connect_with(client);
		let gaps = idle_keep_alive_gaps(&mut pair, client_ch, server_ch, local);
		let longest = gaps.iter().copied().max().unwrap();
		assert!(longest * 3 < local, "{local:?}: {gaps:?}");
		assert!(last.is_none_or(|last| longest < last), "{local:?}: {gaps:?}");
		last = Some(longest);
	}
}

/// An explicit keep-alive no shorter than the negotiated idle timeout is capped at the derived
/// interval, and zero sends none.
#[test]
fn keep_alive_explicit_cap_and_disable() {
	let _guard = subscribe();
	const IDLE: Duration = Duration::from_secs(2);
	let connect = |keep_alive| {
		let mut server = server_config();
		server.transport.idle_timeout = Some(IDLE);
		server.transport.keep_alive = Some(Duration::ZERO);
		let mut client = client_config();
		client.transport.keep_alive = Some(keep_alive);
		let mut pair = Pair::new(Default::default(), server);
		let (client_ch, server_ch) = pair.connect_with(client);
		(pair, client_ch, server_ch)
	};

	let (mut pair, client_ch, server_ch) = connect(Duration::from_secs(60));
	let gaps = idle_keep_alive_gaps(&mut pair, client_ch, server_ch, IDLE);
	assert!(gaps.iter().all(|&gap| gap * 3 < IDLE), "{gaps:?}");

	let (mut pair, client_ch, _) = connect(Duration::ZERO);
	let start = pair.time;
	while !pair.client_conn_mut(client_ch).is_closed() {
		if !pair.step() {
			let Some(time) = min_opt(pair.client.next_wakeup(), pair.server.next_wakeup()) else {
				break;
			};
			pair.time = time;
		}
	}
	assert_matches!(
		pair.client_conn_mut(client_ch).poll(),
		Some(Event::ConnectionLost {
			reason: ConnectionError::TimedOut
		})
	);
	assert!(pair.time - start < 2 * IDLE, "{:?}", pair.time - start);
}

/// A peer advertising the largest idle timeout the wire carries derives a keep-alive too far
/// out to schedule, which must not panic.
#[test]
fn keep_alive_derived_from_a_huge_peer_idle_timeout() {
	let _guard = subscribe();
	let mut server = server_config();
	server.transport.idle_timeout = Some(Duration::from_millis(VarInt::MAX.into_inner()));
	let mut client = client_config();
	client.transport.idle_timeout = None;
	let mut pair = Pair::new(Default::default(), server);
	let (client_ch, _) = pair.connect_with(client);
	pair.client_conn_mut(client_ch).ping();
	pair.drive();
	assert!(!pair.client_conn_mut(client_ch).is_closed());
}

#[test]
fn keep_alive_interval_derivation() {
	use transport::keep_alive_interval as interval;
	let secs = Duration::from_secs;
	assert_eq!(interval(None, Some(secs(10))), Some(secs(3)));
	assert_eq!(interval(None, None), None);
	assert_eq!(interval(Some(Duration::ZERO), Some(secs(10))), None);
	assert_eq!(interval(Some(secs(5)), Some(secs(10))), Some(secs(5)));
	assert_eq!(interval(Some(secs(10)), Some(secs(10))), Some(secs(3)));
	assert_eq!(interval(Some(secs(60)), None), Some(secs(60)));
}

#[test]
fn cid_rotation() {
	let _guard = subscribe();
	const CID_TIMEOUT: Duration = Duration::from_secs(2);

	let cid_generator_factory: fn() -> Box<dyn ConnectionIdGenerator> =
		|| Box::new(*RandomConnectionIdGenerator::new(8).set_lifetime(CID_TIMEOUT));

	// Only test cid rotation on server side to have a clear output trace
	let server = Endpoint::new(
		endpoint::Config {
			server: Some(server_config()),
			..endpoint::Config {
				cid_generator: Some(Arc::new(cid_generator_factory)),
				..endpoint::Config::default()
			}
		},
		true,
	)
	.unwrap();
	let client = Endpoint::new(endpoint::Config::default(), true).unwrap();

	let mut pair = Pair::new_from_endpoint(client, server);
	let (_, server_ch) = pair.connect();

	let mut round: u64 = 1;
	let mut stop = pair.time;
	let end = pair.time + 5 * CID_TIMEOUT;

	use crate::LOC_CID_COUNT;
	use crate::cid_queue::CidQueue;
	let mut active_cid_num = CidQueue::LEN as u64 + 1;
	active_cid_num = active_cid_num.min(LOC_CID_COUNT);
	let mut left_bound = 0;
	let mut right_bound = active_cid_num - 1;

	while pair.time < end {
		stop += CID_TIMEOUT;
		// Run a while until PushNewCID timer fires
		while pair.time < stop {
			if !pair.step()
				&& let Some(time) = min_opt(pair.client.next_wakeup(), pair.server.next_wakeup())
			{
				pair.time = time;
			}
		}
		info!(
			"Checking active cid sequence range before {:?} seconds",
			round * CID_TIMEOUT.as_secs()
		);
		let _bound = (left_bound, right_bound);
		assert_matches!(pair.server_conn_mut(server_ch).active_local_cid_seq(), _bound);
		round += 1;
		left_bound += active_cid_num;
		right_bound += active_cid_num;
		pair.drive_server();
	}
}

#[test]
fn cid_retirement() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	let (client_ch, server_ch) = pair.connect();

	// Server retires current active remote CIDs
	pair.server_conn_mut(server_ch).rotate_local_cid(1, Instant::now());
	pair.drive();
	// Any unexpected behavior may trigger TransportError::CONNECTION_ID_LIMIT_ERROR
	assert!(!pair.client_conn_mut(client_ch).is_closed());
	assert!(!pair.server_conn_mut(server_ch).is_closed());
	assert_matches!(pair.client_conn_mut(client_ch).active_rem_cid_seq(), 1);

	use crate::LOC_CID_COUNT;
	use crate::cid_queue::CidQueue;
	let mut active_cid_num = CidQueue::LEN as u64;
	active_cid_num = active_cid_num.min(LOC_CID_COUNT);

	let next_retire_prior_to = active_cid_num + 1;
	pair.client_conn_mut(client_ch).ping();
	// Server retires all valid remote CIDs
	pair.server_conn_mut(server_ch)
		.rotate_local_cid(next_retire_prior_to, Instant::now());
	pair.drive();
	assert!(!pair.client_conn_mut(client_ch).is_closed());
	assert!(!pair.server_conn_mut(server_ch).is_closed());

	assert_eq!(
		pair.client_conn_mut(client_ch).active_rem_cid_seq(),
		next_retire_prior_to,
	);
}

#[test]
fn finish_stream_flow_control_reordered() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	let (client_ch, server_ch) = pair.connect();

	let s = pair
		.client_streams(client_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();

	const MSG: &[u8] = b"hello";
	pair.client_send(client_ch, s).write(&mut &MSG[..]).unwrap();
	pair.drive_client(); // Send stream data
	pair.server.drive(pair.time, pair.client.addr); // Receive

	// Issue flow control credit
	let mut recv = pair.server_recv(server_ch, s);
	let mut chunks = recv.read().unwrap();
	assert_matches!(
		chunks.next(usize::MAX),
		Ok(Some(chunk)) if chunk.bytes == MSG
	);
	let _ = chunks.finalize();

	pair.server.drive(pair.time, pair.client.addr);
	pair.server.delay_outbound(); // Delay it

	pair.client_send(client_ch, s).finish().unwrap();
	pair.drive_client(); // Send FIN
	pair.server.drive(pair.time, pair.client.addr); // Acknowledge
	pair.server.finish_delay(); // Add flow control packets after
	pair.drive();

	assert_matches!(
		pair.client_conn_mut(client_ch).poll(),
		Some(Event::Stream(StreamEvent::Finished { id })) if id == s
	);
	assert_matches!(pair.client_conn_mut(client_ch).poll(), None);
	assert_matches!(
		pair.server_conn_mut(server_ch).poll(),
		Some(Event::Stream(StreamEvent::Opened { dir: Dir::Uni }))
	);
	assert_matches!(pair.server_streams(server_ch).accept(Dir::Uni), Some(stream) if stream == s);

	let mut recv = pair.server_recv(server_ch, s);
	let mut chunks = recv.read().unwrap();
	assert_matches!(chunks.next(usize::MAX), Ok(None));
	let _ = chunks.finalize();
}

#[test]
fn handshake_1rtt_handling() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	let client_ch = pair.begin_connect(client_config());
	pair.drive_client();
	pair.drive_server();
	let server_ch = pair.server.assert_accept();
	// Server now has 1-RTT keys, but remains in Handshake state until the TLS CFIN has
	// authenticated the client. Delay the final client handshake flight so that doesn't happen yet.
	pair.client.drive(pair.time, pair.server.addr);
	pair.client.delay_outbound();

	// Send some 1-RTT data which will be received first.
	let s = pair
		.client_streams(client_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();
	const MSG: &[u8] = b"hello";
	pair.client_send(client_ch, s).write(&mut &MSG[..]).unwrap();
	pair.client_send(client_ch, s).finish().unwrap();
	pair.client.drive(pair.time, pair.server.addr);
	let early = pair.client.outbound.len();

	// Add the handshake flight back on, and deliver only the 1-RTT data first.
	pair.client.finish_delay();
	pair.drive_client();
	let rest = pair.server.inbound.split_off(early);
	let received = pair.server_conn_mut(server_ch).stats().udp_rx.datagrams;
	pair.drive_server();
	// The 1-RTT data arrived ahead of the client's Finished, so the server has not processed it.
	let server = pair.server_conn_mut(server_ch);
	assert_eq!(server.stats().udp_rx.datagrams, received + early as u64);
	assert!(server.is_handshaking());
	assert_matches!(pair.server_streams(server_ch).accept(Dir::Uni), None);

	pair.server.inbound.extend(rest);
	pair.drive();

	// The server buffered the early 1-RTT packet and processed it once the handshake completed,
	// so nothing had to be retransmitted.
	assert_eq!(pair.client_conn_mut(client_ch).stats().path.lost_packets, 0);
	let mut recv = pair.server_recv(server_ch, s);
	let mut chunks = recv.read().unwrap();
	assert_matches!(
		chunks.next(usize::MAX),
		Ok(Some(chunk)) if chunk.bytes == MSG
	);
	let _ = chunks.finalize();
}

#[test]
fn stop_before_finish() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	let (client_ch, server_ch) = pair.connect();

	let s = pair
		.client_streams(client_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();
	const MSG: &[u8] = b"hello";
	pair.client_send(client_ch, s).write(&mut &MSG[..]).unwrap();
	pair.drive();

	info!("stopping stream");
	const ERROR: VarInt = VarInt(42);
	pair.server_recv(server_ch, s).stop(ERROR).unwrap();
	pair.drive();

	assert_matches!(
		pair.client_send(client_ch, s).finish(),
		Err(send::Error::Stopped(ERROR))
	);
}

#[test]
fn stop_during_finish() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	let (client_ch, server_ch) = pair.connect();

	let s = pair
		.client_streams(client_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();
	const MSG: &[u8] = b"hello";
	pair.client_send(client_ch, s).write(&mut &MSG[..]).unwrap();
	pair.drive();

	assert_matches!(pair.server_streams(server_ch).accept(Dir::Uni), Some(stream) if stream == s);
	info!("stopping and finishing stream");
	const ERROR: VarInt = VarInt(42);
	pair.server_recv(server_ch, s).stop(ERROR).unwrap();
	pair.drive_server();
	pair.client_send(client_ch, s).finish().unwrap();
	pair.drive_client();
	assert_matches!(
		pair.client_conn_mut(client_ch).poll(),
		Some(Event::Stream(StreamEvent::Stopped { id, error_code: ERROR })) if id == s
	);
}

// Ensure we can recover from loss of tail packets when the congestion window is full
#[test]
fn congested_tail_loss() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	let (client_ch, _) = pair.connect();

	const TARGET: u64 = 2048;
	assert!(pair.client_conn_mut(client_ch).congestion_window() > TARGET);
	let s = pair
		.client_streams(client_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();
	// Send data without receiving ACKs until the congestion state falls below target
	while pair.client_conn_mut(client_ch).congestion_window() > TARGET {
		let n = pair.client_send(client_ch, s).write(&mut &[42; 1024][..]).unwrap();
		assert_eq!(n, 1024);
		pair.drive_client();
	}
	assert!(!pair.server.inbound.is_empty());
	pair.server.inbound.clear();
	// Ensure that the congestion state recovers after retransmits occur and are ACKed
	info!("recovering");
	pair.drive();
	assert!(pair.client_conn_mut(client_ch).congestion_window() > TARGET);
	pair.client_send(client_ch, s).write(&mut &[42; 1024][..]).unwrap();
}

// Send a tail-loss probe when GSO segment_size is less than INITIAL_MTU
#[test]
fn tail_loss_small_segment_size() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	let (client_ch, server_ch) = pair.connect();
	// Keep IMMEDIATE_ACK out of the probe so the test isolates whether the available
	// DATAGRAM suppresses the fallback PING.
	pair.client_conn_mut(client_ch).disable_peer_ack_frequency();

	// No datagrams frames received in the handshake.
	let server_stats = pair.server_conn_mut(server_ch).stats();
	assert_eq!(server_stats.frame_rx.datagram, 0);

	const DGRAM_LEN: usize = 1000; // Below INITIAL_MTU after packet overhead.
	const DGRAM_NUM: u64 = 5; // Enough to build a GSO batch.

	info!("Sending an ack-eliciting datagram");
	pair.client_conn_mut(client_ch).ping();
	pair.drive_client();

	// Drop these packets on the server side.
	assert!(!pair.server.inbound.is_empty());
	pair.server.inbound.clear();

	// Doing one step makes the client advance time to the PTO fire time.
	info!("stepping forward to PTO");
	pair.step();
	let ping_count = pair.client_conn_mut(client_ch).stats().frame_tx.ping;

	// Still no datagrams frames received by the server.
	let server_stats = pair.server_conn_mut(server_ch).stats();
	assert_eq!(server_stats.frame_rx.datagram, 0);

	// Now we can send another batch of datagrams, so the PTO can send them instead of
	// sending a ping.  These are small enough that the segment_size is less than the
	// INITIAL_MTU.
	info!("Sending datagram batch");
	for _ in 0..DGRAM_NUM {
		pair.client_datagrams(client_ch)
			.send(Bytes::from(vec![0; DGRAM_LEN]), Default::default())
			.unwrap();
	}

	// If this succeeds the datagrams are received by the server and the client did not
	// crash.
	pair.drive();

	// Finally the server should have received some datagrams.
	let server_stats = pair.server_conn_mut(server_ch).stats();
	assert_eq!(server_stats.frame_rx.datagram, DGRAM_NUM);

	// DATAGRAM frames are ack-eliciting, so the loss probe does not need an additional PING.
	let client_stats = pair.client_conn_mut(client_ch).stats();
	assert_eq!(client_stats.frame_tx.ping, ping_count);
}

#[test]
fn tail_loss_probe_keeps_ping_when_datagram_does_not_fit() {
	let _guard = subscribe();

	const PATH_MTU: u16 = 1452;

	let client_config = {
		let mut config = client_config();
		config.transport.initial_mtu = PATH_MTU;
		config.transport.mtu = None;
		config
	};

	let mut pair = Pair::default();
	pair.mtu = PATH_MTU as usize;
	let (client_ch, server_ch) = pair.connect_with(client_config);

	pair.client_conn_mut(client_ch).disable_peer_ack_frequency();
	assert_eq!(pair.client_conn_mut(client_ch).path_mtu(), PATH_MTU);

	// Establish an outstanding ack-eliciting packet and discard it.
	pair.client_conn_mut(client_ch).ping();
	pair.drive_client();
	assert!(!pair.server.inbound.is_empty());
	pair.server.inbound.clear();

	// Advance to the PTO without transmitting the queued loss probe yet.
	pair.step();
	let ping_count = pair.client_conn_mut(client_ch).stats().frame_tx.ping;

	// This fits the normal path MTU, but not a loss probe capped to INITIAL_MTU.
	let datagram_len = pair.client_datagrams(client_ch).max_size().unwrap();
	assert!(datagram_len > INITIAL_MTU as usize);
	pair.client_datagrams(client_ch)
		.send(Bytes::from(vec![0; datagram_len]), Default::default())
		.unwrap();

	pair.drive();

	// The probe must retain its PING when the queued DATAGRAM cannot fit in that packet.
	let client_stats = pair.client_conn_mut(client_ch).stats();
	assert_eq!(client_stats.frame_tx.ping, ping_count + 2);

	let server_stats = pair.server_conn_mut(server_ch).stats();
	assert_eq!(server_stats.frame_rx.datagram, 1);
}

/// A loss probe ended short by a datagram that fills a whole probe, but not one behind a PING,
/// ends the GSO batch instead of repeating its tiny size for the stream data queued behind.
#[test]
fn tail_loss_probe_ended_short_ends_the_gso_batch() {
	let _guard = subscribe();

	const PATH_MTU: u16 = 1452;

	let client_config = {
		let mut config = client_config();
		config.transport.initial_mtu = PATH_MTU;
		config.transport.mtu = None;
		config
	};

	let mut pair = Pair::default();
	pair.mtu = PATH_MTU as usize;
	let (client_ch, server_ch) = pair.connect_with(client_config);
	pair.client_conn_mut(client_ch).disable_peer_ack_frequency();

	// Establish an outstanding ack-eliciting packet and discard it.
	pair.client_conn_mut(client_ch).ping();
	pair.drive_client();
	assert!(!pair.server.inbound.is_empty());
	pair.server.inbound.clear();

	// Advance to the PTO without transmitting the queued loss probe yet.
	pair.step();

	// Fills a probe capped to INITIAL_MTU exactly.
	// Overhead: flags + CID + PN + tag + frame type + frame length = 1 + 8 + 1 + 16 + 1 + 2 = 29
	const DATAGRAM_LEN: usize = INITIAL_MTU as usize - 29;
	pair.client_datagrams(client_ch)
		.send(Bytes::from(vec![0; DATAGRAM_LEN]), Default::default())
		.unwrap();
	let s = pair
		.client_streams(client_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();
	pair.client_send(client_ch, s).write(&mut &[0; 4096][..]).unwrap();
	pair.client_conn_mut(client_ch).ping();

	pair.drive();

	let server_stats = pair.server_conn_mut(server_ch).stats();
	assert_eq!(server_stats.frame_rx.datagram, 1);
	assert!(server_stats.frame_rx.stream > 0);
}

// Respect max_datagrams when TLP happens
#[test]
fn tail_loss_respect_max_datagrams() {
	let _guard = subscribe();
	let client_config = {
		let mut c_config = client_config();
		//Disabling GSO, so only a single segment should be sent per iops
		c_config.transport.segmentation_offload = false;
		c_config
	};
	let mut pair = Pair::default();
	let (client_ch, _) = pair.connect_with(client_config);

	const DGRAM_LEN: usize = 1000; // High enough so GSO batch could be built
	const DGRAM_NUM: u64 = 5; // Enough to build a GSO batch.

	info!("Sending an ack-eliciting datagram");
	pair.client_conn_mut(client_ch).ping();
	pair.drive_client();

	// Drop these packets on the server side.
	assert!(!pair.server.inbound.is_empty());
	pair.server.inbound.clear();

	// Doing one step makes the client advance time to the PTO fire time.
	info!("stepping forward to PTO");
	pair.step();

	// start sending datagram batches but the first should be a TLP
	info!("Sending datagram batch");
	for _ in 0..DGRAM_NUM {
		pair.client_datagrams(client_ch)
			.send(Bytes::from(vec![0; DGRAM_LEN]), Default::default())
			.unwrap();
	}

	pair.drive();

	// Finally checking the number of sent udp datagrams match the number of iops
	let client_stats = pair.client_conn_mut(client_ch).stats();
	assert_eq!(client_stats.udp_tx.ios, client_stats.udp_tx.datagrams);
}

#[test]
fn datagram_send_recv() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	let (client_ch, server_ch) = pair.connect();
	assert_matches!(pair.server_conn_mut(server_ch).poll(), None);
	assert_matches!(pair.client_datagrams(client_ch).max_size(), Some(x) if x > 0);

	const DATA: &[u8] = b"whee";
	pair.client_datagrams(client_ch)
		.send(Bytes::from(DATA), Default::default())
		.unwrap();
	pair.drive();
	assert_matches!(pair.server_conn_mut(server_ch).poll(), Some(Event::DatagramReceived));
	assert_eq!(pair.server_datagrams(server_ch).recv().unwrap(), DATA);
	assert_matches!(pair.server_datagrams(server_ch).recv(), None);
}

/// Writes more than a congestion window to a stream in a group at `stream`, queues datagrams in
/// a group at `datagram`, and returns how many datagrams the first flight carried.
fn first_flight_datagrams(stream: i64, datagram: i64) -> usize {
	const DATAGRAMS: usize = 8;
	let _guard = subscribe();
	let mut pair = Pair::default();
	let (client_ch, server_ch) = pair.connect();
	let conn = pair.server_conn_mut(server_ch);
	let streams = conn.send_groups().open(stream);
	let datagrams = conn.send_groups().open(datagram);
	let s = conn
		.streams()
		.open(
			Dir::Uni,
			send::Options {
				group: Some(&streams),
				priority: 0,
			},
		)
		.unwrap();
	conn.send_stream(s).write(&mut &[0; 256 * 1024][..]).unwrap();
	for _ in 0..DATAGRAMS {
		let options = send::Options {
			group: Some(&datagrams),
			priority: 0,
		};
		conn.datagrams().send(Bytes::from(vec![1; 100]), options).unwrap();
	}

	// One congestion-limited flight.
	pair.drive_server();
	pair.drive_client();
	assert!(pair.client_streams(client_ch).accept(Dir::Uni).is_some());
	let mut first = 0;
	while pair.client_datagrams(client_ch).recv().is_some() {
		first += 1;
	}

	// Nothing is dropped, only ordered.
	pair.drive();
	let mut rest = 0;
	while pair.client_datagrams(client_ch).recv().is_some() {
		rest += 1;
	}
	assert_eq!(first + rest, DATAGRAMS);
	first
}

#[test]
fn low_priority_datagrams_wait_behind_streams() {
	assert_eq!(first_flight_datagrams(1, 0), 0);
}

#[test]
fn high_priority_datagrams_preempt_streams() {
	assert_eq!(first_flight_datagrams(0, 1), 8);
}

#[test]
fn datagram_recv_buffer_overflow() {
	let _guard = subscribe();
	const PAYLOAD_WINDOW: usize = 100;
	const METADATA_WINDOW: usize = 2 * size_of::<Datagram>();
	const WINDOW: usize = PAYLOAD_WINDOW + METADATA_WINDOW;
	let server = server::Config {
		transport: transport::Config {
			// Account for exactly two datagrams of metadata space
			datagram: transport::datagram::Config {
				receive_buffer: Some(WINDOW),
				..transport::Config::default().datagram
			},
			..transport::Config::default()
		},
		..server_config()
	};
	let mut pair = Pair::new(Default::default(), server);
	let (client_ch, server_ch) = pair.connect();
	assert_matches!(pair.server_conn_mut(server_ch).poll(), None);
	assert_eq!(
		pair.client_conn_mut(client_ch).datagrams().max_size(),
		Datagram::max_payload(WINDOW as u64).map(|n| n as usize)
	);

	const DATA1: &[u8] = &[0xAB; (PAYLOAD_WINDOW / 3) + 1];
	const DATA2: &[u8] = &[0xBC; (PAYLOAD_WINDOW / 3) + 1];
	const DATA3: &[u8] = &[0xCD; (PAYLOAD_WINDOW / 3) + 1];
	pair.client_datagrams(client_ch)
		.send(Bytes::from(DATA1), Default::default())
		.unwrap();
	pair.client_datagrams(client_ch)
		.send(Bytes::from(DATA2), Default::default())
		.unwrap();
	pair.client_datagrams(client_ch)
		.send(Bytes::from(DATA3), Default::default())
		.unwrap();
	pair.drive();
	assert_matches!(pair.server_conn_mut(server_ch).poll(), Some(Event::DatagramReceived));
	assert_eq!(pair.server_datagrams(server_ch).recv().unwrap(), DATA2);
	assert_eq!(pair.server_datagrams(server_ch).recv().unwrap(), DATA3);
	assert_matches!(pair.server_datagrams(server_ch).recv(), None);

	pair.client_datagrams(client_ch)
		.send(Bytes::from(DATA1), Default::default())
		.unwrap();
	pair.drive();
	assert_eq!(pair.server_datagrams(server_ch).recv().unwrap(), DATA1);
	assert_matches!(pair.server_datagrams(server_ch).recv(), None);
}

/// A datagram of the advertised maximum size is a valid frame, so its queue metadata
/// overflowing the receive buffer drops it rather than closing the connection.
#[test]
fn datagram_of_max_size_keeps_connection() {
	let _guard = subscribe();
	const WINDOW: usize = 1000;
	let server = server::Config {
		transport: transport::Config {
			datagram: transport::datagram::Config {
				receive_buffer: Some(WINDOW),
				..transport::Config::default().datagram
			},
			..transport::Config::default()
		},
		..server_config()
	};
	let mut pair = Pair::new(Default::default(), server);
	let (client_ch, server_ch) = pair.connect();
	assert_matches!(pair.server_conn_mut(server_ch).poll(), None);
	let max_size = pair.client_conn_mut(client_ch).datagrams().max_size().unwrap();
	assert_eq!(max_size, Datagram::max_payload(WINDOW as u64).unwrap() as usize);

	pair.client_datagrams(client_ch)
		.send(Bytes::from(vec![0xAB; max_size]), Default::default())
		.unwrap();
	pair.drive();
	assert_matches!(pair.server_conn_mut(server_ch).poll(), None);
	assert!(!pair.server_conn_mut(server_ch).is_closed());

	const DATA: &[u8] = &[0xCD; 100];
	pair.client_datagrams(client_ch)
		.send(Bytes::from(DATA), Default::default())
		.unwrap();
	pair.drive();
	assert_matches!(pair.server_conn_mut(server_ch).poll(), Some(Event::DatagramReceived));
	assert_eq!(pair.server_datagrams(server_ch).recv().unwrap(), DATA);
}

/// RFC 9221's `max_datagram_frame_size` counts the frame type and length, so a 1200-byte limit
/// (quicly's) carries a 1197-byte payload behind a 2-byte length
#[test]
fn datagram_max_size_fills_peer_frame_limit() {
	let _guard = subscribe();
	let server = server::Config {
		transport: transport::Config {
			datagram: transport::datagram::Config {
				receive_buffer: Some(1200),
				..transport::Config::default().datagram
			},
			..transport::Config::default()
		},
		..server_config()
	};
	let transport = transport::Config {
		initial_mtu: 1452,
		mtu: None,
		..transport::Config::default()
	};
	let mut client = client_config();
	client.transport = transport;
	let mut pair = Pair::new(Default::default(), server);
	let (client_ch, _) = pair.connect_with(client);
	assert_eq!(pair.client_conn_mut(client_ch).datagrams().max_size(), Some(1197));
}

#[test]
fn datagram_larger_than_send_buffer_is_too_large() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	let mut client_config = client_config();
	let mut transport_config = transport::Config::default();
	transport_config.datagram.send_buffer = 1;
	client_config.transport = transport_config;
	let (client_ch, _) = pair.connect_with(client_config);

	assert_matches!(
		pair.client_datagrams(client_ch)
			.send(Bytes::from_static(&[0; 2]), Default::default()),
		Err(send::Error::TooLarge)
	);
	assert_matches!(
		pair.client_datagrams(client_ch)
			.send(Bytes::from_static(&[0; 2]), Default::default()),
		Err(send::Error::TooLarge)
	);
}

#[test]
fn datagram_unsupported() {
	let _guard = subscribe();
	let server = server::Config {
		transport: transport::Config {
			datagram: transport::datagram::Config {
				receive_buffer: None,
				..transport::Config::default().datagram
			},
			..transport::Config::default()
		},
		..server_config()
	};
	let mut pair = Pair::new(Default::default(), server);
	let (client_ch, server_ch) = pair.connect();
	assert_matches!(pair.server_conn_mut(server_ch).poll(), None);
	assert_matches!(pair.client_datagrams(client_ch).max_size(), None);

	match pair.client_datagrams(client_ch).send(Bytes::new(), Default::default()) {
		Err(send::Error::Unsupported) => {}
		Err(e) => panic!("unexpected error: {e}"),
		Ok(_) => panic!("unexpected success"),
	}
}

/// RFC 9221 section 3: a `max_datagram_frame_size` of 0 means no DATAGRAM support, and no frame,
/// not even an empty one, fits it
#[test]
fn datagram_zero_limit_unsupported() {
	let _guard = subscribe();
	let server = server::Config {
		transport: transport::Config {
			datagram: transport::datagram::Config {
				receive_buffer: Some(0),
				..transport::Config::default().datagram
			},
			..transport::Config::default()
		},
		..server_config()
	};
	let mut pair = Pair::new(Default::default(), server);
	let (client_ch, server_ch) = pair.connect();
	assert_matches!(pair.server_conn_mut(server_ch).poll(), None);
	assert_matches!(pair.client_datagrams(client_ch).max_size(), None);
	assert_matches!(
		pair.client_datagrams(client_ch).send(Bytes::new(), Default::default()),
		Err(send::Error::Unsupported)
	);
}

#[test]
fn large_initial() {
	let _guard = subscribe();
	let server_config = server::Config::new(Arc::new(server_crypto_with_alpn(vec![vec![0, 0, 0, 42]])));

	let mut pair = Pair::new(endpoint::Config::default(), server_config);
	let client_crypto = client_crypto_with_alpn((0..1000u32).map(|x| x.to_be_bytes().to_vec()).collect());
	let cfg = client::Config::new(Arc::new(client_crypto));
	let client_ch = pair.begin_connect(cfg);
	pair.drive();
	let server_ch = pair.server.assert_accept();
	assert_matches!(pair.client_conn_mut(client_ch).poll(), Some(Event::HandshakeDataReady));
	assert_matches!(pair.client_conn_mut(client_ch).poll(), Some(Event::Connected));
	assert_matches!(pair.server_conn_mut(server_ch).poll(), Some(Event::HandshakeDataReady));
	assert_matches!(pair.server_conn_mut(server_ch).poll(), Some(Event::HandshakeConfirmed));
	assert_matches!(pair.server_conn_mut(server_ch).poll(), Some(Event::Connected));
}

#[test]
/// Ensure that we don't yield a finish event before the actual FIN is acked so the peer isn't left
/// hanging
fn finish_acked() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	let (client_ch, server_ch) = pair.connect();

	let s = pair
		.client_streams(client_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();

	const MSG: &[u8] = b"hello";
	pair.client_send(client_ch, s).write(&mut &MSG[..]).unwrap();
	info!("client sends data to server");
	pair.drive_client(); // send data to server
	info!("server acknowledges data");
	pair.drive_server(); // process data and send data ack

	// Receive data
	assert_matches!(
		pair.server_conn_mut(server_ch).poll(),
		Some(Event::Stream(StreamEvent::Opened { dir: Dir::Uni }))
	);
	assert_matches!(
		pair.server_conn_mut(server_ch).poll(),
		Some(Event::Stream(StreamEvent::Readable { id })) if id == s
	);
	assert_matches!(pair.server_conn_mut(server_ch).poll(), None);

	assert_matches!(pair.server_streams(server_ch).accept(Dir::Uni), Some(stream) if stream == s);

	let mut recv = pair.server_recv(server_ch, s);
	let mut chunks = recv.read().unwrap();
	assert_matches!(
		chunks.next(usize::MAX),
		Ok(Some(chunk)) if chunk.bytes == MSG
	);
	assert_matches!(chunks.next(usize::MAX), Err(recv::Error::Blocked));
	let _ = chunks.finalize();

	// Finish before receiving data ack
	pair.client_send(client_ch, s).finish().unwrap();
	// Send FIN, receive data ack
	info!("client receives ACK, sends FIN");
	pair.drive_client();
	// Check for premature finish from data ack
	assert_matches!(pair.client_conn_mut(client_ch).poll(), None);
	// Process FIN ack
	info!("server ACKs FIN");
	pair.drive();
	assert_matches!(
		pair.client_conn_mut(client_ch).poll(),
		Some(Event::Stream(StreamEvent::Finished { id })) if id == s
	);

	let mut recv = pair.server_recv(server_ch, s);
	let mut chunks = recv.read().unwrap();
	assert_matches!(chunks.next(usize::MAX), Ok(None));
	let _ = chunks.finalize();
}

#[test]
/// Ensure that we don't yield a finish event while there's still unacknowledged data
fn finish_retransmit() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	let (client_ch, server_ch) = pair.connect();

	let s = pair
		.client_streams(client_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();

	const MSG: &[u8] = b"hello";
	pair.client_send(client_ch, s).write(&mut &MSG[..]).unwrap();
	pair.drive_client(); // send data to server
	pair.server.inbound.clear(); // Lose it

	// Send FIN
	pair.client_send(client_ch, s).finish().unwrap();
	pair.drive_client();
	// Process FIN
	pair.drive_server();
	// Receive FIN ack, but no data ack
	pair.drive_client();
	// Check for premature finish from FIN ack
	assert_matches!(pair.client_conn_mut(client_ch).poll(), None);
	// Recover
	pair.drive();
	assert_matches!(
		pair.client_conn_mut(client_ch).poll(),
		Some(Event::Stream(StreamEvent::Finished { id })) if id == s
	);

	assert_matches!(
		pair.server_conn_mut(server_ch).poll(),
		Some(Event::Stream(StreamEvent::Opened { dir: Dir::Uni }))
	);

	assert_matches!(pair.server_streams(server_ch).accept(Dir::Uni), Some(stream) if stream == s);

	let mut recv = pair.server_recv(server_ch, s);
	let mut chunks = recv.read().unwrap();
	assert_matches!(
		chunks.next(usize::MAX),
		Ok(Some(chunk)) if chunk.bytes == MSG
	);
	assert_matches!(chunks.next(usize::MAX), Ok(None));
	let _ = chunks.finalize();
}

/// Ensures that exchanging data on a client-initiated bidirectional stream works past the initial
/// stream window.
#[test]
fn repeated_request_response() {
	let _guard = subscribe();
	let server = server::Config {
		transport: transport::Config {
			limits: Limits {
				max_streams: NonZeroU64::new(1),
				..Limits::default()
			},
			..transport::Config::default()
		},
		..server_config()
	};
	let mut pair = Pair::new(Default::default(), server);
	let (client_ch, server_ch) = pair.connect();
	const REQUEST: &[u8] = b"hello";
	const RESPONSE: &[u8] = b"world";
	for _ in 0..3 {
		let s = pair
			.client_streams(client_ch)
			.open(Dir::Bi, Default::default())
			.unwrap();

		pair.client_send(client_ch, s).write(&mut &REQUEST[..]).unwrap();
		pair.client_send(client_ch, s).finish().unwrap();

		pair.drive();

		assert_eq!(pair.server_streams(server_ch).accept(Dir::Bi), Some(s));
		let mut recv = pair.server_recv(server_ch, s);
		let mut chunks = recv.read().unwrap();
		assert_matches!(
			chunks.next(usize::MAX),
			Ok(Some(chunk)) if chunk.bytes == REQUEST
		);

		assert_matches!(chunks.next(usize::MAX), Ok(None));
		let _ = chunks.finalize();
		pair.server_send(server_ch, s).write(&mut &RESPONSE[..]).unwrap();
		pair.server_send(server_ch, s).finish().unwrap();

		pair.drive();

		let mut recv = pair.client_recv(client_ch, s);
		let mut chunks = recv.read().unwrap();
		assert_matches!(
			chunks.next(usize::MAX),
			Ok(Some(chunk)) if chunk.bytes == RESPONSE
		);
		assert_matches!(chunks.next(usize::MAX), Ok(None));
		let _ = chunks.finalize();
	}
}

/// Ensures that the client sends an anti-deadlock probe after an incomplete server's first flight
#[test]
fn handshake_anti_deadlock_probe() {
	let _guard = subscribe();

	let (cert, key) = big_cert_and_key();
	let server = server_config_with_cert(cert.clone(), key);
	let client = client_config_with_certs(vec![cert]);
	let mut pair = Pair::new(Default::default(), server);

	let client_ch = pair.begin_connect(client);
	// Client sends initial
	pair.drive_client();
	// Server sends first flight, gets blocked on anti-amplification
	pair.drive_server();
	// Client acks...
	pair.drive_client();
	// ...but it's lost, so the server doesn't get anti-amplification credit from it
	pair.server.inbound.clear();
	// Client sends an anti-deadlock probe, and the handshake completes as usual.
	pair.drive();
	assert_matches!(pair.client_conn_mut(client_ch).poll(), Some(Event::HandshakeDataReady));
	assert_matches!(pair.client_conn_mut(client_ch).poll(), Some(Event::Connected));
}

/// The ack-eliciting frames the client has sent, which its probes carry
fn client_probes(pair: &mut Pair, ch: ConnectionHandle) -> u64 {
	let stats = pair.client_conn_mut(ch).stats();
	stats.frame_tx.stream + stats.frame_tx.ping + stats.frame_tx.immediate_ack
}

/// Drive the pair for `outage` with the client's packets lost, and the server's too unless
/// `one_way`, the server pinging every `ping` if set. Returns when, from the outage's start, the
/// client sent ack-eliciting frames.
fn probe_through_outage(
	pair: &mut Pair,
	(client_ch, server_ch): (ConnectionHandle, ConnectionHandle),
	outage: Duration,
	one_way: bool,
	ping: Option<Duration>,
) -> Vec<Duration> {
	pair.to_server.loss = 1.0;
	pair.to_client.loss = if one_way { 0.0 } else { 1.0 };
	let start = pair.time;
	let end = start + outage;
	let mut next_ping = ping.map(|_| start);
	let mut sent = client_probes(pair, client_ch);
	let mut probes = Vec::new();
	loop {
		if let (Some(at), Some(every)) = (next_ping, ping)
			&& at <= pair.time
		{
			pair.server_conn_mut(server_ch).ping();
			next_ping = Some(at + every);
		}
		pair.drive_client();
		pair.drive_server();
		let now = client_probes(pair, client_ch);
		if now > sent {
			probes.push(pair.time - start);
			sent = now;
		}
		let next = [pair.client.next_wakeup(), pair.server.next_wakeup(), next_ping]
			.into_iter()
			.flatten()
			.min();
		match next {
			Some(t) if t < end => pair.time = pair.time.max(t),
			_ => break,
		}
	}
	pair.time = end;
	pair.to_server.loss = 0.0;
	pair.to_client.loss = 0.0;
	probes
}

/// Connect `pair` over a 20 ms RTT, the client's last packet a PING whose ACK it has, so no
/// packet the server sends after it acknowledges anything new
fn settled_pair(mut pair: Pair) -> (Pair, ConnectionHandle, ConnectionHandle) {
	pair.latency = Duration::from_millis(10);
	let (client_ch, server_ch) = pair.connect();
	pair.time += Duration::from_millis(100);
	pair.drive();
	pair.client_conn_mut(client_ch).ping();
	pair.drive();
	(pair, client_ch, server_ch)
}

/// Drive the pair until `done` holds, returning when it first did
fn drive_until(pair: &mut Pair, mut done: impl FnMut(&mut Pair) -> bool) -> Instant {
	loop {
		pair.drive_client();
		pair.drive_server();
		if done(pair) {
			return pair.time;
		}
		let next = [pair.client.next_wakeup(), pair.server.next_wakeup()]
			.into_iter()
			.flatten()
			.min()
			.expect("the connection went idle");
		pair.time = pair.time.max(next);
	}
}

/// Assert that each gap between `probes` is about twice the one before
fn assert_backs_off(probes: &[Duration]) {
	assert!(probes.len() >= 5, "{probes:?}");
	for sends in probes.windows(3) {
		let (gap, next) = (sends[1] - sends[0], sends[2] - sends[1]);
		assert!(next >= gap.mul_f32(1.9), "the client backs off: {probes:?}");
	}
}

/// A path that drops everything for 5 s with the client's data in flight: the client's probes
/// back off while it hears nothing, and once the server is heard again the next one goes out
/// then, not at the backed-off timer.
#[test]
fn a_peer_heard_after_an_outage_brings_the_probe_forward() {
	let _guard = subscribe();
	let (mut pair, client_ch, server_ch) = settled_pair(Pair::default());
	let rtt = pair.latency * 2;
	let s = pair
		.client_streams(client_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();
	pair.client_send(client_ch, s).write(&mut &b"hello"[..]).unwrap();

	let probes = probe_through_outage(&mut pair, (client_ch, server_ch), Duration::from_secs(5), false, None);
	assert_backs_off(&probes);

	let end = pair.time;
	let due = pair.client.next_wakeup().unwrap() - end;
	assert!(due > 3 * rtt, "the backed-off probe is due {due:?} after the outage");
	pair.server_conn_mut(server_ch).ping();
	let sent = client_probes(&mut pair, client_ch);
	let probed = drive_until(&mut pair, |pair| client_probes(pair, client_ch) > sent) - end;
	assert!(
		probed <= rtt,
		"the client probed {probed:?} after the outage, not {due:?}"
	);
	// The probe is an IMMEDIATE_ACK, whose ACK declares the data lost.
	let received = pair.server_conn_mut(server_ch).stats().frame_rx.stream;
	let delivered = drive_until(&mut pair, |pair| {
		pair.server_conn_mut(server_ch).stats().frame_rx.stream > received
	}) - end;
	assert!(
		delivered <= 2 * rtt,
		"the client's data arrived {delivered:?} after the outage"
	);
}

/// While only the client's packets are lost, a server heard every 50 ms says nothing of the
/// path the client's probes take, so they back off as if it were silent.
#[test]
fn a_peer_heard_through_a_one_way_outage_leaves_the_backoff() {
	let _guard = subscribe();
	let (mut pair, client_ch, server_ch) = settled_pair(Pair::default());
	let s = pair
		.client_streams(client_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();
	pair.client_send(client_ch, s).write(&mut &b"hello"[..]).unwrap();

	let probes = probe_through_outage(
		&mut pair,
		(client_ch, server_ch),
		Duration::from_secs(5),
		true,
		Some(Duration::from_millis(50)),
	);
	assert_backs_off(&probes);
}

/// A packet the client handles in the instant its data goes out came before the data, so a probe
/// timeout with nothing heard after it still brings the next probe forward.
#[test]
fn a_peer_heard_as_the_data_left_still_brings_the_probe_forward() {
	let _guard = subscribe();
	let (mut pair, client_ch, server_ch) = settled_pair(Pair::default());
	let rtt = pair.latency * 2;
	pair.server_conn_mut(server_ch).ping();
	pair.drive_server();
	pair.time += pair.latency;
	let s = pair
		.client_streams(client_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();
	pair.client_send(client_ch, s).write(&mut &b"hello"[..]).unwrap();

	// The PING arrives now and the data leaves in the same drive, into an outage that lasts until
	// the client's first probe.
	pair.to_server.loss = 1.0;
	pair.to_client.loss = 1.0;
	for _ in 0..2 {
		let sent = client_probes(&mut pair, client_ch);
		drive_until(&mut pair, |pair| client_probes(pair, client_ch) > sent);
	}
	pair.to_server.loss = 0.0;
	pair.to_client.loss = 0.0;

	let end = pair.time;
	let due = pair.client.next_wakeup().unwrap() - end;
	assert!(due > 3 * rtt, "the backed-off probe is due {due:?} after the outage");
	pair.server_conn_mut(server_ch).ping();
	let sent = client_probes(&mut pair, client_ch);
	let probed = drive_until(&mut pair, |pair| client_probes(pair, client_ch) > sent) - end;
	assert!(
		probed <= rtt,
		"the client probed {probed:?} after the outage, not {due:?}"
	);
}

/// A peer heard from a new address moves the connection to a path with nothing in flight, so
/// there is no probe to bring forward, even when a timer runs before anything is sent on it.
#[test]
fn a_peer_heard_on_a_new_path_brings_no_probe_forward() {
	let _guard = subscribe();
	let (mut pair, client_ch, server_ch) = settled_pair(Pair::default());
	// The server's last packet is a PING whose ACK it has, so the client acknowledges nothing new.
	pair.server_conn_mut(server_ch).ping();
	pair.drive();
	let s = pair
		.server_streams(server_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();
	pair.server_send(server_ch, s).write(&mut &b"hello"[..]).unwrap();

	// The server's data and two probes go unheard.
	pair.to_server.loss = 1.0;
	pair.to_client.loss = 1.0;
	let server_probes = |pair: &mut Pair| {
		let stats = pair.server_conn_mut(server_ch).stats();
		stats.frame_tx.stream + stats.frame_tx.ping + stats.frame_tx.immediate_ack
	};
	for _ in 0..3 {
		let sent = server_probes(&mut pair);
		drive_until(&mut pair, |pair| server_probes(pair) > sent);
	}
	pair.to_server.loss = 0.0;
	pair.to_client.loss = 0.0;

	let moved = SocketAddr::new(Ipv4Addr::new(127, 0, 0, 1).into(), pair.next_port());
	pair.client.addr = moved;
	pair.client_conn_mut(client_ch).ping();
	pair.drive_client();
	let (arrival, ecn, datagram) = pair.server.inbound.pop_front().unwrap();
	pair.time = arrival;
	let mut buf = Vec::new();
	let Some(DatagramEvent::ConnectionEvent(ch, event)) = pair.server.endpoint.handle(
		arrival,
		Received {
			remote: moved,
			local_ip: None,
			ecn,
			data: datagram,
		},
		&mut buf,
	) else {
		panic!("the PING did not reach the connection");
	};
	// The timer runs before the PATH_CHALLENGE goes out, as when the socket is backpressured.
	let conn = pair.server_conn_mut(ch);
	conn.handle_event(event);
	assert_eq!(conn.remote_address(), moved);
	conn.handle_timeout(arrival);

	pair.drive();
	assert_eq!(pair.server_conn_mut(server_ch).remote_address(), moved);
}

/// A Version Negotiation packet carries no packet protection, so anyone on the path can send
/// one: it is not the peer heard again, and brings no probe forward.
#[test]
fn an_unprotected_packet_does_not_bring_the_probe_forward() {
	let _guard = subscribe();
	// Without connection IDs, a forged packet reaches the connection by its address alone.
	let cids: fn() -> Box<dyn ConnectionIdGenerator> = || Box::new(RandomConnectionIdGenerator::new(0));
	let (mut pair, client_ch, server_ch) = settled_pair(Pair::new(
		endpoint::Config {
			cid_generator: Some(Arc::new(cids)),
			..endpoint::Config::default()
		},
		server_config(),
	));
	let s = pair
		.client_streams(client_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();
	pair.client_send(client_ch, s).write(&mut &b"hello"[..]).unwrap();
	let probes = probe_through_outage(&mut pair, (client_ch, server_ch), Duration::from_secs(5), false, None);
	assert_backs_off(&probes);

	// Version Negotiation for a reserved version, with empty connection IDs.
	let forged = hex!("80 00000000 00 00 0a1a2a3a");
	pair.client
		.inbound
		.push_back((pair.time, None, BytesMut::from(&forged[..])));
	let sent = client_probes(&mut pair, client_ch);
	pair.drive_client();
	pair.drive_client();
	assert_eq!(
		client_probes(&mut pair, client_ch),
		sent,
		"a forged packet brought the probe forward"
	);
}

/// A client's lost Finished is probed at the Handshake space's own PTO, not one
/// still backed off from Initials lost before it (RFC 9002 A.11).
#[test]
fn lost_finished_after_initial_backoff() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	// Without latency the RTT is zero and every PTO is the timer granularity.
	pair.latency = Duration::from_millis(50);

	let client_ch = pair.begin_connect(client_config());
	// The client's first Initial and two probes are lost, backing its PTO off three times.
	for _ in 0..3 {
		pair.drive_client();
		pair.server.inbound.clear();
		pair.time = pair.client.next_wakeup().unwrap();
	}

	// The next probe gets through, until the client's Finished, which is lost.
	loop {
		pair.drive_client();
		if !pair.client_conn_mut(client_ch).is_handshaking() {
			break;
		}
		pair.drive_server();
		pair.time = min_opt(pair.client.next_wakeup(), pair.server.next_wakeup()).unwrap();
	}
	pair.server.inbound.clear();
	let lost = pair.time;

	let server_ch = pair.server.assert_accept();
	while pair.server_conn_mut(server_ch).is_handshaking() {
		assert!(pair.step());
	}
	// One RTT is 100ms, so a retransmitted Finished arrives within about 400ms;
	// still backed off, it takes 2.5s.
	let elapsed = pair.time - lost;
	assert!(elapsed < Duration::from_secs(1), "{elapsed:?}");
}

/// A lost ACK of a multi-datagram flight's head is repeated in the next packet the receiver
/// sends in that space, so the sender learns the head arrived without waiting on another ACK.
#[test]
fn lost_handshake_ack_repeats_in_next_packet() {
	let _guard = subscribe();
	let (cert, key) = big_cert_and_key();
	let mut pair = Pair::new(Default::default(), server_config_with_cert(cert.clone(), key));
	pair.latency = Duration::from_millis(50);

	pair.begin_connect(client_config_with_certs(vec![cert]));
	pair.drive_client();
	pair.time = pair.server.next_wakeup().unwrap();
	pair.server.drive(pair.time, pair.client.addr);
	// Only the head of the server's flight arrives. The rest, whether sent with it or paced
	// out behind it, is lost.
	let mut lost = pair.server.outbound.len() - 1;
	pair.server.outbound.truncate(1);
	pair.drive_server();
	while pair.server.next_wakeup() < pair.client.next_wakeup() {
		pair.time = pair.server.next_wakeup().unwrap();
		pair.server.drive(pair.time, pair.client.addr);
		lost += pair.server.outbound.len();
		pair.server.outbound.clear();
	}
	assert!(lost > 0);
	let server_ch = pair.server.assert_accept();

	// The client ACKs the head, and that ACK is lost.
	pair.time = pair.client.next_wakeup().unwrap();
	pair.drive_client();
	assert!(!pair.server.inbound.is_empty());
	pair.server.inbound.clear();
	let acks = pair.server_conn_mut(server_ch).stats().frame_rx.acks;

	// The client's next packet is its PTO probe, which carries the ACK again.
	while pair.server.inbound.is_empty() {
		pair.time = pair.client.next_wakeup().unwrap();
		pair.drive_client();
	}
	pair.time = pair.server.inbound.front().unwrap().0;
	pair.drive_server();
	assert_eq!(pair.server_conn_mut(server_ch).stats().frame_rx.acks, acks + 1);
}

/// A short idle timeout does not cut the handshake to two Initial flights.
///
/// Before an RTT sample the PTO is 999ms, so a 2s idle timeout is armed at 2997ms, while
/// the second probe is due 999ms + 1998ms after the first send, plus however late the first
/// probe went out. The handshake idle timeout governs until the handshake completes.
#[test]
fn handshake_outlasts_short_idle_timeout() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	let transport = transport::Config {
		idle_timeout: Some(Duration::from_secs(2)),
		..transport::Config::default()
	};
	let mut client = client_config();
	client.transport = transport;

	let client_ch = pair.begin_connect(client);
	let start = pair.time;
	// The first flight is lost, and so is the first probe, which goes out 2ms late.
	pair.drive_client();
	pair.server.inbound.clear();
	pair.time = pair.client.next_wakeup().unwrap() + Duration::from_millis(2);
	loop {
		pair.drive_client();
		pair.server.inbound.clear();
		pair.time = pair.client.next_wakeup().unwrap();
		// Past any pacing: the second probe, due 2999ms after the first send, beyond the
		// old 2997ms idle deadline.
		if pair.time - start > Duration::from_secs(2) {
			break;
		}
	}

	pair.drive();
	assert_matches!(pair.client_conn_mut(client_ch).poll(), Some(Event::HandshakeDataReady));
	assert_matches!(pair.client_conn_mut(client_ch).poll(), Some(Event::Connected));
}

/// A handshake idle timeout too long to add to an `Instant` never fires, instead of panicking.
#[test]
fn unbounded_handshake_idle_timeout() {
	let _guard = subscribe();
	let transport = transport::Config {
		handshake_timeout: Duration::MAX,
		..transport::Config::default()
	};
	let mut server = server_config();
	server.transport = transport.clone();
	let mut client = client_config();
	client.transport = transport;

	let mut pair = Pair::new(Default::default(), server);
	pair.connect_with(client);
}

/// Once established, a connection whose peer goes silent idles out at the negotiated idle
/// timeout, not the longer handshake idle timeout.
#[test]
fn established_idles_out_at_negotiated_timeout() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	let transport = transport::Config {
		idle_timeout: Some(Duration::from_secs(2)),
		..transport::Config::default()
	};
	let mut client = client_config();
	client.transport = transport;

	let client_ch = pair.begin_connect(client);
	loop {
		pair.drive_client();
		if !pair.client_conn_mut(client_ch).is_handshaking() {
			break;
		}
		pair.drive_server();
		pair.time = min_opt(pair.client.next_wakeup(), pair.server.next_wakeup()).unwrap();
	}
	let established = pair.time;

	// Nothing reaches the client once it is established.
	while !pair.client_conn_mut(client_ch).is_closed() {
		pair.client.inbound.clear();
		if !pair.step() && !pair.client_conn_mut(client_ch).is_closed() {
			pair.time = pair.client.next_wakeup().expect("an open connection has an idle timer");
		}
	}
	let elapsed = pair.time - established;
	assert!(
		(Duration::from_secs(2)..Duration::from_secs(3)).contains(&elapsed),
		"{elapsed:?}"
	);
}

/// Ensures that the server can respond with 3 initial packets during the handshake
/// before the anti-amplification limit kicks in when MTUs are similar.
#[test]
fn server_can_send_3_inital_packets() {
	let _guard = subscribe();
	let transport = transport::Config {
		// Assume a low-latency connection so pacing doesn't interfere with the test
		initial_rtt: Duration::from_millis(10),
		..transport::Config::default()
	};

	let (cert, key) = big_cert_and_key();
	let mut server = server_config_with_cert(cert.clone(), key);
	server.transport = transport;
	let client = client_config_with_certs(vec![cert]);
	let mut pair = Pair::new(Default::default(), server);

	let client_ch = pair.begin_connect(client);
	// Client sends initial
	pair.drive_client();
	// Server sends first flight, gets blocked on anti-amplification
	pair.drive_server();
	// Server should have queued 3 packets at this time
	assert_eq!(pair.client.inbound.len(), 3);

	pair.drive();
	assert_matches!(pair.client_conn_mut(client_ch).poll(), Some(Event::HandshakeDataReady));
	assert_matches!(pair.client_conn_mut(client_ch).poll(), Some(Event::Connected));
}

/// Generate a big fat certificate that can't fit inside the initial anti-amplification limit
fn big_cert_and_key() -> (CertificateDer<'static>, PrivateKeyDer<'static>) {
	let cert = rcgen::generate_simple_self_signed(
		Some("localhost".into())
			.into_iter()
			.chain((0..1000).map(|x| format!("foo_{x}")))
			.collect::<Vec<_>>(),
	)
	.unwrap();

	(
		cert.cert.into(),
		PrivateKeyDer::Pkcs8(cert.signing_key.serialize_der().into()),
	)
}

#[test]
fn malformed_token_len() {
	let _guard = subscribe();
	let client_addr = "[::2]:7890".parse().unwrap();
	let mut server = Endpoint::new(
		endpoint::Config {
			server: Some(server_config()),
			..endpoint::Config::default()
		},
		true,
	)
	.unwrap();
	let mut buf = Vec::with_capacity(server.max_udp_payload_size() as usize);
	server.handle(
		Instant::now(),
		Received {
			remote: client_addr,
			local_ip: None,
			ecn: None,
			data: hex!("8900 0000 0101 0000 1b1b 841b 0000 0000 3f00")[..].into(),
		},
		&mut buf,
	);
}

#[test]
fn loss_probe_requests_immediate_ack() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	let (client_ch, _) = pair.connect();
	pair.drive();

	let stats_after_connect = pair.client_conn_mut(client_ch).stats();

	// Lose a ping
	let default_mtu = mem::replace(&mut pair.mtu, 0);
	pair.client_conn_mut(client_ch).ping();
	pair.drive_client();
	pair.mtu = default_mtu;

	// Drive the connection further so a loss probe is sent
	pair.drive();

	// Assert that two IMMEDIATE_ACKs were sent (two loss probes)
	let stats_after_recovery = pair.client_conn_mut(client_ch).stats();
	assert_eq!(
		stats_after_recovery.frame_tx.immediate_ack - stats_after_connect.frame_tx.immediate_ack,
		2
	);
}

#[test]
/// This is mostly a sanity check to ensure our testing code is correctly dropping packets above the
/// pmtu
fn connect_too_low_mtu() {
	let _guard = subscribe();
	let mut pair = Pair::default();

	// The maximum payload size is lower than 1200, so no packages will get through!
	pair.mtu = 1000;

	pair.begin_connect(client_config());
	pair.drive();
	pair.server.assert_no_accept();
}

#[test]
fn connect_lost_mtu_probes_do_not_trigger_congestion_control() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	pair.mtu = 1200;

	let (client_ch, server_ch) = pair.connect();
	pair.drive();

	let client_stats = pair.client_conn_mut(client_ch).stats();
	let server_stats = pair.server_conn_mut(server_ch).stats();

	// Sanity check (all MTU probes should have been lost)
	assert_eq!(client_stats.path.sent_plpmtud_probes, 9);
	assert_eq!(client_stats.path.lost_plpmtud_probes, 9);
	assert_eq!(server_stats.path.sent_plpmtud_probes, 9);
	assert_eq!(server_stats.path.lost_plpmtud_probes, 9);

	// No congestion events
	assert_eq!(client_stats.path.congestion_events, 0);
	assert_eq!(server_stats.path.congestion_events, 0);
}

#[test]
fn connect_detects_mtu() {
	let _guard = subscribe();
	let max_udp_payload_and_expected_mtu = &[(1200, 1200), (1400, 1389), (1500, 1452)];

	for &(pair_max_udp, expected_mtu) in max_udp_payload_and_expected_mtu {
		let mut pair = Pair::default();
		pair.mtu = pair_max_udp;
		let (client_ch, server_ch) = pair.connect();
		pair.drive();

		assert_eq!(pair.client_conn_mut(client_ch).path_mtu(), expected_mtu);
		assert_eq!(pair.server_conn_mut(server_ch).path_mtu(), expected_mtu);
	}
}

#[test]
fn migrate_detects_new_mtu_and_respects_original_peer_max_udp_payload_size() {
	let _guard = subscribe();

	let client_max_udp_payload_size: u16 = 1400;

	// Set up a client with a max payload size of 1400 (and use the defaults for the server)
	let server_endpoint_config = endpoint::Config::default();
	let server = Endpoint::new(
		endpoint::Config {
			server: Some(server_config()),
			..server_endpoint_config
		},
		true,
	)
	.unwrap();
	let client_endpoint_config = endpoint::Config {
		max_udp_payload_size: client_max_udp_payload_size,
		..endpoint::Config::default()
	};
	let client = Endpoint::new(client_endpoint_config, true).unwrap();
	let mut pair = Pair::new_from_endpoint(client, server);
	pair.mtu = 1300;

	// Connect
	let (client_ch, server_ch) = pair.connect();
	pair.drive();

	// Sanity check: MTUD ran to completion (the numbers differ because binary search stops when
	// changes are smaller than 20, otherwise both endpoints would converge at the same MTU of 1300)
	assert_eq!(pair.client_conn_mut(client_ch).path_mtu(), 1293);
	assert_eq!(pair.server_conn_mut(server_ch).path_mtu(), 1300);

	// Migrate client to a different port (and simulate a higher path MTU)
	pair.mtu = 1500;
	pair.client.addr = SocketAddr::new(Ipv4Addr::new(127, 0, 0, 1).into(), pair.next_port());
	pair.client_conn_mut(client_ch).ping();
	pair.drive();

	// Sanity check: the server saw that the client address was updated
	assert_eq!(pair.server_conn_mut(server_ch).remote_address(), pair.client.addr);

	// MTU detection has successfully run after migrating
	assert_eq!(pair.server_conn_mut(server_ch).path_mtu(), client_max_udp_payload_size);

	// Sanity check: the client keeps the old MTU, because migration is triggered by incoming
	// packets from a different address
	assert_eq!(pair.client_conn_mut(client_ch).path_mtu(), 1293);
}

#[test]
fn connect_runs_mtud_again_after_600_seconds() {
	let _guard = subscribe();
	let mut server_config = server_config();
	let mut client_config = client_config();

	// Note: we use an infinite idle timeout to ensure we can wait 600 seconds without the
	// connection closing
	server_config.transport.idle_timeout = None;
	client_config.transport.idle_timeout = None;

	let mut pair = Pair::new(Default::default(), server_config);
	pair.mtu = 1400;
	let (client_ch, server_ch) = pair.connect_with(client_config);
	pair.drive();

	// Sanity check: the mtu has been discovered
	let client_conn = pair.client_conn_mut(client_ch);
	assert_eq!(client_conn.path_mtu(), 1389);
	assert_eq!(client_conn.stats().path.sent_plpmtud_probes, 5);
	assert_eq!(client_conn.stats().path.lost_plpmtud_probes, 3);
	let server_conn = pair.server_conn_mut(server_ch);
	assert_eq!(server_conn.path_mtu(), 1389);
	assert_eq!(server_conn.stats().path.sent_plpmtud_probes, 5);
	assert_eq!(server_conn.stats().path.lost_plpmtud_probes, 3);

	// Sanity check: the mtu does not change after the fact, even though the link now supports a
	// higher udp payload size
	pair.mtu = 1500;
	pair.drive();
	assert_eq!(pair.client_conn_mut(client_ch).path_mtu(), 1389);
	assert_eq!(pair.server_conn_mut(server_ch).path_mtu(), 1389);

	// The MTU changes after 600 seconds, because now MTUD runs for the second time
	pair.time += Duration::from_secs(600);
	pair.drive();
	assert!(!pair.client_conn_mut(client_ch).is_closed());
	assert!(!pair.server_conn_mut(client_ch).is_closed());
	assert_eq!(pair.client_conn_mut(client_ch).path_mtu(), 1452);
	assert_eq!(pair.server_conn_mut(server_ch).path_mtu(), 1452);
}

#[test]
fn blackhole_after_mtu_change_repairs_itself() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	pair.mtu = 1500;
	let (client_ch, server_ch) = pair.connect();
	pair.drive();

	// Sanity check
	assert_eq!(pair.client_conn_mut(client_ch).path_mtu(), 1452);
	assert_eq!(pair.server_conn_mut(server_ch).path_mtu(), 1452);

	// Back to the base MTU
	pair.mtu = 1200;

	// The payload will be sent in a single packet, because the detected MTU was 1444, but it will
	// be dropped because the link no longer supports that packet size!
	let payload = vec![42; 1300];
	let s = pair
		.client_streams(client_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();
	pair.client_send(client_ch, s).write(&mut &payload[..]).unwrap();
	let out_of_bounds = pair.drive_bounded();

	if out_of_bounds {
		panic!("Connections never reached an idle state");
	}

	let recv = pair.server_recv(server_ch, s);
	let buf = stream_chunks(recv);

	// The whole packet arrived in the end
	assert_eq!(buf.len(), 1300);

	// Sanity checks (black hole detected after 3 lost packets)
	let client_stats = pair.client_conn_mut(client_ch).stats();
	assert!(client_stats.path.lost_packets >= 3);
	assert!(client_stats.path.congestion_events >= 3);
	assert_eq!(client_stats.path.black_holes_detected, 1);
}

#[test]
fn mtud_probes_include_immediate_ack() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	let (client_ch, _) = pair.connect();
	pair.drive();

	let stats = pair.client_conn_mut(client_ch).stats();
	assert_eq!(stats.path.sent_plpmtud_probes, 4);

	// Each probe contains a ping and an immediate ack
	assert_eq!(stats.frame_tx.ping, 4);
	assert_eq!(stats.frame_tx.immediate_ack, 4);
}

#[test]
fn packet_splitting_with_default_mtu() {
	let _guard = subscribe();

	// The payload needs to be split in 2 in order to be sent, because it is higher than the max MTU
	let payload = vec![42; 1300];

	let mut pair = Pair::default();
	pair.mtu = 1200;
	let (client_ch, _) = pair.connect();
	pair.drive();

	let s = pair
		.client_streams(client_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();

	pair.client_send(client_ch, s).write(&mut &payload[..]).unwrap();
	pair.client.drive(pair.time, pair.server.addr);
	assert_eq!(pair.client.outbound.len(), 2);

	pair.drive_client();
	assert_eq!(pair.server.inbound.len(), 2);
}

#[test]
fn packet_splitting_not_necessary_after_higher_mtu_discovered() {
	let _guard = subscribe();
	let payload = vec![42; 1300];

	let mut pair = Pair::default();
	pair.mtu = 1500;

	let (client_ch, _) = pair.connect();
	pair.drive();

	let s = pair
		.client_streams(client_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();

	pair.client_send(client_ch, s).write(&mut &payload[..]).unwrap();
	pair.client.drive(pair.time, pair.server.addr);
	assert_eq!(pair.client.outbound.len(), 1);

	pair.drive_client();
	assert_eq!(pair.server.inbound.len(), 1);
}

#[test]
fn single_ack_eliciting_packet_triggers_ack_after_delay() {
	let _guard = subscribe();
	let mut pair = Pair::default_with_deterministic_pns();
	let (client_ch, _) = pair.connect_with(client_config_with_deterministic_pns());
	pair.drive();

	let stats_after_connect = pair.client_conn_mut(client_ch).stats();

	let start = pair.time;
	pair.client_conn_mut(client_ch).ping();
	pair.drive_client(); // Send ping
	pair.drive_server(); // Process ping
	pair.drive_client(); // Give the client a chance to process an ack, so our assertion can fail

	// Sanity check: the time hasn't advanced in the meantime)
	assert_eq!(pair.time, start);

	let stats_after_ping = pair.client_conn_mut(client_ch).stats();
	assert_eq!(stats_after_ping.frame_tx.ping - stats_after_connect.frame_tx.ping, 1);
	assert_eq!(stats_after_ping.frame_rx.acks - stats_after_connect.frame_rx.acks, 0);

	pair.client.capture_inbound_packets = true;
	pair.drive();
	let stats_after_drive = pair.client_conn_mut(client_ch).stats();
	assert_eq!(stats_after_drive.frame_rx.acks - stats_after_ping.frame_rx.acks, 1);

	// The time is start + max_ack_delay
	let default_max_ack_delay_ms = TransportParameters::default().max_ack_delay.into_inner();
	assert_eq!(pair.time, start + Duration::from_millis(default_max_ack_delay_ms));

	// The ACK delay is properly calculated
	assert_eq!(pair.client.captured_packets.len(), 1);
	let mut frames = frame::Iter::new(pair.client.captured_packets.remove(0).into())
		.unwrap()
		.collect::<Result<Vec<_>, _>>()
		.unwrap();
	assert_eq!(frames.len(), 1);
	if let Frame::Ack(ack) = frames.remove(0) {
		let ack_delay_exp = TransportParameters::default().ack_delay_exponent;
		let delay = ack.delay << ack_delay_exp.into_inner();
		assert_eq!(delay, default_max_ack_delay_ms * 1_000);
	} else {
		panic!("Expected ACK frame");
	}

	// Sanity check: no loss probe was sent, because the delayed ACK was received on time
	assert_eq!(stats_after_drive.frame_tx.ping - stats_after_connect.frame_tx.ping, 1);
}

#[test]
fn immediate_ack_triggers_ack() {
	let _guard = subscribe();
	let mut pair = Pair::default_with_deterministic_pns();
	let (client_ch, _) = pair.connect_with(client_config_with_deterministic_pns());
	pair.drive();

	let acks_after_connect = pair.client_conn_mut(client_ch).stats().frame_rx.acks;

	pair.client_conn_mut(client_ch).immediate_ack();
	pair.drive_client(); // Send immediate ack
	pair.drive_server(); // Process immediate ack
	pair.drive_client(); // Give the client a chance to process the ack

	let acks_after_ping = pair.client_conn_mut(client_ch).stats().frame_rx.acks;

	assert_eq!(acks_after_ping - acks_after_connect, 1);
}

#[test]
fn out_of_order_ack_eliciting_packet_triggers_ack() {
	let _guard = subscribe();
	let mut pair = Pair::default_with_deterministic_pns();
	let (client_ch, server_ch) = pair.connect_with(client_config_with_deterministic_pns());
	pair.drive();

	let default_mtu = pair.mtu;

	let client_stats_after_connect = pair.client_conn_mut(client_ch).stats();
	let server_stats_after_connect = pair.server_conn_mut(server_ch).stats();

	// Send a packet that won't arrive right away (it will be dropped and be re-sent later)
	pair.mtu = 0;
	pair.client_conn_mut(client_ch).ping();
	pair.drive_client();

	// Sanity check (ping sent, no ACK received)
	let client_stats_after_first_ping = pair.client_conn_mut(client_ch).stats();
	assert_eq!(
		client_stats_after_first_ping.frame_tx.ping - client_stats_after_connect.frame_tx.ping,
		1
	);
	assert_eq!(
		client_stats_after_first_ping.frame_rx.acks - client_stats_after_connect.frame_rx.acks,
		0
	);

	// Restore the default MTU and send another ping, which will arrive earlier than the dropped one
	pair.mtu = default_mtu;
	pair.client_conn_mut(client_ch).ping();
	pair.drive_client();
	pair.drive_server();
	pair.drive_client();

	// Client sanity check (ping sent, one ACK received)
	let client_stats_after_second_ping = pair.client_conn_mut(client_ch).stats();
	assert_eq!(
		client_stats_after_second_ping.frame_tx.ping - client_stats_after_connect.frame_tx.ping,
		2
	);
	assert_eq!(
		client_stats_after_second_ping.frame_rx.acks - client_stats_after_connect.frame_rx.acks,
		1
	);

	// Server checks (single ping received, ACK sent)
	let server_stats_after_second_ping = pair.server_conn_mut(server_ch).stats();
	assert_eq!(
		server_stats_after_second_ping.frame_rx.ping - server_stats_after_connect.frame_rx.ping,
		1
	);
	assert_eq!(
		server_stats_after_second_ping.frame_tx.acks - server_stats_after_connect.frame_tx.acks,
		1
	);
}

#[test]
fn single_ack_eliciting_packet_with_ce_bit_triggers_immediate_ack() {
	let _guard = subscribe();
	let mut pair = Pair::default_with_deterministic_pns();
	let (client_ch, _) = pair.connect_with(client_config_with_deterministic_pns());
	pair.drive();

	let stats_after_connect = pair.client_conn_mut(client_ch).stats();

	let start = pair.time;

	pair.client_conn_mut(client_ch).ping();

	pair.congestion_experienced = true;
	pair.drive_client(); // Send ping
	pair.congestion_experienced = false;

	pair.drive_server(); // Process ping, send ACK in response to congestion
	pair.drive_client(); // Process ACK

	// Sanity check: the time hasn't advanced in the meantime)
	assert_eq!(pair.time, start);

	let stats_after_ping = pair.client_conn_mut(client_ch).stats();
	assert_eq!(stats_after_ping.frame_tx.ping - stats_after_connect.frame_tx.ping, 1);
	assert_eq!(stats_after_ping.frame_rx.acks - stats_after_connect.frame_rx.acks, 1);
	assert_eq!(
		stats_after_ping.path.congestion_events - stats_after_connect.path.congestion_events,
		1
	);
}

fn setup_ack_frequency_test(max_ack_delay: Duration) -> (Pair, ConnectionHandle, ConnectionHandle) {
	let mut client_config = client_config_with_deterministic_pns();
	let ack_freq_config = transport::ack_frequency::Config {
		ack_eliciting_threshold: 10u32.into(),
		max_ack_delay: Some(max_ack_delay),
		..transport::ack_frequency::Config::default()
	};
	client_config.transport.ack_frequency = Some(ack_freq_config);
	client_config.transport.mtu = None; // To keep traffic cleaner
	client_config.transport.initial_rtt = Duration::from_millis(10); // To avoid delays from pacing

	let mut pair = Pair::default_with_deterministic_pns();
	pair.latency = Duration::from_millis(10); // Need latency to avoid an RTT = 0
	let (client_ch, server_ch) = pair.connect_with(client_config);
	pair.drive();

	assert_eq!(pair.client_conn_mut(client_ch).stats().frame_tx.ack_frequency, 1);
	assert_eq!(pair.client_conn_mut(client_ch).stats().frame_tx.ping, 0);
	(pair, client_ch, server_ch)
}

/// Verify that max ACK delay is counted from the first ACK-eliciting packet
#[test]
fn ack_frequency_ack_delayed_from_first_of_flight() {
	let _guard = subscribe();
	let (mut pair, client_ch, server_ch) = setup_ack_frequency_test(Duration::from_millis(30));

	// The client sends the following frames:
	//
	// * 0 ms: ping
	// * 5 ms: ping x2
	pair.client_conn_mut(client_ch).ping();
	pair.drive_client();

	pair.time += Duration::from_millis(5);
	for _ in 0..2 {
		pair.client_conn_mut(client_ch).ping();
		pair.drive_client();
	}

	pair.time += Duration::from_millis(5);
	// Server: receive the first ping and send no ACK
	let server_stats_before = pair.server_conn_mut(server_ch).stats();
	pair.drive_server();
	let server_stats_after = pair.server_conn_mut(server_ch).stats();
	assert_eq!(server_stats_after.frame_rx.ping - server_stats_before.frame_rx.ping, 1);
	assert_eq!(server_stats_after.frame_tx.acks - server_stats_before.frame_tx.acks, 0);

	// Server: receive the second and third pings and send no ACK
	pair.time += Duration::from_millis(10);
	let server_stats_before = pair.server_conn_mut(server_ch).stats();
	pair.drive_server();
	let server_stats_after = pair.server_conn_mut(server_ch).stats();
	assert_eq!(server_stats_after.frame_rx.ping - server_stats_before.frame_rx.ping, 2);
	assert_eq!(server_stats_after.frame_tx.acks - server_stats_before.frame_tx.acks, 0);

	// Server: Send an ACK after ACK delay expires
	pair.time += Duration::from_millis(20);
	let server_stats_before = pair.server_conn_mut(server_ch).stats();
	pair.drive_server();
	let server_stats_after = pair.server_conn_mut(server_ch).stats();
	assert_eq!(server_stats_after.frame_tx.acks - server_stats_before.frame_tx.acks, 1);
}

#[test]
fn ack_frequency_ack_sent_after_max_ack_delay() {
	let _guard = subscribe();
	let max_ack_delay = Duration::from_millis(30);
	let (mut pair, client_ch, server_ch) = setup_ack_frequency_test(max_ack_delay);

	// Client sends a ping
	pair.client_conn_mut(client_ch).ping();
	pair.drive_client();

	// Server: receive the ping, send no ACK
	pair.time += pair.latency;
	let server_stats_before = pair.server_conn_mut(server_ch).stats();
	pair.drive_server();
	let server_stats_after = pair.server_conn_mut(server_ch).stats();
	assert_eq!(server_stats_after.frame_rx.ping - server_stats_before.frame_rx.ping, 1);
	assert_eq!(server_stats_after.frame_tx.acks - server_stats_before.frame_tx.acks, 0);

	// Server: send an ack after max_ack_delay has elapsed
	pair.time += max_ack_delay;
	let server_stats_before = pair.server_conn_mut(server_ch).stats();
	pair.drive_server();
	let server_stats_after = pair.server_conn_mut(server_ch).stats();
	assert_eq!(server_stats_after.frame_rx.ping - server_stats_before.frame_rx.ping, 0);
	assert_eq!(server_stats_after.frame_tx.acks - server_stats_before.frame_tx.acks, 1);
}

#[test]
fn ack_frequency_ack_sent_after_packets_above_threshold() {
	let _guard = subscribe();
	let max_ack_delay = Duration::from_millis(30);
	let (mut pair, client_ch, server_ch) = setup_ack_frequency_test(max_ack_delay);

	// The client sends the following frames:
	//
	// * 0 ms: ping
	// * 5 ms: ping (11x)
	pair.client_conn_mut(client_ch).ping();
	pair.drive_client();

	pair.time += Duration::from_millis(5);
	for _ in 0..11 {
		pair.client_conn_mut(client_ch).ping();
		pair.drive_client();
	}

	// Server: receive the first ping, send no ACK
	pair.time += Duration::from_millis(5);
	let server_stats_before = pair.server_conn_mut(server_ch).stats();
	pair.drive_server();
	let server_stats_after = pair.server_conn_mut(server_ch).stats();
	assert_eq!(server_stats_after.frame_rx.ping - server_stats_before.frame_rx.ping, 1);
	assert_eq!(server_stats_after.frame_tx.acks - server_stats_before.frame_tx.acks, 0);

	// Server: receive the remaining pings, send ACK
	pair.time += Duration::from_millis(5);
	let server_stats_before = pair.server_conn_mut(server_ch).stats();
	pair.drive_server();
	let server_stats_after = pair.server_conn_mut(server_ch).stats();
	assert_eq!(server_stats_after.frame_rx.ping - server_stats_before.frame_rx.ping, 11);
	assert_eq!(server_stats_after.frame_tx.acks - server_stats_before.frame_tx.acks, 1);
}

#[test]
fn ack_frequency_ack_sent_after_reordered_packets_below_threshold() {
	let _guard = subscribe();
	let max_ack_delay = Duration::from_millis(30);
	let (mut pair, client_ch, server_ch) = setup_ack_frequency_test(max_ack_delay);

	// The client sends the following frames:
	//
	// * 0 ms: ping
	// * 5 ms: ping (lost)
	// * 5 ms: ping
	pair.client_conn_mut(client_ch).ping();
	pair.drive_client();

	pair.time += Duration::from_millis(5);

	// Send and lose an ack-eliciting packet
	pair.mtu = 0;
	pair.client_conn_mut(client_ch).ping();
	pair.drive_client();

	// Restore the default MTU and send another ping, which will arrive earlier than the dropped one
	pair.mtu = DEFAULT_MTU;
	pair.client_conn_mut(client_ch).ping();
	pair.drive_client();

	// Server: receive first ping, send no ACK
	pair.time += Duration::from_millis(5);
	let server_stats_before = pair.server_conn_mut(server_ch).stats();
	pair.drive_server();
	let server_stats_after = pair.server_conn_mut(server_ch).stats();
	assert_eq!(server_stats_after.frame_rx.ping - server_stats_before.frame_rx.ping, 1);
	assert_eq!(server_stats_after.frame_tx.acks - server_stats_before.frame_tx.acks, 0);

	// Server: receive second ping, send no ACK
	pair.time += Duration::from_millis(5);
	let server_stats_before = pair.server_conn_mut(server_ch).stats();
	pair.drive_server();
	let server_stats_after = pair.server_conn_mut(server_ch).stats();
	assert_eq!(server_stats_after.frame_rx.ping - server_stats_before.frame_rx.ping, 1);
	assert_eq!(server_stats_after.frame_tx.acks - server_stats_before.frame_tx.acks, 0);
}

#[test]
fn ack_frequency_ack_sent_after_reordered_packets_above_threshold() {
	let _guard = subscribe();
	let max_ack_delay = Duration::from_millis(30);
	let (mut pair, client_ch, server_ch) = setup_ack_frequency_test(max_ack_delay);

	// Send a ping
	pair.client_conn_mut(client_ch).ping();
	pair.drive_client();

	// Send and lose as many ack-eliciting packets as the default reordering threshold
	pair.time += Duration::from_millis(5);
	pair.mtu = 0;
	for _ in 0..3 {
		pair.client_conn_mut(client_ch).ping();
		pair.drive_client();
	}

	// Restore the default MTU and send another ping, which will arrive earlier than the dropped ones
	pair.mtu = DEFAULT_MTU;
	pair.client_conn_mut(client_ch).ping();
	pair.drive_client();

	// Server: receive first ping, send no ACK
	pair.time += Duration::from_millis(5);
	let server_stats_before = pair.server_conn_mut(server_ch).stats();
	pair.drive_server();
	let server_stats_after = pair.server_conn_mut(server_ch).stats();
	assert_eq!(server_stats_after.frame_rx.ping - server_stats_before.frame_rx.ping, 1);
	assert_eq!(server_stats_after.frame_tx.acks - server_stats_before.frame_tx.acks, 0);

	// Server: receive remaining ping, send ACK
	pair.time += Duration::from_millis(5);
	let server_stats_before = pair.server_conn_mut(server_ch).stats();
	pair.drive_server();
	let server_stats_after = pair.server_conn_mut(server_ch).stats();
	assert_eq!(server_stats_after.frame_rx.ping - server_stats_before.frame_rx.ping, 1);
	assert_eq!(server_stats_after.frame_tx.acks - server_stats_before.frame_tx.acks, 1);
}

#[test]
fn ack_frequency_update_max_delay() {
	let _guard = subscribe();
	let (mut pair, client_ch, server_ch) = setup_ack_frequency_test(Duration::from_millis(200));

	// Ack frequency was sent initially
	assert_eq!(pair.server_conn_mut(server_ch).stats().frame_rx.ack_frequency, 1);

	// Client sends a PING
	info!("first ping");
	pair.client_conn_mut(client_ch).ping();
	pair.drive();

	// No change in ACK frequency
	assert_eq!(pair.server_conn_mut(server_ch).stats().frame_rx.ack_frequency, 1);

	// RTT jumps, client sends another ping
	info!("delayed ping");
	pair.latency *= 10;
	pair.client_conn_mut(client_ch).ping();
	pair.drive();

	// ACK frequency updated
	assert!(pair.server_conn_mut(server_ch).stats().frame_rx.ack_frequency >= 2);
}

fn stream_chunks(mut recv: recv::Stream<'_>) -> Vec<u8> {
	let mut buf = Vec::new();

	let mut chunks = recv.read().unwrap();
	while let Ok(Some(chunk)) = chunks.next(usize::MAX) {
		buf.extend(chunk.bytes);
	}

	let _ = chunks.finalize();

	buf
}

/// Verify that an endpoint which receives but does not send ACK-eliciting data still receives ACKs
/// occasionally. This is not required for conformance, but makes loss detection more responsive and
/// reduces receiver memory use.
#[test]
fn pure_sender_voluntarily_acks() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	let (client_ch, server_ch) = pair.connect();

	let receiver_acks_initial = pair.server_conn_mut(server_ch).stats().frame_rx.acks;

	for _ in 0..100 {
		const MSG: &[u8] = b"hello";
		pair.client_datagrams(client_ch)
			.send(Bytes::from_static(MSG), Default::default())
			.unwrap();
		pair.drive();
		assert_eq!(pair.server_datagrams(server_ch).recv().unwrap(), MSG);
	}

	let receiver_acks_final = pair.server_conn_mut(server_ch).stats().frame_rx.acks;
	assert!(receiver_acks_final > receiver_acks_initial);
}

/// Initials rejected under saturation (here via `max_incoming(0)`) are dropped without
/// sending a response: the client times out rather than receiving a CONNECTION_REFUSED.
#[test]
fn silently_drop_rejected_initials() {
	let _guard = subscribe();
	let mut server_config = server_config();
	server_config.max_incoming = 0;
	let mut pair = Pair::new(endpoint::Config::default(), server_config);

	let client_ch = pair.begin_connect(client_config());
	pair.drive();
	pair.server.assert_no_accept();
	// `drive()` stops once the client's only remaining timer is its idle timeout; advance
	// past it so the unanswered attempt gives up.
	pair.time += Duration::from_secs(60);
	pair.drive();
	assert_matches!(
		pair.client_conn_mut(client_ch).poll(),
		Some(Event::ConnectionLost {
			reason: ConnectionError::HandshakeTimeout,
		})
	);
}

#[test]
fn reject_manually() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	pair.server.handle_incoming = Box::new(|_| IncomingConnectionBehavior::Reject);

	// The server should now reject incoming connections.
	let client_ch = pair.begin_connect(client_config());
	pair.drive();
	pair.server.assert_no_accept();
	let client = pair.client.connections.get_mut(&client_ch).unwrap();
	assert!(client.is_closed());
	assert!(matches!(
		client.poll(),
		Some(Event::ConnectionLost {
			reason: ConnectionError::ConnectionClosed(close)
		}) if close.error_code == TransportErrorCode::CONNECTION_REFUSED
	));
}

#[test]
fn validate_then_reject_manually() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	pair.server.handle_incoming = Box::new({
		let mut i = 0;
		move |incoming| {
			if incoming.remote_address_validated() {
				assert_eq!(i, 1);
				i += 1;
				IncomingConnectionBehavior::Reject
			} else {
				assert_eq!(i, 0);
				i += 1;
				IncomingConnectionBehavior::Retry
			}
		}
	});

	// The server should now retry and reject incoming connections.
	let client_ch = pair.begin_connect(client_config());
	pair.drive();
	pair.server.assert_no_accept();
	let client = pair.client.connections.get_mut(&client_ch).unwrap();
	assert!(client.is_closed());
	assert!(matches!(
		client.poll(),
		Some(Event::ConnectionLost {
			reason: ConnectionError::ConnectionClosed(close)
		}) if close.error_code == TransportErrorCode::CONNECTION_REFUSED
	));
	pair.drive();
	assert_matches!(pair.client_conn_mut(client_ch).poll(), None);
	assert_eq!(pair.client.known_connections(), 0);
	assert_eq!(pair.client.known_cids(), 0);
	assert_eq!(pair.server.known_connections(), 0);
	assert_eq!(pair.server.known_cids(), 0);
}

#[test]
fn endpoint_and_connection_impl_send_sync() {
	const fn is_send_sync<T: Send + Sync>() {}
	is_send_sync::<Endpoint>();
	is_send_sync::<Connection>();
}

#[test]
fn stream_gso() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	// Cubic, whose window-derived pacer releases the batches back to back at zero RTT
	let mut config = client_config();
	config.transport = cubic_transport();
	let (client_ch, _) = pair.connect_with(config);

	let s = pair
		.client_streams(client_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();

	let initial_ios = pair.client_conn_mut(client_ch).stats().udp_tx.ios;

	// Send 20KiB of stream data, which comfortably fits inside two `tests::util::MAX_DATAGRAMS`
	// datagram batches
	info!("sending");
	for _ in 0..20 {
		pair.client_send(client_ch, s).write(&mut &[0; 1024][..]).unwrap();
	}
	pair.client_send(client_ch, s).finish().unwrap();
	pair.drive();
	let final_ios = pair.client_conn_mut(client_ch).stats().udp_tx.ios;
	assert_eq!(final_ios - initial_ios, 2);
}

#[test]
fn datagram_gso() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	let (client_ch, server_ch) = pair.connect();

	// Sending ack-eliciting packet from server let client send ACK, which prevents
	// sending bundled ACK for a while.
	pair.server_datagrams(server_ch)
		.send(Bytes::new(), Default::default())
		.unwrap();
	pair.drive();

	let initial_ios = pair.client_conn_mut(client_ch).stats().udp_tx.ios;
	let initial_bytes = pair.client_conn_mut(client_ch).stats().udp_tx.bytes;

	// Send 10 datagrams above half the MTU, which fits inside a `tests::util::MAX_DATAGRAMS`
	// datagram batch
	info!("sending");
	const DATAGRAM_LEN: usize = 1024;
	const DATAGRAMS: usize = 10;
	for _ in 0..DATAGRAMS {
		pair.client_datagrams(client_ch)
			.send(Bytes::from_static(&[0; DATAGRAM_LEN]), Default::default())
			.unwrap();
	}
	pair.drive();
	let final_ios = pair.client_conn_mut(client_ch).stats().udp_tx.ios;
	let final_bytes = pair.client_conn_mut(client_ch).stats().udp_tx.bytes;
	assert_eq!(final_ios - initial_ios, 1);
	// Expected overhead: flags + CID + PN + tag + frame type + frame length = 1 + 8 + 1 + 16 + 1 + 2 = 29
	assert_eq!(final_bytes - initial_bytes, ((29 + DATAGRAM_LEN) * DATAGRAMS) as u64);
}

#[test]
fn gso_truncation() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	let (client_ch, server_ch) = pair.connect();

	let initial_ios = pair.client_conn_mut(client_ch).stats().udp_tx.ios;

	// Send three application datagrams such that each is large to be combined with another in a
	// single MTU, and the second datagram would require an unreasonably large amount of padding to
	// produce a QUIC packet of the same length as the first.
	info!("sending");
	const SIZES: [usize; 3] = [1024, 768, 768];
	for len in SIZES {
		pair.client_datagrams(client_ch)
			.send(Bytes::from(vec![0; len]), Default::default())
			.unwrap();
	}
	pair.drive();
	let final_ios = pair.client_conn_mut(client_ch).stats().udp_tx.ios;
	assert_eq!(final_ios - initial_ios, 2);
	for len in SIZES {
		assert_eq!(
			pair.server_datagrams(server_ch).recv().expect("datagram lost").len(),
			len
		);
	}
}

#[test]
fn pad_initial_to_min_mtu() {
	let _guard = subscribe();
	for min_mtu in [INITIAL_MTU, 1280, 1333] {
		let transport = transport::Config {
			initial_mtu: 1452,
			min_mtu,
			mtu: None,
			..transport::Config::default()
		};
		let mut config = client_config();
		config.transport = transport;
		let mut pair = Pair::default();
		let client_ch = pair.begin_connect(config);
		let expected_size = usize::from(min_mtu);

		// Check the first Initial and two PTO retransmissions after dropping it.
		for attempt in 0..3 {
			pair.client.drive(pair.time, pair.server.addr);
			assert!(!pair.client.outbound.is_empty());
			for (transmit, data) in &pair.client.outbound {
				assert_eq!(transmit.size, expected_size);
				assert_eq!(data.len(), expected_size);
			}
			if attempt < 2 {
				pair.client.outbound.clear();
				pair.time = pair.client.next_wakeup().unwrap();
			}
		}

		pair.drive();
		let server_ch = pair.server.assert_accept();
		pair.finish_connect(client_ch, server_ch);
	}
}

#[test]
fn min_mtu_does_not_pad_application_data() {
	let _guard = subscribe();
	const MTU: u16 = 1333;
	let transport = transport::Config {
		min_mtu: MTU,
		mtu: None,
		..transport::Config::default()
	};
	let mut config = client_config();
	config.transport = transport;
	let mut pair = Pair::default();
	let (client_ch, server_ch) = pair.connect_with(config);

	let message = Bytes::from_static(b"hello");
	pair.client_datagrams(client_ch)
		.send(message.clone(), Default::default())
		.unwrap();
	pair.client.drive(pair.time, pair.server.addr);
	assert_eq!(pair.client.outbound.len(), 1);
	assert!(pair.client.outbound[0].0.size < usize::from(MTU));
	assert!(pair.client.outbound[0].1.len() < usize::from(MTU));
	pair.drive();
	assert_eq!(pair.server_datagrams(server_ch).recv().unwrap(), message);
}

#[test]
fn min_mtu_limits_application_loss_probes() {
	let _guard = subscribe();
	const MIN_MTU: u16 = 1280;
	const MTU: u16 = 1452;
	let transport = transport::Config {
		initial_mtu: MTU,
		min_mtu: MIN_MTU,
		mtu: None,
		pad_to_mtu: true,
		..transport::Config::default()
	};
	let mut config = client_config();
	config.transport = transport;
	let mut pair = Pair::default();
	let (client_ch, _) = pair.connect_with(config);

	pair.client_conn_mut(client_ch).ping();
	pair.client.drive(pair.time, pair.server.addr);
	assert_eq!(pair.client.outbound.len(), 1);
	assert_eq!(pair.client.outbound[0].0.size, usize::from(MTU));
	pair.client.outbound.clear();

	// Fill the loss probe with stream data to check the configured minimum MTU is the limit.
	pair.time = pair.client.next_wakeup().unwrap();
	let now = pair.time;
	let client = pair.client_conn_mut(client_ch);
	client.handle_timeout(now);
	let stream = client.streams().open(Dir::Uni, Default::default()).unwrap();
	client.send_stream(stream).write(&mut &[42; 3000][..]).unwrap();
	let mut buf = Vec::new();
	let transmit = client.poll_transmit(now, 1, &mut buf).unwrap();
	assert_eq!(transmit.size, usize::from(MIN_MTU));
	assert_eq!(buf.len(), usize::from(MIN_MTU));
	pair.drive();
}

#[test]
fn pad_server_initial_to_min_mtu() {
	let _guard = subscribe();
	const MIN_MTU: u16 = 1333;
	let transport = transport::Config {
		initial_mtu: 1452,
		min_mtu: MIN_MTU,
		mtu: None,
		..transport::Config::default()
	};
	let mut config = server_config();
	config.transport = transport;
	let mut pair = Pair::new(Default::default(), config);
	let client_ch = pair.begin_connect(client_config());
	pair.drive_client();
	pair.server.drive(pair.time, pair.client.addr);
	let (transmit, data) = pair.server.outbound.front().unwrap();
	assert_eq!(transmit.size, usize::from(MIN_MTU));
	assert_eq!(data.len(), usize::from(MIN_MTU));

	pair.drive();
	let server_ch = pair.server.assert_accept();
	pair.finish_connect(client_ch, server_ch);
}

#[test]
fn min_mtu_respects_peer_max_udp_payload_size() {
	let _guard = subscribe();
	const PEER_MAX: u16 = 1280;
	let transport = transport::Config {
		min_mtu: 1452,
		mtu: None,
		..transport::Config::default()
	};
	let mut config = server_config();
	config.transport = transport;
	let server = Endpoint::new(
		endpoint::Config {
			server: Some(config),
			..endpoint::Config::default()
		},
		true,
	)
	.unwrap();
	let endpoint_config = endpoint::Config {
		max_udp_payload_size: PEER_MAX,
		..endpoint::Config::default()
	};
	let client = Endpoint::new(endpoint_config, true).unwrap();
	let mut pair = Pair::new_from_endpoint(client, server);
	let client_ch = pair.begin_connect(client_config());
	pair.drive_client();
	pair.server.drive(pair.time, pair.client.addr);
	let (transmit, data) = pair.server.outbound.front().unwrap();
	assert_eq!(transmit.size, usize::from(PEER_MAX));
	assert_eq!(data.len(), usize::from(PEER_MAX));

	pair.drive();
	let server_ch = pair.server.assert_accept();
	pair.finish_connect(client_ch, server_ch);
}

/// Verify that UDP datagrams are padded to MTU if specified in the transport config.
#[test]
fn pad_to_mtu() {
	let _guard = subscribe();
	const MTU: u16 = 1333;
	let client_config = {
		let mut c_config = client_config();
		let t_config = transport::Config {
			initial_mtu: MTU,
			mtu: None,
			pad_to_mtu: true,
			..transport::Config::default()
		};
		c_config.transport = t_config;
		c_config
	};
	let mut pair = Pair::default();
	let (client_ch, server_ch) = pair.connect_with(client_config);

	let initial_ios = pair.client_conn_mut(client_ch).stats().udp_tx.ios;
	pair.server.capture_inbound_packets = true;

	info!("sending");
	// Send two datagrams significantly smaller than MTU, but large enough to require two UDP datagrams.
	const LEN_1: usize = 800;
	const LEN_2: usize = 600;
	pair.client_datagrams(client_ch)
		.send(Bytes::from(vec![0; LEN_1]), Default::default())
		.unwrap();
	pair.client_datagrams(client_ch)
		.send(Bytes::from(vec![0; LEN_2]), Default::default())
		.unwrap();
	pair.client.drive(pair.time, pair.server.addr);

	// Check padding
	assert_eq!(pair.client.outbound.len(), 2);
	assert_eq!(pair.client.outbound[0].0.size, usize::from(MTU));
	assert_eq!(pair.client.outbound[0].1.len(), usize::from(MTU));
	assert_eq!(pair.client.outbound[1].0.size, usize::from(MTU));
	assert_eq!(pair.client.outbound[1].1.len(), usize::from(MTU));
	pair.drive_client();
	assert_eq!(pair.server.inbound.len(), 2);
	assert_eq!(pair.server.inbound[0].2.len(), usize::from(MTU));
	assert_eq!(pair.server.inbound[1].2.len(), usize::from(MTU));
	pair.drive();

	// Check that both datagrams ended up in the same GSO batch
	let final_ios = pair.client_conn_mut(client_ch).stats().udp_tx.ios;
	assert_eq!(final_ios - initial_ios, 1);

	assert_eq!(
		pair.server_datagrams(server_ch).recv().expect("datagram lost").len(),
		LEN_1
	);
	assert_eq!(
		pair.server_datagrams(server_ch).recv().expect("datagram lost").len(),
		LEN_2
	);
}

/// Verify that a large application datagram is sent successfully when an ACK frame too large to fit
/// alongside it is also queued, in exactly 2 UDP datagrams.
#[test]
fn large_datagram_with_acks() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	let (client_ch, server_ch) = pair.connect();

	// Force the client to generate a large ACK frame by dropping several packets
	for _ in 0..10 {
		pair.server_conn_mut(server_ch).ping();
		pair.drive_server();
		pair.client.inbound.pop_back();
		pair.server_conn_mut(server_ch).ping();
		pair.drive_server();
	}

	let max_size = pair.client_datagrams(client_ch).max_size().unwrap();
	let msg = Bytes::from(vec![0; max_size]);
	pair.client_datagrams(client_ch)
		.send(msg.clone(), Default::default())
		.unwrap();
	let initial_datagrams = pair.client_conn_mut(client_ch).stats().udp_tx.datagrams;
	pair.drive();
	let final_datagrams = pair.client_conn_mut(client_ch).stats().udp_tx.datagrams;
	assert_eq!(pair.server_datagrams(server_ch).recv().unwrap(), msg);
	assert_eq!(final_datagrams - initial_datagrams, 2);
}

/// Verify that an ACK prompted by receipt of many non-ACK-eliciting packets is sent alongside
/// outgoing application datagrams too large to coexist in the same packet with it.
#[test]
fn voluntary_ack_with_large_datagrams() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	let (client_ch, _) = pair.connect();

	// Prompt many large ACKs from the server
	let initial_datagrams = pair.client_conn_mut(client_ch).stats().udp_tx.datagrams;
	// Send enough packets that we're confident some packet numbers will be skipped, ensuring that
	// larger ACKs occur
	const COUNT: usize = 256;
	for _ in 0..COUNT {
		let max_size = pair.client_datagrams(client_ch).max_size().unwrap();
		pair.client_datagrams(client_ch)
			.send(Bytes::from(vec![0; max_size]), Default::default())
			.unwrap();
		pair.drive();
	}
	let final_datagrams = pair.client_conn_mut(client_ch).stats().udp_tx.datagrams;
	// Failure may indicate `max_size` is too small and ACKs are reliably being packed into the same
	// datagram, which is reasonable behavior but makes this test ineffective.
	assert_ne!(
		final_datagrams - initial_datagrams,
		COUNT as u64,
		"client should have sent some ACK-only packets"
	);
}

#[test]
fn ack_bundled_with_datagrams() {
	let _guard = subscribe();
	let mut pair = Pair::default_with_deterministic_pns();
	let (client_ch, server_ch) = pair.connect_with(client_config_with_deterministic_pns());

	// Send packet from client and then send from server. the packet from server should include ACKs
	pair.client_datagrams(client_ch)
		.send(Bytes::from(vec![0; 1]), Default::default())
		.unwrap();
	pair.drive_client();
	pair.drive_server();

	let server_tx_acks_before_datagram = pair.server_conn_mut(server_ch).stats().frame_tx.acks;
	let server_tx_packets_before_datagram = pair.server_conn_mut(server_ch).stats().udp_tx.datagrams;

	pair.server_datagrams(server_ch)
		.send(Bytes::from(vec![0; 1]), Default::default())
		.unwrap();
	pair.drive_server();

	let server_tx_acks_after_datagram = pair.server_conn_mut(server_ch).stats().frame_tx.acks;

	assert_eq!(
		server_tx_acks_before_datagram + 1,
		server_tx_acks_after_datagram,
		"server should have sent ACK frame along with DATAGRAM frame"
	);
	assert_eq!(
		server_tx_packets_before_datagram + 1,
		pair.server_conn_mut(server_ch).stats().udp_tx.datagrams,
		"server should not have sent two or more QUIC packets"
	);

	pair.drive();

	// No more acks should be sent from server since ACK to the first packet has been sent with the datagram
	assert_eq!(
		server_tx_acks_after_datagram,
		pair.server_conn_mut(server_ch).stats().frame_tx.acks,
		"server should not sent ACK frames"
	);
}

#[test]
fn path_changes_unblock_oversized_datagrams() {
	let _guard = subscribe();
	for migrate in [false, true] {
		let mut pair = Pair::default();
		let (client_ch, server_ch) = pair.connect();
		pair.drive();
		let old_max = pair.server_datagrams(server_ch).max_size().unwrap();
		assert!(old_max > 1200);
		let empty_space = pair.server_datagrams(server_ch).send_buffer_space();
		let data = Bytes::from(vec![42; old_max]);
		while pair.server_datagrams(server_ch).send_buffer_space() >= old_max {
			pair.server_datagrams(server_ch)
				.send(data.clone(), Default::default())
				.unwrap();
		}
		while pair.server_conn_mut(server_ch).poll().is_some() {}

		pair.mtu = 1200;
		if migrate {
			let port = pair.next_port();
			pair.client.addr.set_port(port);
			pair.client_conn_mut(client_ch).ping();
			pair.drive_client();
			// Process migration without transmitting, so sends cannot free the buffer first.
			let mut buf = Vec::new();
			while let Some((received, ecn, packet)) = pair.server.inbound.pop_front() {
				let Some(DatagramEvent::ConnectionEvent(ch, event)) = pair.server.handle(
					received,
					Received {
						remote: pair.client.addr,
						local_ip: None,
						ecn,
						data: packet,
					},
					&mut buf,
				) else {
					panic!("expected a connection event");
				};
				assert_eq!(ch, server_ch);
				pair.server_conn_mut(ch).handle_event(event);
			}
			assert_eq!(pair.server_conn_mut(server_ch).remote_address(), pair.client.addr);
		} else {
			let now = pair.time;
			pair.server_conn_mut(server_ch).path_changed(now);
		}

		assert!(pair.server_datagrams(server_ch).max_size().unwrap() < old_max);
		assert_eq!(pair.server_datagrams(server_ch).send_buffer_space(), empty_space);

		let small = Bytes::from_static(b"small");
		pair.server_datagrams(server_ch)
			.send(small.clone(), Default::default())
			.unwrap();
		pair.drive();
		assert_eq!(pair.client_datagrams(client_ch).recv(), Some(small));
		assert_eq!(pair.client_datagrams(client_ch).recv(), None);
	}
}

/// Verify that datagrams that no longer fit the path MTU are dropped from the send buffer.
#[test]
fn oversized_datagrams_are_dropped() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	// Start the connection with a large MTU.
	const INITIAL_MTU: usize = 1300;
	pair.mtu = INITIAL_MTU;

	let mut client_config = client_config();
	let mut transport_config = transport::Config::default();
	let send_buffer_size = transport_config.datagram.send_buffer;
	transport_config.initial_mtu = INITIAL_MTU as u16;
	client_config.transport = transport_config;

	let (client_ch, _) = pair.connect_with(client_config);

	// Send datagrams until the send buffer is full.
	let max_size = pair.client_datagrams(client_ch).max_size().unwrap();
	let data = vec![0; max_size];
	while pair.client_datagrams(client_ch).send_buffer_space() >= max_size {
		pair.client_datagrams(client_ch)
			.send(Bytes::from(data.clone()), Default::default())
			.unwrap();
	}
	// Set the MTU to a smaller value so the queued datagrams cannot be sent.
	pair.mtu = 1200;

	// Drive the pair until black hole detection kicks in and the path MTU is adjusted.
	while pair.step() {
		// Keep the buffer full until the MTU is adjusted and the datagram no longer fits.
		let mut sent = Ok(());
		while sent.is_ok() && pair.client_datagrams(client_ch).send_buffer_space() >= max_size {
			sent = pair
				.client_datagrams(client_ch)
				.send(Bytes::from(data.clone()), Default::default());
		}
		match sent {
			Ok(()) => {}
			Err(send::Error::TooLarge) => break,
			Err(err) => panic!("unexpected error: {err}"),
		}
	}

	assert_eq!(
		pair.client_conn_mut(client_ch).stats().path.black_holes_detected,
		1,
		"expected a black hole to have been detected",
	);

	assert_eq!(
		pair.client_datagrams(client_ch).send_buffer_space(),
		send_buffer_size - Datagram::SIZE_BOUND,
		"expected the send buffer to be empty after too large datagrams were dropped",
	);
}

#[test]
fn reject_short_idcid() {
	let _guard = subscribe();
	let client_addr = "[::2]:7890".parse().unwrap();
	let mut server = Endpoint::new(
		endpoint::Config {
			server: Some(server_config()),
			..endpoint::Config::default()
		},
		true,
	)
	.unwrap();
	let now = Instant::now();
	let mut buf = Vec::with_capacity(server.max_udp_payload_size() as usize);
	// Initial header that has an empty DCID but is otherwise well-formed
	let mut initial = BytesMut::from(hex!("c4 00000001 00 00 00 3f").as_ref());
	initial.resize(MIN_INITIAL_SIZE.into(), 0);
	let event = server.handle(
		now,
		Received {
			remote: client_addr,
			local_ip: None,
			ecn: None,
			data: initial,
		},
		&mut buf,
	);
	let Some(DatagramEvent::Response(Transmit { .. })) = event else {
		panic!("expected an initial close");
	};
}

/// Ensure that a connection can be made when a preferred address is advertised by the server,
/// regardless of whether the address is actually used.
#[test]
fn preferred_address() {
	let _guard = subscribe();
	let mut server_config = server_config();
	server_config.preferred_address_v6 = Some("[::1]:65535".parse().unwrap());

	let mut pair = Pair::new(endpoint::Config::default(), server_config);
	pair.connect();
}

#[test]
fn handshake_sequence() {
	let _guard = subscribe();

	// Cubic, whose window-derived pacer sends HANDSHAKE_DONE in the same step at zero RTT
	let mut server_config = server_config();
	server_config.transport = cubic_transport();
	let mut pair = Pair::new(Default::default(), server_config);
	let ch = pair.begin_connect(client_config());

	pair.step();
	assert_matches!(pair.client_conn_mut(ch).poll(), None);
	let sh = pair.server.assert_accept();
	assert_matches!(pair.server_conn_mut(sh).poll(), Some(Event::HandshakeDataReady));
	assert_matches!(pair.server_conn_mut(sh).poll(), None);

	pair.step();
	assert_matches!(pair.client_conn_mut(ch).poll(), Some(Event::HandshakeDataReady));
	assert_matches!(pair.client_conn_mut(ch).poll(), Some(Event::Connected));
	assert_matches!(pair.client_conn_mut(ch).poll(), None);
	assert_matches!(pair.server_conn_mut(sh).poll(), Some(Event::HandshakeConfirmed));
	assert_matches!(pair.server_conn_mut(sh).poll(), Some(Event::Connected));
	assert_matches!(pair.server_conn_mut(sh).poll(), None);

	pair.drive_client();
	assert_matches!(pair.client_conn_mut(ch).poll(), Some(Event::HandshakeConfirmed));
	assert_matches!(pair.client_conn_mut(ch).poll(), None);
}

#[test]
fn handshake_confirmation_no_resumption_shortcut() {
	let _guard = subscribe();

	// Initial connection
	// Cubic, whose window-derived pacer sends HANDSHAKE_DONE in the same step at zero RTT
	let mut server_config = server_config();
	server_config.transport = cubic_transport();
	let mut pair = Pair::new(Default::default(), server_config);
	let config = client_config();
	let (ch, _) = pair.connect_with(config.clone());
	pair.client
		.connections
		.get_mut(&ch)
		.unwrap()
		.close(pair.time, VarInt(0), String::new());
	pair.drive();

	// Resumed connection
	info!("resuming session");
	let ch = pair.begin_connect(config);

	pair.step();
	assert_matches!(pair.client_conn_mut(ch).poll(), None);
	let sh = pair.server.assert_accept();
	assert_matches!(pair.server_conn_mut(sh).poll(), Some(Event::HandshakeDataReady));
	assert_matches!(pair.server_conn_mut(sh).poll(), None);

	pair.step();
	assert_matches!(pair.client_conn_mut(ch).poll(), Some(Event::HandshakeDataReady));
	assert_matches!(pair.client_conn_mut(ch).poll(), Some(Event::Connected));
	assert_matches!(pair.client_conn_mut(ch).poll(), None);
	assert_matches!(pair.server_conn_mut(sh).poll(), Some(Event::HandshakeConfirmed));
	assert_matches!(pair.server_conn_mut(sh).poll(), Some(Event::Connected));
	assert_matches!(pair.server_conn_mut(sh).poll(), None);

	pair.drive_client();
	assert_matches!(pair.client_conn_mut(ch).poll(), Some(Event::HandshakeConfirmed));
	assert_matches!(pair.client_conn_mut(ch).poll(), None);
}

/// A CONNECTION_CLOSE frame of type 0x1d must be rejected in an Initial packet
///
/// RFC 9000 §12.4 Table 3 lists CONNECTION_CLOSE with the packet-type marker `ih01`, defined as
/// "Only a CONNECTION_CLOSE frame of type 0x1c can appear in Initial or Handshake packets", and
/// §12.4 requires that "An endpoint MUST treat receipt of a frame in a packet type that is not
/// permitted as a connection error of type PROTOCOL_VIOLATION". §12.5 repeats the rule:
/// "CONNECTION_CLOSE frames signaling application errors (type 0x1d) MUST only appear in the
/// application data packet number space."
#[test]
fn application_close_in_initial_is_rejected() {
	let _guard = subscribe();
	let server_addr = SocketAddr::new(Ipv6Addr::LOCALHOST.into(), 4433);
	let mut client = Endpoint::new(endpoint::Config::default(), true).unwrap();
	let now = Instant::now();
	let (_, mut conn) = client
		.connect(
			now,
			Dial {
				config: &client_config(),
				remote: server_addr,
				server_name: "localhost",
			},
		)
		.unwrap();

	// Grab the client's Initial packet so we can learn the connection IDs and version it chose.
	let mut buf = Vec::new();
	let transmit = conn
		.poll_transmit(now, 1, &mut buf)
		.expect("client should send an Initial packet");
	let initial = &buf[..transmit.size];
	// Long header: flags(1) version(4) dcid_len(1) dcid scid_len(1) scid ...
	let version = u32::from_be_bytes(initial[1..5].try_into().unwrap());
	let dcid_len = initial[5] as usize;
	let orig_dst_cid = ConnectionId::new(&initial[6..6 + dcid_len]);
	let scid_len = initial[6 + dcid_len] as usize;
	let client_cid = ConnectionId::new(&initial[7 + dcid_len..7 + dcid_len + scid_len]);

	// Forge a server Initial packet whose payload is a single APPLICATION_CLOSE (0x1d) frame.
	// Initial packets are protected with keys derived from the client's original destination
	// connection ID, which travels in the clear, so anyone who observes the handshake can do this.
	let keys = server_config()
		.crypto
		.initial_keys(Version::from_wire(version).unwrap(), orig_dst_cid);
	let number = PacketNumber::U8(0);
	let header = Header::Initial(InitialHeader {
		dst_cid: client_cid,
		src_cid: ConnectionId::new(&[]),
		token: Bytes::new(),
		number,
		version,
	});
	let mut packet = Vec::new();
	let partial = header.encode(&mut packet);
	let header_len = packet.len();
	// APPLICATION_CLOSE: type 0x1d, Error Code (varint) = 42, Reason Phrase Length (varint) = 0
	packet.extend_from_slice(&[0x1d, 0x2a, 0x00]);
	// PADDING, so that the packet is long enough for header protection sampling
	packet.resize(header_len + 16, 0);
	// Room for the AEAD tag
	packet.resize(packet.len() + keys.packet.local.tag_len(), 0);
	partial.finish(
		&mut packet,
		keys.header.local.as_ref(),
		Some((0, keys.packet.local.as_ref())),
	);

	let event = client.handle(
		now,
		Received {
			remote: server_addr,
			local_ip: None,
			ecn: None,
			data: BytesMut::from(&packet[..]),
		},
		&mut buf,
	);
	let Some(DatagramEvent::ConnectionEvent(_, event)) = event else {
		panic!("forged Initial packet was not routed to the connection");
	};
	conn.handle_event(event);

	assert_matches!(
		conn.poll(),
		Some(Event::ConnectionLost {
			reason: ConnectionError::TransportError(TransportError {
				code: TransportErrorCode::PROTOCOL_VIOLATION,
				..
			})
		})
	);
}

#[cfg(feature = "qlog")]
#[test]
fn qlog_packet_lost_trigger() {
	use qlog::events::{EventData, quic::PacketLostTrigger};
	use qlog::reader::{Event as QlogEvent, QlogSeqReader};

	let _guard = subscribe();
	let qlog = SharedBuffer::default();
	let qlog_config = transport::qlog::Config {
		writer: Some(Box::new(qlog.clone())),
		..transport::qlog::Config::default()
	};
	let transport = transport::Config {
		deterministic_packet_numbers: true,
		qlog: qlog_config.into_stream(),
		..transport::Config::default()
	};
	let mut config = client_config();
	config.transport = transport;
	let mut pair = Pair::default();
	let (client_ch, _) = pair.connect_with(config);
	pair.drive();

	// Drop a packet, then deliver fewer later packets than the packet threshold, so that only the
	// time threshold can declare it lost
	pair.client_conn_mut(client_ch).ping();
	pair.client.drive(pair.time, pair.server.addr);
	assert_eq!(pair.client.outbound.len(), 1);
	pair.client.outbound.clear();
	pair.client_conn_mut(client_ch).ping();
	pair.drive();

	// Drop a packet, then deliver as many later packets as the packet threshold without advancing
	// time, so that only the packet threshold can declare it lost
	pair.client_conn_mut(client_ch).ping();
	pair.client.drive(pair.time, pair.server.addr);
	assert_eq!(pair.client.outbound.len(), 1);
	pair.client.outbound.clear();
	for _ in 0..3 {
		pair.client_conn_mut(client_ch).ping();
		pair.client.drive(pair.time, pair.server.addr);
	}
	assert_eq!(pair.client.outbound.len(), 3);
	pair.drive();

	assert_eq!(pair.client_conn_mut(client_ch).stats().path.lost_packets, 2);
	let triggers = QlogSeqReader::new(Box::new(&qlog.0.lock().unwrap()[..]))
		.unwrap()
		.filter_map(|event| match event {
			QlogEvent::Qlog(event) => match event.data {
				EventData::QuicPacketLost(lost) => lost.trigger,
				_ => None,
			},
			QlogEvent::Json(_) => None,
		})
		.collect::<Vec<_>>();
	assert_eq!(
		triggers,
		[PacketLostTrigger::TimeThreshold, PacketLostTrigger::ReorderingThreshold]
	);
}

#[cfg(feature = "qlog")]
#[test]
fn qlog_pacing_rate_is_bits_per_second() {
	use qlog::events::EventData;
	use qlog::reader::{Event as QlogEvent, QlogSeqReader};

	const BYTES_PER_SECOND: u64 = 125_000;
	let qlog = SharedBuffer::default();
	let qlog_config = transport::qlog::Config {
		writer: Some(Box::new(qlog.clone())),
		..transport::qlog::Config::default()
	};
	let mut config = client_config();
	config.transport.congestion = Arc::new(PacketRecorderFactory {
		pacing_rate: Some(BYTES_PER_SECOND),
		..Default::default()
	});
	config.transport.qlog = qlog_config.into_stream();
	let mut pair = Pair::default();
	pair.connect_with(config);

	let rates = QlogSeqReader::new(Box::new(&qlog.0.lock().unwrap()[..]))
		.unwrap()
		.filter_map(|event| match event {
			QlogEvent::Qlog(event) => match event.data {
				EventData::QuicMetricsUpdated(metrics) => metrics.pacing_rate,
				_ => None,
			},
			QlogEvent::Json(_) => None,
		})
		.collect::<Vec<_>>();
	assert!(!rates.is_empty(), "no qlog pacing rate emitted");
	assert!(rates.iter().all(|rate| *rate == BYTES_PER_SECOND * 8));
}

/// Releases packets at `rate` bytes per second, one at a time, for test controllers.
#[derive(Debug, Clone, Default)]
struct TestPacer {
	/// When the next packet may leave.
	next: Option<Instant>,
}

impl TestPacer {
	fn on_sent(&mut self, now: Instant, bytes: u16, rate: u64) {
		let start = self.next.map_or(now, |next| next.max(now));
		self.next = Some(start + Duration::from_secs_f64(f64::from(bytes) / rate as f64));
	}

	fn pace(&self, now: Instant) -> Option<Instant> {
		self.next.filter(|&next| next > now)
	}
}

/// A controller whose window is effectively unbounded but which always paces at a low rate, so
/// the only thing that can ever block a send is the pacer. Counts how many times the connection
/// reports the spec's `C.is_cwnd_limited` signal.
#[derive(Debug)]
struct PacingOnlyController {
	cwnd_limited_reports: Arc<AtomicU64>,
	pacer: TestPacer,
}

impl PacingOnlyController {
	/// 1 Mbit/s in bytes/sec: low enough that a bulk transfer is pacing-blocked almost
	/// continuously.
	const RATE: u64 = 125_000;
}

impl Controller for PacingOnlyController {
	fn on_sent(&mut self, sent: congestion::Sent) -> congestion::Packet {
		self.pacer.on_sent(sent.now, sent.bytes, Self::RATE);
		Default::default()
	}

	fn on_cwnd_limited(&mut self) {
		self.cwnd_limited_reports.fetch_add(1, Ordering::Relaxed);
	}

	fn on_ack(&mut self, _ack: congestion::Ack<'_>) {}

	fn on_loss(&mut self, _loss: congestion::Loss<'_>) {}

	fn on_ce(&mut self, _ce: congestion::Ce) {}

	fn on_mtu_update(&mut self, _new_mtu: u16) {}

	fn window(&self) -> u64 {
		u64::MAX / 2
	}

	fn pace(&mut self, now: Instant, _bytes: u64) -> Option<Instant> {
		self.pacer.pace(now)
	}

	fn metrics(&self) -> ControllerMetrics {
		ControllerMetrics {
			congestion_window: self.window(),
			pacing_rate: Some(Self::RATE),
			..Default::default()
		}
	}

	fn clone_box(&self) -> Box<dyn Controller> {
		Box::new(Self {
			cwnd_limited_reports: self.cwnd_limited_reports.clone(),
			pacer: self.pacer.clone(),
		})
	}

	fn into_any(self: Box<Self>) -> Box<dyn Any> {
		self
	}
}

struct PacingOnlyConfig {
	cwnd_limited_reports: Arc<AtomicU64>,
}

impl ControllerFactory for PacingOnlyConfig {
	fn build(self: Arc<Self>, _init: congestion::Init) -> Box<dyn Controller> {
		Box::new(PacingOnlyController {
			cwnd_limited_reports: self.cwnd_limited_reports.clone(),
			pacer: TestPacer::default(),
		})
	}
}

/// `C.is_cwnd_limited` means the sender filled the congestion window. A flow held back by
/// pacing is not cwnd-limited and a paced controller such as BBR3 is pacing-limited by
/// design, so conflating the two pins the signal true and breaks the decisions built on it.
#[test]
fn cwnd_limited_is_not_reported_when_only_pacing_blocks() {
	let _guard = subscribe();
	let reports = Arc::new(AtomicU64::new(0));

	let mut client_cfg = client_config();
	client_cfg.transport.congestion = Arc::new(PacingOnlyConfig {
		cwnd_limited_reports: reports.clone(),
	});

	let mut pair = Pair::default();
	let (client_ch, _) = pair.connect_with(client_cfg);

	let s = pair
		.client_streams(client_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();
	pair.client_send(client_ch, s).write(&mut &[42; 64 * 1024][..]).unwrap();
	pair.drive();

	assert_eq!(
		reports.load(Ordering::Relaxed),
		0,
		"connection reported cwnd-limited while only pacing was holding sends back"
	);
}

/// A controller that never limits the window or the rate, but reports a small burst, so the
/// burst is the only thing that can bound an aggregate handed to the NIC.
#[derive(Debug, Clone)]
struct FixedQuantumController {
	send_quantum: u64,
}

impl Controller for FixedQuantumController {
	fn on_sent(&mut self, _sent: congestion::Sent) -> congestion::Packet {
		Default::default()
	}

	fn on_ack(&mut self, _ack: congestion::Ack<'_>) {}

	fn on_loss(&mut self, _loss: congestion::Loss<'_>) {}

	fn on_ce(&mut self, _ce: congestion::Ce) {}

	fn on_mtu_update(&mut self, _new_mtu: u16) {}

	fn window(&self) -> u64 {
		u64::MAX / 2
	}

	fn pace(&mut self, _now: Instant, _bytes: u64) -> Option<Instant> {
		None
	}

	fn burst(&self) -> Option<u64> {
		Some(self.send_quantum)
	}

	fn clone_box(&self) -> Box<dyn Controller> {
		Box::new(self.clone())
	}

	fn into_any(self: Box<Self>) -> Box<dyn Any> {
		self
	}
}

impl ControllerFactory for FixedQuantumController {
	fn build(self: Arc<Self>, _init: congestion::Init) -> Box<dyn Controller> {
		Box::new((*self).clone())
	}
}

/// `C.send_quantum` bounds the size of one aggregate scheduled and transmitted together, which
/// for a GSO batch means the number of datagrams written in a single call.
#[test]
fn send_quantum_bounds_the_gso_batch() {
	/// Datagrams the reported quantum should permit per batch.
	const QUANTUM_DATAGRAMS: usize = 3;
	/// Datagrams the caller is willing to accept, well above the quantum.
	const MAX_DATAGRAMS: usize = 10;

	let _guard = subscribe();

	let mut client_cfg = client_config();
	client_cfg.transport.congestion = Arc::new(FixedQuantumController {
		send_quantum: QUANTUM_DATAGRAMS as u64 * DEFAULT_MTU as u64,
	});

	let mut pair = Pair::default();
	let (client_ch, _) = pair.connect_with(client_cfg);

	let s = pair
		.client_streams(client_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();
	pair.client_send(client_ch, s).write(&mut &[42; 64 * 1024][..]).unwrap();

	let now = pair.time;
	let mut buf = Vec::new();
	let transmit = pair
		.client_conn_mut(client_ch)
		.poll_transmit(now, MAX_DATAGRAMS, &mut buf)
		.expect("a stream write must produce a transmit");

	let datagrams = match transmit.segment_size {
		Some(segment_size) => buf.len().div_ceil(segment_size),
		None => 1,
	};
	assert!(
		datagrams <= QUANTUM_DATAGRAMS,
		"batched {datagrams} datagrams, over the {QUANTUM_DATAGRAMS} the send quantum allows"
	);
}

/// A congestion callback, as the transport reported it. Packets are named by send time, which
/// several packets can share.
#[derive(Debug, Clone, Copy)]
enum PacketEvent {
	Sent { at: Instant },
	Acked { sent: Instant },
	Lost { sent: Instant },
	Congestion { sent: Instant, ecn: bool },
	AppLimited { in_flight: u64 },
}

type PacketLog = Arc<Mutex<Vec<PacketEvent>>>;

/// Records the per-packet and starvation callbacks.
#[derive(Clone)]
struct PacketRecorder {
	log: PacketLog,
	window: u64,
	pacing_rate: Option<u64>,
	pacer: TestPacer,
}

impl PacketRecorder {
	fn push(&self, event: PacketEvent) {
		self.log.lock().unwrap().push(event);
	}
}

impl Controller for PacketRecorder {
	fn on_sent(&mut self, sent: congestion::Sent) -> congestion::Packet {
		if let Some(rate) = self.pacing_rate {
			self.pacer.on_sent(sent.now, sent.bytes, rate);
		}
		self.push(PacketEvent::Sent { at: sent.now });
		Default::default()
	}

	fn on_ack(&mut self, ack: congestion::Ack<'_>) {
		for acked in ack.packets {
			self.push(PacketEvent::Acked { sent: acked.sent });
		}
	}

	fn on_loss(&mut self, loss: congestion::Loss<'_>) {
		for lost in loss.packets {
			self.push(PacketEvent::Lost { sent: lost.sent });
		}
		let newest = loss.packets.last().expect("a loss names its packets");
		self.push(PacketEvent::Congestion {
			sent: newest.sent,
			ecn: false,
		});
	}

	fn on_ce(&mut self, ce: congestion::Ce) {
		self.push(PacketEvent::Congestion {
			sent: ce.sent,
			ecn: true,
		});
	}

	fn on_app_limited(&mut self, in_flight: u64) {
		self.push(PacketEvent::AppLimited { in_flight });
	}

	fn on_mtu_update(&mut self, _new_mtu: u16) {}

	fn window(&self) -> u64 {
		self.window
	}

	fn pace(&mut self, now: Instant, _bytes: u64) -> Option<Instant> {
		self.pacer.pace(now)
	}

	fn metrics(&self) -> ControllerMetrics {
		ControllerMetrics {
			congestion_window: self.window,
			pacing_rate: self.pacing_rate,
			..Default::default()
		}
	}

	fn clone_box(&self) -> Box<dyn Controller> {
		Box::new(self.clone())
	}

	fn into_any(self: Box<Self>) -> Box<dyn Any> {
		self
	}
}

/// Gives every controller it builds, one per path per side, a log of its own.
#[derive(Default)]
struct PacketRecorderFactory {
	logs: Mutex<Vec<PacketLog>>,
	/// The congestion window every controller reports; effectively unlimited if unset
	window: Option<u64>,
	/// The rate in bytes/sec every controller paces at; unpaced if unset
	pacing_rate: Option<u64>,
}

impl ControllerFactory for PacketRecorderFactory {
	fn build(self: Arc<Self>, _init: congestion::Init) -> Box<dyn Controller> {
		let log = PacketLog::default();
		self.logs.lock().unwrap().push(log.clone());
		Box::new(PacketRecorder {
			log,
			window: self.window.unwrap_or(u64::MAX / 2),
			pacing_rate: self.pacing_rate,
			pacer: TestPacer::default(),
		})
	}
}

/// Asserts no packet is resolved twice: every ACK and loss in one controller's log names the send
/// time of a packet sent and not yet resolved, a loss-driven congestion event names the newest
/// packet just reported lost, and a CE event names a packet sent. Returns the log for
/// scenario-specific checks.
fn check_packet_resolution(log: &PacketLog) -> Vec<PacketEvent> {
	let events = log.lock().unwrap().clone();
	let mut outstanding = BTreeMap::<Instant, usize>::new();
	let mut sent = BTreeSet::new();
	let mut last_lost = None;
	let resolve = |outstanding: &mut BTreeMap<Instant, usize>, at: Instant| {
		let count = outstanding
			.get_mut(&at)
			.unwrap_or_else(|| panic!("packet sent at {at:?} resolved but never outstanding"));
		*count -= 1;
		if *count == 0 {
			outstanding.remove(&at);
		}
	};
	for event in &events {
		match *event {
			PacketEvent::Sent { at } => {
				*outstanding.entry(at).or_default() += 1;
				sent.insert(at);
			}
			PacketEvent::Acked { sent: at } => resolve(&mut outstanding, at),
			PacketEvent::Lost { sent: at } => {
				resolve(&mut outstanding, at);
				last_lost = Some(at);
			}
			PacketEvent::Congestion { sent: at, ecn } => {
				assert!(sent.contains(&at), "congestion names a packet never sent");
				if !ecn {
					assert_eq!(Some(at), last_lost, "loss congestion names another packet");
				}
			}
			PacketEvent::AppLimited { .. } => {}
		}
	}
	events
}

/// The handshake loses a server flight and sees ECN marks, and every packet the transport
/// resolves through the controller is one it sent and had not resolved yet.
#[test]
fn congestion_callbacks_resolve_each_packet_once() {
	let _guard = subscribe();
	let factory = Arc::new(PacketRecorderFactory::default());
	let transport = transport::Config {
		deterministic_packet_numbers: true,
		congestion: factory.clone(),
		..transport::Config::default()
	};

	let mut server_cfg = server_config();
	server_cfg.transport = transport.clone();
	let mut pair = Pair::new(Default::default(), server_cfg);
	let mut client_cfg = client_config();
	client_cfg.transport = transport;

	let client_ch = pair.begin_connect(client_cfg);
	// Mark the ClientHello and the server's first flight, then drop that flight so the server
	// retransmits and declares the originals lost.
	pair.congestion_experienced = true;
	pair.drive_client();
	pair.drive_server();
	pair.congestion_experienced = false;
	pair.client.inbound.clear();
	pair.drive();
	assert!(!pair.client_conn_mut(client_ch).is_handshaking());

	let s = pair
		.client_streams(client_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();
	pair.client_send(client_ch, s).write(&mut &[42; 16 * 1024][..]).unwrap();
	pair.drive();

	let logs = factory.logs.lock().unwrap().clone();
	assert_eq!(logs.len(), 2, "one controller per side");
	let events: Vec<_> = logs.iter().flat_map(check_packet_resolution).collect();
	assert!(
		events.iter().any(|e| matches!(e, PacketEvent::Lost { .. })),
		"no handshake loss reported"
	);
	assert!(
		events
			.iter()
			.any(|e| matches!(e, PacketEvent::Congestion { ecn: true, .. })),
		"no handshake ECN congestion reported"
	);
}

/// Connects a pair whose controllers record their callbacks, returning the client's log.
fn recorded_pair(factory: PacketRecorderFactory) -> (Pair, ConnectionHandle, PacketLog) {
	let factory = Arc::new(factory);
	let transport = transport::Config {
		congestion: factory.clone(),
		..transport::Config::default()
	};

	let mut server_cfg = server_config();
	server_cfg.transport = transport.clone();
	let mut pair = Pair::new(Default::default(), server_cfg);
	let mut client_cfg = client_config();
	client_cfg.transport = transport;
	let (client_ch, _) = pair.connect_with(client_cfg);

	// The client builds its controller before the server hears of it.
	let log = factory.logs.lock().unwrap()[0].clone();
	(pair, client_ch, log)
}

/// Counts the congestion events one controller's log reports for CE marks.
fn ce_events(log: &PacketLog) -> usize {
	log.lock()
		.unwrap()
		.iter()
		.filter(|e| matches!(e, PacketEvent::Congestion { ecn: true, .. }))
		.count()
}

/// ACKs keep carrying the CE count after the marks stop, but only an increase is a congestion
/// event, so a controller never answers the same marks twice.
#[test]
fn old_ce_marks_report_no_new_congestion() {
	let _guard = subscribe();
	let (mut pair, client_ch, log) = recorded_pair(Default::default());
	let s = pair
		.client_streams(client_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();
	pair.client_send(client_ch, s).write(&mut &[42; 8 * 1024][..]).unwrap();
	pair.congestion_experienced = true;
	pair.drive_client();
	pair.congestion_experienced = false;
	pair.drive();
	let marked = ce_events(&log);
	assert!(marked > 0);

	pair.client_send(client_ch, s).write(&mut &[42; 64 * 1024][..]).unwrap();
	pair.drive();
	assert_eq!(ce_events(&log), marked);
	assert!(pair.client_conn_mut(client_ch).using_ecn());
}

/// A path that starts bleaching the ECN field or re-marking it ECT(1) reports no CE, though the
/// bottleneck marks every packet, and re-marking fails validation, so the sender stops using
/// ECN. Bleaching is only caught once an ACK's count increase falls short of its ACK ranges,
/// which may not happen within this transfer.
#[test]
fn invalid_ecn_feedback_reports_no_congestion() {
	let _guard = subscribe();
	for rewrite in [None, Some(EcnCodepoint::Ect1)] {
		let (mut pair, client_ch, log) = recorded_pair(Default::default());
		assert!(pair.client_conn_mut(client_ch).using_ecn());
		pair.congestion_experienced = true;
		pair.rewrite_ecn = Some(rewrite);
		let s = pair
			.client_streams(client_ch)
			.open(Dir::Uni, Default::default())
			.unwrap();
		pair.client_send(client_ch, s).write(&mut &[42; 16 * 1024][..]).unwrap();
		pair.drive();
		assert_eq!(ce_events(&log), 0, "{rewrite:?}");
		if rewrite.is_some() {
			assert!(!pair.client_conn_mut(client_ch).using_ecn());
		}
	}
}

/// A receiver that pads its ACK-only packets counts them in flight. ACKs and losses of them,
/// before and after the peer migrates to a path with a fresh controller, resolve each through
/// its own controller once and leave nothing in flight.
#[test]
fn padded_ack_only_packets_resolve_exactly() {
	let _guard = subscribe();
	let factory = Arc::new(PacketRecorderFactory::default());
	let transport = transport::Config {
		congestion: factory.clone(),
		pad_to_mtu: true,
		..transport::Config::default()
	};
	let mut server_cfg = server_config();
	server_cfg.transport = transport.clone();
	let mut pair = Pair::new(Default::default(), server_cfg);
	let mut client_cfg = client_config();
	client_cfg.transport = transport;
	let (client_ch, server_ch) = pair.connect_with(client_cfg);

	// The server mostly answers with padded ACK-only packets, some lost. The client acknowledges
	// them only alongside the ack-eliciting packet each round ends with.
	let s = pair
		.client_streams(client_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();
	let round = |pair: &mut Pair, loss| {
		pair.to_client.loss = loss;
		for _ in 0..8 {
			pair.client_send(client_ch, s).write(&mut &[42; 8 * 1024][..]).unwrap();
			pair.drive();
			pair.server_conn_mut(server_ch).ping();
			pair.drive();
		}
		pair.server_conn_mut(server_ch).stats().path.lost_packets
	};
	let lost = round(&mut pair, 0.2);
	assert!(lost > 0, "no ACK-only packet lost");

	// A new address that is not a NAT rebinding gets a new controller.
	pair.client.addr = SocketAddr::new(Ipv4Addr::new(127, 0, 0, 2).into(), pair.next_port());
	assert!(round(&mut pair, 0.2) > lost, "no ACK-only packet lost after migrating");
	assert_eq!(pair.server_conn_mut(server_ch).remote_address(), pair.client.addr);
	round(&mut pair, 0.0);
	assert_eq!(pair.server_conn_mut(server_ch).bytes_in_flight(), 0);
	let logs = factory.logs.lock().unwrap().clone();
	assert_eq!(logs.len(), 3, "a controller per side, and one for the new path");
	for log in &logs {
		check_packet_resolution(log);
	}
}

/// An ACK whose largest packet is no longer tracked yields no RTT sample, even when it newly
/// acknowledges an ack-eliciting packet, rather than one timed from an older packet's send.
#[test]
fn untracked_largest_acked_skips_rtt_sample() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	let (client_ch, _) = pair.connect();
	pair.drive();
	let rtt = pair.client_conn_mut(client_ch).rtt();

	// Long after the last sample, so timing from that packet's send would stand out.
	pair.time += Duration::from_secs(1);
	pair.client_conn_mut(client_ch).ping();
	pair.drive_client();
	pair.client_conn_mut(client_ch).ping();
	pair.drive_client();
	pair.client_conn_mut(client_ch).forget_newest_sent_packet();
	pair.drive();
	assert_eq!(pair.client_conn_mut(client_ch).rtt(), rtt);
}

/// Sends with `send`, lets it deliver, then sends again after an idle gap. With nothing in
/// flight no ACK arrives during the gap, so the empty poll that follows the last ACK must tell
/// the controller of starvation before the resumed send.
fn check_starvation_reported_before_resumed_send(send: impl Fn(&mut Pair, ConnectionHandle)) {
	let _guard = subscribe();
	let (mut pair, client_ch, log) = recorded_pair(Default::default());
	send(&mut pair, client_ch);
	pair.drive();

	pair.time += Duration::from_millis(20);
	let resumed_at = pair.time;
	send(&mut pair, client_ch);
	pair.drive_client();

	let events = check_packet_resolution(&log);
	let resumed = events
		.iter()
		.position(|e| matches!(e, PacketEvent::Sent { at, .. } if *at >= resumed_at))
		.expect("resumed send");
	let last_signal = events[..resumed]
		.iter()
		.rfind(|e| matches!(e, PacketEvent::Acked { .. } | PacketEvent::AppLimited { .. }));
	assert_matches!(last_signal, Some(PacketEvent::AppLimited { in_flight: 0 }));
}

#[test]
fn stream_starvation_reported_before_resumed_send() {
	check_starvation_reported_before_resumed_send(|pair, ch| {
		let s = pair.client_streams(ch).open(Dir::Uni, Default::default()).unwrap();
		pair.client_send(ch, s).write(&mut &[42; 1000][..]).unwrap();
	});
}

#[test]
fn datagram_starvation_reported_before_resumed_send() {
	check_starvation_reported_before_resumed_send(|pair, ch| {
		pair.client_datagrams(ch)
			.send(Bytes::from_static(&[42; 1000]), Default::default())
			.unwrap();
	});
}

/// Writes a backlog that `factory`'s controllers hold back, and checks it never reports
/// starvation.
fn check_blocked_backlog_is_not_app_limited(factory: PacketRecorderFactory) {
	let _guard = subscribe();
	let (mut pair, client_ch, log) = recorded_pair(factory);
	// The handshake can drain the pacer, and how far depends on its random sizes. Refill it so
	// the backlog always starts sending before it is held back.
	pair.time += Duration::from_millis(100);
	pair.drive();
	let before = log.lock().unwrap().len();

	let s = pair
		.client_streams(client_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();
	pair.client_send(client_ch, s).write(&mut &[42; 64 * 1024][..]).unwrap();
	pair.drive_client();

	let events = log.lock().unwrap()[before..].to_vec();
	assert!(events.iter().any(|e| matches!(e, PacketEvent::Sent { .. })));
	assert!(
		!events.iter().any(|e| matches!(e, PacketEvent::AppLimited { .. })),
		"a blocked backlog reported starvation"
	);
}

#[test]
fn window_blocked_backlog_is_not_app_limited() {
	check_blocked_backlog_is_not_app_limited(PacketRecorderFactory {
		window: Some(12_000),
		..Default::default()
	});
}

#[test]
fn pacing_blocked_backlog_is_not_app_limited() {
	// 1 Mbit/s holds the backlog back long before the unlimited window fills.
	check_blocked_backlog_is_not_app_limited(PacketRecorderFactory {
		pacing_rate: Some(125_000),
		..Default::default()
	});
}

/// The last packet in a poll can drain the application while bytes remain in flight.
#[test]
fn partial_poll_reports_starvation() {
	let _guard = subscribe();
	let (mut pair, ch, log) = recorded_pair(Default::default());
	let stream = pair.client_streams(ch).open(Dir::Uni, Default::default()).unwrap();
	pair.client_send(ch, stream).write(&mut &[42; 1000][..]).unwrap();
	log.lock().unwrap().clear();
	let now = pair.time;
	let mut buf = Vec::new();
	assert!(pair.client_conn_mut(ch).poll_transmit(now, 10, &mut buf).is_some());
	assert!(matches!(log.lock().unwrap().last(), Some(PacketEvent::AppLimited { in_flight }) if *in_flight > 0));
}

/// The bytes the server holds leased, none of them read yet.
fn leased(pair: &mut Pair, server_ch: ConnectionHandle) -> usize {
	pair.server_conn_mut(server_ch).stats().credit.leased as usize
}

/// Lease what has arrived on `s`, read `n` bytes of it, and release their credit, returning
/// whether the peer finished the stream right after the leased bytes.
fn consume(pair: &mut Pair, server_ch: ConnectionHandle, s: StreamId, n: usize) -> bool {
	let mut recv = pair.server_recv(server_ch, s);
	let mut lease = recv.lease(0).0.unwrap();
	let fin = lease.is_fin();
	lease.advance(n);
	let _ = recv.release(lease);
	fin
}

/// Write what the client can of `msg` up to `until`, finishing at its end, and lease what the
/// server can, without consuming, until neither makes progress. Returns the bytes newly leased.
fn pump_leased(
	pair: &mut Pair,
	(client_ch, server_ch): (ConnectionHandle, ConnectionHandle),
	s: StreamId,
	(msg, until): (&[u8], usize),
	sent: &mut usize,
) -> usize {
	let mut total = 0;
	loop {
		// Deliver whatever credit the server queued since the last round.
		pair.drive();
		let mut wrote = 0;
		if *sent < until
			&& let Ok(n) = pair.client_send(client_ch, s).write(&mut &msg[*sent..until])
		{
			wrote = n;
			*sent += n;
			if *sent == msg.len() {
				pair.client_send(client_ch, s).finish().unwrap();
			}
		}
		pair.drive();

		let before = leased(pair, server_ch);
		let mut recv = pair.server_recv(server_ch, s);
		if let Ok(lease) = recv.lease(0).0 {
			let _ = recv.restore(lease);
		}
		let read = leased(pair, server_ch) - before;
		if read == 0 && wrote == 0 {
			return total;
		}
		total += read;
	}
}

/// Leases keep replenishing a stream window much smaller than the connection's, while the
/// connection window stays spent until the bytes are consumed. A consume below the usual
/// `MAX_DATA` threshold still unblocks the peer once it nears its limit, without relying on its
/// DATA_BLOCKED.
///
/// The peer never spends the whole window: a lease held then would be evicted, since nothing
/// could free it.
#[test]
fn lease_spends_connection_window_only() {
	const WINDOW: usize = 4000;
	let _guard = subscribe();
	let mut transport = transport::Config::default();
	transport.limits.receive_window = NonZeroU64::new(WINDOW as u64);
	transport.limits.stream_receive_window = NonZeroU64::new(1000);
	let mut pair = Pair::new(
		Default::default(),
		server::Config {
			transport,
			..server_config()
		},
	);
	let chs = pair.connect();
	let (client_ch, server_ch) = chs;
	let s = pair
		.client_streams(client_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();
	let msg = vec![0xAB; WINDOW + 50];
	let mut sent = 0;

	// Three stream windows' worth, all leased and none consumed.
	assert_eq!(pump_leased(&mut pair, chs, s, (&msg, 3000), &mut sent), 3000);
	let stats = pair.server_conn_mut(server_ch).stats().credit;
	assert_eq!((stats.leased, stats.available), (3000, 1000));

	// Too little to announce while the peer has room, so the peer blocks at the window, and the
	// frame that spends the last of its room is what announces it.
	assert!(!consume(&mut pair, server_ch, s, 100));
	let all = msg.len();
	assert_eq!(pump_leased(&mut pair, chs, s, (&msg, all), &mut sent), 1050);
	assert_eq!(sent, msg.len());
	assert_eq!(leased(&mut pair, server_ch), WINDOW + 50 - 100);

	let all = leased(&mut pair, server_ch);
	assert!(consume(&mut pair, server_ch, s, all), "finished after the leased bytes");
	assert_eq!(leased(&mut pair, server_ch), 0);
	pair.server_recv(server_ch, s).stop(0u32.into()).unwrap();
	assert_eq!(
		pair.server_recv(server_ch, s).lease(0).0.err(),
		Some(recv::Error::Closed)
	);
}

#[test]
fn sender_window_is_not_starvation() {
	let _guard = subscribe();
	let (mut pair, ch, log) = recorded_pair(Default::default());
	pair.client_conn_mut(ch).set_send_window(1000);
	let stream = pair.client_streams(ch).open(Dir::Uni, Default::default()).unwrap();
	assert_eq!(pair.client_send(ch, stream).write(&mut &[42; 2000][..]), Ok(1000));
	log.lock().unwrap().clear();
	pair.drive_client();
	assert!(
		log.lock()
			.unwrap()
			.iter()
			.any(|e| matches!(e, PacketEvent::Sent { .. }))
	);
	assert!(
		!log.lock()
			.unwrap()
			.iter()
			.any(|e| matches!(e, PacketEvent::AppLimited { .. }))
	);
}

#[test]
fn receiver_credit_is_starvation() {
	let _guard = subscribe();
	let factory = Arc::new(PacketRecorderFactory::default());
	let mut server_transport = transport::Config::default();
	server_transport.limits.receive_window = NonZeroU64::new(1000);
	let mut server_cfg = server_config();
	server_cfg.transport = server_transport;
	let mut pair = Pair::new(Default::default(), server_cfg);
	let client_transport = transport::Config {
		congestion: factory.clone(),
		..transport::Config::default()
	};
	let mut client_cfg = client_config();
	client_cfg.transport = client_transport;
	let (ch, _) = pair.connect_with(client_cfg);
	let log = factory.logs.lock().unwrap()[0].clone();
	let stream = pair.client_streams(ch).open(Dir::Uni, Default::default()).unwrap();
	assert_eq!(pair.client_send(ch, stream).write(&mut &[42; 2000][..]), Ok(1000));
	log.lock().unwrap().clear();
	pair.drive_client();
	assert!(
		log.lock()
			.unwrap()
			.iter()
			.any(|e| matches!(e, PacketEvent::Sent { .. }))
	);
	assert!(
		log.lock()
			.unwrap()
			.iter()
			.any(|e| matches!(e, PacketEvent::AppLimited { .. }))
	);
}

/// A Retry acknowledges the client's first Initial packet without an ACK frame, and the
/// controller hears of it.
#[test]
fn congestion_callbacks_identify_the_initial_a_retry_acks() {
	let _guard = subscribe();
	let factory = Arc::new(PacketRecorderFactory::default());
	let transport = transport::Config {
		congestion: factory.clone(),
		..transport::Config::default()
	};
	let mut client_cfg = client_config();
	client_cfg.transport = transport;

	let mut pair = Pair::default();
	pair.server.handle_incoming = Box::new(validate_incoming);
	pair.connect_with(client_cfg);

	let logs = factory.logs.lock().unwrap().clone();
	assert_eq!(logs.len(), 1, "only the client records");
	let events = check_packet_resolution(&logs[0]);
	let Some(PacketEvent::Sent { at: first }) = events.first().copied() else {
		panic!("the client sent nothing first");
	};
	assert!(
		events
			.iter()
			.any(|e| matches!(e, PacketEvent::Acked { sent } if *sent == first)),
		"the Retry did not ack the first Initial"
	);
}

/// A 1 MB/s bottleneck with a 20ms RTT and a one-BDP buffer carries a BBR upload, once marking
/// CE at half full and once only tail-dropping when full. BBR answers the marks, so the marking
/// run holds a shorter queue at the dropping run's goodput, and drops at most the one packet its
/// first bandwidth probe can overshoot before that probe's marks come back.
#[test]
fn bbr_marking_versus_dropping() {
	const TOTAL_BYTES: usize = 4_000_000;
	const BPS_LIMIT: u64 = 1_000_000;
	// Replays the whole run: packet numbers, keys, and BBR's probe schedule. This seed's first
	// probe overshoots by one packet.
	const SEED: u64 = 66;

	let _guard = subscribe();
	// Both runs replay the same inputs, so they differ only in how the bottleneck signals.
	let run = |marks_ce| {
		entropy::reseed(SEED);
		let mut pair = Pair::new(Default::default(), server_config());
		pair.latency = Duration::from_millis(10);
		let (client_ch, server_ch) = pair.connect_with(client_config());
		pair.to_server.bottleneck = Some(Bottleneck::new(BPS_LIMIT, 20_000, marks_ce));
		let time = upload(&mut pair, client_ch, server_ch, TOTAL_BYTES, |_| {});
		let goodput = TOTAL_BYTES as f64 / time.as_secs_f64();
		let ecn = pair.client_conn_mut(client_ch).using_ecn();
		let queue = pair.to_server.bottleneck.take().unwrap().stats;
		info!(
			marks_ce,
			ecn,
			goodput,
			mean_delay = ?queue.mean_delay(),
			max_delay = ?queue.max_delay,
			dropped = queue.dropped,
			marked = queue.marked,
			congested = ?queue.congested,
		);
		assert!(ecn);
		(goodput, queue)
	};

	let (marking_goodput, marking) = run(true);
	let (dropping_goodput, dropping) = run(false);
	// Over seeds 0-6999: 3.6-5.3ms mean delay for marking and 6.9-8.4ms for dropping, 0.68-1.13s
	// and 1.46-1.80s past the marking threshold, and 24-38 drops when dropping. Without a CE
	// response this seed's marking run drops 35 packets, at 7.4ms and 1.59s.
	//
	// The marking run drops one packet on 1% of those seeds, never more. The first probe after
	// Startup grows inflight exponentially, and its first mark comes back 1.5 RTTs after the
	// queue crosses half full: a full RTT plus the marked packet's own queueing. BBR answers that
	// first marked ACK, but the queue can reach the buffer's edge just before it.
	assert!(marking.marked > 0, "seed {SEED}");
	assert!(marking.dropped <= 1, "seed {SEED}");
	assert!(dropping.dropped > 0, "seed {SEED}");
	assert!(marking.mean_delay() * 4 < dropping.mean_delay() * 3, "seed {SEED}");
	assert!(marking.congested * 4 < dropping.congested * 3, "seed {SEED}");
	assert!(marking_goodput > 0.9 * dropping_goodput, "seed {SEED}");
}

/// A BBR upload learns a 2 MB/s bottleneck at a 50ms RTT, then the connection idles on
/// keep-alives for five minutes. The next 250 KB must be paced near the bandwidth learned before
/// the idle, not at the rate of the keep-alives (#4219).
#[test]
fn bbr_burst_after_long_idle_keeps_learned_bandwidth() {
	const BPS_LIMIT: u64 = 2_000_000;
	const BURST: usize = 250_000;
	const SEED: u64 = 1;

	let _guard = subscribe();
	entropy::reseed(SEED);
	// Only the sender pings. A ping from the receiver would reset the sender's keep-alive timer,
	// leaving its controller nothing to sample during the idle.
	let mut server = server_config();
	server.transport.keep_alive = Some(Duration::ZERO);
	let mut pair = Pair::new(Default::default(), server);
	pair.latency = Duration::from_millis(25);
	let mut client_cfg = client_config();
	client_cfg.transport.keep_alive = Some(Duration::from_secs(10));
	let (client_ch, server_ch) = pair.connect_with(client_cfg);
	pair.to_server.bottleneck = Some(Bottleneck::new(BPS_LIMIT, 100_000, false));

	let metrics = |pair: &mut Pair| pair.client_conn_mut(client_ch).congestion_state().metrics();
	upload(&mut pair, client_ch, server_ch, 4_000_000, |_| {});
	let learned = metrics(&mut pair).bandwidth_estimate.unwrap();

	let idle_end = pair.time + Duration::from_secs(300);
	while pair.time < idle_end {
		if !pair.step() {
			pair.time = min_opt(pair.client.next_wakeup(), pair.server.next_wakeup())
				.expect("keep-alive timer stopped during the idle");
		}
	}
	assert!(!pair.client_conn_mut(client_ch).is_closed());

	let mut min_pacing = u64::MAX;
	let time = upload(&mut pair, client_ch, server_ch, BURST, |pair| {
		min_pacing = min_pacing.min(metrics(pair).pacing_rate.unwrap());
	});
	info!(learned, min_pacing, ?time);
	// Measured at 0.99x the learned 2 MB/s and 154ms. Without the starvation callback,
	// non-app-limited keep-alive samples age the max-bw filter down to about 530 B/s, and the
	// burst takes 720ms.
	assert!(
		min_pacing * 10 >= learned * 9,
		"paced at {min_pacing} B/s after learning {learned} B/s, seed {SEED}"
	);
}

/// Uploads `total` bytes on a fresh stream from client to server, calling `on_step` after every
/// step, and returns how long it took.
fn upload(
	pair: &mut Pair,
	client_ch: ConnectionHandle,
	server_ch: ConnectionHandle,
	total: usize,
	mut on_step: impl FnMut(&mut Pair),
) -> Duration {
	const CHUNK: [u8; 10_000] = [0; 10_000];

	let start = pair.time;
	let s = pair
		.client_streams(client_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();
	let mut unsent = total;
	let mut received = 0;
	let mut server_stream = None;
	loop {
		while unsent > 0 {
			match pair
				.client_send(client_ch, s)
				.write(&mut &CHUNK[..unsent.min(CHUNK.len())])
			{
				Ok(n) => unsent -= n,
				Err(send::Error::Blocked) => break,
				Err(e) => panic!("{e}"),
			}
			if unsent == 0 {
				pair.client_send(client_ch, s).finish().unwrap();
			}
		}

		if server_stream.is_none() {
			server_stream = pair.server_streams(server_ch).accept(Dir::Uni);
		}
		if let Some(stream) = server_stream {
			let mut recv = pair.server_recv(server_ch, stream);
			let mut chunks = recv.read().unwrap();
			while let Ok(Some(chunk)) = chunks.next(usize::MAX) {
				received += chunk.bytes.len();
			}
			let _ = chunks.finalize();
		}

		if received == total || !pair.step() {
			break;
		}
		on_step(pair);
	}
	assert_eq!(received, total);
	pair.time.saturating_duration_since(start)
}

#[test]
fn default_controller_is_bbr3() {
	let factory = transport::Config::default().congestion;
	let controller = factory.build(congestion::Init {
		now: Instant::now(),
		mtu: INITIAL_MTU,
		rtt: Duration::from_millis(333),
		max_pacing_rate: None,
	});
	assert!(controller.into_any().downcast::<congestion::Bbr3>().is_ok());
}
