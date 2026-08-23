//! The PR-2 ablation matrix: a raw-quiche echo through the worker, toggling
//! receive batching (multishot + provided buffers), GRO, and GSO one at a
//! time. Run it with `just rs bench-echo` on a Linux 6.12+ kernel.

use criterion::{criterion_group, criterion_main};

#[cfg(target_os = "linux")]
#[path = "../tests/support/quiche.rs"]
mod support;

#[cfg(target_os = "linux")]
mod linux {
	use std::net::UdpSocket;
	use std::time::Instant;

	use criterion::{BenchmarkId, Criterion, Throughput};
	use moq_uring::{Config, Error, Worker, udp};

	use super::support;

	/// Bytes echoed per iteration (each direction).
	const PAYLOAD: usize = 1024 * 1024;

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
				eprintln!("skipping io_uring echo benchmark: {reason}");
				return;
			}
			Err(err) => panic!("worker setup failed: {err}"),
		};
		drop(probe);

		let mut group = c.benchmark_group("echo_quiche");
		group.throughput(Throughput::Bytes(PAYLOAD as u64));

		for ablation in ablations() {
			let mut worker = Worker::new(Config::default()).expect("worker");
			let handle = worker.handle();

			let certs = support::certs().expect("certificates");
			let server_sock = handle
				.udp(UdpSocket::bind("127.0.0.1:0").expect("bind"), ablation.config.clone())
				.expect("server socket");
			let server_addr = server_sock.local_addr().expect("server addr");
			let client_sock = handle
				.udp(UdpSocket::bind("127.0.0.1:0").expect("bind"), ablation.config.clone())
				.expect("client socket");

			let server_handle = handle.clone();
			handle.spawn(async move {
				support::echo_server(server_handle, server_sock, certs)
					.await
					.expect("echo server");
			});

			// Establish once; iterations then measure steady-state transfer.
			let mut client = worker
				.block_on(async {
					let mut client = support::Peer::connect(&handle, client_sock, server_addr)?;
					client.flush().await?;
					while !client.conn.is_established() {
						client.step().await?;
						client.flush().await?;
					}
					anyhow::Ok(client)
				})
				.expect("worker")
				.expect("handshake");

			let payload: Vec<u8> = (0..PAYLOAD).map(|i| (i * 31 % 251) as u8).collect();
			// Client-initiated bidirectional stream ids: 0, 4, 8, ...
			let mut next_stream = 0u64;

			group.bench_with_input(BenchmarkId::from_parameter(ablation.name), &ablation.config, |b, _| {
				b.iter_custom(|iterations| {
					worker
						.block_on(async {
							let start = Instant::now();
							for _ in 0..iterations {
								let stream = next_stream;
								next_stream += 4;
								let echoed = support::stream_echo(&mut client, stream, &payload).await?;
								assert_eq!(echoed, PAYLOAD, "lost echo bytes");
							}
							anyhow::Ok(start.elapsed())
						})
						.expect("worker")
						.expect("echo iteration")
				});
			});
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
