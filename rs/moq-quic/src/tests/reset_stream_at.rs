//! Tests for RESET_STREAM_AT, which resets a stream but still delivers its prefix

use super::*;

const ERROR: VarInt = VarInt(42);
const MSG: &[u8] = b"header and payload";
const RELIABLE: usize = 6;

/// A pair whose server does or does not advertise `reset_stream_at`
fn pair(server_reset_stream_at: bool) -> Pair {
	let mut config = server_config();
	let transport = transport::Config {
		reset_stream_at: server_reset_stream_at,
		..transport::Config::default()
	};
	config.transport = transport;
	Pair::new(Default::default(), config)
}

/// Read a server stream until it ends, returning the bytes and how it ended
fn read(pair: &mut Pair, ch: ConnectionHandle, s: StreamId) -> (Vec<u8>, Result<(), ReadError>) {
	let mut recv = pair.server_recv(ch, s);
	let mut chunks = recv.read().unwrap();
	let mut data = Vec::new();
	let end = loop {
		match chunks.next(usize::MAX) {
			Ok(Some(chunk)) => data.extend_from_slice(&chunk.bytes),
			Ok(None) => break Ok(()),
			Err(e) => break Err(e),
		}
	};
	let _ = chunks.finalize();
	(data, end)
}

/// Accept the client's next unidirectional stream on the server
fn accept(pair: &mut Pair, ch: ConnectionHandle) -> StreamId {
	pair.server_streams(ch).accept(Dir::Uni).unwrap()
}

/// Write `MSG` to a client stream, returning the offset after its first `RELIABLE` bytes
fn write(pair: &mut Pair, ch: ConnectionHandle, s: StreamId) -> u64 {
	pair.client_send(ch, s).write(&MSG[..RELIABLE]).unwrap();
	let offset = pair.client_send(ch, s).offset();
	pair.client_send(ch, s).write(&MSG[RELIABLE..]).unwrap();
	offset
}

/// Drive the pair to idle while the server reads stream `s` as its data arrives, returning the
/// bytes read and how the stream ended
///
/// A plain RESET_STREAM lets the receiver discard data it has not read yet, so only an
/// application that keeps reading sees the prefix a peer without RESET_STREAM_AT is sent.
fn drive_reading(pair: &mut Pair, ch: ConnectionHandle, s: StreamId) -> (Vec<u8>, Result<(), ReadError>) {
	let mut accepted = false;
	let mut data = Vec::new();
	loop {
		let busy = pair.step();
		accepted = accepted || pair.server_streams(ch).accept(Dir::Uni) == Some(s);
		if accepted {
			let (read, end) = read(pair, ch, s);
			data.extend(read);
			if end != Err(ReadError::Blocked) {
				return (data, end);
			}
		}
		if !busy {
			return (data, Err(ReadError::Blocked));
		}
	}
}

/// How many RESET_STREAM or RESET_STREAM_AT frames the client has sent
fn resets_sent(pair: &mut Pair, ch: ConnectionHandle) -> u64 {
	pair.client_conn_mut(ch).stats().frame_tx.reset_stream
}

/// Transmit the client's pending packets and drop them
fn lose_client_packets(pair: &mut Pair) {
	pair.client.drive(pair.time, pair.server.addr);
	assert!(!pair.client.outbound.is_empty());
	pair.client.outbound.clear();
}

/// A peer that stops reading gets a zero-sized reset, and the prefix it will not read is never
/// retransmitted.
fn stop_sending_abandons_prefix(reset_stream_at: bool) {
	let _guard = subscribe();
	let mut pair = pair(reset_stream_at);
	let (client_ch, server_ch) = pair.connect();

	// The server learns of the stream from its first byte
	let s = pair
		.client_streams(client_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();
	pair.client_send(client_ch, s).write(&MSG[..1]).unwrap();
	pair.drive();
	assert_eq!(accept(&mut pair, server_ch), s);

	// The rest of the prefix is lost, and so is any reset sent for it
	pair.client_send(client_ch, s).write(&MSG[1..RELIABLE]).unwrap();
	let offset = pair.client_send(client_ch, s).offset();
	pair.client_send(client_ch, s).write(&MSG[RELIABLE..]).unwrap();
	lose_client_packets(&mut pair);
	pair.client_send(client_ch, s).reset_at(offset, ERROR).unwrap();
	pair.client.drive(pair.time, pair.server.addr);
	pair.client.outbound.clear();
	let resets = resets_sent(&mut pair, client_ch);

	pair.server_recv(server_ch, s).stop(VarInt(7)).unwrap();
	let streams = pair.client_conn_mut(client_ch).stats().frame_tx.stream;
	pair.drive();

	assert_eq!(pair.client_conn_mut(client_ch).stats().frame_tx.stream, streams);
	assert!(resets_sent(&mut pair, client_ch) > resets);
	assert_eq!(pair.client_streams(client_ch).send_streams(), 0);
}

#[test]
fn stop_sending_abandons_prefix_supported() {
	stop_sending_abandons_prefix(true);
}

#[test]
fn stop_sending_abandons_prefix_unsupported() {
	stop_sending_abandons_prefix(false);
}

#[test]
fn delivers_prefix_before_error() {
	let _guard = subscribe();
	let mut pair = pair(true);
	let (client_ch, server_ch) = pair.connect();

	let s = pair
		.client_streams(client_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();
	let offset = write(&mut pair, client_ch, s);
	pair.client_send(client_ch, s).reset_at(offset, ERROR).unwrap();
	pair.drive();

	assert_eq!(accept(&mut pair, server_ch), s);
	assert_eq!(
		read(&mut pair, server_ch, s),
		(MSG[..RELIABLE].to_vec(), Err(ReadError::Reset(ERROR)))
	);
	// Both halves let go of the stream once the prefix and the reset are through
	assert_eq!(pair.client_streams(client_ch).send_streams(), 0);
	assert!(matches!(
		pair.server_recv(server_ch, s).read(),
		Err(ReadableError::ClosedStream)
	));
}

/// A stream reset before any of its prefix was sent, as a WebTransport stream that only wrote its
/// header, still reaches the peer.
#[test]
fn delivers_unsent_prefix() {
	let _guard = subscribe();
	let mut pair = pair(true);
	let (client_ch, server_ch) = pair.connect();

	let s = pair
		.client_streams(client_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();
	pair.client_send(client_ch, s).write(&MSG[..RELIABLE]).unwrap();
	let offset = pair.client_send(client_ch, s).offset();
	pair.client_send(client_ch, s).reset_at(offset, ERROR).unwrap();
	pair.drive();

	assert_eq!(accept(&mut pair, server_ch), s);
	assert_eq!(
		read(&mut pair, server_ch, s),
		(MSG[..RELIABLE].to_vec(), Err(ReadError::Reset(ERROR)))
	);
}

#[test]
fn retransmits_lost_prefix() {
	let _guard = subscribe();
	let mut pair = pair(true);
	let (client_ch, server_ch) = pair.connect();

	let s = pair
		.client_streams(client_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();
	let offset = write(&mut pair, client_ch, s);
	lose_client_packets(&mut pair);
	pair.client_send(client_ch, s).reset_at(offset, ERROR).unwrap();

	// The reset arrives first, and withholds the error until the prefix is retransmitted
	pair.drive_client();
	pair.drive_server();
	assert_eq!(accept(&mut pair, server_ch), s);
	assert_eq!(read(&mut pair, server_ch, s), (Vec::new(), Err(ReadError::Blocked)));
	assert_matches!(
		pair.server_conn_mut(server_ch).recv_stream(s).received_reset(),
		Ok(None)
	);

	pair.drive();
	assert_eq!(
		read(&mut pair, server_ch, s),
		(MSG[..RELIABLE].to_vec(), Err(ReadError::Reset(ERROR)))
	);
	assert_eq!(pair.client_streams(client_ch).send_streams(), 0);
}

#[test]
fn retransmits_lost_reset() {
	let _guard = subscribe();
	let mut pair = pair(true);
	let (client_ch, server_ch) = pair.connect();

	let s = pair
		.client_streams(client_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();
	let offset = write(&mut pair, client_ch, s);
	pair.drive();
	pair.client_send(client_ch, s).reset_at(offset, ERROR).unwrap();
	lose_client_packets(&mut pair);
	pair.drive();

	assert_eq!(accept(&mut pair, server_ch), s);
	assert_eq!(
		read(&mut pair, server_ch, s),
		(MSG[..RELIABLE].to_vec(), Err(ReadError::Reset(ERROR)))
	);
	assert_eq!(pair.client_streams(client_ch).send_streams(), 0);
}

/// Once the prefix has been read, the reset is observable without another read.
#[test]
fn received_reset_after_prefix() {
	let _guard = subscribe();
	let mut pair = pair(true);
	let (client_ch, server_ch) = pair.connect();

	let s = pair
		.client_streams(client_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();
	let offset = write(&mut pair, client_ch, s);
	pair.client_send(client_ch, s).reset_at(offset, ERROR).unwrap();
	pair.drive();

	assert_eq!(accept(&mut pair, server_ch), s);
	assert_matches!(
		pair.server_conn_mut(server_ch).recv_stream(s).received_reset(),
		Ok(None)
	);
	let mut recv = pair.server_recv(server_ch, s);
	let mut chunks = recv.read().unwrap();
	assert_matches!(chunks.next(RELIABLE), Ok(Some(chunk)) if chunk.bytes == MSG[..RELIABLE]);
	let _ = chunks.finalize();
	assert_matches!(
		pair.server_conn_mut(server_ch).recv_stream(s).received_reset(),
		Ok(Some(ERROR))
	);
}

#[test]
fn after_finish() {
	let _guard = subscribe();
	let mut pair = pair(true);
	let (client_ch, server_ch) = pair.connect();

	for reliable in [RELIABLE, MSG.len()] {
		let s = pair
			.client_streams(client_ch)
			.open(Dir::Uni, Default::default())
			.unwrap();
		let mut offset = write(&mut pair, client_ch, s);
		if reliable == MSG.len() {
			offset = pair.client_send(client_ch, s).offset();
		}
		pair.client_send(client_ch, s).finish().unwrap();
		pair.client_send(client_ch, s).reset_at(offset, ERROR).unwrap();
		pair.drive();

		assert_eq!(accept(&mut pair, server_ch), s);
		assert_eq!(
			read(&mut pair, server_ch, s),
			(MSG[..reliable].to_vec(), Err(ReadError::Reset(ERROR)))
		);
	}
	assert_eq!(pair.client_streams(client_ch).send_streams(), 0);
}

/// The Final Size never exceeds the flow control the writes were held to.
#[test]
fn flow_control_blocked() {
	let _guard = subscribe();
	let mut config = server_config();
	let mut transport = transport::Config::default();
	transport.limits.stream_receive_window = NonZeroU64::new(10);
	config.transport = transport;
	let mut pair = Pair::new(Default::default(), config);
	let (client_ch, server_ch) = pair.connect();

	let s = pair
		.client_streams(client_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();
	pair.client_send(client_ch, s).write(&MSG[..RELIABLE]).unwrap();
	let offset = pair.client_send(client_ch, s).offset();
	assert_eq!(
		pair.client_send(client_ch, s).write(&MSG[RELIABLE..]),
		Ok(10 - RELIABLE)
	);
	assert_eq!(pair.client_send(client_ch, s).write(MSG), Err(WriteError::Blocked));
	pair.client_send(client_ch, s).reset_at(offset, ERROR).unwrap();
	pair.drive();

	assert_eq!(accept(&mut pair, server_ch), s);
	assert_eq!(
		read(&mut pair, server_ch, s),
		(MSG[..RELIABLE].to_vec(), Err(ReadError::Reset(ERROR)))
	);
	assert!(!pair.server_conn_mut(server_ch).is_closed());
}

/// Without RESET_STREAM_AT, the plain RESET_STREAM waits until the peer has acknowledged the data
/// before the offset, so the peer reads that data before the error.
#[test]
fn unsupported_holds_reset_until_acked() {
	let _guard = subscribe();
	let mut pair = pair(false);
	let (client_ch, server_ch) = pair.connect();

	let s = pair
		.client_streams(client_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();
	let offset = write(&mut pair, client_ch, s);
	pair.client_send(client_ch, s).reset_at(offset, ERROR).unwrap();

	// The first flight carries only the data before the offset
	pair.drive_client();
	assert_eq!(resets_sent(&mut pair, client_ch), 0);
	assert_eq!(
		drive_reading(&mut pair, server_ch, s),
		(MSG[..RELIABLE].to_vec(), Err(ReadError::Reset(ERROR)))
	);
	assert_eq!(resets_sent(&mut pair, client_ch), 1);
	pair.drive();
	assert_eq!(pair.client_streams(client_ch).send_streams(), 0);
}

#[test]
fn unsupported_retransmits_lost_prefix() {
	let _guard = subscribe();
	let mut pair = pair(false);
	let (client_ch, server_ch) = pair.connect();

	let s = pair
		.client_streams(client_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();
	let offset = write(&mut pair, client_ch, s);
	lose_client_packets(&mut pair);
	pair.client_send(client_ch, s).reset_at(offset, ERROR).unwrap();

	assert_eq!(
		drive_reading(&mut pair, server_ch, s),
		(MSG[..RELIABLE].to_vec(), Err(ReadError::Reset(ERROR)))
	);
	assert_eq!(resets_sent(&mut pair, client_ch), 1);
	pair.drive();
	assert_eq!(pair.client_streams(client_ch).send_streams(), 0);
}

/// Data the peer already acknowledged needs no wait.
#[test]
fn unsupported_resets_at_once_when_acked() {
	let _guard = subscribe();
	let mut pair = pair(false);
	let (client_ch, server_ch) = pair.connect();

	let s = pair
		.client_streams(client_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();
	let offset = write(&mut pair, client_ch, s);
	pair.drive();
	assert_eq!(accept(&mut pair, server_ch), s);
	assert_eq!(read(&mut pair, server_ch, s), (MSG.to_vec(), Err(ReadError::Blocked)));

	pair.client_send(client_ch, s).reset_at(offset, ERROR).unwrap();
	pair.drive_client();
	assert_eq!(resets_sent(&mut pair, client_ch), 1);
	pair.drive();
	assert_eq!(
		read(&mut pair, server_ch, s),
		(Vec::new(), Err(ReadError::Reset(ERROR)))
	);
}

/// A finished stream reset while its reset is held ends in the reset, never as finished.
#[test]
fn unsupported_after_finish() {
	let _guard = subscribe();
	let mut pair = pair(false);
	let (client_ch, server_ch) = pair.connect();

	let s = pair
		.client_streams(client_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();
	let offset = write(&mut pair, client_ch, s);
	pair.client_send(client_ch, s).finish().unwrap();
	pair.client_send(client_ch, s).reset_at(offset, ERROR).unwrap();
	assert_eq!(pair.client_send(client_ch, s).finish(), Err(FinishError::ClosedStream));
	assert_eq!(pair.client_send(client_ch, s).write(MSG), Err(WriteError::ClosedStream));

	assert_eq!(
		drive_reading(&mut pair, server_ch, s),
		(MSG[..RELIABLE].to_vec(), Err(ReadError::Reset(ERROR)))
	);
	pair.drive();
	assert_eq!(pair.client_streams(client_ch).send_streams(), 0);
	while let Some(event) = pair.client_conn_mut(client_ch).poll() {
		assert!(!matches!(event, Event::Stream(StreamEvent::Finished { .. })));
	}
}

#[test]
fn unsupported_repeated_reset_is_closed() {
	let _guard = subscribe();
	let mut pair = pair(false);
	let (client_ch, _) = pair.connect();

	let s = pair
		.client_streams(client_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();
	let offset = write(&mut pair, client_ch, s);
	let again = pair.client_send(client_ch, s).offset();
	lose_client_packets(&mut pair);
	pair.client_send(client_ch, s).reset_at(offset, ERROR).unwrap();
	assert_eq!(
		pair.client_send(client_ch, s).reset_at(again, ERROR),
		Err(ClosedStream::default())
	);
	assert!(pair.client_send(client_ch, s).reset(ERROR).is_err());
}

/// A connection closed while a reset is held closes cleanly, without sending it.
#[test]
fn unsupported_close_while_held() {
	let _guard = subscribe();
	let mut pair = pair(false);
	let (client_ch, server_ch) = pair.connect();

	let s = pair
		.client_streams(client_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();
	let offset = write(&mut pair, client_ch, s);
	lose_client_packets(&mut pair);
	pair.client_send(client_ch, s).reset_at(offset, ERROR).unwrap();
	let now = pair.time;
	pair.client_conn_mut(client_ch).close(now, VarInt(0), String::new());
	pair.drive();

	assert_eq!(resets_sent(&mut pair, client_ch), 0);
	assert!(pair.client_conn_mut(client_ch).is_closed());
	assert!(pair.server_conn_mut(server_ch).is_closed());
}

/// The data abandoned past the offset still counts against connection flow control until
/// the peer learns the final size, and then returns.
#[test]
fn unsupported_returns_flow_control() {
	let _guard = subscribe();
	let mut config = server_config();
	let mut transport = transport::Config {
		reset_stream_at: false,
		..transport::Config::default()
	};
	transport.limits.receive_window = NonZeroU64::new(MSG.len() as u64);
	config.transport = transport;
	let mut pair = Pair::new(Default::default(), config);
	let (client_ch, server_ch) = pair.connect();

	let s = pair
		.client_streams(client_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();
	let offset = write(&mut pair, client_ch, s);
	pair.client_send(client_ch, s).reset_at(offset, ERROR).unwrap();
	let next = pair
		.client_streams(client_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();
	assert_eq!(pair.client_send(client_ch, next).write(MSG), Err(WriteError::Blocked));

	assert_eq!(
		drive_reading(&mut pair, server_ch, s),
		(MSG[..RELIABLE].to_vec(), Err(ReadError::Reset(ERROR)))
	);
	pair.drive();
	assert_eq!(pair.client_send(client_ch, next).write(MSG), Ok(MSG.len()));
}

/// A zero Reliable Size is a plain RESET_STREAM, which needs no negotiation.
#[test]
fn unsupported_allows_zero() {
	let _guard = subscribe();
	let mut pair = pair(false);
	let (client_ch, server_ch) = pair.connect();

	let s = pair
		.client_streams(client_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();
	let offset = pair.client_send(client_ch, s).offset();
	pair.client_send(client_ch, s).write(MSG).unwrap();
	pair.client_send(client_ch, s).reset_at(offset, ERROR).unwrap();
	pair.drive();

	assert_eq!(accept(&mut pair, server_ch), s);
	assert_eq!(
		read(&mut pair, server_ch, s),
		(Vec::new(), Err(ReadError::Reset(ERROR)))
	);
}

#[test]
fn repeated_reset_is_closed() {
	let _guard = subscribe();
	let mut pair = pair(true);
	let (client_ch, _) = pair.connect();

	let s = pair
		.client_streams(client_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();
	let offset = write(&mut pair, client_ch, s);
	let again = pair.client_send(client_ch, s).offset();
	pair.client_send(client_ch, s).reset_at(offset, ERROR).unwrap();
	assert_eq!(
		pair.client_send(client_ch, s).reset_at(again, ERROR),
		Err(ClosedStream::default())
	);
	assert!(pair.client_send(client_ch, s).reset(ERROR).is_err());
}
