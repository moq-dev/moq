//! Microbenchmarks for waiter registration.
//!
//! `WaiterList::register` runs on every pending poll of every kio channel, so it is
//! the per-wakeup overhead of the whole stack. These benchmarks isolate the cases
//! that matter: a fresh registration against lists of various sizes, the
//! steady-state re-registration of an already-parked waiter, reclaiming slots from
//! abandoned waiters, and the full park/wake/re-register cycle a poll function
//! actually drives.
//!
//! Contention is deliberately absent: a `WaiterList` is always behind its caller's
//! lock (`register` takes `&mut`), so cross-thread cost belongs to that lock, not to
//! the list.
//!
//! Run with `cargo bench -p kio`.

use std::task::{Context, Waker};

use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use kio::{Park, Waiter, WaiterList};

/// Live-waiter populations to sweep. The top end is the inline capacity, past which
/// the list spills to the heap.
const SIZES: [usize; 4] = [1, 2, 8, 32];

/// A list holding `n` live registrations, plus the waiters keeping them live.
fn populated(n: usize) -> (WaiterList, Vec<Waiter>) {
	let mut list = WaiterList::new();
	let waiters: Vec<_> = (0..n)
		.map(|_| {
			let waiter = Waiter::new(Waker::noop().clone());
			waiter.register(&mut list);
			waiter
		})
		.collect();
	(list, waiters)
}

/// Register a fresh waiter into a list of N live entries: the dedup scan misses,
/// the dead-slot probe finds nothing, and the entry is appended.
fn bench_register(c: &mut Criterion) {
	let mut g = c.benchmark_group("waiter_register_live");
	for &n in &SIZES {
		g.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
			b.iter_batched(
				|| {
					let (list, waiters) = populated(n);
					(list, waiters, Waiter::new(Waker::noop().clone()))
				},
				|(mut list, waiters, fresh)| {
					fresh.register(&mut list);
					(list, waiters, fresh)
				},
				BatchSize::SmallInput,
			)
		});
	}
	g.finish();
}

/// Re-register a waiter already in a list of N live entries: the record fast path,
/// which is the steady-state cost of a poll that re-runs while still parked. Flat
/// in N, since membership is settled by the waiter's own record, not a scan.
fn bench_reregister(c: &mut Criterion) {
	let mut g = c.benchmark_group("waiter_reregister");
	for &n in &SIZES {
		g.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
			let (mut list, waiters) = populated(n);
			let last = waiters.last().unwrap();
			b.iter(|| last.register(&mut list));
		});
	}
	g.finish();
}

/// Register a fresh waiter into a list whose N entries are all dead: the dedup scan
/// misses and the probe reclaims a slot in place.
fn bench_register_dead(c: &mut Criterion) {
	let mut g = c.benchmark_group("waiter_register_dead");
	for &n in &SIZES {
		g.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
			b.iter_batched(
				|| {
					let (list, waiters) = populated(n);
					drop(waiters);
					(list, Waiter::new(Waker::noop().clone()))
				},
				|(mut list, fresh)| {
					fresh.register(&mut list);
					(list, fresh)
				},
				BatchSize::SmallInput,
			)
		});
	}
	g.finish();
}

/// Register-and-abandon without a wake: every iteration allocates a waiter, parks
/// it, and drops it, so the list must keep reclaiming the dead slot it leaves.
fn bench_cancel(c: &mut Criterion) {
	c.bench_function("waiter_cancel", |b| {
		let mut list = WaiterList::new();
		b.iter(|| Waiter::new(Waker::noop().clone()).register(&mut list));
	});
}

/// The cycle a poll function actually drives: hold the park, register with L lists,
/// then one list wakes. The re-poll arrives with live registrations still parked on
/// the other lists, which is exactly the case `Park` must not retire on.
fn bench_park_cycle(c: &mut Criterion) {
	let mut g = c.benchmark_group("waiter_park_cycle");
	// 16 lists overflow the waiter's record table, exercising eviction and the
	// scan fallback rather than the pure record fast path.
	for &lists in &[1usize, 2, 4, 8, 16] {
		g.bench_with_input(BenchmarkId::from_parameter(lists), &lists, |b, &n| {
			let cx = Context::from_waker(Waker::noop());
			let mut park = Park::default();
			let mut lists: Vec<_> = (0..n).map(|_| WaiterList::new()).collect();
			b.iter(|| {
				let waiter = park.hold(&cx);
				for list in &mut lists {
					waiter.register(list);
				}
				lists[0].wake();
			});
		});
	}
	g.finish();
}

/// Fan-out sizes to sweep, crossing the inline capacity into heap spill.
const FANOUT: [usize; 4] = [8, 32, 128, 512];

/// The fan-out cycle: N waiters all parked on one list, a wake drains it, and every
/// waiter re-registers. Throughput is per registration, so a super-linear per-cycle
/// cost shows up as falling throughput at higher N.
fn bench_fanout_cycle(c: &mut Criterion) {
	let mut g = c.benchmark_group("waiter_fanout_cycle");
	for &n in &FANOUT {
		g.throughput(Throughput::Elements(n as u64));
		g.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
			let mut list = WaiterList::new();
			let waiters: Vec<_> = (0..n).map(|_| Waiter::new(Waker::noop().clone())).collect();
			b.iter(|| {
				for waiter in &waiters {
					waiter.register(&mut list);
				}
				// Drain the production way: snapshot under the lock, wake outside.
				list.take().wake();
			});
		});
	}
	g.finish();
}

/// The fan-out cycle when every waiter is also parked on a second, never-woken list
/// (a value list and a closed list under one lock, say). The live second
/// registration is what forecloses the registered-nowhere fast path, so this is the
/// worst case for rebuilding the drained list.
fn bench_fanout_cycle_parked(c: &mut Criterion) {
	let mut g = c.benchmark_group("waiter_fanout_cycle_parked");
	for &n in &FANOUT {
		g.throughput(Throughput::Elements(n as u64));
		g.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
			let mut list = WaiterList::new();
			let mut parked = WaiterList::new();
			let waiters: Vec<_> = (0..n)
				.map(|_| {
					let waiter = Waiter::new(Waker::noop().clone());
					waiter.register(&mut parked);
					waiter
				})
				.collect();
			b.iter(|| {
				for waiter in &waiters {
					waiter.register(&mut list);
					waiter.register(&mut parked);
				}
				// Drain the production way: snapshot under the lock, wake outside.
				list.take().wake();
			});
		});
	}
	g.finish();
}

criterion_group!(
	benches,
	bench_register,
	bench_reregister,
	bench_register_dead,
	bench_cancel,
	bench_park_cycle,
	bench_fanout_cycle,
	bench_fanout_cycle_parked
);
criterion_main!(benches);
