use std::{
	fmt,
	future::Future,
	marker::PhantomData,
	pin::Pin,
	// std, not `crate::sync`: loom's Arc has no `downgrade`, and `Waker::from` takes
	// std's. See `sync.rs`.
	sync::{Arc, OnceLock, Weak},
	task::{Context, Poll, Wake, Waker},
};

use smallvec::SmallVec;

use crate::{
	lock::{Lock, WeakLock},
	sync::Mutex,
};

/// Number of slots stored inline before spilling to the heap.
const INLINE_WAITERS: usize = 32;

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
}

impl Waiter {
	/// Create a new waiter from an async [`Waker`].
	pub fn new(waker: Waker) -> Self {
		Self {
			waker,
			shared: OnceLock::new(),
		}
	}

	/// Create a no-op waiter that discards registrations.
	pub fn noop() -> Self {
		Self::new(Waker::noop().clone())
	}

	/// Register this waiter with a [`WaiterList`] for future notification.
	pub fn register(&self, list: &mut WaiterList) {
		list.register(self);
	}

	/// The underlying task [`Waker`], for hand-rolling foreign-future integration. Prefer
	/// [`poll_future`](Self::poll_future), which wraps the usual [`Context`] dance.
	pub fn waker(&self) -> &Waker {
		&self.waker
	}

	/// The shared waker handle downgraded into lists, allocated on first use and cached so
	/// repeat registrations (across polls, or across lists in one poll) share one allocation.
	fn shared(&self) -> &Arc<Waker> {
		self.shared.get_or_init(|| Arc::new(self.waker.clone()))
	}

	/// Poll a foreign [`Future`] against this waiter, so it re-wakes the enclosing
	/// `poll_*` step when it is ready.
	pub fn poll_future<F: Future + ?Sized>(&self, future: Pin<&mut F>) -> Poll<F::Output> {
		future.poll(&mut Context::from_waker(self.waker()))
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
}

impl WaiterList {
	/// Create an empty list, allocating nothing until the first [`register`](Self::register).
	pub fn new() -> Self {
		Self {
			entries: SmallVec::new(),
			cursor: 0,
		}
	}

	/// Register a waiter.
	///
	/// Performs a small, bounded amount of garbage collection: probes the
	/// slot at the rotating cursor, replacing it in place if dead. The
	/// cursor advances on each append so the probe window covers the
	/// whole list over time.
	pub fn register(&mut self, waiter: &Waiter) {
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

		self.entries.push(new_weak);
	}

	/// Drain all entries into a new [`WaiterList`], leaving this one empty.
	pub fn take(&mut self) -> Self {
		self.cursor = 0;
		Self {
			entries: std::mem::take(&mut self.entries),
			cursor: 0,
		}
	}

	/// Wake all live waiters, draining the list.
	pub fn wake(&mut self) {
		self.cursor = 0;
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
	/// The held waiter is reused when it would wake the same task *and* has no live
	/// list registrations (the usual case after a wakeup, which drains every entry),
	/// so a steady-state park allocates nothing. Otherwise it is retired for a fresh
	/// one: a still-registered waiter must not be registered again, because
	/// [`WaiterList`] reclaims a slot only once its `Arc` dies, so reusing one with
	/// live entries would stack duplicates the list could never collect.
	pub fn hold(&mut self, cx: &Context<'_>) -> &Waiter {
		let reuse = self.0.as_ref().is_some_and(|waiter| {
			cx.waker().will_wake(&waiter.waker) && waiter.shared.get().is_none_or(|shared| Arc::weak_count(shared) == 0)
		});
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

/// A [`WaiterList`] that is shared, and that a [`Waker`] can wake.
///
/// A plain [`WaiterList`] is a field, woken by whoever owns the state around it. That is
/// enough until the thing being waited on is a *foreign* future, which takes a `Waker`
/// and keeps exactly one. Hand it a caller's waker and the most recent caller owns the
/// wakeup for everybody: when that one walks away the notification goes nowhere, while
/// the others sit parked with live registrations. Poll such a future with
/// [`waker`](Self::waker) instead. It outlives every caller, and waking it fans out to
/// the whole list.
///
/// Two shapes, depending on where the list should live:
///
/// - [`new`](Fan::new) owns one, for a caller whose state is behind its own lock.
/// - [`project`](Self::project) reaches into a list already inside a [`Lock`], so there
///   is one lock and one list rather than two. Prefer it when the state is a `Lock`.
///
/// Role-less and cloneable like [`Queue`](crate::Queue): every handle can wake or hand
/// out the waker.
pub struct Fan<T = WaiterList> {
	inner: Arc<FanInner<T>>,
}

/// Where the list a [`Fan`] wakes actually lives.
enum Target<T> {
	/// The fan owns it.
	Owned(Lock<T>),

	/// It belongs to state someone else owns. Weak on purpose: that state routinely
	/// holds the fan's waker (the future being polled with it usually lives there),
	/// and a strong handle would make that a cycle.
	Projected(WeakLock<T>),
}

impl<T> Target<T> {
	fn upgrade(&self) -> Option<Lock<T>> {
		match self {
			Self::Owned(lock) => Some(lock.clone()),
			Self::Projected(weak) => weak.upgrade(),
		}
	}
}

struct FanInner<T> {
	target: Target<T>,

	/// Reaches the list inside `T`. Identity when the fan owns a bare [`WaiterList`].
	project: fn(&mut T) -> &mut WaiterList,

	/// Deferral state, under a lock of its own. It cannot ride along inside the list's
	/// state, because a projected fan does not own that state's layout.
	defer: Mutex<Defer>,
}

#[derive(Default)]
struct Defer {
	/// How many [`Hold`]s are outstanding. While this is non-zero a wake is recorded
	/// rather than delivered.
	held: usize,

	/// A wake arrived while held, and is still owed to the list.
	owed: bool,
}

impl<T> FanInner<T> {
	fn defer_if_held(&self) -> bool {
		let mut defer = self.defer.lock().expect("mutex poisoned");
		if defer.held == 0 {
			return false;
		}

		defer.owed = true;
		true
	}

	fn notify(&self) {
		// A projected fan can wake inline while the caller holds the target lock.
		if self.defer_if_held() {
			return;
		}

		// Gone already: whatever was parked went with it.
		let Some(lock) = self.target.upgrade() else {
			return;
		};

		let mut waiters = {
			let mut state = lock.lock();

			// A wake can block on the target after the first check. Recheck while the
			// target is locked, and keep both locks through the drain, so a hold
			// created in that window defers it.
			let mut defer = self.defer.lock().expect("mutex poisoned");
			if defer.held > 0 {
				defer.owed = true;
				return;
			}

			(self.project)(&mut state).take()
		};

		// Outside both locks: a waker may resume its task inline, and a resumed waiter's
		// first move is to take the lock its list lives under.
		waiters.wake();
	}
}

impl<T: Send + 'static> Wake for FanInner<T> {
	fn wake(self: Arc<Self>) {
		self.notify();
	}

	fn wake_by_ref(self: &Arc<Self>) {
		self.notify();
	}
}

impl Fan<WaiterList> {
	/// Create a fan that owns its list.
	///
	/// For a caller whose own state is not a [`Lock`]. When it is, prefer
	/// [`project`](Self::project): it wakes a list already in there, instead of adding a
	/// second list behind a second lock.
	pub fn new() -> Self {
		Self::build(Target::Owned(Lock::new(WaiterList::new())), |list| list)
	}
}

impl<T: Send + 'static> Fan<T> {
	/// A fan over a [`WaiterList`] that lives inside `state`.
	///
	/// One lock and one list: parking stays a plain `&mut` call on that list wherever
	/// the state is already locked, and this supplies only what a `WaiterList` cannot:
	/// a [`Waker`] of its own, and [`hold`](Self::hold).
	///
	/// The reference is weak, so `state` may hold this fan (or its waker) without
	/// leaking itself. Once `state` is dropped, waking is a no-op.
	pub fn project(state: &Lock<T>, project: fn(&mut T) -> &mut WaiterList) -> Self {
		Self::build(Target::Projected(state.downgrade()), project)
	}

	fn build(target: Target<T>, project: fn(&mut T) -> &mut WaiterList) -> Self {
		Self {
			inner: Arc::new(FanInner {
				target,
				project,
				defer: Mutex::new(Defer::default()),
			}),
		}
	}

	/// Park a waiter until the next wake.
	///
	/// The registration is weak and owned by the waiter, exactly as in [`WaiterList`]: a
	/// caller that gives up releases its slot by dropping.
	///
	/// This takes the lock the list lives under, so a *projected* fan cannot use it from
	/// inside that lock. Park on the list directly there, which is the point of
	/// [`project`](Self::project).
	pub fn register(&self, waiter: &Waiter) {
		if let Some(lock) = self.inner.target.upgrade() {
			let mut state = lock.lock();
			waiter.register((self.inner.project)(&mut state));
		}
	}

	/// Wake every parked waiter, draining the list.
	///
	/// Records the wake instead while a [`hold`](Self::hold) is outstanding. Takes the
	/// list's lock, so the same caveat as [`register`](Self::register) applies.
	pub fn wake(&self) {
		self.inner.notify();
	}

	/// A [`Waker`] that wakes every parked waiter.
	///
	/// Cache it rather than building one per poll: a foreign future compares the waker it
	/// was given against the new one with `will_wake` to decide whether to re-register,
	/// and a fresh handle each time defeats that.
	pub fn waker(&self) -> Waker {
		Waker::from(self.inner.clone())
	}

	/// Hold back wakes until the returned guard drops.
	///
	/// For polling a foreign future with [`waker`](Self::waker) while holding a lock that
	/// the parked waiters will take when they resume. Such a future can wake its waker
	/// *inline* (`FuturesUnordered`, for one, notifies its parent from a child's `wake`),
	/// and delivering that would resume a waiter straight into the lock the waker fired
	/// under.
	///
	/// **Drop the guard once that lock is released, not before**: the deferred wake is
	/// delivered where the guard drops, so dropping it early puts the hazard back. Nested
	/// holds are fine, and only the last one out delivers.
	#[must_use = "wakes are held back only while the guard is alive"]
	pub fn hold(&self) -> Hold<T> {
		self.inner.defer.lock().expect("mutex poisoned").held += 1;

		Hold { fan: self.clone() }
	}
}

impl Default for Fan<WaiterList> {
	fn default() -> Self {
		Self::new()
	}
}

impl<T> Clone for Fan<T> {
	fn clone(&self) -> Self {
		Self {
			inner: self.inner.clone(),
		}
	}
}

impl<T> fmt::Debug for Fan<T> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		let defer = self.inner.defer.lock().expect("mutex poisoned");
		f.debug_struct("Fan")
			.field("held", &defer.held)
			.field("owed", &defer.owed)
			.finish_non_exhaustive()
	}
}

/// Defers wakes on a [`Fan`] until dropped. Created by [`Fan::hold`].
pub struct Hold<T = WaiterList> {
	fan: Fan<T>,
}

impl<T> Drop for Hold<T> {
	fn drop(&mut self) {
		let owed = {
			let mut defer = self.fan.inner.defer.lock().expect("mutex poisoned");
			defer.held -= 1;

			// Still held by someone else: the wake stays owed, and the last one out
			// delivers it.
			match defer.held {
				0 => std::mem::take(&mut defer.owed),
				_ => false,
			}
		};

		if owed {
			// `notify`, not `wake`: `Drop` may not ask for bounds the struct does not
			// carry, and the notify path needs none.
			self.fan.inner.notify();
		}
	}
}

impl<T> fmt::Debug for Hold<T> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("Hold").finish_non_exhaustive()
	}
}

/// Future that drives a poll function, managing waiter lifetime across polls.
struct WaiterFn<F, R> {
	poll: F,
	park: Park, // Retain a parked waiter so its registrations survive.
	// `fn() -> R` keeps the marker `Unpin` (and `Send`/`Sync`) regardless of `R`:
	// the output is only ever moved out of `Poll::Ready`, never stored.
	_marker: PhantomData<fn() -> R>,
}

/// Create a [`Future`] from a poll function that receives a [`Waiter`].
///
/// The waiter is kept alive between polls so its registration in a
/// [`WaiterList`] remains valid until the next poll replaces it.
pub fn wait<F, R>(poll: F) -> impl Future<Output = R>
where
	F: FnMut(&Waiter) -> Poll<R> + Unpin,
{
	WaiterFn {
		poll,
		park: Park::default(),
		_marker: PhantomData,
	}
}

impl<F, R> Future for WaiterFn<F, R>
where
	F: FnMut(&Waiter) -> Poll<R> + Unpin,
{
	type Output = R;

	fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<R> {
		let this = &mut *self;
		let waiter = this.park.hold(cx);
		(this.poll)(waiter)
	}
}

#[cfg(all(test, not(loom)))]
mod tests {
	use super::*;

	/// A waker that records whether it fired.
	#[derive(Default)]
	struct Flag(std::sync::atomic::AtomicBool);

	impl Flag {
		fn woken(&self) -> bool {
			self.0.load(std::sync::atomic::Ordering::SeqCst)
		}
	}

	impl Wake for Flag {
		fn wake(self: Arc<Self>) {
			self.wake_by_ref();
		}

		fn wake_by_ref(self: &Arc<Self>) {
			self.0.store(true, std::sync::atomic::Ordering::SeqCst);
		}
	}

	fn flagged(fan: &Fan) -> (Arc<Flag>, Waiter) {
		let flag = Arc::new(Flag::default());
		let waiter = Waiter::new(Waker::from(flag.clone()));
		fan.register(&waiter);

		// The waiter goes back to the caller: the registration is weak, and dies with it.
		(flag, waiter)
	}

	#[test]
	fn the_waker_fans_out_to_everyone_parked() {
		let fan = Fan::new();
		let (first, _first_waiter) = flagged(&fan);
		let (second, _second_waiter) = flagged(&fan);

		fan.waker().wake();

		assert!(first.woken() && second.woken(), "one wake must reach every waiter");
	}

	#[test]
	fn a_departed_waiter_releases_its_slot() {
		let fan = Fan::new();
		let (gone, waiter) = flagged(&fan);
		drop(waiter);

		let (live, _live_waiter) = flagged(&fan);
		fan.wake();

		assert!(live.woken());
		assert!(!gone.woken(), "a dropped waiter should have no registration left");
	}

	/// Waking while holding the fan's own lock deadlocks the moment a waker resumes its
	/// task inline, because a resumed waiter registers again.
	#[test]
	fn waking_does_not_hold_the_lock() {
		struct Reentrant(Fan);

		impl Wake for Reentrant {
			fn wake(self: Arc<Self>) {
				self.wake_by_ref();
			}

			fn wake_by_ref(self: &Arc<Self>) {
				self.0.register(&Waiter::noop());
			}
		}

		let fan = Fan::new();
		let waiter = Waiter::new(Waker::from(Arc::new(Reentrant(fan.clone()))));
		fan.register(&waiter);

		// On a deadlock this thread never finishes, so the test cannot hang.
		let (tx, rx) = std::sync::mpsc::channel();
		std::thread::spawn({
			let fan = fan.clone();
			move || {
				fan.wake();
				let _ = tx.send(());
			}
		});

		rx.recv_timeout(std::time::Duration::from_secs(5))
			.expect("wake reached a waker while holding the lock, and the wake re-entered it");
	}

	#[test]
	fn a_held_wake_lands_when_the_hold_drops() {
		let fan = Fan::new();
		let (flag, _waiter) = flagged(&fan);

		let hold = fan.hold();
		fan.wake();
		assert!(!flag.woken(), "the wake was delivered while the fan was held");

		drop(hold);
		assert!(flag.woken(), "the held wake never arrived");
	}

	#[test]
	fn only_the_last_hold_out_delivers() {
		let fan = Fan::new();
		let (flag, _waiter) = flagged(&fan);

		let outer = fan.hold();
		let inner = fan.hold();
		fan.wake();

		drop(inner);
		assert!(!flag.woken(), "a hold is still outstanding");

		drop(outer);
		assert!(flag.woken());
	}

	#[test]
	fn a_quiet_hold_wakes_nobody() {
		let fan = Fan::new();
		let (flag, _waiter) = flagged(&fan);

		drop(fan.hold());

		assert!(!flag.woken(), "nothing woke, so nothing was owed");
	}

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
	fn park_reuses_a_drained_waiter_and_retires_a_registered_one() {
		let waker = Waker::noop().clone();
		let cx = Context::from_waker(&waker);
		let mut park = Park::default();
		let mut list = WaiterList::new();

		// Poll 1 parks with a live registration. Hold the Arc so a pointer comparison
		// can't alias a recycled allocation.
		let waiter = park.hold(&cx);
		waiter.register(&mut list);
		let first = waiter.shared().clone();

		// Poll 2 with that registration still live must retire the waiter: reusing it
		// would stack a duplicate entry the list could never reclaim.
		let waiter = park.hold(&cx);
		assert!(!Arc::ptr_eq(&first, waiter.shared()), "a registered waiter was reused");
		waiter.register(&mut list);
		let second = waiter.shared().clone();

		// The wake drains the list, so poll 3 reuses: same task, nothing left to
		// duplicate, and no allocation.
		list.wake();
		let waiter = park.hold(&cx);
		assert!(Arc::ptr_eq(&second, waiter.shared()), "a drained waiter was not reused");
	}

	#[test]
	fn park_retires_a_waiter_for_another_task() {
		struct Nop;
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

	/// The point of projecting: one lock, one list. Parking happens on the list inside
	/// the state, and the fan's waker still reaches it.
	#[test]
	fn a_projected_fan_wakes_the_list_inside_the_state() {
		#[derive(Default)]
		struct State {
			waiters: WaiterList,
			other: WaiterList,
		}

		let state = Lock::new(State::default());
		let fan = Fan::project(&state, |s| &mut s.waiters);

		let flag = Arc::new(Flag::default());
		let waiter = Waiter::new(Waker::from(flag.clone()));

		// Parked directly on the list, as a caller already holding the lock would.
		waiter.register(&mut state.lock().waiters);

		let bystander = Arc::new(Flag::default());
		let bystander_waiter = Waiter::new(Waker::from(bystander.clone()));
		bystander_waiter.register(&mut state.lock().other);

		fan.waker().wake();

		assert!(flag.woken(), "the projected list was not woken");
		assert!(!bystander.woken(), "only the projected list should be woken");
	}

	/// A projected fan is weak, so the state it points at can hold the fan (or its
	/// waker) without leaking. Waking after that state is gone does nothing.
	#[test]
	fn a_projected_fan_outlives_its_state_harmlessly() {
		let state = Lock::new(WaiterList::new());
		let fan = Fan::project(&state, |list| list);

		let flag = Arc::new(Flag::default());
		let waiter = Waiter::new(Waker::from(flag.clone()));
		waiter.register(&mut state.lock());

		drop(state);

		// No panic, and nothing to wake: the list went with the state.
		fan.wake();
		fan.waker().wake();
		assert!(!flag.woken());
	}

	/// Deferral is the same either way round.
	#[test]
	fn a_projected_fan_defers_a_held_wake() {
		let state = Lock::new(WaiterList::new());
		let fan = Fan::project(&state, |list| list);

		let flag = Arc::new(Flag::default());
		let waiter = Waiter::new(Waker::from(flag.clone()));
		waiter.register(&mut state.lock());

		let hold = fan.hold();
		fan.wake();
		assert!(!flag.woken(), "the wake was delivered while the fan was held");

		drop(hold);
		assert!(flag.woken(), "the held wake never arrived");
	}
}
