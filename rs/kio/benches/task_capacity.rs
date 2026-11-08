//! Heap a [`kio::Tasks`] set keeps after a burst drains, swept over the burst's peak.
//!
//! Run `cargo bench -p kio --bench task_capacity` and compare the table across
//! revisions. Each row grows a set to `peak` parked tasks, removes all but
//! `live` of them, and reports the bytes the set still holds (its slots, wakers,
//! wake bitset and live tasks; the tasks' gates are outside the count), the
//! allocations per owner poll over 1,000 more polls, and the bytes left once
//! the set and its gates drop (nonzero would be a leak). Per-poll time for the
//! same shapes is in the `tasks` bench's `tasks_capacity_*` groups.

use std::{
	alloc::{GlobalAlloc, Layout, System},
	hint::black_box,
	sync::{
		Arc, Mutex,
		atomic::{AtomicIsize, AtomicUsize, Ordering},
	},
	task::{Poll, Waker},
};

struct Counter;

static LIVE: AtomicIsize = AtomicIsize::new(0);
static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

#[global_allocator]
static ALLOCATOR: Counter = Counter;

// Counting only. Every call forwards to the system allocator unchanged.
unsafe impl GlobalAlloc for Counter {
	unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
		LIVE.fetch_add(layout.size() as isize, Ordering::Relaxed);
		ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
		unsafe { System.alloc(layout) }
	}

	unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
		LIVE.fetch_add(layout.size() as isize, Ordering::Relaxed);
		ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
		unsafe { System.alloc_zeroed(layout) }
	}

	unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
		LIVE.fetch_add(new_size as isize - layout.size() as isize, Ordering::Relaxed);
		ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
		unsafe { System.realloc(ptr, layout, new_size) }
	}

	unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
		LIVE.fetch_sub(layout.size() as isize, Ordering::Relaxed);
		unsafe { System.dealloc(ptr, layout) }
	}
}

/// A parked waker slot a task re-arms on every poll, so each task holds one
/// registration the way a channel read does.
type Gate = Arc<Mutex<Option<Waker>>>;

fn task(gate: Gate) -> impl FnMut(&kio::Waiter) -> Poll<()> {
	move |waiter| {
		*gate.lock().unwrap() = Some(waiter.waker().clone());
		Poll::Pending
	}
}

fn main() {
	println!("| peak | live | retained bytes | bytes per peak slot | allocations per poll | bytes after drop |");
	println!("|---:|---:|---:|---:|---:|---:|");
	for peak in [1, 1024, 16_384, 65_536] {
		let mut lives = vec![0, 1, peak / 64];
		lives.sort();
		lives.dedup();
		for live in lives {
			let start = LIVE.load(Ordering::Relaxed);
			let gates: Vec<Gate> = (0..peak).map(|_| Gate::default()).collect();
			// Gates, keys and the owner's waiter precede the baseline, so only the set counts.
			let mut keys = Vec::with_capacity(peak);
			let waiter = kio::Waiter::noop();
			let base = LIVE.load(Ordering::Relaxed);

			let mut tasks = kio::Tasks::new();
			for gate in &gates {
				keys.push(tasks.insert(task(gate.clone())));
			}
			let _ = tasks.poll(&waiter);
			// Keep every `peak / live`-th task, so live slots spread over the peak.
			let stride = peak.checked_div(live).unwrap_or(0);
			for (slot, key) in keys.drain(..).enumerate() {
				if live == 0 || !slot.is_multiple_of(stride) {
					drop(tasks.remove(key));
					// The removed task's registration goes with it, as a channel's would.
					gates[slot].lock().unwrap().take();
				}
			}
			assert_eq!(tasks.len(), live);
			let retained = LIVE.load(Ordering::Relaxed) - base;

			let before = ALLOCATIONS.load(Ordering::Relaxed);
			for round in 0..1_000 {
				if live > 0 {
					let gate = &gates[(round % live) * stride];
					if let Some(waker) = gate.lock().unwrap().take() {
						waker.wake();
					}
				}
				let _ = black_box(tasks.poll(&waiter));
			}
			let allocations = (ALLOCATIONS.load(Ordering::Relaxed) - before) as f64 / 1_000.0;

			// Parked wakers hold their slot's waker state until the gates drop.
			drop((tasks, gates, keys, waiter));
			let after = LIVE.load(Ordering::Relaxed) - start;
			println!(
				"| {peak} | {live} | {retained} | {:.1} | {allocations} | {after} |",
				retained as f64 / peak as f64
			);
		}
	}
}
