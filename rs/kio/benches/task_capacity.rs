//! A burst through a [`kio::Tasks`] set, swept over the burst's peak and the tasks it leaves live.
//!
//! Each case grows a set to `peak` parked tasks and removes all but `live` of them, spread
//! over the peak. The set is returned, so its drop stays outside the count and the heap
//! swole reports is what the burst allocated (slots, wakers, wake bitset and tasks; the
//! tasks' gates come from setup). Per-poll time and allocations after such a burst are in
//! the `tasks` bench's `tasks_capacity_*` benchmarks.
//!
//! Run with `cargo bench -p kio --bench task_capacity`.

use std::{
	fmt,
	sync::{Arc, Mutex},
	task::{Poll, Waker},
};

use swole::bench;

/// A parked waker slot a task re-arms on every poll, so each task holds one
/// registration the way a channel read does.
type Gate = Arc<Mutex<Option<Waker>>>;

/// A task that parks on its gate forever.
struct Parked(Gate);

impl kio::Task for Parked {
	type Output = ();

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		*self.0.lock().unwrap() = Some(waiter.waker().clone());
		Poll::Pending
	}
}

/// A burst's peak and the tasks it leaves live, named `<peak>/<live>`.
#[derive(Clone, Copy)]
struct Case {
	peak: usize,
	live: usize,
}

impl fmt::Display for Case {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "{}/{}", self.peak, self.live)
	}
}

fn cases() -> Vec<Case> {
	let mut cases = Vec::new();
	for peak in [1, 1024, 16_384, 65_536] {
		let mut lives = vec![0, 1, peak / 64];
		lives.sort();
		lives.dedup();
		cases.extend(lives.into_iter().map(|live| Case { peak, live }));
	}
	cases
}

fn gates(case: Case) -> (Case, Vec<Gate>) {
	(case, (0..case.peak).map(|_| Gate::default()).collect())
}

#[bench(args = cases(), setup = gates)]
fn burst((case, gates): (Case, Vec<Gate>)) -> (kio::Tasks<Parked>, Vec<Gate>) {
	let waiter = kio::Waiter::noop();
	let mut tasks = kio::Tasks::new();
	let keys: Vec<_> = gates.iter().map(|gate| tasks.insert(Parked(gate.clone()))).collect();
	let _ = tasks.poll(&waiter);
	// Keep every `peak / live`-th task, so live slots spread over the peak.
	let stride = case.peak.checked_div(case.live).unwrap_or(0);
	for (slot, key) in keys.into_iter().enumerate() {
		if case.live == 0 || !slot.is_multiple_of(stride) {
			drop(tasks.remove(key));
			// The removed task's registration goes with it, as a channel's would.
			gates[slot].lock().unwrap().take();
		}
	}
	assert_eq!(tasks.len(), case.live);
	(tasks, gates)
}

fn main() {
	swole::main();
}
