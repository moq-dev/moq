//! The PR-4 ablation matrix at session level: a full moq-lite session pair on
//! one worker, publisher to subscriber over loopback QUIC, toggling receive
//! batching, GRO, and GSO exactly like the raw echo. Each call delivers
//! one group of 32 x 32 KiB frames (1 MiB, matching `echo_quic`'s unit), so
//! the two matrices are directly comparable: the difference is the moq-net
//! machine and container framing on top of the same wire path.
//!
//! Time it with `just bench -- -p moq-uring --bench session_lite` on a Linux 6.12+ kernel.

#[cfg(all(target_os = "linux", feature = "quic"))]
#[path = "../tests/integration/support.rs"]
mod support;

#[cfg(all(target_os = "linux", feature = "quic"))]
mod linux {
	use std::fmt;
	use std::net::UdpSocket;
	use std::thread::JoinHandle;

	use moq_net::origin;
	use moq_uring::{Config, Worker, quic, udp};

	use super::support;

	/// Frames per group and bytes per frame: 1 MiB per call, the same
	/// unit as the echo matrix.
	pub const FRAMES: usize = 32;
	pub const FRAME_SIZE: usize = 32 * 1024;

	const ALPN: &str = "moq-lite-05";

	/// A millisecond track: the frames written here carry timestamps.
	fn timed() -> moq_net::track::Info {
		moq_net::track::Info::default()
			.with_max_age(None)
			.with_timescale(moq_net::Timescale::MILLI)
	}

	/// Which UDP features the worker's sockets use.
	#[derive(Clone, Copy)]
	pub enum Ablation {
		AllOn,
		NoGso,
		NoGro,
		Oneshot,
		AllOff,
	}

	impl Ablation {
		pub const ALL: [Self; 5] = [Self::AllOn, Self::NoGso, Self::NoGro, Self::Oneshot, Self::AllOff];

		fn config(self) -> udp::Config {
			let mut config = udp::Config::default();
			match self {
				Self::AllOn => {}
				Self::NoGso => config.gso = false,
				Self::NoGro => config.gro = false,
				Self::Oneshot => config.multishot = false,
				Self::AllOff => {
					config.gso = false;
					config.gro = false;
					config.multishot = false;
				}
			}
			config
		}
	}

	impl fmt::Display for Ablation {
		fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
			f.write_str(match self {
				Self::AllOn => "all-on",
				Self::NoGso => "no-gso",
				Self::NoGro => "no-gro",
				Self::Oneshot => "oneshot",
				Self::AllOff => "all-off",
			})
		}
	}

	/// A publisher and subscriber session pair on one worker, subscribed once so each group
	/// measures steady state.
	pub struct Session {
		worker: Option<Worker>,
		track: Option<moq_net::track::Producer>,
		broadcast: Option<moq_net::origin::Route>,
		sub_origin: Option<origin::Producer>,
		origins: Option<JoinHandle<()>>,
		frame: bytes::Bytes,
		sub: moq_net::track::Subscriber,
		_session: moq_net::Session,
	}

	impl Session {
		pub fn new(ablation: Ablation) -> Self {
			let mut worker = Worker::new(Config::default()).expect("io_uring worker");
			let handle = worker.handle();

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

			let broadcast = pub_origin
				.route("bench", moq_net::origin::RouteOptions::default())
				.expect("create broadcast");
			let track = broadcast
				.broadcast()
				.create_track("data", timed())
				.expect("create track");

			let certs = support::certs().expect("certificates");
			let server_config = quic::server::Config::new(certs.server(&[ALPN]));

			let server_sock = handle
				.udp(UdpSocket::bind("127.0.0.1:0").expect("bind"), ablation.config())
				.expect("server socket");
			let server_addr = server_sock.local_addr().expect("server addr");
			let client_sock = handle
				.udp(UdpSocket::bind("127.0.0.1:0").expect("bind"), ablation.config())
				.expect("client socket");
			let dial = quic::client::Config::new(support::client_tls(&[ALPN]));

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
				session.closed().await;
			});

			let (session, sub) = worker
				.block_on(async {
					let conn = quic::client::connect(client_sock, &dial, server_addr, "localhost")
						.await
						.expect("quic connect");
					let (session, driver) = moq_net::Client::new()
						.with_subscriber(sub_origin.clone())
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
						let consumer = sub_origin.consume();
						consumer
							.follow("bench", None)
							.unwrap()
							.available()
							.await
							.expect("broadcast resolves")
							.unwrap()
					};
					let sub = bc
						.track("data")
						.expect("track")
						.subscribe(None)
						.await
						.expect("subscribe");
					(session, sub)
				})
				.expect("worker");

			Self {
				worker: Some(worker),
				track: Some(track),
				broadcast: Some(broadcast),
				sub_origin: Some(sub_origin),
				origins: Some(origins),
				frame: bytes::Bytes::from(vec![0x5au8; FRAME_SIZE]),
				sub,
				_session: session,
			}
		}

		/// Publishes one group and reads it back through the subscriber.
		pub fn group(&mut self) {
			let track = self.track.as_mut().expect("track");
			let sub = &mut self.sub;
			let frame = &self.frame;
			self.worker
				.as_mut()
				.expect("worker")
				.block_on(async {
					let mut writer = track
						.append_group(
							moq_net::Timestamp::from_millis(track.largest().map_or(0, |largest| largest.group + 1))
								.unwrap(),
						)
						.expect("append group");
					for _ in 0..FRAMES {
						writer
							.write_frame(moq_net::Timestamp::from_millis(writer.start).unwrap(), frame.clone())
							.expect("write frame");
					}
					writer.finish().expect("finish group");

					let mut reader = sub
						.recv_group()
						.await
						.expect("recv group")
						.expect("track closed prematurely");
					let mut frames = 0;
					while let Some(frame) = reader.read_frame().await.expect("read frame") {
						assert_eq!(frame.payload.len(), FRAME_SIZE, "frame size");
						frames += 1;
					}
					assert_eq!(frames, FRAMES, "lost frames");
				})
				.expect("worker");
		}
	}

	impl Drop for Session {
		fn drop(&mut self) {
			// The worker (and with it the publisher session task) drops first,
			// releasing the last origin handles so the drivers resolve.
			drop(self.worker.take());
			drop(self.track.take());
			drop(self.broadcast.take());
			drop(self.sub_origin.take());
			if let Some(origins) = self.origins.take() {
				origins.join().expect("origin drivers");
			}
		}
	}
}

#[cfg(all(target_os = "linux", feature = "quic"))]
use linux::{Ablation, FRAME_SIZE, FRAMES, Session};
#[cfg(all(target_os = "linux", feature = "quic"))]
use swole::bench;

#[cfg(all(target_os = "linux", feature = "quic"))]
#[bench(args = Ablation::ALL, setup = Session::new, bytes = |_: Ablation| FRAMES * FRAME_SIZE)]
fn session_lite(session: &mut Session) {
	session.group();
}

fn main() {
	swole::main();
}
