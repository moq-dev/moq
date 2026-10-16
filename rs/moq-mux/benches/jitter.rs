//! The jitter buffer's clock steering swept over track count and how many floors the rate
//! window holds.
use std::time::Duration;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use moq_net::Timestamp;
use web_async::time::Instant;

#[path = "../src/jitter.rs"]
#[allow(dead_code)]
mod jitter;

/// Every track sends a frame a second as it decodes, for as many two-second steering steps as
/// fill the rate window with `floors`, each step measuring every track heard.
async fn steer(tracks: u16, floors: u64) -> u64 {
	let start = Instant::now();
	let mut buffer = jitter::Buffer::new(Duration::from_millis(500));
	buffer.expect(1..=tracks);
	let waiter = kio::Waiter::noop();
	for second in 0..floors * 2 {
		let at = start + Duration::from_secs(second);
		tokio::time::advance(at.saturating_duration_since(Instant::now())).await;
		for track in 1..=tracks {
			let arrival = jitter::Arrival {
				arrived: at,
				read: at,
				decode: Timestamp::from_millis(second * 1_000).unwrap(),
				restart: 0,
				skip: 0,
				cut: 0,
				sync: true,
				leading: false,
				item: (),
			};
			buffer.push(track, arrival).unwrap();
		}
		while buffer.poll_next(&waiter).is_ready() {}
	}
	buffer.dropped()
}

fn steering(c: &mut Criterion) {
	let runtime = tokio::runtime::Builder::new_current_thread()
		.enable_time()
		.start_paused(true)
		.build()
		.unwrap();
	let mut group = c.benchmark_group("jitter_steering");
	group.measurement_time(Duration::from_secs(1));
	group.warm_up_time(Duration::from_millis(200));
	group.sample_size(10);
	for tracks in [1u16, 4, 16, 64] {
		for floors in [10u64, 100, 300] {
			group.bench_function(BenchmarkId::new(tracks.to_string(), floors), |b| {
				b.iter(|| assert_eq!(runtime.block_on(steer(tracks, floors)), 0));
			});
		}
	}
	group.finish();
}

criterion_group!(benches, steering);
criterion_main!(benches);
