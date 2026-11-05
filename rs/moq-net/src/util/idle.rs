//! An early exit for a read that parked and was not woken since.
//!
//! A read that parks registers on everything that can change its answer, and each of those
//! notifies on every change. [`Idle`] parks the read through a waiter of its own, which
//! records any wake before passing it on to the task, so until one fires a repeated read
//! can answer `Pending` again without walking, and locking, everything it read before.
//!
//! Only what notifies can be trusted this way. A change the owner makes itself, which
//! notifies nothing, has to [`Idle::disarm`] the gate.
//!
//! An owner with more than one read keys each by `K`, since one parking says nothing
//! about another.

use std::{
	sync::{
		Arc,
		atomic::{AtomicBool, Ordering},
	},
	task::{Context, Wake, Waker},
};

/// A read's parked state, and the waiter it parked on.
pub(crate) struct Idle<K = ()> {
	/// The read that last parked, while nothing has disarmed it since.
	parked: Option<K>,
	woken: Arc<Woken>,
	/// Wakes [`Self::woken`].
	wake: Waker,
	/// Holds the waiter reads park on with [`Self::wake`]. Kept across reads, so parking
	/// again on a list that still holds it is free, as it would be for the task's own
	/// waiter, and retired as a task's own park retires one that parked on more lists
	/// than it can record.
	park: kio::Park,
}

/// Records a wake, then wakes the task.
struct Woken {
	woken: AtomicBool,
	task: Waker,
}

impl Wake for Woken {
	fn wake(self: Arc<Self>) {
		self.wake_by_ref();
	}

	fn wake_by_ref(self: &Arc<Self>) {
		// Before the task's wake, so the read it triggers sees the change.
		self.woken.store(true, Ordering::Release);
		self.task.wake_by_ref();
	}
}

impl<K: Copy + Eq> Idle<K> {
	fn new(task: &Waker) -> Box<Self> {
		let woken = Arc::new(Woken {
			woken: AtomicBool::new(false),
			task: task.clone(),
		});
		Box::new(Self {
			parked: None,
			wake: Waker::from(woken.clone()),
			woken,
			park: kio::Park::default(),
		})
	}

	/// Whether `read`, again for `task`, would park exactly as it last did: it parked,
	/// nothing it parked on has fired since, nothing disarmed it, and it still wakes the
	/// same task.
	pub(crate) fn parked(this: &Option<Box<Self>>, read: K, task: &kio::Waiter) -> bool {
		this.as_ref().is_some_and(|idle| {
			idle.parked == Some(read)
				&& !idle.woken.woken.load(Ordering::Acquire)
				&& idle.woken.task.will_wake(task.waker())
		})
	}

	/// Start a full read for `task`, returning the gate holding the waiter it must park on.
	/// A wake from here on counts against the read about to happen.
	pub(crate) fn start(this: &mut Option<Box<Self>>, task: &kio::Waiter) -> Box<Self> {
		match this.take() {
			Some(mut idle) if idle.woken.task.will_wake(task.waker()) => {
				idle.parked = None;
				// Cleared before the read looks at anything, so a change it misses wakes the
				// waiter after this store and leaves the next read a full one.
				idle.woken.woken.store(false, Ordering::Release);
				idle
			}
			// Another task: parking anew drops the old waiter, whose registrations go dead.
			_ => Self::new(task.waker()),
		}
	}

	/// The waiter a full read parks on.
	pub(crate) fn waiter(&mut self) -> &kio::Waiter {
		self.park.hold(&Context::from_waker(&self.wake))
	}

	/// Put the gate back after a full `read`, armed if it parked.
	pub(crate) fn finish(mut self: Box<Self>, this: &mut Option<Box<Self>>, read: K, pending: bool) {
		self.parked = pending.then_some(read);
		*this = Some(self);
	}

	/// Forget the parked read: something it depends on changed without notifying.
	pub(crate) fn disarm(this: &mut Option<Box<Self>>) {
		if let Some(idle) = this {
			idle.parked = None;
		}
	}
}

#[cfg(test)]
mod test {
	use super::*;

	/// A parked read stays parked until a list it parked on wakes it, a read for another
	/// task parks anew, and a disarmed one walks again.
	#[test]
	fn a_read_stays_parked_until_woken_or_disarmed() {
		let task = kio::Waiter::noop();
		let mut slot: Option<Box<Idle>> = None;
		assert!(!Idle::parked(&slot, (), &task), "nothing parked yet");

		let mut list = kio::WaiterList::new();
		let mut idle = Idle::start(&mut slot, &task);
		idle.waiter().register(&mut list);
		idle.finish(&mut slot, (), true);
		assert!(Idle::parked(&slot, (), &task));

		list.wake();
		assert!(!Idle::parked(&slot, (), &task), "a wake reopens the read");

		let mut idle = Idle::start(&mut slot, &task);
		idle.waiter().register(&mut list);
		idle.finish(&mut slot, (), true);
		assert!(Idle::parked(&slot, (), &task));
		Idle::disarm(&mut slot);
		assert!(!Idle::parked(&slot, (), &task), "a disarm reopens the read");

		let idle = Idle::start(&mut slot, &task);
		idle.finish(&mut slot, (), false);
		assert!(
			!Idle::parked(&slot, (), &task),
			"a read that got something is not parked"
		);

		let other = kio::Waiter::new(Waker::from(Arc::new(Woken {
			woken: AtomicBool::new(false),
			task: Waker::noop().clone(),
		})));
		let idle = Idle::start(&mut slot, &task);
		idle.finish(&mut slot, (), true);
		assert!(!Idle::parked(&slot, (), &other), "another task reads anew");
	}

	/// A gate keyed by read answers only for the read that parked.
	#[test]
	fn a_parked_read_says_nothing_about_another() {
		let task = kio::Waiter::noop();
		let mut slot = None;
		let idle = Idle::start(&mut slot, &task);
		idle.finish(&mut slot, 1u8, true);
		assert!(Idle::parked(&slot, 1, &task));
		assert!(!Idle::parked(&slot, 2, &task), "another read walks");
	}

	/// The waiter reads park on is kept across reads, but retired once it parked on more
	/// lists than it records, so re-parking cannot stack entries those lists never collect.
	#[test]
	fn a_waiter_parked_past_its_records_is_retired() {
		let task = kio::Waiter::noop();
		let mut slot: Option<Box<Idle>> = None;
		for (lists, retired) in [(1, false), (9, true)] {
			let mut idle = Idle::start(&mut slot, &task);
			let mut parked: Vec<_> = (0..lists).map(|_| kio::WaiterList::new()).collect();
			let waiter = idle.waiter();
			for list in &mut parked {
				waiter.register(list);
			}
			idle.finish(&mut slot, (), true);
			assert!(Idle::parked(&slot, (), &task));

			let mut idle = Idle::start(&mut slot, &task);
			idle.waiter();
			assert_eq!(parked.iter().all(kio::WaiterList::is_empty), retired);
			idle.finish(&mut slot, (), false);
		}
	}
}
