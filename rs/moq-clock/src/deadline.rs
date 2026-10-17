use std::{
	collections::BTreeMap,
	sync::{Arc, Mutex},
	task::Poll,
	time::Duration,
};

use crate::Instant;

/// A deadline queue for caller-driven code, advanced only by the instants it is handed.
///
/// A state machine polled with `poll(now, waiter)` owns one: each poll calls
/// [`advance`](Self::advance) with `now`, polls its work (which arms and polls
/// [`Deadline`]s), then [`park`](Self::park)s, returning the earliest deadline as
/// the instant it next wants to be polled by. Clones share the queue.
#[derive(Clone)]
pub struct Deadlines(Arc<Mutex<State>>);

struct State {
	now: Instant,
	/// Orders deadlines armed for the same instant by when they were armed.
	next: u64,
	armed: BTreeMap<(Instant, u64), Arc<Mutex<kio::WaiterList>>>,
	/// The owner's poll, woken when a deadline armed outside it becomes the earliest,
	/// so the owner never sleeps past a deadline it did not see.
	owner: kio::WaiterList,
}

impl Deadlines {
	/// A queue at `now`.
	pub fn new(now: Instant) -> Self {
		Self(Arc::new(Mutex::new(State {
			now,
			next: 0,
			armed: BTreeMap::new(),
			owner: kio::WaiterList::new(),
		})))
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
			let mut state = self.0.lock().unwrap();
			if now < state.now {
				// Unlocked first: a poisoned queue would turn every armed deadline's
				// drop into a second panic, aborting the unwind.
				drop(state);
				panic!("time moved backwards");
			}
			state.now = now;
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
	pub fn park(&self, waiter: &kio::Waiter) -> Option<Instant> {
		let mut state = self.0.lock().unwrap();
		waiter.register(&mut state.owner);
		state.armed.first_key_value().map(|((at, _), _)| *at)
	}

	/// The earliest armed deadline, without registering for changes.
	pub fn next(&self) -> Option<Instant> {
		self.0.lock().unwrap().armed.first_key_value().map(|((at, _), _)| *at)
	}

	/// The latest instant the queue was handed.
	pub fn now(&self) -> Instant {
		self.0.lock().unwrap().now
	}
}

/// A queue the simulator keeps in step without keeping it alive.
#[cfg(feature = "sim")]
pub(crate) struct WeakDeadlines(std::sync::Weak<Mutex<State>>);

#[cfg(feature = "sim")]
impl WeakDeadlines {
	pub(crate) fn new(deadlines: &Deadlines) -> Self {
		Self(Arc::downgrade(&deadlines.0))
	}

	pub(crate) fn upgrade(&self) -> Option<Deadlines> {
		self.0.upgrade().map(Deadlines)
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
	waiters: Arc<Mutex<kio::WaiterList>>,
}

impl Deadline {
	/// A disarmed deadline, which never fires until [`set`](Self::set) arms it.
	pub fn new(deadlines: &Deadlines) -> Self {
		Self {
			deadlines: deadlines.clone(),
			key: None,
			at: None,
			waiters: Arc::new(Mutex::new(kio::WaiterList::new())),
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
		let mut state = self.deadlines.0.lock().unwrap();
		if let Some(key) = self.key.take() {
			state.armed.remove(&key);
		}
		self.at = at;
		let Some(at) = at else { return };
		if at <= state.now {
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
	pub fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		// Register under the queue's lock, so an `advance` past `at` either happened
		// first (and we see it) or wakes this registration. A disarmed deadline
		// registers too, so arming it for a reached instant wakes its poller.
		let state = self.deadlines.0.lock().unwrap();
		if self.at.is_some_and(|at| state.now >= at) {
			return Poll::Ready(());
		}
		waiter.register(&mut self.waiters.lock().unwrap());
		Poll::Pending
	}
}

impl kio::Task for Deadline {
	type Output = ();

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
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
	use std::sync::atomic::{AtomicUsize, Ordering};

	use super::*;
	use crate::Manual;

	/// Counts wakes.
	struct Wakes {
		count: AtomicUsize,
	}

	impl std::task::Wake for Wakes {
		fn wake(self: Arc<Self>) {
			self.count.fetch_add(1, Ordering::SeqCst);
		}
	}

	fn counter() -> (Arc<Wakes>, kio::Waiter) {
		let wakes = Arc::new(Wakes {
			count: AtomicUsize::new(0),
		});
		let waiter = kio::Waiter::new(std::task::Waker::from(wakes.clone()));
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
		assert!(deadline.poll(&kio::Waiter::noop()).is_pending());

		clock.advance(Duration::from_secs(3));
		deadlines.advance(clock.now());
		assert!(deadline.poll(&kio::Waiter::noop()).is_ready());
		assert!(
			deadline.poll(&kio::Waiter::noop()).is_ready(),
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
		assert!(disarmed.poll(&kio::Waiter::noop()).is_pending());

		let mut unrepresentable = Deadline::after(&deadlines, Duration::MAX);
		assert_eq!(unrepresentable.deadline(), None);
		assert!(unrepresentable.poll(&kio::Waiter::noop()).is_pending());
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
			deadline.poll(&kio::Waiter::noop()).is_pending(),
			"the old instant is gone"
		);

		clock.advance(Duration::from_secs(4));
		deadlines.advance(clock.now());
		assert!(deadline.poll(&kio::Waiter::noop()).is_ready());

		deadline.set(Some(clock.now() + Duration::from_secs(1)));
		assert!(
			deadline.poll(&kio::Waiter::noop()).is_pending(),
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
		assert!(deadline.poll(&kio::Waiter::noop()).is_ready(), "in the past");
		deadline.set(Some(clock.now()));
		assert!(deadline.poll(&kio::Waiter::noop()).is_ready(), "exactly now");
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
		let waiter = |name| kio::Waiter::new(std::task::Waker::from(Arc::new(Record(order.clone(), name))));
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
		let mut tasks = kio::Tasks::new();
		let mut tick = Deadline::after(&deadlines, PERIOD);
		let ticking = deadlines.clone();
		tasks.push(move |waiter: &kio::Waiter| {
			while tick.poll(waiter).is_ready() {
				tick.set(Some(ticking.now() + PERIOD));
			}
			Poll::Pending
		});

		// `poll(now, waiter)`, minus the protocol.
		let now = std::cell::Cell::new(start);
		let next = std::cell::Cell::new(None);
		let mut owner = std::pin::pin!(kio::wait(|waiter: &kio::Waiter| {
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
}
