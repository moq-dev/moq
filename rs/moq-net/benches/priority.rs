//! Worst-case insertion benchmarks for the Lite publisher's stream priority queue.

use criterion::{BatchSize, BenchmarkId, Criterion, criterion_group, criterion_main};

// Compiled straight from source so the queue stays private to `lite`; a `pub` re-export
// just for the bench would widen the crate's surface.
#[path = "../src/lite/priority.rs"]
#[allow(dead_code)]
mod priority;

use priority::{Priority, PriorityHandle, PriorityQueue};

const DEPTHS: [usize; 3] = [8, 64, 255];
const OVERFLOW_DEPTHS: [usize; 3] = [256, 1_024, 4_096];

fn filled(depth: usize) -> (PriorityQueue, Vec<PriorityHandle>) {
	let queue = PriorityQueue::default();
	let handles = (0..depth)
		.map(|group| queue.insert(Priority::new(100, 0, group as u64)))
		.collect();
	(queue, handles)
}

fn bench_insert(c: &mut Criterion) {
	let mut group = c.benchmark_group("priority_queue_insert_front");
	for depth in DEPTHS {
		group.bench_with_input(BenchmarkId::from_parameter(depth), &depth, |b, &depth| {
			b.iter_batched_ref(
				|| filled(depth),
				|(queue, handles)| {
					handles.push(queue.insert(Priority::new(u8::MAX, 0, u64::MAX)));
				},
				BatchSize::SmallInput,
			);
		});
	}
	group.finish();
}

fn overflow(depth: usize) -> Vec<PriorityHandle> {
	let queue = PriorityQueue::default();
	let mut handles: Vec<_> = (0..255)
		.map(|group| queue.insert(Priority::new(200, 0, group)))
		.collect();
	handles.extend((255..depth as u64).map(|group| queue.insert(Priority::new(100, 0, group))));
	handles
}

fn bench_remove_overflow(c: &mut Criterion) {
	let mut group = c.benchmark_group("priority_queue_remove_overflow");
	for depth in OVERFLOW_DEPTHS {
		group.bench_with_input(BenchmarkId::from_parameter(depth), &depth, |b, &depth| {
			b.iter_batched_ref(|| overflow(depth), |handles| drop(handles.pop()), BatchSize::SmallInput);
		});
	}
	group.finish();
}

criterion_group!(benches, bench_insert, bench_remove_overflow);
criterion_main!(benches);
