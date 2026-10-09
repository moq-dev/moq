//! Cooperative scheduling: bound how much one task does per turn.
//!
//! A task whose kio polls keep returning `Ready` never yields on its own: a serve
//! loop that always has another item ready holds its thread for as long as items
//! keep arriving, starving everything else on it. Tokio's own budget cannot see
//! that loop, since the work goes through kio rather than a tokio resource.
//!
//! [`budget`] gives a task a fixed number of units for its turn. Every kio poll
//! that would return `Ready` with progress (a value, a write guard, a queue item)
//! spends one. Once none are left, such a poll wakes the task and returns `Pending`
//! instead, so the task yields and picks up where it left off on its next turn.
//! A poll that returns `Pending` anyway spends nothing, and neither does one
//! passed [`Waiter::noop`], whose caller reads `Pending` as "nothing now", nor
//! the synchronous `try_*` and `read` methods. Waiting on a level (closure, demand)
//! is not progress, so those polls spend nothing either.
//!
//! Code outside any [`budget`] call is unconstrained.

use std::cell::Cell;

use crate::Waiter;

/// Units a task gets per turn. The smallest a `session_delivery_burst` sweep over 32,
/// 128 (tokio's), and 512 found as fast as no budget at all.
pub(crate) const UNITS: u16 = 32;

thread_local! {
	/// Units left in the current turn, or `None` outside any [`budget`] call.
	static LEFT: Cell<Option<u16>> = const { Cell::new(None) };
}

/// Run `f` with a fresh budget, restoring the previous one afterwards.
///
/// Call it only where a driver hands a task its turn: [`Tasks`](crate::Tasks) does
/// for each task it polls, and a runtime adapter does around each poll of its root.
/// Nested calls refill, so a child gets its own budget and its parent resumes with
/// whatever it had left.
pub fn budget<R>(f: impl FnOnce() -> R) -> R {
	struct Restore(Option<u16>);

	impl Drop for Restore {
		fn drop(&mut self) {
			LEFT.set(self.0);
		}
	}

	let _restore = Restore(LEFT.replace(Some(UNITS)));
	f()
}

/// Spend a unit for a poll about to return `Ready`.
///
/// `false` means the budget ran out: the caller wakes `waiter`, once it released any
/// lock, and returns `Pending` without taking anything.
pub(crate) fn spend(waiter: &Waiter) -> bool {
	if waiter.is_noop() {
		return true;
	}
	LEFT.with(|left| match left.get() {
		None => true,
		Some(0) => false,
		Some(units) => {
			left.set(Some(units - 1));
			true
		}
	})
}

/// Yield for an exhausted budget: wake the task so it runs again next turn.
pub(crate) fn exhausted<T>(waiter: &Waiter) -> std::task::Poll<T> {
	waiter.waker().wake_by_ref();
	std::task::Poll::Pending
}

/// The units left in this turn, or `None` outside any [`budget`] call.
#[cfg(test)]
pub(crate) fn left() -> Option<u16> {
	LEFT.get()
}

#[cfg(all(test, not(loom)))]
mod tests {
	use std::{
		sync::{
			Arc,
			atomic::{AtomicUsize, Ordering},
		},
		task::{Poll, Wake, Waker},
	};

	use super::*;
	use crate::{Producer, Queue};

	struct Count(AtomicUsize);

	impl Wake for Count {
		fn wake(self: Arc<Self>) {
			self.0.fetch_add(1, Ordering::SeqCst);
		}
	}

	fn counted() -> (Waiter, Arc<Count>) {
		let count = Arc::new(Count(AtomicUsize::new(0)));
		(Waiter::new(Waker::from(count.clone())), count)
	}

	fn ready(value: &crate::Ref<'_, u32>) -> Poll<u32> {
		Poll::Ready(**value)
	}

	#[test]
	fn unconstrained_outside_a_budget() {
		let producer = Producer::new(1u32);
		let consumer = producer.consume();
		let (waiter, _) = counted();
		for _ in 0..1_000 {
			assert!(matches!(consumer.poll(&waiter, ready), Poll::Ready(Ok(1))));
		}
		assert_eq!(left(), None);
	}

	#[test]
	fn ready_spends_and_pending_does_not() {
		let producer = Producer::new(1u32);
		let consumer = producer.consume();
		let (waiter, _) = counted();
		budget(|| {
			assert!(consumer.poll(&waiter, ready).is_ready());
			assert_eq!(left(), Some(UNITS - 1));
			assert!(consumer.poll(&waiter, |_| Poll::<()>::Pending).is_pending());
			assert_eq!(left(), Some(UNITS - 1), "a pending poll must not spend");
		});
	}

	#[test]
	fn exhaustion_yields_and_wakes() {
		let producer = Producer::new(1u32);
		let consumer = producer.consume();
		let (waiter, wakes) = counted();
		budget(|| {
			for _ in 0..UNITS {
				assert!(consumer.poll(&waiter, ready).is_ready());
			}
			assert_eq!(wakes.0.load(Ordering::SeqCst), 0);
			assert!(consumer.poll(&waiter, ready).is_pending());
			assert_eq!(wakes.0.load(Ordering::SeqCst), 1, "a yield must wake the task");

			// Waiting on a level spends nothing, so it still answers.
			producer.close().ok().expect("open");
			assert!(consumer.poll_closed(&waiter).is_ready());
		});
	}

	/// A queue item is never taken by a poll that then yields.
	#[test]
	fn exhaustion_takes_nothing() {
		let queue = Queue::new();
		let (waiter, _) = counted();
		budget(|| {
			for item in 0..=u32::from(UNITS) {
				queue.try_push(item).expect("open");
			}
			for item in 0..u32::from(UNITS) {
				assert_eq!(queue.poll_pop(&waiter), Poll::Ready(Ok(item)));
			}
			assert!(queue.poll_pop(&waiter).is_pending());
			assert!(queue.poll_push_with(&waiter, || 99).is_pending());
		});
		assert_eq!(
			queue.try_pop(),
			Ok(Some(u32::from(UNITS))),
			"the yield left the item queued"
		);
		assert_eq!(queue.try_pop(), Ok(None), "the yield pushed nothing");
	}

	#[test]
	fn noop_waiter_spends_nothing() {
		let producer = Producer::new(1u32);
		let consumer = producer.consume();
		let waiter = Waiter::noop();
		budget(|| {
			for _ in 0..1_000 {
				assert!(consumer.poll(&waiter, ready).is_ready());
			}
			assert_eq!(left(), Some(UNITS));
			// A clone of the noop waiter is still one.
			assert!(consumer.poll(&waiter.clone(), ready).is_ready());
			assert_eq!(left(), Some(UNITS));
		});
	}

	#[test]
	fn nested_budgets_refill_and_restore() {
		let producer = Producer::new(1u32);
		let consumer = producer.consume();
		let (waiter, _) = counted();
		budget(|| {
			assert!(consumer.poll(&waiter, ready).is_ready());
			budget(|| {
				assert_eq!(left(), Some(UNITS), "a nested budget refills");
				for _ in 0..UNITS {
					assert!(consumer.poll(&waiter, ready).is_ready());
				}
				assert!(consumer.poll(&waiter, ready).is_pending());
			});
			assert_eq!(left(), Some(UNITS - 1), "the outer budget resumes where it was");
		});
		assert_eq!(left(), None);
	}

	#[test]
	fn a_panic_restores_the_outer_budget() {
		budget(|| {
			let inner = std::panic::catch_unwind(|| budget(|| panic!("boom")));
			assert!(inner.is_err());
			assert_eq!(left(), Some(UNITS));
		});
		assert_eq!(left(), None);
	}
}
