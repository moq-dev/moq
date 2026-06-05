use std::{
	ops::{Deref, DerefMut},
	pin::Pin,
	task::{Context, Poll},
};

use crate::Waiter;

/// A pollable computation backed by kio channels.
///
/// Implementors write only [`Self::poll`], registering the [`Waiter`] with the
/// channels they read. Wrap the value in [`Pending`] to get a real
/// [`std::future::Future`].
///
/// This exists because a kio [`Waiter`] holds the strong `Arc<Waker>` while the
/// channel's [`crate::WaiterList`] keeps only a `Weak`. A bare
/// [`std::future::Future`] would have to stash the strong `Waiter` in a field and
/// replace it every poll (or lose its wakeup); [`Pending`] does that once so each
/// implementor doesn't have to.
pub trait Future: Unpin {
	type Output;

	/// Poll for the output, registering `waiter` with the relevant channels if not
	/// yet ready.
	///
	/// Takes `&self`: kio channels poll immutably, so a pollable can be driven
	/// through a shared borrow (e.g. while it lives inside an `&self`-borrowed enum).
	/// Carry any per-poll mutable state in a kio channel or a [`std::cell`] type.
	fn poll(&self, waiter: &Waiter) -> Poll<Self::Output>;
}

/// Adapts a kio [`Future`] into a [`std::future::Future`], retaining the strong
/// [`Waiter`] between polls so its weak registration stays live.
///
/// Derefs to the inner value, so any inherent methods you define on it are
/// reachable through the pending handle (e.g. a non-blocking `poll`, or an
/// `update`).
pub struct Pending<F> {
	inner: F,
	// Retain the previous waiter so its Weak registration survives until the next
	// poll replaces it (see [`crate::WaiterList`]).
	waiter: Option<Waiter>,
}

impl<F> Pending<F> {
	/// Wrap a [`Future`] so it can be `.await`ed.
	pub fn new(inner: F) -> Self {
		Self { inner, waiter: None }
	}

	/// Consume the wrapper, returning the inner value.
	pub fn into_inner(self) -> F {
		self.inner
	}
}

impl<F> Deref for Pending<F> {
	type Target = F;

	fn deref(&self) -> &F {
		&self.inner
	}
}

impl<F> DerefMut for Pending<F> {
	fn deref_mut(&mut self) -> &mut F {
		&mut self.inner
	}
}

impl<F: Future> std::future::Future for Pending<F> {
	type Output = F::Output;

	fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<F::Output> {
		// Replacing drops the previous waiter, killing its Weak ref in the list so
		// the inner poll's register call can recycle the slot (see `WaiterList`).
		// `Pending<F>` is `Unpin` (F is, via the trait bound), so this deref is sound.
		let this = &mut *self;
		this.waiter = Some(Waiter::new(cx.waker().clone()));
		Future::poll(&this.inner, this.waiter.as_ref().unwrap())
	}
}

#[cfg(test)]
mod test {
	use super::*;
	use crate::Producer;

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

	impl Future for AtLeast {
		type Output = u64;

		fn poll(&self, waiter: &Waiter) -> Poll<u64> {
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
		assert!(Future::poll(&*pending, &Waiter::noop()).is_pending());

		if let Ok(mut v) = producer.write() {
			*v = 6;
		}

		// The std Future resolves once the threshold is met.
		let mut cx = Context::from_waker(Waker::noop());
		let mut pending = std::pin::pin!(pending);
		assert_eq!(std::future::Future::poll(pending.as_mut(), &mut cx), Poll::Ready(6));
	}
}
