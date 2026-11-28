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
//! What the burst itself allocates is in the `task_capacity` bench.
//!
//! Run with `cargo bench -p kio --bench tasks`.

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

use swole::bench;

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

/// The `Tasks` contestant, with the same poll body as [`Forever`].
struct Parked(Arc<Gate>);

impl kio::Task for Parked {
	type Output = ();

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		black_box(self.0.consume_and_park(waiter.waker()));
		Poll::Pending
	}
}

/// A task that completes on its first poll.
struct Done;

impl kio::Task for Done {
	type Output = ();

	fn poll(&mut self, _: &kio::Waiter) -> Poll<()> {
		Poll::Ready(())
	}
}

mod wake_16_of_1000 {
	use std::{
		hint::black_box,
		sync::Arc,
		task::{Context, Waker},
	};

	use futures::stream::{FuturesUnordered, StreamExt};
	use swole::bench;

	use super::{Forever, Gate, Parked};

	const N: usize = 1000;
	const K: usize = 16;

	/// Every gate's task parked in one set, and the next gate to fire.
	struct Sparse<S> {
		gates: Vec<Arc<Gate>>,
		set: S,
		waiter: kio::Waiter,
		next: usize,
	}

	impl<S> Sparse<S> {
		fn fire(&mut self) {
			for _ in 0..K {
				self.gates[self.next].fire();
				self.next = (self.next + 1) % N;
			}
		}
	}

	fn tasks() -> Sparse<kio::Tasks<Parked>> {
		let gates: Vec<_> = (0..N).map(|_| Gate::new()).collect();
		let mut set = kio::Tasks::new();
		for gate in &gates {
			set.push(Parked(gate.clone()));
		}
		// Park everyone.
		let waiter = kio::Waiter::noop();
		let _ = set.poll(&waiter);
		Sparse {
			gates,
			set,
			waiter,
			next: 0,
		}
	}

	#[bench(setup = tasks)]
	fn kio_tasks(sparse: &mut Sparse<kio::Tasks<Parked>>) {
		sparse.fire();
		let _ = black_box(sparse.set.poll(&sparse.waiter));
	}

	/// The driver shape: the owner fires its own tasks' gates mid-poll, the way
	/// advancing its clock does, inside the batch that holds the set's wake of it.
	#[bench(setup = tasks)]
	fn kio_tasks_batch(sparse: &mut Sparse<kio::Tasks<Parked>>) {
		let Sparse {
			gates,
			set,
			waiter,
			next,
		} = sparse;
		waiter.batch(|| {
			for _ in 0..K {
				gates[*next].fire();
				*next = (*next + 1) % N;
			}
			let _ = black_box(set.poll(waiter));
		});
	}

	fn unordered() -> Sparse<FuturesUnordered<Forever>> {
		let gates: Vec<_> = (0..N).map(|_| Gate::new()).collect();
		let mut set = FuturesUnordered::new();
		for gate in &gates {
			set.push(Forever(gate.clone()));
		}
		// Park everyone. Nothing ever completes, so poll_next reports Pending
		// after polling the woken (here: all new) futures.
		let _ = set.poll_next_unpin(&mut Context::from_waker(Waker::noop()));
		Sparse {
			gates,
			set,
			waiter: kio::Waiter::noop(),
			next: 0,
		}
	}

	#[bench(setup = unordered)]
	fn futures_unordered(sparse: &mut Sparse<FuturesUnordered<Forever>>) {
		sparse.fire();
		let _ = black_box(sparse.set.poll_next_unpin(&mut Context::from_waker(Waker::noop())));
	}
}

const N: usize = 1000;

mod spawn_drain_1000 {
	use std::{
		hint::black_box,
		task::{Context, Poll, Waker},
	};

	use futures::stream::{FuturesUnordered, StreamExt};
	use swole::bench;

	use super::{Done, N};

	#[bench(setup = kio::Waiter::noop)]
	fn kio_tasks(waiter: &kio::Waiter) {
		let mut tasks = kio::Tasks::new();
		for _ in 0..N {
			tasks.push(Done);
		}
		while tasks.poll(waiter).is_pending() {}
		black_box(&tasks);
	}

	#[bench]
	fn futures_unordered() {
		let mut set = FuturesUnordered::new();
		for _ in 0..N {
			set.push(std::future::ready(()));
		}
		let mut cx = Context::from_waker(Waker::noop());
		// Drain to completion. A Stream returns one completion per
		// poll_next call, so this is N+1 calls; that per-item delivery is
		// part of the FuturesUnordered contract (see the module doc).
		while !matches!(set.poll_next_unpin(&mut cx), Poll::Ready(None)) {}
		black_box(&set);
	}
}

mod spawn_drain_1000_hot {
	use std::{
		future::Ready,
		hint::black_box,
		task::{Context, Poll, Waker},
	};

	use futures::stream::{FuturesUnordered, StreamExt};
	use swole::bench;

	use super::{Done, N};

	fn tasks() -> (kio::Waiter, kio::Tasks<Done>) {
		(kio::Waiter::noop(), kio::Tasks::new())
	}

	#[bench(setup = tasks)]
	fn kio_tasks((waiter, tasks): &mut (kio::Waiter, kio::Tasks<Done>)) {
		for _ in 0..N {
			tasks.push(Done);
		}
		while tasks.poll(waiter).is_pending() {}
		black_box(&tasks);
	}

	#[bench(setup = FuturesUnordered::new)]
	fn futures_unordered(set: &mut FuturesUnordered<Ready<()>>) {
		for _ in 0..N {
			set.push(std::future::ready(()));
		}
		let mut cx = Context::from_waker(Waker::noop());
		while !matches!(set.poll_next_unpin(&mut cx), Poll::Ready(None)) {}
		black_box(&set);
	}
}

/// Historical peaks: one task, one chunk, and 16 and 64 chunks (the exact reach
/// of the top wake-index word).
const PEAKS: [usize; 4] = [1, 1024, 16_384, 65_536];

/// A set that grew to its peak, every task parked, then removed all but the live
/// slots, with the live tasks' gates and how many of them each owner poll wakes.
struct Capacity {
	tasks: kio::Tasks<Parked>,
	gates: Vec<Arc<Gate>>,
	ready: usize,
	waiter: kio::Waiter,
	next: usize,
}

impl Capacity {
	fn new(peak: usize, live: impl Fn(usize) -> bool, ready: usize) -> Self {
		let gates: Vec<_> = (0..peak).map(|_| Gate::new()).collect();
		let mut tasks = kio::Tasks::new();
		let keys: Vec<_> = gates.iter().map(|gate| tasks.insert(Parked(gate.clone()))).collect();
		let _ = tasks.poll(&kio::Waiter::noop());
		let mut kept = Vec::new();
		for (slot, (key, gate)) in keys.into_iter().zip(gates).enumerate() {
			match live(slot) {
				true => kept.push(gate),
				false => drop(tasks.remove(key)),
			}
		}
		Self {
			tasks,
			ready: ready.min(kept.len()),
			gates: kept,
			waiter: kio::Waiter::noop(),
			next: 0,
		}
	}

	/// Wake the next `ready` live tasks and poll the set once.
	fn poll(&mut self) {
		for _ in 0..self.ready {
			self.gates[self.next].fire();
			self.next = (self.next + 1) % self.gates.len();
		}
		let _ = black_box(self.tasks.poll(&self.waiter));
	}
}

fn idle(peak: usize) -> Capacity {
	Capacity::new(peak, |slot| slot == 0, 0)
}

fn survivor(peak: usize) -> Capacity {
	Capacity::new(peak, |slot| slot == 0, 1)
}

fn sparse(peak: usize) -> Capacity {
	Capacity::new(peak, |slot| slot % 64 == 0, 16)
}

fn full(peak: usize) -> Capacity {
	Capacity::new(peak, |_| true, 16)
}

#[bench(args = PEAKS, setup = idle)]
fn tasks_capacity_idle(capacity: &mut Capacity) {
	capacity.poll();
}

#[bench(args = PEAKS, setup = survivor)]
fn tasks_capacity_survivor(capacity: &mut Capacity) {
	capacity.poll();
}

#[bench(args = PEAKS, setup = sparse)]
fn tasks_capacity_sparse(capacity: &mut Capacity) {
	capacity.poll();
}

#[bench(args = PEAKS, setup = full)]
fn tasks_capacity_full(capacity: &mut Capacity) {
	capacity.poll();
}

/// A set that grew to `peak` tasks which all completed, leaving their slots.
fn drained(peak: usize) -> (kio::Waiter, kio::Tasks<Done>) {
	let waiter = kio::Waiter::noop();
	let mut tasks = kio::Tasks::new();
	for _ in 0..peak {
		tasks.push(Done);
	}
	while tasks.poll(&waiter).is_pending() {}
	(waiter, tasks)
}

#[bench(args = PEAKS, setup = drained)]
fn tasks_capacity_churn((waiter, tasks): &mut (kio::Waiter, kio::Tasks<Done>)) {
	for _ in 0..16 {
		tasks.push(Done);
	}
	while tasks.poll(waiter).is_pending() {}
	black_box(&tasks);
}

fn main() {
	swole::main();
}
