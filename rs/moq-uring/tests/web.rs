//! WebTransport interop: the reference tokio stack (`web-transport-quinn`,
//! what browsers interop with) dials the uring server. One test hand-drives
//! streams and datagrams through the H3 framing; the other runs a whole
//! moq-lite session over it, which is exactly the browser-to-relay path.
//!
//! Kernel-gated: skips loudly below the Linux 6.12 floor (GitHub-hosted CI),
//! and runs everywhere else.

#![cfg(target_os = "linux")]

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

	let (pub_origin, pub_driver) = origin::Producer::new(origin::Info::new(moq_net::Origin::random()));
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
			let (sub_origin, sub_driver) = origin::Producer::new(origin::Info::new(moq_net::Origin::random()));
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
