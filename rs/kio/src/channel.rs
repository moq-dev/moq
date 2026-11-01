//! The cell every [`Producer`](crate::Producer), [`Consumer`](crate::Consumer) and
//! [`Shared`](crate::Shared) handle points at.
//!
//! The value sits under its own mutex. Beside it, a version counts published writes and
//! carries the closed flag, so the closed checks are a load. The waiter lists sit under a
//! second lock, so parking a waiter never contends with a reader of the value.
//!
//! Splitting the locks is what lets a waiter park without the value's lock, so the check
//! and the registration no longer happen atomically. The version closes that gap: every
//! publish bumps it, under the value lock, before draining a list under the waiter lock.
//! A poll reads the version alongside the value it judged, registers, and reads it again
//! under the waiter lock. Either the drain came after the registration, and wakes it, or
//! it came before, and the second read sees its bump.
//!
//! A waiter kept across polls is usually still on the value list, so re-registering it
//! would take the waiter lock only to find it there. The list's tag, which changes on every
//! drain, is mirrored beside the version: a waiter that recorded the current tag, polled at
//! an unchanged version, skips the lock. No publish since means none it could miss, and the
//! next one's drain takes the entry the tag proves is there.

use std::{
	fmt,
	ops::{Deref, DerefMut},
	sync::{
		Mutex, MutexGuard, PoisonError,
		atomic::{AtomicU64, Ordering},
	},
	task::Poll,
};

use crate::waiter::{Waiter, WaiterList};

/// Set in [`Channel::version`] once the channel closes.
const CLOSED: u64 = 1;

/// What a publish adds to [`Channel::version`], leaving [`CLOSED`] alone.
const STEP: u64 = 2;

pub(crate) struct Channel<T> {
	/// Locked by reads and writes of the value only, never to park.
	value: Mutex<T>,
	/// [`STEP`] per publish, plus [`CLOSED`] once closed. Written only under `value`'s
	/// lock, so a holder of that lock reads the version matching the value it sees.
	version: AtomicU64,
	/// The value list's [tag](WaiterList::tag), stored under the waiter lock whenever it
	/// changes, so a poll can tell its waiter is still parked without that lock.
	value_tag: AtomicU64,
	waiters: Mutex<Waiters>,
}

// SAFETY: every field but `value` is `Send + Sync` for any `T`, and `Mutex<T>` is both
// when `T: Send`, so this is exactly the derived bound. Stating it here, like `Lock` does,
// keeps recursive channel graphs from expanding the same proof at every level.
unsafe impl<T: Send> Send for Channel<T> {}

// SAFETY: as above; `Mutex<T>` is `Sync` when `T: Send`.
unsafe impl<T: Send> Sync for Channel<T> {}

/// Waiters split by what they're waiting on, so an event only wakes the
/// waiters that care about it. The big win is per-modification writes (the hot
/// path) waking only `value`, leaving the long-lived `closed` and `consumer`
/// waiters untouched.
#[derive(Default)]
pub(crate) struct Waiters {
	/// Value changes (`poll`/`wait`). Woken on every modification.
	pub value: WaiterList,
	/// Closure (`closed`). Woken only when the channel closes.
	pub closed: WaiterList,
	/// Consumer-count changes (`wanted_changed`), in either direction.
	pub consumer: WaiterList,
}

impl<T: Default> Default for Channel<T> {
	fn default() -> Self {
		Self::new(T::default())
	}
}

impl<T> Channel<T> {
	pub fn new(value: T) -> Self {
		Self {
			value: Mutex::new(value),
			version: AtomicU64::new(0),
			value_tag: AtomicU64::new(0),
			waiters: Mutex::default(),
		}
	}

	/// Lock the value for reading.
	///
	/// Poisoning is ignored: a panic while the lock is held must not cascade into every
	/// other handle sharing this state, or into a double panic (and an abort) from a
	/// `Drop` that locks it.
	pub fn read(&self) -> Ref<'_, T> {
		Ref {
			value: self.value.lock().unwrap_or_else(PoisonError::into_inner),
			channel: self,
		}
	}

	/// Lock the waiter lists. Nothing takes the value's lock while holding this one, so a
	/// holder of the value's lock may take it.
	pub fn waiters(&self) -> MutexGuard<'_, Waiters> {
		self.waiters.lock().unwrap_or_else(PoisonError::into_inner)
	}

	pub fn version(&self) -> u64 {
		self.version.load(Ordering::Acquire)
	}

	pub fn is_closed(&self) -> bool {
		self.version() & CLOSED != 0
	}

	/// Run a read-only predicate under the value lock, registering `waiter` for the next
	/// change while it is pending, or handing back the guard once the channel closed.
	#[cfg_attr(feature = "sim", track_caller)]
	pub fn poll<F, R>(&self, waiter: &Waiter, f: F) -> Poll<Result<R, Ref<'_, T>>>
	where
		F: FnOnce(&Ref<'_, T>) -> Poll<R>,
	{
		let guard = self.read();
		if let Poll::Ready(res) = f(&guard) {
			return crate::coop::ready(Ok(res));
		}

		// Read under the lock, so it is the version of the value `f` just judged.
		let seen = self.version();
		if seen & CLOSED != 0 {
			return crate::coop::ready(Err(guard));
		}

		drop(guard);
		self.park_value(waiter, seen);
		Poll::Pending
	}

	/// Register `waiter` for the next publish after `seen`, the version a pending predicate
	/// was judged at, outside the value lock.
	///
	/// A publish that landed in between already drained the list without this waiter in it,
	/// so wake it now: its task polls again and judges the new value. `f` was `FnOnce`, so
	/// re-running it here is not an option.
	pub fn park_value(&self, waiter: &Waiter, seen: u64) {
		// Still on the list, and nothing published since `f` judged the value.
		if waiter.holds(self.value_tag.load(Ordering::Acquire)) && self.version() == seen {
			return;
		}

		let changed = {
			let mut waiters = self.waiters();
			waiter.register(&mut waiters.value);
			self.sync_tag(&waiters);
			self.version() != seen
		};
		if changed {
			waiter.waker().wake_by_ref();
		}
	}

	/// Mirror the value list's tag, after anything that may have changed it.
	fn sync_tag(&self, waiters: &Waiters) {
		self.value_tag.store(waiters.value.tag(), Ordering::Release);
	}

	/// Poll for closure, registering `waiter` while open.
	#[cfg_attr(feature = "sim", track_caller)]
	pub fn poll_closed(&self, waiter: &Waiter) -> Poll<()> {
		if self.is_closed() {
			return crate::coop::ready(());
		}

		let mut waiters = self.waiters();
		waiter.register(&mut waiters.closed);
		// Closure drains this list after setting the flag, so if the drain came first
		// this sees the flag.
		if self.is_closed() {
			return crate::coop::ready(());
		}

		Poll::Pending
	}

	/// Count a write, closing the channel too when `close`. Call it holding the value's
	/// lock, on an open channel, before [`wake`](Self::wake).
	fn publish(&self, close: bool) {
		self.version.fetch_add(STEP | close as u64, Ordering::Release);
	}

	/// Close the channel, from a holder of the value's lock. Caller wakes afterwards.
	pub fn close(&self, _value: &Ref<'_, T>) {
		self.publish(true);
	}

	/// Wake the value waiters after a publish, and every waiter when it closed the channel.
	/// Call it once the value's lock is released.
	pub fn wake(&self, closed: bool) {
		let mut lists = {
			let mut waiters = self.waiters();
			if !closed && waiters.value.is_empty() {
				// A write nobody is parked on wakes nobody, so skip moving the list out.
				// Drop any dead entries in place, or every later write would rescan them.
				waiters.value.clear();
				self.sync_tag(&waiters);
				return;
			}

			// A plain modification touches only the value waiters. Closure also resolves
			// the closed and consumer-count waiters.
			let value = waiters.value.take();
			self.sync_tag(&waiters);
			let rest = closed.then(|| [waiters.closed.take(), waiters.consumer.take()]);
			(value, rest)
		};

		lists.0.wake();
		if let Some(rest) = &mut lists.1 {
			for list in rest {
				list.wake();
			}
		}
	}

	/// Wake the consumer-count waiters, after the count moved.
	pub fn wake_consumer(&self) {
		let mut list = self.waiters().consumer.take();
		list.wake();
	}
}

impl<T: fmt::Debug> fmt::Debug for Channel<T> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		let mut debug = f.debug_struct("Channel");
		match self.value.try_lock() {
			Ok(value) => debug.field("value", &*value),
			Err(_) => debug.field("value", &"<locked>"),
		};
		debug.field("closed", &self.is_closed()).finish()
	}
}

/// A mutable guard over the shared state.
///
/// Derefs to `T` for direct access. Automatically notifies all waiting consumers
/// when dropped if the state was accessed mutably.
pub struct Mut<'a, T> {
	// An option so it can drop, releasing the lock, before the wake.
	value: Option<MutexGuard<'a, T>>,
	channel: &'a Channel<T>,
	modified: bool,
	closing: bool,
}

impl<'a, T> Mut<'a, T> {
	pub(crate) fn new(guard: Ref<'a, T>) -> Self {
		Self {
			value: Some(guard.value),
			channel: guard.channel,
			modified: false,
			closing: false,
		}
	}

	/// NOTE: This takes self so it's impossible to be in a closed state.
	pub fn close(mut self) {
		// Every Mut is made on an open channel, and nothing closes it while the lock is held.
		self.closing = true;
		self.modified = true;
	}
}

impl<T: fmt::Debug> fmt::Debug for Mut<'_, T> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("Mut")
			.field("value", &**self)
			.field("modified", &self.modified)
			.finish()
	}
}

impl<T> Deref for Mut<'_, T> {
	type Target = T;

	fn deref(&self) -> &Self::Target {
		self.value.as_ref().unwrap()
	}
}

impl<T> DerefMut for Mut<'_, T> {
	fn deref_mut(&mut self) -> &mut Self::Target {
		// If we use the &mut then notify on Drop.
		self.modified = true;
		self.value.as_mut().unwrap()
	}
}

impl<T> Drop for Mut<'_, T> {
	fn drop(&mut self) {
		let value = self.value.take();
		if !self.modified {
			return;
		}

		self.channel.publish(self.closing);
		// Release the value's lock before waking, so a woken reader doesn't block on it.
		drop(value);
		self.channel.wake(self.closing);
	}
}

/// A read-only guard over the shared state.
///
/// Derefs to `T` for direct access. Does not notify consumers when dropped.
pub struct Ref<'a, T> {
	value: MutexGuard<'a, T>,
	channel: &'a Channel<T>,
}

impl<T> Ref<'_, T> {
	/// Returns `true` if the channel has been closed.
	pub fn is_closed(&self) -> bool {
		self.channel.is_closed()
	}
}

impl<T> Deref for Ref<'_, T> {
	type Target = T;

	fn deref(&self) -> &Self::Target {
		&self.value
	}
}

#[cfg(test)]
mod tests {
	use std::{
		sync::{
			Arc,
			atomic::{AtomicUsize, Ordering},
		},
		task::{Wake, Waker},
		time::Duration,
	};

	use super::*;
	use crate::Producer;

	struct Count(AtomicUsize);

	impl Wake for Count {
		fn wake(self: Arc<Self>) {
			self.0.fetch_add(1, Ordering::SeqCst);
		}
	}

	fn counting() -> (Arc<Count>, Waiter) {
		let count = Arc::new(Count(AtomicUsize::new(0)));
		(count.clone(), Waiter::new(Waker::from(count)))
	}

	fn woken(count: &Count) -> usize {
		count.0.load(Ordering::SeqCst)
	}

	fn write(channel: &Channel<u32>, value: u32) {
		*Mut::new(channel.read()) = value;
	}

	fn pending(_: &Ref<'_, u32>) -> Poll<()> {
		Poll::Pending
	}

	/// A waiter still on the list re-polls without registering again, and the next
	/// publish wakes it all the same.
	#[test]
	fn a_parked_waiter_skips_registering_and_still_wakes() {
		let channel = Channel::new(0u32);
		let (count, waiter) = counting();

		assert!(channel.poll(&waiter, pending).is_pending());
		let tag = channel.value_tag.load(Ordering::Relaxed);
		assert!(waiter.holds(tag), "the first poll registers");

		assert!(channel.poll(&waiter, pending).is_pending());
		assert_eq!(channel.waiters().value.tag(), tag, "re-polling drained nothing");

		write(&channel, 1);
		assert_eq!(woken(&count), 1);
		assert!(
			!waiter.holds(channel.value_tag.load(Ordering::Relaxed)),
			"the drain moved the tag"
		);

		assert!(channel.poll(&waiter, pending).is_pending());
		write(&channel, 2);
		assert_eq!(woken(&count), 2, "the re-registration was lost");
	}

	/// A publish that lands after the predicate judged the value, but before the waiter
	/// parks, drains a list the waiter is not on yet. Parking has to catch it.
	#[test]
	fn a_publish_before_parking_wakes_the_waiter() {
		for parked in [false, true] {
			let channel = Channel::new(0u32);
			let (count, waiter) = counting();
			if parked {
				// On the list already, so the stale tag is what sends it to the lock.
				assert!(channel.poll(&waiter, pending).is_pending());
				write(&channel, 1);
				assert!(channel.poll(&waiter, pending).is_pending());
			}

			let seen = channel.version();
			let before = woken(&count);
			write(&channel, 5);
			// Parking wakes it either way. A parked waiter was also woken by the drain, so it
			// sees one spurious wake.
			channel.park_value(&waiter, seen);
			assert_eq!(
				woken(&count),
				before + 1 + usize::from(parked),
				"a publish between judging and parking was lost (parked: {parked})"
			);
		}
	}

	/// Each publish waits for every reader to see the last, so the reader that saw it last
	/// is parking again just as the next lands, the window a lost wake hangs in. Readers
	/// keep their waiter across polls, as a task's [`Park`](crate::Park) does.
	#[test]
	fn racing_publishes_and_closes_wake_every_waiter() {
		const READERS: usize = 4;
		const STEPS: u32 = 20_000;

		struct Unpark(std::thread::Thread);
		impl Wake for Unpark {
			fn wake(self: Arc<Self>) {
				self.0.unpark();
			}
		}

		/// Spin until `done` holds, failing once a wake was evidently lost.
		fn until(done: impl Fn() -> bool, what: &str) {
			let deadline = std::time::Instant::now() + Duration::from_secs(20);
			while !done() {
				assert!(std::time::Instant::now() < deadline, "{what} was never woken");
				std::thread::yield_now();
			}
		}

		let producer = Producer::new(0u32);
		let seen = Arc::new(AtomicUsize::new(0));
		let closed = Arc::new(AtomicUsize::new(0));
		for _ in 0..READERS {
			let consumer = producer.consume();
			let (seen, closed) = (seen.clone(), closed.clone());
			std::thread::spawn(move || {
				let waiter = Waiter::new(Waker::from(Arc::new(Unpark(std::thread::current()))));
				for target in 1..=STEPS {
					let reached = |v: &Ref<'_, u32>| if **v >= target { Poll::Ready(()) } else { Poll::Pending };
					while consumer.poll(&waiter, reached).is_pending() {
						std::thread::park();
					}
					seen.fetch_add(1, Ordering::SeqCst);
				}
				while consumer.poll_closed(&waiter).is_pending() {
					std::thread::park();
				}
				closed.fetch_add(1, Ordering::SeqCst);
			});
		}

		for value in 1..=STEPS {
			*producer.write().ok().expect("open") = value;
			let want = READERS * value as usize;
			until(|| seen.load(Ordering::SeqCst) == want, "a value waiter");
		}
		drop(producer);
		until(|| closed.load(Ordering::SeqCst) == READERS, "a closed waiter");
	}
}
