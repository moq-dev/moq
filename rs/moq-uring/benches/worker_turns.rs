//! Worker submission and completion amortization across sockets and burst sizes.

use criterion::{criterion_group, criterion_main};

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
									tx.send(udp::Transmit {
										to: receiver.local_addr().expect("addr"),
										segment: 4,
										ecn: None,
									})
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

#[cfg(not(target_os = "linux"))]
fn benchmark(_: &mut criterion::Criterion) {}

criterion_group!(benches, benchmark);
criterion_main!(benches);
