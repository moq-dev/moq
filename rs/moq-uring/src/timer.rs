//! Userspace timers on the worker's deadline queue, never a timeout SQE.
//!
//! Every armed deadline lives in the worker's [`kio::clock::Deadlines`]. The
//! worker advances it to each turn's instant, firing the due prefix, and parks
//! with the earliest remaining deadline as the absolute `io_uring_enter`
//! timeout, so timers cost zero submissions and re-arming is a pure memory
//! operation.

use std::sync::Arc;
use std::task::Poll;

use kio::clock::{Deadline, Deadlines, Instant};

use crate::metrics::Counters;

/// A single re-armable timer on a worker's clock.
///
/// Arm with [`Self::set`] and poll from this worker's thread.
/// Created by [`crate::Handle::timer`].
pub struct Timer {
	deadline: Deadline,
	/// Read to tell whether the armed instant was reached.
	deadlines: Deadlines,
	/// Armed and not yet counted as fired or cancelled.
	armed: bool,
	metrics: Arc<Counters>,
}

impl Timer {
	pub(crate) fn from_queue(deadlines: &Deadlines, metrics: Arc<Counters>) -> Self {
		Self {
			deadline: Deadline::new(deadlines),
			deadlines: deadlines.clone(),
			armed: false,
			metrics,
		}
	}

	/// Count the armed instant as fired if the queue reached it, else cancelled,
	/// whether or not a poll saw it fire.
	fn retire(&mut self) {
		if !std::mem::take(&mut self.armed) {
			return;
		}
		let reached = self.deadline.deadline().is_some_and(|at| at <= self.deadlines.now());
		if reached {
			self.metrics.timers_fired.add(1);
		} else {
			self.metrics.timers_cancelled.add(1);
		}
	}

	/// Arm or disarm the timer.
	///
	/// An instant the worker's clock already reached fires at once.
	pub fn set(&mut self, at: Option<Instant>) {
		if self.deadline.deadline() == at {
			return;
		}
		// A re-arm before the deadline counts as a cancel, so the counter reads
		// as timer churn rather than as loss.
		self.retire();
		self.armed = at.is_some();
		if self.armed {
			self.metrics.timers_armed.add(1);
		}
		self.deadline.set(at);
	}

	/// Poll for expiration and register for wakeups.
	pub fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		let poll = self.deadline.poll(waiter);
		if poll.is_ready() {
			self.retire();
		}
		poll
	}
}

impl Drop for Timer {
	fn drop(&mut self) {
		self.retire();
	}
}

impl std::fmt::Debug for Timer {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Timer").field("at", &self.deadline.deadline()).finish()
	}
}

impl Timer {
	/// Create a disarmed timer on this worker.
	pub fn new(handle: &crate::Handle) -> Self {
		handle.timer()
	}

	/// Create a timer that expires after the given duration on the worker's clock.
	pub fn after(handle: &crate::Handle, duration: std::time::Duration) -> Self {
		let mut timer = handle.timer();
		timer.set(handle.now().checked_add(duration));
		timer
	}

	/// Wait for this timer to expire.
	pub async fn wait(&mut self) {
		kio::wait(|waiter| self.poll(waiter)).await
	}
}
