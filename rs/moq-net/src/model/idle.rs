//! The shared early exit for a read that found nothing.
//!
//! A read that parks registers on everything that can change its answer: for a group read,
//! the track, the subscription, the fills it waits on, the route, and the wall deadlines it
//! judged against; for a frame read, the group and its expiry. Each of those notifies on
//! every change, so until one fires, the answer stays the same. [`Idle`] parks reads
//! through a waiter of its own that records any wake before passing it on to the task, and
//! answers a read repeated before that wake with the same `Pending`, rather than walking
//! the whole read path again to reach it.
//!
//! Only what notifies can be trusted this way. A change the reader makes to its own state,
//! such as moving its range, notifies nothing, so it disarms the gate itself.
//!
//! Gates nest: a read through a gate that polls another gated reader passes it the outer
//! gate's waiter, whose waker stays the same across reads, so the inner gate keeps its arm
//! and its wake reaches the outer gate on the way to the task.

use std::{
	sync::{
		Arc,
		atomic::{AtomicBool, Ordering},
	},
	task::{Wake, Waker},
};

/// Which group read parked: a track reader parks the arrival and sequence walks
/// separately, since one finding nothing says nothing about the other.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Walk {
	Arrival,
	Sequence,
}

impl Walk {
	/// The walk a read in sequence order for `ordered`, or arrival order otherwise, takes.
	pub(super) fn new(ordered: bool) -> Self {
		match ordered {
			true => Self::Sequence,
			false => Self::Arrival,
		}
	}
}

/// A reader's parked read, and the waiter it parked on. `K` tells apart reads that park
/// separately, such as a [`Walk`].
pub(super) struct Idle<K> {
	/// The read that parked, or `None` while nothing is parked.
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
	fn new(task: &Waker) -> Self {
		let woken = Arc::new(Woken {
			woken: AtomicBool::new(false),
			task: task.clone(),
		});
		Self {
			parked: None,
			wake: Waker::from(woken.clone()),
			woken,
			park: kio::Park::default(),
		}
	}

	/// Whether `walk`, read again for `task`, would park exactly as it last did: it parked,
	/// nothing it parked on has fired since, and it still wakes the same task.
	pub(super) fn parked(this: &Option<Box<Self>>, walk: K, task: &kio::Waiter) -> bool {
		this.as_ref().is_some_and(|idle| {
			idle.parked == Some(walk)
				&& !idle.woken.woken.load(Ordering::Acquire)
				&& idle.woken.task.will_wake(task.waker())
		})
	}

	/// Start a full read for `task`, returning the gate holding the waiter it must park
	/// on. A wake from here on counts against the read about to happen.
	pub(super) fn start(this: &mut Option<Box<Self>>, task: &kio::Waiter) -> Box<Self> {
		match this.take() {
			Some(mut idle) if idle.woken.task.will_wake(task.waker()) => {
				idle.parked = None;
				// Cleared before the read looks at anything, so a change it misses wakes
				// the waiter after this store and leaves the next read a full one.
				idle.woken.woken.store(false, Ordering::Release);
				idle
			}
			// Another task: parking anew drops the old waiter, whose registrations go dead.
			_ => Box::new(Self::new(task.waker())),
		}
	}

	/// The waiter a full read parks on.
	pub(super) fn waiter(&mut self) -> &kio::Waiter {
		self.park.hold(&std::task::Context::from_waker(&self.wake))
	}

	/// Put the gate back after a full read of `walk`, armed if it parked.
	pub(super) fn finish(mut self: Box<Self>, this: &mut Option<Box<Self>>, walk: K, pending: bool) {
		self.parked = pending.then_some(walk);
		*this = Some(self);
	}

	/// Forget the parked read: the reader moved in a way nothing notifies.
	pub(super) fn disarm(this: &mut Option<Box<Self>>) {
		if let Some(idle) = this {
			idle.parked = None;
		}
	}
}

#[cfg(test)]
mod test {
	use super::*;

	/// The waiter reads park on is kept across reads, but retired once it parked on more
	/// lists than it records, so re-parking cannot stack entries those lists never collect.
	#[test]
	fn a_waiter_parked_past_its_records_is_retired() {
		let task = kio::Waiter::noop();
		let mut slot = None;
		for (lists, retired) in [(1, false), (9, true)] {
			let mut idle = Idle::start(&mut slot, &task);
			let mut parked: Vec<_> = (0..lists).map(|_| kio::WaiterList::new()).collect();
			let waiter = idle.waiter();
			for list in &mut parked {
				waiter.register(list);
			}
			idle.finish(&mut slot, Walk::Arrival, true);
			assert!(Idle::parked(&slot, Walk::Arrival, &task));

			let mut idle = Idle::start(&mut slot, &task);
			idle.waiter();
			assert_eq!(parked.iter().all(kio::WaiterList::is_empty), retired);
			idle.finish(&mut slot, Walk::Arrival, false);
		}
	}
}
