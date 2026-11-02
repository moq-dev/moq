use std::{
	cell::{Cell, RefCell},
	collections::BTreeMap,
	marker::PhantomData,
	sync::{
		Arc, Mutex,
		atomic::{AtomicU64, Ordering},
	},
	task::{Context, Poll},
	time::Duration,
};

use crate::clock::Instant;

thread_local! {
	/// The queue [`Deadlines::enter`] made this thread's ambient clock.
	static ENTERED: RefCell<Option<Deadlines>> = const { RefCell::new(None) };
	/// The live [`Entered`] guards on this thread, innermost last.
	static GUARDS: RefCell<Vec<u64>> = const { RefCell::new(Vec::new()) };
	/// The id the next [`Entered`] guard on this thread takes.
	static NEXT_GUARD: Cell<u64> = const { Cell::new(0) };
}

/// A deadline queue for caller-driven code, advanced only by the instants it is handed.
///
/// A state machine polled with `poll(now, waiter)` owns one: each poll calls
/// [`advance`](Self::advance) with `now`, polls its work (which arms and polls
/// [`Deadline`]s), then [`park`](Self::park)s, returning the earliest deadline as
/// the instant it next wants to be polled by. Clones share the queue.
///
/// A runtime that reads the host clock itself, such as an io_uring worker, also
/// [`enter`](Self::enter)s its queue to make it the thread's ambient clock.
#[derive(Clone)]
pub struct Deadlines(Arc<Queue>);

struct Queue {
	/// The latest instant the queue was handed, readable without the lock. Only
	/// written under it, so a reader holding the lock sees the instant it moved to.
	now: AtomicU64,
	state: Mutex<State>,
	/// The clock installed where the queue was made, whose wall time it reads
	/// while entered, so its wall and instants come from one clock.
	#[cfg(feature = "manual")]
	installed: Option<crate::clock::Installed>,
}

struct State {
	/// Orders deadlines armed for the same instant by when they were armed.
	next: u64,
	armed: BTreeMap<(Instant, u64), Arc<Mutex<crate::WaiterList>>>,
	/// The owner's poll, woken when a deadline armed outside it becomes the earliest,
	/// so the owner never sleeps past a deadline it did not see.
	owner: crate::WaiterList,
}

impl Queue {
	fn now(&self) -> Instant {
		Instant::from_nanos(self.now.load(Ordering::Acquire))
	}
}

impl Deadlines {
	/// A queue at `now`.
	pub fn new(now: Instant) -> Self {
		Self(Arc::new(Queue {
			now: AtomicU64::new(now.to_nanos()),
			state: Mutex::new(State {
				next: 0,
				armed: BTreeMap::new(),
				owner: crate::WaiterList::new(),
			}),
			#[cfg(feature = "manual")]
			installed: crate::clock::installed::current(),
		}))
	}

	/// Move to `now`, waking every deadline at or before it, in arming order.
	///
	/// The owner is polling from here until [`Self::park`], which reads every
	/// deadline armed meanwhile, so it drops its registration rather than wake
	/// itself. Taken rather than replaced: a new list's serial would cost the
	/// owner's waiter its records of every other list it is parked on.
	///
	/// # Panics
	///
	/// When `now` is earlier than the queue's current instant.
	pub fn advance(&self, now: Instant) {
		self.advance_to(now, true);
	}

	/// Move to `now` from outside the owner's poll, as the simulator does, leaving
	/// the owner registered.
	#[cfg(feature = "sim")]
	pub(crate) fn sync(&self, now: Instant) {
		self.advance_to(now, false);
	}

	fn advance_to(&self, now: Instant, polling: bool) {
		let due = {
			let mut state = self.0.state.lock().unwrap();
			if now < self.0.now() {
				// Unlocked first: a poisoned queue would turn every armed deadline's
				// drop into a second panic, aborting the unwind.
				drop(state);
				panic!("time moved backwards");
			}
			self.0.now.store(now.to_nanos(), Ordering::Release);
			if polling {
				drop(state.owner.take());
			}
			let mut due = Vec::new();
			while state.armed.first_key_value().is_some_and(|((at, _), _)| *at <= now) {
				due.push(state.armed.pop_first().unwrap().1);
			}
			due
		};
		for waiters in due {
			// Take the registrations before waking: a wake may poll the deadline at once.
			let mut ready = waiters.lock().unwrap().take();
			ready.wake();
		}
	}

	/// The earliest armed deadline, registering `waiter` to be woken when an earlier
	/// one is armed. Ends the poll [`Self::advance`] began: one lock, so a deadline
	/// armed elsewhere is either reported here or wakes the owner.
	pub fn park(&self, waiter: &crate::Waiter) -> Option<Instant> {
		let mut state = self.0.state.lock().unwrap();
		waiter.register(&mut state.owner);
		state.armed.first_key_value().map(|((at, _), _)| *at)
	}

	/// The earliest armed deadline, without registering for changes.
	pub fn next(&self) -> Option<Instant> {
		self.0
			.state
			.lock()
			.unwrap()
			.armed
			.first_key_value()
			.map(|((at, _), _)| *at)
	}

	/// The latest instant the queue was handed.
	pub fn now(&self) -> Instant {
		self.0.now()
	}

	/// Make this queue the calling thread's ambient clock until the guard drops.
	///
	/// While entered, [`now`](crate::clock::now) reads the instant last handed to
	/// [`advance`](Self::advance), and [`sleep`](crate::clock::sleep) and the other ambient
	/// timers arm [`Deadline`]s here. Wall time comes from the clock installed on the
	/// runtime the queue was made in, if any, and otherwise from the clock the thread
	/// would read. For a runtime that advances the queue from its own reading of the
	/// clock, so nothing it runs reads that clock again.
	pub fn enter(&self) -> Entered {
		let previous = ENTERED.with(|entered| entered.replace(Some(self.clone())));
		let id = NEXT_GUARD.with(|next| next.replace(next.get() + 1));
		GUARDS.with(|guards| guards.borrow_mut().push(id));
		Entered {
			previous,
			id,
			thread: PhantomData,
		}
	}
}

/// Keeps a [`Deadlines`] queue the thread's ambient clock; dropping it restores the one before.
///
/// Guards must drop in reverse order of entering. Dropping one while a later
/// guard is still live restores the clock from before it, retires the later
/// guards (their drops do nothing), and panics.
#[must_use = "the queue stops being the ambient clock when this drops"]
pub struct Entered {
	previous: Option<Deadlines>,
	/// This guard's entry in the thread's live guards.
	id: u64,
	/// Restores the thread it was entered on.
	thread: PhantomData<*const ()>,
}

impl Drop for Entered {
	fn drop(&mut self) {
		let retired_later = GUARDS.with(|guards| {
			let mut guards = guards.borrow_mut();
			// Gone already: an outer guard dropped out of order retired this one.
			let index = guards.iter().position(|id| *id == self.id)?;
			let later = guards.len() - index - 1;
			guards.truncate(index);
			Some(later)
		});
		let Some(later) = retired_later else { return };
		// The state is repaired before any panic, so a caught panic leaves no
		// retired queue as the ambient clock.
		let previous = self.previous.take();
		ENTERED.with(|entered| *entered.borrow_mut() = previous);
		if later > 0 && !std::thread::panicking() {
			panic!("an Entered guard dropped while a later one is still live");
		}
	}
}

impl std::fmt::Debug for Entered {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Entered").finish_non_exhaustive()
	}
}

/// The instant of the queue entered on this thread, if any.
pub(crate) fn now() -> Option<Instant> {
	ENTERED.with(|entered| entered.borrow().as_ref().map(Deadlines::now))
}

/// The installed clock behind the queue entered on this thread, if any.
#[cfg(feature = "manual")]
pub(crate) fn installed() -> Option<crate::clock::Installed> {
	ENTERED.with(|entered| entered.borrow().as_ref().and_then(|queue| queue.0.installed.clone()))
}

/// The queue entered on this thread, if any.
pub(crate) fn entered() -> Option<Deadlines> {
	ENTERED.with(|entered| entered.borrow().clone())
}

/// A queue the simulator keeps in step without keeping it alive.
#[cfg(feature = "sim")]
pub(crate) struct WeakDeadlines(std::sync::Weak<Queue>);

#[cfg(feature = "sim")]
impl WeakDeadlines {
	pub(crate) fn new(deadlines: &Deadlines) -> Self {
		Self(Arc::downgrade(&deadlines.0))
	}

	pub(crate) fn upgrade(&self) -> Option<Deadlines> {
		self.0.upgrade().map(Deadlines)
	}
}

/// The [`Deadline`] behind an ambient [`Sleep`](crate::clock::Sleep) on an entered queue,
/// with the waiter its polls register.
pub(crate) struct Timer {
	deadline: Deadline,
	park: crate::Park,
}

impl Timer {
	pub(crate) fn new(deadlines: &Deadlines, at: Option<Instant>) -> Self {
		let mut deadline = Deadline::new(deadlines);
		deadline.set(at);
		Self {
			deadline,
			park: crate::Park::default(),
		}
	}

	pub(crate) fn set(&mut self, at: Option<Instant>) {
		self.deadline.set(at);
	}

	pub(crate) fn poll(&mut self, cx: &mut Context<'_>) -> Poll<()> {
		let waiter = self.park.hold(cx);
		self.deadline.poll(waiter)
	}
}

/// A re-armable deadline on [`Deadlines`].
///
/// Arm it, poll it from a `poll_*` function, re-arm or disarm as the deadline
/// moves. Re-setting the instant it already holds does nothing, so a poll loop can
/// recompute its deadline every turn without restarting the countdown. Dropping it
/// disarms it.
pub struct Deadline {
	deadlines: Deadlines,
	key: Option<(Instant, u64)>,
	at: Option<Instant>,
	waiters: Arc<Mutex<crate::WaiterList>>,
}

impl Deadline {
	/// A disarmed deadline, which never fires until [`set`](Self::set) arms it.
	pub fn new(deadlines: &Deadlines) -> Self {
		Self {
			deadlines: deadlines.clone(),
			key: None,
			at: None,
			waiters: Arc::new(Mutex::new(crate::WaiterList::new())),
		}
	}

	/// A deadline armed for `duration` past the queue's [`now`](Deadlines::now).
	///
	/// A duration the clock cannot represent (e.g. [`Duration::MAX`]) leaves the
	/// deadline disarmed, so it never fires rather than panicking on the overflow.
	pub fn after(deadlines: &Deadlines, duration: Duration) -> Self {
		let mut deadline = Self::new(deadlines);
		deadline.set(deadlines.now().checked_add(duration));
		deadline
	}

	/// Arm, re-arm, or disarm (`None`) the deadline.
	///
	/// Re-arming an elapsed deadline for a later instant makes it pend again;
	/// arming one for an instant already reached makes it fire at once.
	pub fn set(&mut self, at: Option<Instant>) {
		if self.at == at {
			return;
		}
		let mut state = self.deadlines.0.state.lock().unwrap();
		if let Some(key) = self.key.take() {
			state.armed.remove(&key);
		}
		self.at = at;
		let Some(at) = at else { return };
		if at <= self.deadlines.0.now() {
			// Already reached: wake whoever is parked on it, and the owner in case
			// nobody polled it yet, so it fires now.
			let mut owner = state.owner.take();
			drop(state);
			let mut ready = self.waiters.lock().unwrap().take();
			ready.wake();
			owner.wake();
			return;
		}
		let key = (at, state.next);
		state.next += 1;
		self.key = Some(key);
		state.armed.insert(key, self.waiters.clone());
		if state.armed.first_key_value().is_some_and(|(first, _)| *first == key) {
			let mut owner = state.owner.take();
			drop(state);
			owner.wake();
		}
	}

	/// The instant this fires at, or `None` while disarmed.
	pub fn deadline(&self) -> Option<Instant> {
		self.at
	}

	/// `Ready` once the queue reaches the armed instant, registering `waiter`
	/// otherwise. Fused until re-armed; a disarmed deadline never fires.
	pub fn poll(&mut self, waiter: &crate::Waiter) -> Poll<()> {
		// Register under the queue's lock, so an `advance` past `at` either happened
		// first (and we see it) or wakes this registration. A disarmed deadline
		// registers too, so arming it for a reached instant wakes its poller.
		let _state = self.deadlines.0.state.lock().unwrap();
		if self.at.is_some_and(|at| self.deadlines.0.now() >= at) {
			return Poll::Ready(());
		}
		waiter.register(&mut self.waiters.lock().unwrap());
		Poll::Pending
	}
}

impl crate::Task for Deadline {
	type Output = ();

	fn poll(&mut self, waiter: &crate::Waiter) -> Poll<()> {
		self.poll(waiter)
	}
}

impl Drop for Deadline {
	fn drop(&mut self) {
		self.set(None);
	}
}

impl std::fmt::Debug for Deadline {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Deadline").field("at", &self.at).finish()
	}
}

#[cfg(test)]
mod tests {
	use std::future::Future;
	use std::sync::atomic::{AtomicUsize, Ordering};

	use super::*;
	use crate::clock::Manual;

	/// Counts wakes.
	struct Wakes {
		count: AtomicUsize,
	}

	impl std::task::Wake for Wakes {
		fn wake(self: Arc<Self>) {
			self.count.fetch_add(1, Ordering::SeqCst);
		}
	}

	fn counter() -> (Arc<Wakes>, crate::Waiter) {
		let wakes = Arc::new(Wakes {
			count: AtomicUsize::new(0),
		});
		let waiter = crate::Waiter::new(std::task::Waker::from(wakes.clone()));
		(wakes, waiter)
	}

	#[test]
	fn fires_only_on_supplied_time() {
		let mut clock = Manual::new();
		let deadlines = Deadlines::new(clock.now());
		let at = clock.now() + Duration::from_secs(3);
		let mut deadline = Deadline::new(&deadlines);
		deadline.set(Some(at));
		assert_eq!(deadlines.next(), Some(at));
		assert!(deadline.poll(&crate::Waiter::noop()).is_pending());

		clock.advance(Duration::from_secs(3));
		deadlines.advance(clock.now());
		assert!(deadline.poll(&crate::Waiter::noop()).is_ready());
		assert!(
			deadline.poll(&crate::Waiter::noop()).is_ready(),
			"an elapsed deadline stays fused"
		);
		assert_eq!(deadlines.next(), None);
	}

	#[test]
	fn cancellation() {
		let mut clock = Manual::new();
		let deadlines = Deadlines::new(clock.now());

		let mut disarmed = Deadline::after(&deadlines, Duration::from_secs(1));
		disarmed.set(None);
		let dropped = Deadline::after(&deadlines, Duration::from_secs(1));
		drop(dropped);
		assert_eq!(deadlines.next(), None, "neither is armed");

		clock.advance(Duration::from_secs(3600));
		deadlines.advance(clock.now());
		assert!(disarmed.poll(&crate::Waiter::noop()).is_pending());

		let mut unrepresentable = Deadline::after(&deadlines, Duration::MAX);
		assert_eq!(unrepresentable.deadline(), None);
		assert!(unrepresentable.poll(&crate::Waiter::noop()).is_pending());
	}

	#[test]
	fn rearming() {
		let mut clock = Manual::new();
		let deadlines = Deadlines::new(clock.now());
		let mut deadline = Deadline::after(&deadlines, Duration::from_secs(1));

		deadline.set(Some(clock.now() + Duration::from_secs(5)));
		assert_eq!(
			deadlines.next(),
			Some(clock.now() + Duration::from_secs(5)),
			"re-armed later"
		);
		clock.advance(Duration::from_secs(1));
		deadlines.advance(clock.now());
		assert!(
			deadline.poll(&crate::Waiter::noop()).is_pending(),
			"the old instant is gone"
		);

		clock.advance(Duration::from_secs(4));
		deadlines.advance(clock.now());
		assert!(deadline.poll(&crate::Waiter::noop()).is_ready());

		deadline.set(Some(clock.now() + Duration::from_secs(1)));
		assert!(
			deadline.poll(&crate::Waiter::noop()).is_pending(),
			"an elapsed deadline re-arms"
		);
	}

	#[test]
	fn an_expired_deadline_fires_at_once() {
		let mut clock = Manual::new();
		clock.advance(Duration::from_secs(10));
		let deadlines = Deadlines::new(clock.now());
		let mut deadline = Deadline::new(&deadlines);

		deadline.set(Some(clock.now() - Duration::from_secs(1)));
		assert!(deadline.poll(&crate::Waiter::noop()).is_ready(), "in the past");
		deadline.set(Some(clock.now()));
		assert!(deadline.poll(&crate::Waiter::noop()).is_ready(), "exactly now");
		assert_eq!(deadlines.next(), None, "neither was queued");
	}

	/// A parked deadline re-armed to an instant already reached wakes its waiter,
	/// rather than leaving it parked on the instant it no longer holds.
	#[test]
	fn rearming_to_a_reached_instant_wakes_the_waiter() {
		let mut clock = Manual::new();
		let deadlines = Deadlines::new(clock.now());
		let mut deadline = Deadline::after(&deadlines, Duration::from_secs(5));
		let (wakes, waiter) = counter();
		assert!(deadline.poll(&waiter).is_pending());

		clock.advance(Duration::from_secs(1));
		deadlines.advance(clock.now());
		deadline.set(Some(clock.now()));
		assert_eq!(wakes.count.load(Ordering::SeqCst), 1, "the parked waiter was not woken");
		assert!(deadline.poll(&waiter).is_ready());
	}

	#[test]
	fn arming_a_disarmed_deadline_to_a_reached_instant_wakes_the_waiter() {
		let clock = Manual::new();
		let deadlines = Deadlines::new(clock.now());
		let mut deadline = Deadline::new(&deadlines);
		let (wakes, waiter) = counter();
		assert!(deadline.poll(&waiter).is_pending());

		deadline.set(Some(clock.now()));
		assert_eq!(wakes.count.load(Ordering::SeqCst), 1, "the parked waiter was not woken");
		assert!(deadline.poll(&waiter).is_ready());
	}

	#[test]
	fn arming_an_unpolled_deadline_to_a_reached_instant_wakes_the_owner() {
		let clock = Manual::new();
		let deadlines = Deadlines::new(clock.now());
		let (wakes, owner) = counter();
		deadlines.advance(clock.now());
		assert_eq!(deadlines.park(&owner), None);

		let mut deadline = Deadline::new(&deadlines);
		deadline.set(Some(clock.now()));
		assert_eq!(wakes.count.load(Ordering::SeqCst), 1, "the parked owner was not woken");
	}

	#[test]
	fn equal_deadlines_fire_in_arming_order() {
		let mut clock = Manual::new();
		let deadlines = Deadlines::new(clock.now());
		let at = clock.now() + Duration::from_secs(1);

		// Created in one order, armed in the other.
		let order = Arc::new(Mutex::new(Vec::new()));
		let mut first = Deadline::new(&deadlines);
		let mut second = Deadline::new(&deadlines);
		second.set(Some(at));
		first.set(Some(at));

		struct Record(Arc<Mutex<Vec<&'static str>>>, &'static str);
		impl std::task::Wake for Record {
			fn wake(self: Arc<Self>) {
				self.0.lock().unwrap().push(self.1);
			}
		}
		// Kept alive: a list holds its waiters weakly.
		let waiter = |name| crate::Waiter::new(std::task::Waker::from(Arc::new(Record(order.clone(), name))));
		let (first_waiter, second_waiter) = (waiter("first"), waiter("second"));
		assert!(first.poll(&first_waiter).is_pending());
		assert!(second.poll(&second_waiter).is_pending());

		clock.advance(Duration::from_secs(1));
		deadlines.advance(clock.now());
		assert_eq!(*order.lock().unwrap(), ["second", "first"]);
	}

	#[test]
	fn advance_wakes_a_parked_waiter() {
		let mut clock = Manual::new();
		let deadlines = Deadlines::new(clock.now());
		let mut deadline = Deadline::after(&deadlines, Duration::from_secs(1));
		let (wakes, waiter) = counter();

		assert!(deadline.poll(&waiter).is_pending());
		clock.advance(Duration::from_secs(1));
		deadlines.advance(clock.now());
		assert_eq!(wakes.count.load(Ordering::SeqCst), 1, "the deadline wake was lost");
		assert!(deadline.poll(&waiter).is_ready());
	}

	/// A deadline that fires and re-arms inside the owner's poll is that poll's work:
	/// neither its wake nor its re-arm asks for another. One armed from outside the
	/// poll still wakes the owner.
	#[test]
	fn a_deadline_costs_the_owner_one_poll() {
		const PERIOD: Duration = Duration::from_secs(1);
		let start = Instant::ZERO;
		let deadlines = Deadlines::new(start);

		// A child that re-arms its deadline every time it fires, like a periodic probe.
		let mut tasks = crate::Tasks::new();
		let mut tick = Deadline::after(&deadlines, PERIOD);
		let ticking = deadlines.clone();
		tasks.push(move |waiter: &crate::Waiter| {
			while tick.poll(waiter).is_ready() {
				tick.set(Some(ticking.now() + PERIOD));
			}
			Poll::Pending
		});

		// `poll(now, waiter)`, minus the protocol.
		let now = std::cell::Cell::new(start);
		let next = std::cell::Cell::new(None);
		let mut owner = std::pin::pin!(crate::wait(|waiter: &crate::Waiter| {
			waiter.batch(|| {
				deadlines.advance(now.get());
				let _ = tasks.poll(waiter);
				next.set(deadlines.park(waiter));
			});
			Poll::<()>::Pending
		}));
		let (wakes, _) = counter();
		let waker = std::task::Waker::from(wakes.clone());
		let mut cx = std::task::Context::from_waker(&waker);

		assert!(owner.as_mut().poll(&mut cx).is_pending());
		for ticks in 1..=3 {
			now.set(start + PERIOD * ticks);
			assert!(owner.as_mut().poll(&mut cx).is_pending());
			assert_eq!(next.get(), Some(now.get() + PERIOD), "the deadline re-armed");
			assert_eq!(wakes.count.load(Ordering::SeqCst), 0, "tick {ticks} woke its own poll");
		}

		let _sooner = Deadline::after(&deadlines, PERIOD / 2);
		assert_eq!(
			wakes.count.load(Ordering::SeqCst),
			1,
			"an earlier deadline armed outside the poll must wake the owner"
		);
	}

	#[test]
	#[should_panic(expected = "time moved backwards")]
	fn time_cannot_move_backwards() {
		let deadlines = Deadlines::new(Instant::ZERO + Duration::from_secs(1));
		deadlines.advance(Instant::ZERO);
	}

	/// The panic leaves the queue usable, so an armed deadline dropped while
	/// unwinding does not panic a second time.
	#[test]
	fn moving_backwards_leaves_the_queue_unpoisoned() {
		let deadlines = Deadlines::new(Instant::ZERO + Duration::from_secs(1));
		let deadline = Deadline::after(&deadlines, Duration::from_secs(1));
		let moved = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| deadlines.advance(Instant::ZERO)));
		assert!(moved.is_err());
		drop(deadline);
		assert_eq!(deadlines.next(), None);
	}

	/// An entered queue is the thread's ambient clock: `now` reads the instant it was
	/// handed, and the ambient timers arm on it and fire only as it advances.
	#[test]
	fn an_entered_queue_is_the_ambient_clock() {
		let start = Instant::ZERO + Duration::from_secs(10);
		let deadlines = Deadlines::new(start);
		let entered = deadlines.enter();
		assert_eq!(crate::clock::now(), start);

		let mut sleep = std::pin::pin!(crate::clock::sleep(Duration::from_secs(1)));
		assert_eq!(deadlines.next(), Some(start + Duration::from_secs(1)));
		let (wakes, waiter) = counter();
		let waker = waiter.waker().clone();
		let mut cx = Context::from_waker(&waker);
		assert!(sleep.as_mut().poll(&mut cx).is_pending());

		deadlines.advance(start + Duration::from_millis(999));
		assert_eq!(crate::clock::now(), start + Duration::from_millis(999));
		assert_eq!(wakes.count.load(Ordering::SeqCst), 0);
		deadlines.advance(start + Duration::from_secs(1));
		assert_eq!(wakes.count.load(Ordering::SeqCst), 1, "the sleep was not woken");
		assert!(sleep.as_mut().poll(&mut cx).is_ready());

		drop(entered);
		assert!(
			crate::clock::now() > start + Duration::from_secs(1),
			"the host clock is back"
		);
	}

	/// A sleep that never fires leaves nothing armed for the queue's owner to park on.
	#[test]
	fn a_never_firing_sleep_arms_nothing() {
		let deadlines = Deadlines::new(Instant::ZERO);
		let _entered = deadlines.enter();
		let mut sleep = crate::clock::sleep(Duration::MAX);
		assert_eq!(deadlines.next(), None);
		sleep.reset(Instant::ZERO + Duration::from_secs(1));
		assert_eq!(deadlines.next(), Some(Instant::ZERO + Duration::from_secs(1)));
	}

	/// Entering nests, and dropping a guard restores the queue entered before it.
	#[test]
	fn entering_nests() {
		let outer = Deadlines::new(Instant::ZERO + Duration::from_secs(1));
		let inner = Deadlines::new(Instant::ZERO + Duration::from_secs(2));
		let _outer = outer.enter();
		{
			let _inner = inner.enter();
			assert_eq!(crate::clock::now(), Instant::ZERO + Duration::from_secs(2));
		}
		assert_eq!(crate::clock::now(), Instant::ZERO + Duration::from_secs(1));
		std::thread::spawn(|| {
			assert!(
				crate::clock::now() > Instant::ZERO + Duration::from_secs(2),
				"other threads keep their own clock"
			)
		})
		.join()
		.unwrap();
	}

	/// Dropping an outer guard while an inner one is live refuses, rather than let
	/// the inner guard's drop bring the outer queue back.
	#[test]
	#[should_panic(expected = "an Entered guard dropped while a later one is still live")]
	fn entered_guards_drop_in_order() {
		let outer = Deadlines::new(Instant::ZERO);
		let inner = Deadlines::new(Instant::ZERO);
		let outer = outer.enter();
		let _inner = inner.enter();
		drop(outer);
	}

	/// A caught out-of-order drop leaves the clock from before the outer guard,
	/// and the retired inner guard's drop does not bring a queue back.
	#[test]
	fn a_caught_out_of_order_drop_restores_the_clock() {
		let outer = Deadlines::new(Instant::ZERO + Duration::from_secs(1));
		let inner = Deadlines::new(Instant::ZERO + Duration::from_secs(2));
		let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
			let outer = outer.enter();
			let _inner = inner.enter();
			drop(outer);
		}));
		assert!(result.is_err(), "the out-of-order drop did not panic");
		assert!(super::entered().is_none(), "a dropped queue is still the ambient clock");
		assert!(
			crate::clock::now() > Instant::ZERO + Duration::from_secs(2),
			"the host clock is back"
		);
	}
}
