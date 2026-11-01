//! [`kio::Tasks`] against `futures::stream::FuturesUnordered`.
//!
//! Both contestants drive the same hand-rolled [`Gate`] primitive with
//! identical poll bodies, so what differs is each set's own machinery plus its
//! API contract. The contracts are not identical, and the churn scenarios
//! include that difference deliberately: `Tasks::poll` retires any number of
//! tasks in one call, while `FuturesUnordered` is a `Stream` that must hand
//! back every completion as an item (one `poll_next` call each), a per-item
//! cost its real consumers pay too. The sparse-wake scenario is contract-free
//! (nothing completes) and compares the wake/dispatch machinery alone.
//!
//! - `wake_16_of_1000`: steady state. 1000 parked tasks, each iteration fires
//!   16 gates and polls the set once; the woken tasks consume the fire and
//!   re-park. This is the shape of a driver serving many subscriptions, where
//!   per-task wakeup granularity is the whole point.
//!   `kio_tasks_batch` fires the gates from inside the owner's
//!   [`kio::Waiter::batch`], the driver-advancing-its-clock shape, so the
//!   difference is what holding and dropping the set's wake of the owner costs.
//! - `spawn_drain_1000`: cold churn. A fresh set per iteration pushes 1000
//!   immediately-ready tasks and drains them, measuring per-task setup and
//!   retirement (allocation included).
//! - `spawn_drain_1000_hot`: hot churn. One long-lived set does the same,
//!   which is the driver shape: `Tasks` reuses retired slots and their wakers,
//!   so steady-state churn stops allocating.
//!
//! The `tasks_capacity_*` groups sweep a set's historical peak (1 to 65,536
//! slots) against what is live and woken now, `kio::Tasks` alone. A burst grows
//! the set, and all but the named tasks are removed before measuring, so a cost
//! that rises with the peak is one the drained burst left behind:
//!
//! - `idle`: one live task, nothing woken. The owner polled for unrelated work.
//! - `survivor`: one live task, woken every poll.
//! - `sparse`: every 64th slot live, 16 of them woken per poll.
//! - `full`: every slot live, 16 woken per poll (no drained burst at all).
//! - `churn`: nothing live; 16 tasks pushed and drained per iteration, reusing
//!   the slots the burst left.
//!
//! Retained bytes after the burst are in the `task_capacity` bench.
//!
//! Run with `cargo bench -p kio`.

use std::{
	future::Future,
	hint::black_box,
	pin::Pin,
	sync::{
		Arc, Mutex,
		atomic::{AtomicBool, Ordering},
	},
	task::{Context, Poll, Waker},
};

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use futures::stream::StreamExt;

/// The shared wakeable primitive: a resettable flag plus a parked waker.
///
/// Deliberately minimal and identical for both contestants, so neither pays
/// costs the other doesn't.
struct Gate {
	fired: AtomicBool,
	waker: Mutex<Option<Waker>>,
}

impl Gate {
	fn new() -> Arc<Self> {
		Arc::new(Self {
			fired: AtomicBool::new(false),
			waker: Mutex::new(None),
		})
	}

	/// Fire the gate, waking whoever parked.
	fn fire(&self) {
		self.fired.store(true, Ordering::Release);
		if let Some(waker) = self.waker.lock().unwrap().take() {
			waker.wake();
		}
	}

	/// Consume a fire (if any) and park `waker` for the next one.
	fn consume_and_park(&self, waker: &Waker) -> bool {
		let fired = self.fired.swap(false, Ordering::AcqRel);
		*self.waker.lock().unwrap() = Some(waker.clone());
		fired
	}
}

/// The `FuturesUnordered` contestant: consumes fires forever, never completes.
/// Its poll body is byte-for-byte the closure the `Tasks` contestant uses.
struct Forever(Arc<Gate>);

impl Future for Forever {
	type Output = ();

	fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
		black_box(self.0.consume_and_park(cx.waker()));
		Poll::Pending
	}
}

/// The `Tasks` contestant, minted by one factory so the set stays unboxed.
fn forever(gate: Arc<Gate>) -> impl FnMut(&kio::Waiter) -> Poll<()> {
	move |waiter| {
		black_box(gate.consume_and_park(waiter.waker()));
		Poll::Pending
	}
}

fn wake_sparse(c: &mut Criterion) {
	const N: usize = 1000;
	const K: usize = 16;

	let mut group = c.benchmark_group("wake_16_of_1000");

	group.bench_function("kio_tasks", |b| {
		let gates: Vec<_> = (0..N).map(|_| Gate::new()).collect();
		let mut tasks = kio::Tasks::new();
		for gate in &gates {
			tasks.push(forever(gate.clone()));
		}
		// Park everyone.
		let waiter = kio::Waiter::noop();
		let _ = tasks.poll(&waiter);

		let mut next = 0;
		b.iter(|| {
			for _ in 0..K {
				gates[next].fire();
				next = (next + 1) % N;
			}
			let _ = black_box(tasks.poll(&waiter));
		});
	});

	// The driver shape: the owner fires its own tasks' gates mid-poll, the way
	// advancing its clock does, inside the batch that holds the set's wake of it.
	group.bench_function("kio_tasks_batch", |b| {
		let gates: Vec<_> = (0..N).map(|_| Gate::new()).collect();
		let mut tasks = kio::Tasks::new();
		for gate in &gates {
			tasks.push(forever(gate.clone()));
		}
		let waiter = kio::Waiter::noop();
		let _ = tasks.poll(&waiter);

		let mut next = 0;
		b.iter(|| {
			waiter.batch(|| {
				for _ in 0..K {
					gates[next].fire();
					next = (next + 1) % N;
				}
				let _ = black_box(tasks.poll(&waiter));
			});
		});
	});

	group.bench_function("futures_unordered", |b| {
		let gates: Vec<_> = (0..N).map(|_| Gate::new()).collect();
		let mut set = futures::stream::FuturesUnordered::new();
		for gate in &gates {
			set.push(Forever(gate.clone()));
		}
		// Park everyone. Nothing ever completes, so poll_next reports Pending
		// after polling the woken (here: all new) futures.
		let waker = Waker::noop();
		let mut cx = Context::from_waker(waker);
		let _ = set.poll_next_unpin(&mut cx);

		let mut next = 0;
		b.iter(|| {
			for _ in 0..K {
				gates[next].fire();
				next = (next + 1) % N;
			}
			let _ = black_box(set.poll_next_unpin(&mut cx));
		});
	});

	group.finish();
}

fn spawn_drain(c: &mut Criterion) {
	const N: usize = 1000;

	let mut group = c.benchmark_group("spawn_drain_1000");

	group.bench_function("kio_tasks", |b| {
		let waiter = kio::Waiter::noop();
		b.iter(|| {
			let mut tasks = kio::Tasks::new();
			for _ in 0..N {
				tasks.push(|_: &kio::Waiter| Poll::Ready(()));
			}
			while tasks.poll(&waiter).is_pending() {}
			black_box(&tasks);
		});
	});

	group.bench_function("futures_unordered", |b| {
		let waker = Waker::noop();
		b.iter(|| {
			let mut set = futures::stream::FuturesUnordered::new();
			for _ in 0..N {
				set.push(std::future::ready(()));
			}
			let mut cx = Context::from_waker(waker);
			// Drain to completion. A Stream returns one completion per
			// poll_next call, so this is N+1 calls; that per-item delivery is
			// part of the FuturesUnordered contract (see the module doc).
			while !matches!(set.poll_next_unpin(&mut cx), Poll::Ready(None)) {}
			black_box(&set);
		});
	});

	group.finish();
}

fn spawn_drain_hot(c: &mut Criterion) {
	const N: usize = 1000;

	let mut group = c.benchmark_group("spawn_drain_1000_hot");

	group.bench_function("kio_tasks", |b| {
		let waiter = kio::Waiter::noop();
		let mut tasks = kio::Tasks::new();
		b.iter(|| {
			for _ in 0..N {
				tasks.push(|_: &kio::Waiter| Poll::Ready(()));
			}
			while tasks.poll(&waiter).is_pending() {}
			black_box(&tasks);
		});
	});

	group.bench_function("futures_unordered", |b| {
		let waker = Waker::noop();
		let mut set = futures::stream::FuturesUnordered::new();
		b.iter(|| {
			for _ in 0..N {
				set.push(std::future::ready(()));
			}
			let mut cx = Context::from_waker(waker);
			while !matches!(set.poll_next_unpin(&mut cx), Poll::Ready(None)) {}
			black_box(&set);
		});
	});

	group.finish();
}

/// Historical peaks: one task, one chunk, and 16 and 64 chunks (the exact reach
/// of the top wake-index word).
const PEAKS: [usize; 4] = [1, 1024, 16_384, 65_536];

/// A set that grew to `peak` tasks, every one parked, then removed all but the
/// slots `live` keeps. Returns the set and the live tasks' gates.
fn after_burst(
	peak: usize,
	live: impl Fn(usize) -> bool,
) -> (kio::Tasks<impl FnMut(&kio::Waiter) -> Poll<()>>, Vec<Arc<Gate>>) {
	let gates: Vec<_> = (0..peak).map(|_| Gate::new()).collect();
	let mut tasks = kio::Tasks::new();
	let keys: Vec<_> = gates.iter().map(|gate| tasks.insert(forever(gate.clone()))).collect();
	let _ = tasks.poll(&kio::Waiter::noop());
	let mut kept = Vec::new();
	for (slot, (key, gate)) in keys.into_iter().zip(gates).enumerate() {
		match live(slot) {
			true => kept.push(gate),
			false => drop(tasks.remove(key)),
		}
	}
	(tasks, kept)
}

fn capacity(c: &mut Criterion) {
	// Live slot predicate and tasks woken per owner poll.
	type Shape = (&'static str, fn(usize) -> bool, usize);
	let shapes: [Shape; 4] = [
		("idle", |slot| slot == 0, 0),
		("survivor", |slot| slot == 0, 1),
		("sparse", |slot| slot % 64 == 0, 16),
		("full", |_| true, 16),
	];
	for (name, live, ready) in shapes {
		let mut group = c.benchmark_group(format!("tasks_capacity_{name}"));
		for peak in PEAKS {
			group.bench_with_input(BenchmarkId::from_parameter(peak), &peak, |b, &peak| {
				let (mut tasks, gates) = after_burst(peak, live);
				let ready = ready.min(gates.len());
				let waiter = kio::Waiter::noop();
				let mut next = 0;
				b.iter(|| {
					for _ in 0..ready {
						gates[next].fire();
						next = (next + 1) % gates.len();
					}
					let _ = black_box(tasks.poll(&waiter));
				});
			});
		}
		group.finish();
	}

	let mut group = c.benchmark_group("tasks_capacity_churn");
	for peak in PEAKS {
		group.bench_with_input(BenchmarkId::from_parameter(peak), &peak, |b, &peak| {
			fn done(_: &kio::Waiter) -> Poll<()> {
				Poll::Ready(())
			}
			let waiter = kio::Waiter::noop();
			let mut tasks = kio::Tasks::new();
			for _ in 0..peak {
				tasks.push(done);
			}
			while tasks.poll(&waiter).is_pending() {}
			b.iter(|| {
				for _ in 0..16 {
					tasks.push(done);
				}
				while tasks.poll(&waiter).is_pending() {}
				black_box(&tasks);
			});
		});
	}
	group.finish();
}

criterion_group!(benches, wake_sparse, spawn_drain, spawn_drain_hot, capacity);
criterion_main!(benches);
