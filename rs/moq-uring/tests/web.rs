//! WebTransport interop: the reference tokio stack (`web-transport-moq`,
//! what browsers interop with) dials the uring server. One test hand-drives
//! streams and datagrams through the H3 framing; the other runs a whole
//! moq-lite session over it, which is exactly the browser-to-relay path.
//!
//! Kernel-gated: skips loudly below the Linux 6.12 floor (GitHub-hosted CI),
//! and runs everywhere else.

#![cfg(all(target_os = "linux", feature = "noq"))]

#[path = "support.rs"]
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
	quic::Endpoint::new(sock, quic::endpoint::Config::default().with_server(server)).expect("endpoint")
}

/// The tokio-side client, in its own runtime on its own thread.
fn noq_client(
	url: String,
	body: impl FnOnce(web_transport_moq::Session) -> ClientFuture + Send + 'static,
) -> std::thread::JoinHandle<()> {
	std::thread::spawn(move || {
		// May already be installed by a sibling test; either way one exists.
		let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
		let rt = tokio::runtime::Builder::new_current_thread()
			.enable_all()
			.build()
			.expect("tokio runtime");
		rt.block_on(async move {
			let client = web_transport_moq::ClientBuilder::new()
				.dangerous()
				.with_no_certificate_verification()
				.expect("client");
			let request =
				web_transport_moq::proto::ConnectRequest::new(url::Url::parse(&url).expect("url")).with_protocol(PROTO);
			let session = client.connect(request).await.expect("connect");
			body(session).await;
		});
	})
}

type ClientFuture = std::pin::Pin<Box<dyn Future<Output = ()> + Send>>;

/// Read a noq-side stream to its end.
async fn read_all(recv: &mut web_transport_moq::RecvStream) -> Vec<u8> {
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

	let client = noq_client(format!("https://{addr}/echo?token=abc"), |session| {
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

			let request = quic::web::Request::accept(conn).await.expect("handshake");
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

	let (pub_origin, pub_driver) = origin::Producer::new(origin::Config::default());
	let origins = std::thread::spawn(move || {
		let rt = tokio::runtime::Builder::new_current_thread()
			.enable_time()
			.build()
			.expect("tokio runtime");
		rt.block_on(moq_net::time::run(pub_driver));
	});

	let broadcast = pub_origin.create_broadcast("test").expect("create broadcast");
	broadcast.announce(Default::default()).expect("create broadcast");
	let track = broadcast.create_track("data", None).expect("create track");
	let mut group = track.append_group().expect("append group");
	group
		.write_frame(moq_net::Timestamp::ZERO, PAYLOAD)
		.expect("write frame");
	group.finish().expect("finish group");

	let endpoint = h3_endpoint(&handle, &certs);
	let addr = endpoint.local_addr();

	let client = noq_client(format!("https://{addr}/"), |session| {
		Box::pin(async move {
			assert_eq!(session.protocol(), Some(PROTO), "negotiated subprotocol");
			let (sub_origin, sub_driver) = origin::Producer::new(origin::Config::default());
			let driver = tokio::spawn(moq_net::time::run(sub_driver));

			let (moq, session_driver) = moq_net::Client::new()
				.with_subscriber(sub_origin.clone())
				.connect_lite(std::time::Instant::now(), moq_tokio::transport::Session::new(session))
				.await
				.expect("connect_lite");
			tokio::spawn(moq_net::time::run(session_driver));

			let bc = {
				let consumer = sub_origin.consume();
				consumer.routed("test").await.expect("broadcast announced");
				consumer.request_broadcast("test").await.expect("broadcast resolves")
			};
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
			let request = quic::web::Request::accept(conn).await.expect("handshake");
			// The WebTransport equivalent of ALPN: pick the moq version.
			let protocol = request.protocols().iter().find(|p| *p == PROTO).cloned();
			let mut response = quic::web::Response::default();
			if let Some(protocol) = &protocol {
				response = response.with_protocol(protocol);
			}
			let session = request.respond(response).await.expect("respond");

			let (session, driver) = moq_net::Server::new()
				.with_publisher(&serve_origin)
				.accept_lite(std::time::Instant::now(), session)
				.await
				.expect("accept_lite");
			let task_handle = handle.clone();
			handle.spawn(async move {
				let _ = task_handle.run(driver).await;
			});
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

/// Await `future`, failing rather than hanging if it takes too long.
///
/// Everything here is a stall or a leak, so the failure mode without the fix
/// is a test that never finishes.
async fn within<T>(handle: &moq_uring::Handle, what: &str, future: impl Future<Output = T>) -> T {
	let mut deadline = moq_uring::Timer::after(handle, std::time::Duration::from_secs(5));
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
fn noq_client_err(url: String) -> std::thread::JoinHandle<web_transport_moq::ClientError> {
	std::thread::spawn(move || {
		let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
		let rt = tokio::runtime::Builder::new_current_thread()
			.enable_all()
			.build()
			.expect("tokio runtime");
		rt.block_on(async move {
			let client = web_transport_moq::ClientBuilder::new()
				.dangerous()
				.with_no_certificate_verification()
				.expect("client");
			let request =
				web_transport_moq::proto::ConnectRequest::new(url::Url::parse(&url).expect("url")).with_protocol(PROTO);
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

	let client = noq_client_err(format!("https://{addr}/"));

	worker
		.block_on(async move {
			let conn = endpoint.accept().await.expect("accept");
			let mut watch = conn.clone();
			let request = quic::web::Request::accept(conn).await.expect("handshake");
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

	let client = noq_client_err(format!("https://{addr}/nope"));

	worker
		.block_on(async move {
			let conn = endpoint.accept().await.expect("accept");
			let request = quic::web::Request::accept(conn).await.expect("handshake");
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
			web_transport_moq::ClientError::HttpError(web_transport_moq::ConnectError::ProtoError(
				web_transport_moq::proto::ConnectError::WrongStatus(Some(status))
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

	let client = noq_client(format!("https://{addr}/"), |session| {
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
			assert!(matches!(err, web_transport_moq::ReadError::Reset(0)), "got {err:?}");

			// And the other direction: the server drops the read half of this
			// one, which must arrive as a WebTransport cancellation too.
			let mut send = session.open_uni().await.expect("open_uni");
			let err = loop {
				match send.write_all(PAYLOAD).await {
					Ok(()) => tokio::task::yield_now().await,
					Err(err) => break err,
				}
			};
			assert!(matches!(err, web_transport_moq::WriteError::Stopped(0)), "got {err:?}");

			session.close(CLOSE_CODE, CLOSE_REASON.as_bytes());
			session.closed().await;
		})
	});

	worker
		.block_on(async move {
			let conn = endpoint.accept().await.expect("accept");
			let request = quic::web::Request::accept(conn).await.expect("handshake");
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

/// A pending HTTP/3 handshake owns the connection even before it has read
/// SETTINGS or CONNECT. Both suspension points must close on cancellation.
fn cancelling_a_pending_web_handshake(send_settings: bool) {
	let Some(mut worker) = worker() else { return };
	let handle = worker.handle();
	let certs = support::certs().expect("certificates");
	let server = h3_endpoint(&handle, &certs);
	let sock = handle
		.udp(UdpSocket::bind("127.0.0.1:0").expect("bind"), udp::Config::default())
		.expect("client socket");
	let client = quic::Endpoint::new(sock, quic::endpoint::Config::default()).expect("client endpoint");
	let mut dial = quic::client::Config::new(server.local_addr(), "localhost");
	dial.alpn = vec!["h3".to_string()];
	dial.verify = false;

	worker
		.block_on(async {
			let mut peer = client.connect(&dial).await.expect("dial");
			let conn = server.accept().await.expect("accept");
			let mut watch = conn.clone();
			let mut control = None;
			if send_settings {
				use web_transport_trait::Stats as _;
				let before = watch.stats().bytes_received().expect("receive stats");
				let mut stream = std::future::poll_fn(|cx| peer.poll_open_uni(cx))
					.await
					.expect("control stream");
				let mut settings = web_transport_proto::Settings::default();
				settings.enable_webtransport(1);
				let mut bytes = Vec::new();
				settings.encode(&mut bytes);
				let mut remaining = bytes.as_slice();
				while !remaining.is_empty() {
					let n = std::future::poll_fn(|cx| stream.poll_write(cx, remaining))
						.await
						.expect("write settings");
					remaining = &remaining[n..];
				}
				control = Some(stream);
				within(&handle, "peer SETTINGS to arrive", async {
					loop {
						if watch.stats().bytes_received().expect("receive stats") > before {
							break;
						}
						let mut tick = moq_uring::Timer::after(&handle, std::time::Duration::from_millis(10));
						kio::wait(|waiter| tick.poll(waiter)).await;
					}
				})
				.await;
			}

			let mut handshake = Box::pin(quic::web::Request::accept(conn));
			std::future::poll_fn(|cx| {
				assert!(
					handshake.as_mut().poll(cx).is_pending(),
					"the handshake must await peer input"
				);
				std::task::Poll::Ready(())
			})
			.await;
			drop(handshake);

			let err = within(
				&handle,
				"cancelled WebTransport connection to close",
				std::future::poll_fn(|cx| watch.poll_closed(cx)),
			)
			.await;
			assert!(matches!(err, quic::Error::App { code: 0x101, .. }), "got {err:?}");
			within(
				&handle,
				"peer to receive the close",
				std::future::poll_fn(|cx| peer.poll_closed(cx)),
			)
			.await;
			drop(control);

			let sibling = within(&handle, "sibling dial", client.connect(&dial))
				.await
				.expect("sibling dial");
			let accepted = within(&handle, "sibling accept", server.accept())
				.await
				.expect("sibling accept");
			assert_eq!(sibling.protocol(), Some("h3"));
			assert_eq!(accepted.protocol(), Some("h3"));
		})
		.expect("worker");
}

#[test]
fn cancelling_before_peer_settings_closes_the_connection() {
	cancelling_a_pending_web_handshake(false);
}

#[test]
fn cancelling_while_awaiting_connect_closes_the_connection() {
	cancelling_a_pending_web_handshake(true);
}
