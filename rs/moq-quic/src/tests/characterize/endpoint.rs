//! How an endpoint answers datagrams that match no connection

use super::*;

const CID_KEY: u64 = 0x1234_5678;
const REMOTE: &str = "[::2]:7890";

/// An endpoint config whose CIDs come from a hashed generator with a known key
fn endpoint_config() -> crate::endpoint::Config {
	crate::endpoint::Config {
		cid_generator: Some(Arc::new(|| Box::new(HashedConnectionIdGenerator::from_key(CID_KEY)))),
		..crate::endpoint::Config::default()
	}
}

/// A server endpoint running [`endpoint_config`], or a client-only one
fn endpoint(server: bool) -> Endpoint {
	let server = server.then(|| {
		let mut config = server_config();
		config.transport = transport();
		config
	});
	Endpoint::new(
		crate::endpoint::Config {
			server,
			..endpoint_config()
		},
		true,
	)
	.unwrap()
}

/// A CID the endpoint would accept as its own, without a connection behind it
fn valid_cid() -> ConnectionId {
	HashedConnectionIdGenerator::from_key(CID_KEY).generate_cid()
}

/// A short-header datagram to `dst_cid`, `len` bytes long
fn short(dst_cid: &[u8], len: usize) -> Vec<u8> {
	let mut packet = vec![0x40];
	packet.extend_from_slice(dst_cid);
	packet.resize(len, 0xaa);
	packet
}

/// A long-header datagram of type `ty` (0 Initial, 2 Handshake), padded to `len` bytes
fn long(ty: u8, version: u32, dst_cid: &[u8], len: usize) -> Vec<u8> {
	let mut packet = vec![0xc0 | (ty << 4)];
	packet.extend_from_slice(&version.to_be_bytes());
	packet.push(dst_cid.len() as u8);
	packet.extend_from_slice(dst_cid);
	packet.extend_from_slice(&[4, 1, 2, 3, 4]);
	if ty == 0 {
		// An empty token
		packet.push(0);
	}
	let remaining = len - packet.len() - 2;
	packet.write_var(remaining as u64);
	packet.resize(len, 0xaa);
	packet
}

/// What `endpoint` answers `packet` with, at `now`
fn answer(endpoint: &mut Endpoint, now: Instant, packet: &[u8]) -> Answer {
	answer_from(endpoint, REMOTE.parse().unwrap(), now, packet)
}

/// [`answer`] for a datagram from `remote`
fn answer_from(endpoint: &mut Endpoint, remote: SocketAddr, now: Instant, packet: &[u8]) -> Answer {
	let mut buf = Vec::new();
	match endpoint.handle(
		now,
		Received {
			remote,
			local_ip: None,
			ecn: None,
			data: packet.into(),
			arrival: None,
		},
		&mut buf,
	) {
		None => Answer::Nothing,
		Some(DatagramEvent::Response(transmit)) => Answer::Response(buf[..transmit.size].to_vec()),
		Some(DatagramEvent::NewConnection(incoming)) => {
			endpoint.ignore(incoming);
			Answer::NewConnection
		}
		Some(DatagramEvent::ConnectionEvent(..)) => panic!("no connection exists"),
	}
}

#[derive(Debug, PartialEq, Eq)]
enum Answer {
	Nothing,
	Response(Vec<u8>),
	NewConnection,
}

impl Answer {
	/// Whether this is `endpoint`'s stateless reset for `cid`: a short header ending in its token
	fn is_reset_for(&self, endpoint: &Endpoint, cid: ConnectionId) -> bool {
		let Self::Response(packet) = self else {
			return false;
		};
		let token = ResetToken::new(endpoint.reset_key(), cid);
		packet[0] & 0xc0 == 0x40 && packet.ends_with(&token[..])
	}
}

/// An unknown but valid CID gets one stateless reset per interval, smaller than what incited it
#[test]
fn unknown_cid_gets_stateless_reset() {
	let _guard = subscribe();
	let mut server = endpoint(true);
	let now = Instant::ZERO;
	let cid = valid_cid();
	let packet = short(&cid, 100);

	let first = answer(&mut server, now, &packet);
	assert!(first.is_reset_for(&server, cid), "got {first:?}");
	let Answer::Response(reset) = first else { unreachable!() };
	assert!(reset.len() < packet.len());

	assert_eq!(answer(&mut server, now, &packet), Answer::Nothing);
	let later = now.saturating_add(server.min_reset_interval());
	assert!(matches!(answer(&mut server, later, &packet), Answer::Response(_)));
}

/// Spoofed sources cannot silence resets for everyone: each source address limits only the
/// addresses sharing its slot, and the table is a fixed size however many addresses send.
#[test]
fn stateless_resets_are_limited_per_source() {
	let _guard = subscribe();
	let mut server = Endpoint::new(
		crate::endpoint::Config {
			server: Some(server_config()),
			rng_seed: Some([7; 32]),
			..endpoint_config()
		},
		true,
	)
	.unwrap();
	let now = Instant::ZERO;
	let cid = valid_cid();
	let packet = short(&cid, 100);
	let from = |n: u32| SocketAddr::new(Ipv6Addr::from(u128::from(n)).into(), 9000);

	let mut answered = 0;
	for source in 0..1024 {
		if matches!(
			answer_from(&mut server, from(source), now, &packet),
			Answer::Response(_)
		) {
			answered += 1;
		}
	}
	// Each slot answers once, and a source never leaves its slot, so sources it shares the slot
	// with, and only those, go unanswered.
	assert!((128..=256).contains(&answered), "answered {answered} sources");
	for source in 0..1024 {
		assert_eq!(answer_from(&mut server, from(source), now, &packet), Answer::Nothing);
	}
	let later = now.saturating_add(server.min_reset_interval());
	assert!(matches!(
		answer_from(&mut server, from(0), later, &packet),
		Answer::Response(_)
	));
}

/// A response is written from the start of the buffer whatever it held, so `Transmit::size` is its length
#[test]
fn responses_ignore_stale_buffer_contents() {
	let _guard = subscribe();
	let mut server = endpoint(true);
	let cid = valid_cid();
	let mut buf = vec![0xee; 40];
	let received = |data: Vec<u8>| Received {
		remote: REMOTE.parse().unwrap(),
		local_ip: None,
		ecn: None,
		data: data.as_slice().into(),
		arrival: None,
	};

	let Some(DatagramEvent::Response(reset)) = server.handle(Instant::ZERO, received(short(&cid, 100)), &mut buf)
	else {
		panic!("expected a stateless reset");
	};
	assert_eq!(reset.size, buf.len());

	let mut unsupported = vec![0xc0, 0xba, 0xba, 0xba, 0xba, 8];
	unsupported.extend_from_slice(&[1; 8]);
	unsupported.push(0);
	unsupported.resize(1200, 0);
	let mut buf = vec![0xee; 40];
	let Some(DatagramEvent::Response(negotiation)) = server.handle(Instant::ZERO, received(unsupported), &mut buf)
	else {
		panic!("expected version negotiation");
	};
	assert_eq!(negotiation.size, buf.len());
	assert_eq!(buf[0] & 0x80, 0x80);
}

/// A reset must be smaller than its inciting datagram yet look like a short header, so datagrams
/// of 21 bytes or fewer get none
#[test]
fn small_unknown_packet_gets_no_reset() {
	let _guard = subscribe();
	let now = Instant::ZERO;
	let cid = valid_cid();
	assert_eq!(answer(&mut endpoint(true), now, &short(&cid, 21)), Answer::Nothing);
	let Answer::Response(reset) = answer(&mut endpoint(true), now, &short(&cid, 22)) else {
		panic!("expected a stateless reset");
	};
	assert_eq!(reset.len(), 21);
}

/// The CID generator rejects a CID it could not have issued, so it gets no reset
#[test]
fn invalid_cid_is_dropped() {
	let _guard = subscribe();
	let now = Instant::ZERO;
	assert_eq!(
		answer(&mut endpoint(true), now, &short(&[0xab; 8], 100)),
		Answer::Nothing
	);
}

#[test]
fn zero_length_cid_short_packet_is_dropped() {
	let _guard = subscribe();
	let config = crate::endpoint::Config {
		cid_generator: Some(Arc::new(|| Box::new(RandomConnectionIdGenerator::new(0)))),
		..crate::endpoint::Config::default()
	};
	let mut server = Endpoint::new(config.with_server(server_config()), true).unwrap();
	assert_eq!(answer(&mut server, Instant::ZERO, &short(&[], 100)), Answer::Nothing);
}

/// Only Initials can start a connection, so another long header for an unknown CID is dropped
#[test]
fn long_header_for_unknown_connection_is_dropped() {
	let _guard = subscribe();
	let now = Instant::ZERO;
	let packet = long(2, Version::V1.wire(), &valid_cid(), 1200);
	assert_eq!(answer(&mut endpoint(true), now, &packet), Answer::Nothing);
}

/// An Initial in a datagram under 1200 bytes is dropped before the endpoint derives its keys,
/// while a client's real Initial is exactly 1200
#[test]
fn short_initial_is_dropped() {
	let _guard = subscribe();
	let packet = long(0, Version::V1.wire(), &[0x11; 8], 1199);
	assert_eq!(answer(&mut endpoint(true), Instant::ZERO, &packet), Answer::Nothing);

	let mut pair = pair_with(|_| {});
	pair.begin_connect(client_with(|_| {}));
	pair.drive_client();
	let (_, _, datagram) = pair.server.inbound.pop_front().unwrap();
	assert_eq!(datagram.len(), 1200);
	let now = pair.time;
	assert_eq!(answer(&mut pair.server, now, &datagram), Answer::NewConnection);
}

/// An authenticated Initial whose DCID is shorter than RFC 9000's 8 bytes is refused with a close
#[test]
fn initial_with_short_cid_is_refused() {
	let _guard = subscribe();
	let mut pair = pair_with(|_| {});
	let mut client = client_with(|_| {});
	client.initial_dst_cid_provider = Arc::new(|| ConnectionId::new(&[0x11; 7]));
	pair.begin_connect(client);
	pair.drive_client();
	let (_, _, datagram) = pair.server.inbound.pop_front().unwrap();
	let now = pair.time;
	let answer = answer(&mut pair.server, now, &datagram);
	let Answer::Response(close) = answer else {
		panic!("expected an Initial close, got {answer:?}");
	};
	assert_eq!(close[0] & 0xf0, 0xc0, "a long-header Initial");
	assert_eq!(pair.server.incoming_buffer_bytes(), 0);
}

/// Junk shaped like an Initial with a short or empty DCID fails authentication, so it earns no
/// close that a spoofed source could aim at a victim
#[test]
fn unauthenticated_initial_with_short_cid_gets_no_reply() {
	let _guard = subscribe();
	let mut server = endpoint(true);
	for dst_cid in [&[0x11; 7][..], &[]] {
		let packet = long(0, Version::V1.wire(), dst_cid, 1200);
		assert_eq!(answer(&mut server, Instant::ZERO, &packet), Answer::Nothing);
	}
}

/// Garbage under header protection unmasks to random reserved bits, and when they pass as zero
/// the packet still fails authentication. The endpoint drops it before it holds a slot, buffers
/// bytes, or earns a reply, whichever sample it carries.
#[test]
fn undecryptable_initial_is_dropped_unbuffered() {
	let _guard = subscribe();
	let mut server = endpoint(true);
	let now = Instant::ZERO;
	let mut packet = long(0, Version::V1.wire(), &[0x11; 8], 1200);
	for byte in 0..=u8::MAX {
		// Vary the header protection sample, 4 bytes past the packet number at byte 22
		packet[26] = byte;
		assert_eq!(answer(&mut server, now, &packet), Answer::Nothing);
	}
	assert_eq!(server.incoming_buffer_bytes(), 0);
	assert_eq!(server.open_connections(), 0);
}

#[test]
fn undecodable_datagram_is_dropped() {
	let _guard = subscribe();
	let now = Instant::ZERO;
	// A long header cut off inside its version
	assert_eq!(answer(&mut endpoint(true), now, &[0xc0, 0, 0]), Answer::Nothing);
	// An Initial whose two-byte length field, after its 20 bytes of header, runs past the
	// datagram
	let mut packet = long(0, Version::V1.wire(), &[0x11; 8], 1200);
	packet[20..22].copy_from_slice(&[0x7f, 0xff]);
	assert_eq!(answer(&mut endpoint(true), now, &packet), Answer::Nothing);
}

/// Only a server negotiates versions, and only for datagrams big enough to start a connection
#[test]
fn wrong_version_initial() {
	let _guard = subscribe();
	let now = Instant::ZERO;
	let packet = long(0, 0x0a1a_2a3a, &[0x11; 8], 1200);
	assert!(matches!(answer(&mut endpoint(true), now, &packet), Answer::Response(_)));
	assert_eq!(answer(&mut endpoint(false), now, &packet), Answer::Nothing);
	let packet = long(0, 0x0a1a_2a3a, &[0x11; 8], 1199);
	assert_eq!(answer(&mut endpoint(true), now, &packet), Answer::Nothing);
}

/// A client-only endpoint answers both an Initial and a short header for an unknown connection
/// with a stateless reset
#[test]
fn client_endpoint_sends_stateless_reset() {
	let _guard = subscribe();
	let now = Instant::ZERO;
	let cid = valid_cid();
	let mut client = endpoint(false);
	assert!(answer(&mut client, now, &short(&cid, 100)).is_reset_for(&client, cid));

	let dst_cid = ConnectionId::new(&[0x11; 8]);
	let packet = long(0, Version::V1.wire(), &dst_cid, 1200);
	let mut client = endpoint(false);
	assert!(answer(&mut client, now, &packet).is_reset_for(&client, dst_cid));
}

/// A server whose one-byte CID space is three-quarters used
fn exhausted() -> Pair {
	exhausted_by(|| Box::new(RandomConnectionIdGenerator::new(1)))
}

fn exhausted_by(generator: fn() -> Box<dyn ConnectionIdGenerator>) -> Pair {
	let config = crate::endpoint::Config {
		cid_generator: Some(Arc::new(generator)),
		..crate::endpoint::Config::default()
	};
	let mut server_config = server_config();
	server_config.transport = transport();
	let server = Endpoint::new(
		crate::endpoint::Config {
			server: Some(server_config),
			..config
		},
		true,
	)
	.unwrap();
	let client = Endpoint::new(crate::endpoint::Config::default(), true).unwrap();
	Pair::new_from_endpoint(client, server)
}

/// Uses up the server's CID space with its own outgoing connections, returning how many it made
fn exhaust(pair: &mut Pair) -> usize {
	let mut made = 0;
	loop {
		let remote = SocketAddr::new(Ipv6Addr::LOCALHOST.into(), 1);
		match pair.server.connect(
			pair.time,
			Dial {
				config: &client_with(|_| {}),
				remote,
				server_name: "localhost",
			},
		) {
			Ok((ch, conn)) => {
				pair.server.connections.insert(ch, conn);
				made += 1;
			}
			Err(crate::endpoint::ConnectError::CidsExhausted) => return made,
			Err(e) => panic!("{e}"),
		}
	}
}

#[test]
fn cids_exhausted_refuses_connect() {
	let _guard = subscribe();
	let mut pair = exhausted();
	// 256 CIDs, a quarter held in reserve
	assert_eq!(exhaust(&mut pair), 193);
}

/// A generator that spends most of its CID on something other than randomness holds fewer CIDs than
/// the CID's length says, and the endpoint stops before `new_cid` could spin looking for a free one.
#[test]
fn cids_exhausted_counts_random_bits_not_bytes() {
	struct Mostly(RandomConnectionIdGenerator);
	impl ConnectionIdGenerator for Mostly {
		fn generate_cid(&mut self) -> ConnectionId {
			let mut bytes = [0u8; 8];
			bytes[0] = self.0.generate_cid()[0];
			ConnectionId::new(&bytes)
		}
		fn cid_len(&self) -> usize {
			8
		}
		fn cid_lifetime(&self) -> Option<Duration> {
			None
		}
		fn entropy_bits(&self) -> u32 {
			8
		}
	}

	let _guard = subscribe();
	let mut pair = exhausted_by(|| Box::new(Mostly(RandomConnectionIdGenerator::new(1))));
	assert_eq!(exhaust(&mut pair), 193);
}

/// Connections asking for more IDs than are left get what the space has to give, never a search
/// for one that may not exist: past 7/8 held, issuance stops.
#[test]
fn cid_issuance_stops_before_the_space_is_held() {
	use crate::shared::{ConnectionEventInner, EndpointEvent, EndpointEventInner};

	let _guard = subscribe();
	let mut pair = exhausted();
	assert_eq!(exhaust(&mut pair), 193);
	let ch = *pair.server.connections.keys().next().unwrap();
	let event = EndpointEvent(EndpointEventInner::NeedIdentifiers(pair.time, 1000));
	let Some(event) = pair.server.endpoint.handle_event(ch, event) else {
		panic!("expected identifiers");
	};
	let ConnectionEventInner::NewIdentifiers(ids, _) = event.0 else {
		panic!("expected identifiers");
	};
	// 256 IDs, an eighth held in reserve
	assert_eq!(pair.server.endpoint.known_cids(), 225);
	assert_eq!(ids.len(), 225 - 193);
}

/// Exhaustion before an Initial arrives drops it without a word
#[test]
fn cids_exhausted_drops_initial() {
	let _guard = subscribe();
	let mut pair = exhausted();
	exhaust(&mut pair);
	let client_ch = pair.begin_connect(client_with(|_| {}));
	pair.drive_client();
	pair.drive_server();
	pair.server.assert_no_accept();
	assert!(pair.client.inbound.is_empty());
	assert!(pair.client_conn_mut(client_ch).is_handshaking());
}

/// Exhaustion between an Initial and its accept refuses the connection
#[test]
fn cids_exhausted_refuses_accept() {
	let _guard = subscribe();
	let mut pair = exhausted();
	pair.server.handle_incoming = Box::new(|_| IncomingConnectionBehavior::Wait);
	let client_ch = pair.begin_connect(client_with(|_| {}));
	pair.drive_client();
	pair.drive_server();
	let incoming = pair.server.pop_waiting_incoming();

	exhaust(&mut pair);
	let now = pair.time;
	assert_matches!(
		pair.server.try_accept(incoming, now),
		Err(ConnectionError::CidsExhausted)
	);
	pair.drive();
	assert_matches!(
		lost(pair.client_conn_mut(client_ch)),
		Some(ConnectionError::ConnectionClosed(ConnectionClose {
			error_code: TransportErrorCode::CONNECTION_REFUSED,
			..
		}))
	);
}
