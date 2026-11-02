//! Routing on a shared port: what every QUIC datagram pays to be told it is QUIC.
//!
//! Swept over the pins held (WebRTC sessions) and the shards routing at once,
//! since every shard of a worker group reads the same pin table.

#![allow(
	clippy::disallowed_methods,
	clippy::disallowed_types,
	reason = "a benchmark times itself on the host clock"
)]

use std::{
	hint::black_box,
	net::SocketAddr,
	time::{Duration, Instant},
};

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use moq_sock::demux::{Flows, Route};

/// Datagrams one shard routes per iteration, as one receive batch would hold.
const BATCH: usize = 64;

fn remote(i: usize) -> SocketAddr {
	SocketAddr::from(([10, (i >> 16) as u8, (i >> 8) as u8, i as u8], 4433))
}

fn bench_route(c: &mut Criterion) {
	let mut group = c.benchmark_group("demux_route");
	let quic = [0x40u8; 1200];
	for pins in [0, 1, 1024] {
		let flows = Flows::default();
		// Pinned addresses never match the senders, so every datagram stays QUIC.
		let _pins: Vec<_> = (0..pins).map(|i| flows.pin(remote(1 << 20 | i))).collect();
		for shards in [1, 4] {
			group.throughput(Throughput::Elements((BATCH * shards) as u64));
			group.bench_function(
				BenchmarkId::new(format!("pins_{pins}"), format!("shards_{shards}")),
				|b| {
					b.iter_custom(|iters| {
						let start = Instant::now();
						std::thread::scope(|scope| {
							for shard in 0..shards {
								let flows = &flows;
								scope.spawn(move || {
									for _ in 0..iters {
										for i in 0..BATCH {
											let route = flows.route(remote(shard * BATCH + i), &quic);
											assert_eq!(black_box(route), Route::Quic);
										}
									}
								});
							}
						});
						start.elapsed()
					})
				},
			);
		}
	}
	group.finish();
}

criterion_group! {
	name = benches;
	config = Criterion::default().measurement_time(Duration::from_secs(2));
	targets = bench_route
}
criterion_main!(benches);
