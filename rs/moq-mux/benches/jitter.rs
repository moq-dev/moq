//! The jitter buffer's clock steering swept over track count and how many floors the rate
//! window holds.
use std::time::Duration;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use moq_net::Timestamp;

#[path = "../src/jitter.rs"]
#[allow(
	dead_code,
	reason = "the bench compiles the private module from source and uses only part of it"
)]
mod jitter;

/// Every track sends a frame a second as it decodes, for as many two-second steering steps as
/// fill the rate window with `floors`, each step measuring every track heard.
async fn steer(tracks: u16, floors: u64) -> u64 {
	let start = moq_clock::now();
	let mut buffer = jitter::Buffer::new(Duration::from_millis(500));
	buffer.expect(1..=tracks);
	let waiter = kio::Waiter::noop();
	for second in 0..floors * 2 {
		let at = start + Duration::from_secs(second);
		tokio::time::advance(at.saturating_duration_since(moq_clock::now())).await;
		for track in 1..=tracks {
			let arrival = jitter::Arrival {
				arrived: at,
				read: at,
				decode: Timestamp::from_millis(second * 1_000).unwrap().into(),
				restart: 0,
				skip: 0,
				sync: true,
				leading: false,
				bytes: 0,
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

/// One fill drains every track's available backlog before polling output.
fn acquisition(c: &mut Criterion) {
	let runtime = tokio::runtime::Builder::new_current_thread()
		.enable_time()
		.start_paused(true)
		.build()
		.unwrap();
	let mut group = c.benchmark_group("jitter_acquisition");
	group.measurement_time(Duration::from_secs(1));
	group.warm_up_time(Duration::from_millis(200));
	group.sample_size(10);
	for tracks in [1u16, 4, 16, 64] {
		for frames in [16u64, 64, 256] {
			group.bench_function(BenchmarkId::new(tracks.to_string(), frames), |b| {
				b.iter(|| {
					runtime.block_on(async {
						let start = moq_clock::now();
						let mut buffer = jitter::Buffer::new(Duration::from_millis(500));
						buffer.expect(1..=tracks);
						for track in 1..=tracks {
							for frame in 0..frames {
								buffer
									.push(
										track,
										jitter::Arrival {
											arrived: start,
											read: start,
											decode: Timestamp::from_millis(frame * 20).unwrap().into(),
											restart: 0,
											skip: 0,
											sync: frame % 15 == 0,
											leading: false,
											bytes: 0,
											item: (),
										},
									)
									.unwrap();
							}
						}
						let _ = std::hint::black_box(buffer.poll_next(&kio::Waiter::noop()));
					})
				});
			});
		}
	}
	group.finish();
}

/// An independent instance acquires while the old tracks still hold their tail.
fn handoff(c: &mut Criterion) {
	let runtime = tokio::runtime::Builder::new_current_thread()
		.enable_time()
		.start_paused(true)
		.build()
		.unwrap();
	let mut group = c.benchmark_group("jitter_handoff");
	group.measurement_time(Duration::from_secs(1));
	group.warm_up_time(Duration::from_millis(200));
	group.sample_size(10);
	for tracks in [1u16, 4, 16, 64] {
		for frames in [16u64, 64, 256] {
			group.bench_function(BenchmarkId::new(tracks.to_string(), frames), |b| {
				b.iter(|| {
					runtime.block_on(async {
						let start = moq_clock::now();
						let mut buffer = jitter::Buffer::new(Duration::from_millis(500));
						buffer.expect(1..=tracks);
						for instance in 0..2 {
							if instance == 1 {
								let keys = tracks + 1..=2 * tracks;
								buffer.expect(keys.clone());
								buffer.start(keys.clone(), keys);
							}
							for track in 1..=tracks {
								for frame in 0..frames {
									buffer
										.push(
											instance * tracks + track,
											jitter::Arrival {
												arrived: start,
												read: start,
												decode: Timestamp::from_millis(
													u64::from(instance) * 10_000 + frame * 20,
												)
												.unwrap()
												.into(),
												restart: 0,
												skip: 0,
												sync: frame % 15 == 0,
												leading: false,
												bytes: 0,
												item: (),
											},
										)
										.unwrap();
								}
							}
							let _ = std::hint::black_box(buffer.poll_next(&kio::Waiter::noop()));
						}
						tokio::time::advance(Duration::from_secs(1)).await;
						if let std::task::Poll::Ready(ready) = buffer.poll_pending(&kio::Waiter::noop()) {
							buffer.commit(ready).unwrap();
						}
						assert!(buffer.is_committed());
					})
				});
			});
		}
	}
	group.finish();
}

/// Admission reads one time window, independent of the backlog still held by each source.
fn admission(c: &mut Criterion) {
	let runtime = tokio::runtime::Builder::new_current_thread()
		.enable_time()
		.start_paused(true)
		.build()
		.unwrap();
	let mut group = c.benchmark_group("jitter_admission");
	group.measurement_time(Duration::from_secs(1));
	group.warm_up_time(Duration::from_millis(200));
	group.sample_size(10);
	for tracks in [1u16, 4, 16, 64] {
		for backlog in [64u64, 256, 1_024] {
			group.bench_function(BenchmarkId::new(tracks.to_string(), backlog), |b| {
				b.iter(|| {
					runtime.block_on(async {
						let start = moq_clock::now();
						let delay = Duration::from_millis(500);
						let mut buffer = jitter::Buffer::new(delay);
						buffer.expect(1..=tracks);
						let frame = |decode| jitter::Arrival {
							arrived: start,
							read: moq_clock::now(),
							decode: Timestamp::from_millis(decode).unwrap().into(),
							restart: 0,
							skip: 0,
							sync: true,
							leading: false,
							bytes: 0,
							item: (),
						};
						for track in 1..=tracks {
							buffer.push(track, frame(0)).unwrap();
						}
						tokio::time::advance(delay).await;
						while buffer.poll_next(&kio::Waiter::noop()).is_ready() {}
						let mut read = 0;
						for track in 1..=tracks {
							for next in 1..=backlog {
								if buffer.full(&track, &kio::Waiter::noop()) {
									break;
								}
								buffer.push(track, frame(next * 20)).unwrap();
								read += 1;
							}
						}
						std::hint::black_box(read)
					})
				});
			});
		}
	}
	group.finish();
}

criterion_group!(benches, steering, acquisition, handoff, admission);
criterion_main!(benches);
