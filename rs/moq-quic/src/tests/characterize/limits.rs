//! Stream and flow control limits changed while a connection is up

use std::num::NonZeroU64;

use super::*;

/// How many more uni streams the client can open right now, opening them all
fn openable(pair: &mut Pair, ch: ConnectionHandle) -> Vec<StreamId> {
	iter::from_fn(|| pair.client_streams(ch).open(Dir::Uni, Default::default())).collect()
}

/// Finishes each client stream and has the server read it to the end
fn finish_all(pair: &mut Pair, client_ch: ConnectionHandle, server_ch: ConnectionHandle, streams: &[StreamId]) {
	for &s in streams {
		pair.client_send(client_ch, s).write(b"x").unwrap();
		pair.client_send(client_ch, s).finish().unwrap();
	}
	pair.drive();
	while let Some(s) = pair.server_streams(server_ch).accept(Dir::Uni) {
		let mut recv = pair.server_recv(server_ch, s);
		let mut chunks = recv.read().unwrap();
		while chunks.next(usize::MAX).unwrap().is_some() {}
		let _ = chunks.finalize();
	}
	pair.drive();
}

#[test]
fn max_concurrent_streams_raised() {
	let _guard = subscribe();
	let (mut pair, client_ch, server_ch) = connected_with(|t| _ = t.max_concurrent_uni_streams(VarInt(1)), |_| {});
	assert_eq!(openable(&mut pair, client_ch).len(), 1);

	pair.server_conn_mut(server_ch)
		.set_max_concurrent_streams(Dir::Uni, VarInt(3));
	assert_eq!(pair.server_conn_mut(server_ch).max_concurrent_streams(Dir::Uni), 3);
	pair.drive();
	assert_eq!(openable(&mut pair, client_ch).len(), 2);
}

/// Lowering the limit keeps the credit already issued, and withholds new credit until enough
/// streams close to fall under it
#[test]
fn max_concurrent_streams_lowered() {
	let _guard = subscribe();
	let (mut pair, client_ch, server_ch) = connected_with(|t| _ = t.max_concurrent_uni_streams(VarInt(3)), |_| {});
	pair.server_conn_mut(server_ch)
		.set_max_concurrent_streams(Dir::Uni, VarInt(1));
	assert_eq!(pair.server_conn_mut(server_ch).max_concurrent_streams(Dir::Uni), 3);
	pair.drive();

	let streams = openable(&mut pair, client_ch);
	assert_eq!(streams.len(), 3);
	finish_all(&mut pair, client_ch, server_ch, &streams);
	assert_eq!(pair.server_conn_mut(server_ch).max_concurrent_streams(Dir::Uni), 1);
	assert_eq!(openable(&mut pair, client_ch).len(), 1);
}

/// How many bytes a fresh client stream can write, up to `limit`
fn writable(pair: &mut Pair, ch: ConnectionHandle, limit: usize) -> usize {
	let s = pair.client_streams(ch).open(Dir::Uni, Default::default()).unwrap();
	pair.client_send(ch, s).write(&vec![0; limit]).unwrap_or(0)
}

/// Reads everything the server has buffered on every client uni stream, returning the total
fn read_buffered(pair: &mut Pair, ch: ConnectionHandle, streams: &mut Vec<StreamId>) -> usize {
	streams.extend(iter::from_fn(|| pair.server_streams(ch).accept(Dir::Uni)));
	let mut total = 0;
	for &s in streams.iter() {
		let mut recv = pair.server_recv(ch, s);
		let Ok(mut chunks) = recv.read() else {
			continue;
		};
		while let Ok(Some(chunk)) = chunks.next(usize::MAX) {
			total += chunk.bytes.len();
		}
		let _ = chunks.finalize();
	}
	total
}

const WINDOW: u32 = 4000;

#[test]
fn receive_window_raised() {
	let _guard = subscribe();
	let (mut pair, client_ch, server_ch) = connected_with(|t| _ = t.receive_window(VarInt::from_u32(WINDOW)), |_| {});
	assert_eq!(writable(&mut pair, client_ch, 2 * WINDOW as usize), WINDOW as usize);
	pair.drive();

	pair.server_conn_mut(server_ch)
		.set_receive_window(VarInt::from_u32(2 * WINDOW));
	pair.drive();
	assert_eq!(writable(&mut pair, client_ch, 2 * WINDOW as usize), WINDOW as usize);
}

/// A lowered window is paid down from credit the reader would otherwise return
#[test]
fn receive_window_lowered() {
	let _guard = subscribe();
	let (mut pair, client_ch, server_ch) =
		connected_with(|t| _ = t.receive_window(VarInt::from_u32(2 * WINDOW)), |_| {});
	assert_eq!(writable(&mut pair, client_ch, 4 * WINDOW as usize), 2 * WINDOW as usize);
	pair.drive();

	pair.server_conn_mut(server_ch)
		.set_receive_window(VarInt::from_u32(WINDOW));
	let mut streams = Vec::new();
	assert_eq!(read_buffered(&mut pair, server_ch, &mut streams), 2 * WINDOW as usize);
	pair.drive();
	assert_eq!(writable(&mut pair, client_ch, 4 * WINDOW as usize), WINDOW as usize);
}

/// The send window bounds unacknowledged data, so lowering it blocks writes until acks drain
/// what is in flight
#[test]
fn send_window_lowered_and_raised() {
	let _guard = subscribe();
	let (mut pair, client_ch, _) = connected();
	pair.client_conn_mut(client_ch).set_send_window(1000);
	assert_eq!(writable(&mut pair, client_ch, 4000), 1000);
	assert_eq!(writable(&mut pair, client_ch, 4000), 0);

	pair.client_conn_mut(client_ch).set_send_window(3000);
	assert_eq!(writable(&mut pair, client_ch, 4000), 2000);

	// Acknowledged data frees the window again
	pair.drive();
	assert_eq!(writable(&mut pair, client_ch, 4000), 3000);
}

/// A server whose stream windows grew since the ticket the client resumes with
///
/// The client opens 0-RTT streams with the remembered, smaller windows, and the larger ones
/// arrive with the handshake. The server believes it granted the new window and never sends
/// MAX_STREAM_DATA for it, so the client's streams must adopt it or stall for good.
fn zero_rtt_window_raise(dir: Dir) {
	let _guard = subscribe();
	const OLD: u32 = 1000;
	const NEW: u32 = 100_000;
	const LEN: usize = 50_000;

	let crypto = Arc::new(server_crypto());
	let server = |window: u32| {
		let mut config = ServerConfig::with_crypto(crypto.clone());
		let mut transport = transport();
		transport.stream_receive_window(VarInt::from_u32(window));
		config.transport = Arc::new(transport);
		config
	};
	let mut pair = Pair::new(Default::default(), server(OLD));
	let config = client_with(|_| {});

	let client_ch = pair.begin_connect(config.clone());
	pair.drive();
	pair.server.assert_accept();
	let now = pair.time;
	pair.client_conn_mut(client_ch).close(now, VarInt(0), Bytes::new());
	pair.drive();

	pair.server.set_server_config(Some(Arc::new(server(NEW))));
	let client_ch = pair.begin_connect(config);
	assert!(pair.client_conn_mut(client_ch).has_0rtt());
	let s = pair.client_streams(client_ch).open(dir, Default::default()).unwrap();
	let mut written = pair.client_send(client_ch, s).write(&[0; LEN]).unwrap();
	assert_eq!(written, OLD as usize);

	pair.drive();
	assert!(pair.client_conn_mut(client_ch).accepted_0rtt());
	let server_ch = pair.server.assert_accept();
	assert_eq!(pair.server_streams(server_ch).accept(dir), Some(s));

	// Keep writing and reading until neither side makes progress
	let mut read = 0;
	loop {
		let before = (written, read);
		if let Ok(n) = pair.client_send(client_ch, s).write(&[0; LEN][written..]) {
			written += n;
		}
		let mut recv = pair.server_recv(server_ch, s);
		let mut chunks = recv.read().unwrap();
		while let Ok(Some(chunk)) = chunks.next(usize::MAX) {
			read += chunk.bytes.len();
		}
		let _ = chunks.finalize();
		pair.drive();
		if (written, read) == before {
			break;
		}
	}
	assert_eq!(read, LEN);
}

#[test]
fn zero_rtt_bidi_window_raise() {
	zero_rtt_window_raise(Dir::Bi);
}

#[test]
fn zero_rtt_uni_window_raise() {
	zero_rtt_window_raise(Dir::Uni);
}

/// `set_limits` raises every limit it sets in one call
#[test]
fn set_limits_raises_what_it_sets() {
	let _guard = subscribe();
	let (mut pair, client_ch, server_ch) = connected_with(
		|t| {
			t.max_concurrent_uni_streams(VarInt(1));
			t.receive_window(VarInt::from_u32(WINDOW));
		},
		|_| {},
	);

	let limits = Limits {
		max_streams: NonZeroU64::new(3),
		receive_window: NonZeroU64::new(2 * WINDOW as u64),
		..Default::default()
	};
	pair.server_conn_mut(server_ch).set_limits(limits).unwrap();
	pair.drive();

	// One stream to fill the wider window, then the two the raised limit leaves
	assert_eq!(writable(&mut pair, client_ch, 4 * WINDOW as usize), 2 * WINDOW as usize);
	assert_eq!(openable(&mut pair, client_ch).len(), 2);
}

/// Limits a peer would reject change nothing, rather than applying the ones that fit
#[test]
fn set_limits_refuses_without_changing_anything() {
	let _guard = subscribe();
	let (mut pair, _, server_ch) = connected_with(|t| _ = t.max_concurrent_uni_streams(VarInt(1)), |_| {});

	let limits = Limits {
		max_streams: NonZeroU64::new(3),
		receive_window: NonZeroU64::new(1 << 62),
		..Default::default()
	};
	assert_eq!(
		pair.server_conn_mut(server_ch).set_limits(limits),
		Err(LimitsError::ReceiveWindow)
	);
	assert_eq!(pair.server_conn_mut(server_ch).max_concurrent_streams(Dir::Uni), 1);
}
