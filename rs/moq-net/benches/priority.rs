//! Worst-case insertion benchmarks for the publishers' stream priority queue.
//!
//! Run with `cargo bench -p moq-net --features async --bench priority`.

use swole::bench;

// Compiled straight from source so the queue stays private to the crate; a `pub` re-export
// just for the bench would widen the crate's surface.
#[path = "../src/priority.rs"]
#[allow(
	dead_code,
	reason = "the bench compiles the private module from source and uses only part of it"
)]
mod priority;

// The queue names `crate::track::Order`, which is public.
mod track {
	pub use moq_net::track::Order;
}

use priority::{Priority, PriorityHandle, PriorityQueue};

const DEPTHS: [usize; 3] = [8, 64, 255];
const OVERFLOW_DEPTHS: [usize; 3] = [256, 1_024, 4_096];

fn filled(depth: usize) -> (PriorityQueue, Vec<PriorityHandle>) {
	let queue = PriorityQueue::default();
	let handles = (0..depth)
		.map(|group| queue.insert(Priority::new(-28, 0, group as u64)))
		.collect();
	(queue, handles)
}

/// Takes the queue by value and returns it, so building and dropping it stay unmeasured.
#[bench(args = DEPTHS, setup = filled)]
fn priority_queue_insert_front(
	(queue, mut handles): (PriorityQueue, Vec<PriorityHandle>),
) -> (PriorityQueue, Vec<PriorityHandle>) {
	handles.push(queue.insert(Priority::new(i8::MAX, 0, u64::MAX)));
	(queue, handles)
}

fn overflow(depth: usize) -> Vec<PriorityHandle> {
	let queue = PriorityQueue::default();
	let mut handles: Vec<_> = (0..255)
		.map(|group| queue.insert(Priority::new(72, 0, group)))
		.collect();
	handles.extend((255..depth as u64).map(|group| queue.insert(Priority::new(-28, 0, group))));
	handles
}

/// Returns the rest of the handles, so only the popped one's removal is measured.
#[bench(args = OVERFLOW_DEPTHS, setup = overflow)]
fn priority_queue_remove_overflow(mut handles: Vec<PriorityHandle>) -> Vec<PriorityHandle> {
	drop(handles.pop());
	handles
}

fn main() {
	swole::main();
}
