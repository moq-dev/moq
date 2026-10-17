//! Over the worker's QUIC path, incomplete frames hold the subscriber's connection
//! credit, and when they fill its window the subscriber stops the least urgent stream
//! and keeps going.
//!
//! The subscriber's connection window is far smaller than a moq session would allow,
//! which keeps the payloads small. Without recovery the high-priority frame never
//! arrives: the window is spent on a low-priority frame that can never complete.
//!
//! Unsupported local kernels skip loudly; MOQ_STRICT makes CI refuse them.

#![cfg(all(target_os = "linux", feature = "quic"))]

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
		Err(Error::Unsupported(reason)) if std::env::var_os("MOQ_STRICT").is_none() => {
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
	let mut dial = quic::client::Config::new(server_sock.local_addr().expect("server addr"), "localhost");
	dial.alpn = vec![ALPN.to_string()];
	dial.verify = false;
	dial.transport.receive_window = Some(WINDOW as u64);
	let client_sock = socket(&handle);

	let server_handle = handle.clone();
	handle.spawn(async move {
		let conn = quic::server::accept(server_sock, &server_config)
			.await
			.expect("quic accept");
		let (session, driver) = moq_net::Server::new()
			.with_publisher(&pub_origin)
			.accept_lite(
				moq_net::time::now(),
				moq_uring::transport::Session::new(quic::web::Session::raw(conn), moq_net::transport::Binding::InBand),
			)
			.await
			.expect("accept_lite");
		let _ = server_handle.run(driver).await;
		session.closed().await;
	});

	let sub = sub_origin.clone();
	worker
		.block_on(async move {
			let conn = quic::client::connect(client_sock, &dial).await.expect("quic connect");
			let (session, driver) = moq_net::Client::new()
				.with_subscriber(sub.clone())
				.connect_lite(
					moq_net::time::now(),
					moq_uring::transport::Session::new(
						quic::web::Session::raw(conn),
						moq_net::transport::Binding::InBand,
					),
				)
				.await
				.expect("connect_lite");
			let task_handle = handle.clone();
			handle.spawn(async move {
				let _ = task_handle.run(driver).await;
			});

			let consumer = sub.consume();
			within(&handle, "the route", consumer.routed("room"))
				.await
				.expect("routed");
			let remote = within(&handle, "the broadcast", consumer.request_broadcast("room", None))
				.await
				.expect("broadcast resolves");
			let subscribe = |name: &str, priority: u8| {
				let track = remote.track(name).expect("track");
				async move {
					track
						.subscribe(Subscription::default().with_priority(priority))
						.await
						.expect("subscribe")
				}
			};
			let mut low_sub = within(&handle, "the low subscription", subscribe("low", 1)).await;
			let mut high_sub = within(&handle, "the high subscription", subscribe("high", 200)).await;

			// Three quarters of the window, held by a frame that never finishes.
			let mut low_group = low.append_group().expect("low group");
			let mut low_frame = low_group
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
			drop(low_group);
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
