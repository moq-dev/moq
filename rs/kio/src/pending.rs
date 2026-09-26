use std::{
	future::Future,
	ops::{Deref, DerefMut},
	pin::Pin,
	task::{Context, Poll},
};

use crate::{Park, Task};

/// Adapts a [`Task`] into a [`Future`], parking the strong [`Waiter`](crate::Waiter) between
/// polls so its weak registration stays live.
///
/// Derefs to the inner value, so any inherent methods you define on it are
/// reachable through the pending handle (e.g. a non-blocking `poll`, or an
/// `update`).
pub struct Pending<P> {
	inner: P,
	// Retains a parked waiter across polls so its weak registrations survive; see [`Park`].
	park: Park,
}

impl<P> Pending<P> {
	/// Wrap a [`Task`] so it can be `.await`ed.
	pub fn new(inner: P) -> Self {
		Self {
			inner,
			park: Park::default(),
		}
	}

	/// Consume the wrapper, returning the inner value.
	pub fn into_inner(self) -> P {
		self.inner
	}
}

impl<P> Deref for Pending<P> {
	type Target = P;

	fn deref(&self) -> &P {
		&self.inner
	}
}

impl<P> DerefMut for Pending<P> {
	fn deref_mut(&mut self) -> &mut P {
		&mut self.inner
	}
}

impl<P: Task + Unpin> Future for Pending<P> {
	type Output = P::Output;

	fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<P::Output> {
		let this = &mut *self;
		let waiter = this.park.hold(cx);
		this.inner.poll(waiter)
	}
}

#[cfg(all(test, not(loom)))]
mod test {
	use super::*;
	use crate::{Producer, Waiter};

	/// A pollable that waits for the channel value to reach a threshold, with an
	/// inherent method reachable through `Pending`'s `DerefMut`.
	struct AtLeast {
		consumer: crate::Consumer<u64>,
		threshold: u64,
	}

	impl AtLeast {
		fn bump_threshold(&mut self) {
			self.threshold += 1;
		}
	}

	impl Task for AtLeast {
		type Output = u64;

		fn poll(&mut self, waiter: &Waiter) -> Poll<u64> {
			let threshold = self.threshold;
			match self.consumer.poll(waiter, |v| {
				let current = **v;
				if current >= threshold {
					Poll::Ready(current)
				} else {
					Poll::Pending
				}
			}) {
				Poll::Ready(Ok(v)) => Poll::Ready(v),
				_ => Poll::Pending,
			}
		}
	}

	#[test]
	fn pending_derefs_and_drives() {
		use std::task::Waker;

		let producer = Producer::new(0u64);
		let mut pending = Pending::new(AtLeast {
			consumer: producer.consume(),
			threshold: 5,
		});

		// Inherent method on the inner reached via DerefMut.
		pending.bump_threshold(); // threshold now 6

		// The kio-level poll (reached through Deref) is pending until the value catches up.
		assert!(Task::poll(&mut *pending, &Waiter::noop()).is_pending());

		if let Ok(mut v) = producer.write() {
			*v = 6;
		}

		// The std Future resolves once the threshold is met.
		let mut cx = Context::from_waker(Waker::noop());
		let mut pending = std::pin::pin!(pending);
		assert_eq!(Future::poll(pending.as_mut(), &mut cx), Poll::Ready(6));
	}
}
