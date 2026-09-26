use std::{
	fmt,
	future::Future,
	pin::Pin,
	// std, not `crate::sync`: loom's Arc has no `downgrade`. See `sync.rs`.
	sync::{
		Arc, OnceLock, Weak,
		atomic::{AtomicBool, AtomicU64, Ordering},
	},
	task::{Context, Poll, Waker},
};

use smallvec::SmallVec;

/// Number of slots stored inline before spilling to the heap.
const INLINE_WAITERS: usize = 4;

/// Handle passed to poll functions for registering with [`WaiterList`]s.
///
/// Holds the task's [`Waker`] by value and, lazily, a shared `Arc<Waker>` that list
/// entries reference weakly. The `Arc` is allocated on the first [`Self::register`],
/// so a poll that resolves without ever parking never touches the heap. Its `Weak`s
/// go dead the moment the owning [`Waiter`] drops, which is how a [`WaiterList`]
/// reclaims slots with no explicit deregister.
///
/// A clone shares this identity: it wakes the same task, its registrations count as
/// the original's, and they all stay live until every clone drops.
pub struct Waiter {
	// The task waker. Cloning it is cheap (an atomic bump, no allocation).
	waker: Waker,

	// The shared handle downgraded into every list this waiter registers with. Created on the
	// first `register` (a poll that never parks never allocates it), then reused so multiple
	// lists in one poll share a single allocation whose `Weak`s die together when the waiter drops.
	shared: OnceLock<Arc<Waker>>,

	// The list tag each registration was made under, so registering on a list that
	// still holds this waiter is a no-op. Filled from the front, zero past the end,
	// and cleared all at once.
	parked: [AtomicU64; PARKED],

	// A registration found no free slot in `parked`, so a list may hold this waiter
	// unrecorded and a later registration there would stack a duplicate.
	lost: AtomicBool,
}

/// Lists a waiter remembers being parked on. A relay's busiest tasks park on up to 8.
const PARKED: usize = 8;

impl Waiter {
	/// Create a new waiter from an async [`Waker`].
	pub fn new(waker: Waker) -> Self {
		Self {
			waker,
			shared: OnceLock::new(),
			parked: Default::default(),
			lost: AtomicBool::new(false),
		}
	}

	/// Create a no-op waiter that discards registrations.
	pub fn noop() -> Self {
		Self::new(Waker::noop().clone())
	}

	/// Register this waiter with a [`WaiterList`] for future notification.
	///
	/// Delegates to [`WaiterList::register`], a no-op while the list still holds it.
	pub fn register(&self, list: &mut WaiterList) {
		list.register(self);
	}

	/// The underlying task [`Waker`].
	pub fn waker(&self) -> &Waker {
		&self.waker
	}

	/// A [`Context`] that wakes this waiter's task, for calling a foreign `poll_*` method.
	pub fn context(&self) -> Context<'_> {
		Context::from_waker(&self.waker)
	}

	/// The shared waker handle downgraded into lists, allocated on first use and cached so
	/// repeat registrations (across polls, or across lists in one poll) share one allocation.
	fn shared(&self) -> &Arc<Waker> {
		self.shared.get_or_init(|| Arc::new(self.waker.clone()))
	}

	/// Record a registration under `tag`, or report one already recorded there.
	///
	/// Returns whether the list needs an entry: false only when a record proves it
	/// still holds this waiter. A record of an older round of the same list is
	/// replaced, since the drain that ended that round took the entry with it.
	fn record(&self, tag: u64) -> bool {
		// Retiring anyway, so skip the bookkeeping: duplicates die with the waiter.
		if self.lost.load(Ordering::Relaxed) {
			return true;
		}

		// The common case, kept a tight loop of its own.
		if self.parked.iter().any(|slot| slot.load(Ordering::Relaxed) == tag) {
			return false;
		}

		let serial = tag >> ROUND_BITS;
		for slot in &self.parked {
			let old = slot.load(Ordering::Relaxed);
			// The first empty slot ends the records. A record of the same list in an
			// older round is stale: the drain that ended that round took the entry.
			if old >> ROUND_BITS == serial {
				// Only a registration on this list, under its lock, writes this slot.
				slot.store(tag, Ordering::Relaxed);
				return true;
			}
			// Claimed, not stored: another thread registering this waiter on another
			// list may be taking the same empty slot.
			if old == 0
				&& slot
					.compare_exchange(0, tag, Ordering::Relaxed, Ordering::Relaxed)
					.is_ok()
			{
				return true;
			}
		}
		self.lost.store(true, Ordering::Relaxed);
		true
	}

	/// Whether every list holding this waiter is recorded, so registering it again
	/// cannot stack a duplicate.
	fn reusable(&self) -> bool {
		if !self.lost.load(Ordering::Relaxed) {
			return true;
		}
		if self.shared.get().is_some_and(|shared| Arc::weak_count(shared) > 0) {
			return false;
		}
		// Every list let go, so every record is stale: start over.
		for slot in &self.parked {
			slot.store(0, Ordering::Relaxed);
		}
		self.lost.store(false, Ordering::Relaxed);
		true
	}

	/// Poll a foreign [`Future`] against this waiter, so it re-wakes the enclosing
	/// `poll_*` step when it is ready.
	pub fn poll_future<F: Future + ?Sized>(&self, future: Pin<&mut F>) -> Poll<F::Output> {
		future.poll(&mut self.context())
	}
}

impl Clone for Waiter {
	fn clone(&self) -> Self {
		// Force the shared Arc into existence so both handles point at one
		// allocation; a lazily separate Arc would give the clone its own (shorter)
		// registration lifetime, silently killing registrations when it drops.
		let shared = self.shared().clone();
		Self {
			waker: self.waker.clone(),
			shared: OnceLock::from(shared),
			parked: std::array::from_fn(|i| AtomicU64::new(self.parked[i].load(Ordering::Relaxed))),
			lost: AtomicBool::new(self.lost.load(Ordering::Relaxed)),
		}
	}
}

/// A list of weak wakers waiting for notification.
///
/// Slots live inline (up to `INLINE_WAITERS`) and only spill to the heap
/// for unusually high concurrency. A rotating cursor amortizes garbage
/// collection across many `register` calls so the list doesn't grow
/// unboundedly while keeping per-call cost O(1).
pub struct WaiterList {
	entries: SmallVec<[Weak<Waker>; INLINE_WAITERS]>,
	/// Rotating cursor for opportunistic GC on `register`.
	cursor: usize,
	/// A serial unique to this list in the high bits and a drain count in the low
	/// [`ROUND_BITS`], so an unchanged tag proves an entry made under it is still
	/// here. Zero until the first registration.
	tag: u64,
}

/// Low bits of a list tag that count drains. A wrap takes a fresh serial instead,
/// so no tag is ever reused.
const ROUND_BITS: u32 = 16;

/// A serial for a list tag, unique for the life of the process. Handed out in
/// per-thread blocks so lists created on many threads don't share a cache line.
fn serial() -> u64 {
	use std::cell::Cell;

	const BLOCK: u64 = 1024;
	static NEXT: AtomicU64 = AtomicU64::new(1);
	thread_local! {
		static RANGE: Cell<(u64, u64)> = const { Cell::new((0, 0)) };
	}

	RANGE.with(|range| {
		let (mut next, mut end) = range.get();
		if next == end {
			next = NEXT.fetch_add(BLOCK, Ordering::Relaxed);
			end = next + BLOCK;
			assert!(end < 1 << (64 - ROUND_BITS), "waiter list serials exhausted");
		}
		range.set((next + 1, end));
		next << ROUND_BITS
	})
}

impl WaiterList {
	/// Create an empty list, allocating nothing until the first [`register`](Self::register).
	pub fn new() -> Self {
		Self {
			entries: SmallVec::new(),
			cursor: 0,
			tag: 0,
		}
	}

	/// Register a waiter, a no-op while this list still holds it.
	///
	/// The list's tag changes on every drain and the waiter records the tag it
	/// joined under, so a matching record proves the entry is still here and a
	/// waiter kept across polls re-registers for free.
	///
	/// Each call probes at most two slots at the rotating cursor and reuses
	/// a dead one in place. The cursor advances on each live probe so the
	/// window covers the list over time. A list about to grow sweeps every
	/// dead slot first.
	pub fn register(&mut self, waiter: &Waiter) {
		if self.tag == 0 {
			self.tag = serial();
		}
		if !waiter.record(self.tag) {
			return;
		}

		let new_weak = Arc::downgrade(waiter.shared());

		for _ in 0..self.entries.len().min(2) {
			if self.entries[self.cursor].strong_count() == 0 {
				// Reuse the dead slot in place. Each Waiter owns a
				// unique Arc<Waker>, so strong_count == 0 uniquely
				// identifies a slot whose owner has been dropped.
				// No will_wake / pointer comparison needed.
				self.entries[self.cursor] = new_weak;
				return;
			}
			self.cursor = (self.cursor + 1) % self.entries.len();
		}

		if self.entries.len() == self.entries.capacity() {
			// Probing alone loses to a list that many live waiters keep re-registering on
			// and nothing wakes: each retired waiter leaves a dead slot the probe window
			// rarely lands on, so the list grows for as long as it lives.
			self.entries.retain(|entry| entry.strong_count() > 0);
			// Leave at least half free, so each sweep is paid for by the pushes before it.
			self.entries.reserve(self.entries.len());
			self.cursor = 0;
		}
		self.entries.push(new_weak);
	}

	/// Start a new round, so no record of an entry made in the last one matches.
	fn drained(&mut self) {
		self.cursor = 0;
		if self.tag == 0 || self.entries.is_empty() {
			// No serial yet (a `take` snapshot, say) or no entry: no record can match
			// the current tag, and a zero tag must stay zero to draw a fresh serial.
			return;
		}
		self.tag += 1;
		if self.tag & ((1 << ROUND_BITS) - 1) == 0 {
			// The round wrapped: take a fresh serial rather than reuse a tag.
			self.tag = 0;
		}
	}

	/// Drain all entries into a new [`WaiterList`], leaving this one empty.
	pub fn take(&mut self) -> Self {
		self.drained();
		Self {
			entries: std::mem::take(&mut self.entries),
			cursor: 0,
			tag: 0,
		}
	}

	/// Wake all live waiters, draining the list.
	pub fn wake(&mut self) {
		self.drained();
		for waker in self.entries.drain(..).filter_map(|w| w.upgrade()) {
			waker.wake_by_ref();
		}
	}
}

impl Default for WaiterList {
	fn default() -> Self {
		Self::new()
	}
}

impl fmt::Debug for WaiterList {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("WaiterList").field("len", &self.entries.len()).finish()
	}
}

/// Holds a parked [`Waiter`] between polls, so the registrations it made outlive the
/// poll that made them.
///
/// A [`WaiterList`] keeps only a `Weak`, so a waiter built inside a poll dies the
/// moment that poll returns, taking its registrations with it. Whoever drives kio's
/// `poll_*` methods from a [`Context`]-based poll (a `Future`, or a `poll_*` trait
/// method) embeds one `Park` per logical operation and calls [`hold`](Self::hold) at
/// the top of each poll:
///
/// ```
/// # use std::task::{Context, Poll};
/// # use kio::{Park, Waiter};
/// # struct State;
/// # impl State {
/// #     fn poll_step(&mut self, _: &Waiter) -> Poll<()> { Poll::Pending }
/// # }
/// # struct Driver { park: Park, state: State }
/// # impl Driver {
/// fn poll(&mut self, cx: &mut Context<'_>) -> Poll<()> {
///     let waiter = self.park.hold(cx);
///     self.state.poll_step(waiter)
/// }
/// # }
/// ```
///
/// Keep the park in a field *disjoint* from the state the body polls, as above. The
/// returned borrow lives as long as the waiter does, so a park bundled into the same
/// `&mut self` the body needs would collide with it.
///
/// `Clone` yields an *empty* park: an in-progress registration belongs to the handle
/// that parked it, so a cloned handle starts idle. This is what lets a containing
/// type stay `Clone`.
#[derive(Default)]
pub struct Park(Option<Waiter>);

impl Park {
	/// Create a park already holding `waiter`, for the rarer case where one exists
	/// before the first poll (a `poll_*` method stashing the waiter its caller passed
	/// in, say). Use [`Default`] for an empty park.
	pub fn new(waiter: Waiter) -> Self {
		Self(Some(waiter))
	}

	/// Hold a waiter for this poll, keeping it (and its registrations) alive until
	/// the next call.
	///
	/// Retention happens here, *before* the body runs, so a body that returns early
	/// (the usual `ready!` on a nested poll) still leaves its registrations live.
	/// There is no second call to forget.
	///
	/// The held waiter is reused when it would wake the same task, so a steady-state
	/// park allocates nothing even when one list woke it and others still hold it:
	/// registering again on those is a no-op (see [`WaiterList::register`]). It is
	/// retired for a fresh one when the task changed, or when it parked on more lists
	/// than it can record while some still hold it, since re-registering there could
	/// stack duplicates a list never collects: it reclaims a slot only once its `Arc`
	/// dies.
	///
	/// A reused waiter keeps the registrations its next poll does not renew, so a
	/// list it has moved on from may wake it once more, spuriously.
	pub fn hold(&mut self, cx: &Context<'_>) -> &Waiter {
		let reuse = self
			.0
			.as_ref()
			.is_some_and(|waiter| cx.waker().will_wake(&waiter.waker) && waiter.reusable());
		if !reuse {
			// The outgoing waiter drops here, killing its registrations so the lists
			// can reclaim those slots.
			self.0 = Some(Waiter::new(cx.waker().clone()));
		}
		self.0.as_ref().unwrap()
	}
}

impl Clone for Park {
	fn clone(&self) -> Self {
		Self(None)
	}
}

impl fmt::Debug for Park {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("Park").field("parked", &self.0.is_some()).finish()
	}
}

/// Create a [`Future`] from a poll function that receives a [`Waiter`].
///
/// The waiter is kept alive between polls so its registration in a
/// [`WaiterList`] remains valid until the next poll replaces it.
pub fn wait<F, R>(poll: F) -> impl Future<Output = R>
where
	F: FnMut(&Waiter) -> Poll<R> + Unpin,
{
	crate::Pending::new(poll)
}

#[cfg(all(test, not(loom)))]
mod tests {
	use super::*;

	#[test]
	fn poll_future_bridges_a_std_future() {
		let waiter = Waiter::noop();

		// A ready future resolves through the waiter.
		let fut = std::pin::pin!(std::future::ready(7u8));
		assert_eq!(waiter.poll_future(fut), Poll::Ready(7));

		// A never-ready future stays pending.
		let fut = std::pin::pin!(std::future::pending::<u8>());
		assert_eq!(waiter.poll_future(fut), Poll::Pending);

		// A type-erased future works too (the `?Sized` bound).
		let mut boxed: Pin<Box<dyn Future<Output = u8>>> = Box::pin(std::future::ready(9u8));
		assert_eq!(waiter.poll_future(boxed.as_mut()), Poll::Ready(9));
	}

	// `Waiter` is shared behind `&self` across threads, so the lazily allocated
	// `shared` handle must use a thread-safe cell. A `!Sync` waiter silently
	// infects `Pending` and `Shared`, and through them every moq-net consumer.
	const fn assert_sync<T: Sync>() {}

	const _: () = {
		assert_sync::<Waiter>();
		assert_sync::<crate::Pending<crate::Consumer<u32>>>();
		assert_sync::<crate::Shared<u32>>();
	};

	/// Three lists sit inside every kio channel's state cell, and that cell is paid
	/// per announced broadcast and per cached group in moq-net whether or not
	/// anything ever parks. Growing either is invisible at the call site, so bound
	/// them: a diff that has to raise these numbers should say why.
	///
	/// kio enables `smallvec/union`, which drops the inline array's enum tag, so the
	/// list costs the same whatever else is in the build graph.
	#[test]
	#[cfg(target_pointer_width = "64")]
	fn the_list_stays_small() {
		let list = std::mem::size_of::<WaiterList>();
		let state = std::mem::size_of::<crate::State<()>>();
		assert!(list <= 56, "WaiterList grew to {list} B");
		assert!(state <= 176, "State<()> grew to {state} B");
	}

	#[test]
	fn park_survives_a_poll_that_returns_early() {
		let waker = Waker::noop().clone();
		let cx = Context::from_waker(&waker);
		let mut park = Park::default();
		let mut list = WaiterList::new();

		// A poll body that bails out the moment a nested poll is pending (the `ready!`
		// idiom) never runs any bookkeeping of its own after registering, so retention
		// has to be in place already or the wakeup is lost forever.
		fn poll_step(park: &mut Park, cx: &Context<'_>, list: &mut WaiterList, nested: Poll<u8>) -> Poll<u8> {
			let waiter = park.hold(cx);
			waiter.register(list);
			// Returns straight out of the function, running nothing below it.
			let value = std::task::ready!(nested);
			Poll::Ready(value + 1)
		}

		assert!(poll_step(&mut park, &cx, &mut list, Poll::Pending).is_pending());
		assert_eq!(
			list.entries[0].strong_count(),
			1,
			"an early return must leave the registration live"
		);

		// The wakeup still reaches it.
		list.wake();
	}

	#[test]
	fn partial_wakes_retire_registrations_across_many_lists() {
		let waker = Waker::noop().clone();
		let cx = Context::from_waker(&waker);
		let mut park = Park::default();
		let mut lists: Vec<_> = (0..32).map(|_| WaiterList::new()).collect();
		for _ in 0..100 {
			let waiter = park.hold(&cx);
			for list in &mut lists {
				waiter.register(list);
			}
			let previous = Arc::downgrade(waiter.shared());
			lists[0].wake();
			park.hold(&cx);
			assert!(previous.upgrade().is_none(), "partial wake retained old registrations");
			for list in &lists {
				assert!(list.entries.len() <= 2, "abandoned registrations accumulated");
			}
		}
	}

	#[test]
	fn drained_waiter_is_reused() {
		let waker = Waker::noop().clone();
		let cx = Context::from_waker(&waker);
		let mut park = Park::default();
		let mut list = WaiterList::new();
		let waiter = park.hold(&cx);
		waiter.register(&mut list);
		let first = waiter.shared().clone();
		list.wake();
		assert!(Arc::ptr_eq(&first, park.hold(&cx).shared()));
	}

	#[test]
	fn abandoned_waiters_do_not_grow_the_list() {
		let mut list = WaiterList::new();
		for _ in 0..1_000 {
			Waiter::noop().register(&mut list);
		}
		assert!(list.entries.len() <= 2);
	}

	/// Tasks parked on one list are retired and re-register in whatever order they
	/// wake, while the list itself is never woken (a rarely-changing value many tasks
	/// watch). The probe window alone let such a list grow without bound.
	#[test]
	fn retired_live_waiters_do_not_grow_the_list() {
		const LIVE: usize = 64;
		let mut list = WaiterList::new();
		let mut waiters: Vec<Waiter> = (0..LIVE).map(|_| Waiter::noop()).collect();
		for waiter in &waiters {
			waiter.register(&mut list);
		}

		let mut seed = 1u64;
		for _ in 0..100_000 {
			seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
			let i = (seed >> 33) as usize % LIVE;
			waiters[i] = Waiter::noop();
			waiters[i].register(&mut list);
		}

		let len = list.entries.len();
		assert!(len <= 4 * LIVE, "{len} slots for {LIVE} live waiters");
	}

	#[test]
	fn register_is_idempotent_until_the_list_drains() {
		let mut list = WaiterList::new();
		let waiter = Waiter::new(Waker::noop().clone());
		waiter.register(&mut list);
		waiter.register(&mut list);
		assert_eq!(list.entries.len(), 1, "a waiter the list holds must not stack");

		list.wake();
		waiter.register(&mut list);
		assert_eq!(list.entries.len(), 1, "a drained waiter must register again");
	}

	/// The relay's shape: a task parks on a few lists, one of them wakes it, and the
	/// rest stay quiet. Retiring the waiter there cost an `Arc` per poll.
	#[test]
	fn partial_wakes_reuse_the_waiter() {
		let waker = Waker::noop().clone();
		let cx = Context::from_waker(&waker);
		let mut park = Park::default();
		let mut lists: Vec<_> = (0..PARKED).map(|_| WaiterList::new()).collect();
		let first = park.hold(&cx).shared().clone();
		for round in 0..100 {
			let waiter = park.hold(&cx);
			assert!(Arc::ptr_eq(&first, waiter.shared()), "retired a recorded waiter");
			for list in &mut lists {
				waiter.register(list);
			}
			lists[round % PARKED].wake();
		}
		for list in &lists {
			assert!(list.entries.len() <= 1, "re-registration stacked a duplicate");
		}
	}

	/// The tag moves with the list, so a list that moved still recognizes its waiter.
	#[test]
	fn a_moved_list_still_holds_its_waiter() {
		let waiter = Waiter::new(Waker::noop().clone());
		let mut list = WaiterList::new();
		waiter.register(&mut list);

		let mut moved = std::mem::take(&mut list);
		waiter.register(&mut moved);
		assert_eq!(moved.entries.len(), 1);

		// The emptied original is a different list now.
		waiter.register(&mut list);
		assert_eq!(list.entries.len(), 1);
	}

	/// A `take` snapshot has no serial, and waking it must not invent one: every
	/// snapshot would share it, and a reused one would skip a real registration.
	#[test]
	fn a_woken_snapshot_draws_a_fresh_serial() {
		let waiter = Waiter::new(Waker::noop().clone());
		let mut snapshots = [WaiterList::new(), WaiterList::new()].map(|mut list| {
			Waiter::new(Waker::noop().clone()).register(&mut list);
			let mut snapshot = list.take();
			snapshot.wake();
			snapshot
		});
		for snapshot in &mut snapshots {
			waiter.register(snapshot);
			assert_eq!(snapshot.entries.len(), 1, "a snapshot skipped a real registration");
		}
	}

	/// A tag must never repeat, or a stale record would skip a registration the list
	/// no longer holds and the wakeup would be lost.
	#[test]
	fn a_wrapped_round_takes_a_fresh_serial() {
		let stale = Waiter::new(Waker::noop().clone());
		let mut list = WaiterList::new();
		stale.register(&mut list);
		list.wake();

		// Skip ahead to the last round, then drain once more.
		list.tag |= (1 << ROUND_BITS) - 1;
		let other = Waiter::new(Waker::noop().clone());
		other.register(&mut list);
		list.wake();

		stale.register(&mut list);
		assert_eq!(list.entries.len(), 1, "a wrapped round matched a stale record");
	}

	#[test]
	fn park_retires_a_waiter_for_another_task() {
		struct Nop;
		// Not `Waker::noop()`, which clippy suggests: that is a singleton, so the two
		// wakers below would name the same task and the assertion would pass without
		// testing anything. What this needs is two wakers that do nothing and differ.
		#[expect(clippy::manual_noop_waker, reason = "the two wakers must not be identical")]
		impl std::task::Wake for Nop {
			fn wake(self: Arc<Self>) {}
		}

		let waker_a = Waker::from(Arc::new(Nop));
		let waker_b = Waker::from(Arc::new(Nop));
		let mut park = Park::default();

		let first = park.hold(&Context::from_waker(&waker_a)).shared().clone();

		// A different task must not inherit the parked waiter, or its wakeup goes to
		// whoever polled last.
		let waiter = park.hold(&Context::from_waker(&waker_b));
		assert!(
			!Arc::ptr_eq(&first, waiter.shared()),
			"a waiter for another task was reused"
		);
	}

	#[test]
	fn park_new_starts_holding() {
		let waker = Waker::noop().clone();
		let cx = Context::from_waker(&waker);
		let mut list = WaiterList::new();

		let waiter = Waiter::new(cx.waker().clone());
		waiter.register(&mut list);
		let mut park = Park::new(waiter);
		assert_eq!(list.entries[0].strong_count(), 1, "a constructed park must hold");

		// A wakeup drains the entry, so the next poll picks that same waiter back up
		// rather than allocating another.
		let shared = park.0.as_ref().unwrap().shared().clone();
		list.wake();
		assert!(
			Arc::ptr_eq(&shared, park.hold(&cx).shared()),
			"the constructed waiter was not reused"
		);
	}

	#[test]
	fn park_clone_is_idle() {
		let waker = Waker::noop().clone();
		let cx = Context::from_waker(&waker);
		let mut park = Park::default();
		park.hold(&cx);
		assert!(park.0.is_some());

		// An in-progress registration belongs to the original handle.
		assert!(park.clone().0.is_none(), "a cloned park must start idle");
	}

	#[test]
	fn waiter_clone_shares_identity() {
		let waker = Waker::noop().clone();
		let cx = Context::from_waker(&waker);
		let mut list = WaiterList::new();

		let waiter = Waiter::new(cx.waker().clone());
		let clone = waiter.clone();
		assert!(Arc::ptr_eq(waiter.shared(), clone.shared()));

		// A registration made through the clone belongs to the shared identity, so
		// dropping the clone alone must not kill it.
		clone.register(&mut list);
		drop(clone);
		assert_eq!(list.entries[0].strong_count(), 1, "the original must keep it live");

		drop(waiter);
		assert_eq!(list.entries[0].strong_count(), 0, "the last handle must release it");
	}

	#[test]
	fn wait_output_need_not_be_unpin() {
		struct NotUnpin(#[allow(dead_code)] std::marker::PhantomPinned);

		let mut fut = std::pin::pin!(crate::wait(|_| Poll::Ready(NotUnpin(std::marker::PhantomPinned))));
		let mut cx = Context::from_waker(Waker::noop());
		assert!(fut.as_mut().poll(&mut cx).is_ready());
	}
}
