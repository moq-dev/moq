//! Session tests over an in-memory byte stream, on paused tokio time.

use std::time::Duration;

use bytes::{Bytes, BytesMut};
use moq_net::transport::poll::{RecvStream as _, SendOptions, SendStream as _, Session as _};
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
use tokio::sync::watch;

use super::*;
use crate::transport::Stream;
use crate::{Protocol, Version};

/// Room in each direction of the in-memory pipe.
const PIPE: usize = 64 * 1024;

fn config(version: Version) -> Config {
	Config::new(version)
}

/// Start a client and a server over `client`/`server`, waiting for both handshakes.
async fn establish(
	client: DuplexStream,
	server: DuplexStream,
	client_config: Config,
	server_config: Config,
) -> (Session, Session) {
	let client = Session::connect(Stream::new(client), client_config);
	let server = Session::accept(Stream::new(server), server_config);
	let (client, server) = tokio::join!(client, server);
	(client.expect("client handshake"), server.expect("server handshake"))
}

async fn pair(client_config: Config, server_config: Config) -> (Session, Session) {
	let (client, server) = tokio::io::duplex(PIPE);
	establish(client, server, client_config, server_config).await
}

/// What a [`link`] does with the bytes in one direction.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Flow {
	/// Pass them on.
	Open,
	/// Stop reading, so the sender's writes back up.
	Stall,
	/// Read and discard them, like a dead peer.
	Drop,
}

/// Two pipes joined by a relay whose flow each direction a test can change.
fn link() -> (DuplexStream, DuplexStream, watch::Sender<Flow>, watch::Sender<Flow>) {
	let (client, client_relay) = tokio::io::duplex(PIPE);
	let (server_relay, server) = tokio::io::duplex(PIPE);
	let (up, up_rx) = watch::channel(Flow::Open);
	let (down, down_rx) = watch::channel(Flow::Open);
	let (client_read, client_write) = tokio::io::split(client_relay);
	let (server_read, server_write) = tokio::io::split(server_relay);
	tokio::spawn(relay(client_read, server_write, up_rx));
	tokio::spawn(relay(server_read, client_write, down_rx));
	(client, server, up, down)
}

async fn relay(
	mut from: tokio::io::ReadHalf<DuplexStream>,
	mut to: tokio::io::WriteHalf<DuplexStream>,
	mut flow: watch::Receiver<Flow>,
) {
	let mut buf = vec![0; 4096];
	loop {
		let mode = *flow.borrow_and_update();
		if mode == Flow::Stall {
			if flow.changed().await.is_err() {
				return;
			}
			continue;
		}
		let n = tokio::select! {
			n = from.read(&mut buf) => n,
			_ = flow.changed() => continue,
		};
		let Ok(n @ 1..) = n else {
			let _ = to.shutdown().await;
			return;
		};
		if mode == Flow::Open && to.write_all(&buf[..n]).await.is_err() {
			return;
		}
	}
}

fn payload(len: usize, seed: usize) -> Bytes {
	(0..len).map(|i| ((i * 31 + seed) % 251) as u8).collect()
}

async fn write_all(send: &mut SendStream, mut data: Bytes) {
	while !data.is_empty() {
		send.write_buf(&mut data).await.expect("write");
	}
}

async fn read_all(recv: &mut RecvStream) -> Result<Bytes, Error> {
	let mut buf = BytesMut::new();
	while let Some(chunk) = recv.read_chunk(usize::MAX).await? {
		buf.extend_from_slice(&chunk);
	}
	Ok(buf.freeze())
}

#[tokio::test(start_paused = true)]
async fn every_version_establishes_and_negotiates_in_band() {
	for &version in Version::ALL {
		let mut client = config(version);
		client.protocol = Protocol::Negotiate(vec!["moq-lite-04".into(), "moq-lite-03".into()]);
		let mut server = config(version);
		server.protocol = Protocol::Negotiate(vec!["moq-lite-03".into()]);

		let (client, server) = pair(client, server).await;
		assert_eq!(client.protocol(), Some("moq-lite-03"), "{version:?}");
		assert_eq!(server.protocol(), Some("moq-lite-03"), "{version:?}");
		assert_eq!(client.version(), version);
	}
}

/// A transfer many times the receive windows completes only if reading returns credit:
/// MAX_STREAM_DATA for the stream and MAX_DATA for the connection.
#[tokio::test(start_paused = true)]
async fn reading_returns_flow_control_credit() {
	for &version in Version::ALL {
		let mut small = config(version);
		small.limits.receive_window = std::num::NonZeroU64::new(64 * 1024);
		small.limits.stream_receive_window = std::num::NonZeroU64::new(16 * 1024);
		let (client, server) = pair(config(version), small).await;

		let data = payload(1 << 20, 3);
		let mut send = client.open_uni(SendOptions::default()).await.unwrap();
		let writer = {
			let data = data.clone();
			tokio::spawn(async move {
				write_all(&mut send, data).await;
				send.finish().unwrap();
				send.closed().await
			})
		};

		let mut recv = server.accept_uni().await.unwrap();
		assert_eq!(read_all(&mut recv).await.unwrap(), data, "{version:?}");
		writer.await.unwrap().expect("the FIN was delivered");
	}
}

#[tokio::test(start_paused = true)]
async fn bidirectional_streams_carry_both_halves_and_fins() {
	for &version in Version::ALL {
		let (client, server) = pair(config(version), config(version)).await;

		let (mut send, mut recv) = client.open_bi(SendOptions::default()).await.unwrap();
		write_all(&mut send, Bytes::from_static(b"ping")).await;
		send.finish().unwrap();

		let (mut reply, mut request) = server.accept_bi().await.unwrap();
		assert_eq!(read_all(&mut request).await.unwrap(), "ping");
		write_all(&mut reply, Bytes::from_static(b"pong")).await;
		reply.finish().unwrap();

		assert_eq!(read_all(&mut recv).await.unwrap(), "pong");
		send.closed().await.unwrap();
		reply.closed().await.unwrap();
	}
}

#[tokio::test(start_paused = true)]
async fn resets_and_stops_carry_their_codes() {
	let (client, server) = pair(config(Version::QMux02), config(Version::QMux02)).await;

	let mut send = client.open_uni(SendOptions::default()).await.unwrap();
	write_all(&mut send, Bytes::from_static(b"partial")).await;
	send.reset(7);
	let mut recv = server.accept_uni().await.unwrap();
	assert!(matches!(read_all(&mut recv).await, Err(Error::StreamReset(7))));
	// The reset sticks: it never turns into a clean end
	assert!(matches!(recv.read_chunk(usize::MAX).await, Err(Error::StreamReset(7))));

	let mut send = client.open_uni(SendOptions::default()).await.unwrap();
	write_all(&mut send, Bytes::from_static(b"unwanted")).await;
	let mut recv = server.accept_uni().await.unwrap();
	recv.stop(9);
	assert!(matches!(send.closed().await, Err(Error::StreamStop(9))));
	assert!(matches!(send.write(b"more").await, Err(Error::StreamStop(9))));

	// A stop that fails `finish` still reads as a stop, not a clean end
	let mut send = client.open_uni(SendOptions::default()).await.unwrap();
	write_all(&mut send, Bytes::from_static(b"refused")).await;
	let mut recv = server.accept_uni().await.unwrap();
	recv.stop(11);
	moq_clock::sleep(Duration::from_millis(1)).await;
	assert!(matches!(send.finish(), Err(Error::StreamStop(11))));
	assert!(matches!(send.closed().await, Err(Error::StreamStop(11))));
}

/// A send stream dropped before `finish` resets with code 0, so its reader learns it ended.
#[tokio::test(start_paused = true)]
async fn dropping_an_unfinished_send_resets_it() {
	let (client, server) = pair(config(Version::QMux02), config(Version::QMux02)).await;

	let mut send = client.open_uni(SendOptions::default()).await.unwrap();
	write_all(&mut send, Bytes::from_static(b"partial")).await;
	drop(send);
	let mut recv = server.accept_uni().await.unwrap();
	let read = moq_clock::timeout(Duration::from_secs(5), read_all(&mut recv)).await;
	assert!(matches!(read, Ok(Err(Error::StreamReset(0)))), "{read:?}");
}

#[tokio::test(start_paused = true)]
async fn the_peer_limits_concurrent_streams() {
	let mut server = config(Version::QMux02);
	server.limits.max_streams = std::num::NonZeroU64::new(1);
	let (client, server) = pair(config(Version::QMux02), server).await;

	let mut first = client.open_uni(SendOptions::default()).await.unwrap();
	let mut opening = std::pin::pin!(client.open_uni(SendOptions::default()));
	assert!(
		futures_poll_once(&mut opening).await.is_none(),
		"no credit for a second stream"
	);

	first.finish().unwrap();
	let mut recv = server.accept_uni().await.unwrap();
	read_all(&mut recv).await.unwrap();
	drop(recv);
	opening.await.expect("credit returned once the first stream is done");
}

#[tokio::test(start_paused = true)]
async fn request_credit_is_refused_without_parking_an_open() {
	let mut server_config = config(Version::QMux02);
	server_config.limits.max_streams = std::num::NonZeroU64::new(1);
	let (client, server) = pair(config(Version::QMux02), server_config).await;
	let waiter = &kio::Waiter::noop();
	let Poll::Ready(Ok(Some((mut send, mut recv)))) = client.poll_open_request(waiter, Default::default()) else {
		panic!("initial request has credit");
	};
	assert!(matches!(
		client.poll_open_request(waiter, Default::default()),
		Poll::Ready(Ok(None))
	));
	assert!(matches!(
		client.poll_open_request(waiter, Default::default()),
		Poll::Ready(Ok(None))
	));
	send.finish().unwrap();
	let (mut peer_send, mut peer_recv) = server.accept_bi().await.unwrap();
	peer_send.finish().unwrap();
	read_all(&mut peer_recv).await.unwrap();
	read_all(&mut recv).await.unwrap();
	drop((send, recv, peer_send, peer_recv));
	client
		.open_bi(Default::default())
		.await
		.expect("only one fresh open consumes returned credit");
	assert!(matches!(
		client.poll_open_request(waiter, Default::default()),
		Poll::Ready(Ok(None))
	));
}

#[tokio::test(start_paused = true)]
async fn a_request_before_the_handshake_waits_for_credit() {
	let (client, server) = tokio::io::duplex(PIPE);
	// Started like a WebSocket client, without waiting for the server's parameters.
	let client = Session::new(Stream::new(client), Side::Client, config(Version::QMux02)).unwrap();
	let mut opening = std::pin::pin!(kio::wait(|waiter| client.poll_open_request(waiter, Default::default())));
	assert!(
		futures_poll_once(&mut opening).await.is_none(),
		"no credit is known before the handshake"
	);

	let _server = Session::accept(Stream::new(server), config(Version::QMux02))
		.await
		.unwrap();
	let opened = opening.await.expect("request after the handshake");
	assert!(opened.is_some(), "the server's initial credit admits the request");
}

/// Poll `future` once, after letting every other task run.
async fn futures_poll_once<F: std::future::Future + Unpin>(future: &mut F) -> Option<F::Output> {
	moq_clock::sleep(Duration::from_millis(10)).await;
	std::future::poll_fn(|cx| {
		Poll::Ready(match Pin::new(&mut *future).poll(cx) {
			Poll::Ready(output) => Some(output),
			Poll::Pending => None,
		})
	})
	.await
}

#[tokio::test(start_paused = true)]
async fn datagrams_need_records() {
	let (client, server) = pair(config(Version::QMux02), config(Version::QMux02)).await;
	assert!(client.max_datagram_size() > 0);
	client
		.send_datagram(Bytes::from_static(b"hello"), SendOptions::default())
		.unwrap();
	assert_eq!(server.recv_datagram().await.unwrap(), "hello");

	let (client, _server) = pair(config(Version::QMux00), config(Version::QMux00)).await;
	assert_eq!(client.max_datagram_size(), 0);
	assert!(matches!(
		client.send_datagram(Bytes::from_static(b"hello"), SendOptions::default()),
		Err(Error::DatagramsUnsupported)
	));
}

/// A keep-alive that would expire before the first Pong could return is refused.
#[cfg(feature = "ws")]
#[tokio::test]
async fn websocket_keep_alive_needs_room_for_a_pong() {
	use tokio_tungstenite::{WebSocketStream, tungstenite::protocol::Role};

	use crate::transport::WsTransport;
	use crate::ws::KeepAlive;

	for (interval, timeout) in [(0, 30), (5, 5), (5, 1)] {
		let (socket, _peer) = tokio::io::duplex(PIPE);
		let ws = WebSocketStream::from_raw_socket(socket, Role::Client, None).await;
		let keep_alive = KeepAlive::new(Duration::from_secs(interval), Duration::from_secs(timeout));
		let result = WsTransport::new(ws, Address::Uri).with_keep_alive(keep_alive);
		assert!(
			matches!(result, Err(Error::InvalidConfig(_))),
			"{interval}s / {timeout}s"
		);
	}
}

/// A sender that never runs out of output still sends its WebSocket keep-alive Pings, so
/// a peer that has no keep-alive and sends nothing back does not let it time out.
#[cfg(feature = "ws")]
#[tokio::test(start_paused = true)]
async fn websocket_keep_alive_runs_during_continuous_output() {
	use tokio_tungstenite::{WebSocketStream, tungstenite::protocol::Role};

	use crate::transport::WsTransport;
	use crate::ws::KeepAlive;

	// The sender's direction drains at 4 KiB per millisecond, far below what it queues
	let (a, a_relay) = tokio::io::duplex(PIPE);
	let (b_relay, b) = tokio::io::duplex(PIPE);
	let (mut a_read, mut a_write) = tokio::io::split(a_relay);
	let (mut b_read, mut b_write) = tokio::io::split(b_relay);
	tokio::spawn(async move {
		let mut buf = vec![0; 4096];
		while let Ok(n @ 1..) = a_read.read(&mut buf).await {
			if b_write.write_all(&buf[..n]).await.is_err() {
				return;
			}
			moq_clock::sleep(Duration::from_millis(1)).await;
		}
	});
	tokio::spawn(async move { tokio::io::copy(&mut b_read, &mut a_write).await });

	let a = WebSocketStream::from_raw_socket(a, Role::Client, None).await;
	let b = WebSocketStream::from_raw_socket(b, Role::Server, None).await;
	// No QX_PING from either side, so only the WebSocket Pings keep the sender alive
	let mut config = config(Version::QMux02);
	config.rtt_interval = None;
	config.idle_timeout = None;
	let a = WsTransport::new(a, Address::Uri)
		.with_keep_alive(KeepAlive::new(Duration::from_millis(10), Duration::from_millis(500)))
		.unwrap();
	let b = WsTransport::new(b, Address::Uri);
	let sender = Session::new(a, Side::Client, config.clone()).unwrap();
	let _receiver = Session::new(b, Side::Server, config).unwrap();
	while sender.max_datagram_size() == 0 {
		moq_clock::sleep(Duration::from_millis(1)).await;
	}

	// Far past the 500ms deadline, queueing more each tick than one batch carries
	for _ in 0..2000 {
		for seed in 0..100 {
			sender
				.send_datagram(payload(1000, seed), SendOptions::default())
				.expect("the sender's keep-alive kept it open");
		}
		tokio::time::advance(Duration::from_millis(1)).await;
	}
}

/// An application close reaches the peer with its code and reason, and the closing side
/// reports it once the close is out.
#[tokio::test(start_paused = true)]
async fn an_application_close_is_delivered() {
	for &version in Version::ALL {
		let (client, server) = pair(config(version), config(version)).await;
		client.close(42, "bye");
		// Dropping the handle straight after must not lose the close.
		let watcher = client.clone();
		drop(client);

		for err in [server.closed().await, watcher.closed().await] {
			assert!(
				matches!(&err, Error::ConnectionClosed { code: 42, reason } if reason == "bye"),
				"{version:?}: got {err:?}"
			);
		}
	}
}

/// An unfinished read hands over what already arrived, then ends with the close instead of
/// hanging.
#[tokio::test(start_paused = true)]
async fn a_close_ends_an_unfinished_read_after_its_data() {
	let (client, server) = pair(config(Version::QMux02), config(Version::QMux02)).await;
	let mut send = client.open_uni(SendOptions::default()).await.unwrap();
	write_all(&mut send, Bytes::from_static(b"tail")).await;
	let mut recv = server.accept_uni().await.unwrap();

	client.close(42, "bye");
	assert!(matches!(
		server.closed().await,
		Error::ConnectionClosed { code: 42, .. }
	));

	let mut data = BytesMut::new();
	let err = loop {
		match recv.read_chunk(usize::MAX).await {
			Ok(Some(chunk)) => data.extend_from_slice(&chunk),
			Ok(None) => panic!("an unfinished stream never finishes"),
			Err(err) => break err,
		}
	};
	assert_eq!(&data[..], b"tail");
	assert!(matches!(err, Error::ConnectionClosed { code: 42, .. }), "got {err:?}");
	drop(send);
}

#[tokio::test(start_paused = true)]
async fn dropping_the_last_handle_closes_the_session() {
	let (client, server) = pair(config(Version::QMux01), config(Version::QMux01)).await;
	drop(client);
	let err = server.closed().await;
	assert!(matches!(err, Error::ConnectionClosed { code: 0, .. }), "got {err:?}");
}

/// A close stuck behind a peer that stopped reading gives up after [`CLOSE_TIMEOUT`]
/// rather than holding the transport open.
#[tokio::test(start_paused = true)]
async fn a_close_behind_a_stalled_peer_is_bounded() {
	let (client, server, up, _down) = link();
	let (client, _server) = establish(client, server, config(Version::QMux02), config(Version::QMux02)).await;

	up.send(Flow::Stall).unwrap();
	// Fill the pipe so the writer is mid-write when the close comes.
	let mut send = client.open_uni(SendOptions::default()).await.unwrap();
	let _ = moq_clock::timeout(Duration::from_millis(100), send.write(&payload(1 << 20, 0))).await;
	moq_clock::sleep(Duration::from_millis(100)).await;

	let start = moq_clock::now();
	client.close(5, "stalled");
	let err = client.closed().await;
	assert!(matches!(err, Error::ConnectionClosed { code: 5, .. }), "got {err:?}");
	assert!(
		start.elapsed() <= CLOSE_TIMEOUT + Duration::from_millis(10),
		"took {:?}",
		start.elapsed()
	);
}

#[tokio::test(start_paused = true)]
async fn a_silent_peer_times_out() {
	let (client, server, up, down) = link();
	let (client, server) = establish(client, server, config(Version::QMux02), config(Version::QMux02)).await;

	up.send(Flow::Drop).unwrap();
	down.send(Flow::Drop).unwrap();
	let start = moq_clock::now();
	assert!(matches!(client.closed().await, Error::IdleTimeout));
	assert!(matches!(server.closed().await, Error::IdleTimeout));
	assert!(start.elapsed() >= Duration::from_secs(30) - Duration::from_secs(1));
}

/// Pings keep a quiet but present peer alive past the idle timeout.
#[tokio::test(start_paused = true)]
async fn an_idle_session_stays_up() {
	let (client, _server) = pair(config(Version::QMux01), config(Version::QMux01)).await;
	let closed = moq_clock::timeout(Duration::from_secs(120), client.closed()).await;
	assert!(closed.is_err(), "closed with {closed:?}");
}

#[tokio::test(start_paused = true)]
async fn a_peer_that_never_sends_parameters_times_out_the_handshake() {
	let (client, _silent) = tokio::io::duplex(PIPE);
	let config = config(Version::QMux02);
	let start = moq_clock::now();
	let err = Session::connect(Stream::new(client), config).await.unwrap_err();
	assert!(matches!(err, Error::HandshakeTimeout), "got {err:?}");
	assert!(start.elapsed() >= Duration::from_secs(10));
}

/// A record over `max_record_size` closes the connection with a CONNECTION_CLOSE the
/// peer can read, rather than dropping the transport.
#[tokio::test(start_paused = true)]
async fn an_oversized_record_is_a_protocol_violation() {
	for version in [Version::QMux01, Version::QMux02] {
		for max_record_size in [16382, 32768] {
			let (client, mut raw) = tokio::io::duplex(PIPE);
			let mut config = config(version);
			config.max_record_size = max_record_size;
			let session = Session::new(Stream::new(client), Side::Client, config).unwrap();
			// A full-sized padding record proves that the configured limit, including
			// one larger than the default, is the reader's actual boundary.
			let mut record = BytesMut::new();
			VarInt::from_u64(max_record_size).unwrap().encode(&mut record);
			record.resize(record.len() + max_record_size as usize, 0);
			VarInt::from_u64(max_record_size + 1).unwrap().encode(&mut record);
			raw.write_all(&record).await.unwrap();
			let err = session.closed().await;
			assert!(matches!(err, Error::ProtocolViolation), "got {err:?}");
			assert_eq!(session.wire().unwrap().received_bytes(), max_record_size);
			let mut sent = Vec::new();
			raw.read_to_end(&mut sent).await.unwrap();
			assert!(
				sent.windows(2).any(|w| w == [0x1c, 0x0a]),
				"no CONNECTION_CLOSE in {sent:02x?}"
			);
		}
	}
}

#[tokio::test(start_paused = true)]
async fn the_wire_counters_track_both_directions() {
	let (client, server) = pair(config(Version::QMux02), config(Version::QMux02)).await;
	let mut send = client.open_uni(SendOptions::default()).await.unwrap();
	write_all(&mut send, payload(100_000, 1)).await;
	send.finish().unwrap();
	let mut recv = server.accept_uni().await.unwrap();
	read_all(&mut recv).await.unwrap();
	send.closed().await.unwrap();

	let sent = client.wire().unwrap().sent_bytes();
	let received = server.wire().unwrap().received_bytes();
	assert!(sent >= 100_000, "sent {sent}");
	assert!(received >= 100_000, "received {received}");
}

/// A local close keeps reading the peer until it finishes, since closing a TCP socket with
/// unread data can reset it and discard our CONNECTION_CLOSE.
#[tokio::test(start_paused = true)]
async fn an_application_close_drains_the_peer() {
	for &version in Version::ALL {
		let (client, mut raw) = tokio::io::duplex(64);
		let config = config(version);
		let session = Session::new(Stream::new(client), Side::Client, config).unwrap();
		session.close(42, "bye");

		let mut sent = Vec::new();
		raw.read_to_end(&mut sent).await.unwrap();
		// More than the pipe holds, so it completes only if the session is still reading.
		raw.write_all(&[0; 1024]).await.expect("session drains the peer");
		raw.shutdown().await.unwrap();
		let err = session.closed().await;
		assert!(
			matches!(err, Error::ConnectionClosed { code: 42, .. }),
			"{version:?}: got {err:?}"
		);
	}
}
