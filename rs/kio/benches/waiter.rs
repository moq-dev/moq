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
//! Run with `cargo bench -p kio --bench waiter`.

use std::task::{Context, Waker};

use kio::{Park, Waiter, WaiterList};
use swole::bench;

/// Live-waiter populations spanning inline storage and heap spill.
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

fn fresh() -> Waiter {
	Waiter::new(Waker::noop().clone())
}

fn empty() -> (WaiterList, Waiter) {
	(WaiterList::new(), fresh())
}

/// Register the first waiter on a newly constructed list.
#[bench(setup = empty)]
fn waiter_register_first((mut list, waiter): (WaiterList, Waiter)) -> (WaiterList, Waiter) {
	waiter.register(&mut list);
	(list, waiter)
}

fn live(n: usize) -> (WaiterList, Vec<Waiter>, Waiter) {
	let (list, waiters) = populated(n);
	(list, waiters, fresh())
}

/// Register a fresh waiter into a list of N live entries: the dead-slot probe
/// finds nothing, and the entry is appended.
#[bench(args = SIZES, setup = live)]
fn waiter_register_live(
	(mut list, waiters, fresh): (WaiterList, Vec<Waiter>, Waiter),
) -> (WaiterList, Vec<Waiter>, Waiter) {
	fresh.register(&mut list);
	(list, waiters, fresh)
}

fn parked(n: usize) -> (WaiterList, Vec<Waiter>, Park) {
	let (list, waiters) = populated(n);
	(list, waiters, Park::default())
}

/// Re-poll with a still-registered waiter, which the list recognizes and skips.
#[bench(args = SIZES, setup = parked)]
fn waiter_reregister((list, _waiters, park): &mut (WaiterList, Vec<Waiter>, Park)) {
	let cx = Context::from_waker(Waker::noop());
	park.hold(&cx).register(list);
}

fn dead(n: usize) -> (WaiterList, Waiter) {
	let (list, waiters) = populated(n);
	drop(waiters);
	(list, fresh())
}

/// Register a fresh waiter into a list whose N entries are all dead: the probe
/// reclaims a slot in place.
#[bench(args = SIZES, setup = dead)]
fn waiter_register_dead((mut list, fresh): (WaiterList, Waiter)) -> (WaiterList, Waiter) {
	fresh.register(&mut list);
	(list, fresh)
}

/// Register-and-abandon without a wake: every call allocates a waiter, parks
/// it, and drops it, so the list must keep reclaiming the dead slot it leaves.
#[bench(setup = WaiterList::new)]
fn waiter_cancel(list: &mut WaiterList) {
	Waiter::new(Waker::noop().clone()).register(list);
}

fn lists(n: usize) -> (Park, Vec<WaiterList>) {
	(Park::default(), (0..n).map(|_| WaiterList::new()).collect())
}

/// The cycle a poll function actually drives: hold the park, register with L lists,
/// then one list wakes. The re-poll arrives with live registrations still parked on
/// the other lists, which it keeps, re-registering only on the list that woke.
///
/// Includes broad polls that remain parked on many quiet lists.
#[bench(args = [1, 2, 4, 8, 16, 32], setup = lists)]
fn waiter_park_cycle((park, lists): &mut (Park, Vec<WaiterList>)) {
	let cx = Context::from_waker(Waker::noop());
	let waiter = park.hold(&cx);
	for list in lists.iter_mut() {
		waiter.register(list);
	}
	lists[0].wake();
}

/// Fan-out sizes to sweep, crossing the inline capacity into heap spill.
const FANOUT: [usize; 4] = [8, 32, 128, 512];

fn waiters(n: usize) -> (WaiterList, Vec<Waiter>) {
	(WaiterList::new(), (0..n).map(|_| fresh()).collect())
}

/// The fan-out cycle: N waiters all parked on one list, a wake drains it, and every
/// waiter re-registers. Throughput is per registration, so a super-linear per-cycle
/// cost shows up as falling throughput at higher N.
#[bench(args = FANOUT, setup = waiters, items = |n| n)]
fn waiter_fanout_cycle((list, waiters): &mut (WaiterList, Vec<Waiter>)) {
	for waiter in waiters.iter() {
		waiter.register(list);
	}
	// Drain the production way: snapshot under the lock, wake outside.
	list.take().wake();
}

fn parks(n: usize) -> (WaiterList, WaiterList, Vec<Park>) {
	(
		WaiterList::new(),
		WaiterList::new(),
		(0..n).map(|_| Park::default()).collect(),
	)
}

/// The fan-out cycle when every waiter is also parked on a second, never-woken list
/// (a value list and a closed list under one lock, say). Each partial wake requires
/// a new waiter, so this includes allocation and reclamation at high fan-out.
#[bench(args = FANOUT, setup = parks, items = |n| n)]
fn waiter_fanout_cycle_parked((list, parked, parks): &mut (WaiterList, WaiterList, Vec<Park>)) {
	let cx = Context::from_waker(Waker::noop());
	for park in parks.iter_mut() {
		let waiter = park.hold(&cx);
		waiter.register(list);
		waiter.register(parked);
	}
	list.take().wake();
}

fn main() {
	swole::main();
}
