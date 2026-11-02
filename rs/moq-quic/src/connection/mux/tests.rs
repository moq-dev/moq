use bytes::{BufMut, Bytes};

use super::*;
use crate::{StreamId, TransportErrorCode, coding::BufMutExt, frame::FrameType, recv, send};

/// Two connections joined by an in-memory transport, on a clock the test advances
struct Pair {
	client: Connection,
	server: Connection,
	now: Instant,
}

impl Pair {
	fn new(config: Config) -> Self {
		Self::with(config.clone(), config)
	}

	fn with(client: Config, server: Config) -> Self {
		let now = Instant::now();
		let mut pair = Self {
			client: Connection::new(client, Side::Client, now),
			server: Connection::new(server, Side::Server, now),
			now,
		};
		pair.drive();
		pair
	}

	/// Exchange records until neither side has anything to write
	fn drive(&mut self) {
		loop {
			let mut idle = true;
			while let Some(record) = self.client.poll_transmit(self.now) {
				self.server.handle_record(record, self.now);
				idle = false;
			}
			while let Some(record) = self.server.poll_transmit(self.now) {
				self.client.handle_record(record, self.now);
				idle = false;
			}
			if idle {
				return;
			}
		}
	}

	fn advance(&mut self, by: Duration) {
		self.now += by;
		self.client.handle_timeout(self.now);
		self.server.handle_timeout(self.now);
	}
}

fn config(version: Version) -> Config {
	Config {
		version,
		..Config::default()
	}
}

fn events(conn: &mut Connection) -> Vec<Event> {
	std::iter::from_fn(|| conn.poll()).collect()
}

/// Read every byte currently available on `id`
fn read_all(conn: &mut Connection, id: StreamId) -> (Vec<u8>, bool) {
	let mut recv = conn.recv_stream(id);
	let mut chunks = recv.read().unwrap();
	let mut data = Vec::new();
	let mut fin = false;
	loop {
		match chunks.next(usize::MAX) {
			Ok(Some(chunk)) => data.extend_from_slice(&chunk.bytes),
			Ok(None) => {
				fin = true;
				break;
			}
			Err(recv::Error::Blocked) => break,
			Err(err) => panic!("read failed: {err}"),
		}
	}
	let _ = chunks.finalize();
	(data, fin)
}

fn lost(conn: &mut Connection) -> Option<ConnectionError> {
	events(conn).into_iter().find_map(|event| match event {
		Event::ConnectionLost { reason } => Some(reason),
		_ => None,
	})
}

#[test]
fn handshake() {
	for &version in Version::ALL {
		let mut pair = Pair::new(config(version));
		assert!(pair.client.is_established(), "{version:?}");
		assert!(pair.server.is_established(), "{version:?}");
		assert!(matches!(events(&mut pair.client)[..], [Event::Connected]));
		assert!(matches!(events(&mut pair.server)[..], [Event::Connected]));
	}
}

#[test]
fn streams_carry_data_and_fin() {
	for &version in Version::ALL {
		let mut pair = Pair::new(config(version));
		let id = pair.client.streams().open(Dir::Bi, send::Options::default()).unwrap();
		let payload = vec![7u8; 100_000];
		let mut sent = 0;
		while sent < payload.len() {
			sent += pair.client.send_stream(id).write(&mut &payload[sent..]).unwrap();
			pair.drive();
		}
		pair.client.send_stream(id).finish().unwrap();
		pair.drive();

		assert_eq!(pair.server.streams().accept(Dir::Bi), Some(id));
		let mut received = Vec::new();
		loop {
			let (data, fin) = read_all(&mut pair.server, id);
			received.extend(data);
			pair.drive();
			if fin {
				break;
			}
		}
		assert_eq!(received, payload, "{version:?}");
		// Serializing the FIN acknowledged it
		assert!(
			events(&mut pair.client)
				.iter()
				.any(|e| matches!(e, Event::Stream(StreamEvent::Finished { id: done }) if *done == id))
		);
	}
}

/// Every byte received returns its connection credit, whether read, dropped unread, or
/// arriving after STOP_SENDING, so a long-lived session never stalls on MAX_DATA
#[test]
fn credit_returns_for_unread_and_stopped_streams() {
	let mut cfg = config(Version::QMux02);
	cfg.limits.receive_window = NonZeroU64::new(64 << 10);
	cfg.limits.stream_receive_window = NonZeroU64::new(32 << 10);
	let mut pair = Pair::new(cfg);
	let chunk = vec![1u8; 16 << 10];

	// Far more than the connection window, a stream at a time
	for round in 0..64 {
		let id = pair.client.streams().open(Dir::Uni, send::Options::default()).unwrap();
		assert_eq!(
			pair.client.send_stream(id).write(&mut &chunk[..]).unwrap(),
			chunk.len(),
			"round {round}"
		);
		pair.drive();

		let accepted = pair.server.streams().accept(Dir::Uni).unwrap();
		match round % 2 {
			// Stopped before the rest arrives, which then arrives anyway
			0 => {
				pair.server.recv_stream(accepted).stop(VarInt::from_u32(1)).unwrap();
				let _ = pair.client.send_stream(id).write(&mut &chunk[..]);
			}
			// Read to the end
			_ => {
				pair.client.send_stream(id).finish().unwrap();
				pair.drive();
				let (data, fin) = read_all(&mut pair.server, accepted);
				assert_eq!(data.len(), chunk.len());
				assert!(fin);
			}
		}
		pair.drive();
	}
	assert!(pair.client.is_established());
	assert!(pair.server.is_established());
}

#[test]
fn close_delivers_application_close() {
	let mut pair = Pair::new(config(Version::QMux02));
	pair.client.close(VarInt::from_u32(42), "bye".to_string());
	// The first close wins
	pair.client.close(VarInt::from_u32(7), String::new());
	assert!(!pair.client.is_drained());
	pair.drive();
	assert!(pair.client.is_drained());
	match lost(&mut pair.server) {
		Some(ConnectionError::ApplicationClosed(close)) => {
			assert_eq!(close.error_code, VarInt::from_u32(42));
			assert_eq!(close.reason, "bye");
		}
		other => panic!("unexpected {other:?}"),
	}
	assert!(matches!(
		lost(&mut pair.client),
		Some(ConnectionError::LocallyClosed { code, reason }) if code == VarInt::from_u32(42) && reason == "bye"
	));
}

/// A record carrying the peer's parameters, then `frame`
fn after_params(version: Version, frame: &[u8]) -> Bytes {
	let mut peer = Connection::new(config(version), Side::Client, Instant::now());
	let mut record = peer.poll_transmit(Instant::now()).unwrap().to_vec();
	record.extend_from_slice(frame);
	record.into()
}

fn transport_error(conn: &mut Connection) -> TransportError {
	match lost(conn) {
		Some(ConnectionError::TransportError(err)) => err,
		other => panic!("expected a transport error, got {other:?}"),
	}
}

#[test]
fn rejects_prohibited_frames() {
	// PING, ACK, CRYPTO, HANDSHAKE_DONE
	let frames: &[&[u8]] = &[&[0x01], &[0x02, 0x00, 0x00, 0x00, 0x00], &[0x06, 0x00, 0x00], &[0x1e]];
	for frame in frames {
		let now = Instant::now();
		let mut server = Connection::new(config(Version::QMux02), Side::Server, now);
		server.handle_record(after_params(Version::QMux02, frame), now);
		let err = transport_error(&mut server);
		assert_eq!(err.code, TransportErrorCode::PROTOCOL_VIOLATION, "{frame:?}");
		// The violation goes out as a CONNECTION_CLOSE after our parameters
		let record = server.poll_transmit(now).unwrap();
		let mut rest = record.clone();
		assert!(matches!(
			wire::next_frame(&mut rest).unwrap(),
			Some(wire::Frame::TransportParameters(_))
		));
		assert!(matches!(
			wire::next_frame(&mut rest).unwrap(),
			Some(wire::Frame::Quic(frame::Frame::Close(Close::Connection(_))))
		));
		assert!(server.is_drained());
	}
}

#[test]
fn draft02_requires_params_first() {
	let now = Instant::now();
	let mut server = Connection::new(config(Version::QMux02), Side::Server, now);
	// MAX_DATA before the parameters
	server.handle_record(Bytes::from_static(&[0x10, 0x01]), now);
	assert_eq!(
		transport_error(&mut server).code,
		TransportErrorCode::PROTOCOL_VIOLATION
	);

	// Draft-01 tolerates it
	let mut server = Connection::new(config(Version::QMux01), Side::Server, now);
	server.handle_record(Bytes::from_static(&[0x10, 0x01]), now);
	assert!(!server.is_closed());

	// A second QX_TRANSPORT_PARAMETERS is a violation on every version
	for &version in Version::ALL {
		let mut pair = Pair::new(config(version));
		let mut repeat = Connection::new(config(version), Side::Client, now);
		let params = repeat.poll_transmit(now).unwrap();
		pair.server.handle_record(params, pair.now);
		assert_eq!(
			transport_error(&mut pair.server).code,
			TransportErrorCode::PROTOCOL_VIOLATION
		);
	}
}

#[test]
fn rejects_empty_records() {
	let mut pair = Pair::new(config(Version::QMux02));
	pair.server.handle_record(Bytes::new(), pair.now);
	assert_eq!(
		transport_error(&mut pair.server).code,
		TransportErrorCode::FRAME_ENCODING_ERROR
	);
}

#[test]
fn draft00_counts_each_padding_byte_as_a_frame() {
	let mut pair = Pair::new(config(Version::QMux00));
	pair.server.handle_record(Bytes::from_static(&[0x00]), pair.now);
	assert!(!pair.server.is_closed());
	pair.server.handle_record(Bytes::from_static(&[0x00, 0x00]), pair.now);
	assert_eq!(
		transport_error(&mut pair.server).code,
		TransportErrorCode::FRAME_ENCODING_ERROR
	);
}

#[test]
fn draft00_rejects_qx_ping() {
	for ty in [0x348c67529ef8c7bd_u64, 0x348c67529ef8c7be] {
		let mut pair = Pair::new(config(Version::QMux00));
		let mut ping = Vec::new();
		ping.write_var(ty);
		ping.write_var(0);
		pair.server.handle_record(ping.into(), pair.now);
		assert_eq!(
			transport_error(&mut pair.server).code,
			TransportErrorCode::FRAME_ENCODING_ERROR
		);
	}
}

#[test]
fn rejects_oversized_records() {
	let now = Instant::now();
	let mut server = Connection::new(config(Version::QMux01), Side::Server, now);
	server.handle_record(vec![0; DEFAULT_MAX_RECORD_SIZE as usize + 1].into(), now);
	assert_eq!(
		transport_error(&mut server).code,
		TransportErrorCode::PROTOCOL_VIOLATION
	);

	let mut server = Connection::new(config(Version::QMux00), Side::Server, now);
	server.handle_record(vec![0; MAX_FRAME_SIZE + 1].into(), now);
	assert_eq!(
		transport_error(&mut server).code,
		TransportErrorCode::FRAME_ENCODING_ERROR
	);

	// The same, refused by the transport before it was read
	for (version, code) in [
		(Version::QMux01, TransportErrorCode::PROTOCOL_VIOLATION),
		(Version::QMux00, TransportErrorCode::FRAME_ENCODING_ERROR),
	] {
		let mut server = Connection::new(config(version), Side::Server, now);
		server.handle_oversized();
		assert_eq!(transport_error(&mut server).code, code);
		assert!(server.poll_transmit(now).is_some(), "CONNECTION_CLOSE");
	}
}

#[test]
fn records_stay_under_the_stream_ceiling() {
	let mut cfg = config(Version::QMux02);
	cfg.max_record_size = VarInt::from_u32(1 << 20);
	let mut pair = Pair::new(cfg);
	let id = pair.client.streams().open(Dir::Uni, send::Options::default()).unwrap();
	pair.client.send_stream(id).write(&mut &[0; 100_000][..]).unwrap();
	while let Some(record) = pair.client.poll_transmit(pair.now) {
		assert!(record.len() <= MAX_RECORD_BUDGET);
		pair.server.handle_record(record, pair.now);
	}
}

#[test]
fn draft00_sends_one_length_prefixed_frame_per_message() {
	let now = Instant::now();
	let mut client = Connection::new(config(Version::QMux00), Side::Client, now);
	let mut server = Connection::new(config(Version::QMux00), Side::Server, now);
	while let Some(frame) = server.poll_transmit(now) {
		client.handle_record(frame, now);
	}
	let id = client.streams().open(Dir::Uni, send::Options::default()).unwrap();
	client.send_stream(id).write(&mut &[3; 40_000][..]).unwrap();
	client.send_stream(id).finish().unwrap();
	let mut messages = 0;
	while let Some(message) = client.poll_transmit(now) {
		assert!(message.len() <= MAX_FRAME_SIZE);
		let ty = message[0];
		if (0x08..=0x0f).contains(&ty) {
			assert_ne!(ty & 0x02, 0, "STREAM frame without a Length field");
		}
		let mut rest = message.clone();
		wire::next_frame(&mut rest).unwrap().unwrap();
		assert!(rest.is_empty(), "more than one frame in a message");
		server.handle_record(message, now);
		messages += 1;
	}
	assert!(messages > 3);
	assert!(!server.is_closed());
	assert_eq!(server.streams().accept(Dir::Uni), Some(id));
	let (data, fin) = read_all(&mut server, id);
	assert_eq!(data.len(), 40_000);
	assert!(fin);
}

#[test]
fn stream_offsets_follow_the_transport_order() {
	let now = Instant::now();
	// A STREAM frame on stream 2 (client uni) at offset 5, before any data
	let mut gap = Vec::new();
	gap.write_var(0x0e); // OFF | LEN
	gap.write_var(2);
	gap.write_var(5);
	gap.write_var(1);
	gap.put_u8(0);
	let mut first = Vec::new();
	frame::StreamMeta {
		id: StreamId(2),
		offsets: 0..3,
		fin: false,
	}
	.encode(true, &mut first);
	first.extend_from_slice(b"abc");

	let mut server = Connection::new(config(Version::QMux01), Side::Server, now);
	server.handle_record(after_params(Version::QMux01, &first), now);
	server.handle_record(gap.clone().into(), now);
	assert_eq!(
		transport_error(&mut server).code,
		TransportErrorCode::PROTOCOL_VIOLATION
	);

	// Including as the stream's first frame
	let mut server = Connection::new(config(Version::QMux01), Side::Server, now);
	server.handle_record(after_params(Version::QMux01, &gap), now);
	assert_eq!(
		transport_error(&mut server).code,
		TransportErrorCode::PROTOCOL_VIOLATION
	);

	// Released senders omit the offset on every frame, which continues the stream
	let mut server = Connection::new(config(Version::QMux01), Side::Server, now);
	server.handle_record(after_params(Version::QMux01, &first), now);
	server.handle_record(Bytes::from(first.clone()), now);
	assert!(!server.is_closed());
	let id = server.streams().accept(Dir::Uni).unwrap();
	assert_eq!(read_all(&mut server, id).0, b"abcabc");
}

#[test]
fn reset_stream_at_delivers_the_prefix() {
	let mut pair = Pair::new(config(Version::QMux02));
	let id = pair.client.streams().open(Dir::Uni, send::Options::default()).unwrap();
	pair.client.send_stream(id).write(&mut &b"header"[..]).unwrap();
	pair.client.send_stream(id).write(&mut &b"payload"[..]).unwrap();
	let mut stream = pair.client.send_stream(id);
	let at = stream.offset().rewind(6);
	stream.reset_at(at, VarInt::from_u32(9)).unwrap();
	pair.drive();

	let accepted = pair.server.streams().accept(Dir::Uni).unwrap();
	let mut recv = pair.server.recv_stream(accepted);
	let mut chunks = recv.read().unwrap();
	let mut data = Vec::new();
	let err = loop {
		match chunks.next(usize::MAX) {
			Ok(Some(chunk)) => data.extend_from_slice(&chunk.bytes),
			Ok(None) => panic!("finished instead of reset"),
			Err(err) => break err,
		}
	};
	let _ = chunks.finalize();
	assert!(data.starts_with(b"header"), "{data:?} {err:?}");
	assert_eq!(err, recv::Error::Reset(VarInt::from_u32(9)));
}

#[test]
fn reset_stream_at_needs_draft02() {
	let mut reset = Vec::new();
	reset.write(FrameType::RESET_STREAM_AT);
	reset.write_var(2);
	reset.write_var(0);
	reset.write_var(4);
	reset.write_var(2);
	let now = Instant::now();
	let mut server = Connection::new(config(Version::QMux01), Side::Server, now);
	server.handle_record(after_params(Version::QMux01, &reset), now);
	assert_eq!(
		transport_error(&mut server).code,
		TransportErrorCode::FRAME_ENCODING_ERROR
	);
}

#[test]
fn datagrams() {
	let mut pair = Pair::new(config(Version::QMux02));
	let max = pair.client.datagrams().max_size().unwrap();
	assert!(max > 16_000);
	pair.client
		.datagrams()
		.send(Bytes::from_static(b"hello"), send::Options::default())
		.unwrap();
	pair.drive();
	assert!(
		events(&mut pair.server)
			.iter()
			.any(|e| matches!(e, Event::DatagramReceived))
	);
	assert_eq!(pair.server.datagrams().recv().unwrap(), &b"hello"[..]);

	// Draft-00 has none
	let mut pair = Pair::new(config(Version::QMux00));
	assert_eq!(
		pair.client
			.datagrams()
			.send(Bytes::from_static(b"hello"), send::Options::default()),
		Err(send::Error::Unsupported)
	);
}

#[test]
fn datagram_limits_count_the_encoded_frame() {
	let mut limited = config(Version::QMux02);
	limited.max_datagram_frame_size = Some(VarInt::from_u32(1200));

	// A frame without its Length field may fill the limit exactly
	let mut pair = Pair::with(config(Version::QMux02), limited.clone());
	let mut fits = vec![0x30];
	fits.resize(1200, 7);
	pair.server.handle_record(fits.into(), pair.now);
	assert_eq!(pair.server.datagrams().recv().unwrap().len(), 1199);
	let mut over = vec![0x30];
	over.resize(1201, 7);
	pair.server.handle_record(over.into(), pair.now);
	assert_eq!(
		transport_error(&mut pair.server).code,
		TransportErrorCode::PROTOCOL_VIOLATION
	);

	// Sending sizes the Length field exactly: 1200 bytes carry 1197
	let mut pair = Pair::with(config(Version::QMux02), limited);
	assert_eq!(pair.client.datagrams().max_size(), Some(1197));
	pair.client
		.datagrams()
		.send(Bytes::from(vec![7; 1197]), send::Options::default())
		.unwrap();
	pair.drive();
	assert_eq!(pair.server.datagrams().recv().unwrap().len(), 1197);

	// A limit too small for an empty frame takes none
	let mut tiny = config(Version::QMux02);
	tiny.max_datagram_frame_size = Some(VarInt::from_u32(1));
	let mut pair = Pair::with(config(Version::QMux02), tiny);
	assert_eq!(pair.client.datagrams().max_size(), None);
	assert!(
		pair.client
			.datagrams()
			.send(Bytes::new(), send::Options::default())
			.is_err()
	);
}

#[test]
fn a_small_datagram_does_not_pin_its_record() {
	let mut pair = Pair::new(config(Version::QMux02));
	let mut record = Vec::new();
	record.write_var(0x31); // DATAGRAM with a length
	record.write_var(1);
	record.put_u8(7);
	record.resize(DEFAULT_MAX_RECORD_SIZE as usize, 0); // PADDING
	let record = Bytes::from(record);
	let span = record.as_ptr_range();
	pair.server.handle_record(record.clone(), pair.now);

	let datagram = pair.server.datagrams().recv().unwrap();
	assert_eq!(datagram, &[7][..]);
	assert!(
		!span.contains(&datagram.as_ptr()),
		"the datagram still points into its record"
	);
}

#[test]
fn negotiates_the_servers_preference() {
	let mut client = config(Version::QMux01);
	client.protocols = Some(vec!["a".into(), "b".into(), "c".into()]);
	let mut server = config(Version::QMux01);
	server.protocols = Some(vec!["c".into(), "b".into()]);
	let pair = Pair::with(client.clone(), server);
	assert_eq!(pair.client.protocol(), Some("c"));
	assert_eq!(pair.server.protocol(), Some("c"));

	// An offer the other side did not ask for is an error
	let mut pair = Pair::with(client.clone(), config(Version::QMux01));
	assert_eq!(
		transport_error(&mut pair.server).code,
		TransportErrorCode::TRANSPORT_PARAMETER_ERROR
	);

	// So is agreeing on nothing
	let mut server = config(Version::QMux01);
	server.protocols = Some(vec!["d".into()]);
	let mut pair = Pair::with(client, server);
	assert_eq!(
		transport_error(&mut pair.server).code,
		TransportErrorCode::TRANSPORT_PARAMETER_ERROR
	);
}

#[test]
fn keepalive_holds_an_idle_session_open() {
	let mut pair = Pair::new(config(Version::QMux02));
	for _ in 0..30 {
		pair.advance(Duration::from_secs(5));
		pair.drive();
	}
	assert!(pair.client.is_established());
	assert!(pair.client.rtt().is_some());
}

#[test]
fn idle_timeout_reclaims_a_silent_peer() {
	let mut pair = Pair::new(config(Version::QMux02));
	// The server never hears back: its keep-alives go out, nothing answers
	for _ in 0..10 {
		pair.advance(Duration::from_secs(5));
		while pair.server.poll_transmit(pair.now).is_some() {}
		while pair.client.poll_transmit(pair.now).is_some() {}
	}
	assert!(matches!(lost(&mut pair.server), Some(ConnectionError::TimedOut)));
}

#[test]
fn backpressure_defers_the_idle_timeout_once() {
	let mut pair = Pair::new(config(Version::QMux02));
	pair.server.set_backpressured(true);
	pair.advance(Duration::from_secs(31));
	assert!(!pair.server.is_closed());
	pair.advance(Duration::from_secs(31));
	assert!(matches!(lost(&mut pair.server), Some(ConnectionError::TimedOut)));
}

#[test]
fn handshake_timeout() {
	let now = Instant::now();
	let mut server = Connection::new(config(Version::QMux02), Side::Server, now);
	let deadline = server.poll_timeout().unwrap();
	server.handle_timeout(deadline);
	assert!(matches!(lost(&mut server), Some(ConnectionError::HandshakeTimeout)));
}

#[test]
fn transport_end_without_close() {
	let mut pair = Pair::new(config(Version::QMux02));
	pair.server.transport_closed();
	assert!(matches!(lost(&mut pair.server), Some(ConnectionError::Reset)));
	assert!(pair.server.is_drained());
}

/// The first record of a default client, byte for byte, in the order qmux 0.6 encodes, so a
/// codec change shows up as a wire change
#[test]
fn golden_transport_parameters() {
	let expected = [
		(
			Version::QMux02,
			concat!(
				"ff5153300d0a0d0a370104800075300404801000000504800400000604800400",
				"00070480040000080240640902406420027ffec571c59429cd0845027ffe1d00",
			),
		),
		(
			Version::QMux01,
			concat!(
				"ff5153300d0a0d0a350104800075300404801000000504800400000604800400",
				"00070480040000080240640902406420027ffec571c59429cd0845027ffe",
			),
		),
		(
			Version::QMux00,
			concat!(
				"ff5153300d0a0d0a2b0404801000000504800400000604800400000704800400",
				"000802406409024064c571c59429cd0845027ffe",
			),
		),
	];
	for (version, hex) in expected {
		let mut client = Connection::new(config(version), Side::Client, Instant::now());
		let record = client.poll_transmit(Instant::now()).unwrap();
		let actual: String = record.iter().map(|b| format!("{b:02x}")).collect();
		assert_eq!(actual, hex, "{version:?}");
	}
}

/// An established client's records for a finished stream, a reset, and a close, byte for
/// byte, so a framing change shows up as a wire change. Every version frames them alike:
/// the record header is the transport's.
#[test]
fn golden_records() {
	for &version in Version::ALL {
		let mut pair = Pair::new(config(version));
		let record = |conn: &mut Connection, now| {
			let record = conn.poll_transmit(now).unwrap();
			assert!(conn.poll_transmit(now).is_none());
			record.iter().map(|b| format!("{b:02x}")).collect::<String>()
		};

		let id = pair.client.streams().open(Dir::Uni, send::Options::default()).unwrap();
		pair.client.send_stream(id).write(&mut &b"hi"[..]).unwrap();
		pair.client.send_stream(id).finish().unwrap();
		let stream = record(&mut pair.client, pair.now);

		let id = pair.client.streams().open(Dir::Uni, send::Options::default()).unwrap();
		pair.client.send_stream(id).write(&mut &b"x"[..]).unwrap();
		pair.client.send_stream(id).reset(VarInt::from_u32(5)).unwrap();
		let reset = record(&mut pair.client, pair.now);

		pair.client.close(VarInt::from_u32(42), "bye".to_string());
		let close = record(&mut pair.client, pair.now);

		// STREAM(OFF=0, LEN, FIN) id 2 "hi"; RESET_STREAM id 6 code 5 final size 1;
		// CONNECTION_CLOSE(application) code 42 "bye"
		assert_eq!(
			[stream, reset, close],
			["0b02026869", "04060501", "1d2a03627965"],
			"{version:?}"
		);
	}
}

/// The client's STREAM bytes in the order its records carry them, as `(stream, bytes)` runs
fn stream_order(pair: &mut Pair) -> Vec<(u64, usize)> {
	let mut order: Vec<(u64, usize)> = Vec::new();
	while let Some(record) = pair.client.poll_transmit(pair.now) {
		let mut rest = record.clone();
		while let Some(frame) = wire::next_frame(&mut rest).unwrap() {
			let (wire::Frame::Quic(frame::Frame::Stream(stream)) | wire::Frame::Continue(stream)) = frame else {
				continue;
			};
			let id = u64::from(stream.id);
			match order.last_mut() {
				Some((last, len)) if *last == id => *len += stream.data.len(),
				_ => order.push((id, stream.data.len())),
			}
		}
		pair.server.handle_record(record, pair.now);
	}
	order
}

/// Open a client uni stream with `options` and queue `len` bytes and a FIN on it
fn queue(pair: &mut Pair, options: send::Options<'_>, len: usize) -> u64 {
	let id = pair.client.streams().open(Dir::Uni, options).unwrap();
	assert_eq!(pair.client.send_stream(id).write(&mut &vec![0; len][..]).unwrap(), len);
	pair.client.send_stream(id).finish().unwrap();
	u64::from(id)
}

/// The record writer is QUIC's scheduler: a higher priority preempts a lower one.
#[test]
fn records_follow_strict_priority() {
	let mut pair = Pair::new(config(Version::QMux02));
	let low = queue(
		&mut pair,
		send::Options {
			group: None,
			priority: 0,
		},
		30_000,
	);
	let high = queue(
		&mut pair,
		send::Options {
			group: None,
			priority: 1,
		},
		30_000,
	);
	assert_eq!(stream_order(&mut pair), [(high, 30_000), (low, 30_000)]);
}

/// Streams of equal priority share the records instead of one draining first.
#[test]
fn records_share_equal_priority() {
	let mut pair = Pair::new(config(Version::QMux02));
	let a = queue(&mut pair, send::Options::default(), 30_000);
	let b = queue(&mut pair, send::Options::default(), 30_000);
	let order = stream_order(&mut pair);
	assert!(order.len() >= 4, "{order:?}");
	for (id, _) in &order {
		assert!([a, b].contains(id));
	}
	for stream in [a, b] {
		let sent: usize = order.iter().filter(|(id, _)| *id == stream).map(|(_, len)| len).sum();
		assert_eq!(sent, 30_000);
	}
}

/// Ungrouped streams go before every send group, and groups order by their own priority.
#[test]
fn records_order_send_groups() {
	let mut pair = Pair::new(config(Version::QMux02));
	let low = pair.client.send_groups().open(0);
	let high = pair.client.send_groups().open(1);
	let in_low = queue(
		&mut pair,
		send::Options {
			group: Some(&low),
			priority: 0,
		},
		20_000,
	);
	let in_high = queue(
		&mut pair,
		send::Options {
			group: Some(&high),
			priority: 0,
		},
		20_000,
	);
	let ungrouped = queue(&mut pair, send::Options::default(), 20_000);
	assert_eq!(
		stream_order(&mut pair),
		[(ungrouped, 20_000), (in_high, 20_000), (in_low, 20_000)]
	);

	// Raising a group's priority reorders what it has left to send.
	let first = queue(
		&mut pair,
		send::Options {
			group: Some(&high),
			priority: 0,
		},
		20_000,
	);
	let second = queue(
		&mut pair,
		send::Options {
			group: Some(&low),
			priority: 0,
		},
		20_000,
	);
	pair.client.send_groups().set_priority(&low, 2);
	assert_eq!(stream_order(&mut pair), [(second, 20_000), (first, 20_000)]);
}

/// The waits work over QMux as over QUIC: arrivals wake them, and a close wakes them to drain
/// what arrived and then fail
#[test]
fn waits_drain_then_fail() {
	use std::task::Poll;

	use crate::connection::waits::Counter;

	let mut pair = Pair::new(config(Version::QMux02));
	let (accept, closed) = (Counter::new(), Counter::new());
	assert_eq!(pair.server.poll_connected(&accept), Poll::Ready(Ok(())));
	assert!(pair.server.streams().poll_accept(&accept, Dir::Uni).is_pending());
	assert!(pair.server.poll_closed(&closed).is_pending());

	let id = pair.client.streams().open(Dir::Uni, send::Options::default()).unwrap();
	pair.client.send_stream(id).write(&mut &b"a"[..]).unwrap();
	pair.drive();
	assert_eq!(accept.wakes(), 1);

	pair.client.close(VarInt::from_u32(42), "bye".to_string());
	pair.drive();
	assert_eq!(closed.wakes(), 1);
	let reason = match pair.server.poll_closed(&closed) {
		Poll::Ready(reason) => reason,
		Poll::Pending => panic!("still open"),
	};
	assert_eq!(
		pair.server.streams().poll_accept(&accept, Dir::Uni),
		Poll::Ready(Ok(id))
	);
	assert_eq!(
		pair.server.streams().poll_accept(&accept, Dir::Uni),
		Poll::Ready(Err(reason))
	);
	let mut recv = pair.server.recv_stream(id);
	let mut chunks = recv.read().unwrap();
	assert!(matches!(
		chunks.poll_next(&accept, usize::MAX),
		Poll::Ready(Ok(Some(_)))
	));
	assert_eq!(
		chunks.poll_next(&accept, usize::MAX),
		Poll::Ready(Err(recv::Error::ConnectionLost))
	);
}
