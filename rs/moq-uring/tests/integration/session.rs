//! The PR-4 milestone: a real moq-lite session over the worker's QUIC path.
//!
//! A publisher and a subscriber session run on one worker (two sockets, two
//! [`moq_uring::quic::Connection`]s over loopback), negotiated end to end by
//! `moq_net::Server::accept` / `Client::connect`, with a broadcast,
//! a track, and a frame flowing through the model.
//!
//! Origin drivers run through Tokio's explicit-time adapter while session
//! drivers use the worker's clock and timer.

#![cfg(feature = "quic")]

use crate::support;

use std::net::UdpSocket;

use moq_net::origin;
use moq_uring::{Config, Error, Worker, quic, udp};

fn worker() -> Option<Worker> {
	match Worker::new(Config::default()) {
		Ok(worker) => Some(worker),
		Err(Error::Unsupported(reason)) if std::env::var_os("CI_STRICT").is_none() => {
			eprintln!("skipping io_uring session test: {reason}");
			None
		}
		Err(err) => panic!("worker setup failed: {err}"),
	}
}

const ALPN: &str = "moq-lite-05";
const PAYLOAD: &[u8] = b"hello over io_uring";

/// A millisecond track: the frames written here carry timestamps.
fn timed() -> moq_net::track::Info {
	moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI)
}

#[test]
fn lite_session_over_the_worker() {
	let Some(mut worker) = worker() else { return };
	let handle = worker.handle();

	// The model and its origins, driven on a tokio thread (see module docs).
	let (pub_origin, pub_driver) = origin::Producer::new(origin::Config::default());
	let (sub_origin, sub_driver) = origin::Producer::new(origin::Config::default());
	let origins = std::thread::spawn(move || {
		let rt = tokio::runtime::Builder::new_current_thread()
			.enable_time()
			.build()
			.expect("tokio runtime");
		rt.block_on(async move {
			tokio::join!(moq_net::time::run(pub_driver), moq_net::time::run(sub_driver));
		});
	});

	// Content ready before anyone connects: one broadcast, one track, one
	// finished group.
	let broadcast = pub_origin
		.route("test", moq_net::origin::RouteOptions::default())
		.expect("create broadcast");
	let track = broadcast
		.broadcast()
		.create_track("data", timed())
		.expect("create track");
	let mut group = track
		.append_group(track.largest().map_or(0, |largest| largest.group + 1))
		.expect("append group");
	group
		.write_frame(moq_net::Timestamp::from_millis(group.start).unwrap(), PAYLOAD)
		.expect("write frame");
	group.close().expect("finish group");

	let certs = support::certs().expect("certificates");
	let server_config = quic::server::Config::new(certs.server(&[ALPN]));

	let server_sock = handle
		.udp(UdpSocket::bind("127.0.0.1:0").expect("bind"), udp::Config::default())
		.expect("server socket");
	let server_addr = server_sock.local_addr().expect("server addr");
	let client_sock = handle
		.udp(UdpSocket::bind("127.0.0.1:0").expect("bind"), udp::Config::default())
		.expect("client socket");
	// The server's certificate is our own self-signed one; this is loopback.
	let dial = quic::client::Config::new(support::client_tls(&[ALPN]));

	// The publisher session runs as a worker task for the life of the test.
	let server_handle = handle.clone();
	handle.spawn(async move {
		let conn = quic::server::accept(server_sock, &server_config)
			.await
			.expect("quic accept");
		let (session, driver) = moq_net::Server::new()
			.with_publisher(&pub_origin)
			.accept(
				moq_net::time::Instant::now(),
				moq_uring::transport::Local::new(quic::web::Session::raw(conn)),
			)
			.await
			.expect("accept");
		let _ = server_handle.run(driver).await;
		// Serve until the client walks away.
		session.closed().await;
	});

	let sub = sub_origin.clone();
	let payload = worker
		.block_on(async move {
			let conn = quic::client::connect(client_sock, &dial, server_addr, "localhost")
				.await
				.expect("quic connect");
			assert_eq!(conn.protocol(), Some(ALPN), "negotiated ALPN");

			let (session, driver) = moq_net::Client::new()
				.with_subscriber(sub.clone())
				.connect(
					moq_net::time::Instant::now(),
					moq_uring::transport::Local::new(quic::web::Session::raw(conn)),
				)
				.await
				.expect("connect");
			let task_handle = handle.clone();
			handle.spawn(async move {
				let _ = task_handle.run(driver).await;
			});

			let bc = {
				let consumer = sub.consume();
				consumer
					.follow("test", None)
					.unwrap()
					.available()
					.await
					.expect("broadcast resolves")
					.unwrap()
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

	let (pub_origin, pub_driver) = origin::Producer::new(origin::Config::default());
	let (sub_a, sub_a_driver) = origin::Producer::new(origin::Config::default());
	let (sub_b, sub_b_driver) = origin::Producer::new(origin::Config::default());
	let origins = std::thread::spawn(move || {
		let rt = tokio::runtime::Builder::new_current_thread()
			.enable_time()
			.build()
			.expect("tokio runtime");
		rt.block_on(async move {
			tokio::join!(
				moq_net::time::run(pub_driver),
				moq_net::time::run(sub_a_driver),
				moq_net::time::run(sub_b_driver),
			);
		});
	});

	let broadcast = pub_origin
		.route("test", moq_net::origin::RouteOptions::default())
		.expect("create broadcast");
	let track = broadcast
		.broadcast()
		.create_track("data", timed())
		.expect("create track");
	let mut group = track
		.append_group(track.largest().map_or(0, |largest| largest.group + 1))
		.expect("append group");
	group
		.write_frame(moq_net::Timestamp::from_millis(group.start).unwrap(), PAYLOAD)
		.expect("write frame");
	group.close().expect("finish group");

	let certs = support::certs().expect("certificates");
	let server_config = quic::server::Config::new(certs.server(&[ALPN]));

	let server_sock = handle
		.udp(UdpSocket::bind("127.0.0.1:0").expect("bind"), udp::Config::default())
		.expect("server socket");
	let endpoint = quic::Endpoint::new(
		quic::endpoint::Config::default().with_server(server_config),
		server_sock,
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
				let (session, driver) = moq_net::Server::new()
					.with_publisher(&pub_origin)
					.accept(
						moq_net::time::Instant::now(),
						moq_uring::transport::Local::new(quic::web::Session::raw(conn)),
					)
					.await
					.expect("accept");
				let _ = session_handle.run(driver).await;
				session.closed().await;
			});
		}
	});

	let dial = quic::client::Config::new(support::client_tls(&[ALPN]));

	let subs = [sub_a.clone(), sub_b.clone()];
	worker
		.block_on(async move {
			for sub in subs {
				let client_sock = handle
					.udp(UdpSocket::bind("127.0.0.1:0").expect("bind"), udp::Config::default())
					.expect("client socket");
				let conn = quic::client::connect(client_sock, &dial, server_addr, "localhost")
					.await
					.expect("quic connect");
				let (session, driver) = moq_net::Client::new()
					.with_subscriber(sub.clone())
					.connect(
						moq_net::time::Instant::now(),
						moq_uring::transport::Local::new(quic::web::Session::raw(conn)),
					)
					.await
					.expect("connect");
				let task_handle = handle.clone();
				handle.spawn(async move {
					let _ = task_handle.run(driver).await;
				});

				let bc = {
					let consumer = sub.consume();
					consumer
						.follow("test", None)
						.unwrap()
						.available()
						.await
						.expect("broadcast resolves")
						.unwrap()
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
