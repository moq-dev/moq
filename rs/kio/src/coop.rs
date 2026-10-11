//! Cooperative scheduling: bound how many passes a loop makes per poll.
//!
//! A task whose loop keeps finding work ready never yields on its own: a serve loop
//! that always has another item ready holds its thread for as long as items keep
//! arriving, starving everything else on it. Tokio's own budget cannot see that loop,
//! since the work goes through kio rather than a tokio resource.

use std::task::Poll;

use crate::Waiter;

/// Yields a loop after a fixed number of passes, sized by the loop's cost per pass.
///
/// Keep one per loop and call [`Self::poll_yield`] at the head of each pass.
#[derive(Debug, Clone)]
pub struct Budget {
	passes: u32,
	left: u32,
}

impl Budget {
	/// A budget allowing `passes` passes between yields.
	pub const fn new(passes: u32) -> Self {
		Self { passes, left: passes }
	}

	/// Spend a pass, or wake the task and return `Pending` once none are left.
	///
	/// The yield refills the budget for the next poll. Call it only where `Pending` goes
	/// straight up to the task: a caller that reads `Pending` as "nothing now" would
	/// mistake the yield for an answer.
	pub fn poll_yield(&mut self, waiter: &Waiter) -> Poll<()> {
		match self.left.checked_sub(1) {
			Some(left) => {
				self.left = left;
				Poll::Ready(())
			}
			None => {
				self.left = self.passes;
				waiter.waker().wake_by_ref();
				Poll::Pending
			}
		}
	}
}

#[cfg(all(test, not(loom)))]
mod tests {
	use std::{
		sync::{
			Arc,
			atomic::{AtomicUsize, Ordering},
		},
		task::{Wake, Waker},
	};

	use super::*;

	struct Count(AtomicUsize);

	impl Wake for Count {
		fn wake(self: Arc<Self>) {
			self.0.fetch_add(1, Ordering::SeqCst);
		}
	}

	#[test]
	fn yields_after_its_passes_and_refills() {
		let wakes = Arc::new(Count(AtomicUsize::new(0)));
		let waiter = Waiter::new(Waker::from(wakes.clone()));
		let mut budget = Budget::new(4);
		for round in 1..=3 {
			for _ in 0..4 {
				assert!(budget.poll_yield(&waiter).is_ready());
			}
			assert!(budget.poll_yield(&waiter).is_pending());
			assert_eq!(wakes.0.load(Ordering::SeqCst), round, "a yield must wake the task");
		}
	}
}
