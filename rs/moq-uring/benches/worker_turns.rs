//! Worker submission and completion amortization across sockets and burst sizes.

use criterion::{criterion_group, criterion_main};

#[cfg(all(target_os = "linux", feature = "quic"))]
#[path = "../tests/integration/support.rs"]
mod support;

/// A request and its reply traverse demux, connection and application tasks.
/// Sweep connections and reply bytes so batching cannot hide a fanout cost.
#[cfg(all(target_os = "linux", feature = "quic"))]
#[expect(
	clippy::disallowed_methods,
	clippy::disallowed_types,
	reason = "benchmarks measure themselves on the host clock"
)]
fn request_reply(c: &mut criterion::Criterion) {
	use std::{net::UdpSocket, num::NonZeroUsize, time::Instant};

	use criterion::Throughput;
	use moq_uring::{Config, Worker, quic, udp};

	const ALPN: &str = "worker-turns";
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
	let certs = support::certs().expect("certificates");
	let tls = quic::Identity::open(&certs.cert, &certs.key)
		.expect("identity")
		.server(&[ALPN], quic::server::ClientAuth::None)
		.expect("tls");
	let mut group = c.benchmark_group("worker_request_reply");
	for connections in [1, 16] {
		for bytes in [64, 256 * 1024] {
			for passes in [1, 4] {
				let mut config = Config::default();
				config.passes = NonZeroUsize::new(passes).unwrap();
				let mut worker = Worker::new(config).expect("worker");
				let handle = worker.handle();
				let socket = || {
					handle
						.udp(UdpSocket::bind("127.0.0.1:0").expect("bind"), udp::Config::default())
						.expect("socket")
				};
				let server = quic::Endpoint::new(
					socket(),
					quic::endpoint::Config::default().with_server(quic::server::Config::new(tls.clone())),
				)
				.expect("server");
				let client = quic::Endpoint::new(socket(), quic::endpoint::Config::default()).expect("client");
				let mut dial = quic::client::Config::default();
				dial.alpn = vec![ALPN.to_owned()];
				dial.verify = false;
				for _ in 0..connections {
					let server = server.clone();
					handle.spawn(async move {
						let conn = server.accept().await.expect("accept");
						let (mut send, mut recv) = conn.accept_bi().await.expect("stream");
						let reply = bytes::Bytes::from(vec![0x5a; bytes]);
						let mut request = [0; 64];
						while recv.read_exact(&mut request).await.is_ok() {
							send.write_all(reply.clone()).await.expect("reply");
						}
					});
				}
				let (mut streams, _held) = worker
					.block_on(async {
						let mut streams = Vec::new();
						let mut held = Vec::new();
						for _ in 0..connections {
							let conn = client
								.connect(&dial, server.local_addr(), "localhost")
								.await
								.expect("dial");
							streams.push(conn.open_bi(Default::default()).await.expect("stream"));
							held.push(conn);
						}
						(streams, held)
					})
					.expect("worker");
				let id = format!("connections-{connections}/bytes-{bytes}/passes-{passes}");
				group.throughput(Throughput::Bytes((connections * (64 + bytes)) as u64));
				let mut reply = vec![0; bytes];
				let completed = kio::Queue::bounded(1);
				group.bench_function(id.clone(), |b| {
					b.iter_custom(|rounds| {
						let mut running = std::mem::take(&mut streams);
						let mut answer = std::mem::take(&mut reply);
						let done = completed.clone();
						handle.spawn(async move {
							for _ in 0..rounds {
								for (send, _) in &mut running {
									send.write_all(&[0xa5; 64][..]).await.expect("request");
								}
								for (_, recv) in &mut running {
									recv.read_exact(&mut answer).await.expect("reply");
								}
							}
							done.try_push((running, answer)).expect("complete");
						});
						let before = handle.metrics().snapshot();
						let start = Instant::now();
						(streams, reply) = worker
							.block_on(kio::wait(|waiter| completed.poll_pop(waiter)))
							.expect("worker")
							.expect("measurement");
						let elapsed = start.elapsed();
						let after = handle.metrics().snapshot();
						assert!(reply.iter().all(|byte| *byte == 0x5a));
						eprintln!(
							"{id} rounds={rounds} turns={} passes={} enters={} rx_datagrams={}",
							after.turns - before.turns,
							after.passes - before.passes,
							after.enters - before.enters,
							after.rx_datagrams - before.rx_datagrams,
						);
						elapsed
					});
				});
			}
		}
	}
	group.finish();
}

#[cfg(not(all(target_os = "linux", feature = "quic")))]
fn request_reply(_: &mut criterion::Criterion) {}

#[cfg(target_os = "linux")]
fn benchmark(c: &mut criterion::Criterion) {
	use std::net::UdpSocket;

	use criterion::{BenchmarkId, Throughput};
	use moq_uring::{Config, Error, Worker, udp};

	let mut group = c.benchmark_group("worker_turns");
	for sockets in [1, 4, 16] {
		for burst in [1, 8, 64] {
			let mut worker = match Worker::new(Config::default()) {
				Ok(worker) => worker,
				Err(Error::Unsupported(reason)) => {
					eprintln!("skipping worker benchmark: {reason}");
					return;
				}
				Err(err) => panic!("worker setup failed: {err}"),
			};
			let handle = worker.handle();
			let mut config = udp::Config::default();
			config.gro = false;
			config.gso = false;
			config.rx_buffer_len = 2048;
			config.tx_buffer_len = 2048;
			config.rx_buffers_max = 64;
			let bind = || UdpSocket::bind("127.0.0.1:0").expect("bind");
			let sender = handle.udp(bind(), config.clone()).expect("sender");
			let receivers: Vec<_> = (0..sockets)
				.map(|_| handle.udp(bind(), config.clone()).expect("receiver"))
				.collect();
			group.throughput(Throughput::Elements(sockets * burst));
			group.bench_function(BenchmarkId::new(format!("sockets-{sockets}"), burst), |b| {
				b.iter(|| {
					worker
						.block_on(async {
							for receiver in &receivers {
								for _ in 0..burst {
									let mut tx = sender.acquire().await.expect("send buffer");
									tx.extend_from_slice(b"turn");
									tx.send(udp::Transmit::new(receiver.local_addr().expect("addr"), 4))
										.expect("send");
								}
							}
							for receiver in &receivers {
								for _ in 0..burst {
									assert_eq!(receiver.recv().await.expect("receive").payload(), b"turn");
								}
							}
						})
						.expect("worker");
				});
			});
			eprintln!(
				"worker_turns sockets={sockets} burst={burst}: {:?}",
				handle.metrics().snapshot()
			);
		}
	}
	group.finish();
}

/// Compare the same transport method before and after Local's thread guard.
/// The memory transport returns a constant here, keeping actual I/O out of the check cost.
#[cfg(target_os = "linux")]
fn local_thread_check(c: &mut criterion::Criterion) {
	use std::hint::black_box;

	use moq_net::transport::{memory, poll::Session as _};
	use moq_uring::transport::Local;

	let (session, _peer) = memory::pair(memory::Config::default());
	let local = Local::new(session.clone());
	let mut group = c.benchmark_group("local_thread_check");
	group.bench_function("direct", |b| b.iter(|| black_box(&session).max_datagram_size()));
	group.bench_function("checked", |b| b.iter(|| black_box(&local).max_datagram_size()));
	group.finish();
}

#[cfg(not(target_os = "linux"))]
fn local_thread_check(_: &mut criterion::Criterion) {}

#[cfg(not(target_os = "linux"))]
fn benchmark(_: &mut criterion::Criterion) {}

criterion_group!(benches, benchmark, request_reply, local_thread_check);
criterion_main!(benches);
