//! WebTransport interop: the reference tokio stack (`web-transport-quinn`,
//! what browsers interop with) dials the uring server. One test hand-drives
//! streams and datagrams through the H3 framing; the other runs a whole
//! moq-lite session over it, which is exactly the browser-to-relay path.
//!
//! Kernel-gated: skips loudly below the Linux 6.12 floor (GitHub-hosted CI),
//! and runs everywhere else.

#![cfg(all(target_os = "linux", any(feature = "quiche", feature = "quinn")))]

#[path = "support/quiche.rs"]
mod support;

use std::net::UdpSocket;

use moq_net::origin;
use moq_uring::{Config, Error, Worker, quic, udp};
use web_transport_trait::poll::{RecvStream as _, SendStream as _, Session as _};

fn worker() -> Option<Worker> {
	match Worker::new(Config::default()) {
		Ok(worker) => Some(worker),
		Err(Error::Unsupported(reason)) => {
			eprintln!("skipping io_uring web test: {reason}");
			None
		}
		Err(err) => panic!("worker setup failed: {err}"),
	}
}

/// The moq version negotiated as a WebTransport subprotocol.
const PROTO: &str = "moq-lite-05";
const PAYLOAD: &[u8] = b"hello over webtransport";
const CLOSE_CODE: u32 = 42;
const CLOSE_REASON: &str = "bye";

/// Build the uring server endpoint serving `h3`.
fn h3_endpoint(handle: &moq_uring::Handle, certs: &support::Certs) -> quic::Endpoint {
	let mut server = quic::server::Config::new(quic::Identity::open(&certs.cert, &certs.key).expect("identity"));
	server.alpn = vec!["h3".to_string()];
	let sock = handle
		.udp(UdpSocket::bind("127.0.0.1:0").expect("bind"), udp::Config::default())
		.expect("socket");
	quic::Endpoint::new(handle, sock, quic::endpoint::Config::default().with_server(server)).expect("endpoint")
}

/// The tokio-side client, in its own runtime on its own thread.
fn quinn_client(
	url: String,
	body: impl FnOnce(web_transport_quinn::Session) -> ClientFuture + Send + 'static,
) -> std::thread::JoinHandle<()> {
	std::thread::spawn(move || {
		// May already be installed by a sibling test; either way one exists.
		let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
		let rt = tokio::runtime::Builder::new_current_thread()
			.enable_all()
			.build()
			.expect("tokio runtime");
		rt.block_on(async move {
			let client = web_transport_quinn::ClientBuilder::new()
				.dangerous()
				.with_no_certificate_verification()
				.expect("client");
			let request = web_transport_quinn::proto::ConnectRequest::new(url::Url::parse(&url).expect("url"))
				.with_protocol(PROTO);
			let session = client.connect(request).await.expect("connect");
			body(session).await;
		});
	})
}

type ClientFuture = std::pin::Pin<Box<dyn Future<Output = ()> + Send>>;

/// Read a quinn-side stream to its end.
async fn read_all(recv: &mut web_transport_quinn::RecvStream) -> Vec<u8> {
	let mut out = Vec::new();
	let mut buf = [0u8; 4096];
	while let Some(n) = recv.read(&mut buf).await.expect("read") {
		out.extend_from_slice(&buf[..n]);
	}
	out
}

/// Read a uring-side stream to its end.
async fn drain(recv: &mut quic::web::RecvStream) -> Vec<u8> {
	let mut out = Vec::new();
	let mut buf = [0u8; 4096];
	loop {
		let n = std::future::poll_fn(|cx| recv.poll_read(cx, &mut buf))
			.await
			.expect("read");
		match n {
			Some(n) => out.extend_from_slice(&buf[..n]),
			None => return out,
		}
	}
}

async fn write_finish(send: &mut quic::web::SendStream, mut buf: &[u8]) {
	while !buf.is_empty() {
		let n = std::future::poll_fn(|cx| send.poll_write(cx, buf))
			.await
			.expect("write");
		buf = &buf[n..];
	}
	send.finish().expect("finish");
}

/// Streams, datagrams, and the close code, hand-driven through the framing:
/// echoes in both directions, on both stream kinds, and a session close whose
/// code and reason survive the round trip through the HTTP/3 error mapping
/// and the `CloseWebTransportSession` capsule.
#[test]
fn webtransport_echo_end_to_end() {
	let Some(mut worker) = worker() else { return };
	let handle = worker.handle();
	let certs = support::certs().expect("certificates");
	let endpoint = h3_endpoint(&handle, &certs);
	let addr = endpoint.local_addr();

	let client = quinn_client(format!("https://{addr}/echo?token=abc"), |session| {
		Box::pin(async move {
			// Bidirectional echo.
			let (mut send, mut recv) = session.open_bi().await.expect("open_bi");
			send.write_all(PAYLOAD).await.expect("write");
			send.finish().expect("finish");
			assert_eq!(read_all(&mut recv).await, PAYLOAD, "bidi echo");

			// Unidirectional, both directions.
			let mut send = session.open_uni().await.expect("open_uni");
			send.write_all(PAYLOAD).await.expect("write");
			send.finish().expect("finish");
			let mut recv = session.accept_uni().await.expect("accept_uni");
			assert_eq!(read_all(&mut recv).await, PAYLOAD, "uni echo");

			// Datagram echo.
			session.send_datagram(PAYLOAD.to_vec().into()).expect("send datagram");
			let echoed = session.read_datagram().await.expect("read datagram");
			assert_eq!(&echoed[..], PAYLOAD, "datagram echo");

			// The close code and reason must reach the server intact.
			session.close(CLOSE_CODE, CLOSE_REASON.as_bytes());
			session.closed().await;
		})
	});

	worker
		.block_on(async move {
			let conn = endpoint.accept().await.expect("accept");
			assert_eq!(conn.protocol(), Some("h3"), "negotiated ALPN");

			let request = quic::web::Request::accept(&handle, conn).await.expect("handshake");
			assert_eq!(request.url().path(), "/echo");
			assert_eq!(request.url().query(), Some("token=abc"));
			assert_eq!(request.protocols(), [PROTO.to_string()]);
			let mut session = request
				.respond(quic::web::Response::default().with_protocol(PROTO))
				.await
				.expect("respond");
			assert_eq!(session.protocol(), Some(PROTO), "negotiated subprotocol");

			// Bidirectional echo.
			let (mut send, mut recv) = std::future::poll_fn(|cx| session.poll_accept_bi(cx))
				.await
				.expect("accept_bi");
			let payload = drain(&mut recv).await;
			write_finish(&mut send, &payload).await;

			// Unidirectional, both directions.
			let mut recv = std::future::poll_fn(|cx| session.poll_accept_uni(cx))
				.await
				.expect("accept_uni");
			let payload = drain(&mut recv).await;
			let mut send = std::future::poll_fn(|cx| session.poll_open_uni(cx))
				.await
				.expect("open_uni");
			write_finish(&mut send, &payload).await;

			// Datagram echo.
			let datagram = std::future::poll_fn(|cx| session.poll_recv_datagram(cx))
				.await
				.expect("recv datagram");
			std::future::poll_fn(|cx| session.poll_send_datagram(cx, &datagram))
				.await
				.expect("send datagram");

			// The peer's close arrives as a capsule; its code must come back
			// out of the H3 mapping as itself.
			let err = std::future::poll_fn(|cx| session.poll_closed(cx)).await;
			match err {
				quic::Error::App { code, reason } => {
					assert_eq!(code, u64::from(CLOSE_CODE), "close code");
					assert_eq!(reason, CLOSE_REASON, "close reason");
				}
				other => panic!("expected an application close, got {other:?}"),
			}
		})
		.expect("worker");

	client.join().expect("client thread");
}

/// A whole moq-lite session over WebTransport: subprotocol negotiation, SETUP
/// on the bidirectional stream, announce, subscribe, and a group on a
/// unidirectional stream, with the tokio stack as the subscriber. This is the
/// browser-to-relay path end to end.
#[test]
fn lite_session_over_webtransport() {
	let Some(mut worker) = worker() else { return };
	let handle = worker.handle();
	let certs = support::certs().expect("certificates");

	let (pub_origin, pub_driver) = origin::Producer::new(origin::Info::new(moq_net::Hop::random()));
	let origins = std::thread::spawn(move || {
		let rt = tokio::runtime::Builder::new_current_thread()
			.enable_time()
			.build()
			.expect("tokio runtime");
		rt.block_on(pub_driver.run(support::TokioTimers));
	});

	let mut broadcast = pub_origin
		.create_broadcast("test", moq_net::broadcast::Route::new().with_announce(true))
		.expect("create broadcast");
	let mut track = broadcast.create_track("data", None).expect("create track");
	let mut group = track.append_group().expect("append group");
	group
		.write_frame(moq_net::Timestamp::ZERO, PAYLOAD)
		.expect("write frame");
	group.finish().expect("finish group");

	let endpoint = h3_endpoint(&handle, &certs);
	let addr = endpoint.local_addr();

	let client = quinn_client(format!("https://{addr}/"), |session| {
		Box::pin(async move {
			assert_eq!(session.protocol(), Some(PROTO), "negotiated subprotocol");
			let (sub_origin, sub_driver) = origin::Producer::new(origin::Info::new(moq_net::Hop::random()));
			let driver = tokio::spawn(sub_driver.run(moq_tokio::runtime::Runtime::<()>::new()));

			let moq = moq_net::Client::new()
				.with_subscriber(sub_origin.clone())
				.connect_lite(moq_tokio::runtime::Runtime::new(), session)
				.await
				.expect("connect_lite");

			let bc = sub_origin
				.consume()
				.announced_broadcast("test")
				.await
				.expect("broadcast announced");
			let mut track = bc
				.track("data")
				.expect("track")
				.subscribe(None)
				.await
				.expect("subscribe");
			let mut group = track
				.recv_group()
				.await
				.expect("recv group")
				.expect("track closed prematurely");
			let frame = group.read_frame().await.expect("read frame").expect("frame");
			assert_eq!(&frame.payload[..], PAYLOAD);

			moq.abort(moq_net::Error::Cancel);
			drop(sub_origin);
			driver.await.expect("subscriber origin driver");
		})
	});

	let serve_origin = pub_origin.clone();
	worker
		.block_on(async move {
			let conn = endpoint.accept().await.expect("accept");
			let request = quic::web::Request::accept(&handle, conn).await.expect("handshake");
			// The WebTransport equivalent of ALPN: pick the moq version.
			let protocol = request.protocols().iter().find(|p| *p == PROTO).cloned();
			let mut response = quic::web::Response::default();
			if let Some(protocol) = &protocol {
				response = response.with_protocol(protocol);
			}
			let session = request.respond(response).await.expect("respond");

			let session = moq_net::Server::new()
				.with_publisher(&serve_origin)
				.accept_lite(handle.clone(), session)
				.await
				.expect("accept_lite");
			session.closed().await;
		})
		.expect("worker");

	drop(worker);
	drop(broadcast);
	drop(track);
	drop(pub_origin);
	client.join().expect("client thread");
	origins.join().expect("origin driver");
}

/// A peer that opens unidirectional streams of an unknown type and never
/// sends a control stream must not hold the handshake open forever.
///
/// Classification drops an unknown type rather than keeping it, and a
/// finished stream returns its credit, so a cap counting only the streams the
/// handshake *retains* never trips and the loop runs as long as the peer
/// cares to feed it.
#[test]
fn unknown_streams_cannot_stall_the_handshake() {
	let Some(mut worker) = worker() else { return };
	let handle = worker.handle();
	let certs = support::certs().expect("certificates");
	let endpoint = h3_endpoint(&handle, &certs);
	let server = endpoint.local_addr();

	let peer_handle = handle.clone();
	worker
		.block_on(async move {
			let sock = peer_handle
				.udp(UdpSocket::bind("127.0.0.1:0").expect("bind"), udp::Config::default())
				.expect("client socket");
			// Raw quiche, because the misbehavior here is exactly what a real
			// HTTP/3 client would refuse to produce. It drives from a task so it
			// and the handshake below make progress against each other on this
			// one worker thread.
			let task_handle = peer_handle.clone();
			peer_handle.spawn(async move {
				let mut peer = support::Peer::connect_alpn(&task_handle, sock, server, &[b"h3"]).expect("client");
				peer.flush().await.expect("first flight");
				while !peer.conn.is_established() {
					peer.step().await.expect("step");
					peer.flush().await.expect("flush");
				}

				// Comfortably past the cap, each a single junk type byte,
				// finished so its credit comes straight back.
				for i in 0..80u64 {
					let stream = 2 + i * 4; // client-initiated unidirectional
					if peer.conn.stream_send(stream, &[0x3f], true).is_err() {
						break;
					}
					peer.flush().await.expect("flush");
					peer.step().await.expect("step");
				}
			});

			let conn = endpoint.accept().await.expect("accepted connection");
			let err = quic::web::Request::accept(&handle, conn)
				.await
				.expect_err("the handshake must give up");
			assert!(
				matches!(err, quic::Error::Web(ref reason) if reason.contains("too many streams")),
				"got {err:?}"
			);
		})
		.expect("worker");
}

// ── Handshake and teardown edge cases ───────────────────────────────

/// Connection-level flow control the throttled peer advertises, which is what
/// the server runs out of below. Big enough for the handshake, small enough to
/// fill in a handful of writes.
const THROTTLE: u64 = 64 * 1024;
/// Mirrors the crate's own cap on unidirectional streams accepted before the
/// control stream arrives.
const HANDSHAKE_STREAMS: usize = 64;
/// The client's first bidirectional stream, which the CONNECT rides.
const CLIENT_BI: u64 = 0;
/// The client's first two unidirectional streams. QUIC creates lower-numbered
/// streams implicitly, so the server's accept queue hands them out in this
/// order however the packets arrive.
const CLIENT_UNI: [u64; 2] = [2, 6];

/// A client offering `alpn` on its own socket, not yet handshaken.
fn raw_peer(handle: &moq_uring::Handle, server: std::net::SocketAddr, throttle: Option<u64>) -> support::Peer {
	let sock = handle
		.udp(UdpSocket::bind("127.0.0.1:0").expect("bind"), udp::Config::default())
		.expect("client socket");
	match throttle {
		Some(max_data) => support::Peer::connect_throttled(handle, sock, server, &[b"h3"], max_data).expect("client"),
		None => support::Peer::connect_alpn(handle, sock, server, &[b"h3"]).expect("client"),
	}
}

/// Queue the client half of the HTTP/3 handshake: SETTINGS on `control`, then
/// the CONNECT request. Raw quiche, because these tests need arrivals a real
/// WebTransport client would never produce.
fn h3_request(peer: &mut support::Peer, control: u64, url: &str) {
	let mut settings = web_transport_quinn::proto::Settings::default();
	settings.enable_webtransport(1);
	let mut buf = Vec::new();
	settings.encode(&mut buf);
	peer.conn.stream_send(control, &buf, false).expect("control stream");

	let mut buf = Vec::new();
	web_transport_quinn::proto::ConnectRequest::new(url::Url::parse(url).expect("url"))
		.with_protocol(PROTO)
		.encode(&mut buf)
		.expect("encode CONNECT");
	peer.conn.stream_send(CLIENT_BI, &buf, false).expect("connect stream");
}

/// Await `future`, failing rather than hanging if it takes too long.
///
/// Everything here is a stall or a leak, so the failure mode without the fix
/// is a test that never finishes.
async fn within<T>(handle: &moq_uring::Handle, what: &str, future: impl Future<Output = T>) -> T {
	let mut deadline = moq_net::runtime::Deadline::after(handle, std::time::Duration::from_secs(5));
	let mut future = std::pin::pin!(future);
	kio::wait(|waiter| {
		let mut cx = std::task::Context::from_waker(waiter.waker());
		if let std::task::Poll::Ready(value) = future.as_mut().poll(&mut cx) {
			return std::task::Poll::Ready(Some(value));
		}
		deadline.poll(waiter).map(|()| None)
	})
	.await
	.unwrap_or_else(|| panic!("timed out waiting for {what}"))
}

/// A client whose CONNECT is expected to fail, reporting how it failed.
fn quinn_client_err(url: String) -> std::thread::JoinHandle<web_transport_quinn::ClientError> {
	std::thread::spawn(move || {
		let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
		let rt = tokio::runtime::Builder::new_current_thread()
			.enable_all()
			.build()
			.expect("tokio runtime");
		rt.block_on(async move {
			let client = web_transport_quinn::ClientBuilder::new()
				.dangerous()
				.with_no_certificate_verification()
				.expect("client");
			let request = web_transport_quinn::proto::ConnectRequest::new(url::Url::parse(&url).expect("url"))
				.with_protocol(PROTO);
			// The server answers by closing, so the failure has to arrive as
			// that close. Waiting out the idle timeout would "fail" too, ten
			// seconds later, which is what a peer sees when a close is
			// published to the application before it reaches the wire.
			tokio::time::timeout(std::time::Duration::from_secs(3), client.connect(request))
				.await
				.expect("the CONNECT must fail before the idle timeout")
				.expect_err("the CONNECT must fail")
		})
	})
}

/// A unidirectional stream that names its type and then stalls must not hold
/// off a control stream that has fully arrived.
///
/// The accept queue is strictly id-ordered, so the stalled stream is always
/// adopted first. Classifying one at a time parked there for as long as the
/// peer kept the connection alive, with a complete SETTINGS sitting behind it.
#[test]
fn a_stalled_stream_cannot_block_the_handshake() {
	let Some(mut worker) = worker() else { return };
	let handle = worker.handle();
	let certs = support::certs().expect("certificates");
	let endpoint = h3_endpoint(&handle, &certs);
	let server = endpoint.local_addr();

	let peer_handle = handle.clone();
	worker
		.block_on(async move {
			peer_handle.clone().spawn(async move {
				let mut peer = raw_peer(&peer_handle, server, None);
				peer.flush().await.expect("first flight");
				while !peer.conn.is_established() {
					peer.step().await.expect("step");
					peer.flush().await.expect("flush");
				}

				// A WebTransport unidirectional header, complete except for
				// the session id it never sends.
				let stalled = [0x40, 0x54];
				peer.conn
					.stream_send(CLIENT_UNI[0], &stalled, false)
					.expect("stalled stream");
				h3_request(&mut peer, CLIENT_UNI[1], "https://localhost/stall");
				peer.flush().await.expect("flush");

				// Keep turning so the handshake can complete against it.
				while !peer.conn.is_closed() {
					if peer.step().await.is_err() || peer.flush().await.is_err() {
						break;
					}
				}
			});

			let conn = endpoint.accept().await.expect("accepted connection");
			let request = within(
				&handle,
				"the handshake to get past the stalled stream",
				quic::web::Request::accept(&handle, conn),
			)
			.await
			.expect("handshake");
			assert_eq!(request.url().path(), "/stall");
		})
		.expect("worker");
}

/// A handshake the server abandons must close the connection.
///
/// Dropping the public `Connection` does not: the endpoint keeps it, its
/// routes, and its driver task until the driver sees a terminal state, and the
/// backlog stopped counting it at accept. The peer picks which subprotocols it
/// offers, so answering with one it did not is a path it controls.
#[test]
fn an_abandoned_handshake_closes_the_connection() {
	let Some(mut worker) = worker() else { return };
	let handle = worker.handle();
	let certs = support::certs().expect("certificates");
	let endpoint = h3_endpoint(&handle, &certs);
	let addr = endpoint.local_addr();

	let client = quinn_client_err(format!("https://{addr}/"));

	worker
		.block_on(async move {
			let conn = endpoint.accept().await.expect("accept");
			let mut watch = conn.clone();
			let request = quic::web::Request::accept(&handle, conn).await.expect("handshake");
			let err = request
				.respond(quic::web::Response::default().with_protocol("never-offered"))
				.await
				.expect_err("a subprotocol the peer did not offer");
			assert!(matches!(err, quic::Error::Web(_)), "got {err:?}");

			// Nothing here closed it by hand; the guard on the dropped request
			// is what does.
			within(
				&handle,
				"the abandoned connection to close",
				std::future::poll_fn(|cx| watch.poll_closed(cx)),
			)
			.await;
		})
		.expect("worker");

	client.join().expect("client thread");
}

/// A rejection reaches the peer as the status it was sent.
///
/// The HTTP/3 critical streams (the peer's control and QPACK streams, and
/// ours) have to outlive the response: RFC 9114 makes closing one a connection
/// error, so tearing them down would show an H3 failure instead of the 404.
#[test]
fn a_rejection_reaches_the_peer() {
	let Some(mut worker) = worker() else { return };
	let handle = worker.handle();
	let certs = support::certs().expect("certificates");
	let endpoint = h3_endpoint(&handle, &certs);
	let addr = endpoint.local_addr();

	let client = quinn_client_err(format!("https://{addr}/nope"));

	worker
		.block_on(async move {
			let conn = endpoint.accept().await.expect("accept");
			let request = quic::web::Request::accept(&handle, conn).await.expect("handshake");
			within(
				&handle,
				"the rejection to be delivered",
				request.reject(quic::web::Rejected::NotFound),
			)
			.await
			.expect("reject");
		})
		.expect("worker");

	let err = client.join().expect("client thread");
	assert!(
		matches!(
			&err,
			web_transport_quinn::ClientError::HttpError(web_transport_quinn::ConnectError::ProtoError(
				web_transport_quinn::proto::ConnectError::WrongStatus(Some(status))
			)) if *status == http::StatusCode::NOT_FOUND
		),
		"got {err:?}"
	);
}

/// Dropping a web-mode stream cancels it with a WebTransport code, not the
/// raw zero the inner stream would send.
///
/// moq cancels a subscription by dropping its stream, so this is the ordinary
/// path: unmapped, a browser reads it as an HTTP/3 stream error instead.
#[test]
fn a_dropped_stream_carries_a_webtransport_code() {
	let Some(mut worker) = worker() else { return };
	let handle = worker.handle();
	let certs = support::certs().expect("certificates");
	let endpoint = h3_endpoint(&handle, &certs);
	let addr = endpoint.local_addr();

	let client = quinn_client(format!("https://{addr}/"), |session| {
		Box::pin(async move {
			// The server writes this and then drops the stream unfinished.
			let mut recv = session.accept_uni().await.expect("accept_uni");
			let mut buf = [0u8; 4096];
			let n = recv.read(&mut buf).await.expect("read").expect("payload");
			assert_eq!(&buf[..n], PAYLOAD);

			// Tell it we have the payload, so the reset below cannot race it.
			let mut ack = session.open_uni().await.expect("open_uni");
			ack.write_all(b"ack").await.expect("write");
			ack.finish().expect("finish");

			let err = recv.read(&mut buf).await.expect_err("the server dropped it");
			assert!(matches!(err, web_transport_quinn::ReadError::Reset(0)), "got {err:?}");

			// And the other direction: the server drops the read half of this
			// one, which must arrive as a WebTransport cancellation too.
			let mut send = session.open_uni().await.expect("open_uni");
			let err = loop {
				match send.write_all(PAYLOAD).await {
					Ok(()) => tokio::task::yield_now().await,
					Err(err) => break err,
				}
			};
			assert!(
				matches!(err, web_transport_quinn::WriteError::Stopped(0)),
				"got {err:?}"
			);

			session.close(CLOSE_CODE, CLOSE_REASON.as_bytes());
			session.closed().await;
		})
	});

	worker
		.block_on(async move {
			let conn = endpoint.accept().await.expect("accept");
			let request = quic::web::Request::accept(&handle, conn).await.expect("handshake");
			let mut session = request
				.respond(quic::web::Response::default().with_protocol(PROTO))
				.await
				.expect("respond");

			let mut send = std::future::poll_fn(|cx| session.poll_open_uni(cx))
				.await
				.expect("open_uni");
			let mut payload = PAYLOAD;
			while !payload.is_empty() {
				let n = std::future::poll_fn(|cx| send.poll_write(cx, payload))
					.await
					.expect("write");
				payload = &payload[n..];
			}

			let mut ack = std::future::poll_fn(|cx| session.poll_accept_uni(cx))
				.await
				.expect("accept_uni");
			assert_eq!(drain(&mut ack).await, b"ack");
			drop(send);

			// The client's stream, whose read half we abandon.
			let mut recv = std::future::poll_fn(|cx| session.poll_accept_uni(cx))
				.await
				.expect("accept_uni");
			let mut buf = [0u8; 4096];
			std::future::poll_fn(|cx| recv.poll_read(cx, &mut buf))
				.await
				.expect("read");
			drop(recv);

			std::future::poll_fn(|cx| session.poll_closed(cx)).await;
		})
		.expect("worker");

	client.join().expect("client thread");
}

/// Finishing a web-mode stream that cannot frame itself yet is backpressure,
/// not a failure.
///
/// A stream opened while the connection's flow-control credit is spent still
/// owes its WebTransport header, and callers drop (and so reset) a stream that
/// reports a terminal error. The finish has to complete once credit returns.
#[test]
fn finishing_under_backpressure_is_not_an_error() {
	let Some(mut worker) = worker() else { return };
	let handle = worker.handle();
	let certs = support::certs().expect("certificates");
	let endpoint = h3_endpoint(&handle, &certs);
	let server = endpoint.local_addr();

	// Set by the server once it has filled the peer's credit: reading is what
	// returns it, so the peer must not read before then.
	let reading = std::rc::Rc::new(std::cell::Cell::new(false));
	// How many streams the peer has seen end cleanly. A reset arrives instead
	// of a FIN, so this never reaches its target if a finished stream is
	// cancelled on the way out. A kio channel rather than a flag polled on a
	// timer: the tests share a machine, and a wall clock loses that race.
	let ended = kio::Producer::new(0usize);
	let counted = ended.consume();

	let peer_handle = handle.clone();
	let peer_reading = reading.clone();
	worker
		.block_on(async move {
			peer_handle.clone().spawn(async move {
				let mut peer = raw_peer(&peer_handle, server, Some(THROTTLE));
				peer.flush().await.expect("first flight");
				while !peer.conn.is_established() {
					peer.step().await.expect("step");
					peer.flush().await.expect("flush");
				}
				h3_request(&mut peer, CLIENT_UNI[0], "https://localhost/backpressure");
				peer.flush().await.expect("flush");

				let mut scratch = vec![0u8; 64 * 1024];
				while !peer.conn.is_closed() {
					if peer.step().await.is_err() {
						break;
					}
					if peer_reading.get() {
						let readable: Vec<u64> = peer.conn.readable().collect();
						for stream in readable {
							loop {
								match peer.conn.stream_recv(stream, &mut scratch) {
									Ok((_, true)) => {
										if let Ok(mut count) = ended.write() {
											*count += 1;
										}
										break;
									}
									Ok((_, false)) => {}
									Err(_) => break,
								}
							}
						}
					}
					if peer.flush().await.is_err() {
						break;
					}
				}
			});

			let conn = endpoint.accept().await.expect("accepted connection");
			let request = quic::web::Request::accept(&handle, conn).await.expect("handshake");
			let mut session = request
				.respond(quic::web::Response::default().with_protocol(PROTO))
				.await
				.expect("respond");

			// Spend the peer's connection-level credit on a stream it is not
			// reading. Polling with a no-op waker stops at the first refusal
			// rather than parking.
			let mut filler = std::future::poll_fn(|cx| session.poll_open_uni(cx))
				.await
				.expect("open_uni");
			let chunk = [0u8; 4096];
			let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
			loop {
				match filler.poll_write(&mut cx, &chunk) {
					std::task::Poll::Ready(Ok(_)) => {}
					std::task::Poll::Ready(Err(err)) => panic!("filling the credit failed: {err}"),
					std::task::Poll::Pending => break,
				}
			}

			// A fresh stream still owes its header and has no credit to write
			// it with, which is exactly the case that used to fail.
			let mut blocked = std::future::poll_fn(|cx| session.poll_open_uni(cx))
				.await
				.expect("open_uni");
			blocked.finish().expect("finishing under backpressure");

			// The same case, for a caller that finishes and drops rather than
			// polling: the FIN is the stream's debt by then, so `Drop` has to
			// pay it instead of resetting a stream that finished cleanly.
			let mut dropped = std::future::poll_fn(|cx| session.poll_open_uni(cx))
				.await
				.expect("open_uni");
			dropped.finish().expect("finishing under backpressure");

			reading.set(true);
			within(
				&handle,
				"the finished stream to close once credit returns",
				std::future::poll_fn(|cx| blocked.poll_closed(cx)),
			)
			.await
			.expect("closed");

			// Credit is back by now, so the drop can make good on the finish.
			// Both streams have to reach the peer as clean ends; a cancelled
			// one arrives as a reset and never counts.
			drop(dropped);
			within(
				&handle,
				"both finished streams to arrive intact",
				counted.wait(|count| match **count >= 2 {
					true => std::task::Poll::Ready(()),
					false => std::task::Poll::Pending,
				}),
			)
			.await
			.expect("the peer is still reading");
		})
		.expect("worker");
}

/// The arrival cap must not refuse a peer whose control stream is already
/// first in the queue.
///
/// The cap bounds a peer that never sends a control stream. Counting arrivals
/// before classifying any of them turned it into a limit on how much a valid
/// client may pipeline: the control stream sat at the head of the queue,
/// fully arrived, while the streams behind it tripped the cap.
#[test]
fn a_pipelining_peer_is_not_refused_by_the_arrival_cap() {
	let Some(mut worker) = worker() else { return };
	let handle = worker.handle();
	let certs = support::certs().expect("certificates");
	let endpoint = h3_endpoint(&handle, &certs);
	let server = endpoint.local_addr();

	// Closed once the server has acknowledged every stream, which is what
	// proves they are all sitting in its accept queue rather than still on
	// the wire. The handshake below must not start before that.
	let queued = kio::Producer::new(());
	let ready = queued.consume();

	let peer_handle = handle.clone();
	worker
		.block_on(async move {
			peer_handle.clone().spawn(async move {
				let mut peer = raw_peer(&peer_handle, server, None);
				peer.flush().await.expect("first flight");
				while !peer.conn.is_established() {
					peer.step().await.expect("step");
					peer.flush().await.expect("flush");
				}

				// The control stream leads, then comfortably more than the cap
				// behind it. The one that tips the cap over is a real
				// WebTransport stream rather than junk, so losing it is
				// visible: junk is dropped by classification either way.
				h3_request(&mut peer, CLIENT_UNI[0], "https://localhost/pipelined");
				for i in 1..90u64 {
					let stream = CLIENT_UNI[0] + i * 4;
					// Arrivals are in id order, and the control stream is the
					// first, so this one is arrival HANDSHAKE_STREAMS + 1.
					let payload: &[u8] = match i as usize == HANDSHAKE_STREAMS {
						// `0x4054` is the WebTransport stream type, then the
						// session id: the CONNECT stream, which is 0.
						true => &[0x40, 0x54, 0x00],
						false => &[0x3f],
					};
					if peer
						.conn
						.stream_send(stream, payload, i as usize != HANDSHAKE_STREAMS)
						.is_err()
					{
						break;
					}
				}
				peer.flush().await.expect("flush");
				// Wait for the server to answer, which it must: the streams
				// above are ack-eliciting, and an acknowledgement means its
				// driver has already ingested them into the accept queue. That
				// is what makes the handshake below start against a full queue
				// instead of racing the packets into it, which is the
				// difference between reproducing the bug and not.
				//
				// Exactly one round trip. Waiting for a second would hang
				// until the idle timeout, since the server sends nothing more
				// while it waits for the signal below.
				let received = peer.conn.stats().recv;
				while peer.conn.stats().recv == received {
					peer.step().await.expect("step");
					peer.flush().await.expect("flush");
				}
				let _ = queued.close();

				while !peer.conn.is_closed() {
					if peer.step().await.is_err() || peer.flush().await.is_err() {
						break;
					}
				}
			});

			let conn = endpoint.accept().await.expect("accepted connection");
			within(&handle, "the peer to queue every stream", ready.closed()).await;
			let request = within(
				&handle,
				"the handshake to accept a pipelining peer",
				quic::web::Request::accept(&handle, conn),
			)
			.await
			.expect("handshake");
			assert_eq!(request.url().path(), "/pipelined");

			// And the stream that tipped the cap over is still the peer's to
			// use, rather than one the handshake quietly cancelled.
			let mut session = request
				.respond(quic::web::Response::default().with_protocol(PROTO))
				.await
				.expect("respond");
			within(
				&handle,
				"the stream that tipped the cap over to survive the handshake",
				std::future::poll_fn(|cx| session.poll_accept_uni(cx)),
			)
			.await
			.expect("accept_uni");
		})
		.expect("worker");
}

/// A rejection abandoned mid-grace still closes the connection.
///
/// `reject` waits for the peer to acknowledge the response before closing
/// deliberately. Disarming the guard before that wait meant a cancelled
/// `reject` skipped both, leaving the endpoint holding the connection, its
/// routes, and its driver for a peer that keeps sending.
#[test]
fn an_abandoned_rejection_still_closes_the_connection() {
	let Some(mut worker) = worker() else { return };
	let handle = worker.handle();
	let certs = support::certs().expect("certificates");
	let endpoint = h3_endpoint(&handle, &certs);
	let server = endpoint.local_addr();

	let peer_handle = handle.clone();
	worker
		.block_on(async move {
			peer_handle.clone().spawn(async move {
				let mut peer = raw_peer(&peer_handle, server, None);
				peer.flush().await.expect("first flight");
				while !peer.conn.is_established() {
					peer.step().await.expect("step");
					peer.flush().await.expect("flush");
				}
				h3_request(&mut peer, CLIENT_UNI[0], "https://localhost/abandoned");
				peer.flush().await.expect("flush");

				// Deliberately never acknowledges the response, so the
				// rejection stays parked in its grace period.
				std::future::pending::<()>().await;
			});

			let conn = endpoint.accept().await.expect("accepted connection");
			let mut watch = conn.clone();
			let request = quic::web::Request::accept(&handle, conn).await.expect("handshake");

			// Drive the rejection until it parks waiting for the peer, then
			// walk away from it.
			// Boxed, not `pin!`: dropping a `Pin<&mut F>` leaves the future
			// itself alive in its hidden local, which is not the case here.
			let mut reject = Box::pin(request.reject(quic::web::Rejected::NotFound));
			let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
			for _ in 0..64 {
				match reject.as_mut().poll(&mut cx) {
					std::task::Poll::Ready(result) => panic!("the rejection must still be waiting: {result:?}"),
					std::task::Poll::Pending => {}
				}
			}
			drop(reject);

			within(
				&handle,
				"the abandoned rejection to close the connection",
				std::future::poll_fn(|cx| watch.poll_closed(cx)),
			)
			.await;
		})
		.expect("worker");
}
