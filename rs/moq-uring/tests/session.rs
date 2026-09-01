//! The PR-4 milestone: a real moq-lite session over the worker's QUIC path.
//!
//! A publisher and a subscriber session run on one worker (two sockets, two
//! [`moq_uring::quic::Connection`]s over loopback), negotiated end to end by
//! `moq_net::Server::accept_lite` / `Client::connect_lite`, with a broadcast,
//! a track, and a frame flowing through the model.
//!
//! The origin drivers demand `Send` timers (`origin::Driver::run` erases them
//! into the model's shared state), which the worker's `!Send` handle cannot
//! provide yet, so they run on a tokio thread here: the model is `Send + Sync`
//! by design, and this mirrors the relay topology where a main runtime owns
//! the origins while workers run sessions.
//!
//! Kernel-gated: skips loudly below the Linux 6.12 floor (GitHub-hosted CI),
//! and runs everywhere else.

#![cfg(all(target_os = "linux", any(feature = "quiche", feature = "quinn")))]

#[path = "support/quiche.rs"]
mod support;

use std::net::UdpSocket;

use moq_net::origin;
use moq_uring::{Config, Error, Worker, quic, udp};

fn worker() -> Option<Worker> {
	match Worker::new(Config::default()) {
		Ok(worker) => Some(worker),
		Err(Error::Unsupported(reason)) => {
			eprintln!("skipping io_uring session test: {reason}");
			None
		}
		Err(err) => panic!("worker setup failed: {err}"),
	}
}

const ALPN: &str = "moq-lite-05";
const PAYLOAD: &[u8] = b"hello over io_uring";

#[test]
fn lite_session_over_the_worker() {
	let Some(mut worker) = worker() else { return };
	let handle = worker.handle();

	// The model and its origins, driven on a tokio thread (see module docs).
	let (pub_origin, pub_driver) = origin::Producer::new(origin::Info::new(moq_net::Hop::random()));
	let (sub_origin, sub_driver) = origin::Producer::new(origin::Info::new(moq_net::Hop::random()));
	let origins = std::thread::spawn(move || {
		let rt = tokio::runtime::Builder::new_current_thread()
			.enable_time()
			.build()
			.expect("tokio runtime");
		rt.block_on(async move {
			tokio::join!(
				pub_driver.run(support::TokioTimers),
				sub_driver.run(support::TokioTimers)
			);
		});
	});

	// Content ready before anyone connects: one broadcast, one track, one
	// finished group.
	let mut broadcast = pub_origin.create_broadcast("test").expect("create broadcast");
	let _announce_broadcast = pub_origin
		.announce("test", Default::default())
		.expect("create broadcast");
	let mut track = broadcast.create_track("data", None).expect("create track");
	let mut group = track.append_group().expect("append group");
	group
		.write_frame(moq_net::Timestamp::ZERO, PAYLOAD)
		.expect("write frame");
	group.finish().expect("finish group");

	let certs = support::certs().expect("certificates");
	let mut server_config = quic::server::Config::new(quic::Identity::open(&certs.cert, &certs.key).expect("identity"));
	server_config.alpn = vec![ALPN.to_string()];

	let server_sock = handle
		.udp(UdpSocket::bind("127.0.0.1:0").expect("bind"), udp::Config::default())
		.expect("server socket");
	let server_addr = server_sock.local_addr().expect("server addr");
	let client_sock = handle
		.udp(UdpSocket::bind("127.0.0.1:0").expect("bind"), udp::Config::default())
		.expect("client socket");
	let mut dial = quic::client::Config::new(server_addr, "localhost");
	dial.alpn = vec![ALPN.to_string()];
	// The server's certificate is our own self-signed one; this is loopback.
	dial.verify = false;

	// The publisher session runs as a worker task for the life of the test.
	let server_handle = handle.clone();
	handle.spawn(async move {
		let conn = quic::server::accept(&server_handle, server_sock, &server_config)
			.await
			.expect("quic accept");
		let session = moq_net::Server::new()
			.with_publisher(&pub_origin)
			.accept_lite(server_handle.clone(), quic::web::Session::raw(conn))
			.await
			.expect("accept_lite");
		// Serve until the client walks away.
		session.closed().await;
	});

	let sub = sub_origin.clone();
	let payload = worker
		.block_on(async move {
			let conn = quic::client::connect(&handle, client_sock, &dial)
				.await
				.expect("quic connect");
			assert_eq!(
				web_transport_trait::poll::Session::protocol(&conn),
				Some(ALPN),
				"negotiated ALPN"
			);

			let session = moq_net::Client::new()
				.with_subscriber(sub.clone())
				.connect_lite(handle.clone(), quic::web::Session::raw(conn))
				.await
				.expect("connect_lite");

			let bc = {
				let consumer = sub.consume();
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

			session.abort(moq_net::Error::Cancel);
			frame.payload
		})
		.expect("worker");

	assert_eq!(&payload[..], PAYLOAD);

	// Dropping the worker drops the server task (and with it the last
	// publisher handle), which is what lets the origin drivers resolve.
	drop(worker);
	drop(broadcast);
	drop(track);
	drop(sub_origin);
	origins.join().expect("origin drivers");
}

/// Two clients handshake and run moq-lite sessions against one server
/// socket: the endpoint demuxes them by connection id, which is the relay
/// shape (one socket per worker, every session on it).
#[test]
fn two_lite_sessions_share_the_server_socket() {
	let Some(mut worker) = worker() else { return };
	let handle = worker.handle();

	let (pub_origin, pub_driver) = origin::Producer::new(origin::Info::new(moq_net::Hop::random()));
	let (sub_a, sub_a_driver) = origin::Producer::new(origin::Info::new(moq_net::Hop::random()));
	let (sub_b, sub_b_driver) = origin::Producer::new(origin::Info::new(moq_net::Hop::random()));
	let origins = std::thread::spawn(move || {
		let rt = tokio::runtime::Builder::new_current_thread()
			.enable_time()
			.build()
			.expect("tokio runtime");
		rt.block_on(async move {
			tokio::join!(
				pub_driver.run(support::TokioTimers),
				sub_a_driver.run(support::TokioTimers),
				sub_b_driver.run(support::TokioTimers),
			);
		});
	});

	let mut broadcast = pub_origin.create_broadcast("test").expect("create broadcast");
	let _announce_broadcast = pub_origin
		.announce("test", Default::default())
		.expect("create broadcast");
	let mut track = broadcast.create_track("data", None).expect("create track");
	let mut group = track.append_group().expect("append group");
	group
		.write_frame(moq_net::Timestamp::ZERO, PAYLOAD)
		.expect("write frame");
	group.finish().expect("finish group");

	let certs = support::certs().expect("certificates");
	let mut server_config = quic::server::Config::new(quic::Identity::open(&certs.cert, &certs.key).expect("identity"));
	server_config.alpn = vec![ALPN.to_string()];

	let server_sock = handle
		.udp(UdpSocket::bind("127.0.0.1:0").expect("bind"), udp::Config::default())
		.expect("server socket");
	let endpoint = quic::Endpoint::new(
		&handle,
		server_sock,
		quic::endpoint::Config::default().with_server(server_config),
	)
	.expect("endpoint");
	let server_addr = endpoint.local_addr();

	// The listener: every accepted connection becomes a publisher session.
	let server_handle = handle.clone();
	handle.spawn(async move {
		while let Ok(conn) = endpoint.accept().await {
			let pub_origin = pub_origin.clone();
			let session_handle = server_handle.clone();
			server_handle.spawn(async move {
				let session = moq_net::Server::new()
					.with_publisher(&pub_origin)
					.accept_lite(session_handle, quic::web::Session::raw(conn))
					.await
					.expect("accept_lite");
				session.closed().await;
			});
		}
	});

	let mut dial = quic::client::Config::new(server_addr, "localhost");
	dial.alpn = vec![ALPN.to_string()];
	dial.verify = false;

	let subs = [sub_a.clone(), sub_b.clone()];
	worker
		.block_on(async move {
			for sub in subs {
				let client_sock = handle
					.udp(UdpSocket::bind("127.0.0.1:0").expect("bind"), udp::Config::default())
					.expect("client socket");
				let conn = quic::client::connect(&handle, client_sock, &dial)
					.await
					.expect("quic connect");
				let session = moq_net::Client::new()
					.with_subscriber(sub.clone())
					.connect_lite(handle.clone(), quic::web::Session::raw(conn))
					.await
					.expect("connect_lite");

				let bc = {
					let consumer = sub.consume();
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

				session.abort(moq_net::Error::Cancel);
			}
		})
		.expect("worker");

	drop(worker);
	drop(broadcast);
	drop(track);
	drop(sub_a);
	drop(sub_b);
	origins.join().expect("origin drivers");
}

/// The client verifies for real, trusting only the certificate the server
/// presents: nothing handshakes unless the configured roots reach quiche.
#[test]
fn configured_roots_verify_the_server() {
	let Some(mut worker) = worker() else { return };
	let handle = worker.handle();
	let certs = support::certs().expect("certificates");

	let mut server_config = quic::server::Config::new(quic::Identity::open(&certs.cert, &certs.key).expect("identity"));
	server_config.alpn = vec![ALPN.to_string()];

	let server_sock = handle
		.udp(UdpSocket::bind("127.0.0.1:0").expect("bind"), udp::Config::default())
		.expect("server socket");
	let server_addr = server_sock.local_addr().expect("server addr");
	let client_sock = handle
		.udp(UdpSocket::bind("127.0.0.1:0").expect("bind"), udp::Config::default())
		.expect("client socket");
	let mut dial = quic::client::Config::new(server_addr, "localhost");
	dial.alpn = vec![ALPN.to_string()];
	// Verification on, trusting nothing but the certificate this server
	// presents. The platform store is off, so if it leaked in, an unrelated
	// public CA would be trusted too.
	dial.system_roots = false;
	dial.roots = vec![certs.cert.clone()];

	let server_handle = handle.clone();
	handle.spawn(async move {
		quic::server::accept(&server_handle, server_sock, &server_config)
			.await
			.expect("quic accept");
	});

	worker
		.block_on(async move {
			let conn = quic::client::connect(&handle, client_sock, &dial)
				.await
				.expect("quic connect");
			assert_eq!(
				web_transport_trait::poll::Session::protocol(&conn),
				Some(ALPN),
				"negotiated ALPN"
			);
		})
		.expect("worker");
}

/// `ClientAuth::Required` is the only setting that actually demands a client
/// certificate. `SSL_VERIFY_PEER` on its own validates one that arrives and
/// waves through a client that presents none, so a server meaning mTLS has to
/// say `Required`.
///
/// A failed handshake never reaches `accept` (the endpoint keeps listening),
/// so the assertion is double-sided: the server accepts nothing, and the
/// client's connection dies. Under TLS 1.3 the client may finish its own
/// handshake before the server's verdict on the certificate it never sent
/// comes back, so `connect` returning `Ok` proves nothing; the refusal
/// arrives as the connection's terminal error.
#[test]
fn required_client_auth_refuses_an_anonymous_client() {
	let Some(mut worker) = worker() else { return };
	let handle = worker.handle();
	let certs = support::certs().expect("certificates");

	let mut server_config = quic::server::Config::new(quic::Identity::open(&certs.cert, &certs.key).expect("identity"));
	server_config.alpn = vec![ALPN.to_string()];
	// Any root will do: the client presents nothing, so it fails before the
	// chain is ever checked.
	server_config.client_auth = quic::server::ClientAuth::Required(vec![certs.cert.clone()]);

	let server_sock = handle
		.udp(UdpSocket::bind("127.0.0.1:0").expect("bind"), udp::Config::default())
		.expect("server socket");
	let client_sock = handle
		.udp(UdpSocket::bind("127.0.0.1:0").expect("bind"), udp::Config::default())
		.expect("client socket");

	let endpoint = quic::Endpoint::new(
		&handle,
		server_sock,
		quic::endpoint::Config::default().with_server(server_config),
	)
	.expect("endpoint");
	let server_addr = endpoint.local_addr();

	let mut dial = quic::client::Config::new(server_addr, "localhost");
	dial.alpn = vec![ALPN.to_string()];
	dial.verify = false;
	// No identity: this client is anonymous.

	let accepted = std::rc::Rc::new(std::cell::Cell::new(false));
	let accept_flag = accepted.clone();
	handle.spawn(async move {
		if endpoint.accept().await.is_ok() {
			accept_flag.set(true);
		}
	});

	worker
		.block_on(async move {
			match quic::client::connect(&handle, client_sock, &dial).await {
				// The dial itself was refused; done.
				Err(_) => {}
				// Established locally, but the server's refusal closes it.
				Ok(mut conn) => {
					std::future::poll_fn(|cx| web_transport_trait::poll::Session::poll_closed(&mut conn, cx)).await;
				}
			}
		})
		.expect("worker");

	assert!(
		!accepted.get(),
		"a server requiring a certificate must refuse an anonymous client"
	);
}

/// A root file is routinely a bundle of several CAs, and it has to be loaded
/// whole.
///
/// Only the second CA in the bundle below signed the server, so a loader that
/// stops at the first certificate rejects a peer that is perfectly valid while
/// still looking configured. The relay reaches this through `listen.tls.root`.
#[test]
fn a_root_bundle_is_loaded_whole() {
	let Some(mut worker) = worker() else { return };
	let handle = worker.handle();
	let bundle = support::bundle().expect("bundle");

	let mut server_config =
		quic::server::Config::new(quic::Identity::open(&bundle.cert, &bundle.key).expect("identity"));
	server_config.alpn = vec![ALPN.to_string()];

	let server_sock = handle
		.udp(UdpSocket::bind("127.0.0.1:0").expect("bind"), udp::Config::default())
		.expect("server socket");
	let server_addr = server_sock.local_addr().expect("server addr");
	let client_sock = handle
		.udp(UdpSocket::bind("127.0.0.1:0").expect("bind"), udp::Config::default())
		.expect("client socket");
	let mut dial = quic::client::Config::new(server_addr, "localhost");
	dial.alpn = vec![ALPN.to_string()];
	dial.system_roots = false;
	dial.roots = vec![bundle.roots.clone()];

	let server_handle = handle.clone();
	handle.spawn(async move {
		quic::server::accept(&server_handle, server_sock, &server_config)
			.await
			.expect("quic accept");
	});

	worker
		.block_on(async move {
			let conn = quic::client::connect(&handle, client_sock, &dial)
				.await
				.expect("quic connect");
			assert_eq!(
				web_transport_trait::poll::Session::protocol(&conn),
				Some(ALPN),
				"negotiated ALPN"
			);
		})
		.expect("worker");
}
