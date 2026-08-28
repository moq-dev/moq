//! The PR-4 ablation matrix at session level: a full moq-lite session pair on
//! one worker, publisher to subscriber over loopback QUIC, toggling receive
//! batching, GRO, and GSO exactly like the raw echo. Each iteration delivers
//! one group of 32 x 32 KiB frames (1 MiB, matching `echo_quiche`'s unit), so
//! the two matrices are directly comparable: the difference is the moq-net
//! machine and container framing on top of the same wire path.
//!
//! Run it with `just rs bench-session` on a Linux 6.12+ kernel.

use criterion::{criterion_group, criterion_main};

#[cfg(target_os = "linux")]
#[path = "../tests/support/quiche.rs"]
mod support;

#[cfg(target_os = "linux")]
mod linux {
	use std::net::UdpSocket;
	use std::pin::Pin;
	use std::task::Poll;
	use std::time::Instant;

	use criterion::{BenchmarkId, Criterion, Throughput};
	use moq_net::origin;
	use moq_uring::{Config, Error, Worker, quic, udp};

	use super::support;

	/// Frames per group and bytes per frame: 1 MiB per iteration, the same
	/// unit as the echo matrix.
	const FRAMES: usize = 32;
	const FRAME_SIZE: usize = 32 * 1024;

	const ALPN: &str = "moq-lite-05";

	/// A `Send` timers impl over tokio for the origin drivers, which cannot
	/// take the worker's `!Send` handle (see `tests/session.rs`).
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

	struct Ablation {
		name: &'static str,
		config: udp::Config,
	}

	fn ablations() -> Vec<Ablation> {
		let all = udp::Config::default();
		let mut no_gso = all.clone();
		no_gso.gso = false;
		let mut no_gro = all.clone();
		no_gro.gro = false;
		let mut oneshot = all.clone();
		oneshot.multishot = false;
		let mut none = all.clone();
		none.gso = false;
		none.gro = false;
		none.multishot = false;
		vec![
			Ablation {
				name: "all-on",
				config: all,
			},
			Ablation {
				name: "no-gso",
				config: no_gso,
			},
			Ablation {
				name: "no-gro",
				config: no_gro,
			},
			Ablation {
				name: "oneshot",
				config: oneshot,
			},
			Ablation {
				name: "all-off",
				config: none,
			},
		]
	}

	pub fn benchmark(c: &mut Criterion) {
		// Kernel-gated like the tests: skip loudly below the 6.12 floor.
		let probe = match Worker::new(Config::default()) {
			Ok(worker) => worker,
			Err(Error::Unsupported(reason)) => {
				eprintln!("skipping io_uring session benchmark: {reason}");
				return;
			}
			Err(err) => panic!("worker setup failed: {err}"),
		};
		drop(probe);

		let mut group = c.benchmark_group("session_lite");
		group.throughput(Throughput::Bytes((FRAMES * FRAME_SIZE) as u64));

		for ablation in ablations() {
			let mut worker = Worker::new(Config::default()).expect("worker");
			let handle = worker.handle();

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

			let mut broadcast = pub_origin
				.create_broadcast("bench", moq_net::broadcast::Route::new().with_announce(true))
				.expect("create broadcast");
			let mut track = broadcast.create_track("data", None).expect("create track");

			let certs = support::certs().expect("certificates");
			let mut server_config =
				quic::server::Config::new(quic::Identity::open(&certs.cert, &certs.key).expect("identity"));
			server_config.alpn = vec![ALPN.to_string()];

			let server_sock = handle
				.udp(UdpSocket::bind("127.0.0.1:0").expect("bind"), ablation.config.clone())
				.expect("server socket");
			let server_addr = server_sock.local_addr().expect("server addr");
			let client_sock = handle
				.udp(UdpSocket::bind("127.0.0.1:0").expect("bind"), ablation.config.clone())
				.expect("client socket");
			let mut dial = quic::client::Config::new(server_addr, "localhost");
			dial.alpn = vec![ALPN.to_string()];
			dial.verify = false;

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
				session.closed().await;
			});

			// Establish and subscribe once; iterations measure steady state.
			let (_session, mut sub) = worker
				.block_on(async {
					let conn = quic::client::connect(&handle, client_sock, &dial)
						.await
						.expect("quic connect");
					let session = moq_net::Client::new()
						.with_subscriber(sub_origin.clone())
						.connect_lite(handle.clone(), quic::web::Session::raw(conn))
						.await
						.expect("connect_lite");
					let bc = sub_origin
						.consume()
						.announced_broadcast("bench")
						.await
						.expect("broadcast announced");
					let sub = bc
						.track("data")
						.expect("track")
						.subscribe(None)
						.await
						.expect("subscribe");
					(session, sub)
				})
				.expect("worker");

			let frame = bytes::Bytes::from(vec![0x5au8; FRAME_SIZE]);

			group.bench_with_input(BenchmarkId::from_parameter(ablation.name), &ablation.config, |b, _| {
				b.iter_custom(|iterations| {
					worker
						.block_on(async {
							let start = Instant::now();
							for _ in 0..iterations {
								let mut writer = track.append_group().expect("append group");
								for _ in 0..FRAMES {
									writer
										.write_frame(moq_net::Timestamp::ZERO, frame.clone())
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
							}
							start.elapsed()
						})
						.expect("worker")
				});
			});

			// The worker (and with it the publisher session task) drops first,
			// releasing the last origin handles so the drivers resolve.
			drop(worker);
			drop(track);
			drop(broadcast);
			drop(sub_origin);
			origins.join().expect("origin drivers");
		}

		group.finish();
	}
}

#[cfg(target_os = "linux")]
use linux::benchmark;

#[cfg(not(target_os = "linux"))]
fn benchmark(_: &mut criterion::Criterion) {}

criterion_group!(benches, benchmark);
criterion_main!(benches);
