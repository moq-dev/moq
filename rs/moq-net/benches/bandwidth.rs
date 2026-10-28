//! What following a session's probe target costs as its reservations fan out.
//!
//! - update: one reservation's ceiling moves and the session reads the new target.
//!   Swept over the reservations sharing the allocator, so an update that rescans
//!   every reservation shows up as a slope.
//!
//! Run with `cargo bench -p moq-net --features fuzz --bench bandwidth`.

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use moq_net::bandwidth::{Allocator, Rate, Reservation};
use moq_net::{broadcast, fuzz, track};

/// Reservations sharing the allocator.
const RESERVATIONS: [usize; 4] = [1, 10, 1_000, 10_000];

/// An allocator holding `count` reservations, one track each, plus what keeps them alive.
fn table(count: usize) -> (Allocator, broadcast::Producer, Vec<track::Producer>, Vec<Reservation>) {
	let allocator = Allocator::unlimited();
	let broadcast = broadcast::Info::new().produce();
	let mut tracks = Vec::with_capacity(count);
	let mut reservations = Vec::with_capacity(count);
	for i in 0..count {
		let track = broadcast.create_track(format!("t{i}"), None).unwrap();
		reservations.push(allocator.reserve(&track.demand(), Rate::from_kbps(1_000)));
		tracks.push(track);
	}
	(allocator, broadcast, tracks, reservations)
}

fn bench_update(c: &mut Criterion) {
	let mut group = c.benchmark_group("bandwidth/update");
	for count in RESERVATIONS {
		group.bench_function(BenchmarkId::from_parameter(count), |b| {
			let (allocator, _broadcast, _tracks, reservations) = table(count);
			let mut target = fuzz::ProbeTarget::new(&allocator);
			target.poll().expect("the first poll reports the target");
			let touched = &reservations[count / 2];
			let mut raised = false;
			b.iter(|| {
				raised = !raised;
				touched.update(Rate::from_kbps(if raised { 2_000 } else { 1_000 }));
				std::hint::black_box(target.poll().expect("the target moved"));
			});
		});
	}
	group.finish();
}

criterion_group!(benches, bench_update);
criterion_main!(benches);
