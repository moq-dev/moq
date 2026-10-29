//! Characterization tests: they pin what the connection does today, right or wrong, so a
//! rewrite that changes behavior fails here and has to accept or fix each change in its diff.
//!
//! A pinned bug lands as an `#[ignore]`d test asserting the correct behavior, naming the quest
//! that fixes it. Every pair pins its congestion controller instead of taking the harness
//! default, so a change of default does not churn the goldens.

use super::*;
use crate::coding::{BufMutExt, Codec};

mod ack_frequency;
mod endpoint;
mod frames;
mod limits;
mod paths;
mod reset;
mod schedule;

/// The transport every characterization pair runs: Cubic with deterministic packet numbers
fn transport() -> transport::Config {
	transport::Config {
		congestion: Arc::new(congestion::CubicConfig::default()),
		deterministic_packet_numbers: true,
		..transport::Config::default()
	}
}

/// A pair whose server runs [`transport`] with `configure` applied
fn pair_with(configure: impl FnOnce(&mut transport::Config)) -> Pair {
	let mut config = server_config();
	let mut transport = transport();
	configure(&mut transport);
	config.transport = transport;
	Pair::new(Default::default(), config)
}

/// A client config running [`transport`] with `configure` applied
fn client_with(configure: impl FnOnce(&mut transport::Config)) -> client::Config {
	let mut config = client_config();
	let mut transport = transport();
	configure(&mut transport);
	config.transport = transport;
	config
}

/// A connected pair whose endpoints both run [`transport`]
fn connected() -> (Pair, ConnectionHandle, ConnectionHandle) {
	connected_with(|_| {}, |_| {})
}

/// A connected pair whose server and client transports are configured separately
fn connected_with(
	server: impl FnOnce(&mut transport::Config),
	client: impl FnOnce(&mut transport::Config),
) -> (Pair, ConnectionHandle, ConnectionHandle) {
	let mut pair = pair_with(server);
	let (client_ch, server_ch) = pair.connect_with(client_with(client));
	(pair, client_ch, server_ch)
}

/// Frames encoded by hand, so tests can send what the frame encoder refuses to
#[derive(Default)]
struct Frames(Vec<u8>);

impl Frames {
	/// Appends a frame of type `ty` whose fields are all varints
	fn var(mut self, ty: FrameType, fields: &[u64]) -> Self {
		ty.encode(&mut self.0);
		for &field in fields {
			self.0.write_var(field);
		}
		self
	}

	/// Appends a STREAM frame with explicit offset and length
	fn stream(mut self, id: u64, offset: u64, data: &[u8], fin: bool) -> Self {
		// The STREAM type with its OFF and LEN bits set, plus FIN
		self.0.write_var(0x0e | fin as u64);
		for field in [id, offset, data.len() as u64] {
			self.0.write_var(field);
		}
		self.raw(data)
	}

	/// Appends raw bytes, such as the payload after a frame's length field
	fn raw(mut self, bytes: &[u8]) -> Self {
		self.0.extend_from_slice(bytes);
		self
	}
}

/// Sends `frames` from the client to the server in the client's next packet in `space`
fn client_sends(pair: &mut Pair, ch: ConnectionHandle, space: SpaceId, frames: Frames) {
	let now = pair.time;
	let packet = pair.client_conn_mut(ch).forge_packet(now, space, &frames.0);
	pair.server
		.inbound
		.push_back((pair.time, None, packet.as_slice().into()));
}

/// Sends `frames` from the server to the client in the server's next packet in `space`
fn server_sends(pair: &mut Pair, ch: ConnectionHandle, space: SpaceId, frames: Frames) {
	let now = pair.time;
	let packet = pair.server_conn_mut(ch).forge_packet(now, space, &frames.0);
	pair.client
		.inbound
		.push_back((pair.time, None, packet.as_slice().into()));
}

/// Drains `conn`'s events, returning the error it was lost with, if any
fn lost(conn: &mut Connection) -> Option<ConnectionError> {
	let mut lost = None;
	while let Some(event) = conn.poll() {
		if let Event::ConnectionLost { reason } = event {
			assert!(lost.is_none(), "lost twice: {lost:?}, then {reason:?}");
			lost = Some(reason);
		}
	}
	lost
}

/// Drains `conn`'s events, panicking unless it connected and was never lost
fn assert_connected(conn: &mut Connection) {
	let mut connected = false;
	while let Some(event) = conn.poll() {
		match event {
			Event::Connected => connected = true,
			Event::ConnectionLost { reason } => panic!("lost: {reason:?}"),
			_ => {}
		}
	}
	assert!(connected, "never connected");
}

/// The transport error code `conn` was lost with, panicking if it is still up or lost otherwise
fn lost_code(conn: &mut Connection) -> TransportErrorCode {
	match lost(conn) {
		Some(ConnectionError::TransportError(error)) => error.code,
		other => panic!("expected a transport error, got {other:?}"),
	}
}
