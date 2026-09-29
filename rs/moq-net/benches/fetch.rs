//! Standalone FETCH walk over a sparse track: what one range request costs a
//! publisher answering it from its own cache, with nothing upstream to ask.
//!
//! `fetch/span` holds the cached groups fixed and widens the range they are spread
//! across; `fetch/present` holds the range fixed and adds groups. The walk should
//! cost the groups it returns, so `span` stays flat and `present` grows linearly.
//! Throughput counts the groups returned.
//!
//! Run with `cargo bench -p moq-net --features fuzz --bench fetch`.

use std::time::Duration;

use bytes::Bytes;
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use moq_net::{Timestamp, broadcast, cache, group, track};

/// Range widths, from dense to sparse enough that stepping through it never ends.
const SPANS: [u64; 4] = [1 << 6, 1 << 12, 1 << 24, 1 << 40];

/// Cached group counts spread across the range.
const PRESENT: [u64; 4] = [1, 16, 256, 4096];

/// Held fixed while the other axis sweeps.
const FIXED_PRESENT: u64 = 64;
const FIXED_SPAN: u64 = 1 << 24;

/// Keeps the ownership chain alive around the track being fetched.
struct Sparse {
	_broadcast: broadcast::Producer,
	_track: track::Producer,
	consumer: track::Consumer,
	span: u64,
}

impl Sparse {
	/// `present` one-frame groups spread evenly across `0..span`.
	fn new(span: u64, present: u64) -> Self {
		let mut info = broadcast::Info::default();
		// Nothing may expire or evict mid-measurement, or later samples walk fewer groups.
		let config = cache::Config::default()
			.with_capacity(1 << 30)
			.with_expiry(Duration::from_secs(24 * 60 * 60));
		info.pool = cache::Pool::new(config);
		let broadcast = broadcast::Producer::new(info);
		let track = broadcast.create_track("bench", None).unwrap();

		let step = span / present;
		for sequence in (0..present).map(|i| i * step) {
			let mut group = track.create_group(group::Info { sequence }).unwrap();
			group
				.write_frame(Timestamp::ZERO, Bytes::from_static(&[0; 64]))
				.unwrap();
			group.finish().unwrap();
		}

		Self {
			consumer: track.consume(),
			_broadcast: broadcast,
			_track: track,
			span,
		}
	}

	fn walk(&self) -> usize {
		futures::executor::block_on(moq_net::fuzz::walk_fetch(&self.consumer, 0, self.span)).unwrap()
	}
}

fn bench_span(c: &mut Criterion) {
	let mut group = c.benchmark_group("fetch/span");
	group.throughput(Throughput::Elements(FIXED_PRESENT));
	for span in SPANS {
		let sparse = Sparse::new(span, FIXED_PRESENT);
		assert_eq!(sparse.walk() as u64, FIXED_PRESENT);
		group.bench_function(BenchmarkId::from_parameter(format!("2^{}", span.ilog2())), |b| {
			b.iter(|| sparse.walk())
		});
	}
	group.finish();
}

fn bench_present(c: &mut Criterion) {
	let mut group = c.benchmark_group("fetch/present");
	for present in PRESENT {
		group.throughput(Throughput::Elements(present));
		let sparse = Sparse::new(FIXED_SPAN, present);
		assert_eq!(sparse.walk() as u64, present);
		group.bench_function(BenchmarkId::from_parameter(present), |b| b.iter(|| sparse.walk()));
	}
	group.finish();
}

criterion_group!(benches, bench_span, bench_present);
criterion_main!(benches);
