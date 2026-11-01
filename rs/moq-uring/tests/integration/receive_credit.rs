//! Over the worker's QUIC path, incomplete frames hold the subscriber's connection
//! credit, and when they fill its window the subscriber stops the least urgent stream
//! and keeps going.
//!
//! The subscriber's connection window is far smaller than a moq session would allow,
//! which keeps the payloads small. Without recovery the high-priority frame never
//! arrives: the window is spent on a low-priority frame that can never complete. The
//! JavaScript arm serves WebTransport to a Bun publisher; it needs Bun and runs in the
//! interop workflow.
//!
//! Unsupported local kernels skip loudly; CI_STRICT makes CI refuse them.

#![cfg(feature = "quic")]

use crate::support;

use std::net::UdpSocket;
use std::task::Poll;
use std::time::Duration;

use bytes::Bytes;
use moq_net::{Timestamp, frame, origin, track::Subscription};
use moq_uring::{Config, Error, Worker, quic, udp};

const ALPN: &str = "moq-lite-06";
/// The subscriber's connection receive window.
const WINDOW: usize = 256 * 1024;

fn worker() -> Option<Worker> {
	match Worker::new(Config::default()) {
		Ok(worker) => Some(worker),
		Err(Error::Unsupported(reason)) if std::env::var_os("CI_STRICT").is_none() => {
			eprintln!("skipping io_uring receive credit test: {reason}");
			None
		}
		Err(err) => panic!("worker setup failed: {err}"),
	}
}

fn timed() -> moq_net::track::Info {
	moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI)
}

/// Await `future`, failing loudly rather than hanging if it never resolves.
async fn within<T>(handle: &moq_uring::Handle, what: &str, future: impl Future<Output = T>) -> T {
	let mut deadline = moq_uring::Timer::after(handle, Duration::from_secs(10));
	let mut future = std::pin::pin!(future);
	kio::wait(|waiter| {
		let mut cx = waiter.context();
		if let Poll::Ready(value) = future.as_mut().poll(&mut cx) {
			return Poll::Ready(Some(value));
		}
		deadline.poll(waiter).map(|()| None)
	})
	.await
	.unwrap_or_else(|| panic!("timed out waiting for {what}"))
}

/// The next frame's payload.
async fn next_frame(handle: &moq_uring::Handle, sub: &mut moq_net::track::Subscriber) -> Bytes {
	let mut group = within(handle, "a group", sub.recv_group())
		.await
		.expect("group")
		.expect("track ended");
	within(handle, "a frame", group.read_frame())
		.await
		.expect("frame")
		.expect("group ended")
		.payload
}

/// Two partial frames that together exceed the window: the lower-priority one is
/// stopped, and the higher-priority one completes on the credit it returns.
#[test]
fn partial_frame_gives_way() {
	let Some(mut worker) = worker() else { return };
	let handle = worker.handle();

	// The model and its origins, driven on a tokio thread as in `session.rs`.
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

	let broadcast = pub_origin.create_broadcast("room").expect("create broadcast");
	broadcast.announce(Default::default()).expect("announce");
	let low = broadcast.create_track("low", timed()).expect("low track");
	let high = broadcast.create_track("high", timed()).expect("high track");

	let certs = support::certs().expect("certificates");
	let server_config = quic::server::Config::new(
		quic::Identity::open(&certs.cert, &certs.key)
			.expect("identity")
			.server(&[ALPN], quic::server::ClientAuth::None)
			.expect("tls"),
	);
	let socket = |handle: &moq_uring::Handle| {
		handle
			.udp(UdpSocket::bind("127.0.0.1:0").expect("bind"), udp::Config::default())
			.expect("socket")
	};
	let server_sock = socket(&handle);
	let server_addr = server_sock.local_addr().expect("server addr");
	let mut dial = quic::client::Config::default();
	dial.alpn = vec![ALPN.to_string()];
	dial.verify = false;
	dial.transport.limits.receive_window = std::num::NonZeroU64::new(WINDOW as u64);
	let client_sock = socket(&handle);

	let server_handle = handle.clone();
	handle.spawn(async move {
		let conn = quic::server::accept(server_sock, &server_config)
			.await
			.expect("quic accept");
		let (session, driver) = moq_net::Server::new()
			.with_publisher(&pub_origin)
			.accept(
				moq_net::time::Instant::now(),
				moq_uring::transport::Local::new(moq_uring::transport::Session::new(
					quic::web::Session::raw(conn),
					moq_net::transport::Address::Setup,
				)),
			)
			.await
			.expect("accept");
		let _ = server_handle.run(driver).await;
		session.closed().await;
	});

	let sub = sub_origin.clone();
	worker
		.block_on(async move {
			let conn = quic::client::connect(client_sock, &dial, server_addr, "localhost")
				.await
				.expect("quic connect");
			let (session, driver) = moq_net::Client::new()
				.with_subscriber(sub.clone())
				.connect(
					moq_net::time::Instant::now(),
					moq_uring::transport::Local::new(moq_uring::transport::Session::new(
						quic::web::Session::raw(conn),
						moq_net::transport::Address::Setup,
					)),
				)
				.await
				.expect("connect");
			let task_handle = handle.clone();
			handle.spawn(async move {
				let _ = task_handle.run(driver).await;
			});

			let consumer = sub.consume();
			let remote = within(
				&handle,
				"the broadcast",
				consumer.follow("room", None).unwrap().available(),
			)
			.await
			.expect("broadcast resolves");
			let subscribe = |name: &str, priority: i8| {
				let track = remote.track(name).expect("track");
				async move {
					track
						.subscribe(Subscription::default().with_priority(priority))
						.await
						.expect("subscribe")
				}
			};
			let mut low_sub = within(&handle, "the low subscription", subscribe("low", 1)).await;
			let mut high_sub = within(&handle, "the high subscription", subscribe("high", 100)).await;

			// Three quarters of the window, held by a frame that never finishes.
			let mut low_frame = low
				.append_group()
				.expect("low group")
				.create_frame(frame::Info {
					size: WINDOW as u64,
					timestamp: Some(Timestamp::ZERO),
				})
				.expect("low frame");
			low_frame.write(vec![1u8; WINDOW * 3 / 4]).expect("partial payload");

			// Wait until the subscriber holds all of it, so nothing else got there first.
			let mut partial = within(&handle, "the low group", low_sub.recv_group())
				.await
				.expect("group")
				.expect("track ended");
			let mut partial = within(&handle, "the low frame header", partial.next_frame())
				.await
				.expect("frame")
				.expect("group ended");
			let mut seen = 0;
			while seen < WINDOW * 3 / 4 {
				seen += within(&handle, "the partial payload", partial.read_chunk())
					.await
					.expect("chunk")
					.expect("frame ended")
					.len();
			}

			// Half the window, which only fits once the low frame lets go.
			let payload = Bytes::from(vec![2u8; WINDOW / 2]);
			let mut high_group = high.append_group().expect("high group");
			high_group
				.write_frame(Timestamp::ZERO, payload.clone())
				.expect("high frame");
			high_group.finish().expect("finish high");
			assert_eq!(next_frame(&handle, &mut high_sub).await, payload);

			// The session survives, and the low track keeps delivering what fits.
			drop(low_frame);
			let mut group = low.append_group().expect("after group");
			group.write_frame(Timestamp::ZERO, &b"after"[..]).expect("after frame");
			group.finish().expect("finish after");
			assert_eq!(next_frame(&handle, &mut low_sub).await, &b"after"[..]);

			session.abort(moq_net::Error::Cancel);
		})
		.expect("worker");

	drop(worker);
	drop(broadcast);
	drop(sub_origin);
	origins.join().expect("origin drivers");
}

/// A Bun publisher (`test/receive-credit/sender.ts`) dials a WebTransport endpoint whose
/// connection window is [`WINDOW`] and sends a low-priority frame twice that size. The
/// subscriber stops it, a high-priority frame completes, and a later low group arrives.
/// The worker speaks moq-lite only.
#[test]
#[ignore = "requires Bun; run by just test receive-credit in the interop workflow"]
fn javascript_partial_frame_gives_way() {
	use std::io::Write;

	let Some(mut worker) = worker() else { return };
	let handle = worker.handle();

	let (sub_origin, sub_driver) = origin::Producer::new(origin::Config::default());
	let origins = std::thread::spawn(move || {
		let rt = tokio::runtime::Builder::new_current_thread()
			.enable_time()
			.build()
			.expect("tokio runtime");
		rt.block_on(moq_net::time::run(sub_driver));
	});

	let certs = support::certs().expect("certificates");
	let mut server = quic::server::Config::new(
		quic::Identity::open(&certs.cert, &certs.key)
			.expect("identity")
			.server(&["h3"], quic::server::ClientAuth::None)
			.expect("tls"),
	);
	server.transport.limits.receive_window = std::num::NonZeroU64::new(WINDOW as u64);
	let sock = handle
		.udp(UdpSocket::bind("127.0.0.1:0").expect("bind"), udp::Config::default())
		.expect("socket");
	let endpoint = quic::Endpoint::new(sock, quic::endpoint::Config::default().with_server(server)).expect("endpoint");

	let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../test/receive-credit/sender.ts");
	let mut child = std::process::Command::new("bun")
		.arg(script)
		.arg(format!("https://{}/", endpoint.local_addr()))
		.arg(ALPN)
		.arg(&certs.cert)
		.arg(WINDOW.to_string())
		.stdin(std::process::Stdio::piped())
		.stdout(std::process::Stdio::null())
		.stderr(std::process::Stdio::inherit())
		.spawn()
		.expect("spawn bun");
	let mut commands = child.stdin.take().unwrap();
	// Waiting would block the worker, so a thread waits and reports back.
	let (exited, exit) = tokio::sync::oneshot::channel();
	let waiter = std::thread::spawn(move || {
		let _ = exited.send(child.wait());
	});

	let sub = sub_origin.clone();
	worker
		.block_on(async move {
			let conn = within(&handle, "the connection", endpoint.accept())
				.await
				.expect("accept");
			let request = within(&handle, "the CONNECT", quic::web::Request::accept(conn))
				.await
				.expect("handshake");
			let session = request
				.respond(quic::web::Response::default().with_protocol(ALPN))
				.await
				.expect("respond");
			let (session, driver) = moq_net::Server::new()
				.with_subscriber(sub.clone())
				.accept(
					moq_net::time::Instant::now(),
					moq_uring::transport::Local::new(moq_uring::transport::Session::new(
						session,
						moq_net::transport::Address::Uri,
					)),
				)
				.await
				.expect("accept");
			let task_handle = handle.clone();
			handle.spawn(async move {
				let _ = task_handle.run(driver).await;
			});

			let consumer = sub.consume();
			let remote = within(
				&handle,
				"the broadcast",
				consumer.follow("room", None).unwrap().available(),
			)
			.await
			.expect("broadcast resolves");
			let subscribe = |name: &str, priority: i8| {
				let track = remote.track(name).expect("track");
				async move {
					track
						.subscribe(Subscription::default().with_priority(priority))
						.await
						.expect("subscribe")
				}
			};
			let mut low = within(&handle, "the low subscription", subscribe("low", 1)).await;
			let mut high = within(&handle, "the high subscription", subscribe("high", 100)).await;

			// Wait until three quarters of the window holds a frame that can never complete.
			commands.write_all(b"low\n").expect("command");
			let mut partial = within(&handle, "the low group", low.recv_group())
				.await
				.expect("group")
				.expect("track ended");
			let mut partial = within(&handle, "the low frame header", partial.next_frame())
				.await
				.expect("frame")
				.expect("group ended");
			let mut seen = 0;
			while seen < WINDOW * 3 / 4 {
				seen += within(&handle, "the partial payload", partial.read_chunk())
					.await
					.expect("chunk")
					.expect("frame ended")
					.len();
			}

			// Half the window, which only fits once the low frame lets go.
			commands.write_all(b"high\n").expect("command");
			assert_eq!(next_frame(&handle, &mut high).await, vec![2u8; WINDOW / 2]);
			loop {
				match within(&handle, "the low stop", partial.read_chunk()).await {
					Ok(Some(_)) => {}
					Ok(None) => panic!("a frame larger than the window completed"),
					Err(_) => break,
				}
			}

			// The session survives, and the low track keeps delivering what fits.
			commands.write_all(b"after\n").expect("command");
			assert_eq!(next_frame(&handle, &mut low).await, &b"after"[..]);

			drop(commands);
			let status = within(&handle, "the sender to exit", exit)
				.await
				.expect("waiter")
				.expect("sender");
			assert!(status.success(), "JS sender failed");
			drop(session);
		})
		.expect("worker");

	waiter.join().expect("waiter");
	drop(worker);
	drop(sub_origin);
	origins.join().expect("origin driver");
}
