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

#![cfg(target_os = "linux")]

#[path = "support/quiche.rs"]
mod support;

use std::net::UdpSocket;
use std::pin::Pin;
use std::task::Poll;

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

/// A `Send` [`moq_net::runtime::Timers`] over tokio, for the origin drivers.
#[derive(Clone, Default)]
struct TokioTimers;

impl moq_net::runtime::Timers for TokioTimers {
	type Timer = TokioTimer;

	fn timer(&self) -> Self::Timer {
		TokioTimer { at: None, sleep: None }
	}

	fn now(&self) -> moq_net::runtime::Instant {
		tokio::time::Instant::now().into_std()
	}
}

struct TokioTimer {
	at: Option<moq_net::runtime::Instant>,
	// Allocated on the first poll after arming, then re-armed in place;
	// construction panics without a live tokio time driver.
	sleep: Option<Pin<Box<tokio::time::Sleep>>>,
}

impl moq_net::runtime::Timer for TokioTimer {
	fn set(&mut self, at: Option<moq_net::runtime::Instant>) {
		self.at = at;
		if let (Some(at), Some(sleep)) = (at, &mut self.sleep) {
			sleep.as_mut().reset(tokio::time::Instant::from_std(at));
		}
	}

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		let Some(at) = self.at else { return Poll::Pending };
		let sleep = self
			.sleep
			.get_or_insert_with(|| Box::pin(tokio::time::sleep_until(tokio::time::Instant::from_std(at))));
		if sleep.is_elapsed() {
			return Poll::Ready(());
		}
		waiter.poll_future(sleep.as_mut())
	}
}

const ALPN: &str = "moq-lite-05";
const PAYLOAD: &[u8] = b"hello over io_uring";

#[test]
fn lite_session_over_the_worker() {
	let Some(mut worker) = worker() else { return };
	let handle = worker.handle();

	// The model and its origins, driven on a tokio thread (see module docs).
	let (pub_origin, pub_driver) = origin::Producer::new(origin::Info::new(moq_net::Origin::random()));
	let (sub_origin, sub_driver) = origin::Producer::new(origin::Info::new(moq_net::Origin::random()));
	let origins = std::thread::spawn(move || {
		let rt = tokio::runtime::Builder::new_current_thread()
			.enable_time()
			.build()
			.expect("tokio runtime");
		rt.block_on(async move {
			tokio::join!(pub_driver.run(TokioTimers), sub_driver.run(TokioTimers));
		});
	});

	// Content ready before anyone connects: one broadcast, one track, one
	// finished group.
	let mut broadcast = pub_origin
		.create_broadcast("test", moq_net::broadcast::Route::new().with_announce(true))
		.expect("create broadcast");
	let mut track = broadcast.create_track("data", None).expect("create track");
	let mut group = track.append_group().expect("append group");
	group
		.write_frame(moq_net::Timestamp::ZERO, PAYLOAD)
		.expect("write frame");
	group.finish().expect("finish group");

	let certs = support::certs().expect("certificates");
	let mut server_config = quic::Config::default();
	server_config.alpn = vec![ALPN.to_string()];
	server_config.cert = Some(certs.cert.clone());
	server_config.key = Some(certs.key.clone());
	let mut client_config = quic::Config::default();
	client_config.alpn = vec![ALPN.to_string()];
	// The server's certificate is our own self-signed one; this is loopback.
	client_config.verify_peer = false;

	let server_sock = handle
		.udp(UdpSocket::bind("127.0.0.1:0").expect("bind"), udp::Config::default())
		.expect("server socket");
	let server_addr = server_sock.local_addr().expect("server addr");
	let client_sock = handle
		.udp(UdpSocket::bind("127.0.0.1:0").expect("bind"), udp::Config::default())
		.expect("client socket");

	// The publisher session runs as a worker task for the life of the test.
	let server_handle = handle.clone();
	handle.spawn(async move {
		let conn = quic::accept(&server_handle, server_sock, &server_config)
			.await
			.expect("quic accept");
		let session = moq_net::Server::new()
			.with_publisher(&pub_origin)
			.accept_lite(server_handle.clone(), conn)
			.await
			.expect("accept_lite");
		// Serve until the client walks away.
		session.closed().await;
	});

	let sub = sub_origin.clone();
	let payload = worker
		.block_on(async move {
			let conn = quic::connect(&handle, client_sock, "localhost", server_addr, &client_config)
				.await
				.expect("quic connect");
			assert_eq!(
				web_transport_trait::poll::Session::protocol(&conn),
				Some(ALPN),
				"negotiated ALPN"
			);

			let session = moq_net::Client::new()
				.with_subscriber(sub.clone())
				.connect_lite(handle.clone(), conn)
				.await
				.expect("connect_lite");

			let bc = sub
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
