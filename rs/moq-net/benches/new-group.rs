//! A downstream NEW_GROUP_REQUEST reaching a relay's upstream subscription: one
//! subscriber raises its request and the track's upstream driver reads the aggregate.
//! Swept over tracks and subscribers per track, so a cost that grows with the relay's
//! other tracks instead of the touched track's subscribers shows up as a slope.
//!
//! The request is crate-private, so this reaches it through the hidden `fuzz` module.
//!
//! Run with `cargo bench -p moq-net --features fuzz --bench new-group`.

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use moq_net::fuzz::NewGroups;

const TRACKS: [usize; 3] = [1, 100, 1_000];
const SUBSCRIBERS: [usize; 3] = [1, 10, 100];

fn request(c: &mut Criterion) {
	let mut group = c.benchmark_group("new_group/request");
	for tracks in TRACKS {
		for subscribers in SUBSCRIBERS {
			let mut relay = NewGroups::new(tracks, subscribers);
			let mut value = 0;
			group.bench_function(BenchmarkId::new(format!("{tracks}x"), subscribers), |b| {
				b.iter(|| {
					// A higher value every time, so the aggregate always changes.
					value += 1;
					let seen = relay.request(tracks / 2, subscribers / 2, value);
					assert_eq!(seen, Some(value));
				})
			});
		}
	}
	group.finish();
}

criterion_group!(benches, request);
criterion_main!(benches);
